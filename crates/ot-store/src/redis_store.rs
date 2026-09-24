//! Hot state in Redis: observation streams, live track state and the outbox
//! the peat writer consumes.

use std::time::Duration;

use chrono::Utc;
use ot_core::{Observation, SystemTrack, Uid};
use redis::aio::ConnectionManager;
use redis::streams::StreamReadReply;

use crate::keys::Keys;
use crate::sqlite::{Result, StoreError};

/// Work item for the peat writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboxOp {
    /// Publish the current state of a system track (read from `sys:<uid>`
    /// at publish time, so bursts of updates coalesce naturally). `urgent`
    /// bypasses the writer's minimum interval, for identity and
    /// classification changes.
    Publish { uid: Uid, urgent: bool },
    /// Delete a document on peat-node (retired or merged-away track).
    Tombstone { collection: String, doc_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    pub id: String,
    pub op: OutboxOp,
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

    pub async fn get_system_track(&self, uid: Uid) -> Result<Option<SystemTrack>> {
        let raw: Option<String> = redis::cmd("GET")
            .arg(self.keys.system_track(&uid.to_string()))
            .query_async(&mut self.conn.clone())
            .await?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }

    /// Remove a system track's live state and queue its tombstone.
    pub async fn retire_system_track(&self, uid: Uid, collection: &str) -> Result<()> {
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
            .arg("collection")
            .arg(collection)
            .arg("doc_id")
            .arg(uid.doc_id())
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
                let op = match field("op").as_deref() {
                    Some("publish") => {
                        field("uid")
                            .and_then(|u| u.parse().ok())
                            .map(|uid| OutboxOp::Publish {
                                uid,
                                urgent: field("urgent").as_deref() == Some("1"),
                            })
                    }
                    Some("tombstone") => match (field("collection"), field("doc_id")) {
                        (Some(collection), Some(doc_id)) => {
                            Some(OutboxOp::Tombstone { collection, doc_id })
                        }
                        _ => None,
                    },
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
