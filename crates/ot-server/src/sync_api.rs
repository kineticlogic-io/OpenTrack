//! How sharing the picture with other nodes is going (see
//! `docs/multi-node.md`): the settings, the peers the link has heard, the
//! replicated decision log, and how many tracks this node reports.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::control::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new().route("/sync/status", get(status))
}

async fn status(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let (saved, heads, counts) = s
        .with_db(|db| Ok((db.app_settings()?, db.sync_heads()?, db.sync_counts()?)))
        .await?;
    let settings = crate::settings_api::sync_settings(&saved);
    let live = s
        .redis
        .sync_status()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let heads: Vec<Value> = heads
        .into_iter()
        .map(|(site, seq)| json!({ "site": site.to_string(), "seq": seq }))
        .collect();
    Ok(Json(json!({
        "site": s.common.site.to_string(),
        "settings": settings,
        "log": { "heads": heads, "by_status": counts },
        "link": live.get("link"),
        "engine": live.get("engine"),
    })))
}
