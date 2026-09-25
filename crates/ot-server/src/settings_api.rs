//! The Settings tab over the API: instance settings (site name,
//! classification banner), data export, and purging the tracks.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::actor;
use crate::control::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings", get(get_settings).put(put_settings))
        .route("/public/banner", get(banner))
        .route("/export/tracks", get(export_tracks))
        .route("/export/config", get(export_config))
        .route("/admin/purge", post(purge))
}

/// Settings an admin changes from the Settings tab.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppSettings {
    /// Shown in the header and the browser title; the site code in track
    /// UIDs is fixed at deployment (`OT_SITE_CODE`).
    pub site_name: String,
    pub banner: BannerSettings,
}

/// The classification banner: off, set here, or OpenStare's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BannerSettings {
    pub mode: BannerMode,
    pub text: String,
    /// CSS colours, `#rrggbb`.
    pub background: String,
    pub color: String,
    /// OpenStare's base URL, for `mode: openstare` (its `/api/public/banner`).
    pub openstare_url: String,
}

impl Default for BannerSettings {
    fn default() -> Self {
        Self {
            mode: BannerMode::Off,
            text: "UNCLASSIFIED".into(),
            background: "#007a33".into(),
            color: "#ffffff".into(),
            openstare_url: "http://127.0.0.1".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BannerMode {
    #[default]
    Off,
    Manual,
    Openstare,
}

fn hex_colour(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

impl AppSettings {
    fn validate(&self) -> Result<(), String> {
        if self.site_name.chars().count() > 64 {
            return Err("site_name is at most 64 characters".into());
        }
        let b = &self.banner;
        if b.mode == BannerMode::Manual {
            if b.text.trim().is_empty() || b.text.chars().count() > 128 {
                return Err("banner text is 1 to 128 characters".into());
            }
            if !hex_colour(&b.background) || !hex_colour(&b.color) {
                return Err("banner colours are #rrggbb".into());
            }
        }
        if b.mode == BannerMode::Openstare
            && !(b.openstare_url.starts_with("http://") || b.openstare_url.starts_with("https://"))
        {
            return Err("openstare_url is an http(s) URL".into());
        }
        Ok(())
    }
}

async fn load(s: &AppState) -> Result<AppSettings, ApiError> {
    let v = s.with_db(|db| db.app_settings()).await?;
    serde_json::from_value(v).map_err(|e| ApiError::internal(format!("saved settings: {e}")))
}

async fn get_settings(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let settings = load(&s).await?;
    Ok(Json(json!({
        "settings": settings,
        "site_code": s.common.site.to_string(),
        "node_id": s.common.node_id(),
    })))
}

async fn put_settings(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(settings): Json<AppSettings>,
) -> Result<Json<Value>, ApiError> {
    settings.validate().map_err(ApiError::unprocessable)?;
    let (v, who) = (
        serde_json::to_value(&settings).unwrap_or_default(),
        actor(&headers),
    );
    s.with_db(move |db| db.put_app_settings(&v, &who)).await?;
    *BANNER_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    get_settings(State(s)).await
}

/// The banner as OpenStare's `/api/public/banner` shapes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Banner {
    enabled: bool,
    text: String,
    background: String,
    color: String,
}

/// OpenStare's banner, fetched at most once a minute.
static BANNER_CACHE: Mutex<Option<(Instant, String, Banner)>> = Mutex::new(None);
const BANNER_TTL: Duration = Duration::from_secs(60);

/// The banner to show (unauthenticated, like OpenStare's): off, the one set
/// here, or OpenStare's (with `source`, and `error` when it cannot be read).
async fn banner(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let b = load(&s).await?.banner;
    let off = json!({"enabled": false, "text": "", "background": b.background, "color": b.color});
    match b.mode {
        BannerMode::Off => Ok(Json(json!({"source": "off", "banner": off}))),
        BannerMode::Manual => Ok(Json(json!({"source": "manual", "banner": {
            "enabled": true, "text": b.text, "background": b.background, "color": b.color,
        }}))),
        BannerMode::Openstare => {
            let url = format!(
                "{}/api/public/banner",
                b.openstare_url.trim_end_matches('/')
            );
            if let Some((at, u, cached)) = BANNER_CACHE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                && u == url
                && at.elapsed() < BANNER_TTL
            {
                return Ok(Json(json!({"source": "openstare", "banner": cached})));
            }
            let fetched = async {
                let r = reqwest::Client::new()
                    .get(&url)
                    .timeout(Duration::from_secs(3))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                if !r.status().is_success() {
                    return Err(format!("{url}: HTTP {}", r.status()));
                }
                r.json::<Banner>().await.map_err(|e| e.to_string())
            }
            .await;
            match fetched {
                Ok(banner) => {
                    *BANNER_CACHE.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some((Instant::now(), url, banner.clone()));
                    Ok(Json(json!({"source": "openstare", "banner": banner})))
                }
                // Without OpenStare's answer, show the one set here rather
                // than nothing: a banner that silently vanishes is worse.
                Err(e) => Ok(Json(json!({"source": "manual", "error": e, "banner": {
                    "enabled": true, "text": b.text, "background": b.background, "color": b.color,
                }}))),
            }
        }
    }
}

#[derive(Deserialize)]
struct ExportQuery {
    #[serde(default = "geojson")]
    format: String,
}

fn geojson() -> String {
    "geojson".into()
}

fn download(
    bytes: Vec<u8>,
    content_type: &'static str,
    name: String,
) -> Result<Response, ApiError> {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .map_err(|e| ApiError::internal(e.to_string()))?,
    );
    Ok((StatusCode::OK, headers, bytes).into_response())
}

fn stamp() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M").to_string()
}

/// Every live system track as published (the GOLD fields and attributes),
/// as GeoJSON or CSV.
async fn export_tracks(
    State(s): State<AppState>,
    Query(q): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    let tracks = s.redis.list_system_tracks().await?;
    let (ctx, now) = (s.common.publish_context(), chrono::Utc::now());
    let messages: Vec<(ot_core::SystemTrack, ot_core::wire::TrackMessage)> = tracks
        .into_iter()
        .map(|t| {
            let m = ot_core::wire::to_message(&t, &ctx, now);
            (t, m)
        })
        .collect();
    match q.format.as_str() {
        "geojson" => {
            let features: Vec<Value> = messages
                .iter()
                .map(|(t, m)| {
                    let mut props = serde_json::to_value(m).unwrap_or_default();
                    if let Some(p) = props.as_object_mut() {
                        p.remove("lat");
                        p.remove("lon");
                        p.insert("state".into(), json!(t.state));
                        p.insert("confidence".into(), json!(t.confidence()));
                        p.insert(
                            "sources".into(),
                            json!(
                                t.contributors
                                    .iter()
                                    .map(|c| format!("{}/{}", c.source_id, c.source_track_key))
                                    .collect::<Vec<_>>()
                            ),
                        );
                    }
                    json!({"type": "Feature", "id": m.track_id,
                           "geometry": {"type": "Point", "coordinates": [m.lon, m.lat]},
                           "properties": props})
                })
                .collect();
            let body = serde_json::to_vec_pretty(
                &json!({"type": "FeatureCollection", "features": features}),
            )
            .map_err(|e| ApiError::internal(e.to_string()))?;
            download(
                body,
                "application/geo+json",
                format!("opentrack-tracks-{}.geojson", stamp()),
            )
        }
        "csv" => {
            let mut w = csv::Writer::from_writer(Vec::new());
            let header = [
                "track_id",
                "name",
                "class",
                "domain",
                "affiliation",
                "force_code",
                "track_type",
                "sidc",
                "time",
                "lat",
                "lon",
                "state",
                "confidence",
                "published",
                "sources",
                "attributes",
            ];
            w.write_record(header)
                .map_err(|e| ApiError::internal(e.to_string()))?;
            for (t, m) in &messages {
                let text = |v: Value| match v {
                    Value::String(s) => s,
                    other => other.to_string(),
                };
                w.write_record([
                    m.track_id.clone(),
                    m.name.clone(),
                    m.class.clone(),
                    m.domain.clone(),
                    m.affiliation.clone(),
                    m.force_code.to_string(),
                    text(json!(m.track_type)),
                    m.sidc.code.clone(),
                    m.time.to_rfc3339(),
                    m.lat.to_string(),
                    m.lon.to_string(),
                    text(json!(t.state)),
                    t.confidence().to_string(),
                    t.is_published().to_string(),
                    t.contributors
                        .iter()
                        .map(|c| format!("{}/{}", c.source_id, c.source_track_key))
                        .collect::<Vec<_>>()
                        .join(" "),
                    Value::Object(m.attributes.clone()).to_string(),
                ])
                .map_err(|e| ApiError::internal(e.to_string()))?;
            }
            let body = w
                .into_inner()
                .map_err(|e| ApiError::internal(e.to_string()))?;
            download(
                body,
                "text/csv; charset=utf-8",
                format!("opentrack-tracks-{}.csv", stamp()),
            )
        }
        other => Err(ApiError::bad_request(format!(
            "format {other:?}: use geojson or csv"
        ))),
    }
}

/// The configuration as one JSON document, for backup: sources, output
/// schema versions, correlation and instance settings.
async fn export_config(State(s): State<AppState>) -> Result<Response, ApiError> {
    let (sources, schema, correlation, app) = s
        .with_db(|db| {
            Ok((
                db.list_sources()?,
                db.schema_versions()?,
                db.correlation_settings()?,
                db.app_settings()?,
            ))
        })
        .await?;
    let body = serde_json::to_vec_pretty(&json!({
        "opentrack": env!("CARGO_PKG_VERSION"),
        "site_code": s.common.site.to_string(),
        "exported_at": chrono::Utc::now(),
        "sources": sources.iter().map(|r| json!({"spec": r.spec, "enabled": r.enabled, "priority": r.priority})).collect::<Vec<_>>(),
        "schema_versions": schema.iter().map(|v| json!({"version": v.version, "status": v.status, "notes": v.notes, "fields": v.fields})).collect::<Vec<_>>(),
        "correlation_settings": correlation,
        "app_settings": app,
    }))
    .map_err(|e| ApiError::internal(e.to_string()))?;
    download(
        body,
        "application/json",
        format!("opentrack-config-{}.json", stamp()),
    )
}

#[derive(Deserialize)]
struct PurgeBody {
    /// The site code, typed to confirm.
    confirm: String,
    /// Also delete the track graph (how every track was formed and paired).
    #[serde(default)]
    history: bool,
}

/// Retire every live track (published ones are deleted downstream too) and,
/// with `history`, delete the track graph. Configuration, the registry,
/// cards and the decision log stay.
async fn purge(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PurgeBody>,
) -> Result<Json<Value>, ApiError> {
    if body.confirm.trim() != s.common.site.to_string() {
        return Err(ApiError::unprocessable(format!(
            "type the site code ({}) to confirm",
            s.common.site
        )));
    }
    crate::correlation_api::command_waiting(
        &s,
        &headers,
        json!({"op": "purge", "history": body.history}),
        Duration::from_secs(300),
    )
    .await
}
