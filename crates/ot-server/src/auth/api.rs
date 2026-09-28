//! Sign-in endpoints, an account's own profile and password, and the
//! admin's accounts, API tokens and sign-in settings. Every admin change
//! goes in the decision log.

use std::net::SocketAddr;
use std::sync::OnceLock;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{AuthSettings, AuthUser, Role, Via, check_password, hash_password, verify_password};
use crate::api::actor;
use crate::control::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    let r = Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/public", get(public))
        .route("/auth/me", get(me))
        .route("/auth/password", post(change_password))
        .route("/auth/users", get(list_users).post(create_user))
        .route("/auth/users/{id}", put(update_user).delete(delete_user))
        .route("/auth/users/{id}/password", post(reset_password))
        .route("/auth/users/{id}/revoke", post(revoke_sessions))
        .route("/auth/api-tokens", get(list_tokens).post(create_token))
        .route(
            "/auth/api-tokens/{jti}",
            axum::routing::delete(revoke_token),
        )
        .route("/auth/settings", get(get_settings).put(put_settings));
    #[cfg(feature = "saml")]
    let r = r.merge(super::saml::routes());
    r
}

fn unauthorized(m: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": m }))).into_response()
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))
}

/// A hash to check unknown emails against, so a wrong email takes as long
/// as a wrong password.
fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password("not a password of anyone's").unwrap_or_default())
}

#[derive(Deserialize)]
struct Login {
    email: String,
    password: String,
}

/// The account as the UI shows it, with how it signed in.
fn me_json(u: &AuthUser, has_password: bool) -> Value {
    json!({
        "id": u.id, "email": u.email, "name": u.name, "role": u.role, "via": u.via,
        "can_change_password": u.via == Via::Session && has_password,
    })
}

/// Sign in to a session for `user` (after a password or single sign-on).
pub(super) async fn start_session(
    s: &AppState,
    user: ot_store::User,
) -> Result<(HeaderMap, Value), ApiError> {
    let ttl = s.auth.session_secs();
    let (token, jti, exp) = s
        .auth
        .issue(&user, "session", ttl)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let id = user.id.clone();
    s.with_db(move |db| db.note_login(&id)).await?;
    tracing::info!(email = %user.email, "signed in");
    let mut headers = HeaderMap::new();
    headers.insert(header::SET_COOKIE, s.auth.session_cookie(&token, ttl));
    let role = Role::parse(&user.role).unwrap_or(Role::Viewer);
    let au = AuthUser {
        id: user.id,
        email: user.email,
        name: user.name,
        role,
        via: Via::Session,
        jti: Some((jti, exp)),
    };
    Ok((headers, me_json(&au, user.has_password)))
}

async fn login(
    State(s): State<AppState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(b): Json<Login>,
) -> Result<Response, ApiError> {
    if s.auth.disabled {
        return Err(ApiError::bad_request("sign-in is turned off (OT_AUTH=off)"));
    }
    if s.auth.settings().disable_password_login {
        return Ok((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "password sign-in is off: use single sign-on" })),
        )
            .into_response());
    }
    if !s
        .auth
        .allow_attempt(super::client_ip(peer.as_ref().map(|p| &p.0)))
    {
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "too many attempts: wait a moment" })),
        )
            .into_response());
    }
    let email = b.email.trim().to_owned();
    let found = s.with_db(move |db| db.password_hash(&email)).await?;
    let (user, hash) = match found {
        Some((u, Some(h))) if u.active => (Some(u), h),
        _ => (None, dummy_hash().to_owned()),
    };
    let rehash = super::needs_rehash(&hash);
    let password = b.password.clone();
    let ok = blocking(move || verify_password(&password, &hash)).await?;
    let Some(user) = user.filter(|_| ok) else {
        tracing::info!(email = %b.email.trim(), "sign-in refused");
        return Ok(unauthorized("wrong email or password"));
    };
    // Hashes from before 0.4.0 (Argon2id) move to PBKDF2 in the FIPS module.
    if rehash {
        let new = blocking(move || hash_password(&b.password))
            .await?
            .map_err(|e| ApiError::internal(e.to_string()))?;
        let id = user.id.clone();
        s.with_db(move |db| db.set_password_hash(&id, Some(&new)))
            .await?;
        tracing::info!(user = %user.email, "password hash upgraded to PBKDF2");
    }
    let (headers, body) = start_session(&s, user).await?;
    Ok((headers, Json(body)).into_response())
}

async fn logout(State(s): State<AppState>, headers: HeaderMap) -> Response {
    let user = super::identify(&s, &headers, None).await;
    if let Some((jti, exp)) = user.and_then(|u| u.jti)
        && headers.get(header::AUTHORIZATION).is_none()
    {
        let _ = s.with_db(move |db| db.revoke_token(&jti, exp)).await;
    }
    let mut h = HeaderMap::new();
    h.insert(header::SET_COOKIE, s.auth.clear_cookie());
    (StatusCode::NO_CONTENT, h).into_response()
}

/// What the sign-in page offers.
async fn public(State(s): State<AppState>) -> Json<Value> {
    let st = s.auth.settings();
    Json(json!({
        "auth": !s.auth.disabled,
        "password_login": !st.disable_password_login,
        "saml": { "enabled": st.saml.enabled && saml_ready(), "label": st.saml.button_label },
        "openstare": {
            "enabled": st.openstare.enabled,
            "login_url": st.openstare.login_url,
        },
    }))
}

async fn me(
    State(s): State<AppState>,
    Extension(u): Extension<AuthUser>,
) -> Result<Json<Value>, ApiError> {
    let id = u.id.clone();
    let has_password = if u.via == Via::Session {
        s.with_db(move |db| db.user(&id))
            .await?
            .is_some_and(|u| u.has_password)
    } else {
        false
    };
    Ok(Json(me_json(&u, has_password)))
}

#[derive(Deserialize)]
struct ChangePassword {
    current: String,
    new: String,
}

/// Change your own password; your other sessions end.
async fn change_password(
    State(s): State<AppState>,
    Extension(u): Extension<AuthUser>,
    Json(b): Json<ChangePassword>,
) -> Result<Response, ApiError> {
    if u.via != Via::Session {
        return Err(ApiError::bad_request(
            "only an account signed in with a password can change it here",
        ));
    }
    check_password(&b.new).map_err(ApiError::unprocessable)?;
    let email = u.email.clone();
    let Some((user, Some(hash))) = s.with_db(move |db| db.password_hash(&email)).await? else {
        return Err(ApiError::bad_request("this account has no password"));
    };
    let current = b.current;
    if !blocking(move || verify_password(&current, &hash)).await? {
        return Ok(unauthorized("the current password is wrong"));
    }
    let new = blocking(move || hash_password(&b.new))
        .await?
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let (id, actor) = (user.id.clone(), u.email.clone());
    s.with_db(move |db| {
        db.set_password_hash(&id, Some(&new))?;
        db.revoke_user_tokens(&id)?;
        db.record(&ot_store::Decision::new(&actor, "change_password"))
    })
    .await?;
    // A fresh session for this browser: the old ones just ended.
    let user = s
        .with_db(move |db| db.user_by_email(&u.email))
        .await?
        .ok_or_else(|| ApiError::not_found("account"))?;
    let (headers, body) = start_session(&s, user).await?;
    Ok((headers, Json(body)).into_response())
}

async fn list_users(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let users = s.with_db(|db| db.users()).await?;
    Ok(Json(json!({ "users": users })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewUser {
    email: String,
    #[serde(default)]
    name: String,
    role: Role,
    /// Without one the account can only use single sign-on.
    #[serde(default)]
    password: Option<String>,
}

fn check_email(e: &str) -> Result<(), ApiError> {
    let e = e.trim();
    if e.is_empty() || e.len() > 254 || !e.contains('@') || e.contains(char::is_whitespace) {
        return Err(ApiError::unprocessable(format!(
            "{e:?} is not an email address"
        )));
    }
    Ok(())
}

async fn create_user(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(b): Json<NewUser>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    check_email(&b.email)?;
    let hash = match b.password {
        Some(p) => {
            check_password(&p).map_err(ApiError::unprocessable)?;
            Some(
                blocking(move || hash_password(&p))
                    .await?
                    .map_err(|e| ApiError::internal(e.to_string()))?,
            )
        }
        None => None,
    };
    let actor = actor(&headers);
    let user = s
        .with_db(move |db| {
            let id = crate::fips::uuid_v4();
            let u = db.create_user(&ot_store::NewUser {
                id: &id,
                email: &b.email,
                name: &b.name,
                role: b.role.as_str(),
                password_hash: hash.as_deref(),
                origin: "local",
            })?;
            db.record(&ot_store::Decision {
                after: Some(json!(u)),
                ..ot_store::Decision::new(&actor, "create_user")
            })?;
            Ok(u)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(json!(user))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserChange {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    active: Option<bool>,
}

/// Refuse a change that would leave no active admin.
fn keeps_an_admin(
    db: &ot_store::Db,
    id: &str,
    role: Option<Role>,
    active: Option<bool>,
    deleting: bool,
) -> ot_store::sqlite::Result<bool> {
    let users = db.users()?;
    let admins = users
        .iter()
        .filter(|u| u.active && u.role == "admin")
        .filter(|u| {
            if u.id != id {
                return true;
            }
            !deleting && active != Some(false) && role.is_none_or(|r| r == Role::Admin)
        })
        .count();
    Ok(admins > 0)
}

async fn update_user(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(b): Json<UserChange>,
) -> Result<Json<Value>, ApiError> {
    let actor = actor(&headers);
    let user = s
        .with_db(move |db| {
            if !keeps_an_admin(db, &id, b.role, b.active, false)? {
                return Err(ot_store::StoreError::Conflict(
                    "that would leave no active admin".into(),
                ));
            }
            let before = db.user(&id)?;
            let u = db.update_user(&id, b.name.as_deref(), b.role.map(Role::as_str), b.active)?;
            db.record(&ot_store::Decision {
                before: before.map(|b| json!(b)),
                after: Some(json!(u)),
                ..ot_store::Decision::new(&actor, "update_user")
            })?;
            Ok(u)
        })
        .await?;
    Ok(Json(json!(user)))
}

async fn delete_user(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Extension(me): Extension<AuthUser>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    if me.id == id {
        return Err(ApiError::conflict("you cannot delete your own account"));
    }
    let actor = actor(&headers);
    s.with_db(move |db| {
        if !keeps_an_admin(db, &id, None, None, true)? {
            return Err(ot_store::StoreError::Conflict(
                "that would leave no active admin".into(),
            ));
        }
        let before = db.user(&id)?;
        db.delete_user(&id)?;
        db.record(&ot_store::Decision {
            before: before.map(|b| json!(b)),
            ..ot_store::Decision::new(&actor, "delete_user")
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Reset {
    /// `None` removes the password (single sign-on only).
    password: Option<String>,
}

async fn reset_password(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(b): Json<Reset>,
) -> Result<StatusCode, ApiError> {
    let hash = match b.password {
        Some(p) => {
            check_password(&p).map_err(ApiError::unprocessable)?;
            Some(
                blocking(move || hash_password(&p))
                    .await?
                    .map_err(|e| ApiError::internal(e.to_string()))?,
            )
        }
        None => None,
    };
    let actor = actor(&headers);
    s.with_db(move |db| {
        db.set_password_hash(&id, hash.as_deref())?;
        db.revoke_user_tokens(&id)?;
        db.record(&ot_store::Decision {
            evidence: json!({ "user": id }),
            ..ot_store::Decision::new(&actor, "reset_password")
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_sessions(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let actor = actor(&headers);
    s.with_db(move |db| {
        db.revoke_user_tokens(&id)?;
        db.record(&ot_store::Decision {
            evidence: json!({ "user": id }),
            ..ot_store::Decision::new(&actor, "revoke_sessions")
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_tokens(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let tokens = s.with_db(|db| db.api_tokens()).await?;
    Ok(Json(json!({ "tokens": tokens })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewToken {
    name: String,
    /// The account it acts as (default: yours).
    #[serde(default)]
    user_id: Option<String>,
    /// Days until it expires (default 365).
    #[serde(default)]
    days: Option<f64>,
}

/// Make an API token; it is shown this once.
async fn create_token(
    State(s): State<AppState>,
    Extension(me): Extension<AuthUser>,
    headers: HeaderMap,
    Json(b): Json<NewToken>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if b.name.trim().is_empty() {
        return Err(ApiError::unprocessable("give the token a name"));
    }
    let days = b.days.unwrap_or(365.0);
    if !(1.0 / 24.0..=3650.0).contains(&days) {
        return Err(ApiError::unprocessable("days: between 1 hour and 10 years"));
    }
    let id = b.user_id.unwrap_or(me.id);
    let lookup = id.clone();
    let user = s
        .with_db(move |db| db.user(&lookup))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("account {id}")))?;
    let (token, jti, exp) = s
        .auth
        .issue(&user, "api", (days * 86_400.0) as i64)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let actor = actor(&headers);
    let name = b.name.clone();
    let (j, uid) = (jti.clone(), user.id.clone());
    s.with_db(move |db| {
        db.add_api_token(&j, &name, &uid, &actor, exp)?;
        db.record(&ot_store::Decision {
            evidence: json!({ "token": j, "name": name, "user": uid }),
            ..ot_store::Decision::new(&actor, "create_api_token")
        })
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({ "token": token, "jti": jti, "name": b.name, "user": user.email, "expires_at_ms": exp }),
        ),
    ))
}

async fn revoke_token(
    State(s): State<AppState>,
    Path(jti): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let actor = actor(&headers);
    s.with_db(move |db| {
        db.revoke_api_token(&jti)?;
        db.record(&ot_store::Decision {
            evidence: json!({ "token": jti }),
            ..ot_store::Decision::new(&actor, "revoke_api_token")
        })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_settings(State(s): State<AppState>) -> Json<Value> {
    let mut v = json!(s.auth.settings());
    v["build"] = json!({ "saml": cfg!(feature = "saml"), "public_url": s.auth.public_url });
    Json(v)
}

async fn put_settings(
    State(s): State<AppState>,
    Extension(me): Extension<AuthUser>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    if let Some(o) = body.as_object_mut() {
        o.remove("build");
    }
    let new: AuthSettings =
        serde_json::from_value(body).map_err(|e| ApiError::unprocessable(e.to_string()))?;
    new.validate().map_err(ApiError::unprocessable)?;
    // Turning password sign-in off from a password session would lock you
    // out if single sign-on then fails: make sure there is a way back.
    if new.disable_password_login && me.via == Via::Session {
        return Err(ApiError::conflict(
            "sign in with single sign-on before turning password sign-in off",
        ));
    }
    let before = s.auth.settings();
    let (actor, saved) = (actor(&headers), new.clone());
    s.with_db(move |db| {
        db.put_auth_settings(&json!(saved))?;
        db.record(&ot_store::Decision {
            before: Some(json!(before)),
            after: Some(json!(saved)),
            ..ot_store::Decision::new(&actor, "auth_settings")
        })
    })
    .await?;
    s.auth.set_settings(new);
    Ok(get_settings(State(s)).await)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::auth::{Auth, AuthSettings};
    use crate::config::Common;

    /// The router with sign-in on, over an in-memory database holding one
    /// admin; `None` (test skipped) without `OT_TEST_REDIS_URL`.
    async fn app() -> Option<axum::Router> {
        let url = std::env::var("OT_TEST_REDIS_URL").ok()?;
        let ns = format!("ot-auth-test-{}", chrono::Utc::now().timestamp_micros());
        let common = Common {
            sqlite: ":memory:".into(),
            redis: url.clone(),
            redis_namespace: ns.clone(),
            site: ot_core::SiteCode::new("TST").unwrap(),
            nats: crate::config::NatsArgs {
                url: "nats://127.0.0.1:9".into(),
                creds: None,
                token: None,
                user: None,
                password: None,
                stream: "TRACKS".into(),
                tracks_subject: "tracks".into(),
                max_age_hours: 24.0,
            },
            profiles_dir: "profiles/trackers".into(),
            obs_window_secs: 600,
            shared_db: Default::default(),
        };
        let mut db = ot_store::Db::open_in_memory().unwrap();
        crate::auth::bootstrap_admin(&common, &mut db, Some("root@x.org"), Some("rootpass1"))
            .unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            redis: ot_store::RedisStore::connect(&url, ot_store::Keys::new(ns))
                .await
                .unwrap(),
            nats: common.connect_nats().await.unwrap(),
            auth: Arc::new(Auth::new(
                vec![1; 32],
                false,
                false,
                None,
                AuthSettings::default(),
            )),
            common,
        };
        Some(crate::control::router(state, None))
    }

    /// A request with optional cookie or bearer token; the status, the
    /// JSON body and any session cookie set.
    async fn call(
        app: &axum::Router,
        method: &str,
        uri: &str,
        auth: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value, Option<String>) {
        let mut req = Request::builder().method(method).uri(uri);
        match auth {
            Some(a) if a.starts_with("Bearer ") => req = req.header("authorization", a),
            Some(c) => req = req.header("cookie", c),
            None => {}
        }
        // Clients cannot say who they are this way.
        req = req.header("x-opentrack-actor", "spoofed");
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let cookie = res
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::to_owned);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            cookie,
        )
    }

    #[tokio::test]
    async fn sign_in_roles_tokens_and_sign_out() {
        let Some(app) = app().await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        // Nothing but the sign-in page's calls without signing in.
        let (st, ..) = call(&app, "GET", "/api/v1/tracks", None, None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, body, _) = call(&app, "GET", "/api/v1/auth/public", None, None).await;
        assert_eq!(
            (st, body["password_login"].as_bool()),
            (StatusCode::OK, Some(true))
        );

        let login = |e: &str, p: &str| json!({ "email": e, "password": p });
        let (st, ..) = call(
            &app,
            "POST",
            "/api/v1/auth/login",
            None,
            Some(login("root@x.org", "nope")),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, ..) = call(
            &app,
            "POST",
            "/api/v1/auth/login",
            None,
            Some(login("who@x.org", "rootpass1")),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, body, cookie) = call(
            &app,
            "POST",
            "/api/v1/auth/login",
            None,
            Some(login("ROOT@x.org", "rootpass1")),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["role"], "admin");
        let admin = cookie.expect("a session cookie");
        assert!(admin.starts_with("ot_session="));
        let (st, body, _) = call(&app, "GET", "/api/v1/auth/me", Some(&admin), None).await;
        assert_eq!(
            (st, body["email"].as_str()),
            (StatusCode::OK, Some("root@x.org"))
        );

        // A viewer reads but changes nothing; a track manager manages tracks.
        let (st, viewer, _) = call(
            &app,
            "POST",
            "/api/v1/auth/users",
            Some(&admin),
            Some(json!({ "email": "v@x.org", "role": "viewer", "password": "viewpass1" })),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{viewer}");
        let (_, _, vcookie) = call(
            &app,
            "POST",
            "/api/v1/auth/login",
            None,
            Some(login("v@x.org", "viewpass1")),
        )
        .await;
        let vcookie = vcookie.unwrap();
        let (st, ..) = call(&app, "GET", "/api/v1/tracks", Some(&vcookie), None).await;
        assert_eq!(st, StatusCode::OK);
        let (st, body, _) = call(
            &app,
            "POST",
            "/api/v1/tracks/delete",
            Some(&vcookie),
            Some(json!({})),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
        let (st, ..) = call(&app, "GET", "/api/v1/auth/users", Some(&vcookie), None).await;
        assert_eq!(st, StatusCode::FORBIDDEN);

        // The decision log names the account, not what the client claimed.
        let (st, ..) = call(
            &app,
            "POST",
            "/api/v1/auth/users",
            Some(&admin),
            Some(json!({ "email": "tm@x.org", "role": "track_manager" })),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED);
        let (_, body, _) = call(
            &app,
            "GET",
            "/api/v1/decisions?op=create_user",
            Some(&admin),
            None,
        )
        .await;
        let text = body.to_string();
        assert!(
            text.contains("root@x.org") && !text.contains("spoofed"),
            "{text}"
        );

        // An API token acts as its account until revoked.
        let (st, tok, _) = call(
            &app,
            "POST",
            "/api/v1/auth/api-tokens",
            Some(&admin),
            Some(json!({ "name": "script", "user_id": viewer["id"] })),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{tok}");
        let bearer = format!("Bearer {}", tok["token"].as_str().unwrap());
        let (st, body, _) = call(&app, "GET", "/api/v1/auth/me", Some(&bearer), None).await;
        assert_eq!(
            (st, body["via"].as_str()),
            (StatusCode::OK, Some("api_token"))
        );
        // A session cookie is not an API token, nor the other way round.
        let (st, ..) = call(
            &app,
            "GET",
            "/api/v1/auth/me",
            Some(&format!("ot_session={}", tok["token"].as_str().unwrap())),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let uri = format!("/api/v1/auth/api-tokens/{}", tok["jti"].as_str().unwrap());
        let (st, ..) = call(&app, "DELETE", &uri, Some(&admin), None).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        let (st, ..) = call(&app, "GET", "/api/v1/auth/me", Some(&bearer), None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);

        // Deactivating an account ends its sessions at once; the last
        // admin cannot be demoted.
        let uri = format!("/api/v1/auth/users/{}", viewer["id"].as_str().unwrap());
        let (st, ..) = call(
            &app,
            "PUT",
            &uri,
            Some(&admin),
            Some(json!({ "active": false })),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, ..) = call(&app, "GET", "/api/v1/tracks", Some(&vcookie), None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (_, me, _) = call(&app, "GET", "/api/v1/auth/me", Some(&admin), None).await;
        let uri = format!("/api/v1/auth/users/{}", me["id"].as_str().unwrap());
        let (st, ..) = call(
            &app,
            "PUT",
            &uri,
            Some(&admin),
            Some(json!({ "role": "viewer" })),
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT);

        // Signing out ends the session.
        let (st, _, cleared) = call(&app, "POST", "/api/v1/auth/logout", Some(&admin), None).await;
        assert_eq!(
            (st, cleared.as_deref()),
            (StatusCode::NO_CONTENT, Some("ot_session="))
        );
        let (st, ..) = call(&app, "GET", "/api/v1/auth/me", Some(&admin), None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }
}

/// SAML is built in and OpenSSL (which checks its signatures) is in FIPS mode.
fn saml_ready() -> bool {
    #[cfg(feature = "saml")]
    return super::saml::openssl_fips();
    #[cfg(not(feature = "saml"))]
    false
}

