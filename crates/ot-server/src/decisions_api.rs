//! The decision log: every change to the picture and the configuration,
//! who made it and why.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::control::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new().route("/decisions", get(list))
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
