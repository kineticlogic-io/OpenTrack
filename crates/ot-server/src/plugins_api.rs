//! Plugins over the API (Settings → Plugins): list the built-in and added
//! ones with where each is used; add a WebAssembly component (the body,
//! `Content-Type: application/wasm`) or an external plugin (`{"address"}`);
//! enable, disable and grant; check; delete. A plugin is loaded and its
//! manifest read before it is stored, so a broken file never gets in.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use ot_plugin::{Grants, Source};
use ot_store::PluginWrite;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::actor;
use crate::control::{ApiError, AppState};

/// Largest component accepted (a Python plugin bundles its interpreter).
const MAX_PLUGIN_BYTES: usize = 128 * 1024 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/plugins",
            get(list)
                .post(add)
                .layer(DefaultBodyLimit::max(MAX_PLUGIN_BYTES)),
        )
        .route("/plugins/{name}", get(view).put(configure).delete(delete))
        .route("/plugins/{name}/check", post(check))
}

/// Where each plugin is used: sources' codecs and tracker stages, and the
/// correlation scorer.
async fn uses(s: &AppState) -> Result<Vec<(String, Value)>, ApiError> {
    s.with_db(|db| {
        let mut out = Vec::new();
        for src in db.list_sources()? {
            for (what, ptr) in [
                ("codec", "/pipeline/codec/plugin"),
                ("tracker", "/pipeline/tracker/plugin"),
            ] {
                if let Some(name) = src.spec.pointer(ptr).and_then(Value::as_str) {
                    out.push((
                        name.to_owned(),
                        json!({"source": src.id, "name": src.name, "as": what, "enabled": src.enabled}),
                    ));
                }
            }
        }
        if let Some(settings) = db.correlation_settings()?
            && let Some(name) = settings.pointer("/scorer/plugin").and_then(Value::as_str)
        {
            out.push((name.to_owned(), json!({"correlation": true, "as": "scorer"})));
        }
        Ok(out)
    })
    .await
}

fn used_by(uses: &[(String, Value)], name: &str) -> Vec<Value> {
    uses.iter()
        .filter(|(n, _)| n == name)
        .map(|(_, u)| u.clone())
        .collect()
}

/// Every plugin: built in, then added (with whether it is running here).
async fn list(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let uses = uses(&s).await?;
    let rows = s.with_db(|db| db.list_plugins()).await?;
    let mut plugins: Vec<Value> = ot_source::plugin::plugins()
        .into_iter()
        .filter(|p| ot_source::plugin::describe(p.as_ref())["builtin"] == true)
        .map(|p| {
            let mut v = ot_source::plugin::describe(p.as_ref());
            v["runtime"] = json!("builtin");
            v["enabled"] = json!(true);
            v["status"] = json!("loaded");
            v["used_by"] = json!(used_by(&uses, &p.manifest().name));
            v
        })
        .collect();
    for r in rows {
        plugins.push(row_view(&r, &uses));
    }
    Ok(Json(json!({ "plugins": plugins })))
}

fn row_view(r: &ot_store::PluginRow, uses: &[(String, Value)]) -> Value {
    let mut v = r.manifest.clone();
    if !v.is_object() {
        v = json!({});
    }
    let manifest: Option<ot_source::plugin::Manifest> =
        serde_json::from_value(r.manifest.clone()).ok();
    v["default_options"] = manifest
        .as_ref()
        .map_or(json!({}), ot_source::plugin::default_options);
    v["builtin"] = json!(false);
    v["name"] = json!(r.name);
    v["version"] = json!(r.version);
    v["runtime"] = json!(r.runtime);
    v["sha256"] = json!(r.sha256);
    v["size"] = json!(r.size);
    v["address"] = json!(r.address);
    v["grants"] = r.grants.clone();
    v["enabled"] = json!(r.enabled);
    v["updated_at_ms"] = json!(r.updated_at_ms);
    let status = match (r.enabled, crate::plugins::load_error(&r.name)) {
        (false, _) => json!("disabled"),
        (true, Some(e)) => {
            v["error"] = json!(e);
            json!("error")
        }
        (true, None) if crate::plugins::is_loaded(&r.name) => json!("loaded"),
        (true, None) => json!("loading"),
    };
    v["status"] = status;
    v["used_by"] = json!(used_by(uses, &r.name));
    v
}

async fn view(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let uses = uses(&s).await?;
    let key = name.clone();
    let row = s
        .with_db(move |db| db.get_plugin(&key))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("plugin {name}")))?;
    Ok(Json(row_view(&row, &uses)))
}

#[derive(Deserialize)]
struct AddQuery {
    /// Replace a plugin of the same name (a new build or address).
    #[serde(default)]
    replace: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalBody {
    address: String,
    #[serde(default)]
    grants: Option<Grants>,
}

/// Load a plugin off the async threads (compiling a component takes a moment).
async fn try_load(source: Source, grants: Grants) -> Result<ot_source::plugin::Manifest, ApiError> {
    tokio::task::spawn_blocking(move || ot_plugin::load(&source, &grants))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map(|p| p.manifest().clone())
        .map_err(|e| ApiError::unprocessable(format!("the plugin did not load: {e:#}")))
}

async fn add(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AddQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let wasm = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| {
            t.starts_with("application/wasm") || t.starts_with("application/octet-stream")
        });
    let (source, grants, sha) = if wasm {
        let sha = ot_plugin::sha256_hex(&body);
        (Source::Wasm(body.to_vec()), Grants::default(), Some(sha))
    } else {
        let b: ExternalBody = serde_json::from_slice(&body).map_err(|e| {
            ApiError::bad_request(format!(
                "send a component (application/wasm) or {{\"address\"}}: {e}"
            ))
        })?;
        let grants = b.grants.unwrap_or_default();
        grants.validate().map_err(ApiError::unprocessable)?;
        (Source::External(b.address), grants, None)
    };
    let manifest = try_load(source.clone(), grants.clone()).await?;
    let name = manifest.name.clone();
    if ot_source::plugin::is_builtin(&name) {
        return Err(ApiError::conflict(format!("{name} is a built-in plugin")));
    }
    let actor = actor(&headers);
    let manifest_json =
        serde_json::to_value(&manifest).map_err(|e| ApiError::internal(e.to_string()))?;
    let grants_json =
        serde_json::to_value(&grants).map_err(|e| ApiError::internal(e.to_string()))?;
    let replace = q.replace;
    let row = s
        .with_db(move |db| {
            if !replace && db.get_plugin(&manifest.name)?.is_some() {
                return Err(ot_store::StoreError::Conflict(format!(
                    "plugin {} exists: replace it to add this build",
                    manifest.name
                )));
            }
            let (wasm, address) = match &source {
                Source::Wasm(b) => (Some(b.as_slice()), None),
                Source::External(a) => (None, Some(a.as_str())),
            };
            db.put_plugin(
                &PluginWrite {
                    name: &manifest.name,
                    version: &manifest.version,
                    manifest: &manifest_json,
                    wasm: wasm.zip(sha.as_deref()),
                    address,
                    grants: &grants_json,
                },
                &actor,
            )
        })
        .await?;
    crate::plugins::sync(&s.common)
        .await
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    let uses = uses(&s).await?;
    tracing::info!(plugin = %name, runtime = %row.runtime, "plugin added");
    Ok((StatusCode::CREATED, Json(row_view(&row, &uses))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigureBody {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    grants: Option<Grants>,
}

async fn configure(
    State(s): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(b): Json<ConfigureBody>,
) -> Result<Json<Value>, ApiError> {
    if let Some(g) = &b.grants {
        g.validate().map_err(ApiError::unprocessable)?;
    }
    let key = name.clone();
    let row = s
        .with_db(move |db| db.get_plugin(&key))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("plugin {name}")))?;
    // Check it still loads with what it will run with.
    if b.enabled.unwrap_or(row.enabled) {
        let key = name.clone();
        let wasm = s.with_db(move |db| db.plugin_wasm(&key)).await?;
        let (source, mut grants) = crate::plugins::source_of(&row, wasm)
            .map_err(|e| ApiError::internal(format!("{e:#}")))?;
        if let Some(g) = &b.grants {
            grants = g.clone();
        }
        try_load(source, grants).await?;
    }
    let actor = actor(&headers);
    let grants = b
        .grants
        .map(|g| serde_json::to_value(g).unwrap_or_default());
    let key = name.clone();
    let row = s
        .with_db(move |db| db.set_plugin(&key, b.enabled, grants.as_ref(), &actor))
        .await?;
    crate::plugins::sync(&s.common)
        .await
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    let uses = uses(&s).await?;
    Ok(Json(row_view(&row, &uses)))
}

#[derive(Deserialize)]
struct DeleteQuery {
    /// Delete even while sources or correlation use it.
    #[serde(default)]
    force: bool,
}

async fn delete(
    State(s): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(q): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    let users = used_by(&uses(&s).await?, &name);
    if !users.is_empty() && !q.force {
        return Err(ApiError::conflict(format!(
            "plugin {name} is in use ({}): change those first, or delete with force",
            users
                .iter()
                .map(|u| u["source"]
                    .as_str()
                    .map_or("correlation".to_owned(), |x| format!("source {x}")))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let actor = actor(&headers);
    s.with_db(move |db| db.delete_plugin(&name, &actor)).await?;
    crate::plugins::sync(&s.common)
        .await
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Load a stored plugin afresh and open each kind it provides with its
/// default options: whether it works, without changing anything.
async fn check(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let key = name.clone();
    let (row, wasm) = s
        .with_db(move |db| Ok((db.get_plugin(&key)?, db.plugin_wasm(&key)?)))
        .await?;
    let row = row.ok_or_else(|| ApiError::not_found(format!("plugin {name}")))?;
    let (source, grants) =
        crate::plugins::source_of(&row, wasm).map_err(|e| ApiError::internal(format!("{e:#}")))?;
    let report = tokio::task::spawn_blocking(move || crate::plugin_cli::smoke(&source, &grants))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(report))
}
