//! The decision log: every change to the picture and the configuration,
//! who made it and why.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::control::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/decisions", get(list))
        .route("/decisions/{id}/undo", post(undo))
}

#[derive(Deserialize)]
struct ListQuery {
    /// Comma-separated operations (`pair,merge`).
    op: String,
    limit: Option<usize>,
}

async fn list(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = q.limit.unwrap_or(100).min(1000);
    let ops: Vec<String> =
        q.op.split(',')
            .map(str::trim)
            .filter(|o| !o.is_empty())
            .map(str::to_owned)
            .collect();
    let rows = s
        .with_db(move |db| {
            let ops: Vec<&str> = ops.iter().map(String::as_str).collect();
            db.decisions_by_op(&ops, limit)
        })
        .await?;
    Ok(Json(json!({ "decisions": rows })))
}

#[derive(Deserialize, Default)]
struct UndoBody {
    reason: Option<String>,
}

/// Undo a track management decision (the engine does it, as a decision of
/// its own): a merge or split, a pairing, a delete, a group change.
async fn undo(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    body: Option<Json<UndoBody>>,
) -> Result<Json<Value>, ApiError> {
    let reason = body.and_then(|b| b.0.reason);
    crate::correlation_api::command(
        &s,
        &headers,
        json!({ "op": "undo", "decision": id, "reason": reason }),
    )
    .await
}
