//! REST routes for sources, the registry, metrics and system tracks.
//!
//! Authentication arrives later; until then the acting operator is taken from
//! the `X-OpenTrack-Actor` header (default `op:api`) and recorded with every
//! decision.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use ot_source::frame::Frame;
use ot_source::pipeline::Pipeline;
use ot_source::source::SourceSpec;
use ot_store::{RegistryEntity, SourceRow, SourceWrite};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::control::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/sources", get(list_sources).post(create_source))
        .route("/sources/validate", post(validate_source))
        .route(
            "/sources/{id}",
            get(get_source).put(put_source).delete(delete_source),
        )
        .route("/sources/{id}/enable", post(enable_source))
        .route("/sources/{id}/disable", post(disable_source))
        .route(
            "/sources/{id}/raw-output",
            put(set_raw_output).delete(clear_raw_output),
        )
        .route("/sources/{id}/revisions", get(source_revisions))
        .route("/sources/{id}/metrics", get(source_metrics))
        .route("/registry", get(registry_resolve))
        .route("/registry/stats", get(registry_stats))
        .route("/registry/import", post(registry_import))
        .route("/registry/entities/{id}", get(registry_entity))
        .route("/tracks", get(list_tracks))
}

fn actor(headers: &HeaderMap) -> String {
    headers
        .get("x-opentrack-actor")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map_or_else(|| "op:api".to_owned(), str::to_owned)
}

fn parse_spec(body: Value) -> Result<(SourceSpec, Value), ApiError> {
    let spec: SourceSpec = serde_json::from_value(body)
        .map_err(|e| ApiError::unprocessable(format!("invalid source spec: {e}")))?;
    spec.validate()
        .map_err(|e| ApiError::unprocessable(e.to_string()))?;
    // Store the normalised form (defaults filled in).
    let normalised = serde_json::to_value(&spec).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok((spec, normalised))
}

async fn with_status(s: &AppState, row: SourceRow) -> Value {
    let status = s
        .redis
        .get_source_status(&row.id)
        .await
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str::<Value>(&j).ok());
    let mut v = serde_json::to_value(&row).unwrap_or_default();
    v["status"] = status.unwrap_or(Value::Null);
    v
}

async fn list_sources(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows = s.with_db(|db| db.list_sources()).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(with_status(&s, r).await);
    }
    Ok(Json(json!({ "sources": out })))
}

async fn get_source(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = s
        .with_db(move |db| db.get_source(&id))
        .await?
        .ok_or_else(|| ApiError::not_found("source"))?;
    Ok(Json(with_status(&s, row).await))
}

async fn save(
    s: &AppState,
    spec: SourceSpec,
    normalised: Value,
    actor: String,
) -> Result<SourceRow, ApiError> {
    s.with_db(move |db| {
        db.put_source(
            &SourceWrite {
                id: &spec.id,
                name: &spec.name,
                transport: spec.transport.kind(),
                codec: spec.pipeline.codec.name(),
                priority: spec.priority,
                spec: &normalised,
            },
            &actor,
        )
    })
    .await
}

async fn create_source(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (spec, normalised) = parse_spec(body)?;
    let id = spec.id.clone();
    if s.with_db(move |db| db.get_source(&id)).await?.is_some() {
        return Err(ApiError::conflict(format!(
            "source {} already exists; use PUT to update",
            spec.id
        )));
    }
    let row = save(&s, spec, normalised, actor(&headers)).await?;
    Ok((StatusCode::CREATED, Json(with_status(&s, row).await)))
}

async fn put_source(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let (spec, normalised) = parse_spec(body)?;
    if spec.id != id {
        return Err(ApiError::unprocessable(format!(
            "body id {:?} does not match path id {id:?} (source ids never change)",
            spec.id
        )));
    }
    let row = save(&s, spec, normalised, actor(&headers)).await?;
    Ok(Json(with_status(&s, row).await))
}

async fn delete_source(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let actor = actor(&headers);
    s.with_db(move |db| db.delete_source(&id, &actor)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn toggle(
    s: AppState,
    id: String,
    enabled: bool,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let actor = actor(&headers);
    let row = s
        .with_db(move |db| db.set_source_enabled(&id, enabled, &actor))
        .await?;
    Ok(Json(with_status(&s, row).await))
}

async fn enable_source(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    toggle(s, id, true, headers).await
}

async fn disable_source(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    toggle(s, id, false, headers).await
}

#[derive(Deserialize)]
struct RawOutput {
    collection: String,
    /// Must be true: the admin confirms this source's raw data goes to the mesh.
    consent: bool,
}

async fn set_raw_output(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RawOutput>,
) -> Result<Json<Value>, ApiError> {
    if !body.consent {
        return Err(ApiError::unprocessable(
            "raw output sends this source's tracks to the mesh; set consent to true to confirm",
        ));
    }
    let c = body.collection.trim().to_owned();
    if c.is_empty() || c == s.common.tracks_collection {
        return Err(ApiError::unprocessable(
            "raw output needs its own collection, not the authoritative tracks collection",
        ));
    }
    let actor = actor(&headers);
    let row = s
        .with_db(move |db| db.set_raw_output(&id, Some(&c), &actor))
        .await?;
    Ok(Json(with_status(&s, row).await))
}

async fn clear_raw_output(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let actor = actor(&headers);
    let row = s
        .with_db(move |db| db.set_raw_output(&id, None, &actor))
        .await?;
    Ok(Json(with_status(&s, row).await))
}

async fn source_revisions(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let revs = s.with_db(move |db| db.source_revisions(&id)).await?;
    Ok(Json(json!({ "revisions": revs })))
}

#[derive(Deserialize)]
struct MetricsQuery {
    minutes: Option<i64>,
}

async fn source_metrics(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<MetricsQuery>,
) -> Result<Json<Value>, ApiError> {
    let minutes = q.minutes.unwrap_or(60).clamp(1, 7 * 24 * 60);
    let series = s.redis.metrics_range(&id, minutes).await?;
    let mut totals: BTreeMap<String, u64> = BTreeMap::new();
    let points: Vec<Value> = series
        .into_iter()
        .map(|(minute, counts)| {
            for (k, v) in &counts {
                *totals.entry(k.clone()).or_default() += v;
            }
            json!({ "minute": minute, "counts": counts })
        })
        .collect();
    Ok(Json(
        json!({ "source": id, "minutes": minutes, "totals": totals, "series": points }),
    ))
}

#[derive(Deserialize)]
struct ValidateBody {
    spec: Value,
    /// Optional sample frames (strings) to run through the pipeline.
    #[serde(default)]
    samples: Vec<String>,
}

/// Validate a source spec and optionally dry-run sample frames through its
/// pipeline, using the live registry. Nothing is saved or published.
async fn validate_source(
    State(s): State<AppState>,
    Json(body): Json<ValidateBody>,
) -> Result<Json<Value>, ApiError> {
    let (spec, normalised) = parse_spec(body.spec)?;
    if body.samples.is_empty() {
        return Ok(Json(json!({ "valid": true, "spec": normalised })));
    }
    let rows = s.with_db(|db| db.registry_rows()).await?;
    let registry: BTreeMap<(String, String), ot_source::registry::RegistryEntry> = rows
        .into_iter()
        .map(|r| {
            (
                (r.scheme, r.value),
                ot_source::registry::RegistryEntry {
                    entity_id: r.entity_id,
                    name: r.name,
                    expected_name: r.expected_name,
                    fields: r.fields,
                },
            )
        })
        .collect();
    let mut pipeline = Pipeline::new(spec.id.clone(), spec.pipeline.clone())
        .map_err(|e| ApiError::unprocessable(e.to_string()))?;
    let mut observations = Vec::new();
    let mut errors = Vec::new();
    for sample in body.samples.iter().take(1000) {
        let out = pipeline.process(&Frame::new(sample.clone().into_bytes()), &registry);
        observations.extend(out.observations);
        if let Some(e) = out.last_error {
            errors.push(e);
        }
    }
    let counts: BTreeMap<String, u64> = pipeline.take_counts().pairs().into_iter().collect();
    Ok(Json(json!({
        "valid": true,
        "counts": counts,
        "errors": errors,
        "observations": observations,
    })))
}

#[derive(Deserialize)]
struct ResolveQuery {
    scheme: String,
    value: String,
}

async fn registry_resolve(
    State(s): State<AppState>,
    Query(q): Query<ResolveQuery>,
) -> Result<Json<Value>, ApiError> {
    let e = s
        .with_db(move |db| db.registry_resolve(&q.scheme, &q.value))
        .await?
        .ok_or_else(|| ApiError::not_found("identifier"))?;
    Ok(Json(serde_json::to_value(e).unwrap_or_default()))
}

async fn registry_entity(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let e = s
        .with_db(move |db| db.registry_entity(&id))
        .await?
        .ok_or_else(|| ApiError::not_found("entity"))?;
    Ok(Json(serde_json::to_value(e).unwrap_or_default()))
}

async fn registry_stats(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.with_db(|db| db.registry_counts()).await?))
}

#[derive(Deserialize)]
struct ImportBody {
    label: String,
    entities: Vec<RegistryEntity>,
}

async fn registry_import(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ImportBody>,
) -> Result<Json<Value>, ApiError> {
    let actor = actor(&headers);
    let counts = s
        .with_db(move |db| db.registry_import(&body.entities, &actor, &body.label))
        .await?;
    Ok(Json(serde_json::to_value(counts).unwrap_or_default()))
}

#[derive(Deserialize)]
struct TracksQuery {
    source: Option<String>,
    limit: Option<usize>,
}

/// Live system tracks, newest report first.
async fn list_tracks(
    State(s): State<AppState>,
    Query(q): Query<TracksQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut tracks = s.redis.list_system_tracks().await?;
    if let Some(src) = &q.source {
        tracks.retain(|t| t.contributors.iter().any(|c| &c.source_id == src));
    }
    tracks.sort_by_key(|t| std::cmp::Reverse(t.last_seen));
    let total = tracks.len();
    let items: Vec<Value> = tracks
        .into_iter()
        .take(q.limit.unwrap_or(500).min(10_000))
        .map(|t| {
            json!({
                "uid": t.uid,
                "doc_id": t.uid.doc_id(),
                "state": t.state,
                "name": t.view.name,
                "callsign": t.view.callsign,
                "classification": t.view.classification.cot_type_or_derived(),
                "identifiers": t.view.identifiers,
                "latitude": t.view.position.latitude,
                "longitude": t.view.position.longitude,
                "last_seen": t.last_seen,
                "observation_count": t.observation_count,
                "sources": t.contributors.iter().map(|c| format!("{}/{}", c.source_id, c.source_track_key)).collect::<Vec<_>>(),
                "registry": t.view.ext.get("registry"),
            })
        })
        .collect();
    Ok(Json(json!({ "total": total, "tracks": items })))
}
