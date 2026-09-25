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
//! published output schema: the entity's card first (with a notice where a
//! feed disagrees), then the feed, then linked built-ins. Card and schema
//! changes are picked up within seconds and republish the affected tracks.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use ot_core::{Contributor, Domain, Observation, PairingType, SystemTrack, TrackState, Uid};
use ot_source::schema::{ExtensionSchema, resolve_attributes};
use ot_store::{Decision, RedisStore};
use serde_json::{Map, Value, json};

use crate::config::Common;
use crate::correlate::{
    self, Approach, Contribution, GateSettings, Grid, KinematicSettings, Persistence,
};

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
    /// Reports this close to a system track's newest one compete on quality
    /// for its position (best-source selection).
    pub freshness_secs: f64,
    /// Sanity gate on identifier matches.
    pub gate: GateSettings,
    /// How tracks with no shared identity pair.
    pub approach: Approach,
    /// Kinematic pairing and detection association.
    pub kinematic: KinematicSettings,
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
            freshness_secs: 60.0,
            gate: GateSettings::default(),
            approach: Approach::default(),
            kinematic: KinematicSettings::default(),
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

/// Apply an observation to a system track whose only contributor reported it.
pub fn apply(track: &mut SystemTrack, obs: Observation, confirm_after: u64) -> Applied {
    track.observation_count += 1;
    if let Some(c) = track.contributors.first_mut() {
        c.last_report = c.last_report.max(obs.observed_at);
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

/// What attribute resolution needs: the latest published output schema and
/// every card, with the version they were loaded at.
#[derive(Default)]
struct Attributes {
    version: String,
    schema: Option<ExtensionSchema>,
    cards: HashMap<String, Map<String, Value>>,
}

/// The entity a track's registry match points to, when it was corroborated
/// and unambiguous (so its card may speak for the track).
fn entity_of(t: &SystemTrack) -> Option<String> {
    crate::correlate::trusted_entity(&t.view)
}

/// Re-resolve a track's entity, attributes and notices. True if any changed.
fn resolve(attrs: &Attributes, t: &mut SystemTrack) -> bool {
    let entity = entity_of(t);
    let entity_changed = entity != t.entity_id;
    t.entity_id = entity;
    let (values, notices) = match &attrs.schema {
        Some(schema) => {
            let card = t.entity_id.as_ref().and_then(|e| attrs.cards.get(e));
            resolve_attributes(schema, t, card)
        }
        None => (Map::new(), Vec::new()),
    };
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
    /// Where each live system track is, for kinematic comparisons.
    grid: Grid<Uid>,
    /// Recent gate results per pair of system tracks (lower uid first).
    candidates: HashMap<(Uid, Uid), Persistence>,
    attrs: Attributes,
    /// Where each detection went, for scoring replays.
    #[cfg(test)]
    associations: Vec<(String, Option<Uid>)>,
    /// Every report, association and merge in order, for replay videos.
    #[cfg(test)]
    trace: Vec<Value>,
    /// The time of the report being processed (for traced merges).
    #[cfg(test)]
    clock: Option<DateTime<Utc>>,
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
    pub async fn new(common: Common, settings: EngineSettings) -> anyhow::Result<Self> {
        let redis = common.open_redis().await?;
        let c = common.clone();
        let reports = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok(c.open_db()?.live_reports()?)
        })
        .await??;
        let tracks: HashMap<Uid, SystemTrack> = redis
            .list_system_tracks()
            .await?
            .into_iter()
            .map(|t| (t.uid, t))
            .collect();
        tracing::info!(
            links = reports.len(),
            tracks = tracks.len(),
            "engine state loaded"
        );
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
            grid: Grid::default(),
            candidates: HashMap::new(),
            attrs: Attributes::default(),
            #[cfg(test)]
            associations: Vec::new(),
            #[cfg(test)]
            trace: Vec::new(),
            #[cfg(test)]
            clock: None,
            #[cfg(test)]
            ended_on: HashMap::new(),
        };
        let uids: Vec<Uid> = engine.tracks.keys().copied().collect();
        for uid in uids {
            engine.index_track(uid);
            let p = engine.tracks[&uid].view.position;
            engine.grid.put(uid, p.latitude, p.longitude);
        }
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
        let mut sources_tick = tokio::time::interval(Duration::from_secs(5));
        let mut reap_tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = sources_tick.tick() => {
                    if let Err(e) = self.refresh_sources().await {
                        tracing::warn!(error = %format!("{e:#}"), "source list refresh failed");
                    }
                    if let Err(e) = self.refresh_attributes().await {
                        tracing::warn!(error = %format!("{e:#}"), "card refresh failed");
                    }
                }
                _ = reap_tick.tick() => {
                    if let Err(e) = self.reap().await {
                        tracing::warn!(error = %format!("{e:#}"), "lifecycle sweep failed");
                    }
                }
                res = self.pump(false) => {
                    if let Err(e) = res {
                        tracing::warn!(error = %format!("{e:#}"), "engine cycle failed; retrying");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
        tracing::info!(tracks = self.tracks.len(), "engine stopped");
        Ok(())
    }

    async fn refresh_sources(&mut self) -> anyhow::Result<()> {
        let c = self.common.clone();
        let rows: Vec<(String, i64, bool, bool)> =
            tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
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
                        let alone =
                            serde_json::from_value::<ot_source::source::SourceSpec>(s.spec.clone())
                                .map_or(true, |spec| spec.publishes_alone());
                        (s.id, s.priority, detections, alone)
                    })
                    .collect())
            })
            .await??;
        self.priorities = rows.iter().map(|(id, p, _, _)| (id.clone(), *p)).collect();
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

    /// Reload cards and the output schema when either changed, and republish
    /// every live track whose attributes change as a result.
    async fn refresh_attributes(&mut self) -> anyhow::Result<()> {
        let c = self.common.clone();
        let known = self.attrs.version.clone();
        let loaded = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Attributes>> {
            let db = c.open_db()?;
            let version = db.cards_version()?;
            if version == known {
                return Ok(None);
            }
            let schema = crate::sources::load_schemas(&db)?
                .into_values()
                .max_by_key(|s| s.version);
            Ok(Some(Attributes {
                version,
                schema,
                cards: db.all_cards()?.into_iter().collect(),
            }))
        })
        .await??;
        let Some(attrs) = loaded else {
            return Ok(());
        };
        self.attrs = attrs;
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
            cards = self.attrs.cards.len(),
            republished,
            "cards and output schema loaded"
        );
        Ok(())
    }

    async fn pump(&mut self, pending: bool) -> anyhow::Result<()> {
        let batch = self
            .redis
            .read_observations(
                &self.sources,
                GROUP,
                &self.settings.consumer,
                1000,
                Duration::from_secs(1),
                pending,
            )
            .await?;
        if batch.is_empty() {
            return Ok(());
        }
        let mut counts = EngineCounts::default();
        let mut acks: HashMap<String, Vec<String>> = HashMap::new();
        let mut reports = Vec::with_capacity(batch.len());
        for (source, id, obs) in batch {
            acks.entry(source.clone()).or_default().push(id.clone());
            match obs {
                Ok(o) => reports.push(o),
                Err(e) => {
                    tracing::warn!(%source, %id, error = %e, "unreadable observation skipped");
                    counts.unreadable += 1;
                }
            }
        }
        // A batch holds each source's reports in turn: take them in time order.
        reports.sort_by_key(|o| o.observed_at);
        let mut reports = reports.into_iter().peekable();
        while let Some(obs) = reports.next() {
            #[cfg(test)]
            {
                self.clock = Some(obs.observed_at);
            }
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
            self.redis
                .put_source_track(&obs, Duration::from_secs(24 * 3600))
                .await?;
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
                }
                None => counts.out_of_order += 1,
            }
            #[cfg(test)]
            self.trace.push(json!({
                "t": obs.observed_at, "kind": "report", "source": obs.source_id,
                "key": obs.source_track_key, "lat": obs.position.latitude,
                "lon": obs.position.longitude,
                "uid": self.reports.get(&key).map(|u| u.doc_id()),
            }));
        }
        for (source, ids) in acks {
            self.redis.ack_observations(&source, GROUP, &ids).await?;
        }
        self.redis
            .incr_metrics(crate::metrics::ENGINE, &counts.pairs())
            .await?;
        Ok(())
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
            let gate = correlate::sanity_gate(obs, &t.view, &self.settings.gate);
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
        let key = format!("{}/{}", obs.source_id, obs.source_track_key);
        let (source, track_key) = (obs.source_id.clone(), obs.source_track_key.clone());
        let c = self.common.clone();
        if let Some((uid, evidence)) = self.find_match(obs, None) {
            let attrs = json!({ "pairing": "auto", "confidence": 1.0, "evidence": evidence });
            let decision = engine_decision("pair")
                .reason(format!(
                    "{key} shares {} with {}",
                    evidence["identity"].as_str().unwrap_or("an identity"),
                    uid.doc_id()
                ))
                .evidence(evidence);
            tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
                Ok(c.open_db()?
                    .pair_source_track(&source, &track_key, uid, &attrs, decision)?)
            })
            .await??;
            if let Some(t) = self.tracks.get_mut(&uid) {
                t.contributors.push(Contributor {
                    source_id: obs.source_id.clone(),
                    source_track_key: obs.source_track_key.clone(),
                    pairing: PairingType::Auto,
                    confidence: 1.0,
                    last_report: obs.observed_at,
                });
            }
            self.reports.insert(key, uid);
            counts.paired += 1;
            return Ok(uid);
        }
        let site = c.site;
        let (uid, _) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok(c.open_db()?.create_system_track(
                site,
                &source,
                &track_key,
                engine_decision("create_system_track")
                    .reason("new source track with no identity match"),
            )?)
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
        #[cfg(test)]
        self.trace.push(json!({
            "t": self.clock, "kind": "merge", "from": from.doc_id(), "into": into.doc_id(),
        }));
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
        if let Some(t) = self.tracks.get_mut(&into) {
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
        Ok(())
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
        let Some(t) = self.tracks.get(&uid) else {
            return Ok(());
        };
        let publish = t.is_published() || self.authoritative(t);
        let t = self.tracks.get_mut(&uid).expect("checked above");
        if publish {
            let first = !t.is_published();
            t.published = Some(true);
            self.redis.put_system_track(t, urgent || first).await?;
        } else {
            self.redis.put_system_track_quietly(t).await?;
        }
        Ok(())
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
            let mut t = SystemTrack::from_first_observation(uid, obs);
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
        let confirm = self.settings.confirm_after;
        let t = self.tracks.get_mut(&uid).expect("checked above");
        let before = t.entity_id.clone();
        let outcome = match best {
            Some((view, provenance)) => {
                t.observation_count += 1;
                if let Some(c) = t.contributors.iter_mut().find(|c| {
                    c.source_id == obs.source_id && c.source_track_key == obs.source_track_key
                }) {
                    c.last_report = c.last_report.max(obs.observed_at);
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
            .then(|| correlate::best_view(&contribs, self.settings.freshness_secs))
    }

    /// Sources still reporting for a track (within the kinematic max age of
    /// `at`) through source tracks of their own; not detections, which the
    /// engine associated. A source's track that stopped reporting does not
    /// block its next track of the same object (a sensor re-initiating).
    fn track_sources(&self, uid: Uid, at: DateTime<Utc>) -> HashSet<&str> {
        let max_age =
            chrono::Duration::milliseconds((self.settings.kinematic.max_age_secs * 1000.0) as i64);
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
        if self.settings.approach == Approach::Identifiers || !self.tracks.contains_key(&uid) {
            return Ok(());
        }
        let s = self.settings.kinematic;
        let mine: HashSet<String> = self
            .track_sources(uid, obs.observed_at)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let max_age = chrono::Duration::milliseconds((s.max_age_secs * 1000.0) as i64);
        let mut ready: Option<(Uid, correlate::Kinematic, usize, usize)> = None;
        for other in self
            .grid
            .near(obs.position.latitude, obs.position.longitude)
        {
            if other == uid {
                continue;
            }
            let Some(t) = self.tracks.get(&other) else {
                continue;
            };
            if (obs.observed_at - t.view.observed_at).abs() > max_age
                || t.state == TrackState::Lost
                || self
                    .track_sources(other, obs.observed_at)
                    .iter()
                    .any(|x| mine.contains(*x))
            {
                continue;
            }
            if self.settings.approach == Approach::KinematicsMetadata
                && correlate::veto(obs, &t.view).is_some()
            {
                continue;
            }
            let k = correlate::kinematic(obs, &t.view, &s);
            let key = pair_key(uid, other);
            if !k.pass && !self.candidates.contains_key(&key) {
                continue;
            }
            let (hits, of) =
                self.candidates
                    .entry(key)
                    .or_default()
                    .record(obs.observed_at, k.pass, &s);
            if hits >= s.m && ready.as_ref().is_none_or(|r| k.d2 < r.1.d2) {
                ready = Some((other, k, hits, of));
            }
        }
        // Forget pairs that stopped being compared.
        let window = chrono::Duration::milliseconds((s.window_secs * 1000.0) as i64);
        self.candidates
            .retain(|_, p| p.last().is_some_and(|l| obs.observed_at - l <= window));
        let Some((other, k, hits, of)) = ready else {
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
            "approach": self.settings.approach,
            "hits": hits, "of": of,
            "chi2_gate": s.chi2_gate,
            "last": k,
        });
        let decision = engine_decision("merge")
            .reason(format!(
                "{} and {} agreed kinematically in {hits} of {of} comparisons ({:.0} m apart, σ {:.0} m)",
                from.doc_id(),
                into.doc_id(),
                k.distance_m,
                k.sigma_m
            ))
            .evidence(evidence);
        let c = self.common.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            Ok(c.open_db()?.merge_system_tracks(from, into, decision)?)
        })
        .await??;
        self.absorb(from, into).await?;
        counts.kinematic_merged += 1;
        self.republish(into).await?;
        Ok(())
    }

    /// Recompute a track's view from its contributors and publish it.
    async fn republish(&mut self, uid: Uid) -> anyhow::Result<()> {
        self.load_missing(uid).await?;
        let best = self.best_of(uid);
        let confirm = self.settings.confirm_after;
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
        let s = self.settings.kinematic;
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
                #[cfg(test)]
                self.trace.push(json!({
                    "t": det.observed_at, "kind": "detection", "source": det.source_id,
                    "key": det.source_track_key, "lat": det.position.latitude,
                    "lon": det.position.longitude, "uid": null,
                }));
                #[cfg(test)]
                self.associations
                    .push((format!("{}/{}", det.source_id, det.source_track_key), None));
                continue;
            };
            #[cfg(test)]
            self.associations.push((
                format!("{}/{}", det.source_id, det.source_track_key),
                Some(uid),
            ));
            #[cfg(test)]
            self.trace.push(json!({
                "t": det.observed_at, "kind": "detection", "source": det.source_id,
                "key": det.source_track_key, "lat": det.position.latitude,
                "lon": det.position.longitude, "uid": uid.doc_id(),
            }));
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
                });
            }
            if let Some((_, urgent)) = self.observe(uid, det).await? {
                counts.associated += 1;
                self.save(uid, urgent).await?;
            }
        }
        Ok(())
    }

    async fn reap(&mut self) -> anyhow::Result<()> {
        let now = Utc::now();
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
    fn cards_attach_through_a_corroborated_registry_match() {
        let schema: ExtensionSchema =
            serde_json::from_value(serde_json::json!({"version": 2, "fields": [
                {"key": "destination", "type": "string"},
                {"key": "contact_phone", "type": "string"},
                {"key": "state", "type": "string", "builtin": "state"}
            ]}))
            .unwrap();
        let card = serde_json::json!({"contact_phone": "+1 555 0100", "destination": "LONG BEACH"});
        let attrs = Attributes {
            version: "v".into(),
            schema: Some(schema),
            cards: [("ent-1".to_string(), card.as_object().unwrap().clone())].into(),
        };
        let mut o = obs(t0(), "A", Domain::Surface);
        o.ext
            .insert("destination".into(), serde_json::json!("SAN DIEGO"));
        o.ext.insert(
            "registry".into(),
            serde_json::json!({"entity_id": "ent-1", "applied": true, "grade": "exact"}),
        );
        let uid: Uid = "OTK000000001".parse().unwrap();
        let mut t = SystemTrack::from_first_observation(uid, o.clone());
        assert!(resolve(&attrs, &mut t));
        assert_eq!(t.entity_id.as_deref(), Some("ent-1"));
        assert_eq!(
            Value::Object(t.attributes.clone()),
            serde_json::json!({"contact_phone": "+1 555 0100", "destination": "LONG BEACH",
                               "state": "tentative"})
        );
        assert_eq!(t.notices.len(), 1);
        assert_eq!(t.notices[0].feed, "SAN DIEGO");
        // Nothing changed: no republish needed.
        assert!(!resolve(&attrs, &mut t));

        // A stale (uncorroborated) match must not pull in the card.
        o.ext.insert(
            "registry".into(),
            serde_json::json!({"entity_id": "ent-1", "applied": false, "grade": "stale"}),
        );
        let mut stale = SystemTrack::from_first_observation(uid, o);
        resolve(&attrs, &mut stale);
        assert_eq!(stale.entity_id, None);
        assert_eq!(stale.attributes["destination"], "SAN DIEGO");
        assert!(stale.notices.is_empty());
    }

    // --- Replay: scripted reports through a real engine (Redis + SQLite) ---

    /// An engine on a throwaway Redis namespace and database; None (test
    /// skipped) without OT_TEST_REDIS_URL.
    async fn engine(sources: &[&str]) -> Option<(Engine, tempfile::TempDir)> {
        let url = std::env::var("OT_TEST_REDIS_URL").ok()?;
        let dir = tempfile::tempdir().unwrap();
        let common = Common {
            sqlite: dir.path().join("ot.db"),
            redis: url,
            redis_namespace: format!(
                "ot-engine-test-{}-{}",
                std::process::id(),
                Utc::now().timestamp_micros()
            ),
            site: ot_core::SiteCode::new("TST").unwrap(),
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
        };
        common.open_db().unwrap();
        let mut e = Engine::new(common, EngineSettings::default())
            .await
            .unwrap();
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
        for s in 10..13 {
            feed(&mut e, &[ais(s)]).await;
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
    // Fixtures from scripts/autoferry.py (Autoferry sensor fusion dataset,
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
    /// (scripts/replay-video.py renders it).
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
}
