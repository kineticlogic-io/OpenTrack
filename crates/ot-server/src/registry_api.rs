//! The registry over the API: browse and search entities with their cards,
//! and move the whole registry in and out as a spreadsheet (see
//! [`crate::registry_sheet`]).

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::actor;
use crate::cards::latest_schema;
use crate::control::{ApiError, AppState};
use crate::registry_sheet::{self, Format, Planned};

/// Largest sheet an import accepts.
const MAX_SHEET_BYTES: usize = 32 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/registry/entities", get(list))
        .route("/registry/export", get(export))
        .route(
            "/registry/import-sheet",
            post(import).layer(DefaultBodyLimit::max(MAX_SHEET_BYTES)),
        )
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    q: String,
    limit: Option<usize>,
    #[serde(default)]
    offset: usize,
}

/// A page of entities (all, or those whose name or identifier matches `q`),
/// each with its card.
async fn list(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let (page, total) = s
        .with_db(move |db| db.list_entities(&q.q, limit, q.offset))
        .await?;
    let entities: Vec<Value> = page
        .into_iter()
        .map(|(e, card)| {
            json!({
                "id": e.id, "name": e.name, "status": e.status, "fields": e.fields,
                "identifiers": e.identifiers,
                "card": card.as_ref().map(|c| &c.values),
                "card_updated_at_ms": card.as_ref().map(|c| c.updated_at_ms),
            })
        })
        .collect();
    Ok(Json(json!({ "entities": entities, "total": total })))
}

#[derive(Deserialize)]
struct FormatQuery {
    #[serde(default = "xlsx")]
    format: String,
}

fn xlsx() -> String {
    "xlsx".into()
}

fn format_of(q: &FormatQuery) -> Result<Format, ApiError> {
    Format::parse(&q.format)
        .ok_or_else(|| ApiError::bad_request(format!("format {:?}: use xlsx or csv", q.format)))
}

/// The whole registry with its cards as a spreadsheet download.
async fn export(
    State(s): State<AppState>,
    Query(q): Query<FormatQuery>,
) -> Result<Response, ApiError> {
    let format = format_of(&q)?;
    let schema = latest_schema(&s).await?;
    let (all, _) = s.with_db(|db| db.list_entities("", usize::MAX, 0)).await?;
    let bytes = tokio::task::spawn_blocking(move || {
        registry_sheet::write(&registry_sheet::export(&all, &schema), format)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(ApiError::internal)?;
    let name = format!(
        "opentrack-registry-{}.{}",
        chrono::Utc::now().format("%Y%m%d-%H%M"),
        format.extension()
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .map_err(|e| ApiError::internal(e.to_string()))?,
    );
    Ok((StatusCode::OK, headers, bytes).into_response())
}

#[derive(Deserialize)]
struct ImportQuery {
    #[serde(default = "xlsx")]
    format: String,
    /// Write the changes; otherwise only report what they would be.
    #[serde(default)]
    apply: bool,
    /// Recorded with the import decision (e.g. the file name).
    #[serde(default)]
    label: Option<String>,
}

/// Plan (and with `apply`, make) the changes a spreadsheet describes. Nothing
/// is written while any row has an error.
async fn import(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ImportQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let format = format_of(&FormatQuery {
        format: q.format.clone(),
    })?;
    let sheet = registry_sheet::read(&body, format).map_err(ApiError::unprocessable)?;
    let schema = latest_schema(&s).await?;
    let plan_schema = schema.clone();
    let planned = s
        .with_db(move |db| {
            // Lookups fail only on a database error: remember the first.
            let failure = std::cell::RefCell::new(None);
            fn keep<T>(
                failure: &std::cell::RefCell<Option<ot_store::StoreError>>,
                r: ot_store::sqlite::Result<Option<T>>,
            ) -> Option<T> {
                r.unwrap_or_else(|e| {
                    failure.borrow_mut().get_or_insert(e);
                    None
                })
            }
            let db: &ot_store::Db = db;
            let result = registry_sheet::plan(
                &sheet,
                &plan_schema,
                &mut |id| {
                    let e = keep(&failure, db.registry_entity(id))?;
                    let c = keep(&failure, db.card(id));
                    Some((e, c))
                },
                &mut |scheme, value| {
                    keep(&failure, db.registry_resolve(scheme, value)).map(|e| e.id)
                },
                &mut ot_store::new_entity_id,
            );
            match failure.into_inner() {
                Some(e) => Err(e),
                None => Ok(result),
            }
        })
        .await?;
    let rows: Vec<Planned> = planned.map_err(ApiError::unprocessable)?;
    let count = |a: &str| rows.iter().filter(|r| r.action == a).count();
    let counts = json!({
        "create": count("create"), "update": count("update"),
        "unchanged": count("unchanged"), "error": count("error"),
    });
    let errors = count("error");
    if !q.apply || errors > 0 {
        let status = if q.apply {
            StatusCode::UNPROCESSABLE_ENTITY
        } else {
            StatusCode::OK
        };
        return Ok((
            status,
            Json(json!({ "applied": false, "counts": counts, "rows": rows })),
        )
            .into_response());
    }
    let actor = actor(&headers);
    let label = q.label.unwrap_or_else(|| "spreadsheet import".into());
    let entities: Vec<ot_store::RegistryEntity> =
        rows.iter().filter_map(|r| r.entity.clone()).collect();
    let cards: Vec<(String, serde_json::Map<String, Value>)> = rows
        .iter()
        .filter_map(|r| r.card.clone().map(|c| (r.entity_id.clone(), c)))
        .collect();
    let version = schema.version;
    let imported = s
        .with_db(move |db| {
            let counts = db.registry_import(&entities, &actor, &label)?;
            for (id, values) in &cards {
                db.put_card(id, values, version, &actor)?;
            }
            Ok(counts)
        })
        .await?;
    Ok((
        StatusCode::OK,
        Json(json!({ "applied": true, "counts": counts, "registry": imported, "rows": rows })),
    )
        .into_response())
}
