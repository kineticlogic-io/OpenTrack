//! The engine role.
//!
//! Phase 1 engine: every source track becomes its own system track (1:1), so
//! the pipeline is proven before correlation arrives in phase 3. It already
//! owns what correlation will build on: system track creation through the
//! track graph, the lifecycle (tentative → confirmed → lost → dropped) with
//! per-domain stale times, and publishing through the outbox.
//!
//! It also resolves each track's published `attributes` against the latest
//! published output schema: the entity's card first (with a notice where a
//! feed disagrees), then the feed, then linked built-ins. Card and schema
//! changes are picked up within seconds and republish the affected tracks.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ot_core::{Domain, Observation, SystemTrack, TrackState, Uid};
use ot_source::schema::{ExtensionSchema, resolve_attributes};
use ot_store::{Decision, RedisStore};
use serde_json::{Map, Value};

use crate::config::Common;

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

/// Apply an observation to a system track (1:1: the track's only source).
pub fn apply(track: &mut SystemTrack, obs: Observation, confirm_after: u64) -> Applied {
    track.observation_count += 1;
    if let Some(c) = track.contributors.first_mut() {
        c.last_report = c.last_report.max(obs.observed_at);
    }
    if obs.observed_at < track.last_seen {
        return Applied::OutOfOrder;
    }
    let before_state = track.state;
    let identity_changed = track.view.name != obs.name
        || track.view.callsign != obs.callsign
        || track.view.identifiers != obs.identifiers
        || track.view.classification.cot_type_or_derived()
            != obs.classification.cot_type_or_derived();
    track.last_seen = obs.observed_at;
    track.view = obs;
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
    let r = t.view.ext.get("registry")?;
    let corroborated = r
        .get("corroborated")
        .or_else(|| r.get("applied"))
        .and_then(Value::as_bool)
        == Some(true);
    let conflicted = r
        .get("conflicts")
        .and_then(Value::as_array)
        .is_some_and(|c| !c.is_empty());
    (corroborated && !conflicted)
        .then(|| {
            r.get("entity_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .flatten()
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

pub struct Engine {
    common: Common,
    settings: EngineSettings,
    redis: RedisStore,
    /// `<source>/<key>` → system track.
    reports: HashMap<String, Uid>,
    tracks: HashMap<Uid, SystemTrack>,
    sources: Vec<String>,
    attrs: Attributes,
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
        Ok(Self {
            common,
            settings,
            redis,
            reports: reports.into_iter().collect(),
            tracks,
            sources: Vec::new(),
            attrs: Attributes::default(),
        })
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
        let mut ids: Vec<String> = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok(c.open_db()?
                .list_sources()?
                .into_iter()
                .filter(|s| s.enabled)
                .map(|s| s.id)
                .collect())
        })
        .await??;
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
        // Create system tracks for new source tracks in one transaction batch.
        let new_keys: Vec<(String, String)> = batch
            .iter()
            .filter_map(|(_, _, obs)| obs.as_ref().ok())
            .map(|o| (o.source_id.clone(), o.source_track_key.clone()))
            .filter(|(s, k)| !self.reports.contains_key(&format!("{s}/{k}")))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !new_keys.is_empty() {
            let c = self.common.clone();
            let created = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(String, Uid)>> {
                let mut db = c.open_db()?;
                let mut out = Vec::new();
                for (source, key) in new_keys {
                    let (uid, _) = db.create_system_track(
                        c.site,
                        &source,
                        &key,
                        Decision::new("engine", "create_system_track")
                            .reason("new source track; one system track per source track until correlation (phase 3)"),
                    )?;
                    out.push((format!("{source}/{key}"), uid));
                }
                Ok(out)
            })
            .await??;
            self.reports.extend(created);
        }

        let mut acks: HashMap<String, Vec<String>> = HashMap::new();
        for (source, id, obs) in batch {
            acks.entry(source.clone()).or_default().push(id.clone());
            let obs = match obs {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(%source, %id, error = %e, "unreadable observation skipped");
                    continue;
                }
            };
            let key = format!("{}/{}", obs.source_id, obs.source_track_key);
            let Some(&uid) = self.reports.get(&key) else {
                continue;
            };
            self.redis
                .put_source_track(&obs, Duration::from_secs(24 * 3600))
                .await?;
            let (track, urgent) = match self.tracks.get_mut(&uid) {
                Some(t) => {
                    let outcome = apply(t, obs, self.settings.confirm_after);
                    if outcome == Applied::OutOfOrder {
                        continue;
                    }
                    let before = t.entity_id.clone();
                    resolve(&self.attrs, t);
                    // A track gaining or losing its card is worth publishing now.
                    let urgent = outcome == Applied::Significant || before != t.entity_id;
                    (t.clone(), urgent)
                }
                None => {
                    let mut t = SystemTrack::from_first_observation(uid, obs);
                    resolve(&self.attrs, &mut t);
                    self.tracks.insert(uid, t.clone());
                    (t, true)
                }
            };
            self.redis.put_system_track(&track, urgent).await?;
        }
        for (source, ids) in acks {
            self.redis.ack_observations(&source, GROUP, &ids).await?;
        }
        Ok(())
    }

    async fn reap(&mut self) -> anyhow::Result<()> {
        let now = Utc::now();
        let mut dropped = Vec::new();
        for track in self.tracks.values_mut() {
            match lifecycle(track, now, &self.settings) {
                Lifecycle::Keep => {}
                Lifecycle::Lost => {
                    track.state = TrackState::Lost;
                    resolve(&self.attrs, track);
                    self.redis.put_system_track(track, true).await?;
                }
                Lifecycle::Drop => dropped.push(track.uid),
            }
        }
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
            self.tracks.remove(&uid);
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
}
