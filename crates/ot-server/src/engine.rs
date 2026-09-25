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
//! The engine also owns the lifecycle (tentative → confirmed → lost →
//! dropped) with per-domain stale times, and publishing through the outbox.
//!
//! It also resolves each track's published `attributes` against the latest
//! published output schema: the entity's card first (with a notice where a
//! feed disagrees), then the feed, then linked built-ins. Card and schema
//! changes are picked up within seconds and republish the affected tracks.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ot_core::{Contributor, Domain, Observation, PairingType, SystemTrack, TrackState, Uid};
use ot_source::schema::{ExtensionSchema, resolve_attributes};
use ot_store::{Decision, RedisStore};
use serde_json::{Map, Value, json};

use crate::config::Common;
use crate::correlate::{self, Contribution, GateSettings};

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
pub fn apply_view(track: &mut SystemTrack, view: Observation, confirm_after: u64) -> Applied {
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
    /// System tracks merged into another.
    merged: u64,
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
    attrs: Attributes,
}

const DEFAULT_PRIORITY: i64 = 100;

fn contributor_key(c: &Contributor) -> String {
    format!("{}/{}", c.source_id, c.source_track_key)
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
            attrs: Attributes::default(),
        };
        let uids: Vec<Uid> = engine.tracks.keys().copied().collect();
        for uid in uids {
            engine.index_track(uid);
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
        let rows: Vec<(String, i64)> = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok(c.open_db()?
                .list_sources()?
                .into_iter()
                .filter(|s| s.enabled)
                .map(|s| (s.id, s.priority))
                .collect())
        })
        .await??;
        self.priorities = rows.iter().cloned().collect();
        let mut ids: Vec<String> = rows.into_iter().map(|(id, _)| id).collect();
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
        let mut republished = 0;
        for track in self.tracks.values_mut() {
            if resolve(&self.attrs, track) {
                self.redis.put_system_track(track, true).await?;
                republished += 1;
            }
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
        for (source, id, obs) in batch {
            acks.entry(source.clone()).or_default().push(id.clone());
            let obs = match obs {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(%source, %id, error = %e, "unreadable observation skipped");
                    counts.unreadable += 1;
                    continue;
                }
            };
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
            match self.observe(uid, obs).await? {
                Some((track, urgent)) => {
                    counts.observations += 1;
                    counts.urgent += u64::from(urgent);
                    self.redis.put_system_track(&track, urgent).await?;
                }
                None => counts.out_of_order += 1,
            }
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
            let decision = Decision::new("engine", "pair")
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
                Decision::new("engine", "create_system_track")
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
        let o = &self.tracks[&other];
        let (from, into) = if o.contributors.len() == 1 && o.first_seen > t.first_seen {
            (other, uid)
        } else {
            (uid, other)
        };
        let decision = Decision::new("engine", "merge")
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
        for c in &gone.contributors {
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
        self.redis
            .retire_system_track(from, &format!("merged into {}", into.doc_id()))
            .await?;
        Ok(())
    }

    /// Load contributors' latest reports that are not in memory (after a restart).
    async fn load_missing(&mut self, uid: Uid) -> anyhow::Result<()> {
        let missing: Vec<(String, String)> = self.tracks[&uid]
            .contributors
            .iter()
            .filter(|c| !self.latest.contains_key(&contributor_key(c)))
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
        let key = format!("{}/{}", obs.source_id, obs.source_track_key);
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
            return Ok(Some((t, true)));
        }
        let multi = self.tracks[&uid].contributors.len() > 1;
        if multi {
            self.load_missing(uid).await?;
        }
        let best = multi.then(|| {
            let t = &self.tracks[&uid];
            let contribs: Vec<Contribution<'_>> = t
                .contributors
                .iter()
                .filter_map(|c| {
                    self.latest.get(&contributor_key(c)).map(|o| Contribution {
                        obs: o,
                        priority: self
                            .priorities
                            .get(&c.source_id)
                            .copied()
                            .unwrap_or(DEFAULT_PRIORITY),
                    })
                })
                .collect();
            correlate::best_view(&contribs, self.settings.freshness_secs)
        });
        let confirm = self.settings.confirm_after;
        let t = self.tracks.get_mut(&uid).expect("checked above");
        let before = t.entity_id.clone();
        let outcome = match best {
            Some((view, provenance)) => {
                t.observation_count += 1;
                if let Some(c) = t
                    .contributors
                    .iter_mut()
                    .find(|c| contributor_key(c) == key)
                {
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
        Ok(Some((out, urgent)))
    }

    async fn reap(&mut self) -> anyhow::Result<()> {
        let now = Utc::now();
        let mut dropped = Vec::new();
        let mut lost = 0;
        for track in self.tracks.values_mut() {
            match lifecycle(track, now, &self.settings) {
                Lifecycle::Keep => {}
                Lifecycle::Lost => {
                    lost += 1;
                    track.state = TrackState::Lost;
                    resolve(&self.attrs, track);
                    self.redis.put_system_track(track, true).await?;
                }
                Lifecycle::Drop => dropped.push(track.uid),
            }
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
                let d = Decision::new("engine", "drop_system_track")
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
            self.redis.retire_system_track(uid, &reason).await?;
            if let Some(t) = self.tracks.remove(&uid) {
                for c in &t.contributors {
                    self.latest.remove(&contributor_key(c));
                }
            }
            self.reports.retain(|_, u| *u != uid);
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
        e.reports[&format!("{source}/{key}")]
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
        feed(&mut e, &[report("ais", "777", 0, 10.0, 20.0, Some("777"))]).await;
        // TAK sees something nearby with no identifier yet: its own track.
        feed(&mut e, &[report("tak", "u9", 2, 10.001, 20.0, None)]).await;
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
}
