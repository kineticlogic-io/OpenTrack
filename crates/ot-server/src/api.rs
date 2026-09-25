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

    #[test]
    fn worked_examples_stay_valid() {
        for (name, text) in [("aisstream", AIS), ("adsb-lol", ADSB)] {
            let spec: ot_source::source::SourceSpec =
                serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            spec.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
        }
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
            peat: "http://127.0.0.1:9".into(),
            site: ot_core::SiteCode::new("TST").unwrap(),
            tracks_collection: "tracks".into(),
        };
        let redis = ot_store::RedisStore::connect(&url, ot_store::Keys::new(ns))
            .await
            .unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(ot_store::Db::open_in_memory().unwrap())),
            redis: redis.clone(),
            peat: ot_peat::PeatClient::connect_lazy(&common.peat).unwrap(),
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

        // Raw output needs explicit consent and its own collection.
        let (st, _) = call(
            &app,
            "PUT",
            "/api/v1/sources/adsb-lol/raw-output",
            Some(json!({"collection": "tracks_raw_adsb", "consent": false})),
        )
        .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        let (st, _) = call(
            &app,
            "PUT",
            "/api/v1/sources/adsb-lol/raw-output",
            Some(json!({"collection": "tracks", "consent": true})),
        )
        .await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
        let (st, body) = call(
            &app,
            "PUT",
            "/api/v1/sources/adsb-lol/raw-output",
            Some(json!({"collection": "tracks_raw_adsb", "consent": true})),
        )
        .await;
        assert_eq!(
            (st, body["raw_collection"].as_str()),
            (StatusCode::OK, Some("tracks_raw_adsb"))
        );

        let (st, _) = call(&app, "DELETE", "/api/v1/sources/adsb-lol", None).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        let (st, _) = call(&app, "GET", "/api/v1/sources/adsb-lol", None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
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
}
