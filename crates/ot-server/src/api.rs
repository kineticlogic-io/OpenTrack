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
use ot_source::schema::{ExtensionField, ExtensionSchema};
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
        .route("/probe", post(crate::probe::probe))
        .route("/sources/{id}/samples", get(crate::probe::samples))
        .route("/schema", get(schema_overview))
        .route(
            "/schema/draft",
            put(put_schema_draft).delete(discard_schema_draft),
        )
        .route("/schema/draft/publish", post(publish_schema_draft))
        .merge(crate::cards::routes())
}

pub(crate) fn actor(headers: &HeaderMap) -> String {
    headers
        .get("x-opentrack-actor")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map_or_else(|| "op:api".to_owned(), str::to_owned)
}

/// Parse and validate a source spec, including against the published
/// extension schema version its mapping targets. Returns the spec, its
/// normalised JSON (defaults filled in) and that schema.
async fn parse_spec(
    s: &AppState,
    body: Value,
) -> Result<(SourceSpec, Value, ExtensionSchema), ApiError> {
    let spec: SourceSpec = serde_json::from_value(body)
        .map_err(|e| ApiError::unprocessable(format!("invalid source spec: {e}")))?;
    let schema = published_schema(s, spec.pipeline.mapping.schema_version).await?;
    spec.validate_against(schema.as_ref())
        .map_err(|e| ApiError::unprocessable(e.to_string()))?;
    let normalised = serde_json::to_value(&spec).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok((
        spec,
        normalised,
        schema.expect("validated against a published schema"),
    ))
}

/// A published extension schema version, typed.
pub(crate) async fn published_schema(
    s: &AppState,
    version: u32,
) -> Result<Option<ExtensionSchema>, ApiError> {
    let v = s.with_db(move |db| db.schema_version_get(version)).await?;
    v.filter(|v| v.status == "published")
        .map(to_extension_schema)
        .transpose()
}

pub(crate) fn to_extension_schema(v: ot_store::SchemaVersion) -> Result<ExtensionSchema, ApiError> {
    let fields = v
        .fields
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<Vec<ExtensionField>, _>>()
        .map_err(|e| {
            ApiError::internal(format!("stored schema {} is unreadable: {e}", v.version))
        })?;
    Ok(ExtensionSchema {
        version: v.version,
        fields,
    })
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
    let (spec, normalised, _) = parse_spec(&s, body).await?;
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
    let (spec, normalised, _) = parse_spec(&s, body).await?;
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
    /// NATS subject for this source's raw tracks; `opentrack.raw.<source>`
    /// when omitted.
    #[serde(default)]
    subject: Option<String>,
    /// Must be true: the admin confirms this source's raw data is published.
    consent: bool,
}

/// A concrete (wildcard-free) NATS subject outside the system tracks prefix.
fn check_raw_subject(subject: &str, tracks_prefix: &str) -> Result<(), String> {
    let valid = !subject.is_empty()
        && subject.split('.').all(|t| {
            !t.is_empty() && t != "*" && t != ">" && !t.chars().any(|c| c.is_whitespace())
        });
    if !valid {
        return Err(format!(
            "{subject:?} is not a valid NATS subject (dot-separated tokens, no wildcards or spaces)"
        ));
    }
    if subject == tracks_prefix || subject.starts_with(&format!("{tracks_prefix}.")) {
        return Err(format!(
            "raw output needs its own subject, outside the system tracks subjects ({tracks_prefix}.>)"
        ));
    }
    Ok(())
}

async fn set_raw_output(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RawOutput>,
) -> Result<Json<Value>, ApiError> {
    if !body.consent {
        return Err(ApiError::unprocessable(
            "raw output publishes this source's tracks as received; set consent to true to confirm",
        ));
    }
    let c = match body.subject.as_deref().map(str::trim) {
        Some(sub) if !sub.is_empty() => sub.to_owned(),
        _ => format!("opentrack.raw.{id}"),
    };
    check_raw_subject(&c, &s.common.nats.tracks_subject).map_err(ApiError::unprocessable)?;
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
    /// Also run the stored probe samples of this source id.
    #[serde(default)]
    stored_samples_of: Option<String>,
}

/// Validate a source spec and optionally dry-run sample frames through its
/// pipeline, using the live registry. Nothing is saved or published.
async fn validate_source(
    State(s): State<AppState>,
    Json(body): Json<ValidateBody>,
) -> Result<Json<Value>, ApiError> {
    let (spec, normalised, schema) = parse_spec(&s, body.spec).await?;
    let mut frames: Vec<Frame> = body
        .samples
        .iter()
        .map(|s| Frame::new(s.clone().into_bytes()))
        .collect();
    if let Some(id) = body.stored_samples_of.clone() {
        let stored = s
            .with_db(move |db| db.probe_samples(&id, ot_store::probe::DEFAULT_SAMPLE_CAP))
            .await?;
        frames.extend(stored.into_iter().map(|s| {
            let meta = match s.meta {
                Some(Value::Object(m)) => m,
                _ => Default::default(),
            };
            Frame::new(s.bytes).with_meta(meta)
        }));
    }
    if frames.is_empty() {
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
        .map_err(|e| ApiError::unprocessable(e.to_string()))?
        .with_schema(schema);
    let mut observations = Vec::new();
    let mut errors = Vec::new();
    for frame in frames.iter().take(1000) {
        let out = pipeline.process(frame, &registry);
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
    let ctx = s.common.publish_context();
    let now = chrono::Utc::now();
    let items: Vec<Value> = tracks
        .into_iter()
        .take(q.limit.unwrap_or(500).min(10_000))
        .map(|t| {
            // The GOLD fields exactly as published.
            let m = ot_core::wire::to_message(&t, &ctx, now);
            json!({
                "uid": t.uid,
                "track_id": m.track_id,
                "entity_id": t.entity_id,
                "notices": t.notices.len(),
                "state": t.state,
                "class": m.class,
                "gold_name": m.name,
                "domain": m.domain,
                "affiliation": m.affiliation,
                "force_code": m.force_code,
                "track_type": m.track_type,
                "sidc": m.sidc,
                "name": t.view.name,
                "callsign": t.view.callsign,
                "classification": t.view.classification.cot_type_or_derived(),
                "identifiers": t.view.identifiers,
                "latitude": t.view.position.latitude,
                "longitude": t.view.position.longitude,
                "course_deg": t.view.kinematics.course_deg,
                "speed_mps": t.view.kinematics.speed_mps,
                "last_seen": t.last_seen,
                "observation_count": t.observation_count,
                "sources": t.contributors.iter().map(|c| format!("{}/{}", c.source_id, c.source_track_key)).collect::<Vec<_>>(),
                "registry": t.view.ext.get("registry"),
            })
        })
        .collect();
    Ok(Json(json!({ "total": total, "tracks": items })))
}

/// The whole schema: fixed core fields, every extension version, and which
/// sources target which version.
async fn schema_overview(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let versions = s.with_db(|db| db.schema_versions()).await?;
    let sources = s.with_db(|db| db.list_sources()).await?;
    let usage: Vec<Value> = sources
        .iter()
        .map(|r| {
            json!({
                "source": r.id,
                "enabled": r.enabled,
                "schema_version": r.spec.pointer("/pipeline/mapping/schema_version").cloned().unwrap_or(json!(1)),
            })
        })
        .collect();
    let latest = versions
        .iter()
        .filter(|v| v.status == "published")
        .map(|v| v.version)
        .max();
    let builtins: Vec<Value> = ot_source::schema::Builtin::ALL
        .iter()
        .map(|b| json!({ "name": b, "type": b.kind() }))
        .collect();
    Ok(Json(json!({
        // Always published (the OTH-GOLD minimum); the schema adds `attributes`.
        "published_core": ["track_id", "class", "name", "domain", "affiliation", "force_code",
                           "track_type", "sidc", "time", "lat", "lon"],
        "builtins": builtins,
        "core": ot_source::mapping::target_fields().collect::<Vec<_>>(),
        "reserved_extension_keys": ot_source::schema::RESERVED_KEYS,
        "latest_published": latest,
        "versions": versions,
        "sources": usage,
    })))
}

#[derive(Deserialize)]
struct DraftBody {
    fields: Vec<Value>,
    #[serde(default)]
    notes: Option<String>,
}

async fn put_schema_draft(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DraftBody>,
) -> Result<Json<Value>, ApiError> {
    let fields: Vec<ExtensionField> = body
        .fields
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .map_err(|e| ApiError::unprocessable(format!("invalid field definition: {e}")))?;
    ExtensionSchema {
        version: 0,
        fields: fields.clone(),
    }
    .validate()
    .map_err(|e| ApiError::unprocessable(e.to_string()))?;
    let normalised: Vec<Value> = fields
        .iter()
        .map(|f| serde_json::to_value(f).expect("field serialises"))
        .collect();
    let actor = actor(&headers);
    let draft = s
        .with_db(move |db| db.put_schema_draft(&normalised, body.notes.as_deref(), &actor))
        .await?;
    Ok(Json(serde_json::to_value(draft).unwrap_or_default()))
}

async fn publish_schema_draft(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let actor = actor(&headers);
    let v = s.with_db(move |db| db.publish_schema_draft(&actor)).await?;
    Ok(Json(serde_json::to_value(v).unwrap_or_default()))
}

async fn discard_schema_draft(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let actor = actor(&headers);
    s.with_db(move |db| db.discard_schema_draft(&actor)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::config::Common;
    use crate::control::{AppState, router};

    const AIS: &str = include_str!("../../../docs/examples/aisstream.json");
    const ADSB: &str = include_str!("../../../docs/examples/adsb-lol.json");
    const SCHEMA: &str = include_str!("../../../docs/examples/schema.json");

    #[test]
    fn worked_examples_stay_valid() {
        let schema: Value = serde_json::from_str(SCHEMA).unwrap();
        let schema = ot_source::schema::ExtensionSchema {
            version: 2,
            fields: serde_json::from_value(schema["fields"].clone()).unwrap(),
        };
        schema.validate().unwrap();
        for (name, text) in [("aisstream", AIS), ("adsb-lol", ADSB)] {
            let spec: ot_source::source::SourceSpec =
                serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            spec.validate_against(Some(&schema))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    async fn publish_example_schema(app: &axum::Router) {
        let (st, body) = call(
            app,
            "PUT",
            "/api/v1/schema/draft",
            Some(serde_json::from_str(SCHEMA).unwrap()),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let (st, body) = call(app, "POST", "/api/v1/schema/draft/publish", None).await;
        assert_eq!(
            (st, body["version"].as_u64()),
            (StatusCode::OK, Some(2)),
            "{body}"
        );
    }

    /// The router over an in-memory database and an isolated Redis
    /// namespace; `None` (test skipped) without `OT_TEST_REDIS_URL`.
    async fn app() -> Option<(axum::Router, ot_store::RedisStore)> {
        let url = std::env::var("OT_TEST_REDIS_URL").ok()?;
        let ns = format!(
            "ot-api-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_micros()
        );
        let common = Common {
            sqlite: ":memory:".into(),
            redis: url.clone(),
            redis_namespace: ns.clone(),
            site: ot_core::SiteCode::new("TST").unwrap(),
            nats: crate::config::NatsArgs {
                // Nothing listens here: status reports it, nothing blocks.
                url: "nats://127.0.0.1:9".into(),
                creds: None,
                token: None,
                user: None,
                password: None,
                stream: "TRACKS".into(),
                tracks_subject: "tracks".into(),
                max_age_hours: 24.0,
            },
        };
        let redis = ot_store::RedisStore::connect(&url, ot_store::Keys::new(ns))
            .await
            .unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(ot_store::Db::open_in_memory().unwrap())),
            redis: redis.clone(),
            nats: common.connect_nats().await.unwrap(),
            common,
        };
        Some((router(state, None), redis))
    }

    async fn call(
        app: &axum::Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-opentrack-actor", "op:test");
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn source_lifecycle_through_the_api() {
        let Some((app, redis)) = app().await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        let spec: Value = serde_json::from_str(ADSB).unwrap();

        // Its extension schema is not published yet.
        let (st, body) = call(&app, "POST", "/api/v1/sources", Some(spec.clone())).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            body["error"].as_str().unwrap().contains("schema version 2"),
            "{body}"
        );
        publish_example_schema(&app).await;

        let (st, body) = call(&app, "POST", "/api/v1/sources", Some(spec.clone())).await;
        assert_eq!(st, StatusCode::CREATED, "{body}");
        assert_eq!(
            (body["revision"].as_i64(), body["enabled"].as_bool()),
            (Some(1), Some(false))
        );
        assert_eq!(body["transport"], "http_poll");

        let (st, _) = call(&app, "POST", "/api/v1/sources", Some(spec.clone())).await;
        assert_eq!(st, StatusCode::CONFLICT);

        let (st, body) = call(&app, "PUT", "/api/v1/sources/other-id", Some(spec.clone())).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

        let mut typo = spec.clone();
        typo["pipeline"]["mapping"]["rules"][1]["fields"]["kinematic.speed_mps"] = json!("gs");
        let (st, body) = call(&app, "PUT", "/api/v1/sources/adsb-lol", Some(typo)).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("kinematic.speed_mps"),
            "{body}"
        );

        let mut undeclared = spec.clone();
        undeclared["pipeline"]["mapping"]["rules"][1]["fields"]["ext.tail_art"] = json!("r");
        let (st, body) = call(&app, "PUT", "/api/v1/sources/adsb-lol", Some(undeclared)).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            body["error"].as_str().unwrap().contains("ext.tail_art"),
            "{body}"
        );

        let mut edited = spec.clone();
        edited["name"] = json!("adsb.lol (edited)");
        let (st, body) = call(&app, "PUT", "/api/v1/sources/adsb-lol", Some(edited)).await;
        assert_eq!((st, body["revision"].as_i64()), (StatusCode::OK, Some(2)));

        let (_, body) = call(&app, "POST", "/api/v1/sources/adsb-lol/enable", None).await;
        assert_eq!(body["enabled"], true);
        let (_, body) = call(&app, "GET", "/api/v1/sources/adsb-lol/revisions", None).await;
        assert_eq!(body["revisions"].as_array().unwrap().len(), 2);
        let (_, body) = call(&app, "GET", "/api/v1/sources", None).await;
        assert_eq!(body["sources"][0]["name"], "adsb.lol (edited)");

        // Raw output needs explicit consent and a subject outside `tracks.>`.
        for bad in [
            json!({"consent": false}),
            json!({"subject": "tracks.raw", "consent": true}),
            json!({"subject": "tracks", "consent": true}),
            json!({"subject": "raw.*", "consent": true}),
            json!({"subject": "raw..adsb", "consent": true}),
        ] {
            let (st, _) = call(
                &app,
                "PUT",
                "/api/v1/sources/adsb-lol/raw-output",
                Some(bad.clone()),
            )
            .await;
            assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        }
        let (st, body) = call(
            &app,
            "PUT",
            "/api/v1/sources/adsb-lol/raw-output",
            Some(json!({"consent": true})),
        )
        .await;
        assert_eq!(
            (st, body["raw_subject"].as_str()),
            (StatusCode::OK, Some("opentrack.raw.adsb-lol"))
        );
        let (st, body) = call(
            &app,
            "PUT",
            "/api/v1/sources/adsb-lol/raw-output",
            Some(json!({"subject": "raw.adsb", "consent": true})),
        )
        .await;
        assert_eq!(
            (st, body["raw_subject"].as_str()),
            (StatusCode::OK, Some("raw.adsb"))
        );

        // Status reports NATS as down without blocking the API.
        let (st, body) = call(&app, "GET", "/api/v1/status", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body["nats"]["ok"], false);
        assert_eq!(body["nats"]["stream"], "TRACKS");

        let (st, _) = call(&app, "DELETE", "/api/v1/sources/adsb-lol", None).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        let (st, _) = call(&app, "GET", "/api/v1/sources/adsb-lol", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn schema_workspace() {
        let Some((app, redis)) = app().await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        let (_, body) = call(&app, "GET", "/api/v1/schema", None).await;
        assert_eq!(body["latest_published"], 1);
        assert!(
            body["core"]
                .as_array()
                .unwrap()
                .contains(&json!("position.latitude"))
        );

        let (st, _) = call(
            &app,
            "PUT",
            "/api/v1/schema/draft",
            Some(json!({"fields": [{"key": "registry", "type": "json"}]})),
        )
        .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "reserved key");
        let (st, _) = call(
            &app,
            "PUT",
            "/api/v1/schema/draft",
            Some(json!({"fields": [{"key": "mode", "type": "enum"}]})),
        )
        .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "enum without values");

        publish_example_schema(&app).await;
        let (_, body) = call(&app, "GET", "/api/v1/schema", None).await;
        assert_eq!(body["latest_published"], 2);
        assert_eq!(body["versions"][1]["fields"].as_array().unwrap().len(), 8);
        let (st, _) = call(&app, "POST", "/api/v1/schema/draft/publish", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "no draft left to publish");
        redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn dry_run_and_registry_with_any_identifier_scheme() {
        let Some((app, redis)) = app().await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        let (st, body) = call(&app, "POST", "/api/v1/registry/import", Some(json!({
            "label": "test",
            "entities": [{"id": "cvn75", "name": "USS HARRY S TRUMAN", "fields": {"cot": "a-f-S-C-A"},
                "identifiers": [{"scheme": "mmsi", "value": "338000001"}, {"scheme": "imo", "value": "9876543"},
                                {"scheme": "elnot", "value": "NL504"}]}]
        }))).await;
        assert_eq!(
            (st, body["identifiers"].as_u64()),
            (StatusCode::OK, Some(3)),
            "{body}"
        );
        let (st, body) = call(
            &app,
            "GET",
            "/api/v1/registry?scheme=elnot&value=NL504",
            None,
        )
        .await;
        assert_eq!((st, body["id"].as_str()), (StatusCode::OK, Some("cvn75")));

        // Dry run: a CoT event carrying an ELNOT resolves through the registry.
        let spec = json!({
            "id": "cot-test", "name": "CoT test",
            "transport": {"type": "udp", "bind": "127.0.0.1:0"},
            "pipeline": {
                "codec": {"type": "cot_xml"},
                "mapping": {"rules": [{"name": "event", "key": "event.@uid",
                    "identifiers": [{"scheme": "elnot", "value": "event.detail.elnot.@code"}],
                    "fields": {"position.latitude": "event.point.@lat", "position.longitude": "event.point.@lon",
                               "classification.cot_type": "event.@type", "name": "event.detail.contact.@callsign"}}]},
                "registry": {"apply": {"classification.cot_type": "cot", "platform.name": "name"}}
            }
        });
        let frame = r#"<event uid="E1" type="a-u-S" time="2026-09-25T00:00:00Z"><point lat="10" lon="20"/>
            <detail><contact callsign="HARRY S TRUMAN"/><elnot code="NL504"/></detail></event>"#;
        let (st, body) = call(
            &app,
            "POST",
            "/api/v1/sources/validate",
            Some(json!({"spec": spec, "samples": [frame]})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let o = &body["observations"][0];
        assert_eq!(
            o["identifiers"][0],
            json!({"scheme": "elnot", "value": "NL504"})
        );
        assert_eq!(o["ext"]["registry"]["scheme"], "elnot");
        assert_eq!(o["ext"]["registry"]["grade"], "name");
        assert_eq!(o["classification"]["cot_type"], "a-f-S-C-A");
        redis.purge_namespace().await.unwrap();
    }

    /// Probe a live MQTT topic, keep the samples, and preview a mapping that
    /// takes the track key from the topic. Needs `OT_TEST_MQTT_URL` too.
    #[tokio::test]
    async fn mqtt_probe_keeps_topics_for_the_preview() {
        let Ok(mqtt) = std::env::var("OT_TEST_MQTT_URL") else {
            eprintln!("skipped: OT_TEST_MQTT_URL not set");
            return;
        };
        let Some((app, redis)) = app().await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        let run_id = format!("otapi{}", chrono::Utc::now().timestamp_micros());
        let (host, port) = mqtt
            .trim_start_matches("mqtt://")
            .rsplit_once(':')
            .map(|(h, p)| (h.to_owned(), p.parse::<u16>().unwrap()))
            .unwrap();
        let (client, mut events) = rumqttc::AsyncClient::new(
            rumqttc::MqttOptions::new(format!("{run_id}-pub"), host, port),
            16,
        );
        let pump = tokio::spawn(async move { while events.poll().await.is_ok() {} });
        let topic = format!("{run_id}/366123456/pos");
        let publisher = tokio::spawn(async move {
            loop {
                let _ = client
                    .publish(
                        topic.clone(),
                        rumqttc::QoS::AtMostOnce,
                        false,
                        r#"{"lat":32.7,"lon":-117.2}"#,
                    )
                    .await;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        });

        let transport = json!({"type": "mqtt", "url": mqtt, "topics": [format!("{run_id}/+/pos")]});
        let (st, body) = call(
            &app,
            "POST",
            "/api/v1/probe",
            Some(json!({
                "transport": transport, "codec": {"type": "json"},
                "max_frames": 3, "max_secs": 10, "save_as": "mqtt-test"
            })),
        )
        .await;
        publisher.abort();
        pump.abort();
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["frames"], 3, "{body}");
        assert_eq!(body["samples_saved"], 3);
        assert_eq!(
            body["sample_frames"][0]["meta"]["topic_levels"][1],
            "366123456"
        );
        // Inference sees the topic like any other field.
        assert!(
            body["fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["path"] == "_frame.topic"),
            "{body}"
        );

        let spec = json!({
            "id": "mqtt-test", "name": "MQTT test", "transport": transport,
            "pipeline": {
                "codec": {"type": "json"},
                "mapping": {"rules": [{"name": "pos", "key": "_frame.topic_levels[1]",
                    "identifiers": [{"scheme": "mmsi", "value": "_frame.topic_levels[1]"}],
                    "fields": {"position.latitude": "lat", "position.longitude": "lon"}}]}
            }
        });
        let (st, body) = call(
            &app,
            "POST",
            "/api/v1/sources/validate",
            Some(json!({"spec": spec, "stored_samples_of": "mqtt-test"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let o = &body["observations"][0];
        assert_eq!(o["source_track_key"], "366123456", "{body}");
        assert_eq!(
            o["identifiers"][0],
            json!({"scheme": "mmsi", "value": "366123456"})
        );
        redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn cards_through_the_api() {
        let Some((app, redis)) = app().await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        let (st, body) = call(
            &app,
            "PUT",
            "/api/v1/schema/draft",
            Some(json!({"fields": [
                {"key": "contact_phone", "type": "string", "description": "Ship's contact number"},
                {"key": "length_m", "type": "number", "unit": "m"},
                {"key": "speed_mps", "type": "number", "builtin": "speed_mps"}
            ]})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["fields"][2]["builtin"], "speed_mps");
        let (st, _) = call(&app, "POST", "/api/v1/schema/draft/publish", None).await;
        assert_eq!(st, StatusCode::OK);
        // A linked field must declare its built-in's type.
        let (st, _) = call(
            &app,
            "PUT",
            "/api/v1/schema/draft",
            Some(json!({"fields": [
                {"key": "speed", "type": "string", "builtin": "speed_mps"}
            ]})),
        )
        .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        call(&app, "DELETE", "/api/v1/schema/draft", None).await;

        let (st, body) = call(
            &app,
            "POST",
            "/api/v1/cards",
            Some(json!({
                "name": "TED STEVENS",
                "identifiers": [{"scheme": "mmsi", "value": "338924210"}],
                "values": {"contact_phone": "+1 555 0100", "length_m": "210"}
            })),
        )
        .await;
        assert_eq!(st, StatusCode::CREATED, "{body}");
        let id = body["entity"]["id"].as_str().unwrap().to_owned();
        assert_eq!(
            body["card"]["values"],
            json!({"contact_phone": "+1 555 0100", "length_m": 210.0})
        );
        assert_eq!(body["schema"]["version"], 2);

        // The same identifier cannot start a second card.
        let (st, _) = call(
            &app,
            "POST",
            "/api/v1/cards",
            Some(json!({
                "identifiers": [{"scheme": "mmsi", "value": "338924210"}]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT);
        let (st, _) = call(&app, "POST", "/api/v1/cards", Some(json!({"name": "x"}))).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

        // Built-in fields and unknown keys cannot be set on a card.
        for bad in [
            json!({"speed_mps": 3}),
            json!({"nope": 1}),
            json!({"length_m": "long"}),
        ] {
            let (st, _) = call(
                &app,
                "PUT",
                &format!("/api/v1/cards/{id}"),
                Some(json!({"values": bad.clone()})),
            )
            .await;
            assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        }
        let (st, body) = call(
            &app,
            "PUT",
            &format!("/api/v1/cards/{id}"),
            Some(json!({"values": {"contact_phone": "+1 555 0199"}})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(
            body["card"]["values"],
            json!({"contact_phone": "+1 555 0199"})
        );
        assert_eq!(body["revisions"].as_array().unwrap().len(), 2);

        let (_, body) = call(&app, "GET", "/api/v1/cards?q=stevens", None).await;
        assert_eq!(body["entities"][0]["id"], id);
        assert_eq!(body["entities"][0]["has_card"], true);
        let (st, _) = call(&app, "GET", "/api/v1/cards/ent-nope", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (_, body) = call(&app, "GET", "/api/v1/schema", None).await;
        assert!(
            body["builtins"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["name"] == "state")
        );
        redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn system_metrics() {
        let Some((app, redis)) = app().await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        // One delete waiting for the writer, and some counted pipeline work.
        redis
            .ensure_outbox_group(crate::writer::GROUP)
            .await
            .unwrap();
        let uid = ot_core::Uid::new(ot_core::SiteCode::new("TST").unwrap(), 7).unwrap();
        redis.retire_system_track(uid, "test").await.unwrap();
        redis
            .incr_metrics(crate::metrics::ENGINE, &[("observations".into(), 5)])
            .await
            .unwrap();
        redis
            .set_gauges(crate::metrics::SYSTEM, &[("cpu_milli", 120)])
            .await
            .unwrap();

        let (st, body) = call(&app, "GET", "/api/v1/metrics?minutes=10", None).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let series = body["series"].as_array().unwrap();
        assert_eq!(series.len(), 10);
        assert_eq!(series[9]["engine"]["observations"], 5, "{body}");
        assert_eq!(body["recent"]["engine"]["observations"], 5);
        let live = &body["live"];
        assert_eq!(live["outbox"], json!({"pending": 0, "lag": 1}), "{body}");
        assert_eq!(live["tracks"], 0);
        assert_eq!(live["cpu_milli"], 120);
        assert!(live["redis_bytes"].as_u64().unwrap() > 0);
        redis.purge_namespace().await.unwrap();
    }
}
