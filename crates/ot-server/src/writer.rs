//! The peat writer role: drains the Redis outbox into peat-node.
//!
//! Writes are coalesced per system track: at most one per minimum interval
//! (default 5 s), immediately when the producer marks an update urgent
//! (identity or classification changed). Outbox entries are acknowledged only
//! once their write has landed, so a crash re-delivers them.

use std::collections::HashMap;
use std::time::Duration;

use ot_core::Uid;
use ot_core::peat::{PublishContext, to_document};
use ot_peat::DocumentSink;
use ot_store::{OutboxOp, RedisStore};
use tokio::time::Instant;

/// Consumer group every writer instance joins.
pub const GROUP: &str = "peat-writer";

/// Metrics bucket the writer counts under.
const METRICS_SOURCE: &str = "_writer";

/// Attempts a background tombstone makes before leaving its entry pending
/// (it is then re-read when the writer next starts).
const TOMBSTONE_ATTEMPTS: u32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TombstoneOutcome {
    Deleted,
    Rejected,
    GaveUp,
}

#[derive(Debug, Clone)]
pub struct WriterSettings {
    pub consumer: String,
    pub collection: String,
    pub min_interval: Duration,
    /// Delay before retrying a write that failed transiently.
    pub retry_after: Duration,
    pub batch: usize,
}

/// Per-track write scheduling, kept free of I/O so it can be tested alone.
#[derive(Debug, Default)]
pub struct Schedule {
    min_interval: Duration,
    last_written: HashMap<Uid, Instant>,
    /// Tracks waiting to be written: when, and which outbox entries that
    /// write will acknowledge.
    waiting: HashMap<Uid, (Instant, Vec<String>)>,
}

impl Schedule {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            ..Default::default()
        }
    }

    /// Record an outbox entry for `uid`.
    pub fn offer(&mut self, uid: Uid, entry_id: String, urgent: bool, now: Instant) {
        let earliest = match (urgent, self.last_written.get(&uid)) {
            (false, Some(last)) => (*last + self.min_interval).max(now),
            _ => now,
        };
        let slot = self.waiting.entry(uid).or_insert((earliest, Vec::new()));
        slot.0 = slot.0.min(earliest);
        slot.1.push(entry_id);
    }

    /// Take every track whose write is due.
    pub fn take_due(&mut self, now: Instant) -> Vec<(Uid, Vec<String>)> {
        let due: Vec<Uid> = self
            .waiting
            .iter()
            .filter(|(_, (at, _))| *at <= now)
            .map(|(uid, _)| *uid)
            .collect();
        due.into_iter()
            .filter_map(|uid| self.waiting.remove(&uid).map(|(_, ids)| (uid, ids)))
            .collect()
    }

    pub fn written(&mut self, uid: Uid, now: Instant) {
        self.last_written.insert(uid, now);
    }

    /// Put a failed write back, to be retried at `at`.
    pub fn retry(&mut self, uid: Uid, ids: Vec<String>, at: Instant) {
        let slot = self.waiting.entry(uid).or_insert((at, Vec::new()));
        slot.0 = slot.0.min(at);
        slot.1.extend(ids);
    }

    pub fn next_due(&self) -> Option<Instant> {
        self.waiting.values().map(|(at, _)| *at).min()
    }

    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// Forget write times older than the interval; they no longer delay anything.
    pub fn prune(&mut self, now: Instant) {
        let horizon = self.min_interval;
        self.last_written
            .retain(|_, at| now.saturating_duration_since(*at) < horizon);
    }
}

/// Outcome counters, for logs and tests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub written: u64,
    pub deleted: u64,
    pub rejected: u64,
    pub retried: u64,
    pub skipped: u64,
}

pub struct Writer<S> {
    redis: RedisStore,
    sink: S,
    ctx: PublishContext,
    settings: WriterSettings,
    schedule: Schedule,
    /// Deletes in flight. They run beside the publish loop because peat-node
    /// can take a full RPC timeout to settle one (see `ot_peat` delete).
    tombstones: tokio::task::JoinSet<(String, String, TombstoneOutcome)>,
    pub tally: Tally,
}

impl<S: DocumentSink + Clone + 'static> Writer<S> {
    pub fn new(redis: RedisStore, sink: S, ctx: PublishContext, settings: WriterSettings) -> Self {
        Self {
            schedule: Schedule::new(settings.min_interval),
            tombstones: tokio::task::JoinSet::new(),
            redis,
            sink,
            ctx,
            settings,
            tally: Tally::default(),
        }
    }

    /// Run until `shutdown` resolves.
    pub async fn run(
        mut self,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> anyhow::Result<Tally> {
        self.redis.ensure_outbox_group(GROUP).await?;
        tracing::info!(consumer = %self.settings.consumer, collection = %self.settings.collection, "peat writer started");
        // First, anything this consumer was handed before a restart.
        self.pump(true).await?;
        tokio::pin!(shutdown);
        let mut backoff = Duration::from_millis(500);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                res = self.pump(false) => match res {
                    Ok(()) => backoff = Duration::from_millis(500),
                    Err(e) => {
                        // Redis hiccups must not kill the writer; unacknowledged
                        // entries are simply read again.
                        tracing::warn!(error = %e, ?backoff, "writer cycle failed; retrying");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(10));
                    }
                },
            }
        }
        // Deferred entries stay unacknowledged and are re-delivered on restart.
        tracing::info!(tally = ?self.tally, waiting = self.schedule.waiting(),
            tombstones_in_flight = self.tombstones_in_flight(), "peat writer stopped");
        Ok(self.tally)
    }

    /// One read-and-flush cycle. With `pending`, drain this consumer's
    /// unacknowledged entries instead of waiting for new ones.
    pub async fn pump(&mut self, pending: bool) -> anyhow::Result<()> {
        let now = Instant::now();
        let block = self
            .schedule
            .next_due()
            .map(|at| at.saturating_duration_since(now))
            .unwrap_or(Duration::from_secs(1))
            .clamp(
                Duration::from_millis(1),
                if self.tombstones.is_empty() {
                    Duration::from_secs(1)
                } else {
                    Duration::from_millis(200)
                },
            );
        let entries = self
            .redis
            .read_outbox(
                GROUP,
                &self.settings.consumer,
                self.settings.batch,
                block,
                pending,
            )
            .await?;
        let now = Instant::now();
        for entry in entries {
            match entry.op {
                OutboxOp::Publish { uid, urgent } => {
                    self.schedule.offer(uid, entry.id, urgent, now)
                }
                OutboxOp::Tombstone { collection, doc_id } => {
                    let sink = self.sink.clone();
                    let retry_after = self.settings.retry_after;
                    self.tombstones.spawn(async move {
                        let outcome = tombstone(&sink, &collection, &doc_id, retry_after).await;
                        (entry.id, doc_id, outcome)
                    });
                }
            }
        }
        self.reap_tombstones().await?;
        self.flush().await
    }

    /// Acknowledge finished tombstones without waiting for running ones.
    async fn reap_tombstones(&mut self) -> anyhow::Result<()> {
        while let Some(done) = self.tombstones.try_join_next() {
            let (id, doc_id, outcome) = done?;
            match outcome {
                TombstoneOutcome::Deleted => self.tally.deleted += 1,
                TombstoneOutcome::Rejected => self.tally.rejected += 1,
                TombstoneOutcome::GaveUp => {
                    // Stays pending; re-read on the next start.
                    tracing::warn!(%doc_id, "tombstone still failing; left pending");
                    self.tally.retried += 1;
                    continue;
                }
            }
            self.redis.ack_outbox(GROUP, &[id]).await?;
        }
        Ok(())
    }

    /// Tombstones still running.
    pub fn tombstones_in_flight(&self) -> usize {
        self.tombstones.len()
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        let now = Instant::now();
        let (mut written, mut failed) = (0, 0);
        for (uid, ids) in self.schedule.take_due(now) {
            let Some(track) = self.redis.get_system_track(uid).await? else {
                // Retired since it was queued; its tombstone is in the outbox.
                self.tally.skipped += 1;
                self.redis.ack_outbox(GROUP, &ids).await?;
                continue;
            };
            let doc = to_document(&track, &self.ctx).to_string();
            match self
                .sink
                .put(&self.settings.collection, &uid.doc_id(), &doc)
                .await
            {
                Ok(()) => {
                    self.schedule.written(uid, now);
                    self.tally.written += 1;
                    written += 1;
                    self.redis.ack_outbox(GROUP, &ids).await?;
                }
                Err(e) if e.is_transient() => {
                    tracing::warn!(%e, %uid, "write failed, will retry");
                    self.tally.retried += 1;
                    failed += 1;
                    self.schedule
                        .retry(uid, ids, now + self.settings.retry_after);
                }
                Err(e) => {
                    tracing::error!(%e, %uid, "peat-node rejected track document");
                    self.tally.rejected += 1;
                    failed += 1;
                    self.redis.ack_outbox(GROUP, &ids).await?;
                }
            }
        }
        if written > 0 {
            self.redis
                .incr_metric(METRICS_SOURCE, "written", written)
                .await?;
        }
        if failed > 0 {
            self.redis
                .incr_metric(METRICS_SOURCE, "write_error", failed)
                .await?;
        }
        self.schedule.prune(now);
        Ok(())
    }
}

/// Delete one document, retrying transient failures in the background.
async fn tombstone<S: DocumentSink>(
    sink: &S,
    collection: &str,
    doc_id: &str,
    retry_after: Duration,
) -> TombstoneOutcome {
    for attempt in 1..=TOMBSTONE_ATTEMPTS {
        match sink.delete(collection, doc_id).await {
            Ok(()) => return TombstoneOutcome::Deleted,
            Err(e) if e.is_transient() => {
                tracing::warn!(%e, %doc_id, attempt, "tombstone failed, will retry");
                tokio::time::sleep(retry_after).await;
            }
            Err(e) => {
                tracing::error!(%e, %doc_id, "tombstone rejected");
                return TombstoneOutcome::Rejected;
            }
        }
    }
    TombstoneOutcome::GaveUp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid(n: u64) -> Uid {
        Uid::new(ot_core::SiteCode::new("OTK").unwrap(), n).unwrap()
    }

    #[test]
    fn first_write_is_immediate_then_rate_limited() {
        let t0 = Instant::now();
        let mut s = Schedule::new(Duration::from_secs(5));
        s.offer(uid(1), "1-0".into(), false, t0);
        assert_eq!(s.take_due(t0), vec![(uid(1), vec!["1-0".to_string()])]);
        s.written(uid(1), t0);

        // Two more updates within the interval coalesce into one write at t0+5s.
        s.offer(uid(1), "2-0".into(), false, t0 + Duration::from_secs(1));
        s.offer(uid(1), "3-0".into(), false, t0 + Duration::from_secs(2));
        assert!(s.take_due(t0 + Duration::from_secs(4)).is_empty());
        assert_eq!(s.next_due(), Some(t0 + Duration::from_secs(5)));
        assert_eq!(
            s.take_due(t0 + Duration::from_secs(5)),
            vec![(uid(1), vec!["2-0".to_string(), "3-0".to_string()])]
        );
    }

    #[test]
    fn urgent_updates_skip_the_interval() {
        let t0 = Instant::now();
        let mut s = Schedule::new(Duration::from_secs(5));
        s.written(uid(1), t0);
        s.offer(uid(1), "1-0".into(), false, t0 + Duration::from_secs(1));
        s.offer(uid(1), "2-0".into(), true, t0 + Duration::from_secs(2));
        let due = s.take_due(t0 + Duration::from_secs(2));
        assert_eq!(
            due,
            vec![(uid(1), vec!["1-0".to_string(), "2-0".to_string()])]
        );
    }

    #[test]
    fn retries_keep_their_entries_and_prune_forgets_old_writes() {
        let t0 = Instant::now();
        let mut s = Schedule::new(Duration::from_secs(5));
        s.retry(uid(2), vec!["9-0".into()], t0 + Duration::from_secs(5));
        assert!(s.take_due(t0).is_empty());
        assert_eq!(s.take_due(t0 + Duration::from_secs(5)).len(), 1);

        s.written(uid(3), t0);
        s.prune(t0 + Duration::from_secs(6));
        // uid(3) is no longer rate-limited.
        s.offer(uid(3), "1-0".into(), false, t0 + Duration::from_secs(6));
        assert_eq!(s.take_due(t0 + Duration::from_secs(6)).len(), 1);
    }

    /// End to end against a real Redis (set `OT_TEST_REDIS_URL`), with a fake
    /// peat-node. Runs in its own key namespace and cleans up after itself.
    mod redis_e2e {
        use super::*;
        use ot_core::SystemTrack;
        use ot_peat::PeatError;
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct FakeSink {
            puts: Arc<Mutex<Vec<(String, String, String)>>>,
            deletes: Arc<Mutex<Vec<(String, String)>>>,
        }

        impl DocumentSink for FakeSink {
            async fn put(&self, c: &str, id: &str, json: &str) -> Result<(), PeatError> {
                self.puts
                    .lock()
                    .unwrap()
                    .push((c.into(), id.into(), json.into()));
                Ok(())
            }
            async fn delete(&self, c: &str, id: &str) -> Result<(), PeatError> {
                self.deletes.lock().unwrap().push((c.into(), id.into()));
                Ok(())
            }
        }

        async fn store() -> Option<RedisStore> {
            let url = std::env::var("OT_TEST_REDIS_URL").ok()?;
            let ns = format!(
                "ot-test-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_micros()
            );
            Some(
                RedisStore::connect(&url, ot_store::Keys::new(ns))
                    .await
                    .unwrap(),
            )
        }

        fn track(n: u64, lat: f64) -> SystemTrack {
            let now = chrono::Utc::now();
            let obs: ot_core::Observation = serde_json::from_value(serde_json::json!({
                "schema_version": 1, "source_id": "t", "source_track_key": format!("k{n}"),
                "observed_at": now, "received_at": now,
                "position": { "latitude": lat, "longitude": -117.0 }
            }))
            .unwrap();
            SystemTrack::from_first_observation(uid(n), obs)
        }

        #[tokio::test]
        async fn outbox_to_sink_coalesces_and_tombstones() {
            let Some(redis) = store().await else {
                eprintln!("skipped: OT_TEST_REDIS_URL not set");
                return;
            };
            let sink = FakeSink::default();
            let ctx = PublishContext {
                node_id: "opentrack-OTK".into(),
                model_version: "test".into(),
            };
            let settings = WriterSettings {
                consumer: "c1".into(),
                collection: "tracks".into(),
                min_interval: Duration::from_secs(60),
                retry_after: Duration::from_secs(1),
                batch: 100,
            };
            let mut w = Writer::new(redis.clone(), sink.clone(), ctx, settings);
            redis.ensure_outbox_group(GROUP).await.unwrap();

            // First update writes at once; the next two coalesce behind the interval.
            redis
                .put_system_track(&track(1, 32.0), false)
                .await
                .unwrap();
            w.pump(false).await.unwrap();
            redis
                .put_system_track(&track(1, 32.1), false)
                .await
                .unwrap();
            redis
                .put_system_track(&track(1, 32.2), false)
                .await
                .unwrap();
            w.pump(false).await.unwrap();
            assert_eq!(sink.puts.lock().unwrap().len(), 1);

            // An urgent update goes straight out with the latest state.
            redis.put_system_track(&track(1, 32.3), true).await.unwrap();
            w.pump(false).await.unwrap();
            {
                let puts = sink.puts.lock().unwrap();
                assert_eq!(puts.len(), 2);
                assert_eq!(puts[1].1, "tms-OTK000000001");
                let doc: serde_json::Value = serde_json::from_str(&puts[1].2).unwrap();
                assert_eq!(doc["position"]["latitude"], 32.3);
            }

            // Retiring tombstones the document; nothing is left unacknowledged.
            redis.retire_system_track(uid(1), "tracks").await.unwrap();
            w.pump(false).await.unwrap();
            // Tombstones run in the background; later cycles acknowledge them.
            for _ in 0..50 {
                if w.tombstones_in_flight() == 0 && w.tally.deleted == 1 {
                    break;
                }
                w.pump(false).await.unwrap();
            }
            assert_eq!(
                sink.deletes.lock().unwrap().as_slice(),
                [("tracks".to_string(), "tms-OTK000000001".to_string())]
            );
            assert!(
                redis
                    .read_outbox(GROUP, "c1", 100, Duration::from_millis(1), true)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(w.tally.written, 2);
            assert_eq!(w.tally.deleted, 1);

            redis.purge_namespace().await.unwrap();
        }
    }
}
