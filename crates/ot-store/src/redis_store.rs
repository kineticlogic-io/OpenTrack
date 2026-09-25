//! Hot state in Redis: observation streams, live track state and the outbox
//! the track writer consumes.

use std::time::Duration;

use chrono::Utc;
use ot_core::{Observation, SystemTrack, Uid};
use redis::aio::ConnectionManager;
use redis::streams::StreamReadReply;

use crate::keys::Keys;
use crate::sqlite::{Result, StoreError};

/// The error of an observation stream entry trimmed (by the stream's length
/// cap) before its consumer acknowledged it: nothing to read, only to ack.
pub const TRIMMED: &str = "trimmed before it was read";

/// Work item for the track writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboxOp {
    /// Publish the current state of a system track (read from `sys:<uid>`
    /// at publish time, so bursts of updates coalesce naturally). `urgent`
    /// bypasses the writer's minimum interval, for identity and
    /// classification changes.
    Publish { uid: Uid, urgent: bool },
    /// Publish a delete for a retired (dropped, merged-away or deleted) track.
    Tombstone { uid: Uid, reason: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    pub id: String,
    pub op: OutboxOp,
}

/// Backlog of a stream consumer group.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct GroupBacklog {
    /// Delivered, not yet acknowledged.
    pub pending: u64,
    /// Not yet delivered.
    pub lag: u64,
}

#[derive(Clone)]
pub struct RedisStore {
    conn: ConnectionManager,
    keys: Keys,
    /// How much of each observation stream to keep.
    pub obs_window: Duration,
    /// Approximate cap on the outbox stream.
    pub outbox_maxlen: usize,
}

impl RedisStore {
    pub async fn connect(url: &str, keys: Keys) -> Result<Self> {
        let client = redis::Client::open(url)?;
        // The default response timeout is shorter than the writer's blocking
        // XREADGROUP, which would then fail every idle poll.
        let config = redis::aio::ConnectionManagerConfig::new()
            .set_response_timeout(Some(Duration::from_secs(15)));
        let conn = ConnectionManager::new_with_config(client, config).await?;
        Ok(Self {
            conn,
            keys,
            obs_window: Duration::from_secs(600),
            outbox_maxlen: 100_000,
        })
    }

    pub fn keys(&self) -> &Keys {
        &self.keys
    }

    pub async fn ping(&self) -> Result<()> {
        let _: String = redis::cmd("PING")
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    /// Append a mapped observation to its source's stream, trimming entries
    /// older than the window.
    pub async fn append_observation(&self, obs: &Observation) -> Result<String> {
        let min_id = Utc::now().timestamp_millis() - self.obs_window.as_millis() as i64;
        Ok(redis::cmd("XADD")
            .arg(self.keys.obs_stream(&obs.source_id))
            .arg("MINID")
            .arg("~")
            .arg(min_id.max(0))
            .arg("*")
            .arg("obs")
            .arg(serde_json::to_string(obs)?)
            .query_async(&mut self.conn.clone())
            .await?)
    }

    /// Store a system track's current state and queue it for publishing, in
    /// one MULTI so the writer never sees a queued UID without its state.
    pub async fn put_system_track(&self, track: &SystemTrack, urgent: bool) -> Result<()> {
        let uid = track.uid.to_string();
        redis::pipe()
            .atomic()
            .set(self.keys.system_track(&uid), serde_json::to_string(track)?)
            .ignore()
            .cmd("XADD")
            .arg(self.keys.outbox())
            .arg("MAXLEN")
            .arg("~")
            .arg(self.outbox_maxlen)
            .arg("*")
            .arg("op")
            .arg("publish")
            .arg("uid")
            .arg(&uid)
            .arg("urgent")
            .arg(u8::from(urgent))
            .ignore()
            .query_async::<()>(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    /// Store a system track's state without publishing it (a track not yet
    /// authoritative enough to leave OpenTrack).
    pub async fn put_system_track_quietly(&self, track: &SystemTrack) -> Result<()> {
        redis::cmd("SET")
            .arg(self.keys.system_track(&track.uid.to_string()))
            .arg(serde_json::to_string(track)?)
            .query_async::<()>(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    /// Queue a command for the engine.
    pub async fn push_command(&self, command: &serde_json::Value) -> Result<()> {
        redis::cmd("LPUSH")
            .arg(self.keys.commands())
            .arg(command.to_string())
            .query_async::<()>(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    /// Take up to `n` queued commands, oldest first.
    pub async fn pop_commands(&self, n: usize) -> Result<Vec<serde_json::Value>> {
        let raw: Option<Vec<String>> = redis::cmd("RPOP")
            .arg(self.keys.commands())
            .arg(n)
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(raw
            .unwrap_or_default()
            .iter()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect())
    }

    /// The engine's answer to a command, kept for a minute.
    pub async fn put_command_result(&self, id: &str, result: &serde_json::Value) -> Result<()> {
        redis::cmd("SET")
            .arg(self.keys.command_result(id))
            .arg(result.to_string())
            .arg("EX")
            .arg(60)
            .query_async::<()>(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    pub async fn command_result(&self, id: &str) -> Result<Option<serde_json::Value>> {
        let raw: Option<String> = redis::cmd("GET")
            .arg(self.keys.command_result(id))
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
    }

    /// Remove a system track that was never published (no tombstone needed).
    pub async fn forget_system_track(&self, uid: Uid) -> Result<()> {
        redis::cmd("DEL")
            .arg(self.keys.system_track(&uid.to_string()))
            .query_async::<()>(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    pub async fn get_system_track(&self, uid: Uid) -> Result<Option<SystemTrack>> {
        let raw: Option<String> = redis::cmd("GET")
            .arg(self.keys.system_track(&uid.to_string()))
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    /// Remove a system track's live state and queue its tombstone.
    pub async fn retire_system_track(&self, uid: Uid, reason: &str) -> Result<()> {
        redis::pipe()
            .atomic()
            .del(self.keys.system_track(&uid.to_string()))
            .ignore()
            .cmd("XADD")
            .arg(self.keys.outbox())
            .arg("MAXLEN")
            .arg("~")
            .arg(self.outbox_maxlen)
            .arg("*")
            .arg("op")
            .arg("tombstone")
            .arg("uid")
            .arg(uid.to_string())
            .arg("reason")
            .arg(reason)
            .ignore()
            .query_async::<()>(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    /// Create a consumer group on the outbox if it does not exist yet.
    pub async fn ensure_outbox_group(&self, group: &str) -> Result<()> {
        let res: redis::RedisResult<String> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(self.keys.outbox())
            .arg(group)
            .arg("0")
            .arg("MKSTREAM")
            .query_async(&mut self.conn.clone())
            .await;
        match res {
            Ok(_) => Ok(()),
            Err(e) if e.code() == Some("BUSYGROUP") => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Read outbox entries for `consumer`. With `pending`, re-read this
    /// consumer's delivered-but-unacknowledged entries (after a restart);
    /// otherwise wait up to `block` for new ones.
    pub async fn read_outbox(
        &self,
        group: &str,
        consumer: &str,
        count: usize,
        block: Duration,
        pending: bool,
    ) -> Result<Vec<OutboxEntry>> {
        let mut cmd = redis::cmd("XREADGROUP");
        cmd.arg("GROUP")
            .arg(group)
            .arg(consumer)
            .arg("COUNT")
            .arg(count);
        if !pending {
            cmd.arg("BLOCK").arg(block.as_millis() as u64);
        }
        cmd.arg("STREAMS")
            .arg(self.keys.outbox())
            .arg(if pending { "0" } else { ">" });
        let reply: Option<StreamReadReply> = cmd.query_async(&mut self.conn.clone()).await?;
        let mut out = Vec::new();
        for key in reply.map(|r| r.keys).unwrap_or_default() {
            for entry in key.ids {
                let field = |name: &str| entry.get::<String>(name);
                let op =
                    match field("op").as_deref() {
                        Some("publish") => {
                            field("uid")
                                .and_then(|u| u.parse().ok())
                                .map(|uid| OutboxOp::Publish {
                                    uid,
                                    urgent: field("urgent").as_deref() == Some("1"),
                                })
                        }
                        Some("tombstone") => field("uid").and_then(|u| u.parse().ok()).map(|uid| {
                            OutboxOp::Tombstone {
                                uid,
                                reason: field("reason").filter(|r| !r.is_empty()),
                            }
                        }),
                        _ => None,
                    };
                match op {
                    Some(op) => out.push(OutboxEntry { id: entry.id, op }),
                    None => {
                        // Unreadable entries are acknowledged so they cannot
                        // wedge the group, and logged for investigation.
                        tracing::warn!(id = %entry.id, "dropping malformed outbox entry");
                        self.ack_outbox(group, std::slice::from_ref(&entry.id))
                            .await?;
                    }
                }
            }
        }
        Ok(out)
    }

    pub async fn ack_outbox(&self, group: &str, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let _: i64 = redis::cmd("XACK")
            .arg(self.keys.outbox())
            .arg(group)
            .arg(ids)
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    /// Add `n` to a per-source, per-minute counter (kept 7 days).
    pub async fn incr_metric(&self, source: &str, field: &str, n: i64) -> Result<()> {
        let minute = Utc::now().timestamp() / 60;
        let key = self.keys.metrics(source, minute);
        redis::pipe()
            .hincr(&key, field, n)
            .ignore()
            .expire(&key, 7 * 24 * 3600)
            .ignore()
            .query_async::<()>(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    /// Append a batch of observations (one pipeline, trimmed to the window).
    pub async fn append_observations(&self, source: &str, batch: &[Observation]) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let min_id = (Utc::now().timestamp_millis() - self.obs_window.as_millis() as i64).max(0);
        let stream = self.keys.obs_stream(source);
        let mut pipe = redis::pipe();
        for obs in batch {
            pipe.cmd("XADD")
                .arg(&stream)
                .arg("MINID")
                .arg("~")
                .arg(min_id)
                .arg("*")
                .arg("obs")
                .arg(serde_json::to_string(obs)?)
                .ignore();
        }
        pipe.query_async::<()>(&mut self.conn.clone()).await?;
        Ok(())
    }

    /// Create `group` on each source's observation stream if missing.
    pub async fn ensure_obs_groups(&self, sources: &[String], group: &str) -> Result<()> {
        for source in sources {
            let res: redis::RedisResult<String> = redis::cmd("XGROUP")
                .arg("CREATE")
                .arg(self.keys.obs_stream(source))
                .arg(group)
                .arg("$")
                .arg("MKSTREAM")
                .query_async(&mut self.conn.clone())
                .await;
            match res {
                Ok(_) => {}
                Err(e) if e.code() == Some("BUSYGROUP") => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Read observations from several sources' streams. Entries that fail to
    /// parse come back as `Err` with their id so they can be acknowledged.
    #[allow(clippy::type_complexity)]
    pub async fn read_observations(
        &self,
        sources: &[String],
        group: &str,
        consumer: &str,
        count: usize,
        block: Duration,
        pending: bool,
    ) -> Result<Vec<(String, String, std::result::Result<Observation, String>)>> {
        if sources.is_empty() {
            tokio::time::sleep(block).await;
            return Ok(Vec::new());
        }
        let mut cmd = redis::cmd("XREADGROUP");
        cmd.arg("GROUP")
            .arg(group)
            .arg(consumer)
            .arg("COUNT")
            .arg(count);
        if !pending {
            cmd.arg("BLOCK").arg(block.as_millis() as u64);
        }
        cmd.arg("STREAMS");
        for source in sources {
            cmd.arg(self.keys.obs_stream(source));
        }
        for _ in sources {
            cmd.arg(if pending { "0" } else { ">" });
        }
        let reply: Option<StreamReadReply> = cmd.query_async(&mut self.conn.clone()).await?;
        let prefix = format!("{}:obs:", self.keys.namespace());
        let mut out = Vec::new();
        for key in reply.map(|r| r.keys).unwrap_or_default() {
            let source = key.key.strip_prefix(&prefix).unwrap_or(&key.key).to_owned();
            for entry in key.ids {
                // A pending entry the stream's length cap removed before it was
                // acknowledged comes back with no fields.
                if entry.map.is_empty() {
                    out.push((source.clone(), entry.id, Err(TRIMMED.to_owned())));
                    continue;
                }
                let obs = entry
                    .get::<String>("obs")
                    .ok_or_else(|| "missing obs field".to_owned())
                    .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()));
                out.push((source.clone(), entry.id, obs));
            }
        }
        Ok(out)
    }

    pub async fn ack_observations(&self, source: &str, group: &str, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let _: i64 = redis::cmd("XACK")
            .arg(self.keys.obs_stream(source))
            .arg(group)
            .arg(ids)
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    /// Current state of a source track, kept for `ttl`.
    pub async fn put_source_track(&self, obs: &Observation, ttl: Duration) -> Result<()> {
        let _: () = redis::cmd("SET")
            .arg(
                self.keys
                    .source_track(&obs.source_id, &obs.source_track_key),
            )
            .arg(serde_json::to_string(obs)?)
            .arg("EX")
            .arg(ttl.as_secs().max(1))
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    /// A source track's latest observation, if it has reported within its TTL.
    pub async fn get_source_track(&self, source: &str, key: &str) -> Result<Option<Observation>> {
        let raw: Option<String> = redis::cmd("GET")
            .arg(self.keys.source_track(source, key))
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(match raw {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        })
    }

    /// Every live system track (for the engine's warm start and the API).
    pub async fn list_system_tracks(&self) -> Result<Vec<SystemTrack>> {
        let keys = self
            .scan(&format!("{}:sys:*", self.keys.namespace()))
            .await?;
        let mut out = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(500) {
            let values: Vec<Option<String>> = redis::cmd("MGET")
                .arg(chunk)
                .query_async(&mut self.conn.clone())
                .await?;
            for v in values.into_iter().flatten() {
                match serde_json::from_str(&v) {
                    Ok(t) => out.push(t),
                    Err(e) => tracing::warn!(error = %e, "skipping unreadable system track"),
                }
            }
        }
        Ok(out)
    }

    /// Persist one static-cache entry (JSON) with a TTL.
    pub async fn put_static(
        &self,
        source: &str,
        key: &str,
        json: &str,
        ttl: Duration,
    ) -> Result<()> {
        let _: () = redis::cmd("SET")
            .arg(self.keys.static_cache(source, key))
            .arg(json)
            .arg("EX")
            .arg(ttl.as_secs().max(1))
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    /// Every cached static entry of a source, as (track key, JSON).
    pub async fn load_statics(&self, source: &str) -> Result<Vec<(String, String)>> {
        let keys = self.scan(&self.keys.static_cache_all(source)).await?;
        let prefix = self.keys.static_cache(source, "");
        let mut out = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(500) {
            let values: Vec<Option<String>> = redis::cmd("MGET")
                .arg(chunk)
                .query_async(&mut self.conn.clone())
                .await?;
            for (k, v) in chunk.iter().zip(values) {
                if let Some(v) = v {
                    out.push((k.strip_prefix(&prefix).unwrap_or(k).to_owned(), v));
                }
            }
        }
        Ok(out)
    }

    /// Publish a source worker's status (JSON), expiring if the worker stops.
    pub async fn put_source_status(&self, source: &str, json: &str, ttl: Duration) -> Result<()> {
        let _: () = redis::cmd("SET")
            .arg(self.keys.source_status(source))
            .arg(json)
            .arg("EX")
            .arg(ttl.as_secs().max(1))
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(())
    }

    pub async fn get_source_status(&self, source: &str) -> Result<Option<String>> {
        Ok(redis::cmd("GET")
            .arg(self.keys.source_status(source))
            .query_async(&mut self.conn.clone())
            .await?)
    }

    /// Add several counters to this minute's bucket for `source`.
    pub async fn incr_metrics(&self, source: &str, counts: &[(String, u64)]) -> Result<()> {
        if counts.is_empty() {
            return Ok(());
        }
        let minute = Utc::now().timestamp() / 60;
        let key = self.keys.metrics(source, minute);
        let mut pipe = redis::pipe();
        for (field, n) in counts {
            pipe.hincr(&key, field, *n).ignore();
        }
        pipe.expire(&key, 7 * 24 * 3600).ignore();
        pipe.query_async::<()>(&mut self.conn.clone()).await?;
        Ok(())
    }

    /// Per-minute counters for `source` over the last `minutes` minutes,
    /// oldest first, as (unix minute, counters).
    pub async fn metrics_range(
        &self,
        source: &str,
        minutes: i64,
    ) -> Result<Vec<(i64, std::collections::HashMap<String, u64>)>> {
        let now = Utc::now().timestamp() / 60;
        let mut pipe = redis::pipe();
        let range: Vec<i64> = (now - minutes + 1..=now).collect();
        for m in &range {
            pipe.hgetall(self.keys.metrics(source, *m));
        }
        let maps: Vec<std::collections::HashMap<String, u64>> =
            pipe.query_async(&mut self.conn.clone()).await?;
        Ok(range.into_iter().zip(maps).collect())
    }

    /// Set gauges (last value wins) in this minute's bucket for `source`,
    /// beside the counters, so charts get history for sampled values too.
    pub async fn set_gauges(&self, source: &str, gauges: &[(&str, u64)]) -> Result<()> {
        if gauges.is_empty() {
            return Ok(());
        }
        let minute = Utc::now().timestamp() / 60;
        let key = self.keys.metrics(source, minute);
        let mut pipe = redis::pipe();
        for (field, v) in gauges {
            pipe.hset(&key, *field, *v).ignore();
        }
        pipe.expire(&key, 7 * 24 * 3600).ignore();
        pipe.query_async::<()>(&mut self.conn.clone()).await?;
        Ok(())
    }

    /// Backlog of one consumer group: entries delivered but not acknowledged
    /// (`pending`) and entries not yet delivered (`lag`). Zeros when the
    /// stream or group does not exist.
    pub async fn group_backlog(&self, stream: &str, group: &str) -> Result<GroupBacklog> {
        let res: redis::RedisResult<Vec<std::collections::HashMap<String, redis::Value>>> =
            redis::cmd("XINFO")
                .arg("GROUPS")
                .arg(stream)
                .query_async(&mut self.conn.clone())
                .await;
        let groups = match res {
            Ok(g) => g,
            // `ERR no such key`: nothing has been written to the stream yet.
            Err(e) if e.code() == Some("ERR") => return Ok(GroupBacklog::default()),
            Err(e) => return Err(e.into()),
        };
        let int = |v: Option<&redis::Value>| match v {
            Some(redis::Value::Int(n)) => (*n).max(0) as u64,
            _ => 0,
        };
        let name_is = |v: Option<&redis::Value>| match v {
            Some(redis::Value::BulkString(b)) => b.as_slice() == group.as_bytes(),
            Some(redis::Value::SimpleString(s)) => s == group,
            _ => false,
        };
        Ok(groups
            .iter()
            .find(|g| name_is(g.get("name")))
            .map(|g| GroupBacklog {
                pending: int(g.get("pending")),
                lag: int(g.get("lag")),
            })
            .unwrap_or_default())
    }

    /// Backlog of the writer on the outbox.
    pub async fn outbox_backlog(&self, group: &str) -> Result<GroupBacklog> {
        self.group_backlog(&self.keys.outbox(), group).await
    }

    /// Backlog of the engine on a source's observation stream.
    pub async fn obs_backlog(&self, source: &str, group: &str) -> Result<GroupBacklog> {
        self.group_backlog(&self.keys.obs_stream(source), group)
            .await
    }

    /// Bytes Redis uses (`INFO memory` `used_memory`).
    pub async fn used_memory(&self) -> Result<u64> {
        let info: String = redis::cmd("INFO")
            .arg("memory")
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(info
            .lines()
            .find_map(|l| l.strip_prefix("used_memory:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0))
    }

    async fn scan(&self, pattern: &str) -> Result<Vec<String>> {
        let mut conn = self.conn.clone();
        let mut cursor: u64 = 0;
        let mut out = Vec::new();
        loop {
            let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(1000)
                .query_async(&mut conn)
                .await?;
            out.extend(batch);
            if next == 0 {
                return Ok(out);
            }
            cursor = next;
        }
    }

    /// Delete every key in this store's namespace. For tests and resets only.
    pub async fn purge_namespace(&self) -> Result<u64> {
        let mut conn = self.conn.clone();
        let mut cursor: u64 = 0;
        let mut deleted = 0;
        loop {
            let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(self.keys.all())
                .arg("COUNT")
                .arg(500)
                .query_async(&mut conn)
                .await?;
            if !batch.is_empty() {
                deleted += redis::cmd("DEL")
                    .arg(&batch)
                    .query_async::<u64>(&mut conn)
                    .await?;
            }
            if next == 0 {
                return Ok(deleted);
            }
            cursor = next;
        }
    }
}

impl From<redis::RedisError> for StoreError {
    fn from(e: redis::RedisError) -> Self {
        StoreError::Redis(e)
    }
}
