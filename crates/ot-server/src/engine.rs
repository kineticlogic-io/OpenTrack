//! The engine role.
//!
//! Phase 1 engine: every source track becomes its own system track (1:1), so
//! the pipeline is proven before correlation arrives in phase 3. It already
//! owns what correlation will build on: system track creation through the
//! track graph, the lifecycle (tentative → confirmed → lost → dropped) with
//! per-domain stale times, and publishing through the outbox.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ot_core::{Domain, Observation, SystemTrack, TrackState, Uid};
use ot_store::{Decision, RedisStore};

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

pub struct Engine {
    common: Common,
    settings: EngineSettings,
    redis: RedisStore,
    /// `<source>/<key>` → system track.
    reports: HashMap<String, Uid>,
    tracks: HashMap<Uid, SystemTrack>,
    sources: Vec<String>,
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
        })
    }

    pub async fn run(
        mut self,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> anyhow::Result<()> {
        tokio::pin!(shutdown);
        // Re-read anything delivered to this consumer before a restart.
        self.refresh_sources().await?;
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
                    (t.clone(), outcome == Applied::Significant)
                }
                None => {
                    let t = SystemTrack::from_first_observation(uid, obs);
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
        for uid in dropped {
            self.redis
                .retire_system_track(uid, &self.common.tracks_collection)
                .await?;
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
}
