//! SAML 2.0 single sign-on, ported from OpenStare's: OpenTrack is the
//! service provider. `/auth/saml/login` sends the browser to the identity
//! provider with a signed-request id in the relay state; the provider
//! posts its signed assertion to `/auth/saml/acs`, which checks it (the
//! signature, the conditions, that it answers our request, that it was not
//! used before), finds or makes the account, and starts a session.
//!
//! Only the HTTP-Redirect binding for requests and the POST binding for
//! responses; no identity-provider-initiated sign-on.

use std::collections::HashMap;
use std::io::Write;
use std::str::FromStr;
use std::sync::Mutex;

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Form, State as AxumState};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use base64::Engine;
use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use samael::metadata::EntityDescriptor;
use samael::schema::Assertion;
use samael::service_provider::{ServiceProvider, ServiceProviderBuilder};
use samael::traits::ToXml;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::api::{Client, How};
use super::{AuthSettings, Role, settings::SamlSettings};
use crate::control::{ApiError, AppState};

const HTTP_REDIRECT: &str = "urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect";
/// How long a sign-on may take between our request and the answer.
const RELAY_MINUTES: i64 = 5;

/// Assertions already used, until they expire (a replayed one is refused).
#[derive(Default)]
pub struct State {
    seen: Mutex<HashMap<String, DateTime<Utc>>>,
}

impl State {
    fn first_use(&self, id: &str, until: DateTime<Utc>) -> bool {
        let Ok(mut m) = self.seen.lock() else {
            return false;
        };
        let now = Utc::now();
        m.retain(|_, t| *t > now);
        if m.contains_key(id) {
            return false;
        }
        m.insert(id.to_owned(), until);
        true
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/saml/login", get(login))
        .route("/auth/saml/acs", post(acs))
        .route("/auth/saml/metadata", get(metadata))
        .route("/auth/saml/parse-metadata", post(parse_metadata))
}

fn base_url(s: &AppState) -> Result<String, ApiError> {
    s.auth.public_url.clone().ok_or_else(|| {
        ApiError::bad_request(
            "set OT_PUBLIC_URL (where browsers reach OpenTrack) for single sign-on",
        )
    })
}

fn service_provider(cfg: &SamlSettings, base: &str) -> Result<ServiceProvider, String> {
    let idp = EntityDescriptor::from_str(&cfg.idp_metadata_xml)
        .map_err(|e| format!("the identity provider's metadata: {e}"))?;
    ServiceProviderBuilder::default()
        .entity_id(format!("{base}/api/v1/auth/saml/metadata"))
        .acs_url(format!("{base}/api/v1/auth/saml/acs"))
        .idp_metadata(idp)
        .allow_idp_initiated(false)
        .build()
        .map_err(|e| format!("building the service provider: {e}"))
}

#[derive(Serialize, Deserialize)]
struct Relay {
    rid: String,
    exp: i64,
}

fn mint_relay(rid: &str, secret: &[u8]) -> Result<String, String> {
    let claims = Relay {
        rid: rid.to_owned(),
        exp: (Utc::now() + chrono::Duration::minutes(RELAY_MINUTES)).timestamp(),
    };
    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|e| e.to_string())
}

fn verify_relay(token: &str, secret: &[u8]) -> Option<String> {
    let mut v = Validation::new(Algorithm::HS256);
    v.validate_exp = true;
    jsonwebtoken::decode::<Relay>(token, &DecodingKey::from_secret(secret), &v)
        .ok()
        .map(|d| d.claims.rid)
}

fn deflate_base64(bytes: &[u8]) -> std::io::Result<String> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(bytes)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(e.finish()?))
}

/// The identity provider's sign-in page, with our request.
fn redirect_url(s: &AppState, cfg: &SamlSettings) -> Result<String, String> {
    let base = base_url(s).map_err(|e| e.message)?;
    let sp = service_provider(cfg, &base)?;
    let sso = sp
        .sso_binding_location(HTTP_REDIRECT)
        .ok_or("the identity provider has no HTTP-Redirect sign-on")?;
    let authn = sp
        .make_authentication_request(&sso)
        .map_err(|e| format!("the sign-on request: {e}"))?;
    let xml = authn.to_string().map_err(|e| e.to_string())?;
    let relay = mint_relay(&authn.id, &s.auth.secret)?;
    let request = deflate_base64(xml.as_bytes()).map_err(|e| e.to_string())?;
    url::Url::parse_with_params(
        &sso,
        &[("SAMLRequest", request.as_str()), ("RelayState", &relay)],
    )
    .map(|u| u.to_string())
    .map_err(|e| e.to_string())
}

async fn login(AxumState(s): AxumState<AppState>) -> Response {
    let cfg = s.auth.settings().saml;
    if !cfg.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    match redirect_url(&s, &cfg).and_then(|u| HeaderValue::from_str(&u).map_err(|e| e.to_string()))
    {
        Ok(loc) => (StatusCode::FOUND, [(header::LOCATION, loc)]).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "SAML sign-on could not start");
            (StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    }
}

#[derive(Deserialize)]
struct AcsForm {
    #[serde(rename = "SAMLResponse")]
    response: Option<String>,
    #[serde(rename = "RelayState")]
    relay: Option<String>,
}

/// Back to the sign-in page with a message (which check failed stays in
/// the log, not the page).
fn refused(why: &str) -> Response {
    tracing::info!(reason = why, "SAML sign-on refused");
    (
        StatusCode::FOUND,
        [(
            header::LOCATION,
            HeaderValue::from_static("/login?sso=failed"),
        )],
    )
        .into_response()
}

fn name_id(a: &Assertion) -> Option<String> {
    a.subject
        .as_ref()?
        .name_id
        .as_ref()
        .map(|n| n.value.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn attribute(a: &Assertion, name: &str) -> Vec<String> {
    a.attribute_statements
        .iter()
        .flatten()
        .flat_map(|st| st.attributes.iter())
        .filter(|at| at.name.as_deref() == Some(name) || at.friendly_name.as_deref() == Some(name))
        .flat_map(|at| at.values.iter())
        .filter_map(|v| v.value.clone())
        .collect()
}

/// The account a verified assertion signs in: an existing one by email
/// (a SAML-made account takes the role the provider gives it now), or a
/// new one with the mapped role.
fn account(
    db: &mut ot_store::Db,
    email: &str,
    role: Option<Role>,
) -> Result<Option<ot_store::User>, ot_store::StoreError> {
    if let Some(u) = db.user_by_email(email)? {
        if !u.active {
            return Ok(None);
        }
        return match (u.origin.as_str(), role) {
            ("saml", Some(r)) if r.as_str() != u.role => {
                let after = db.update_user(&u.id, None, Some(r.as_str()), None)?;
                db.record(&ot_store::Decision {
                    before: Some(json!(u)),
                    after: Some(json!(after)),
                    ..ot_store::Decision::new("saml", "update_user")
                        .reason("the identity provider gave another role")
                })?;
                Ok(Some(after))
            }
            ("saml", None) => Ok(None),
            _ => Ok(Some(u)),
        };
    }
    let Some(role) = role else {
        return Ok(None);
    };
    let id = uuid::Uuid::new_v4().to_string();
    let u = db.create_user(&ot_store::NewUser {
        id: &id,
        email,
        name: "",
        role: role.as_str(),
        password_hash: None,
        origin: "saml",
    })?;
    db.record(&ot_store::Decision {
        after: Some(json!(u)),
        ..ot_store::Decision::new("saml", "create_user").reason("first single sign-on")
    })?;
    Ok(Some(u))
}

async fn acs_checked(s: &AppState, f: AcsForm, client: &Client) -> Result<Response, String> {
    let cfg = s.auth.settings().saml;
    if !cfg.enabled {
        return Err("SAML is off".into());
    }
    let (Some(response), Some(relay)) = (f.response, f.relay) else {
        return Err("no SAMLResponse or RelayState".into());
    };
    let Some(rid) = verify_relay(&relay, &s.auth.secret) else {
        return Err("the relay state is not ours or has expired".into());
    };
    let Ok(base) = base_url(s) else {
        return Err("no OT_PUBLIC_URL".into());
    };
    let c = cfg.clone();
    let parsed = tokio::task::spawn_blocking(move || {
        let sp = service_provider(&c, &base)?;
        sp.parse_base64_response(&response, Some(&[rid.as_str()]))
            .map_err(|e| format!("the assertion: {e}"))
    })
    .await;
    let assertion = match parsed {
        Ok(Ok(a)) => a,
        Ok(Err(e)) => return Err(e),
        Err(e) => return Err(e.to_string()),
    };
    let until = assertion
        .conditions
        .as_ref()
        .and_then(|c| c.not_on_or_after)
        .unwrap_or_else(|| Utc::now() + chrono::Duration::minutes(10));
    if !s.auth.saml.first_use(&assertion.id, until) {
        return Err("the assertion was used before".into());
    }
    let Some(email) = name_id(&assertion) else {
        return Err("the assertion names no one".into());
    };
    let values = attribute(&assertion, &cfg.role_attribute);
    let role = AuthSettings::map_role(&cfg.role_mapping, &values, cfg.default_role).map(|r| {
        if r == Role::Admin && !cfg.allow_admin {
            Role::TrackManager
        } else {
            r
        }
    });
    let e = email.clone();
    let user = match s.with_db(move |db| account(db, &e, role)).await {
        Ok(Some(u)) => u,
        Ok(None) => return Err("no role for this user, or the account is off".into()),
        Err(e) => return Err(e.message),
    };
    match super::api::start_session(s, user, client, How::Saml).await {
        Ok((mut headers, _)) => {
            headers.insert(header::LOCATION, HeaderValue::from_static("/"));
            Ok((StatusCode::FOUND, headers).into_response())
        }
        Err(e) => Err(e.message),
    }
}

/// The assertion consumer service: sign in, or back to the sign-in page
/// with the refusal in the audit record.
async fn acs(
    AxumState(s): AxumState<AppState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Form(f): Form<AcsForm>,
) -> Response {
    let client = Client::new(peer.as_ref().map(|p| &p.0), &headers);
    match acs_checked(&s, f, &client).await {
        Ok(r) => r,
        Err(why) => {
            let e = ot_store::AuditEvent::new("saml", "login")
                .failure()
                .ip(&client.ip)
                .detail(json!({ "reason": why, "via": "saml" }));
            if let Err(e) = s.with_db(move |db| db.audit(&e)).await {
                tracing::error!(error = %e.message, "recording a refused sign-on failed");
            }
            refused(&why)
        }
    }
}

/// Our service-provider metadata, for the identity provider's admin.
async fn metadata(AxumState(s): AxumState<AppState>) -> Result<Response, ApiError> {
    let base = base_url(&s)?;
    let cfg = s.auth.settings().saml;
    let xml = service_provider(&cfg, &base)
        .and_then(|sp| {
            sp.metadata()
                .map_err(|e| e.to_string())?
                .to_string()
                .map_err(|e| e.to_string())
        })
        .map_err(ApiError::unprocessable)?;
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/samlmetadata+xml"),
    );
    Ok((h, xml).into_response())
}

#[derive(Deserialize)]
struct Metadata {
    xml: String,
}

/// Read an identity provider's metadata into the settings' fields.
async fn parse_metadata(Json(b): Json<Metadata>) -> Result<Json<Value>, ApiError> {
    let ed = EntityDescriptor::from_str(&b.xml)
        .map_err(|e| ApiError::unprocessable(format!("not SAML metadata: {e}")))?;
    let idps = ed
        .idp_sso_descriptors
        .as_ref()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::unprocessable("no identity provider in the metadata"))?;
    let sso_url = idps
        .iter()
        .flat_map(|d| d.single_sign_on_services.iter())
        .find(|ep| ep.binding == HTTP_REDIRECT)
        .map(|ep| ep.location.clone())
        .ok_or_else(|| ApiError::unprocessable("the metadata has no HTTP-Redirect sign-on"))?;
    let cert = idps
        .iter()
        .flat_map(|d| d.key_descriptors.iter())
        .filter(|k| k.is_signing() || k.key_use.is_none())
        .filter_map(|k| k.key_info.x509_data.as_ref())
        .flat_map(|x| x.certificates.iter())
        .map(|c| c.trim().to_owned())
        .find(|c| !c.is_empty())
        .ok_or_else(|| ApiError::unprocessable("the metadata has no signing certificate"))?;
    Ok(Json(json!({
        "idp_metadata_xml": b.xml,
        "idp_entity_id": ed.entity_id.unwrap_or_default(),
        "sso_url": sso_url,
        "signing_cert": cert,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &[u8] = b"k1k1k1k1k1k1k1k1k1k1k1k1k1k1k1k1";
    const K2: &[u8] = b"k2k2k2k2k2k2k2k2k2k2k2k2k2k2k2k2";

    #[test]
    fn relay_state_is_ours() {
        let t = mint_relay("id-1", K1).unwrap();
        assert_eq!(verify_relay(&t, K1).as_deref(), Some("id-1"));
        assert!(verify_relay(&t, K2).is_none());
    }

    #[test]
    fn an_assertion_is_used_once() {
        let st = State::default();
        let until = Utc::now() + chrono::Duration::minutes(5);
        assert!(st.first_use("a1", until));
        assert!(!st.first_use("a1", until));
        assert!(st.first_use("a2", until));
    }

    #[test]
    fn single_sign_on_finds_or_makes_the_account() {
        let mut db = ot_store::Db::open_in_memory().unwrap();
        // No role: refused, nothing made.
        assert!(account(&mut db, "n@x.org", None).unwrap().is_none());
        assert_eq!(db.user_count().unwrap(), 0);
        let u = account(&mut db, "n@x.org", Some(Role::Viewer))
            .unwrap()
            .unwrap();
        assert_eq!((u.origin.as_str(), u.role.as_str()), ("saml", "viewer"));
        assert!(!u.has_password);
        // The provider's role holds on the next sign-on.
        let u = account(&mut db, "n@x.org", Some(Role::TrackManager))
            .unwrap()
            .unwrap();
        assert_eq!(u.role, "track_manager");
        // A local account keeps the role an admin gave it.
        db.create_user(&ot_store::NewUser {
            id: "l1",
            email: "l@x.org",
            name: "",
            role: "admin",
            password_hash: None,
            origin: "local",
        })
        .unwrap();
        let u = account(&mut db, "l@x.org", Some(Role::Viewer))
            .unwrap()
            .unwrap();
        assert_eq!(u.role, "admin");
    }
}
