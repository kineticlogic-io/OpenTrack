//! The control plane: REST API and the React UI.
//!
//! Health, a status summary of every dependency, system tracks and their
//! graph history here; sources, registry and metrics in [`crate::api`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ot_core::Uid;
use ot_store::{Db, RedisStore};
use serde_json::{Value, json};
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::config::Common;

#[derive(Clone)]
pub struct AppState {
    pub common: Common,
    pub db: Arc<Mutex<Db>>,
    pub redis: RedisStore,
    pub nats: ot_nats::Nats,
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
        .merge(crate::metrics::routes());

    let mut app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api/v1", api)
        .with_state(state);

    if let Some(dir) = ui_dir.filter(|d| d.join("index.html").is_file()) {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)));
    }
    app.layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
}

async fn status(State(s): State<AppState>) -> Json<Value> {
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
    Json(json!({
        "service": "opentrack",
        "version": env!("CARGO_PKG_VERSION"),
        "site": s.common.site.as_str(),
        "node_id": s.common.node_id(),
        "sqlite": sqlite,
        "redis": redis,
        "nats": nats,
    }))
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
fn parse_uid(raw: &str) -> Result<Uid, ApiError> {
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
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
