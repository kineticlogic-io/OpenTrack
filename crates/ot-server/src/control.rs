//! The control plane: REST API and the React UI.
//!
//! Health, a status summary of every dependency, system tracks and their
//! graph history here; sources, registry and metrics in [`crate::api`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Request, State};
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::header::{self, CACHE_CONTROL};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ot_core::Uid;
use ot_store::{Db, RedisStore};
use serde_json::{Value, json};
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeader;
use tower_http::trace::{DefaultMakeSpan, TraceLayer};

use crate::config::Common;

#[derive(Clone)]
pub struct AppState {
    pub common: Common,
    pub db: Arc<Mutex<Db>>,
    pub redis: RedisStore,
    pub nats: ot_nats::Nats,
    pub auth: crate::auth::SharedAuth,
}

impl AppState {
    /// Run a SQLite call off the async runtime.
    pub(crate) async fn with_db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Db) -> ot_store::sqlite::Result<T> + Send + 'static,
    ) -> Result<T, ApiError> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let mut db = db
                .lock()
                .map_err(|_| ApiError::internal("database lock poisoned"))?;
            f(&mut db).map_err(ApiError::from)
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    }
}

pub fn router(state: AppState, ui_dir: Option<PathBuf>) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/tracks/{uid}", get(track))
        .route("/tracks/{uid}/explain", get(explain))
        .merge(crate::api::routes())
        .merge(crate::metrics::routes())
        .merge(crate::cot::api::routes())
        .merge(crate::auth::api::routes())
        .merge(crate::decisions_api::routes())
        .merge(crate::audit_api::routes())
        .merge(crate::history_api::routes())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::layer,
        ));

    let hsts = state.auth.secure_cookies;
    let mut app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api/v1", api)
        .with_state(state);

    if let Some(dir) = ui_dir.filter(|d| d.join("index.html").is_file()) {
        app = app
            .nest_service("/assets", assets(&dir))
            .fallback_service(page(&dir));
    }
    app.layer(CompressionLayer::new())
        .layer(axum::middleware::from_fn_with_state(hsts, security_headers))
        // Requests as INFO spans, so they are traced (OTLP) at the default
        // log level.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(tracing::Level::INFO)),
        )
}

/// The page's content security policy: everything from this server (the
/// map's worker is a bundled file; its land outlines are served here),
/// inline styles (React's `style` attributes, the map's and editors'), and
/// images and fonts as data or blob URLs (symbols, the map's sprites). No
/// inline or evaluated script, no plugins, never framed.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self'; \
    worker-src 'self' blob:; child-src 'self' blob:; object-src 'none'; base-uri 'self'; \
    form-action 'self'; frame-ancestors 'none'";

/// Security headers on every response, the API's and the UI's: the
/// content security policy, no MIME sniffing, no framing, no referrer, no
/// powerful browser features, HSTS when served over TLS (here or at a
/// proxy: `OT_PUBLIC_TLS`), and nothing from the API kept in a cache but
/// basemap tiles.
async fn security_headers(State(hsts): State<bool>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let api = path.starts_with("/api/");
    let tile = path.starts_with("/api/v1/basemap/");
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let set = |h: &mut axum::http::HeaderMap, k: &'static str, v: &'static str| {
        h.insert(
            axum::http::HeaderName::from_static(k),
            HeaderValue::from_static(v),
        );
    };
    set(h, "content-security-policy", CSP);
    set(h, "x-content-type-options", "nosniff");
    set(h, "x-frame-options", "DENY");
    set(h, "referrer-policy", "no-referrer");
    set(
        h,
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), payment=(), usb=(), serial=(), bluetooth=()",
    );
    set(h, "cross-origin-opener-policy", "same-origin");
    if hsts {
        set(
            h,
            "strict-transport-security",
            "max-age=31536000; includeSubDomains",
        );
    }
    // A basemap tile is the one API answer a browser may keep (a day); its
    // errors are not.
    if api && !(tile && h.contains_key(CACHE_CONTROL)) {
        h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    }
    res
}

/// The UI's content-hashed build files. A missing one is a 404, never the
/// page: a browser still holding a page from before a deploy then gets a
/// clean load error (and reloads) instead of HTML where it expects a script.
fn assets(dir: &std::path::Path) -> SetResponseHeader<ServeDir, HeaderValue> {
    SetResponseHeader::overriding(
        ServeDir::new(dir.join("assets")),
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    )
}

/// The single-page app: any other path is the page, which is never cached so
/// a deploy takes effect on the next load.
fn page(dir: &std::path::Path) -> SetResponseHeader<ServeDir<ServeFile>, HeaderValue> {
    SetResponseHeader::overriding(
        ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html"))),
        CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    )
}

/// Every dependency's state. 503 (with the same body) when SQLite, Redis or
/// NATS is down, so a monitor that reads only the status code sees it;
/// `/healthz` stays liveness only.
async fn status(
    State(s): State<AppState>,
    user: Option<axum::Extension<crate::auth::AuthUser>>,
) -> (StatusCode, Json<Value>) {
    let admin = user.is_none_or(|u| u.role == crate::auth::Role::Admin);
    let sqlite = match s.with_db(|db| db.schema_version()).await {
        Ok(v) => json!({ "ok": true, "schema_version": v, "path": s.common.sqlite }),
        Err(e) => json!({ "ok": false, "error": e.message }),
    };
    let redis = match tokio::time::timeout(Duration::from_secs(2), s.redis.ping()).await {
        Ok(Ok(())) => json!({ "ok": true, "namespace": s.common.redis_namespace }),
        Ok(Err(e)) => json!({ "ok": false, "error": e.to_string() }),
        Err(_) => json!({ "ok": false, "error": "timed out" }),
    };
    let nats = match tokio::time::timeout(Duration::from_secs(3), s.nats.status()).await {
        Ok(n) => {
            let mut v = json!(n);
            v["ok"] = json!(n.connected && n.stream_error.is_none());
            v["url"] = json!(s.common.nats.url);
            if !n.connected {
                v["error"] = json!(format!("not connected to {}", s.common.nats.url));
            } else if let Some(e) = &n.stream_error {
                v["error"] = json!(e);
            }
            v
        }
        Err(_) => json!({ "ok": false, "error": "timed out" }),
    };
    // Not counted in `up`: a collector outage must not make an
    // orchestrator restart OpenTrack (it keeps working, and logs stay on
    // standard output).
    let telemetry = crate::telemetry::status(&s.redis).await;
    let up = [&sqlite, &redis, &nats].iter().all(|d| d["ok"] == true);
    let code = if up {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = json!({
        "service": "opentrack",
        "version": env!("CARGO_PKG_VERSION"),
        "algorithms": {
            "correlation": crate::correlate::VERSION,
            "trackers": {
                "gnn": ot_source::tracker::GNN_VERSION,
                "mht": ot_source::tracker::MHT_VERSION,
            },
        },
        "site": s.common.site.as_str(),
        "node_id": s.common.node_id(),
        "sqlite": sqlite,
        "redis": redis,
        "nats": nats,
        "telemetry": telemetry,
    });
    (
        code,
        Json(if admin {
            body
        } else {
            status_for_non_admins(body)
        }),
    )
}

/// What `/status` shows a viewer or track manager: whether each part is up,
/// and the versions; not paths, URLs, endpoints or error text (ASD V-222600).
fn status_for_non_admins(mut body: Value) -> Value {
    const KEEP: &[&str] = &[
        "ok",
        "configured",
        "connected",
        "stream",
        "tracks_subject",
        "namespace",
        "schema_version",
    ];
    for part in ["sqlite", "redis", "nats", "telemetry"] {
        if let Some(Value::Object(o)) = body.get_mut(part) {
            let down = o.contains_key("error");
            o.retain(|k, _| KEEP.contains(&k.as_str()));
            if down {
                o.insert("error".into(), json!("unavailable (an admin can see why)"));
            }
        }
    }
    body
}

async fn track(
    State(s): State<AppState>,
    Path(uid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let uid = parse_uid(&uid)?;
    let track = s
        .redis
        .get_system_track(uid)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("system track {uid}")))?;
    Ok(Json(json!({
        "confidence": track.confidence(),
        "track": track,
        "message": ot_core::wire::to_message(&track, &s.common.publish_context(), chrono::Utc::now()),
        "subject": ot_core::wire::subject(&s.common.nats.tracks_subject, uid),
    })))
}

async fn explain(
    State(s): State<AppState>,
    Path(uid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let uid = parse_uid(&uid)?;
    let edges = s.with_db(move |db| db.explain(uid)).await?;
    if edges.is_empty() {
        return Err(ApiError::not_found(format!("system track {uid}")));
    }
    Ok(Json(json!({ "uid": uid, "edges": edges })))
}

/// Accepts a bare UID or a `tms-<UID>` track id.
pub(crate) fn parse_uid(raw: &str) -> Result<Uid, ApiError> {
    Uid::from_doc_id(raw)
        .or_else(|_| raw.parse())
        .map_err(|e| ApiError::bad_request(e.to_string()))
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    pub(crate) message: String,
}

impl ApiError {
    pub(crate) fn internal(m: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: m.into(),
        }
    }
    pub(crate) fn not_found(m: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: m.into(),
        }
    }
    pub(crate) fn unprocessable(m: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: m.into(),
        }
    }
    pub(crate) fn conflict(m: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: m.into(),
        }
    }
    pub(crate) fn unauthorized(m: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: m.into(),
        }
    }
    pub(crate) fn forbidden(m: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: m.into(),
        }
    }
    pub(crate) fn bad_request(m: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: m.into(),
        }
    }
}

impl From<ot_store::StoreError> for ApiError {
    fn from(e: ot_store::StoreError) -> Self {
        match e {
            ot_store::StoreError::NotFound(m) => Self::not_found(m),
            ot_store::StoreError::Conflict(m) => Self {
                status: StatusCode::CONFLICT,
                message: m,
            },
            other => Self::internal(other.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // A server-side failure's text (SQLite, Redis, file paths) stays in
        // the log; the caller gets a reference to find it (SI-11).
        if self.status.is_server_error() {
            let reference = crate::fips::random_hex::<6>();
            tracing::error!(%reference, status = self.status.as_u16(), error = %self.message, "request failed");
            let message =
                format!("internal error (reference {reference}); the server log has the detail");
            return (
                self.status,
                Json(json!({ "error": message, "reference": reference })),
            )
                .into_response();
        }
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_admins_see_status_without_paths_urls_or_errors() {
        let body = json!({
            "version": "0.4.3",
            "sqlite": { "ok": true, "schema_version": 20, "path": "/data/opentrack.db" },
            "redis": { "ok": false, "error": "connection refused (os error 111)" },
            "nats": { "ok": true, "url": "nats://token@nats:4222", "connected": true, "server_name": "n1", "stream": "TRACKS" },
            "telemetry": { "configured": true, "ok": false, "error": "collector: refused", "endpoints": ["grpc https://c:4317"], "processes": [] },
        });
        let v = status_for_non_admins(body);
        assert_eq!(v["version"], "0.4.3");
        assert!(v["sqlite"].get("path").is_none());
        assert_eq!(v["sqlite"]["schema_version"], 20);
        assert_eq!(v["redis"]["ok"], false);
        assert!(!v["redis"]["error"].as_str().unwrap().contains("111"));
        assert!(v["nats"].get("url").is_none() && v["nats"].get("server_name").is_none());
        assert!(
            v["telemetry"].get("endpoints").is_none() && v["telemetry"].get("processes").is_none()
        );
    }

    #[tokio::test]
    async fn a_server_error_answers_with_a_reference_not_its_text() {
        use http_body_util::BodyExt;
        let res = ApiError::internal("disk I/O error at /data/opentrack.db").into_response();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(!v["error"].as_str().unwrap().contains("/data"), "{v}");
        assert!(v["reference"].as_str().is_some_and(|r| r.len() == 12));
        let res = ApiError::not_found("system track x").into_response();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn every_response_carries_the_security_headers() {
        use tower::ServiceExt;
        let app = |hsts| {
            Router::new()
                .route("/api/v1/x", get(|| async { "{}" }))
                .route("/page", get(|| async { "<html>" }))
                .layer(axum::middleware::from_fn_with_state(hsts, security_headers))
        };
        let get = |uri: &str| {
            axum::http::Request::builder()
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let res = app(false).oneshot(get("/api/v1/x")).await.unwrap();
        let h = res.headers();
        assert!(
            h["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["x-frame-options"], "DENY");
        assert_eq!(h["referrer-policy"], "no-referrer");
        assert_eq!(h["cache-control"], "no-store");
        assert!(h.get("strict-transport-security").is_none());
        let res = app(true).oneshot(get("/page")).await.unwrap();
        let h = res.headers();
        assert!(h.get("cache-control").is_none(), "the UI sets its own");
        assert!(
            h["strict-transport-security"]
                .to_str()
                .unwrap()
                .starts_with("max-age=")
        );
    }

    #[test]
    fn uid_path_accepts_doc_id_or_bare_uid() {
        assert_eq!(
            parse_uid("tms-OTK000000007").unwrap().to_string(),
            "OTK000000007"
        );
        assert_eq!(
            parse_uid("OTK000000007").unwrap().to_string(),
            "OTK000000007"
        );
        assert_eq!(
            parse_uid("ais-1").unwrap_err().status,
            StatusCode::BAD_REQUEST
        );
    }
}
