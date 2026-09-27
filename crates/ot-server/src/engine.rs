//! The engine role: source tracks in, system tracks out.
//!
//! Each new source track is correlated before it gets a system track of its
//! own: when it shares an identifier (or a trusted registry entity) with a
//! live system track from another source, and passes the kinematic sanity
//! gate, it reports for that track instead. A source track that already has
//! a system track of its own merges into a matching one when its identity
//! appears later (a static report, a registry match). Every pairing and
//! merge is a decision in the track graph with its evidence. A system track
//! with several contributors shows the best of them, field group by field
//! group (see [`crate::correlate::best_view`]).
//!
//! Source tracks with no shared identity pair kinematically: when a track's
//! reports stay within the chi-square gate of another source's track for M of
//! N comparisons, the two merge (vetoed by conflicting identifiers or
//! domains, depending on the approach). Detection sources report anonymous
//! plots: each scan is associated with the nearest system tracks inside the
//! gate (one plot per track), and plots with no track are counted, not kept.
//!
//! The engine also owns the lifecycle (tentative → confirmed → lost →
//! dropped) with per-domain stale times, and publishing through the outbox.
//! Only authoritative tracks leave OpenTrack: a system track is published
//! once it is confirmed and a source that may stand alone reports for it
//! (track feeds, by default). Tracks only sensors report for (a radar's
//! clutter, a lidar fragment, or radar and lidar agreeing on something no
//! track feed reports) stay internal. Once
//! published, a track's updates and its tombstone always follow. A source
//! can end a source track (a tracker dropping it): the link ends, and a
//! system track left with no reporting source is retired at once.
//!
//! It also resolves each track's published `attributes` against the latest
//! published output schema: the track's `ext` values (from feeds, and from
//! entity links in the source pipelines, which record where an entity
//! replaced a feed value), then linked built-ins. A newly published schema is
//! picked up within seconds and republishes the affected tracks.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use ot_core::{Contributor, Domain, Observation, PairingType, SystemTrack, TrackState, Uid};
use ot_source::schema::{ExtensionSchema, resolve_attributes};
use ot_store::{Decision, RedisStore};
use serde_json::{Map, Value, json};

use crate::config::Common;

pub(crate) mod bench;
mod history;
mod manage;
mod sync;
mod undo;
use crate::correlate::{self, Approach, Contribution, CorrelationSettings, Evidence, Grid, Mode};

pub const GROUP: &str = "engine";

#[derive(Debug, Clone)]
pub struct EngineSettings {
    pub consumer: String,
    /// Observations before a tentative track is confirmed.
    pub confirm_after: u64,
    pub stale_air: Duration,
    pub stale_surface: Duration,
    pub stale_subsurface: Duration,
    pub stale_other: Duration,
    /// Time after the last report at which a track is dropped and tombstoned.
    pub drop_after: Duration,
    /// Correlation settings until an operator saves others (then those,
    /// reloaded while running).
    pub correlation: CorrelationSettings,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            consumer: "engine-1".into(),
            confirm_after: 3,
            stale_air: Duration::from_secs(60),
            stale_surface: Duration::from_secs(15 * 60),
            stale_subsurface: Duration::from_secs(30 * 60),
            stale_other: Duration::from_secs(15 * 60),
            drop_after: Duration::from_secs(6 * 3600),
            correlation: CorrelationSettings::default(),
        }
    }
}

impl EngineSettings {
    fn stale_after(&self, domain: Option<Domain>) -> Duration {
        match domain {
            Some(Domain::Air) | Some(Domain::Space) => self.stale_air,
            Some(Domain::Surface) => self.stale_surface,
            Some(Domain::Subsurface) => self.stale_subsurface,
            Some(Domain::Ground) | None => self.stale_other,
        }
    }
}

/// What changed when an observation was applied, deciding publish urgency.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// Identity, classification or state changed: publish now.
    Significant,
    Routine,
    /// Older than what the track already shows; counted, not applied.
    OutOfOrder,
}

/// A comparison in words, for decision reasons.
fn describe(k: &correlate::Kinematic) -> String {
    let speed = k
        .speed_diff_mps
        .map(|d| format!(", speeds {:.1} m/s apart", d.abs()))
        .unwrap_or_default();
    format!("{:.0} m apart, σ {:.0} m{speed}", k.distance_m, k.sigma_m)
}

/// Apply an observation to a system track whose only contributor reported it.
pub fn apply(track: &mut SystemTrack, obs: Observation, confirm_after: u64) -> Applied {
    track.observation_count += 1;
    if let Some(c) = track.contributors.first_mut() {
        c.last_report = c.last_report.max(obs.observed_at);
        c.existence = obs.provenance.confidence;
    }
    if obs.observed_at < track.last_seen {
        return Applied::OutOfOrder;
    }
    apply_view(track, obs, confirm_after)
}

/// Show a new view on a system track (one contributor's report, or the best
/// of several) and update its state.
pub fn apply_view(track: &mut SystemTrack, mut view: Observation, confirm_after: u64) -> Applied {
    keep_identity(&track.view, &mut view);
    let before_state = track.state;
    let identity_changed = track.view.name != view.name
        || track.view.callsign != view.callsign
        || track.view.identifiers != view.identifiers
        || track.view.classification.cot_type_or_derived()
            != view.classification.cot_type_or_derived();
    track.last_seen = track.last_seen.max(view.observed_at);
    track.view = view;
    track.state = if track.observation_count >= confirm_after {
        TrackState::Confirmed
    } else {
        TrackState::Tentative
    };
    if identity_changed || track.state != before_state {
        Applied::Significant
    } else {
        Applied::Routine
    }
}

/// Identity is sticky: when the sources that named a track stop (an AIS feed
/// ends, a ship switches its transponder off) but sensors still hold it, the
/// track keeps its name, identifiers, classification and extension fields
/// rather than turning anonymous. A new view only replaces what it reports.
fn keep_identity(prev: &Observation, view: &mut Observation) {
    fn keep(v: &mut Option<String>, p: &Option<String>) {
        if v.is_none() {
            v.clone_from(p);
        }
    }
    keep(&mut view.name, &prev.name);
    keep(&mut view.callsign, &prev.callsign);
    keep(&mut view.platform.name, &prev.platform.name);
    keep(&mut view.platform.class, &prev.platform.class);
    keep(&mut view.platform.type_code, &prev.platform.type_code);
    keep(&mut view.platform.flag, &prev.platform.flag);
    keep(&mut view.platform.hull, &prev.platform.hull);
    if view.identifiers.is_empty() {
        view.identifiers.clone_from(&prev.identifiers);
    }
    let vague = |c: &ot_core::Classification| {
        matches!(
            c.effective_affiliation(),
            None | Some(ot_core::Affiliation::Unknown)
        ) && c.cot_type.is_none()
    };
    if vague(&view.classification) && !vague(&prev.classification) {
        view.classification = prev.classification.clone();
    } else if view.classification.effective_domain().is_none() {
        view.classification.domain = prev.classification.effective_domain();
    }
    for (k, v) in &prev.ext {
        view.ext.entry(k.clone()).or_insert_with(|| v.clone());
    }
}

/// A decision by the engine, stamped with the correlation version.
fn engine_decision(op: &str) -> Decision {
    Decision::new("engine", op).evidence(json!({ "correlation_version": correlate::VERSION }))
}

/// Lifecycle verdict for a track at `now`.
#[derive(Debug, PartialEq, Eq)]
pub enum Lifecycle {
    Keep,
    Lost,
    Drop,
}

pub fn lifecycle(track: &SystemTrack, now: DateTime<Utc>, s: &EngineSettings) -> Lifecycle {
    let age = (now - track.last_seen).to_std().unwrap_or_default();
    if age >= s.drop_after {
        Lifecycle::Drop
    } else if age >= s.stale_after(track.view.classification.effective_domain())
        && track.state != TrackState::Lost
    {
        Lifecycle::Lost
    } else {
        Lifecycle::Keep
    }
}

/// What attribute resolution needs: the latest published output schema, and
/// the entities pinned to system tracks, with the versions they were loaded at.
#[derive(Default)]
struct Attributes {
    version: u32,
    schema: Option<ExtensionSchema>,
    registry: String,
    /// `tms-<UID>` → the entity pinned to that track (identifier `track:tms-<UID>`).
    pins: HashMap<String, Pin>,
    /// Entity id → a track manager's publish override: always (true) or never.
    publish: HashMap<String, bool>,
    /// Every active identifier → its entity, to re-apply sources' entity
    /// links when an entity is saved.
    lookup: BTreeMap<(String, String), ot_source::registry::RegistryEntry>,
}

/// An entity pinned to one system track: a track manager's designation of a
/// track no identifier resolves (a radar track, say). It applies to the
/// track itself, whatever reports for it, and follows it through merges.
#[derive(Debug, Clone, PartialEq)]
struct Pin {
    entity_id: String,
    fields: Map<String, Value>,
}

/// The identifier scheme that pins an entity to a system track.
pub const TRACK_SCHEME: &str = "track";

/// Apply a pinned entity to a track's view: its OTH-GOLD minimum and its
/// attributes (as `ext.<key>`), recording where it replaced another value.
fn apply_pin(pin: &Pin, t: &mut SystemTrack) -> Vec<ot_core::AttributeNotice> {
    let mut notices = Vec::new();
    let source = "track manager".to_owned();
    let mut note = |key: &str, was: Option<Value>, now: &Value| {
        if let Some(was) = was.filter(|w| !w.is_null() && w != now) {
            notices.push(ot_core::AttributeNotice {
                key: key.to_owned(),
                entity: now.clone(),
                feed: was,
                source_id: source.clone(),
            });
        }
    };
    let text = |v: &Value| v.as_str().map(str::to_owned);
    let to_json = |v: Option<&String>| v.map(|s| Value::String(s.clone()));
    for (k, v) in &pin.fields {
        let view = &mut t.view;
        match k.as_str() {
            "name" => {
                note("name", to_json(view.name.as_ref()), v);
                view.name = text(v);
            }
            "class_name" => {
                note("platform.class", to_json(view.platform.class.as_ref()), v);
                view.platform.class = text(v);
            }
            "cot_type" => {
                note(
                    "classification.cot_type",
                    to_json(view.classification.cot_type.as_ref()),
                    v,
                );
                view.classification.cot_type = text(v);
            }
            "sidc" => {
                note(
                    "classification.sidc",
                    to_json(view.classification.sidc.as_ref()),
                    v,
                );
                view.classification.sidc = text(v);
            }
            "domain" => {
                if let Ok(d) = serde_json::from_value(v.clone()) {
                    note(
                        "classification.domain",
                        serde_json::to_value(view.classification.domain).ok(),
                        v,
                    );
                    view.classification.domain = Some(d);
                }
            }
            "affiliation" => {
                if let Ok(a) = serde_json::from_value(v.clone()) {
                    note(
                        "classification.affiliation",
                        serde_json::to_value(view.classification.affiliation).ok(),
                        v,
                    );
                    view.classification.affiliation = Some(a);
                }
            }
            "track_type" => {
                if let Ok(tt) = serde_json::from_value(v.clone()) {
                    note("track_type", serde_json::to_value(view.track_type).ok(), v);
                    view.track_type = Some(tt);
                }
            }
            "registry" | "entity" => {}
            key => {
                note(&format!("ext.{key}"), view.ext.get(key).cloned(), v);
                view.ext.insert(key.to_owned(), v.clone());
            }
        }
    }
    notices
}

/// The entity a track's registry match points to, when it was corroborated
/// and unambiguous (so its links were used).
fn entity_of(t: &SystemTrack) -> Option<String> {
    crate::correlate::trusted_entity(&t.view)
}

/// Re-resolve a track's entity, attributes and notices. True if any changed.
/// An entity pinned to the track (or a track merged into it) applies first
/// and is the track's entity.
fn resolve(attrs: &Attributes, t: &mut SystemTrack) -> bool {
    let pin = std::iter::once(t.uid)
        .chain(t.aliases.iter().copied())
        .find_map(|u| attrs.pins.get(&u.doc_id()))
        .cloned();
    let pinned = pin.as_ref().map(|p| apply_pin(p, t)).unwrap_or_default();
    let entity = pin.map(|p| p.entity_id).or_else(|| entity_of(t));
    let entity_changed = entity != t.entity_id;
    t.entity_id = entity;
    let (values, mut notices) = match &attrs.schema {
        Some(schema) => resolve_attributes(schema, t),
        None => (Map::new(), Vec::new()),
    };
    notices.extend(pinned);
    let changed = entity_changed || values != t.attributes || notices != t.notices;
    t.attributes = values;
    t.notices = notices;
    changed
}

/// Per-cycle counters, added to the `_engine` metrics.
#[derive(Debug, Default)]
struct EngineCounts {
    /// Observations applied to a system track.
    observations: u64,
    /// System tracks created for new source tracks.
    created: u64,
    /// New source tracks paired onto an existing system track.
    paired: u64,
    /// System tracks merged into another (shared identity).
    merged: u64,
    /// System tracks merged into another on kinematic agreement.
    kinematic_merged: u64,
    /// Detections associated with a system track.
    associated: u64,
    /// Detections with no system track inside the gate.
    unassociated: u64,
    /// Source tracks their source ended.
    ended: u64,
    /// System tracks retired because every source track ended.
    retired: u64,
    /// Pair or split suggestions made (in suggest mode, or for splits).
    suggested: u64,
    /// Source tracks split off their system track.
    split: u64,
    /// Changes published at once rather than throttled.
    urgent: u64,
    out_of_order: u64,
    unreadable: u64,
    lost: u64,
    dropped: u64,
}

impl EngineCounts {
    fn pairs(&self) -> Vec<(String, u64)> {
        [
            ("observations", self.observations),
            ("created", self.created),
            ("paired", self.paired),
            ("merged", self.merged),
            ("kinematic_merged", self.kinematic_merged),
            ("associated", self.associated),
            ("unassociated", self.unassociated),
            ("ended", self.ended),
            ("retired", self.retired),
            ("suggested", self.suggested),
            ("split", self.split),
            ("urgent", self.urgent),
            ("out_of_order", self.out_of_order),
            ("unreadable", self.unreadable),
            ("lost", self.lost),
            ("dropped", self.dropped),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(k, n)| (k.to_owned(), n))
        .collect()
    }
}

/// What saving a system track writes besides its state.
enum Saved {
    Quiet,
    Publish { urgent: bool },
    Withdraw { reason: String },
}

/// A scorer plugin as the engine holds it.
struct OpenScorer {
    /// The settings it was opened for, and the plugin build (`<name>-<version>`).
    key: (correlate::ScorerRef, String),
    scorer: Option<Box<dyn ot_source::plugin::PluginScorer>>,
    /// A failure was logged (once, until it works again).
    reported: bool,
}

pub struct Engine {
    common: Common,
    settings: EngineSettings,
    redis: RedisStore,
    /// `<source>/<key>` → system track.
    reports: HashMap<String, Uid>,
    tracks: HashMap<Uid, SystemTrack>,
    /// `<source>/<key>` → its latest report, for best-source selection.
    latest: HashMap<String, Observation>,
    /// Identity key (see [`correlate::identity_keys`]) → the system track
    /// that carries it. Checked on use, so a stale entry is harmless.
    index: HashMap<String, Uid>,
    sources: Vec<String>,
    /// Source id → priority (higher wins best-source selection).
    priorities: HashMap<String, i64>,
    /// Sources that report detections rather than tracks.
    detection_sources: HashSet<String>,
    /// Source id → whether a track it alone reports for is published.
    alone: HashMap<String, bool>,
    /// Source id → reports a new track needs from it to be confirmed (unset:
    /// the engine's default).
    confirm: HashMap<String, u64>,
    /// Source id → its pipeline's entity stage, to re-apply entity links.
    stages: HashMap<String, ot_source::registry::RegistryStage>,
    /// Correlation settings from the command line, used until saved ones exist.
    default_correlation: CorrelationSettings,
    /// Version of the saved settings and "do not pair" decisions loaded.
    correlation_version: String,
    /// Source track keys an operator said are different objects (sorted pairs).
    do_not_pair: HashSet<(String, String)>,
    /// Recent disagreements of a source track with the rest of its system track.
    misses: HashMap<(Uid, String), Evidence>,
    /// When each open suggestion was last written (at most every 10 s).
    suggested: HashMap<String, DateTime<Utc>>,
    /// Splits an operator rejected, not proposed again for a while.
    split_rejected: HashMap<(Uid, String), DateTime<Utc>>,
    /// Where each live system track is, for kinematic comparisons.
    grid: Grid<Uid>,
    /// Recent gate results per pair of system tracks (lower uid first).
    candidates: HashMap<(Uid, Uid), Evidence>,
    attrs: Attributes,
    /// Groups a track manager formed, published as tracks of their own
    /// (kept apart from `tracks`: correlation never sees them).
    groups: HashMap<Uid, manage::GroupState>,
    /// Tracks changed by track management, saved on the next tick.
    dirty: HashSet<Uid>,
    /// Keep [`Self::trace`] and [`Self::associations`] (replays and
    /// benchmarks; never a live engine, where they would only grow).
    pub(crate) recording: bool,
    /// Where each detection went, for scoring replays.
    pub(crate) associations: Vec<(String, Option<Uid>)>,
    /// Every report, association and merge in order, for replay videos.
    pub(crate) trace: Vec<Value>,
    /// The time of the report being processed (for traced merges).
    clock: Option<DateTime<Utc>>,
    /// Scenario time for a replay run faster than real time: lifecycle and
    /// timers follow it instead of the wall clock.
    pub(crate) sim_now: Option<DateTime<Utc>>,
    /// The correlation settings' scorer plugin, once opened.
    scorer: Option<OpenScorer>,
    /// Processing a batch of observations: saves and source reports wait in
    /// the two maps below and are written together when the batch ends.
    defer: bool,
    deferred_saves: HashMap<Uid, Saved>,
    deferred_sources: HashMap<String, Observation>,
    /// How long each track's position history is kept (Settings; None: none).
    history_keep: Option<Duration>,
    /// At most one history point per track this often (ms), and when each
    /// track's last one was.
    history_every_ms: i64,
    history_last: HashMap<Uid, i64>,
    /// How long a read waits for observations when none are queued (zero:
    /// not at all; Redis checks block timeouts only every 100 ms or so).
    pub(crate) read_block: Duration,
    /// This node's clock for the replicated decision log.
    hlc: ot_sync::Clock,
    /// Set by a command whose logged form is not what it was given (an
    /// accepted suggestion is logged as the merge it made).
    canonical: Option<Value>,
    /// Applying another node's decision: UIDs it minted are kept.
    remote: bool,
    /// Where each ended source track last reported, for scoring replays.
    #[cfg(test)]
    ended_on: HashMap<String, Uid>,
}

const DEFAULT_PRIORITY: i64 = 100;

/// The contributor key a detection source reports under on each system track
/// it has plots associated with.
pub const DETECTIONS: &str = "~detections";

fn contributor_key(c: &Contributor) -> String {
    format!("{}/{}", c.source_id, c.source_track_key)
}

/// Where a contributor's latest report is kept: per source track, or per
/// system track for a detection source (whose plots reach many tracks).
fn latest_key(uid: Uid, source: &str, key: &str) -> String {
    if key == DETECTIONS {
        format!("{source}/{key}@{}", uid.doc_id())
    } else {
        format!("{source}/{key}")
    }
}

fn pair_key(a: Uid, b: Uid) -> (Uid, Uid) {
    if a < b { (a, b) } else { (b, a) }
}

impl Engine {
    pub async fn new(mut common: Common, settings: EngineSettings) -> anyhow::Result<Self> {
        // The engine writes a decision for every new, paired and ended track:
        // keep one connection open for them.
        common.share_db()?;
        let redis = common.open_redis().await?;
        let c = common.clone();
        let (reports, last_hlc) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let db = c.open_db()?;
            Ok((db.live_reports()?, db.sync_last_hlc()?))
        })
        .await??;
        let (stored_groups, tracks): (Vec<SystemTrack>, Vec<SystemTrack>) = redis
            .list_system_tracks()
            .await?
            .into_iter()
            .partition(|t| t.kind == ot_core::TrackKind::Group);
        let tracks: HashMap<Uid, SystemTrack> = tracks.into_iter().map(|t| (t.uid, t)).collect();
        tracing::info!(
            links = reports.len(),
            tracks = tracks.len(),
            "engine state loaded"
        );
        let default_correlation = settings.correlation.clone();
        let mut engine = Self {
            common,
            settings,
            redis,
            reports: reports.into_iter().collect(),
            tracks,
            latest: HashMap::new(),
            index: HashMap::new(),
            sources: Vec::new(),
            priorities: HashMap::new(),
            detection_sources: HashSet::new(),
            alone: HashMap::new(),
            confirm: HashMap::new(),
            stages: HashMap::new(),
            default_correlation,
            correlation_version: String::new(),
            do_not_pair: HashSet::new(),
            misses: HashMap::new(),
            suggested: HashMap::new(),
            split_rejected: HashMap::new(),
            grid: Grid::default(),
            candidates: HashMap::new(),
            attrs: Attributes::default(),
            groups: HashMap::new(),
            dirty: HashSet::new(),
            recording: cfg!(test),
            associations: Vec::new(),
            trace: Vec::new(),
            clock: None,
            sim_now: None,
            read_block: Duration::from_secs(1),
            scorer: None,
            defer: false,
            deferred_saves: HashMap::new(),
            deferred_sources: HashMap::new(),
            history_keep: Some(Duration::from_secs_f64(
                crate::settings_api::DEFAULT_HISTORY_HOURS * 3600.0,
            )),
            history_every_ms: (crate::settings_api::DEFAULT_HISTORY_INTERVAL_SECS * 1000.0) as i64,
            history_last: HashMap::new(),
            hlc: ot_sync::Clock::after(last_hlc),
            canonical: None,
            remote: false,
            #[cfg(test)]
            ended_on: HashMap::new(),
        };
        let uids: Vec<Uid> = engine.tracks.keys().copied().collect();
        for uid in uids {
            engine.index_track(uid);
            let p = engine.tracks[&uid].view.position;
            engine.grid.put(uid, p.latitude, p.longitude);
        }
        engine.load_groups(stored_groups).await?;
        Ok(engine)
    }

    pub async fn run(
        mut self,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> anyhow::Result<()> {
        tokio::pin!(shutdown);
        // Re-read anything delivered to this consumer before a restart.
        self.refresh_sources().await?;
        self.refresh_attributes().await?;
        self.pump(true).await?;
        // A batch is never abandoned part way (it holds read but unacknowledged
        // observations and deferred writes): timers and shutdown are looked
        // at between batches, and a batch waits at most `read_block`.
        let (mut next_refresh, mut next_reap) = (Instant::now(), Instant::now());
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => break,
                _ = std::future::ready(()) => {}
            }
            let now = Instant::now();
            if now >= next_refresh {
                next_refresh = now + Duration::from_secs(5);
                if let Err(e) = self.refresh_sources().await {
                    tracing::warn!(error = %format!("{e:#}"), "source list refresh failed");
                }
                if let Err(e) = self.refresh_correlation().await {
                    tracing::warn!(error = %format!("{e:#}"), "correlation settings refresh failed");
                }
                if let Err(e) = self.refresh_attributes().await {
                    tracing::warn!(error = %format!("{e:#}"), "output schema refresh failed");
                }
                if let Err(e) = self.refresh_groups().await {
                    tracing::warn!(error = %format!("{e:#}"), "group refresh failed");
                }
                if let Err(e) = self.retry_pending().await {
                    tracing::warn!(error = %format!("{e:#}"), "pending decisions retry failed");
                }
            }
            if now >= next_reap {
                next_reap = now + Duration::from_secs(10);
                if let Err(e) = self.reap().await {
                    tracing::warn!(error = %format!("{e:#}"), "lifecycle sweep failed");
                }
            }
            if let Err(e) = self.pump(false).await {
                tracing::warn!(error = %format!("{e:#}"), "engine cycle failed; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        tracing::info!(tracks = self.tracks.len(), "engine stopped");
        Ok(())
    }

    pub(crate) async fn refresh_sources(&mut self) -> anyhow::Result<()> {
        let c = self.common.clone();
        type Row = (
            String,
            i64,
            bool,
            bool,
            Option<u64>,
            ot_source::registry::RegistryStage,
        );
        let rows: Vec<Row> = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok(c.open_db()?
                .list_sources()?
                .into_iter()
                .filter(|s| s.enabled)
                .map(|s| {
                    // A tracker stage turns detections into tracks before they get here.
                    let detections = s.spec.get("reports").and_then(|r| r.as_str())
                        == Some("detections")
                        && s.spec
                            .pointer("/pipeline/tracker")
                            .is_none_or(Value::is_null);
                    let spec =
                        serde_json::from_value::<ot_source::source::SourceSpec>(s.spec.clone())
                            .ok();
                    let alone = spec.as_ref().is_none_or(|spec| spec.publishes_alone());
                    let confirm = spec.as_ref().and_then(|spec| spec.confirms_after());
                    let stage = spec
                        .and_then(|spec| spec.pipeline.registry)
                        .unwrap_or_default();
                    (s.id, s.priority, detections, alone, confirm, stage)
                })
                .collect())
        })
        .await??;
        self.priorities = rows.iter().map(|r| (r.0.clone(), r.1)).collect();
        self.confirm = rows
            .iter()
            .filter_map(|r| r.4.map(|n| (r.0.clone(), n)))
            .collect();
        self.stages = rows.iter().map(|r| (r.0.clone(), r.5.clone())).collect();
        self.alone = rows.iter().map(|r| (r.0.clone(), r.3)).collect();
        self.detection_sources = rows.iter().filter(|r| r.2).map(|r| r.0.clone()).collect();
        let mut ids: Vec<String> = rows.into_iter().map(|r| r.0).collect();
        ids.sort();
        if ids != self.sources {
            self.redis.ensure_obs_groups(&ids, GROUP).await?;
            tracing::info!(sources = ?ids, "engine consuming");
            self.sources = ids;
        }
        Ok(())
    }

    /// Reload the output schema when a new version is published, and
    /// republish every live track whose attributes change as a result.
    pub(crate) async fn refresh_attributes(&mut self) -> anyhow::Result<()> {
        let c = self.common.clone();
        let (hours, every) = tokio::task::spawn_blocking(move || -> anyhow::Result<(f64, f64)> {
            Ok(crate::settings_api::history_retention(
                &c.open_db()?.app_settings()?,
            ))
        })
        .await??;
        self.history_keep = (hours > 0.0).then(|| Duration::from_secs_f64(hours * 3600.0));
        self.history_every_ms = (every * 1000.0) as i64;
        let c = self.common.clone();
        let known = (
            self.attrs.version,
            self.attrs.schema.is_some(),
            self.attrs.registry.clone(),
        );
        let loaded = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Attributes>> {
            let db = c.open_db()?;
            let version = db.latest_published_schema()?;
            let registry = db.registry_version()?;
            if (version, true, registry.clone()) == known {
                return Ok(None);
            }
            let schema = crate::sources::load_schemas(&db)?
                .into_values()
                .max_by_key(|s| s.version);
            let rows = db.registry_rows()?;
            let lookup = rows
                .iter()
                .map(|r| {
                    (
                        (r.scheme.clone(), r.value.clone()),
                        ot_source::registry::RegistryEntry {
                            entity_id: r.entity_id.clone(),
                            name: r.name.clone(),
                            expected_name: r.expected_name.clone(),
                            fields: r.fields.clone(),
                        },
                    )
                })
                .collect();
            let publish = rows
                .iter()
                .filter_map(|r| match r.fields.get("publish").and_then(Value::as_str) {
                    Some("always") => Some((r.entity_id.clone(), true)),
                    Some("never") => Some((r.entity_id.clone(), false)),
                    _ => None,
                })
                .collect();
            let pins = rows
                .into_iter()
                .filter(|r| r.scheme == TRACK_SCHEME)
                .map(|r| {
                    let mut fields = r.fields;
                    if let Some(n) = r.name {
                        fields.insert("name".into(), Value::String(n));
                    }
                    (
                        r.value,
                        Pin {
                            entity_id: r.entity_id,
                            fields,
                        },
                    )
                })
                .collect();
            Ok(Some(Attributes {
                version,
                schema,
                registry,
                pins,
                publish,
                lookup,
            }))
        })
        .await??;
        let Some(attrs) = loaded else {
            return Ok(());
        };
        let registry_changed = attrs.registry != self.attrs.registry;
        self.attrs = attrs;
        if registry_changed {
            let n = self.reapply_entities().await?;
            if n > 0 {
                tracing::info!(tracks = n, "entity changes applied to live tracks");
            }
        }
        let mut changed = Vec::new();
        for track in self.tracks.values_mut() {
            if resolve(&self.attrs, track) {
                changed.push(track.uid);
            }
        }
        let republished = changed.len();
        for uid in changed {
            self.save(uid, true).await?;
        }
        tracing::info!(
            schema = self.attrs.schema.as_ref().map(|s| s.version),
            pins = self.attrs.pins.len(),
            republished,
            "output schema and pinned entities loaded"
        );
        Ok(())
    }

    /// Now: the wall clock, or the scenario's time in a fast replay.
    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.sim_now.unwrap_or_else(Utc::now)
    }

    /// Process one batch of queued observations; how many were read.
    pub(crate) async fn pump(&mut self, pending: bool) -> anyhow::Result<usize> {
        // A batch that failed part way leaves its writes deferred: write them first.
        if self.defer {
            self.defer = false;
            self.flush_deferred().await?;
        }
        if let Err(e) = self.process_commands().await {
            tracing::warn!(error = %format!("{e:#}"), "operator commands failed");
        }
        let batch = self
            .redis
            .read_observations(
                &self.sources,
                GROUP,
                &self.settings.consumer,
                1000,
                self.read_block,
                pending,
            )
            .await?;
        if batch.is_empty() {
            return Ok(0);
        }
        let read = batch.len();
        let mut counts = EngineCounts::default();
        let mut acks: HashMap<String, Vec<String>> = HashMap::new();
        let mut reports = Vec::with_capacity(batch.len());
        let mut trimmed: HashMap<String, usize> = HashMap::new();
        for (source, id, obs) in batch {
            acks.entry(source.clone()).or_default().push(id.clone());
            match obs {
                Ok(o) => reports.push(o),
                Err(e) if e == ot_store::redis_store::TRIMMED => {
                    *trimmed.entry(source).or_default() += 1;
                }
                Err(e) => {
                    tracing::warn!(%source, %id, error = %e, "unreadable observation skipped");
                    counts.unreadable += 1;
                }
            }
        }
        for (source, n) in trimmed {
            tracing::warn!(
                %source,
                count = n,
                "observations trimmed from the stream before the engine read them"
            );
        }
        // A batch holds each source's reports in turn: take them in time order.
        reports.sort_by_key(|o| o.observed_at);
        self.defer = true;
        let mut reports = reports.into_iter().peekable();
        while let Some(obs) = reports.next() {
            self.clock = Some(obs.observed_at);
            if self.detection_sources.contains(&obs.source_id) {
                // One scan: every plot of this source at this instant.
                let mut scan = vec![obs];
                while let Some(next) = reports.next_if(|o| {
                    o.source_id == scan[0].source_id && o.observed_at == scan[0].observed_at
                }) {
                    scan.push(next);
                }
                self.associate(scan, &mut counts).await?;
                continue;
            }
            if obs.state == Some(TrackState::Dropped) {
                self.end_source_track(&obs, &mut counts).await?;
                continue;
            }
            if self.defer {
                self.deferred_sources.insert(
                    format!("{}/{}", obs.source_id, obs.source_track_key),
                    obs.clone(),
                );
            } else {
                self.redis
                    .put_source_track(&obs, Duration::from_secs(24 * 3600))
                    .await?;
            }
            let key = format!("{}/{}", obs.source_id, obs.source_track_key);
            let uid = match self.reports.get(&key).copied() {
                Some(uid) => self
                    .maybe_merge(uid, &obs, &mut counts)
                    .await?
                    .unwrap_or(uid),
                None => self.place(&obs, &mut counts).await?,
            };
            match self.observe(uid, obs.clone()).await? {
                Some((_, urgent)) => {
                    counts.observations += 1;
                    counts.urgent += u64::from(urgent);
                    self.save(uid, urgent).await?;
                    self.pair_kinematically(uid, &obs, &mut counts).await?;
                    if let Some(now) = self.reports.get(&key).copied() {
                        self.check_split(now, &obs, &mut counts).await?;
                    }
                }
                None => counts.out_of_order += 1,
            }
            if self.recording {
                self.trace.push(json!({
                    "t": obs.observed_at, "kind": "report", "source": obs.source_id,
                    "key": obs.source_track_key, "lat": obs.position.latitude,
                    "lon": obs.position.longitude,
                    "uid": self.reports.get(&key).map(|u| u.doc_id()),
                }));
            }
        }
        self.defer = false;
        self.flush_deferred().await?;
        for (source, ids) in acks {
            self.redis.ack_observations(&source, GROUP, &ids).await?;
        }
        self.redis
            .incr_metrics(crate::metrics::ENGINE, &counts.pairs())
            .await?;
        Ok(read)
    }

    /// Record the identity keys a track's view carries, unless another live
    /// track already holds them (that conflict is what pairing resolves).
    fn index_track(&mut self, uid: Uid) {
        let Some(t) = self.tracks.get(&uid) else {
            return;
        };
        for k in correlate::identity_keys(&t.view) {
            match self.index.get(&k) {
                Some(u) if *u != uid && self.tracks.contains_key(u) => {}
                _ => {
                    self.index.insert(k, uid);
                }
            }
        }
    }

    /// A live system track (other than `exclude`) that `obs` shares an
    /// identity with, has no report from the same source, and passes the
    /// sanity gate; with the evidence for the decision.
    fn find_match(&self, obs: &Observation, exclude: Option<Uid>) -> Option<(Uid, Value)> {
        for k in correlate::identity_keys(obs) {
            let Some(&uid) = self.index.get(&k) else {
                continue;
            };
            if Some(uid) == exclude {
                continue;
            }
            let Some(t) = self.tracks.get(&uid) else {
                continue;
            };
            // The index can lag a track's view; trust only what it still carries.
            if !correlate::identity_keys(&t.view).contains(&k) {
                continue;
            }
            // A feed reports distinct objects under distinct keys.
            if t.contributors.iter().any(|c| c.source_id == obs.source_id) {
                continue;
            }
            // An operator said they are different objects.
            if self.key_forbidden(&format!("{}/{}", obs.source_id, obs.source_track_key), uid) {
                continue;
            }
            let gate = correlate::sanity_gate(obs, &t.view, &self.settings.correlation.gate);
            if !gate.pass {
                tracing::debug!(identity = %k, track = %uid.doc_id(), ?gate, "identity match outside the sanity gate");
                continue;
            }
            return Some((
                uid,
                json!({ "rule": "identifier", "identity": k, "gate": gate }),
            ));
        }
        None
    }

    /// Where a new source track goes: onto a matching system track, or a new one.
    async fn place(&mut self, obs: &Observation, counts: &mut EngineCounts) -> anyhow::Result<Uid> {
        if obs.source_id.starts_with(sync::PEER)
            && let Ok(uid) = obs.source_track_key.parse::<Uid>()
        {
            return self.adopt(obs, uid, counts).await;
        }
        let key = format!("{}/{}", obs.source_id, obs.source_track_key);
        let (source, track_key) = (obs.source_id.clone(), obs.source_track_key.clone());
        let c = self.common.clone();
        // A tracker formed this source track from the source's detections:
        // keep that in its lineage.
        let formed = obs
            .provenance
            .tracker
            .as_ref()
            .map(|t| json!({ "tracker": t }));
        if let Some((uid, evidence)) = self.find_match(obs, None) {
            let attrs = json!({ "pairing": "auto", "confidence": 1.0, "evidence": evidence });
            let decision = engine_decision("pair")
                .reason(format!(
                    "{key} shares {} with {}",
                    evidence["identity"].as_str().unwrap_or("an identity"),
                    uid.doc_id()
                ))
                .evidence(evidence);
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let mut db = c.open_db()?;
                db.pair_source_track(&source, &track_key, uid, &attrs, decision)?;
                if let Some(formed) = formed {
                    db.note_source_track(&source, &track_key, &formed)?;
                }
                Ok(())
            })
            .await??;
            if let Some(t) = self.tracks.get_mut(&uid) {
                t.contributors.push(Contributor {
                    source_id: obs.source_id.clone(),
                    source_track_key: obs.source_track_key.clone(),
                    pairing: PairingType::Auto,
                    confidence: 1.0,
                    last_report: obs.observed_at,
                    existence: obs.provenance.confidence,
                });
            }
            self.reports.insert(key, uid);
            counts.paired += 1;
            return Ok(uid);
        }
        let site = c.site;
        let (uid, _) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let mut db = c.open_db()?;
            let created = db.create_system_track(
                site,
                &source,
                &track_key,
                engine_decision("create_system_track")
                    .reason("new source track with no identity match"),
            )?;
            if let Some(formed) = formed {
                db.note_source_track(&source, &track_key, &formed)?;
            }
            Ok(created)
        })
        .await??;
        self.reports.insert(key, uid);
        counts.created += 1;
        Ok(uid)
    }

    /// A source track alone on its system track whose identity now matches
    /// another system track merges with it (into the older of the two when
    /// both are single-source). Returns the surviving track.
    async fn maybe_merge(
        &mut self,
        uid: Uid,
        obs: &Observation,
        counts: &mut EngineCounts,
    ) -> anyhow::Result<Option<Uid>> {
        let Some(t) = self.tracks.get(&uid) else {
            return Ok(None);
        };
        if t.contributors.len() != 1 {
            return Ok(None);
        }
        let Some((other, evidence)) = self.find_match(obs, Some(uid)) else {
            return Ok(None);
        };
        if self.forbidden(uid, other) {
            return Ok(None);
        }
        let (from, into) = self.merge_order(uid, other);
        let decision = engine_decision("merge")
            .reason(format!(
                "{} and {} share {}",
                from.doc_id(),
                into.doc_id(),
                evidence["identity"].as_str().unwrap_or("an identity")
            ))
            .evidence(evidence);
        let c = self.common.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            Ok(c.open_db()?.merge_system_tracks(from, into, decision)?)
        })
        .await??;
        self.absorb(from, into).await?;
        counts.merged += 1;
        Ok(Some(into))
    }

    /// Move `from`'s contributors onto `into` in memory and retire `from`
    /// (its tombstone is published; it stays resolvable as an alias).
    async fn absorb(&mut self, from: Uid, into: Uid) -> anyhow::Result<()> {
        let Some(gone) = self.tracks.remove(&from) else {
            return Ok(());
        };
        if self.recording {
            self.trace.push(json!({
                "t": self.clock, "kind": "merge", "from": from.doc_id(), "into": into.doc_id(),
            }));
        }
        self.grid.remove(from);
        self.candidates.retain(|(a, b), _| *a != from && *b != from);
        let has_detections_from = |t: &SystemTrack, source: &str| {
            t.contributors
                .iter()
                .any(|c| c.source_id == source && c.source_track_key == DETECTIONS)
        };
        let mut gone = gone;
        let published = gone.is_published();
        let into_track = self.tracks.get(&into);
        gone.contributors.retain(|c| {
            if c.source_track_key != DETECTIONS {
                return true;
            }
            // A detection source's plots on `from` count for `into` unless it has its own.
            let old = latest_key(from, &c.source_id, DETECTIONS);
            let keep = into_track.is_some_and(|t| !has_detections_from(t, &c.source_id));
            if let Some(o) = self.latest.remove(&old)
                && keep
            {
                self.latest
                    .insert(latest_key(into, &c.source_id, DETECTIONS), o);
            }
            keep
        });
        for c in gone
            .contributors
            .iter()
            .filter(|c| c.source_track_key != DETECTIONS)
        {
            self.reports.insert(contributor_key(c), into);
        }
        for v in self.index.values_mut() {
            if *v == from {
                *v = into;
            }
        }
        self.detach(from, Some(into));
        if let Some(t) = self.tracks.get_mut(&into) {
            for g in &gone.groups {
                if !t.groups.contains(g) {
                    t.groups.push(g.clone());
                }
            }
            for p in &gone.paired_with {
                if *p != into && !t.paired_with.contains(p) {
                    t.paired_with.push(*p);
                }
            }
            for c in gone.contributors {
                if !t
                    .contributors
                    .iter()
                    .any(|x| contributor_key(x) == contributor_key(&c))
                {
                    t.contributors.push(c);
                }
            }
            t.aliases.push(from);
            t.aliases.extend(gone.aliases);
            t.observation_count += gone.observation_count;
            t.first_seen = t.first_seen.min(gone.first_seen);
        }
        self.retire_in_redis(from, published, &format!("merged into {}", into.doc_id()))
            .await?;
        Ok(())
    }

    /// Which of two tracks survives a merge, as (from, into): the published
    /// one, so consumers keep the id they know; else the older.
    fn merge_order(&self, a: Uid, b: Uid) -> (Uid, Uid) {
        let (ta, tb) = (&self.tracks[&a], &self.tracks[&b]);
        match (ta.is_published(), tb.is_published()) {
            (true, false) => (b, a),
            (false, true) => (a, b),
            _ if tb.first_seen <= ta.first_seen => (a, b),
            _ => (b, a),
        }
    }

    /// Tombstone a retired track for consumers if they ever saw it, else
    /// just forget it.
    async fn retire_in_redis(&self, uid: Uid, published: bool, reason: &str) -> anyhow::Result<()> {
        if published {
            self.redis.retire_system_track(uid, reason).await?;
        } else {
            self.redis.forget_system_track(uid).await?;
        }
        // Suggestions about it are moot.
        let c = self.common.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            Ok(c.open_db()?.expire_suggestions(uid)?)
        })
        .await??;
        Ok(())
    }

    /// Reload correlation settings and "do not pair" decisions when they
    /// changed (an operator saved settings, rejected a pairing, split a track).
    pub(crate) async fn refresh_correlation(&mut self) -> anyhow::Result<()> {
        type Loaded = (String, Option<Value>, Vec<(String, String)>);
        let c = self.common.clone();
        let known = self.correlation_version.clone();
        let loaded = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Loaded>> {
            let db = c.open_db()?;
            let version = db.correlation_version()?;
            if version == known {
                return Ok(None);
            }
            Ok(Some((
                version,
                db.correlation_settings()?,
                db.do_not_pairs()?,
            )))
        })
        .await??;
        let Some((version, saved, pairs)) = loaded else {
            return Ok(());
        };
        self.correlation_version = version;
        self.do_not_pair = pairs.into_iter().collect();
        let settings = match saved {
            None => self.default_correlation.clone(),
            Some(v) => match serde_json::from_value::<CorrelationSettings>(v)
                .map_err(|e| e.to_string())
                .and_then(|s| s.validate().map(|_| s))
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "saved correlation settings are invalid; keeping the current ones");
                    self.settings.correlation.clone()
                }
            },
        };
        if settings != self.settings.correlation {
            tracing::info!(?settings, "correlation settings loaded");
            self.settings.correlation = settings;
        }
        Ok(())
    }

    /// A track's own source tracks (not detection sources' contributions).
    fn keys_of(&self, uid: Uid) -> Vec<String> {
        self.tracks
            .get(&uid)
            .map(|t| {
                t.contributors
                    .iter()
                    .filter(|c| c.source_track_key != DETECTIONS)
                    .map(contributor_key)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn keys_forbidden(&self, a: &str, b: &str) -> bool {
        let pair = if a < b { (a, b) } else { (b, a) };
        self.do_not_pair
            .iter()
            .any(|(x, y)| x == pair.0 && y == pair.1)
    }

    /// Whether an operator said this source track is not the object a track shows.
    fn key_forbidden(&self, key: &str, uid: Uid) -> bool {
        !self.do_not_pair.is_empty()
            && self
                .keys_of(uid)
                .iter()
                .any(|k| self.keys_forbidden(key, k))
    }

    /// Whether an operator said two tracks are different objects.
    /// Put the correlation settings' scorer plugin's evidence in place of
    /// the kinematic comparisons (its ln likelihood ratio and gate), and
    /// return the plugin (`<name>-<version>`) with what it said per
    /// candidate. Without a scorer, or when it fails, the comparisons stand.
    fn score_with_plugin(
        &mut self,
        obs: &Observation,
        compared: &mut [(Uid, correlate::Kinematic)],
    ) -> Option<(String, HashMap<Uid, Value>)> {
        let r = self.settings.correlation.scorer.clone()?;
        if compared.is_empty() {
            return None;
        }
        let Some(plugin) = ot_source::plugin::plugin(&r.plugin) else {
            if self.scorer.as_ref().is_none_or(|s| !s.reported) {
                tracing::warn!(plugin = %r.plugin, "scorer plugin not loaded: kinematic scores used");
                self.scorer = Some(OpenScorer {
                    key: (r, String::new()),
                    scorer: None,
                    reported: true,
                });
            }
            return None;
        };
        let version = format!("{}-{}", r.plugin, plugin.manifest().version);
        let key = (r.clone(), version.clone());
        if self.scorer.as_ref().is_none_or(|s| s.key != key) {
            let opened = plugin.scorer(&r.options);
            if let Err(e) = &opened {
                tracing::warn!(plugin = %version, error = %e, "scorer plugin did not open: kinematic scores used");
            }
            self.scorer = Some(OpenScorer {
                key,
                scorer: opened.ok(),
                reported: false,
            });
        }
        let candidates: Vec<ot_source::plugin::ScoreCandidate> = compared
            .iter()
            .map(|(u, k)| ot_source::plugin::ScoreCandidate {
                view: self
                    .tracks
                    .get(u)
                    .and_then(|t| serde_json::to_value(&t.view).ok())
                    .unwrap_or(Value::Null),
                kinematic: serde_json::to_value(k).unwrap_or(Value::Null),
            })
            .collect();
        let open = self.scorer.as_mut()?;
        let scorer = open.scorer.as_mut()?;
        match scorer.score(obs, &candidates) {
            Ok(scores) if scores.len() == compared.len() => {
                open.reported = false;
                let mut by = HashMap::new();
                for ((u, k), sc) in compared.iter_mut().zip(scores) {
                    if sc.ln_lr.is_finite() {
                        k.ln_lr = sc.ln_lr;
                        k.pass = sc.pass;
                    }
                    by.insert(*u, sc.evidence);
                }
                Some((version, by))
            }
            other => {
                if !open.reported {
                    let why = match other {
                        Ok(s) => format!("{} scores for {} candidates", s.len(), compared.len()),
                        Err(e) => e,
                    };
                    tracing::warn!(plugin = %version, error = %why, "scorer plugin failed: kinematic scores used");
                    open.reported = true;
                }
                None
            }
        }
    }

    fn forbidden(&self, a: Uid, b: Uid) -> bool {
        !self.do_not_pair.is_empty() && self.keys_of(a).iter().any(|k| self.key_forbidden(k, b))
    }

    /// Record (or refresh) a suggestion; true when it is new.
    async fn suggest(
        &mut self,
        kind: &'static str,
        a: Uid,
        b: Option<Uid>,
        source_track: Option<String>,
        mut evidence: Value,
        reason: String,
    ) -> anyhow::Result<bool> {
        let key = format!(
            "{kind}:{a}:{}:{}",
            b.map(|u| u.to_string()).unwrap_or_default(),
            source_track.clone().unwrap_or_default()
        );
        let now = self.now();
        let first = !self.suggested.contains_key(&key);
        if self
            .suggested
            .get(&key)
            .is_some_and(|t| now - *t < chrono::Duration::seconds(10))
        {
            return Ok(false);
        }
        self.suggested.insert(key, now);
        evidence["reason"] = json!(reason);
        evidence["correlation_version"] = json!(correlate::VERSION);
        let c = self.common.clone();
        let (a, b) = (a.to_string(), b.map(|u| u.to_string()));
        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            Ok(c.open_db()?.upsert_suggestion(
                kind,
                &a,
                b.as_deref(),
                source_track.as_deref(),
                &evidence,
            )?)
        })
        .await??;
        Ok(first)
    }

    /// Compare a source track's report with the rest of its system track;
    /// when it has disagreed for M of the last N comparisons, propose (or,
    /// with automatic splits, make) a split.
    async fn check_split(
        &mut self,
        uid: Uid,
        obs: &Observation,
        counts: &mut EngineCounts,
    ) -> anyhow::Result<()> {
        let split = self.settings.correlation.split;
        if !split.propose && !split.automatic {
            return Ok(());
        }
        let kin = self.settings.correlation.kinematic;
        let key = format!("{}/{}", obs.source_id, obs.source_track_key);
        // A track manager's merge holds: correlation does not undo it.
        if self.tracks.get(&uid).is_some_and(|t| {
            t.contributors
                .iter()
                .any(|c| contributor_key(c) == key && c.pairing == PairingType::Manual)
        }) {
            return Ok(());
        }
        let others: Vec<(String, String)> = match self.tracks.get(&uid) {
            Some(t) => t
                .contributors
                .iter()
                .filter(|c| c.source_track_key != DETECTIONS && contributor_key(c) != key)
                .map(|c| (contributor_key(c), c.source_id.clone()))
                .collect(),
            None => return Ok(()),
        };
        if others.is_empty() {
            return Ok(());
        }
        let max_age = chrono::Duration::milliseconds((kin.max_age_secs * 1000.0) as i64);
        let contribs: Vec<Contribution<'_>> = others
            .iter()
            .filter_map(|(k, _)| self.latest.get(k))
            .filter(|o| (obs.observed_at - o.observed_at).abs() <= max_age)
            .map(|o| Contribution {
                obs: o,
                priority: self
                    .priorities
                    .get(&o.source_id)
                    .copied()
                    .unwrap_or(DEFAULT_PRIORITY),
            })
            .collect();
        if contribs.is_empty() {
            return Ok(());
        }
        let (view, _) = correlate::best_view(&contribs, self.settings.correlation.freshness_secs);
        let k = correlate::kinematic(obs, &view, &kin);
        let far = k.d2 > correlate::chi2_quantile(split.gate_probability, k.dof);
        if self.recording {
            self.trace.push(json!({
                "t": obs.observed_at, "kind": "split_check", "key": key, "uid": uid.doc_id(),
                "d": k.distance_m, "sigma": k.sigma_m, "d2": k.d2, "ln_lr": k.ln_lr, "far": far,
                "view_age_s": (obs.observed_at - view.observed_at).num_milliseconds() as f64 / 1000.0,
            }));
        }
        // The split's own window; the rest's last report may be reused,
        // weighted, as pairing does (a slow feed would otherwise give too
        // few comparisons to ever split).
        let within = correlate::KinematicSettings {
            window_secs: split.window_secs.max(kin.window_secs),
            ..kin
        };
        let reuse = {
            let (r, v) = (k.sigma_report_m.powi(2), k.sigma_view_m.powi(2));
            (kin.reuse_views && r + v > 0.0).then(|| r / (r + v))
        };
        let Some(tally) = self
            .misses
            .entry((uid, key.clone()))
            .or_default()
            .record_reusing(
                (&key, obs.observed_at),
                ("rest", view.observed_at),
                k.ln_lr,
                far,
                (split.n, &within),
                reuse,
            )
        else {
            return Ok(());
        };
        // Paired at the pairing threshold; its comparisons since move it.
        let p = correlate::posterior(kin.pair_probability, tally.ln_lr);
        let of = tally.comparisons;
        if let Some(c) = self.tracks.get_mut(&uid).and_then(|t| {
            t.contributors
                .iter_mut()
                .find(|c| contributor_key(c) == key)
        }) {
            c.confidence = (p * 1e4).round() / 1e4;
        }
        if tally.outside < split.m || p > split.split_probability {
            return Ok(());
        }
        self.misses.remove(&(uid, key.clone()));
        // Which one leaves: of two, the one that may not stand alone (a sensor
        // that followed the wrong object), else the one that disagrees.
        let alone = |s: &str| self.alone.get(s).copied().unwrap_or(true);
        let leaving = match others.as_slice() {
            [(other, other_source)] if alone(&obs.source_id) && !alone(other_source) => {
                other.clone()
            }
            _ => key,
        };
        if self
            .split_rejected
            .get(&(uid, leaving.clone()))
            .is_some_and(|t| self.now() - *t < chrono::Duration::minutes(30))
        {
            return Ok(());
        }
        let evidence = json!({
            "rule": "divergence", "probability": p, "comparisons": of,
            "outside_gate": tally.outside, "split_gate_probability": split.gate_probability,
            "split_probability": split.split_probability, "last": k,
        });
        let reason = format!(
            "{leaving} stopped agreeing with the rest of {}: probability {p:.3} of the same object over {of} comparisons ({})",
            uid.doc_id(),
            describe(&k)
        );
        if split.automatic {
            self.split(uid, &leaving, "engine", &reason, evidence)
                .await?;
            counts.split += 1;
        } else if self
            .suggest("split", uid, None, Some(leaving), evidence, reason)
            .await?
        {
            counts.suggested += 1;
        }
        Ok(())
    }

    /// Split a source track (`<source>/<key>`) off its system track onto a
    /// new one, and record that the two are different objects so they do not
    /// pair again. Returns the new track.
    async fn split(
        &mut self,
        uid: Uid,
        leaving: &str,
        actor: &str,
        reason: &str,
        evidence: Value,
    ) -> anyhow::Result<Uid> {
        if !self.tracks.contains_key(&uid) {
            anyhow::bail!("{} is not a live track", uid.doc_id());
        }
        let keys = self.keys_of(uid);
        if !keys.iter().any(|k| k == leaving) {
            anyhow::bail!("{leaving} does not report for {}", uid.doc_id());
        }
        if keys.len() < 2 {
            anyhow::bail!("{leaving} is the only source track of {}", uid.doc_id());
        }
        let rest: Vec<String> = keys.into_iter().filter(|k| k != leaving).collect();
        let (source, track_key) = leaving
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("source track {leaving:?} is not <source>/<key>"))?;
        let decision = Decision::new(actor, "split")
            .reason(reason)
            .evidence(evidence)
            .evidence(json!({
                "correlation_version": correlate::VERSION,
                "from": uid.doc_id(),
                "source_track": leaving,
            }));
        let dnp = Decision::new(actor, "do_not_pair")
            .reason(format!("{leaving} split from {}", uid.doc_id()));
        let c = self.common.clone();
        let (source, track_key, left, rest_keys) = (
            source.to_owned(),
            track_key.to_owned(),
            leaving.to_owned(),
            rest.clone(),
        );
        let new = tokio::task::spawn_blocking(move || -> anyhow::Result<Uid> {
            let mut db = c.open_db()?;
            let (new, split) = db.split_source_track(c.site, &source, &track_key, decision)?;
            // Made with the split: undoing the split undoes it too.
            let dnp = dnp.evidence(json!({ "with_decision": split }));
            db.do_not_pair(&[left], &rest_keys, dnp)?;
            Ok(new)
        })
        .await??;
        for r in &rest {
            let pair = if leaving < r.as_str() {
                (leaving.to_owned(), r.clone())
            } else {
                (r.clone(), leaving.to_owned())
            };
            self.do_not_pair.insert(pair);
        }
        if let Some(t) = self.tracks.get_mut(&uid) {
            t.contributors.retain(|c| contributor_key(c) != leaving);
        }
        self.reports.insert(leaving.to_owned(), new);
        let latest = match self.latest.get(leaving).cloned() {
            Some(o) => Some(o),
            None => {
                let (s, k) = leaving.split_once('/').expect("checked above");
                self.redis.get_source_track(s, k).await?
            }
        };
        if let Some(obs) = latest {
            let mut t = SystemTrack::from_first_observation(new, obs);
            t.observation_count = self.settings.confirm_after;
            t.state = TrackState::Confirmed;
            resolve(&self.attrs, &mut t);
            self.grid
                .put(new, t.view.position.latitude, t.view.position.longitude);
            self.tracks.insert(new, t);
            self.index_track(new);
            self.save(new, true).await?;
        }
        // The track left behind shows only what its remaining sources say,
        // identity included: it may have come from the one that left.
        self.load_missing(uid).await?;
        if let Some((view, provenance)) = self.best_of(uid)
            && let Some(t) = self.tracks.get_mut(&uid)
        {
            t.view = view;
            t.provenance = provenance;
            resolve(&self.attrs, t);
        }
        self.index_track(uid);
        self.save(uid, true).await?;
        self.misses.retain(|(u, _), _| *u != uid);
        self.candidates.retain(|(a, b), _| *a != uid && *b != uid);
        Ok(new)
    }

    /// Merge two tracks on an operator's word.
    async fn operator_merge(
        &mut self,
        from: Uid,
        into: Uid,
        actor: &str,
        reason: String,
        evidence: Value,
    ) -> anyhow::Result<Uid> {
        for u in [from, into] {
            if !self.tracks.contains_key(&u) {
                anyhow::bail!("{} is not a live track", u.doc_id());
            }
        }
        if from == into {
            anyhow::bail!("cannot merge a track into itself");
        }
        let decision = Decision::new(actor, "merge")
            .reason(reason)
            .evidence(evidence)
            .evidence(json!({
                "correlation_version": correlate::VERSION,
                "from": from.doc_id(),
                "into": into.doc_id(),
            }));
        let c = self.common.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            Ok(c.open_db()?.merge_system_tracks(from, into, decision)?)
        })
        .await??;
        self.absorb(from, into).await?;
        self.republish(into).await?;
        Ok(into)
    }

    /// Record an operator's word that two tracks are different objects.
    async fn operator_do_not_pair(
        &mut self,
        a: Uid,
        b: Uid,
        actor: &str,
        reason: String,
    ) -> anyhow::Result<()> {
        let (ka, kb) = (self.keys_of(a), self.keys_of(b));
        if ka.is_empty() || kb.is_empty() {
            anyhow::bail!("both tracks must be live");
        }
        let decision = Decision::new(actor, "do_not_pair").reason(reason);
        let c = self.common.clone();
        let (la, lb) = (ka.clone(), kb.clone());
        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            Ok(c.open_db()?.do_not_pair(&la, &lb, decision)?)
        })
        .await??;
        for x in &ka {
            for y in &kb {
                let pair = if x < y {
                    (x.clone(), y.clone())
                } else {
                    (y.clone(), x.clone())
                };
                self.do_not_pair.insert(pair);
            }
        }
        self.candidates.remove(&pair_key(a, b));
        Ok(())
    }

    /// Run queued operator commands, answering each.
    async fn process_commands(&mut self) -> anyhow::Result<()> {
        for cmd in self.redis.pop_commands(20).await? {
            let id = cmd["id"].as_str().unwrap_or_default().to_owned();
            let result = match self.command(&cmd).await {
                Ok(v) => json!({ "ok": true, "result": v }),
                Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
            };
            tracing::info!(command = %cmd, %result, "operator command");
            if !id.is_empty() {
                self.redis.put_command_result(&id, &result).await?;
            }
        }
        Ok(())
    }

    /// Run a command as given (see [`Self::command`] for the log).
    async fn run_command(&mut self, cmd: &Value) -> anyhow::Result<Value> {
        let actor = cmd["actor"].as_str().unwrap_or("operator").to_owned();
        let uid = |k: &str| -> anyhow::Result<Uid> {
            let s = cmd[k]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("{k} is required"))?;
            s.trim_start_matches("tms-")
                .parse()
                .map_err(|e| anyhow::anyhow!("{k}: {e}"))
        };
        let op = cmd["op"].as_str().unwrap_or_default();
        match op {
            "accept" | "reject" => {
                let id = cmd["suggestion"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("suggestion is required"))?;
                let c = self.common.clone();
                let s = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                    Ok(c.open_db()?.suggestion(id)?)
                })
                .await??
                .ok_or_else(|| anyhow::anyhow!("no suggestion {id}"))?;
                if s.status != "open" {
                    anyhow::bail!("suggestion {id} is already {}", s.status);
                }
                let a: Uid = s.track_a.parse()?;
                let common = self.common.clone();
                let close = |status: &'static str, d: Option<Decision>| {
                    let c = common.clone();
                    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                        Ok(c.open_db()?.close_suggestion(id, status, d)?)
                    })
                };
                let reason = s.evidence["reason"].as_str().unwrap_or_default().to_owned();
                match (s.kind.as_str(), op == "accept") {
                    ("pair", accept) => {
                        let b: Uid = s
                            .track_b
                            .as_deref()
                            .ok_or_else(|| {
                                anyhow::anyhow!("pair suggestion without a second track")
                            })?
                            .parse()?;
                        if !self.tracks.contains_key(&a) || !self.tracks.contains_key(&b) {
                            close("expired", None).await??;
                            anyhow::bail!(
                                "suggestion {id} is out of date: a track no longer exists"
                            );
                        }
                        if accept {
                            close("accepted", None).await??;
                            let reason = format!("accepted suggestion {id}: {reason}");
                            let into = self
                                .operator_merge(b, a, &actor, reason.clone(), s.evidence.clone())
                                .await?;
                            self.canonical = Some(json!({
                                "op": "merge", "from": b.doc_id(), "into": into.doc_id(), "reason": reason,
                            }));
                            Ok(json!({ "merged_into": into.doc_id() }))
                        } else {
                            let reason = format!("rejected suggestion {id}: {reason}");
                            self.operator_do_not_pair(a, b, &actor, reason.clone())
                                .await?;
                            self.canonical = Some(json!({
                                "op": "do_not_pair", "a": a.doc_id(), "b": b.doc_id(), "reason": reason,
                            }));
                            close("rejected", None).await??;
                            Ok(json!({}))
                        }
                    }
                    ("split", accept) => {
                        let leaving = s.source_track.clone().ok_or_else(|| {
                            anyhow::anyhow!("split suggestion without a source track")
                        })?;
                        if accept {
                            let new = self
                                .split(
                                    a,
                                    &leaving,
                                    &actor,
                                    &format!("accepted suggestion {id}: {reason}"),
                                    s.evidence.clone(),
                                )
                                .await?;
                            close("accepted", None).await??;
                            Ok(json!({ "new_track": new.doc_id() }))
                        } else {
                            let now = self.now();
                            self.split_rejected.insert((a, leaving.clone()), now);
                            let d = Decision::new(&actor, "reject_split").reason(format!(
                                "{leaving} stays on {}: rejected suggestion {id}",
                                a.doc_id()
                            ));
                            close("rejected", Some(d)).await??;
                            Ok(json!({}))
                        }
                    }
                    (other, _) => anyhow::bail!("unknown suggestion kind {other:?}"),
                }
            }
            "split" => {
                let track = uid("track")?;
                let leaving = cmd["source_track"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("source_track is required"))?;
                let reason = cmd["reason"].as_str().unwrap_or("split by an operator");
                let new = self
                    .split(track, leaving, &actor, reason, json!({}))
                    .await?;
                Ok(json!({ "new_track": new.doc_id() }))
            }
            "merge" => {
                let (from, into) = (uid("from")?, uid("into")?);
                let reason = cmd["reason"]
                    .as_str()
                    .unwrap_or("merged by an operator")
                    .to_owned();
                let into = self
                    .operator_merge(from, into, &actor, reason, json!({}))
                    .await?;
                // A track manager's merge (GOLD MRG) holds: every source
                // track on the survivor is the manager's, which correlation
                // does not split off.
                if cmd["hold"].as_bool() == Some(true)
                    && let Some(t) = self.tracks.get_mut(&into)
                {
                    for c in &mut t.contributors {
                        c.pairing = PairingType::Manual;
                    }
                    self.save(into, true).await?;
                }
                Ok(json!({ "merged_into": into.doc_id() }))
            }
            // Retire every live track (published ones are tombstoned), and
            // with `history` delete the track graph too.
            "purge" => {
                // Groups first: they are dissolved, not just retired.
                let groups: Vec<Uid> = self.groups.keys().copied().collect();
                for g in groups {
                    self.manage(
                        "group_dissolve",
                        &json!({ "group": g.doc_id(), "reason": "the operator purged every track" }),
                        &actor,
                    )
                    .await?;
                }
                let all = self.redis.list_system_tracks().await?;
                let published: HashMap<Uid, bool> =
                    all.iter().map(|t| (t.uid, t.is_published())).collect();
                let uids: Vec<Uid> = published.keys().copied().collect();
                let history = cmd["history"].as_bool().unwrap_or(false);
                let (c, who, ids) = (self.common.clone(), actor.clone(), uids.clone());
                let purged = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                    let mut db = c.open_db()?;
                    for uid in ids {
                        let d =
                            Decision::new(&who, "purge").reason("the operator purged every track");
                        match db.retire_system_track(uid, d) {
                            Ok(_) | Err(ot_store::StoreError::NotFound(_)) => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                    Ok(if history {
                        Some(db.purge_track_history(Decision::new(&who, "purge_track_history"))?)
                    } else {
                        None
                    })
                })
                .await??;
                for uid in &uids {
                    let was_published = if self.tracks.contains_key(uid) {
                        self.forget(*uid)
                    } else {
                        published[uid]
                    };
                    self.retire_in_redis(*uid, was_published, "purged by an operator")
                        .await?;
                }
                self.candidates.clear();
                self.misses.clear();
                Ok(json!({
                    "retired": uids.len(),
                    "history": purged.map(|(nodes, edges)| json!({"nodes": nodes, "edges": edges})),
                }))
            }
            "do_not_pair" => {
                let (a, b) = (uid("a")?, uid("b")?);
                let reason = cmd["reason"]
                    .as_str()
                    .unwrap_or("different objects, by an operator")
                    .to_owned();
                self.operator_do_not_pair(a, b, &actor, reason).await?;
                Ok(json!({}))
            }
            "delete_history_point" => {
                let track = uid("track")?;
                let t = cmd["t"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("t (Unix ms) is required"))?;
                let reason = cmd["reason"]
                    .as_str()
                    .unwrap_or("a bad position, deleted by a track manager");
                self.delete_history_point(track, t, &actor, reason).await
            }
            "undo" => {
                let id = cmd["decision"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("decision is required"))?;
                let reason = cmd["reason"]
                    .as_str()
                    .unwrap_or("undone by a track manager");
                self.undo(id, &actor, reason).await
            }
            op if manage::OPS.contains(&op) => self.manage(op, cmd, &actor).await,
            other => anyhow::bail!("unknown command {other:?}"),
        }
    }

    /// An entity was saved: re-apply each source's entity links to the
    /// latest report of every source track that resolved to an entity, with
    /// what its feed reported put back first, and republish the tracks that
    /// change. An edit shows at once, not at the object's next report.
    async fn reapply_entities(&mut self) -> anyhow::Result<usize> {
        // After a restart the latest reports are loaded on demand.
        let with_entity: Vec<Uid> = self
            .tracks
            .values()
            .filter(|t| t.entity_id.is_some())
            .map(|t| t.uid)
            .collect();
        for uid in with_entity {
            self.load_missing(uid).await?;
        }
        let keys: Vec<String> = self
            .latest
            .iter()
            .filter(|(k, o)| !k.contains('@') && o.ext.contains_key("registry"))
            .map(|(k, _)| k.clone())
            .collect();
        let mut touched = HashSet::new();
        for key in keys {
            let o = &self.latest[&key];
            let stage = self.stages.get(&o.source_id).cloned().unwrap_or_default();
            let mut v = serde_json::to_value(o)?;
            let overrides = v
                .pointer("/ext/registry/overrides")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for ov in overrides {
                if let (Some(field), Some(reported)) = (ov["field"].as_str(), ov.get("reported"))
                    && let Ok(path) = field.parse::<ot_source::path::Path>()
                {
                    path.set(&mut v, reported.clone());
                }
            }
            if let Some(ext) = v.get_mut("ext").and_then(Value::as_object_mut) {
                ext.remove("registry");
            }
            stage.run(&mut v, &self.attrs.lookup);
            let Ok(new) = serde_json::from_value::<Observation>(v) else {
                continue;
            };
            if new == *o {
                continue;
            }
            if let Some(uid) = self.reports.get(&key).copied() {
                touched.insert(uid);
            }
            self.latest.insert(key, new);
        }
        let n = touched.len();
        for uid in touched {
            self.republish(uid).await?;
        }
        Ok(n)
    }

    /// Reports a track needs to be confirmed: the fewest any of its sources
    /// asks for (a track feed's report confirms what a radar also sees).
    fn confirm_for(&self, uid: Uid) -> u64 {
        self.tracks
            .get(&uid)
            .into_iter()
            .flat_map(|t| &t.contributors)
            .map(|c| {
                self.confirm
                    .get(&c.source_id)
                    .copied()
                    .unwrap_or(self.settings.confirm_after)
            })
            .min()
            .unwrap_or(self.settings.confirm_after)
    }

    /// Whether a track is authoritative enough to publish: confirmed, and
    /// reported for by a source that may stand alone. Sensors agreeing with
    /// each other are not enough.
    fn authoritative(&self, t: &SystemTrack) -> bool {
        t.state == TrackState::Confirmed
            && t.contributors
                .iter()
                .any(|c| self.alone.get(&c.source_id).copied().unwrap_or(true))
    }

    /// Store a track's state; publish it if it is (or once was) authoritative.
    async fn save(&mut self, uid: Uid, urgent: bool) -> anyhow::Result<()> {
        // Decided now (the published and filtered flags matter to what the
        // batch does next); inside a batch, written once at its end.
        let Some(decision) = self.decide_save(uid, urgent) else {
            return Ok(());
        };
        if self.defer {
            let merged = match (self.deferred_saves.remove(&uid), decision) {
                // A withdrawal stands unless the track is published again.
                (Some(Saved::Withdraw { reason }), Saved::Quiet) => Saved::Withdraw { reason },
                (Some(Saved::Publish { urgent: a }), Saved::Publish { urgent: b }) => {
                    Saved::Publish { urgent: a || b }
                }
                (Some(Saved::Publish { urgent: true }), Saved::Quiet) => {
                    Saved::Publish { urgent: true }
                }
                (_, d) => d,
            };
            self.deferred_saves.insert(uid, merged);
            return Ok(());
        }
        let reason = match &decision {
            Saved::Withdraw { reason } => format!("output filter: {reason}"),
            _ => String::new(),
        };
        let write = match &decision {
            Saved::Publish { urgent } => ot_store::TrackWrite::Publish { urgent: *urgent },
            Saved::Withdraw { .. } => ot_store::TrackWrite::Withdraw { reason: &reason },
            Saved::Quiet => ot_store::TrackWrite::Quiet,
        };
        let due = self.history_due(uid);
        let t = &self.tracks[&uid];
        self.redis
            .write_batch(&[], Duration::ZERO, &[(t, write, due)], self.history_keep)
            .await?;
        Ok(())
    }

    /// Whether a track's position is due a history point: its first, one
    /// at least `history_every_ms` after the last, or one out of order.
    fn history_due(&mut self, uid: Uid) -> bool {
        let Some(t) = self.tracks.get(&uid) else {
            return false;
        };
        let at = t.view.observed_at.timestamp_millis();
        match self.history_last.get(&uid) {
            Some(&last) if at >= last && at - last < self.history_every_ms => false,
            _ => {
                self.history_last.insert(uid, at);
                true
            }
        }
    }

    /// Write every save and source report the batch deferred, in one round trip.
    async fn flush_deferred(&mut self) -> anyhow::Result<()> {
        let decisions: Vec<(Uid, Saved)> = std::mem::take(&mut self.deferred_saves)
            .into_iter()
            .collect();
        let reasons: Vec<String> = decisions
            .iter()
            .map(|(_, d)| match d {
                Saved::Withdraw { reason } => format!("output filter: {reason}"),
                _ => String::new(),
            })
            .collect();
        let due: HashSet<Uid> = decisions
            .iter()
            .map(|(u, _)| *u)
            .filter(|u| self.history_due(*u))
            .collect();
        let tracks: Vec<(&SystemTrack, ot_store::TrackWrite<'_>, bool)> = decisions
            .iter()
            .zip(&reasons)
            .filter_map(|((uid, d), reason)| {
                let w = match d {
                    Saved::Publish { urgent } => ot_store::TrackWrite::Publish { urgent: *urgent },
                    Saved::Withdraw { .. } => ot_store::TrackWrite::Withdraw { reason },
                    Saved::Quiet => ot_store::TrackWrite::Quiet,
                };
                self.tracks.get(uid).map(|t| (t, w, due.contains(uid)))
            })
            .collect();
        let sources: Vec<&Observation> = self.deferred_sources.values().collect();
        self.redis
            .write_batch(
                &sources,
                Duration::from_secs(24 * 3600),
                &tracks,
                self.history_keep,
            )
            .await?;
        self.deferred_sources.clear();
        Ok(())
    }

    /// Whether a track is published, withdrawn or only stored, with its
    /// published and filtered flags set to match.
    fn decide_save(&mut self, uid: Uid, urgent: bool) -> Option<Saved> {
        let t = self.tracks.get(&uid)?;
        // A track manager's override on the track's entity comes first.
        let forced = t
            .entity_id
            .as_ref()
            .and_then(|e| self.attrs.publish.get(e))
            .copied();
        let held = match forced {
            Some(true) => None,
            Some(false) => Some("its entity is set never to publish".to_owned()),
            None => self.settings.correlation.output.rejects(t),
        };
        let was = t.is_published();
        let publish = held.is_none() && (was || forced == Some(true) || self.authoritative(t));
        let t = self.tracks.get_mut(&uid).expect("checked above");
        t.filtered = held.clone();
        Some(if publish {
            t.published = Some(true);
            Saved::Publish {
                urgent: urgent || !was,
            }
        } else if was && let Some(why) = held {
            // Published, and now filtered out: withdrawn downstream.
            t.published = Some(false);
            Saved::Withdraw { reason: why }
        } else {
            Saved::Quiet
        })
    }

    /// A source ended one of its tracks: end its link, and retire the
    /// system track if no source track reports for it any more.
    async fn end_source_track(
        &mut self,
        obs: &Observation,
        counts: &mut EngineCounts,
    ) -> anyhow::Result<()> {
        let key = format!("{}/{}", obs.source_id, obs.source_track_key);
        self.latest.remove(&key);
        let Some(uid) = self.reports.remove(&key) else {
            return Ok(());
        };
        #[cfg(test)]
        self.ended_on.insert(key.clone(), uid);
        counts.ended += 1;
        let (source, track_key) = (obs.source_id.clone(), obs.source_track_key.clone());
        let c = self.common.clone();
        let reason = format!("{key} ended by its source");
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            c.open_db()?.end_source_track(
                &source,
                &track_key,
                engine_decision("end_source_track").reason(reason),
            )?;
            Ok(())
        })
        .await??;
        let empty = match self.tracks.get_mut(&uid) {
            Some(t) => {
                t.contributors.retain(|c| {
                    !(c.source_id == obs.source_id && c.source_track_key == obs.source_track_key)
                });
                t.contributors
                    .iter()
                    .all(|c| c.source_track_key == DETECTIONS)
            }
            None => return Ok(()),
        };
        if !empty {
            return self.republish(uid).await;
        }
        counts.retired += 1;
        let c = self.common.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let d = engine_decision("retire_system_track")
                .reason("every source track reporting for it ended");
            match c.open_db()?.retire_system_track(uid, d) {
                Ok(_) | Err(ot_store::StoreError::NotFound(_)) => Ok(()),
                Err(e) => Err(e.into()),
            }
        })
        .await??;
        let published = self.forget(uid);
        self.retire_in_redis(uid, published, "every source track ended")
            .await
    }

    /// Drop a track from memory; returns whether it was published.
    fn forget(&mut self, uid: Uid) -> bool {
        self.detach(uid, None);
        let published = match self.tracks.remove(&uid) {
            Some(t) => {
                for c in &t.contributors {
                    self.latest
                        .remove(&latest_key(uid, &c.source_id, &c.source_track_key));
                }
                t.is_published()
            }
            None => false,
        };
        self.grid.remove(uid);
        self.history_last.remove(&uid);
        self.candidates.retain(|(a, b), _| *a != uid && *b != uid);
        self.reports.retain(|_, u| *u != uid);
        published
    }

    /// Load contributors' latest reports that are not in memory (after a restart).
    async fn load_missing(&mut self, uid: Uid) -> anyhow::Result<()> {
        let missing: Vec<(String, String)> = self.tracks[&uid]
            .contributors
            .iter()
            .filter(|c| {
                c.source_track_key != DETECTIONS && !self.latest.contains_key(&contributor_key(c))
            })
            .map(|c| (c.source_id.clone(), c.source_track_key.clone()))
            .collect();
        for (source, key) in missing {
            if let Some(o) = self.redis.get_source_track(&source, &key).await? {
                self.latest.insert(format!("{source}/{key}"), o);
            }
        }
        Ok(())
    }

    /// Apply a report to the system track its source track reports for.
    /// Returns the track to publish and whether it is urgent, or None when
    /// the report is older than that source track's latest.
    async fn observe(
        &mut self,
        uid: Uid,
        obs: Observation,
    ) -> anyhow::Result<Option<(SystemTrack, bool)>> {
        let key = latest_key(uid, &obs.source_id, &obs.source_track_key);
        if self
            .latest
            .get(&key)
            .is_some_and(|prev| obs.observed_at < prev.observed_at)
        {
            return Ok(None);
        }
        self.latest.insert(key.clone(), obs.clone());
        if !self.tracks.contains_key(&uid) {
            let confirm = self
                .confirm
                .get(&obs.source_id)
                .copied()
                .unwrap_or(self.settings.confirm_after);
            let mut t = SystemTrack::from_first_observation(uid, obs);
            if t.observation_count >= confirm {
                t.state = TrackState::Confirmed;
            }
            resolve(&self.attrs, &mut t);
            self.tracks.insert(uid, t.clone());
            self.index_track(uid);
            self.grid
                .put(uid, t.view.position.latitude, t.view.position.longitude);
            return Ok(Some((t, true)));
        }
        let multi = self.tracks[&uid].contributors.len() > 1;
        if multi {
            self.load_missing(uid).await?;
        }
        let best = multi.then(|| self.best_of(uid)).flatten();
        let confirm = self.confirm_for(uid);
        let t = self.tracks.get_mut(&uid).expect("checked above");
        let before = t.entity_id.clone();
        let outcome = match best {
            Some((view, provenance)) => {
                t.observation_count += 1;
                if let Some(c) = t.contributors.iter_mut().find(|c| {
                    c.source_id == obs.source_id && c.source_track_key == obs.source_track_key
                }) {
                    c.last_report = c.last_report.max(obs.observed_at);
                    c.existence = obs.provenance.confidence;
                }
                t.provenance = provenance;
                apply_view(t, view, confirm)
            }
            None => apply(t, obs, confirm),
        };
        if outcome == Applied::OutOfOrder {
            return Ok(None);
        }
        resolve(&self.attrs, t);
        let urgent = outcome == Applied::Significant || before != t.entity_id;
        let out = t.clone();
        self.index_track(uid);
        self.grid
            .put(uid, out.view.position.latitude, out.view.position.longitude);
        Ok(Some((out, urgent)))
    }

    /// The best view of a system track from its contributors' latest reports.
    fn best_of(&self, uid: Uid) -> Option<(Observation, BTreeMap<String, String>)> {
        let t = self.tracks.get(&uid)?;
        let contribs: Vec<Contribution<'_>> = t
            .contributors
            .iter()
            .filter_map(|c| {
                self.latest
                    .get(&latest_key(uid, &c.source_id, &c.source_track_key))
                    .map(|o| Contribution {
                        obs: o,
                        priority: self
                            .priorities
                            .get(&c.source_id)
                            .copied()
                            .unwrap_or(DEFAULT_PRIORITY),
                    })
            })
            .collect();
        (!contribs.is_empty())
            .then(|| correlate::best_view(&contribs, self.settings.correlation.freshness_secs))
    }

    /// Sources still reporting for a track (within the kinematic max age of
    /// `at`) through source tracks of their own; not detections, which the
    /// engine associated. A source's track that stopped reporting does not
    /// block its next track of the same object (a sensor re-initiating).
    fn track_sources(&self, uid: Uid, at: DateTime<Utc>) -> HashSet<&str> {
        let max_age = chrono::Duration::milliseconds(
            (self.settings.correlation.kinematic.source_live_secs * 1000.0) as i64,
        );
        self.tracks
            .get(&uid)
            .map(|t| {
                t.contributors
                    .iter()
                    .filter(|c| c.source_track_key != DETECTIONS && at - c.last_report <= max_age)
                    .map(|c| c.source_id.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Compare a system track's new report with its neighbours from other
    /// sources; when one has agreed for M of the last N comparisons, merge
    /// the two (the newer into the older) with the evidence.
    async fn pair_kinematically(
        &mut self,
        uid: Uid,
        obs: &Observation,
        counts: &mut EngineCounts,
    ) -> anyhow::Result<()> {
        if self.settings.correlation.approach == Approach::Identifiers
            || !self.tracks.contains_key(&uid)
        {
            return Ok(());
        }
        let mut s = self.settings.correlation.kinematic;
        let near = self
            .grid
            .near(obs.position.latitude, obs.position.longitude);
        if s.local_density_radius_m > 0.0 {
            let r = s.local_density_radius_m;
            let count = near
                .iter()
                .filter(|o| **o != uid)
                .filter_map(|o| self.tracks.get(o))
                .filter(|t| {
                    correlate::distance_m(
                        obs.position.latitude,
                        obs.position.longitude,
                        t.view.position.latitude,
                        t.view.position.longitude,
                    ) <= r
                })
                .count();
            let local = count as f64 / (std::f64::consts::PI * (r / 1000.0).powi(2));
            s.object_density_per_km2 = s.object_density_per_km2.max(local);
        }
        let mine: HashSet<String> = self
            .track_sources(uid, obs.observed_at)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let max_age = chrono::Duration::milliseconds((s.max_age_secs * 1000.0) as i64);
        // Every system track nearby that could be the same object, with the
        // kinematic comparison (or a scorer plugin's evidence in its place).
        let mut compared: Vec<(Uid, correlate::Kinematic)> = Vec::new();
        for other in near {
            if other == uid {
                continue;
            }
            let Some(t) = self.tracks.get(&other) else {
                continue;
            };
            let skip = if (obs.observed_at - t.view.observed_at).abs() > max_age {
                Some("view too old")
            } else if t.state == TrackState::Lost {
                Some("lost")
            } else if self.forbidden(uid, other) {
                Some("do not pair")
            } else if self
                .track_sources(other, obs.observed_at)
                .iter()
                .any(|x| mine.contains(*x))
            {
                Some("same source")
            } else if self.settings.correlation.approach == Approach::KinematicsMetadata
                && correlate::veto(obs, &t.view).is_some()
            {
                Some("veto")
            } else {
                None
            };
            if let Some(why) = skip {
                if self.recording {
                    self.trace.push(json!({
                        "t": obs.observed_at, "kind": "compare",
                        "key": format!("{}/{}", obs.source_id, obs.source_track_key),
                        "a": uid.doc_id(), "b": other.doc_id(), "skip": why,
                    }));
                }
                continue;
            }
            compared.push((other, correlate::kinematic(obs, &t.view, &s)));
        }
        let plugin_evidence = self.score_with_plugin(obs, &mut compared);
        let mut ready: Option<(Uid, correlate::Kinematic, f64, usize)> = None;
        for (other, k) in compared {
            let Some(t) = self.tracks.get(&other) else {
                continue;
            };
            let key = pair_key(uid, other);
            let trace = |p: Option<f64>, why: Option<&str>| {
                json!({
                    "t": obs.observed_at, "kind": "compare",
                    "key": format!("{}/{}", obs.source_id, obs.source_track_key),
                    "a": uid.doc_id(), "b": other.doc_id(), "skip": why,
                    "d": k.distance_m, "sigma": k.sigma_m, "d2": k.d2, "dof": k.dof,
                    "ln_lr": k.ln_lr, "pass": k.pass, "p": p,
                })
            };
            if !k.pass && !self.candidates.contains_key(&key) {
                if self.recording {
                    self.trace.push(trace(None, Some("outside the gate")));
                }
                continue;
            }
            // Reusing the view: weigh by the report's share of the uncertainty.
            let reuse = {
                let (r, v) = (k.sigma_report_m.powi(2), k.sigma_view_m.powi(2));
                (s.reuse_views && r + v > 0.0).then(|| r / (r + v))
            };
            let Some(tally) = self.candidates.entry(key).or_default().record_reusing(
                (&uid.to_string(), obs.observed_at),
                (&other.to_string(), t.view.observed_at),
                k.ln_lr,
                !k.pass,
                (s.n, &s),
                reuse,
            ) else {
                if self.recording {
                    self.trace
                        .push(trace(None, Some("too soon after the last comparison")));
                }
                continue;
            };
            let (p, of) = (
                correlate::posterior(s.prior_probability, tally.ln_lr),
                tally.comparisons,
            );
            if self.recording {
                self.trace.push(trace(Some(p), None));
            }
            if of >= s.m && p >= s.pair_probability && ready.as_ref().is_none_or(|r| p > r.2) {
                ready = Some((other, k, p, of));
            }
        }
        // Forget pairs that stopped being compared.
        let window = chrono::Duration::milliseconds((s.window_secs * 1000.0) as i64);
        self.candidates
            .retain(|_, p| p.last().is_some_and(|l| obs.observed_at - l <= window));
        let Some((other, k, p, of)) = ready else {
            return Ok(());
        };
        let (from, into) = self.merge_order(uid, other);
        let trackers: Vec<&str> = [from, into]
            .iter()
            .flat_map(|u| self.tracks[u].contributors.iter())
            .filter_map(|c| self.latest.get(&contributor_key(c)))
            .filter_map(|o| o.provenance.tracker.as_deref())
            .collect();
        let evidence = json!({
            "rule": "kinematic",
            "trackers": trackers,
            "approach": self.settings.correlation.approach,
            "probability": p, "comparisons": of,
            "pair_probability": s.pair_probability,
            "last": k,
            "scorer": plugin_evidence.as_ref().map(|(plugin, by)| json!({
                "plugin": plugin, "evidence": by.get(&other).cloned().unwrap_or(Value::Null),
            })),
        });
        let reason = format!(
            "{} and {} agreed kinematically: probability {p:.3} of the same object over {of} comparisons ({})",
            from.doc_id(),
            into.doc_id(),
            describe(&k)
        );
        if self.settings.correlation.mode == Mode::Suggest {
            if self
                .suggest("pair", into, Some(from), None, evidence, reason)
                .await?
            {
                counts.suggested += 1;
            }
            return Ok(());
        }
        let decision = engine_decision("merge").reason(reason).evidence(evidence);
        let c = self.common.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            Ok(c.open_db()?.merge_system_tracks(from, into, decision)?)
        })
        .await??;
        let moved = self.keys_of(from);
        self.absorb(from, into).await?;
        // The source tracks that joined are paired at this probability.
        if let Some(t) = self.tracks.get_mut(&into) {
            for c in t
                .contributors
                .iter_mut()
                .filter(|c| moved.contains(&contributor_key(c)))
            {
                c.confidence = (p * 1e4).round() / 1e4;
            }
        }
        counts.kinematic_merged += 1;
        self.republish(into).await?;
        Ok(())
    }

    /// Recompute a track's view from its contributors and publish it.
    async fn republish(&mut self, uid: Uid) -> anyhow::Result<()> {
        self.load_missing(uid).await?;
        let best = self.best_of(uid);
        let confirm = self.confirm_for(uid);
        let Some(t) = self.tracks.get_mut(&uid) else {
            return Ok(());
        };
        if let Some((view, provenance)) = best {
            t.provenance = provenance;
            apply_view(t, view, confirm);
        }
        resolve(&self.attrs, t);
        self.index_track(uid);
        self.save(uid, true).await
    }

    /// Associate one scan of a detection source with system tracks: every
    /// plot/track pair inside the gate, taken nearest first, one plot per
    /// track. Associated plots update their track as that source's report.
    async fn associate(
        &mut self,
        scan: Vec<Observation>,
        counts: &mut EngineCounts,
    ) -> anyhow::Result<()> {
        let s = self.settings.correlation.kinematic;
        let max_age = chrono::Duration::milliseconds((s.max_age_secs * 1000.0) as i64);
        let mut pairs = Vec::new();
        for (i, det) in scan.iter().enumerate() {
            for uid in self
                .grid
                .near(det.position.latitude, det.position.longitude)
            {
                let Some(t) = self.tracks.get(&uid) else {
                    continue;
                };
                // Only tracks another source keeps alive: plots do not hold a track up alone.
                if (det.observed_at - t.view.observed_at).abs() > max_age
                    || t.state == TrackState::Lost
                    || self.track_sources(uid, det.observed_at).is_empty()
                {
                    continue;
                }
                let k = correlate::kinematic(det, &t.view, &s);
                if k.pass {
                    pairs.push((k.d2, i, uid));
                }
            }
        }
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut taken: HashMap<usize, Uid> = HashMap::new();
        let mut used = HashSet::new();
        for (_, i, uid) in pairs {
            if !taken.contains_key(&i) && used.insert(uid) {
                taken.insert(i, uid);
            }
        }
        for (i, mut det) in scan.into_iter().enumerate() {
            let Some(&uid) = taken.get(&i) else {
                counts.unassociated += 1;
                if self.recording {
                    self.trace.push(json!({
                        "t": det.observed_at, "kind": "detection", "source": det.source_id,
                        "key": det.source_track_key, "lat": det.position.latitude,
                        "lon": det.position.longitude, "uid": null,
                    }));
                }
                if self.recording {
                    self.associations
                        .push((format!("{}/{}", det.source_id, det.source_track_key), None));
                }
                continue;
            };
            if self.recording {
                self.associations.push((
                    format!("{}/{}", det.source_id, det.source_track_key),
                    Some(uid),
                ));
            }
            if self.recording {
                self.trace.push(json!({
                    "t": det.observed_at, "kind": "detection", "source": det.source_id,
                    "key": det.source_track_key, "lat": det.position.latitude,
                    "lon": det.position.longitude, "uid": uid.doc_id(),
                }));
            }
            det.source_track_key = DETECTIONS.into();
            if let Some(t) = self.tracks.get_mut(&uid)
                && !t
                    .contributors
                    .iter()
                    .any(|c| c.source_id == det.source_id && c.source_track_key == DETECTIONS)
            {
                t.contributors.push(Contributor {
                    source_id: det.source_id.clone(),
                    source_track_key: DETECTIONS.into(),
                    pairing: PairingType::Auto,
                    confidence: 1.0,
                    last_report: det.observed_at,
                    existence: None,
                });
            }
            if let Some((_, urgent)) = self.observe(uid, det).await? {
                counts.associated += 1;
                self.save(uid, urgent).await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn reap(&mut self) -> anyhow::Result<()> {
        let now = self.now();
        let mut dropped = Vec::new();
        let mut went_lost = Vec::new();
        for track in self.tracks.values_mut() {
            match lifecycle(track, now, &self.settings) {
                Lifecycle::Keep => {}
                Lifecycle::Lost => {
                    track.state = TrackState::Lost;
                    resolve(&self.attrs, track);
                    went_lost.push(track.uid);
                }
                Lifecycle::Drop => dropped.push(track.uid),
            }
        }
        let lost = went_lost.len() as u64;
        for uid in went_lost {
            self.save(uid, true).await?;
        }
        let counts = EngineCounts {
            lost,
            dropped: dropped.len() as u64,
            ..Default::default()
        };
        self.redis
            .incr_metrics(crate::metrics::ENGINE, &counts.pairs())
            .await?;
        if dropped.is_empty() {
            return Ok(());
        }
        let c = self.common.clone();
        let to_retire = dropped.clone();
        let drop_after = self.settings.drop_after;
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut db = c.open_db()?;
            for uid in to_retire {
                let d = engine_decision("drop_system_track")
                    .reason(format!("no report for {}s", drop_after.as_secs()));
                match db.retire_system_track(uid, d) {
                    Ok(_) | Err(ot_store::StoreError::NotFound(_)) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(())
        })
        .await??;
        let reason = format!("no report for {}s", drop_after.as_secs());
        let hour = chrono::Duration::hours(1);
        self.suggested.retain(|_, t| now - *t < hour);
        self.split_rejected.retain(|_, t| now - *t < hour);
        for uid in dropped {
            let published = self.forget(uid);
            self.retire_in_redis(uid, published, &reason).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn obs(at: DateTime<Utc>, name: &str, domain: Domain) -> Observation {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1, "source_id": "s", "source_track_key": "k",
            "observed_at": at, "received_at": at, "name": name,
            "position": {"latitude": 1.0, "longitude": 2.0},
            "classification": {"domain": domain}
        }))
        .unwrap()
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap()
    }

    #[test]
    fn confirms_after_k_and_flags_significant_changes() {
        let uid: Uid = "OTK000000001".parse().unwrap();
        let mut t = SystemTrack::from_first_observation(uid, obs(t0(), "A", Domain::Surface));
        assert_eq!(t.state, TrackState::Tentative);
        let s = |secs| t0() + chrono::Duration::seconds(secs);
        assert_eq!(
            apply(&mut t, obs(s(10), "A", Domain::Surface), 3),
            Applied::Routine
        );
        assert_eq!(
            apply(&mut t, obs(s(20), "A", Domain::Surface), 3),
            Applied::Significant
        );
        assert_eq!(t.state, TrackState::Confirmed);
        assert_eq!(
            apply(&mut t, obs(s(30), "B", Domain::Surface), 3),
            Applied::Significant
        );
        assert_eq!(
            apply(&mut t, obs(s(5), "C", Domain::Surface), 3),
            Applied::OutOfOrder
        );
        assert_eq!(t.view.name.as_deref(), Some("B"));
        assert_eq!(t.observation_count, 5);
    }

    #[test]
    fn lifecycle_uses_domain_stale_times() {
        let s = EngineSettings::default();
        let uid: Uid = "OTK000000001".parse().unwrap();
        let air = SystemTrack::from_first_observation(uid, obs(t0(), "A", Domain::Air));
        let sea = SystemTrack::from_first_observation(uid, obs(t0(), "S", Domain::Surface));
        let at = |secs| t0() + chrono::Duration::seconds(secs);
        assert_eq!(lifecycle(&air, at(59), &s), Lifecycle::Keep);
        assert_eq!(lifecycle(&air, at(61), &s), Lifecycle::Lost);
        assert_eq!(lifecycle(&sea, at(61), &s), Lifecycle::Keep);
        assert_eq!(lifecycle(&sea, at(16 * 60), &s), Lifecycle::Lost);
        assert_eq!(lifecycle(&sea, at(6 * 3600), &s), Lifecycle::Drop);
        let mut lost = air.clone();
        lost.state = TrackState::Lost;
        assert_eq!(lifecycle(&lost, at(120), &s), Lifecycle::Keep);
    }

    #[test]
    fn entity_overrides_become_notices_and_stale_matches_link_no_entity() {
        let schema: ExtensionSchema =
            serde_json::from_value(serde_json::json!({"version": 2, "fields": [
                {"key": "destination", "type": "string"},
                {"key": "state", "type": "string", "builtin": "state"}
            ]}))
            .unwrap();
        let attrs = Attributes {
            version: 2,
            schema: Some(schema),
            ..Default::default()
        };
        let mut o = obs(t0(), "A", Domain::Surface);
        o.ext
            .insert("destination".into(), serde_json::json!("LONG BEACH"));
        o.ext.insert(
            "registry".into(),
            serde_json::json!({"entity_id": "ent-1", "applied": true, "corroborated": true,
                               "grade": "exact", "overrides": [
                {"field": "ext.destination", "reported": "SAN DIEGO", "entity": "LONG BEACH"}]}),
        );
        let uid: Uid = "OTK000000001".parse().unwrap();
        let mut t = SystemTrack::from_first_observation(uid, o.clone());
        assert!(resolve(&attrs, &mut t));
        assert_eq!(t.entity_id.as_deref(), Some("ent-1"));
        assert_eq!(
            Value::Object(t.attributes.clone()),
            serde_json::json!({"destination": "LONG BEACH", "state": "tentative"})
        );
        assert_eq!(t.notices.len(), 1);
        assert_eq!(t.notices[0].feed, "SAN DIEGO");
        // Nothing changed: no republish needed.
        assert!(!resolve(&attrs, &mut t));

        // A stale (uncorroborated) match links no entity.
        o.ext.insert(
            "registry".into(),
            serde_json::json!({"entity_id": "ent-1", "applied": false, "grade": "stale"}),
        );
        let mut stale = SystemTrack::from_first_observation(uid, o);
        resolve(&attrs, &mut stale);
        assert_eq!(stale.entity_id, None);
    }

    #[test]
    fn an_entity_pinned_to_a_track_designates_it_through_merges() {
        let pin = Pin {
            entity_id: "ent-radar".into(),
            fields: serde_json::from_value(serde_json::json!({
                "affiliation": "hostile", "name": "BOGEY 1", "threat": "high"
            }))
            .unwrap(),
        };
        let attrs = Attributes {
            pins: [("tms-OTK000000007".to_string(), pin)].into(),
            ..Default::default()
        };
        let mut o = obs(t0(), "A", Domain::Air);
        o.classification.affiliation = Some(ot_core::Affiliation::Unknown);
        // The pinned track was merged into this one: the pin follows it.
        let mut t = SystemTrack::from_first_observation("OTK000000001".parse().unwrap(), o);
        t.aliases.push("OTK000000007".parse().unwrap());
        assert!(resolve(&attrs, &mut t));
        assert_eq!(t.entity_id.as_deref(), Some("ent-radar"));
        assert_eq!(
            t.view.classification.affiliation,
            Some(ot_core::Affiliation::Hostile)
        );
        assert_eq!(t.view.name.as_deref(), Some("BOGEY 1"));
        assert_eq!(t.view.ext["threat"], "high");
        let ctx = ot_core::wire::PublishContext {
            node_id: "test".into(),
            version: "0".into(),
            correlation: String::new(),
        };
        let m = ot_core::wire::to_message(&t, &ctx, t0());
        assert_eq!(m.affiliation, "hostile");
        assert!(
            t.notices
                .iter()
                .any(|n| n.key == "classification.affiliation"
                    && n.feed == "unknown"
                    && n.source_id == "track manager")
        );
    }

    // --- Replay: scripted reports through a real engine (Redis + SQLite) ---

    /// An engine on a throwaway Redis namespace and database; None (test
    /// skipped) without OT_TEST_REDIS_URL.
    async fn engine(sources: &[&str]) -> Option<(Engine, tempfile::TempDir)> {
        engine_at("TST", sources).await
    }

    async fn engine_at(site: &str, sources: &[&str]) -> Option<(Engine, tempfile::TempDir)> {
        let url = std::env::var("OT_TEST_REDIS_URL").ok()?;
        let dir = tempfile::tempdir().unwrap();
        let common = Common {
            sqlite: dir.path().join("ot.db"),
            redis: url,
            redis_namespace: format!(
                "ot-engine-test-{site}-{}-{}",
                std::process::id(),
                Utc::now().timestamp_micros()
            ),
            site: ot_core::SiteCode::new(site).unwrap(),
            nats: crate::config::NatsArgs {
                url: "nats://127.0.0.1:9".into(),
                creds: None,
                token: None,
                user: None,
                password: None,
                stream: "TRACKS".into(),
                tracks_subject: "tracks".into(),
                max_age_hours: 24.0,
            },
            profiles_dir: "profiles/trackers".into(),
            obs_window_secs: 600,
            shared_db: Default::default(),
        };
        common.open_db().unwrap();
        let mut e = Engine::new(common, EngineSettings::default())
            .await
            .unwrap();
        // The synthetic feeds below report once a second with independent
        // noise, so each report is new evidence.
        e.settings.correlation.kinematic.min_interval_secs = 1.0;
        e.sources = sources.iter().map(|s| s.to_string()).collect();
        e.redis.ensure_obs_groups(&e.sources, GROUP).await.unwrap();
        Some((e, dir))
    }

    /// A report from `source`/`key` at `t0 + secs`, at (lat, lon).
    fn report(
        source: &str,
        key: &str,
        secs: i64,
        lat: f64,
        lon: f64,
        mmsi: Option<&str>,
    ) -> Observation {
        let at = t0() + chrono::Duration::seconds(secs);
        let ids: Vec<Value> = mmsi
            .map(|m| json!({"scheme": "mmsi", "value": m}))
            .into_iter()
            .collect();
        serde_json::from_value(json!({
            "schema_version": 1, "source_id": source, "source_track_key": key,
            "observed_at": at, "received_at": at, "identifiers": ids,
            "position": {"latitude": lat, "longitude": lon},
            "classification": {"domain": "surface"}
        }))
        .unwrap()
    }

    async fn feed(e: &mut Engine, reports: &[Observation]) {
        for r in reports {
            e.redis
                .append_observations(&r.source_id, std::slice::from_ref(r))
                .await
                .unwrap();
        }
        e.pump(false).await.unwrap();
    }

    fn track_of(e: &Engine, source: &str, key: &str) -> Uid {
        uid_of(e, &format!("{source}/{key}"))
    }

    /// The system track a source track reports for, or last reported for
    /// before its source ended it (followed through later merges).
    fn uid_of(e: &Engine, key: &str) -> Uid {
        if let Some(u) = e.reports.get(key) {
            return *u;
        }
        let mut uid = e.ended_on[key];
        while let Some(t) = e.tracks.values().find(|t| t.aliases.contains(&uid)) {
            uid = t.uid;
        }
        uid
    }

    #[tokio::test]
    async fn replay_identifier_pairing_and_non_pairing() {
        let Some((mut e, _dir)) = engine(&["ais", "tak", "radar"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        feed(
            &mut e,
            &[report("ais", "366", 0, 32.0, -117.0, Some("366"))],
        )
        .await;
        // TAK reports the same MMSI 1 km away 5 s later: the same ship.
        feed(
            &mut e,
            &[report("tak", "uid-1", 5, 32.009, -117.0, Some("366"))],
        )
        .await;
        let ship = track_of(&e, "ais", "366");
        assert_eq!(track_of(&e, "tak", "uid-1"), ship);
        assert_eq!(e.tracks[&ship].contributors.len(), 2);
        // Radar claims the same MMSI 300 km away at the same time: not the same ship.
        feed(
            &mut e,
            &[report("radar", "r1", 5, 34.7, -117.0, Some("366"))],
        )
        .await;
        assert_ne!(track_of(&e, "radar", "r1"), ship);
        // A second AIS key with the same MMSI: a feed's keys are distinct objects.
        feed(
            &mut e,
            &[report("ais", "999", 6, 32.0, -117.0, Some("366"))],
        )
        .await;
        assert_ne!(track_of(&e, "ais", "999"), ship);
        assert_eq!(e.tracks.len(), 3);

        // The pairing is a decision with its evidence, in the track's history.
        let c = e.common.clone();
        let edges =
            tokio::task::spawn_blocking(move || c.open_db().unwrap().explain(ship).unwrap())
                .await
                .unwrap();
        let pair = edges
            .iter()
            .find(|x| x.src_key == "tak/uid-1")
            .expect("tak reports for the ship");
        assert_eq!(pair.decision_op, "pair");
        // Which correlation version decided it.
        let decision_id = pair.decision_id;
        let c = e.common.clone();
        let decision = tokio::task::spawn_blocking(move || {
            c.open_db().unwrap().decision(decision_id).unwrap()
        })
        .await
        .unwrap()
        .expect("the pairing decision");
        assert_eq!(
            decision["evidence"]["correlation_version"],
            correlate::VERSION
        );
        assert_eq!(decision["evidence"]["identity"], "mmsi:366");
        assert_eq!(pair.attrs["evidence"]["identity"], "mmsi:366");
        assert!(pair.attrs["evidence"]["gate"]["pass"].as_bool().unwrap());
        e.redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn replay_late_identity_merges_tracks() {
        let Some((mut e, _dir)) = engine(&["ais", "tak"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        // AIS reports a ship, long enough to be confirmed and published.
        feed(
            &mut e,
            &[
                report("ais", "777", -4, 10.0, 20.0, Some("777")),
                report("ais", "777", -2, 10.0, 20.0, Some("777")),
                report("ais", "777", 0, 10.0, 20.0, Some("777")),
            ],
        )
        .await;
        // TAK sees something nearby with no identifier yet: its own track,
        // confirmed and published.
        feed(
            &mut e,
            &[
                report("tak", "u9", 2, 10.001, 20.0, None),
                report("tak", "u9", 4, 10.001, 20.0, None),
                report("tak", "u9", 6, 10.001, 20.0, None),
            ],
        )
        .await;
        let (older, newer) = (track_of(&e, "ais", "777"), track_of(&e, "tak", "u9"));
        assert_ne!(older, newer);
        // Its next report carries the MMSI: the two merge, into the older track.
        feed(
            &mut e,
            &[report("tak", "u9", 10, 10.002, 20.0, Some("777"))],
        )
        .await;
        assert_eq!(track_of(&e, "tak", "u9"), older);
        assert!(!e.tracks.contains_key(&newer));
        assert_eq!(e.tracks[&older].aliases, vec![newer]);
        // The merged-away track is tombstoned for consumers.
        e.redis.ensure_outbox_group("check").await.unwrap();
        let entries = e
            .redis
            .read_outbox("check", "c", 1000, Duration::from_millis(10), true)
            .await
            .unwrap();
        let entries = if entries.is_empty() {
            e.redis
                .read_outbox("check", "c", 1000, Duration::from_millis(10), false)
                .await
                .unwrap()
        } else {
            entries
        };
        assert!(
            entries.iter().any(
                |x| matches!(&x.op, ot_store::OutboxOp::Tombstone { uid, reason }
                if *uid == newer && reason.as_deref().is_some_and(|r| r.starts_with("merged into")))
            ),
            "{entries:?}"
        );
        let c = e.common.clone();
        let edges =
            tokio::task::spawn_blocking(move || c.open_db().unwrap().explain(older).unwrap())
                .await
                .unwrap();
        assert!(
            edges
                .iter()
                .any(|x| x.kind.as_str() == "MERGED_INTO" && x.decision_op == "merge")
        );
        e.redis.purge_namespace().await.unwrap();
    }

    /// Outbox entries written since the last call (group "check").
    async fn outbox(e: &Engine) -> Vec<ot_store::OutboxEntry> {
        e.redis
            .read_outbox("check", "c", 1000, Duration::from_millis(10), false)
            .await
            .unwrap()
    }

    fn published(entries: &[ot_store::OutboxEntry], who: Uid) -> bool {
        entries
            .iter()
            .any(|x| matches!(&x.op, ot_store::OutboxOp::Publish { uid, .. } if *uid == who))
    }

    fn tombstoned(entries: &[ot_store::OutboxEntry], who: Uid) -> bool {
        entries
            .iter()
            .any(|x| matches!(&x.op, ot_store::OutboxOp::Tombstone { uid, .. } if *uid == who))
    }

    #[tokio::test]
    async fn the_output_filter_holds_back_and_withdraws_tracks() {
        let Some((mut e, _dir)) = engine(&["ais"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        e.redis.ensure_outbox_group("check").await.unwrap();
        e.settings.correlation.output = serde_json::from_value(json!({
            "areas": [{"name": "harbour", "min_lat": 31.5, "min_lon": -117.5, "max_lat": 32.5, "max_lon": -116.5}]
        }))
        .unwrap();
        // Outside the harbour: kept, not published, and it says why.
        for s in 0..3 {
            feed(
                &mut e,
                &[report("ais", "367", s, 33.0, -117.0, Some("367"))],
            )
            .await;
        }
        let out = track_of(&e, "ais", "367");
        assert_eq!(e.tracks[&out].published, Some(false));
        assert_eq!(
            e.tracks[&out].filtered.as_deref(),
            Some("outside every included area")
        );
        assert!(!published(&outbox(&e).await, out));
        // Inside: published; then it leaves and is withdrawn downstream.
        for s in 0..3 {
            feed(
                &mut e,
                &[report("ais", "366", s, 32.0, -117.0, Some("366"))],
            )
            .await;
        }
        let ship = track_of(&e, "ais", "366");
        assert_eq!(e.tracks[&ship].published, Some(true));
        feed(
            &mut e,
            &[report("ais", "366", 5, 33.0, -117.0, Some("366"))],
        )
        .await;
        assert_eq!(e.tracks[&ship].published, Some(false));
        assert!(tombstoned(&outbox(&e).await, ship));
        // Back in: published again.
        feed(
            &mut e,
            &[report("ais", "366", 9, 32.0, -117.0, Some("366"))],
        )
        .await;
        assert_eq!(e.tracks[&ship].published, Some(true));
        assert_eq!(e.tracks[&ship].filtered, None);
    }

    #[tokio::test]
    async fn purge_retires_every_track_and_its_history() {
        let Some((mut e, _dir)) = engine(&["ais"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        for s in 0..3 {
            feed(
                &mut e,
                &[report("ais", "366", s, 32.0, -117.0, Some("366"))],
            )
            .await;
            feed(
                &mut e,
                &[report("ais", "367", s, 33.0, -117.0, Some("367"))],
            )
            .await;
        }
        let ship = track_of(&e, "ais", "366");
        assert_eq!(e.redis.list_system_tracks().await.unwrap().len(), 2);
        let r = e
            .command(&json!({"op": "purge", "history": true, "actor": "op:test"}))
            .await
            .unwrap();
        assert_eq!(r["retired"], 2);
        assert!(e.tracks.is_empty());
        assert!(e.redis.list_system_tracks().await.unwrap().is_empty());
        let c = e.common.clone();
        let edges =
            tokio::task::spawn_blocking(move || c.open_db().unwrap().explain(ship).unwrap())
                .await
                .unwrap();
        assert!(edges.is_empty(), "history purged");
        // The feed goes on: a new track, with a new UID.
        feed(
            &mut e,
            &[report("ais", "366", 10, 32.0, -117.0, Some("366"))],
        )
        .await;
        assert_ne!(track_of(&e, "ais", "366"), ship);
    }

    #[tokio::test]
    async fn replay_only_authoritative_tracks_are_published() {
        let Some((mut e, _dir)) = engine(&["ais", "radar", "lidar"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        // Radar and lidar tracks may not stand alone (a detection source's default).
        e.alone = [("radar".to_string(), false), ("lidar".to_string(), false)].into();
        e.redis.ensure_outbox_group("check").await.unwrap();
        let ended = |mut o: Observation| {
            o.state = Some(TrackState::Dropped);
            o
        };

        // A lone radar track: confirmed, but kept inside OpenTrack.
        let radar: Vec<_> = (0..5)
            .map(|s| report("radar", "r1", s * 2, 32.0, -117.0, None))
            .collect();
        feed(&mut e, &radar).await;
        let ship = track_of(&e, "radar", "r1");
        assert_eq!(e.tracks[&ship].state, TrackState::Confirmed);
        assert_eq!(e.tracks[&ship].published, Some(false));
        assert!(!published(&outbox(&e).await, ship));

        // AIS reports the same ship: published on its own once confirmed, then
        // kinematic agreement merges the radar's internal track into it (the
        // published track survives, so consumers keep its id).
        let ais = |s: i64| {
            let mut o = report("ais", "366", s, 32.00001, -117.0, Some("366"));
            o.name = Some("TED STEVENS".into());
            o
        };
        // Both report: each comparison needs a new report from each side.
        for s in 10..13 {
            feed(&mut e, &[ais(s)]).await;
            feed(&mut e, &[report("radar", "r1", s, 32.0, -117.0, None)]).await;
        }
        assert_eq!(e.tracks[&track_of(&e, "ais", "366")].published, Some(true));
        feed(&mut e, &[report("radar", "r1", 13, 32.0, -117.0, None)]).await;
        let internal = ship;
        let ship = track_of(&e, "ais", "366");
        assert_eq!(track_of(&e, "radar", "r1"), ship);
        assert_ne!(ship, internal);
        assert_eq!(e.tracks[&ship].published, Some(true));
        let entries = outbox(&e).await;
        assert!(published(&entries, ship));
        assert!(
            !tombstoned(&entries, internal),
            "never published, so no tombstone"
        );

        // AIS ends (a transponder switched off) while the radar holds the ship:
        // the track stays published and keeps its identity.
        feed(&mut e, &[ended(ais(20))]).await;
        feed(&mut e, &[report("radar", "r1", 21, 32.0, -117.0, None)]).await;
        assert_eq!(e.tracks[&ship].contributors.len(), 1);
        assert_eq!(e.tracks[&ship].view.name.as_deref(), Some("TED STEVENS"));
        assert_eq!(e.tracks[&ship].view.identifiers.len(), 1);
        assert!(published(&outbox(&e).await, ship));
        // The radar ends too: nothing reports for it, so it is retired and tombstoned.
        feed(
            &mut e,
            &[ended(report("radar", "r1", 22, 32.0, -117.0, None))],
        )
        .await;
        assert!(!e.tracks.contains_key(&ship));
        assert!(tombstoned(&outbox(&e).await, ship));

        // Radar and lidar agreeing on something no track feed reports: paired,
        // but still not published.
        for s in 0..6 {
            feed(
                &mut e,
                &[
                    report("radar", "r3", 40 + s * 2, 34.0, -117.0, None),
                    report("lidar", "l3", 40 + s * 2, 34.00001, -117.0, None),
                ],
            )
            .await;
        }
        let dark = track_of(&e, "radar", "r3");
        assert_eq!(track_of(&e, "lidar", "l3"), dark);
        assert_eq!(e.tracks[&dark].state, TrackState::Confirmed);
        assert_eq!(e.tracks[&dark].published, Some(false));
        assert!(!published(&outbox(&e).await, dark));

        // A lone radar track that ends before anyone saw it is simply forgotten.
        let clutter: Vec<_> = (0..4)
            .map(|s| report("radar", "r2", 30 + s, 33.0, -117.0, None))
            .collect();
        feed(&mut e, &clutter).await;
        let ghost = track_of(&e, "radar", "r2");
        feed(
            &mut e,
            &[ended(report("radar", "r2", 35, 33.0, -117.0, None))],
        )
        .await;
        assert!(!e.tracks.contains_key(&ghost));
        let entries = outbox(&e).await;
        assert!(!published(&entries, ghost) && !tombstoned(&entries, ghost));
        assert!(e.redis.get_system_track(ghost).await.unwrap().is_none());
        // Its lineage records why it ended.
        let c = e.common.clone();
        let edges =
            tokio::task::spawn_blocking(move || c.open_db().unwrap().explain(ghost).unwrap())
                .await
                .unwrap();
        assert!(edges.iter().all(|x| x.valid_to_ms.is_some()), "{edges:?}");
        e.redis.purge_namespace().await.unwrap();
    }

    /// Queue an operator command and run it; the engine's answer.
    #[tokio::test]
    async fn track_feeds_confirm_on_one_report_and_entities_override_publishing() {
        let Some((mut e, _dir)) = engine(&["ais", "radar"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        e.confirm = [("ais".to_string(), 1)].into();
        feed(&mut e, &[report("ais", "1", 0, 32.0, -117.0, Some("1"))]).await;
        let t = track_of(&e, "ais", "1");
        assert_eq!(e.tracks[&t].state, TrackState::Confirmed);
        assert!(e.tracks[&t].is_published());

        // Its entity says never: withdrawn. Back to automatic: published again.
        e.attrs.pins.insert(
            t.doc_id(),
            Pin {
                entity_id: "ent-1".into(),
                fields: Map::new(),
            },
        );
        e.attrs.publish.insert("ent-1".into(), false);
        feed(&mut e, &[report("ais", "1", 40, 32.0, -117.0, Some("1"))]).await;
        assert!(!e.tracks[&t].is_published());
        assert!(e.tracks[&t].filtered.as_deref().unwrap().contains("never"));
        e.attrs.publish.clear();
        feed(&mut e, &[report("ais", "1", 80, 32.0, -117.0, Some("1"))]).await;
        assert!(e.tracks[&t].is_published());

        // A detection-like source at the default needs three reports; its
        // entity set to always publishes it at once.
        feed(&mut e, &[report("radar", "r", 0, 33.0, -117.0, None)]).await;
        let r = track_of(&e, "radar", "r");
        assert_eq!(e.tracks[&r].state, TrackState::Tentative);
        e.attrs.pins.insert(
            r.doc_id(),
            Pin {
                entity_id: "ent-2".into(),
                fields: Map::new(),
            },
        );
        e.attrs.publish.insert("ent-2".into(), true);
        feed(&mut e, &[report("radar", "r", 5, 33.0, -117.0, None)]).await;
        assert_eq!(e.tracks[&r].state, TrackState::Tentative);
        assert!(e.tracks[&r].is_published());
        e.redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn a_saved_entity_reaches_its_tracks_without_a_new_report() {
        let Some((mut e, _dir)) = engine(&["ais"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        let stage: ot_source::registry::RegistryStage =
            serde_json::from_value(json!({"apply_grades": ["exact", "stale"]})).unwrap();
        e.stages.insert("ais".into(), stage);
        let entry = |name: &str| ot_source::registry::RegistryEntry {
            entity_id: "ent-omaha".into(),
            name: Some(name.into()),
            expected_name: None,
            fields: serde_json::from_value(json!({"class_name": "INDEPENDENCE"})).unwrap(),
        };
        e.attrs.lookup = [(("mmsi".to_string(), "1".to_string()), entry("USS OMAHA"))].into();
        let mut r = named(
            report("ais", "1", 0, 32.0, -117.0, Some("1")),
            "US GOV VESSEL",
        );
        r.ext.insert(
            "registry".into(),
            json!({"entity_id": "ent-omaha", "corroborated": true, "applied": true, "grade": "stale"}),
        );
        feed(&mut e, &[r]).await;
        let t = track_of(&e, "ais", "1");
        assert_eq!(e.tracks[&t].view.name.as_deref(), Some("US GOV VESSEL"));

        // The entity is saved: its name reaches the track at once.
        assert_eq!(e.reapply_entities().await.unwrap(), 1);
        let v = &e.tracks[&t].view;
        assert_eq!(v.name.as_deref(), Some("USS OMAHA"));
        assert_eq!(v.platform.class.as_deref(), Some("INDEPENDENCE"));
        // Renamed again: the feed's own name is put back before re-applying,
        // so the recorded difference is still against what the feed reported.
        e.attrs.lookup = [(
            ("mmsi".to_string(), "1".to_string()),
            entry("OMAHA (LCS-12)"),
        )]
        .into();
        e.reapply_entities().await.unwrap();
        let v = &e.tracks[&t].view;
        assert_eq!(v.name.as_deref(), Some("OMAHA (LCS-12)"));
        assert_eq!(
            v.ext["registry"]["overrides"][0]["reported"],
            "US GOV VESSEL"
        );
        e.redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn track_management_pairs_groups_merges_and_deletes() {
        let Some((mut e, _dir)) = engine(&["ais"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        // Four ships, far enough apart that correlation leaves them alone.
        for s in 0..3 {
            feed(
                &mut e,
                &[
                    report("ais", "1", s, 32.0, -117.0, Some("1")),
                    report("ais", "2", s, 32.05, -117.0, Some("2")),
                    report("ais", "3", s, 32.10, -117.0, Some("3")),
                    report("ais", "4", s, 32.15, -117.0, Some("4")),
                ],
            )
            .await;
        }
        let t: Vec<Uid> = (1..=4)
            .map(|i| track_of(&e, "ais", &i.to_string()))
            .collect();
        let id = |u: Uid| u.doc_id();

        let a = operator(
            &mut e,
            json!({"op": "pair", "tracks": [id(t[0]), id(t[1])]}),
        )
        .await;
        assert_eq!(a["ok"], true, "{a}");
        assert_eq!(e.tracks[&t[0]].paired_with, [t[1]]);

        // A battle group of three: published at their centre, with its members.
        let a = operator(&mut e, json!({"op": "group_create", "members": [id(t[0]), id(t[1]), id(t[2])],
            "spec": {"name": "CSG 12", "sidc": "SHSPGG----", "affiliation": "hostile", "echelon": "G"}}))
        .await;
        assert_eq!(a["ok"], true, "{a}");
        let g: Uid = a["result"]["group"]
            .as_str()
            .unwrap()
            .trim_start_matches("tms-")
            .parse()
            .unwrap();
        let group = e
            .redis
            .get_system_track(g)
            .await
            .unwrap()
            .expect("the group is stored");
        assert_eq!(group.kind, ot_core::TrackKind::Group);
        assert_eq!(group.members, &t[..3]);
        assert!((group.view.position.latitude - 32.05).abs() < 1e-3);
        assert_eq!(
            ot_core::wire::to_message(&group, &e.common.publish_context(), Utc::now()).affiliation,
            "hostile"
        );
        assert_eq!(e.tracks[&t[2]].groups, [id(g)]);

        // A track manager's merge holds, and carries the pairing and group.
        let a = operator(
            &mut e,
            json!({"op": "merge", "from": id(t[1]), "into": id(t[3]), "hold": true}),
        )
        .await;
        assert_eq!(a["ok"], true, "{a}");
        let into = &e.tracks[&t[3]];
        assert!(
            into.contributors
                .iter()
                .all(|c| c.pairing == PairingType::Manual)
        );
        assert_eq!(into.paired_with, [t[0]]);
        assert_eq!(e.tracks[&t[0]].paired_with, [t[3]]);
        assert!(e.groups[&g].track.members.contains(&t[3]));

        // Delete a member; then dissolve the group by deleting it.
        let a = operator(&mut e, json!({"op": "delete", "tracks": [id(t[2])]})).await;
        assert_eq!(a["ok"], true, "{a}");
        assert!(!e.tracks.contains_key(&t[2]));
        assert!(!e.groups[&g].track.members.contains(&t[2]));
        let a = operator(&mut e, json!({"op": "delete", "tracks": [id(g)]})).await;
        assert_eq!(a["ok"], true, "{a}");
        assert!(e.groups.is_empty());
        assert!(e.tracks[&t[0]].groups.is_empty());
        assert!(e.redis.get_system_track(g).await.unwrap().is_none());
        let a = operator(
            &mut e,
            json!({"op": "pair", "tracks": [id(t[0]), "tms-OTK999999999"]}),
        )
        .await;
        assert_eq!(a["ok"], false);
        e.redis.purge_namespace().await.unwrap();
    }

    async fn operator(e: &mut Engine, mut cmd: Value) -> Value {
        let id = format!("t{}", Utc::now().timestamp_micros());
        cmd["id"] = json!(id);
        e.redis.push_command(&cmd).await.unwrap();
        e.process_commands().await.unwrap();
        e.redis
            .command_result(&id)
            .await
            .unwrap()
            .expect("an answer")
    }

    async fn suggestions(e: &Engine, status: &str) -> Vec<ot_store::Suggestion> {
        let c = e.common.clone();
        let status = status.to_owned();
        tokio::task::spawn_blocking(move || {
            c.open_db()
                .unwrap()
                .suggestions(Some(&status), 100)
                .unwrap()
        })
        .await
        .unwrap()
    }

    fn named(mut o: Observation, name: &str) -> Observation {
        o.name = Some(name.into());
        o
    }

    #[tokio::test]
    async fn replay_divergence_proposes_a_split_and_an_operator_accepts_it() {
        let Some((mut e, _dir)) = engine(&["ais", "radar"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        // Proposed, not split: an operator decides.
        e.settings.correlation.split.automatic = false;
        e.alone = [("radar".to_string(), false)].into();
        // AIS and radar on the same ship: paired.
        for s in 0..6 {
            feed(
                &mut e,
                &[
                    named(
                        report("ais", "366", s, 32.0, -117.0, Some("366")),
                        "TED STEVENS",
                    ),
                    report("radar", "r1", s, 32.00001, -117.0, None),
                ],
            )
            .await;
        }
        let ship = track_of(&e, "ais", "366");
        assert_eq!(track_of(&e, "radar", "r1"), ship);
        // The radar track wanders onto another boat 450 m north.
        for s in 6..14 {
            feed(
                &mut e,
                &[
                    named(
                        report("ais", "366", s, 32.0, -117.0, Some("366")),
                        "TED STEVENS",
                    ),
                    report("radar", "r1", s, 32.004, -117.0, None),
                ],
            )
            .await;
        }
        // Proposed, not done: the radar (which may not stand alone) is the one to leave.
        assert_eq!(track_of(&e, "radar", "r1"), ship);
        let open = suggestions(&e, "open").await;
        assert_eq!(open.len(), 1, "{open:?}");
        assert_eq!(open[0].kind, "split");
        assert_eq!(open[0].source_track.as_deref(), Some("radar/r1"));
        assert_eq!(open[0].evidence["rule"], "divergence");

        let answer = operator(&mut e, json!({"op": "accept", "suggestion": open[0].id})).await;
        assert_eq!(answer["ok"], true, "{answer}");
        let radar = track_of(&e, "radar", "r1");
        assert_ne!(radar, ship);
        assert_eq!(answer["result"]["new_track"], radar.doc_id());
        assert_eq!(e.tracks[&ship].contributors.len(), 1);
        assert_eq!(e.tracks[&ship].view.name.as_deref(), Some("TED STEVENS"));
        assert_eq!(
            e.tracks[&radar].view.name, None,
            "the radar track carries no identity"
        );
        assert!(suggestions(&e, "open").await.is_empty());
        // Even back alongside, the two are not paired again.
        for s in 14..22 {
            feed(
                &mut e,
                &[
                    report("ais", "366", s, 32.0, -117.0, Some("366")),
                    report("radar", "r1", s, 32.00001, -117.0, None),
                ],
            )
            .await;
        }
        assert_ne!(track_of(&e, "radar", "r1"), track_of(&e, "ais", "366"));
        let c = e.common.clone();
        let log = tokio::task::spawn_blocking(move || {
            c.open_db()
                .unwrap()
                .decisions_by_op(ot_store::correlation::CORRELATION_OPS, 50)
                .unwrap()
        })
        .await
        .unwrap();
        let split = log
            .iter()
            .find(|d| d.op == "split")
            .expect("a split decision");
        assert_eq!(split.actor, "operator");
        assert_eq!(split.evidence["source_track"], "radar/r1");
        assert!(log.iter().any(|d| d.op == "do_not_pair"));
        e.redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn replay_automatic_split() {
        let Some((mut e, _dir)) = engine(&["ais", "radar"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        e.alone = [("radar".to_string(), false)].into();
        e.settings.correlation.split.automatic = true;
        for s in 0..6 {
            feed(
                &mut e,
                &[
                    report("ais", "366", s, 32.0, -117.0, Some("366")),
                    report("radar", "r1", s, 32.00001, -117.0, None),
                ],
            )
            .await;
        }
        let ship = track_of(&e, "ais", "366");
        assert_eq!(track_of(&e, "radar", "r1"), ship);
        for s in 6..14 {
            feed(
                &mut e,
                &[
                    report("ais", "366", s, 32.0, -117.0, Some("366")),
                    report("radar", "r1", s, 32.004, -117.0, None),
                ],
            )
            .await;
        }
        assert_ne!(track_of(&e, "radar", "r1"), ship);
        assert!(suggestions(&e, "open").await.is_empty());
        e.redis.purge_namespace().await.unwrap();
    }

    /// A scorer plugin for tests: OpenTrack's kinematic evidence, or (with
    /// `"veto": true`) a veto of every pair.
    struct TestScorer {
        manifest: ot_source::plugin::Manifest,
    }

    impl ot_source::plugin::Plugin for TestScorer {
        fn manifest(&self) -> &ot_source::plugin::Manifest {
            &self.manifest
        }
        fn scorer(
            &self,
            options: &Value,
        ) -> Result<Box<dyn ot_source::plugin::PluginScorer>, String> {
            Ok(Box::new(TestScorerRun(options["veto"] == true)))
        }
    }

    struct TestScorerRun(bool);

    impl ot_source::plugin::PluginScorer for TestScorerRun {
        fn score(
            &mut self,
            _: &Observation,
            candidates: &[ot_source::plugin::ScoreCandidate],
        ) -> Result<Vec<ot_source::plugin::Score>, String> {
            Ok(candidates
                .iter()
                .map(|c| ot_source::plugin::Score {
                    ln_lr: if self.0 {
                        -20.0
                    } else {
                        c.kinematic["ln_lr"].as_f64().unwrap()
                    },
                    pass: !self.0 && c.kinematic["pass"] == true,
                    evidence: json!({ "veto": self.0 }),
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn a_scorer_plugin_decides_the_pairing_evidence() {
        let Some((mut e, _dir)) = engine(&["ais", "radar"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        ot_source::plugin::install(std::sync::Arc::new(TestScorer {
            manifest: serde_json::from_value(json!({
                "name": "test-scorer", "version": "7", "kinds": ["scorer"]
            }))
            .unwrap(),
        }))
        .unwrap();
        let scorer = |veto: bool| {
            Some(correlate::ScorerRef {
                plugin: "test-scorer".into(),
                options: json!({ "veto": veto }),
            })
        };
        let pair = |e: &Engine| track_of(e, "ais", "367") == track_of(e, "radar", "r2");
        let reports = |s: i64| {
            [
                report("ais", "367", s, 33.0, -117.0, Some("367")),
                report("radar", "r2", s, 33.00001, -117.0, None),
            ]
        };
        // The plugin vetoes: close as they are, they stay apart.
        e.settings.correlation.scorer = scorer(true);
        for s in 0..10 {
            feed(&mut e, &reports(s)).await;
        }
        assert!(!pair(&e), "vetoed by the scorer");
        // It passes the kinematic evidence on: they pair, and the decision
        // names the plugin and what it said.
        e.settings.correlation.scorer = scorer(false);
        for s in 10..20 {
            feed(&mut e, &reports(s)).await;
        }
        assert!(pair(&e), "paired on the scorer's evidence");
        let c = e.common.clone();
        let evidence: Vec<Value> = tokio::task::spawn_blocking(move || {
            let db = c.open_db().unwrap();
            let mut stmt = db
                .connection()
                .prepare("SELECT evidence FROM decisions WHERE op = 'merge'")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|v| serde_json::from_str(&v.unwrap()).unwrap())
                .collect()
        })
        .await
        .unwrap();
        assert_eq!(evidence.len(), 1, "{evidence:?}");
        assert_eq!(evidence[0]["scorer"]["plugin"], "test-scorer-7");
        assert_eq!(evidence[0]["scorer"]["evidence"]["veto"], false);
        ot_source::plugin::uninstall("test-scorer");
        e.redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn replay_suggest_mode_and_operator_decisions() {
        let Some((mut e, _dir)) = engine(&["ais", "radar", "tak"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        e.alone = [("radar".to_string(), false)].into();
        // Saved settings reach the running engine.
        let c = e.common.clone();
        tokio::task::spawn_blocking(move || {
            c.open_db()
                .unwrap()
                .save_correlation_settings(
                    &json!({"mode": "suggest", "kinematic": {"min_interval_secs": 1.0}}),
                    ot_store::Decision::new("operator", "correlation_settings"),
                )
                .unwrap()
        })
        .await
        .unwrap();
        e.refresh_correlation().await.unwrap();
        assert_eq!(e.settings.correlation.mode, Mode::Suggest);

        let pair = |e: &Engine, a: &str, b: &str| track_of(e, "ais", a) == track_of(e, "radar", b);
        // Agreement is proposed, not acted on.
        for s in 0..6 {
            feed(
                &mut e,
                &[
                    report("ais", "367", s, 33.0, -117.0, Some("367")),
                    report("radar", "r2", s, 33.00001, -117.0, None),
                    report("ais", "368", s, 34.0, -117.0, Some("368")),
                    report("radar", "r3", s, 34.00001, -117.0, None),
                ],
            )
            .await;
        }
        assert!(!pair(&e, "367", "r2") && !pair(&e, "368", "r3"));
        let open = suggestions(&e, "open").await;
        assert_eq!(open.len(), 2, "{open:?}");
        let about = |a: &str| {
            let uid = track_of(&e, "ais", a).to_string();
            open.iter()
                .find(|s| s.track_a == uid)
                .expect("a suggestion")
                .id
        };
        let (reject, accept) = (about("367"), about("368"));

        // Accepted: merged, into the published AIS track.
        let answer = operator(&mut e, json!({"op": "accept", "suggestion": accept})).await;
        assert_eq!(answer["ok"], true, "{answer}");
        assert!(pair(&e, "368", "r3"));
        // Rejected: never paired, and not proposed again.
        let answer = operator(&mut e, json!({"op": "reject", "suggestion": reject})).await;
        assert_eq!(answer["ok"], true, "{answer}");
        e.settings.correlation.mode = Mode::Automatic;
        for s in 6..14 {
            feed(
                &mut e,
                &[
                    report("ais", "367", s, 33.0, -117.0, Some("367")),
                    report("radar", "r2", s, 33.00001, -117.0, None),
                ],
            )
            .await;
        }
        assert!(!pair(&e, "367", "r2"));
        assert!(suggestions(&e, "open").await.is_empty());
        // A closed suggestion cannot be decided twice.
        let again = operator(&mut e, json!({"op": "accept", "suggestion": accept})).await;
        assert_eq!(again["ok"], false);

        // "Do not pair" holds even against a shared identifier.
        feed(&mut e, &[report("tak", "u1", 20, 35.0, -117.0, None)]).await;
        feed(
            &mut e,
            &[report("ais", "369", 20, 35.0, -117.0, Some("369"))],
        )
        .await;
        let (tak, ais) = (track_of(&e, "tak", "u1"), track_of(&e, "ais", "369"));
        let answer = operator(
            &mut e,
            json!({"op": "do_not_pair", "a": tak.doc_id(), "b": ais.doc_id()}),
        )
        .await;
        assert_eq!(answer["ok"], true, "{answer}");
        feed(
            &mut e,
            &[report("tak", "u1", 21, 35.0, -117.0, Some("369"))],
        )
        .await;
        assert_ne!(track_of(&e, "tak", "u1"), track_of(&e, "ais", "369"));

        // An operator can still merge by hand, and bad commands say why they failed.
        let (from, into) = (
            track_of(&e, "tak", "u1").doc_id(),
            track_of(&e, "ais", "369").doc_id(),
        );
        let answer = operator(&mut e, json!({"op": "merge", "from": from, "into": into})).await;
        assert_eq!(answer["ok"], true, "{answer}");
        assert_eq!(track_of(&e, "tak", "u1"), track_of(&e, "ais", "369"));
        let bad = operator(
            &mut e,
            json!({"op": "split", "track": "tms-TST000999999", "source_track": "x/y"}),
        )
        .await;
        assert_eq!(bad["ok"], false);
        assert!(
            bad["error"].as_str().unwrap().contains("not a live track"),
            "{bad}"
        );
        e.redis.purge_namespace().await.unwrap();
    }

    #[test]
    fn algorithm_versions_are_documented() {
        let doc = include_str!("../../../docs/algorithms.md");
        for v in [
            correlate::VERSION,
            ot_source::tracker::GNN_VERSION,
            ot_source::tracker::MHT_VERSION,
        ] {
            assert!(
                doc.contains(&format!("### {v} (")),
                "docs/algorithms.md has no changelog entry for {v}"
            );
        }
    }

    #[tokio::test]
    async fn replay_best_source_view() {
        let Some((mut e, _dir)) = engine(&["ais", "radar"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        let mut ais = report("ais", "366", 0, 32.0, -117.0, Some("366"));
        ais.name = Some("TED STEVENS".into());
        ais.uncertainty = Some(ot_core::Uncertainty {
            circular_error_m: Some(10.0),
            ..Default::default()
        });
        let mut radar = report("radar", "t7", 3, 32.001, -117.0, Some("366"));
        radar.uncertainty = Some(ot_core::Uncertainty {
            circular_error_m: Some(200.0),
            ..Default::default()
        });
        radar.classification.cot_type = Some("a-f-S-C-L-D-D".into());
        feed(&mut e, &[ais, radar]).await;
        let t = &e.tracks[&track_of(&e, "ais", "366")];
        assert_eq!(t.contributors.len(), 2);
        // Newer radar, but AIS is more precise: AIS supplies position.
        assert_eq!(t.provenance["position"], "ais/366");
        assert_eq!(t.view.position.latitude, 32.0);
        assert_eq!(t.provenance["classification"], "radar/t7");
        assert_eq!(t.view.name.as_deref(), Some("TED STEVENS"));
        e.redis.purge_namespace().await.unwrap();
    }

    // --- Autoferry: recorded radar and lidar against a track feed ---
    //
    // Fixtures from scripts/benchmark/replay/autoferry.py (Autoferry sensor fusion dataset,
    // NTNU, CC0). Each report carries the ground-truth target it is nearest
    // to (none for clutter), which scores where the engine put it.

    #[derive(serde::Deserialize)]
    struct Recorded {
        feed: String,
        truth: Option<u64>,
        obs: Observation,
    }

    fn recorded(name: &str) -> Vec<Recorded> {
        let path = format!(
            "{}/tests/data/autoferry/{name}.jsonl",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Feed the reports a second of recorded time at a time, as they would arrive.
    async fn replay(e: &mut Engine, rows: &[Recorded]) {
        let mut i = 0;
        while i < rows.len() {
            let until = rows[i].obs.observed_at + chrono::Duration::seconds(1);
            let j = rows[i..]
                .iter()
                .position(|r| r.obs.observed_at >= until)
                .map_or(rows.len(), |n| i + n);
            let chunk: Vec<Observation> = rows[i..j].iter().map(|r| r.obs.clone()).collect();
            for r in &chunk {
                e.redis
                    .append_observations(&r.source_id, std::slice::from_ref(r))
                    .await
                    .unwrap();
            }
            e.pump(false).await.unwrap();
            i = j;
        }
    }

    /// With OT_REPLAY_TRACE=<dir>, write the engine's trace of a replay there
    /// (scripts/benchmark/replay/replay-video.py renders it).
    fn write_trace(e: &Engine, name: &str) {
        let Ok(dir) = std::env::var("OT_REPLAY_TRACE") else {
            return;
        };
        let lines: Vec<String> = e.trace.iter().map(|v| v.to_string()).collect();
        std::fs::write(format!("{dir}/{name}.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    /// Where each labelled source track ended: on its target's track, on
    /// the other target's, or alone; and clutter tracks that ended on a
    /// target's track. A track reported while another track of the same
    /// sensor on the same target was live is a duplicate (a 30 m vessel can
    /// hold several lidar tracks): the sensor says two objects, so it must
    /// stay unpaired.
    async fn autoferry_tracks(scenario: u32) {
        let rows = recorded(&format!("scenario{scenario}-tracks"));
        score_tracks(scenario, &rows, &format!("scenario{scenario}-tracks")).await;
    }

    /// The detections through OpenTrack's own tracker stage (one per
    /// sensor, as each would be its own source), then into the engine.
    async fn autoferry_tracker(scenario: u32, algorithm: &str) {
        use ot_source::tracker::{Tracker, TrackerSpec};
        let rows = recorded(&format!("scenario{scenario}-detections"));
        let mut out: Vec<Recorded> = rows
            .iter()
            .filter(|r| r.feed == "track")
            .map(|r| Recorded {
                feed: r.feed.clone(),
                truth: r.truth,
                obs: r.obs.clone(),
            })
            .collect();
        for (feed, sigma) in [("lidar-det", 4.0), ("radar-det", 6.0)] {
            let spec: TrackerSpec = serde_json::from_value(json!({
                "algorithm": algorithm, "measurement_sigma_m": sigma, "domain": "surface",
                "cluster_m": if feed == "lidar-det" { 10.0 } else { 0.0 },
                "key_prefix": if feed == "lidar-det" { "L" } else { "R" },
                "mht": {"clutter_density": 4e-6}
            }))
            .unwrap();
            let mut t = Tracker::new(spec).unwrap();
            let truth: HashMap<String, Option<u64>> = rows
                .iter()
                .filter(|r| r.feed == feed)
                .map(|r| (r.obs.source_track_key.clone(), r.truth))
                .collect();
            let plots: Vec<&Recorded> = rows.iter().filter(|r| r.feed == feed).collect();
            let mut i = 0;
            while i < plots.len() {
                let when = plots[i].obs.observed_at;
                while i < plots.len() && plots[i].obs.observed_at == when {
                    t.push(plots[i].obs.clone(), when);
                    i += 1;
                }
                for (obs, det) in t.run(when, true) {
                    out.push(Recorded {
                        feed: feed.into(),
                        truth: truth[&det.source_track_key],
                        obs,
                    });
                }
            }
        }
        out.sort_by_key(|r| r.obs.observed_at);
        score_tracks(scenario, &out, &format!("scenario{scenario}-{algorithm}")).await;
    }

    async fn score_tracks(_scenario: u32, rows: &[Recorded], name: &str) {
        let sources: Vec<&str> = {
            let mut s: Vec<&str> = rows.iter().map(|r| r.obs.source_id.as_str()).collect();
            s.sort();
            s.dedup();
            s
        };
        let Some((mut e, _dir)) = engine(&sources).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        replay(&mut e, rows).await;
        write_trace(&e, name);
        let c = e.common.clone();
        let suggestions = tokio::task::spawn_blocking(move || {
            c.open_db().unwrap().suggestions(None, 1000).unwrap()
        })
        .await
        .unwrap();
        for sg in &suggestions {
            eprintln!(
                "{name}: suggestion {} {} {} {:?}: {}",
                sg.kind,
                sg.track_a,
                sg.track_b
                    .as_deref()
                    .or(sg.source_track.as_deref())
                    .unwrap_or(""),
                sg.status,
                sg.evidence["reason"].as_str().unwrap_or("")
            );
        }
        let target = |k: u64| track_of(&e, "track", &format!("target-{k}"));
        assert_ne!(target(1), target(2), "the two targets stay apart");

        let mut labels: BTreeMap<String, HashMap<Option<u64>, usize>> = BTreeMap::new();
        let mut spans: HashMap<String, (DateTime<Utc>, DateTime<Utc>)> = HashMap::new();
        for r in rows.iter().filter(|r| r.feed != "track") {
            let key = format!("{}/{}", r.obs.source_id, r.obs.source_track_key);
            *labels
                .entry(key.clone())
                .or_default()
                .entry(r.truth)
                .or_default() += 1;
            let span = spans
                .entry(key)
                .or_insert((r.obs.observed_at, r.obs.observed_at));
            span.1 = r.obs.observed_at;
        }
        // Clear cases only: at least 10 reports, four in five on one target.
        let target_of = |key: &str| {
            let counts = &labels[key];
            let total: usize = counts.values().sum();
            let (label, n) = counts.iter().max_by_key(|(_, n)| **n).unwrap();
            (total >= 10 && *n * 5 >= total * 4)
                .then_some(*label)
                .flatten()
        };
        // A second track of one sensor on a target, while another of its tracks is on the
        // target's system track, is a duplicate.
        let duplicate = |key: &str, k: u64| {
            let (source, (start, end)) = (key.split('/').next().unwrap(), spans[key]);
            uid_of(&e, key) != target(k)
                && spans.iter().any(|(other, (s2, e2))| {
                    other != key
                        && other.starts_with(&format!("{source}/"))
                        && target_of(other) == Some(k)
                        && uid_of(&e, other) == target(k)
                        && *s2 < end
                        && *e2 > start
                })
        };
        let (mut right, mut wrong, mut alone, mut clutter, mut duplicates) = (0, 0, 0, 0, 0);
        for (key, counts) in &labels {
            let total: usize = counts.values().sum();
            let label = target_of(key);
            let uid = uid_of(&e, key);
            match label {
                Some(k) if duplicate(key, k) => duplicates += 1,
                Some(k) => {
                    let other = if k == 1 { 2 } else { 1 };
                    if uid == target(k) {
                        right += 1;
                    } else if uid == target(other) {
                        wrong += 1;
                        eprintln!("{key} (target {k}) ended on target {other}");
                    } else {
                        alone += 1;
                        eprintln!("{key} (target {k}, {total} reports) not paired");
                    }
                }
                None if counts.get(&None).is_some_and(|n| n * 5 >= total * 4)
                    && (uid == target(1) || uid == target(2)) =>
                {
                    clutter += 1;
                    eprintln!("{key} (clutter, {total} reports) paired with a target");
                }
                _ => {}
            }
        }
        eprintln!(
            "{name}: {right} paired right, {wrong} wrong, {alone} unpaired, {duplicates} same-sensor duplicates kept apart, {clutter} clutter paired"
        );
        assert_eq!(wrong, 0);
        assert_eq!(clutter, 0);
        assert!(right * 10 >= (right + alone) * 8, "at least 80% paired");
        let c = e.common.clone();
        let t1 = target(1);
        let edges = tokio::task::spawn_blocking(move || c.open_db().unwrap().explain(t1).unwrap())
            .await
            .unwrap();
        assert!(
            edges
                .iter()
                .any(|x| x.kind.as_str() == "MERGED_INTO" && x.decision_op == "merge")
        );
        e.redis.purge_namespace().await.unwrap();
    }

    /// Where each labelled plot went: its target's track, the other
    /// target's, or none; and how many clutter plots reached a track.
    async fn autoferry_detections(scenario: u32) {
        let Some((mut e, _dir)) = engine(&["track", "lidar-det", "radar-det"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        e.detection_sources = ["lidar-det", "radar-det"].map(String::from).into();
        let rows = recorded(&format!("scenario{scenario}-detections"));
        replay(&mut e, &rows).await;
        write_trace(&e, &format!("scenario{scenario}-detections"));
        let target = |k: u64| track_of(&e, "track", &format!("target-{k}"));
        let went: HashMap<&str, Option<Uid>> = e
            .associations
            .iter()
            .map(|(k, u)| (k.as_str(), *u))
            .collect();
        let (mut right, mut wrong, mut missed, mut clutter, mut noise) = (0, 0, 0, 0, 0);
        for r in rows.iter().filter(|r| r.feed != "track") {
            let key = format!("{}/{}", r.obs.source_id, r.obs.source_track_key);
            let to = went[key.as_str()];
            match r.truth {
                Some(k) if to == Some(target(k)) => right += 1,
                Some(_) if to.is_some() => wrong += 1,
                Some(_) => missed += 1,
                None if to.is_some() => clutter += 1,
                None => noise += 1,
            }
        }
        let on_target = right + wrong + missed;
        eprintln!(
            "scenario {scenario} detections: {right}/{on_target} to the right track, {wrong} wrong, {missed} missed; {clutter}/{} clutter associated",
            clutter + noise
        );
        // Plots only update tracks another source keeps: none are created.
        assert_eq!(e.tracks.len(), 2);
        assert!(
            right * 100 >= on_target * 85,
            "at least 85% associated right"
        );
        assert!(
            wrong * 100 <= on_target * 2,
            "at most 2% to the wrong target"
        );
        assert!(
            clutter * 10 <= clutter + noise,
            "at most 10% of clutter associated"
        );
        let t = &e.tracks[&target(1)];
        assert!(
            t.contributors
                .iter()
                .any(|c| c.source_track_key == DETECTIONS)
        );
        e.redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn replay_autoferry_2_tracks() {
        autoferry_tracks(2).await;
    }

    #[tokio::test]
    async fn replay_autoferry_16_tracks() {
        autoferry_tracks(16).await;
    }

    #[tokio::test]
    async fn replay_autoferry_2_gnn() {
        autoferry_tracker(2, "gnn").await;
    }

    #[tokio::test]
    async fn replay_autoferry_16_gnn() {
        autoferry_tracker(16, "gnn").await;
    }

    #[tokio::test]
    async fn replay_autoferry_2_mht() {
        autoferry_tracker(2, "mht").await;
    }

    #[tokio::test]
    async fn replay_autoferry_16_mht() {
        autoferry_tracker(16, "mht").await;
    }

    #[tokio::test]
    async fn replay_autoferry_2_detections() {
        autoferry_detections(2).await;
    }

    #[tokio::test]
    async fn replay_autoferry_16_detections() {
        autoferry_detections(16).await;
    }

    /// The newest decision of `op`.
    async fn last_decision(e: &Engine, op: &'static str) -> i64 {
        e.db(move |db| db.decisions_by_op(&[op], 1)).await.unwrap()[0].id
    }

    #[tokio::test]
    async fn track_management_is_undone_on_the_live_picture() {
        let Some((mut e, _dir)) = engine(&["ais"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        let ships = |s: i64| {
            vec![
                report("ais", "1", s, 32.0, -117.0, Some("1")),
                report("ais", "2", s, 32.05, -117.0, Some("2")),
                report("ais", "3", s, 32.10, -117.0, Some("3")),
            ]
        };
        for s in 0..3 {
            feed(&mut e, &ships(s)).await;
        }
        let t: Vec<Uid> = (1..=3)
            .map(|i| track_of(&e, "ais", &i.to_string()))
            .collect();
        let id = |u: Uid| u.doc_id();
        let undo = |d: i64| json!({"op": "undo", "decision": d, "actor": "bob@x.org"});

        // A delete undone: the track is back under its UID, even though the
        // feed went on and the engine made a new track for it meanwhile.
        let a = operator(&mut e, json!({"op": "delete", "tracks": [id(t[2])]})).await;
        assert_eq!(a["ok"], true, "{a}");
        feed(&mut e, &[report("ais", "3", 3, 32.10, -117.0, Some("3"))]).await;
        let meanwhile = track_of(&e, "ais", "3");
        assert_ne!(meanwhile, t[2]);
        let d = last_decision(&e, "delete_track").await;
        let a = operator(&mut e, undo(d)).await;
        assert_eq!(a["ok"], true, "{a}");
        assert_eq!(track_of(&e, "ais", "3"), t[2]);
        assert!(e.tracks.contains_key(&t[2]));
        assert!(!e.tracks.contains_key(&meanwhile), "the stand-in retires");
        assert!(e.redis.get_system_track(t[2]).await.unwrap().is_some());
        // Reports keep coming to it.
        feed(&mut e, &[report("ais", "3", 4, 32.10, -117.0, Some("3"))]).await;
        assert_eq!(track_of(&e, "ais", "3"), t[2]);

        // A merge undone: each track has its own source track again.
        let a = operator(
            &mut e,
            json!({"op": "merge", "from": id(t[0]), "into": id(t[1])}),
        )
        .await;
        assert_eq!(a["ok"], true, "{a}");
        assert!(!e.tracks.contains_key(&t[0]));
        let merge = last_decision(&e, "merge").await;
        let a = operator(&mut e, undo(merge)).await;
        assert_eq!(a["ok"], true, "{a}");
        assert_eq!(track_of(&e, "ais", "1"), t[0]);
        assert_eq!(track_of(&e, "ais", "2"), t[1]);
        assert_eq!(e.tracks[&t[1]].contributors.len(), 1);
        // Not twice.
        let a = operator(&mut e, undo(merge)).await;
        assert_eq!(a["ok"], false, "{a}");

        // A group formed and a pairing, undone.
        let a = operator(&mut e, json!({"op": "group_create", "members": [id(t[0]), id(t[1])],
            "spec": {"name": "TG 1", "sidc": "SFSPGG----", "affiliation": "friend", "echelon": "G"}}))
        .await;
        assert_eq!(a["ok"], true, "{a}");
        assert_eq!(e.groups.len(), 1);
        let d = last_decision(&e, "create_group").await;
        let a = operator(&mut e, undo(d)).await;
        assert_eq!(a["ok"], true, "{a}");
        assert!(e.groups.is_empty());
        assert!(e.tracks[&t[0]].groups.is_empty());
        let a = operator(
            &mut e,
            json!({"op": "pair", "tracks": [id(t[0]), id(t[2])]}),
        )
        .await;
        assert_eq!(a["ok"], true, "{a}");
        let d = last_decision(&e, "pair_tracks").await;
        let a = operator(&mut e, undo(d)).await;
        assert_eq!(a["ok"], true, "{a}");
        assert!(e.tracks[&t[0]].paired_with.is_empty());
        assert!(e.tracks[&t[2]].paired_with.is_empty());
        e.redis.purge_namespace().await.unwrap();
    }

    /// Hand every entry `from` logged since `after` to `to`, as the sync
    /// link will; returns the last sequence handed over.
    async fn hand_over(from: &Engine, to: &mut Engine, after: u64) -> u64 {
        let site = from.common.site;
        let entries = from
            .db(move |db| db.sync_since(site, after, 1000))
            .await
            .unwrap();
        let mut last = after;
        for entry in entries {
            last = entry.id.seq;
            let a = operator(to, json!({"op": "sync_entry", "entry": entry})).await;
            assert_eq!(a["ok"], true, "{a}");
        }
        last
    }

    async fn sync_status(e: &Engine, id: &str) -> ot_store::SyncStatus {
        let id: ot_sync::GlobalId = id.parse().unwrap();
        e.db(move |db| db.sync_entry(id)).await.unwrap().unwrap().1
    }

    #[tokio::test]
    async fn track_management_on_one_node_holds_on_another() {
        let Some((mut a, _da)) = engine_at("AAA", &["ais"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        let (mut b, _db) = engine_at("BBB", &["peer:AAA"]).await.unwrap();
        let ships = |s: i64| {
            (1..=3)
                .map(|i| {
                    report(
                        "ais",
                        &i.to_string(),
                        s,
                        32.0 + 0.05 * i as f64,
                        -117.0,
                        Some(&i.to_string()),
                    )
                })
                .collect::<Vec<_>>()
        };
        for s in 0..3 {
            feed(&mut a, &ships(s)).await;
        }
        let t: Vec<Uid> = (1..=3)
            .map(|i| track_of(&a, "ais", &i.to_string()))
            .collect();
        let id = |u: Uid| u.doc_id();
        // B hears A's reports of its first two tracks, under A's UIDs.
        let peer =
            |u: Uid, s: i64, lat: f64| report("peer:AAA", &u.to_string(), s, lat, -117.0, None);
        for s in 0..3 {
            feed(&mut b, &[peer(t[0], s, 32.05), peer(t[1], s, 32.10)]).await;
        }
        assert!(b.tracks.contains_key(&t[0]) && b.tracks.contains_key(&t[1]));

        // A pair on A holds on B.
        let r = operator(
            &mut a,
            json!({"op": "pair", "tracks": [id(t[0]), id(t[1])], "actor": "tm@a"}),
        )
        .await;
        assert_eq!(r["ok"], true, "{r}");
        let mut seen = hand_over(&a, &mut b, 0).await;
        assert_eq!(seen, 1);
        assert_eq!(b.tracks[&t[0]].paired_with, vec![t[1]]);
        assert_eq!(
            sync_status(&b, "AAA:1").await,
            ot_store::SyncStatus::Applied
        );

        // A group keeps A's UID on B.
        let r = operator(&mut a, json!({"op": "group_create", "members": [id(t[0]), id(t[1])], "actor": "tm@a",
            "spec": {"name": "TG 1", "sidc": "SFSPGG----", "affiliation": "friend", "echelon": "G"}})).await;
        assert_eq!(r["ok"], true, "{r}");
        let g: Uid = r["result"]["group"]
            .as_str()
            .unwrap()
            .trim_start_matches("tms-")
            .parse()
            .unwrap();
        assert_eq!(g.site().as_str(), "AAA");
        seen = hand_over(&a, &mut b, seen).await;
        assert!(b.groups.contains_key(&g));

        // Undone on A: undone on B (the group first; it blocks the pair's undo).
        for op in ["create_group", "pair_tracks"] {
            let d = last_decision(&a, op).await;
            let r = operator(
                &mut a,
                json!({"op": "undo", "decision": d, "actor": "tm@a"}),
            )
            .await;
            assert_eq!(r["ok"], true, "{r}");
            seen = hand_over(&a, &mut b, seen).await;
        }
        assert!(b.groups.is_empty());
        assert!(b.tracks[&t[0]].paired_with.is_empty());

        // A decision about a track B has not heard of waits for it.
        let r = operator(
            &mut a,
            json!({"op": "delete", "tracks": [id(t[2])], "actor": "tm@a"}),
        )
        .await;
        assert_eq!(r["ok"], true, "{r}");
        let del = format!("AAA:{}", seen + 1);
        hand_over(&a, &mut b, seen).await;
        assert_eq!(sync_status(&b, &del).await, ot_store::SyncStatus::Pending);
        feed(&mut b, &[peer(t[2], 3, 32.15)]).await;
        assert!(b.tracks.contains_key(&t[2]));
        b.retry_pending().await.unwrap();
        assert_eq!(sync_status(&b, &del).await, ot_store::SyncStatus::Applied);
        assert!(!b.tracks.contains_key(&t[2]));

        // B's own word, and a late one from A on the same pair: the later
        // stamp wins on both nodes.
        let r = operator(
            &mut b,
            json!({"op": "do_not_pair", "a": id(t[0]), "b": id(t[1]), "actor": "tm@b"}),
        )
        .await;
        assert_eq!(r["ok"], true, "{r}");
        let early = ot_sync::Entry {
            id: "AAA:99".parse().unwrap(),
            hlc: ot_sync::Hlc::new(1, 0),
            actor: "tm@a".into(),
            role: "track_manager".into(),
            command: json!({"op": "pair", "tracks": [id(t[0]), id(t[1])]}),
        };
        let r = operator(&mut b, json!({"op": "sync_entry", "entry": early})).await;
        assert_eq!(r["result"]["status"], "superseded", "{r}");
        assert!(b.tracks[&t[0]].paired_with.is_empty());
        // Heard twice: kept once.
        let r = operator(&mut b, json!({"op": "sync_entry", "entry": early})).await;
        assert_eq!(r["result"]["status"], "superseded", "{r}");
        // And B's decision reaches A.
        hand_over(&b, &mut a, 0).await;
        let bb = b.common.site;
        let on_a = a.db(move |db| db.sync_since(bb, 0, 10)).await.unwrap();
        assert_eq!(on_a.len(), 1);
        a.redis.purge_namespace().await.unwrap();
        b.redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn a_bad_history_point_is_deleted_and_the_track_steps_back() {
        let Some((mut e, _dir)) = engine(&["ais"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        // Every update a point (the default keeps one per 10 s).
        e.history_every_ms = 0;
        for s in 0..3 {
            feed(
                &mut e,
                &[report(
                    "ais",
                    "1",
                    s,
                    32.0 + 0.001 * s as f64,
                    -117.0,
                    Some("1"),
                )],
            )
            .await;
        }
        // A bad fix, far off.
        feed(&mut e, &[report("ais", "1", 3, 33.0, -117.0, Some("1"))]).await;
        let t = track_of(&e, "ais", "1");
        let points = e.redis.track_history(t, None, None, 100).await.unwrap();
        assert_eq!(points.len(), 4, "{points:?}");
        let bad = points.last().unwrap().t;
        assert!((e.tracks[&t].view.position.latitude - 33.0).abs() < 1e-9);

        let a = operator(
            &mut e,
            json!({"op": "delete_history_point", "track": t.doc_id(), "t": bad,
                   "actor": "ann@x.org", "reason": "a GPS jump"}),
        )
        .await;
        assert_eq!(a["ok"], true, "{a}");
        assert_eq!(a["result"]["stepped_back"], true);
        let points = e.redis.track_history(t, None, None, 100).await.unwrap();
        assert_eq!(points.len(), 3);
        assert!((e.tracks[&t].view.position.latitude - 32.002).abs() < 1e-9);
        // The deletion is queued for NATS.
        e.redis.ensure_outbox_group("w").await.unwrap();
        let out = e
            .redis
            .read_outbox("w", "w1", 1000, std::time::Duration::ZERO, false)
            .await;
        let deleted = out
            .map(|v| v.into_iter().any(|o| matches!(o.op, ot_store::OutboxOp::HistoryDelete { point, .. } if point.t == bad)))
            .unwrap_or(false);
        assert!(deleted, "a history_delete in the outbox");
        // No point there any more.
        let a = operator(
            &mut e,
            json!({"op": "delete_history_point", "track": t.doc_id(), "t": bad}),
        )
        .await;
        assert_eq!(a["ok"], false, "{a}");
        e.redis.purge_namespace().await.unwrap();
    }

    #[tokio::test]
    async fn a_radar_track_leaves_a_slow_ais_track_it_stopped_following() {
        let Some((mut e, _dir)) = engine(&["ais", "radar"]).await else {
            eprintln!("skipped: OT_TEST_REDIS_URL not set");
            return;
        };
        e.settings.correlation.split.automatic = true;
        // AIS every 10 s, radar every 3 s, together for three minutes.
        let at = |s: i64| {
            let mut v = Vec::new();
            if s % 10 == 0 {
                v.push(report("ais", "1", s, 32.0, -117.0, Some("1")));
            }
            if s % 3 == 0 {
                v.push(report("radar", "r1", s, 32.0, -117.0, None));
            }
            v
        };
        for s in 0..180 {
            let r = at(s);
            if !r.is_empty() {
                feed(&mut e, &r).await;
            }
        }
        assert_eq!(
            track_of(&e, "radar", "r1"),
            track_of(&e, "ais", "1"),
            "paired"
        );
        // The radar track follows another vessel, 500 m off, from now on.
        for s in 180..420 {
            let mut r = Vec::new();
            if s % 10 == 0 {
                r.push(report("ais", "1", s, 32.0, -117.0, Some("1")));
            }
            if s % 3 == 0 {
                r.push(report("radar", "r1", s, 32.0045, -117.0, None));
            }
            if !r.is_empty() {
                feed(&mut e, &r).await;
            }
        }
        assert_ne!(
            track_of(&e, "radar", "r1"),
            track_of(&e, "ais", "1"),
            "split within four minutes"
        );
        e.redis.purge_namespace().await.unwrap();
    }
}
