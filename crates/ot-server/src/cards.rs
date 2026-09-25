//! Baseball cards over the API: find entities, create one (optionally from
//! a live track), and read or edit its card. A card holds values for the
//! output schema's fields that no feed provides; it is the authority for
//! them, and live tracks whose feed disagrees carry a notice.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use ot_core::Uid;
use ot_source::schema::{ExtensionSchema, check_card};
use ot_store::RegistryIdentifier;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::api::{actor, to_extension_schema};
use crate::control::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/cards", get(search).post(create))
        .route("/cards/{id}", get(card).put(save))
}

/// The newest published output schema (version 1 has no fields).
async fn latest_schema(s: &AppState) -> Result<ExtensionSchema, ApiError> {
    let versions = s.with_db(|db| db.schema_versions()).await?;
    versions
        .into_iter()
        .filter(|v| v.status == "published")
        .max_by_key(|v| v.version)
        .map(to_extension_schema)
        .transpose()?
        .ok_or_else(|| ApiError::internal("no published schema version"))
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
    limit: Option<usize>,
}

async fn search(
    State(s): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    if q.q.trim().len() < 2 {
        return Ok(Json(json!({ "entities": [] })));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let hits = s.with_db(move |db| db.search_entities(&q.q, limit)).await?;
    let entities: Vec<Value> = hits
        .into_iter()
        .map(|(e, has_card)| {
            json!({ "id": e.id, "name": e.name, "status": e.status,
                    "identifiers": e.identifiers, "has_card": has_card })
        })
        .collect();
    Ok(Json(json!({ "entities": entities })))
}

/// Everything the card view needs: the entity, the schema that shapes the
/// form, the card, its history, and the live tracks it speaks for.
async fn view(s: &AppState, id: String) -> Result<Value, ApiError> {
    let key = id.clone();
    let (entity, card, revisions) = s
        .with_db(move |db| {
            Ok((
                db.registry_entity(&key)?,
                db.card(&key)?,
                db.card_revisions(&key)?,
            ))
        })
        .await?;
    let entity = entity.ok_or_else(|| ApiError::not_found(format!("entity {id}")))?;
    let schema = latest_schema(s).await?;
    let tracks: Vec<Value> = s
        .redis
        .list_system_tracks()
        .await?
        .into_iter()
        .filter(|t| t.entity_id.as_deref() == Some(id.as_str()))
        .map(|t| {
            // What the feed reports for each schema field, card or not.
            let feed: Map<String, Value> = schema
                .fields
                .iter()
                .filter(|f| f.builtin.is_none())
                .filter_map(|f| t.view.ext.get(&f.key).map(|v| (f.key.clone(), v.clone())))
                .collect();
            json!({
                "uid": t.uid, "track_id": t.uid.doc_id(), "state": t.state,
                "source_id": t.view.source_id, "last_seen": t.last_seen,
                "feed": feed, "notices": t.notices,
            })
        })
        .collect();
    Ok(json!({
        "entity": entity,
        "schema": schema,
        "card": card,
        "revisions": revisions,
        "tracks": tracks,
    }))
}

async fn card(State(s): State<AppState>, Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    Ok(Json(view(&s, id).await?))
}

#[derive(Deserialize)]
struct SaveBody {
    values: Map<String, Value>,
}

async fn save(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SaveBody>,
) -> Result<Json<Value>, ApiError> {
    let schema = latest_schema(&s).await?;
    let values = check_card(&schema, &body.values).map_err(ApiError::unprocessable)?;
    let (actor, key, version) = (actor(&headers), id.clone(), schema.version);
    s.with_db(move |db| db.put_card(&key, &values, version, &actor))
        .await?;
    Ok(Json(view(&s, id).await?))
}

#[derive(Deserialize)]
struct CreateBody {
    /// Seed the entity's name and identifiers from this live track.
    #[serde(default)]
    from_track: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    identifiers: Vec<RegistryIdentifier>,
    #[serde(default)]
    values: Map<String, Value>,
}

async fn create(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(mut body): Json<CreateBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if let Some(raw) = &body.from_track {
        let uid: Uid = Uid::from_doc_id(raw)
            .or_else(|_| raw.parse())
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        let t = s
            .redis
            .get_system_track(uid)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("live system track {uid}")))?;
        if let Some(e) = &t.entity_id {
            return Err(ApiError::conflict(format!(
                "track {uid} already has a card: {e}"
            )));
        }
        body.name = body.name.or(t.view.name.clone());
        for i in &t.view.identifiers {
            body.identifiers.push(RegistryIdentifier {
                scheme: i.scheme.clone(),
                value: i.value.clone(),
                expected_name: t.view.name.clone(),
                source: None,
            });
        }
    }
    if body.identifiers.is_empty() {
        return Err(ApiError::unprocessable(
            "a card needs at least one identifier (scheme and value), so tracks can find it",
        ));
    }
    let schema = latest_schema(&s).await?;
    let values = check_card(&schema, &body.values).map_err(ApiError::unprocessable)?;
    let actor = actor(&headers);
    let version = schema.version;
    let entity = s
        .with_db(move |db| {
            let e = db.create_entity(body.name.as_deref(), &body.identifiers, &actor)?;
            if !values.is_empty() {
                db.put_card(&e.id, &values, version, &actor)?;
            }
            Ok(e)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(view(&s, entity.id).await?)))
}
