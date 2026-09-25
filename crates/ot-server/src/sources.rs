//! The source-worker role: runs every enabled source.
//!
//! It polls SQLite for configuration changes and (re)starts sources whose
//! revision changed. Each source is a supervised transport (reconnecting with
//! backoff) feeding its pipeline, which appends observations to the source's
//! Redis stream. The worker also persists the static-join cache, flushes
//! per-source metrics, and publishes a live status the API reads.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use ot_source::frame::Frame;
use ot_source::pipeline::{Pipeline, StaticEntry};
use ot_source::registry::{RegistryEntry, RegistryLookup};
use ot_source::schema::{ExtensionField, ExtensionSchema};
use ot_source::source::SourceSpec;
use ot_source::transport::{self, SharedStatus};
use ot_store::RedisStore;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::Common;

const CONFIG_POLL: Duration = Duration::from_secs(2);
const REGISTRY_POLL: Duration = Duration::from_secs(15);
const FLUSH_EVERY: Duration = Duration::from_secs(5);
const STATUS_TTL: Duration = Duration::from_secs(30);

/// Registry snapshot shared by every source's pipeline.
#[derive(Default)]
pub struct Registry {
    map: RwLock<HashMap<(String, String), RegistryEntry>>,
}

impl RegistryLookup for Registry {
    fn lookup(&self, scheme: &str, value: &str) -> Option<RegistryEntry> {
        self.map
            .read()
            .ok()?
            .get(&(scheme.to_owned(), value.to_owned()))
            .cloned()
    }
}

impl Registry {
    fn replace(&self, rows: Vec<ot_store::RegistryRow>) -> usize {
        let map: HashMap<_, _> = rows
            .into_iter()
            .map(|r| {
                (
                    (r.scheme, r.value),
                    RegistryEntry {
                        entity_id: r.entity_id,
                        name: r.name,
                        expected_name: r.expected_name,
                        fields: r.fields,
                    },
                )
            })
            .collect();
        let n = map.len();
        if let Ok(mut m) = self.map.write() {
            *m = map;
        }
        n
    }
}

struct Running {
    revision: i64,
    handle: JoinHandle<()>,
}

pub async fn run(
    common: Common,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    let redis = common.open_redis().await?;
    let registry = Arc::new(Registry::default());
    let mut running: BTreeMap<String, Running> = BTreeMap::new();
    let (mut config_version, mut registry_version) = (String::new(), String::new());
    let mut config_tick = tokio::time::interval(CONFIG_POLL);
    let mut registry_tick = tokio::time::interval(REGISTRY_POLL);
    tokio::pin!(shutdown);
    tracing::info!("source workers started");
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = registry_tick.tick() => {
                let c = common.clone();
                let seen = registry_version.clone();
                let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<(String, Vec<ot_store::RegistryRow>)>> {
                    let db = c.open_db()?;
                    let v = db.registry_version()?;
                    if v == seen { return Ok(None) }
                    Ok(Some((v, db.registry_rows()?)))
                }).await?;
                match res {
                    Ok(Some((v, rows))) => {
                        let n = registry.replace(rows);
                        tracing::info!(identifiers = n, "registry snapshot loaded");
                        registry_version = v;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(error = %e, "registry refresh failed"),
                }
            }
            _ = config_tick.tick() => {
                let c = common.clone();
                let seen = config_version.clone();
                let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<(String, Vec<ot_store::SourceRow>, Schemas)>> {
                    let db = c.open_db()?;
                    let v = db.sources_version()?;
                    if v == seen { return Ok(None) }
                    Ok(Some((v, db.list_sources()?, load_schemas(&db)?)))
                }).await?;
                match res {
                    Ok(Some((v, rows, schemas))) => {
                        reconcile(&mut running, rows, &schemas, &redis, &registry);
                        config_version = v;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(error = %e, "source config refresh failed"),
                }
            }
        }
    }
    for (id, r) in running {
        r.handle.abort();
        tracing::info!(source = %id, "stopped");
    }
    Ok(())
}

/// Published extension schemas by version.
type Schemas = HashMap<u32, ExtensionSchema>;

fn load_schemas(db: &ot_store::Db) -> anyhow::Result<Schemas> {
    let mut out = HashMap::new();
    for v in db.schema_versions()? {
        if v.status != "published" {
            continue;
        }
        let fields = v
            .fields
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<ExtensionField>, _>>()?;
        out.insert(
            v.version,
            ExtensionSchema {
                version: v.version,
                fields,
            },
        );
    }
    Ok(out)
}

fn reconcile(
    running: &mut BTreeMap<String, Running>,
    rows: Vec<ot_store::SourceRow>,
    schemas: &Schemas,
    redis: &RedisStore,
    registry: &Arc<Registry>,
) {
    let wanted: BTreeMap<String, ot_store::SourceRow> = rows
        .into_iter()
        .filter(|r| r.enabled)
        .map(|r| (r.id.clone(), r))
        .collect();
    running.retain(|id, r| {
        let keep = wanted.get(id).is_some_and(|w| w.revision == r.revision);
        if !keep {
            r.handle.abort();
            tracing::info!(source = %id, "stopped (disabled, removed or reconfigured)");
        }
        keep
    });
    for (id, row) in wanted {
        if running.contains_key(&id) {
            continue;
        }
        let spec: SourceSpec = match serde_json::from_value(row.spec.clone()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(source = %id, error = %e, "stored spec no longer parses; not started");
                continue;
            }
        };
        let Some(schema) = schemas.get(&spec.pipeline.mapping.schema_version).cloned() else {
            tracing::error!(source = %id, version = spec.pipeline.mapping.schema_version,
                "mapping targets an unpublished schema version; not started");
            continue;
        };
        let handle = tokio::spawn(run_source(
            spec,
            schema,
            row.revision,
            redis.clone(),
            registry.clone(),
        ));
        tracing::info!(source = %id, revision = row.revision, "started");
        running.insert(
            id,
            Running {
                revision: row.revision,
                handle,
            },
        );
    }
}

/// Run one source forever: a supervised transport and its pipeline.
async fn run_source(
    spec: SourceSpec,
    schema: ExtensionSchema,
    revision: i64,
    redis: RedisStore,
    registry: Arc<Registry>,
) {
    let id = spec.id.clone();
    let link = SharedStatus::default();
    let (tx, rx) = mpsc::channel::<Frame>(10_000);

    let transport_cfg = spec.transport.clone();
    let link_t = link.clone();
    let tid = id.clone();
    let supervisor = tokio::spawn(async move {
        let mut backoff = Duration::from_secs(2);
        loop {
            let started = tokio::time::Instant::now();
            let res = transport::run(&transport_cfg, tx.clone(), link_t.clone()).await;
            if let Ok(mut s) = link_t.lock() {
                s.connected = false;
                if let Err(e) = &res {
                    s.errors += 1;
                    s.last_error = Some(format!("{e:#}"));
                }
            }
            match &res {
                Ok(()) => tracing::info!(source = %tid, "transport closed; reconnecting"),
                Err(e) => {
                    tracing::warn!(source = %tid, error = %format!("{e:#}"), ?backoff, "transport failed")
                }
            }
            if tx.is_closed() {
                return;
            }
            // A connection that lasted a while resets the backoff.
            if started.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_secs(2);
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    });

    let mut rx = rx;
    loop {
        match pipeline_loop(&spec, &schema, revision, &mut rx, &redis, &registry, &link).await {
            Ok(()) => break,
            Err(e) => {
                // Usually Redis; frames buffer in the channel meanwhile.
                tracing::error!(source = %id, error = %format!("{e:#}"), "pipeline failed; restarting in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    supervisor.abort();
}

async fn pipeline_loop(
    spec: &SourceSpec,
    schema: &ExtensionSchema,
    revision: i64,
    rx: &mut mpsc::Receiver<Frame>,
    redis: &RedisStore,
    registry: &Arc<Registry>,
    link: &SharedStatus,
) -> anyhow::Result<()> {
    let id = spec.id.as_str();
    let mut pipeline = Pipeline::new(id, spec.pipeline.clone())
        .context("invalid pipeline")?
        .with_schema(schema.clone());
    let static_ttl = Duration::from_secs(spec.pipeline.static_join.ttl_secs);

    // Warm the static cache so identity survives a restart.
    let mut loaded = 0;
    for (key, json) in redis.load_statics(id).await? {
        if let Ok(entry) = serde_json::from_str::<StaticEntry>(&json) {
            pipeline.load_static(key, entry);
            loaded += 1;
        }
    }
    tracing::info!(source = %id, statics = loaded, "pipeline ready");

    let mut flush = tokio::time::interval(FLUSH_EVERY);
    let mut last_error: Option<String> = None;
    let mut totals: HashMap<String, u64> = HashMap::new();
    loop {
        tokio::select! {
            frame = rx.recv() => {
                let Some(frame) = frame else { return Ok(()) };
                let out = pipeline.process(&frame, registry.as_ref());
                redis.append_observations(id, &out.observations).await?;
                for (key, entry) in &out.statics {
                    redis.put_static(id, key, &serde_json::to_string(entry)?, static_ttl).await?;
                }
                if out.last_error.is_some() {
                    last_error = out.last_error;
                }
            }
            _ = flush.tick() => {
                let counts = pipeline.take_counts().pairs();
                redis.incr_metrics(id, &counts).await?;
                for (k, n) in &counts {
                    *totals.entry(k.clone()).or_default() += n;
                }
                pipeline.prune(Utc::now());
                let link = link.lock().map(|l| l.clone()).unwrap_or_default();
                let status = json!({
                    "source": id,
                    "revision": revision,
                    "transport": spec.transport.kind(),
                    "link": link,
                    "last_error": last_error,
                    "totals_since_start": totals,
                    "updated_at": Utc::now(),
                });
                redis.put_source_status(id, &status.to_string(), STATUS_TTL).await?;
            }
        }
    }
}
