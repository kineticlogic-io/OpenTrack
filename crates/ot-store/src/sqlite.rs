//! The SQLite store: one file in WAL mode, migrated on open.

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use ot_core::{SiteCode, Uid};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

use crate::graph::{self, EdgeKind, NodeKind};

/// Ordered migrations. Append only; never edit a released one.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_registry.sql"),
    include_str!("../migrations/0003_schema_seed.sql"),
];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Redis(redis::RedisError),
    #[error(transparent)]
    Uid(#[from] ot_core::UidError),
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    TooNew { found: i64, supported: i64 },
    #[error("UID sequence for site {0} is exhausted")]
    UidExhausted(String),
    #[error("{0}")]
    Conflict(String),
    #[error("not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// A decision about to be recorded in the audit log.
#[derive(Debug, Clone)]
pub struct Decision {
    pub actor: String,
    pub op: String,
    pub reason: Option<String>,
    pub evidence: Value,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

impl Decision {
    pub fn new(actor: impl Into<String>, op: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            op: op.into(),
            reason: None,
            evidence: Value::Object(Default::default()),
            before: None,
            after: None,
        }
    }

    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    pub fn evidence(mut self, evidence: Value) -> Self {
        self.evidence = evidence;
        self
    }
}

pub struct Db {
    conn: Connection,
}

impl Db {
    /// Open (creating if needed) and migrate the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    /// An in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let mut db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&mut self) -> Result<()> {
        let current: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        let supported = MIGRATIONS.len() as i64;
        if current > supported {
            return Err(StoreError::TooNew {
                found: current,
                supported,
            });
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
            tracing::info!(version = i + 1, "applied SQLite migration");
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Run `f` in an immediate (write-locked) transaction.
    pub fn write<T>(&mut self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Create a system track for a source track: allocate a UID, record the
    /// decision and link the source track to it, in one transaction.
    pub fn create_system_track(
        &mut self,
        site: SiteCode,
        source_id: &str,
        source_track_key: &str,
        decision: Decision,
    ) -> Result<(Uid, i64)> {
        self.write(|tx| {
            let now = now_ms();
            let uid = allocate_uid(tx, site)?;
            let decision_id = record_decision(tx, &decision, now)?;
            let sys = graph::upsert_node(tx, NodeKind::SystemTrack, &uid.to_string(), now)?;
            let src = graph::upsert_node(
                tx,
                NodeKind::SourceTrack,
                &graph::source_track_key(source_id, source_track_key),
                now,
            )?;
            graph::add_edge(
                tx,
                EdgeKind::ReportsFor,
                src,
                sys,
                decision_id,
                &serde_json::json!({ "pairing": "auto", "confidence": 1.0 }),
                now,
            )?;
            Ok((uid, decision_id))
        })
    }

    /// Retire a system track: record the decision, close every live edge
    /// touching it and mark the node retired. Returns the decision id.
    pub fn retire_system_track(&mut self, uid: Uid, decision: Decision) -> Result<i64> {
        self.write(|tx| {
            let now = now_ms();
            let node: i64 = tx
                .query_row(
                    "SELECT id FROM nodes WHERE kind = 'system_track' AND key = ?1 AND retired_at_ms IS NULL",
                    [uid.to_string()],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("live system track {uid}")))?;
            let decision_id = record_decision(tx, &decision, now)?;
            tx.execute(
                "UPDATE edges SET valid_to_ms = max(?2, valid_from_ms), ended_by = ?3
                 WHERE (src = ?1 OR dst = ?1) AND valid_to_ms IS NULL",
                params![node, now, decision_id],
            )?;
            tx.execute(
                "UPDATE nodes SET retired_at_ms = ?2 WHERE id = ?1",
                params![node, now],
            )?;
            Ok(decision_id)
        })
    }

    /// Every live source-track → system-track link, as
    /// (`<source>/<key>`, UID). The engine warms its map from this.
    pub fn live_reports(&self) -> Result<Vec<(String, Uid)>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.key, d.key FROM edges e
             JOIN nodes s ON s.id = e.src JOIN nodes d ON d.id = e.dst
             WHERE e.kind = 'REPORTS_FOR' AND e.valid_to_ms IS NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| {
            let (src, uid) = r?;
            Ok((src, uid.parse()?))
        })
        .collect()
    }

    /// Every edge that ever touched a system track, with the decisions that
    /// opened and closed it: the answer to "why is this one track?".
    pub fn explain(&self, uid: Uid) -> Result<Vec<graph::EdgeRecord>> {
        graph::explain_system_track(&self.conn, &uid.to_string())
    }

    pub fn decision(&self, id: i64) -> Result<Option<Value>> {
        self.conn
            .query_row(
                "SELECT json_object('id', id, 'at_ms', at_ms, 'actor', actor, 'op', op,
                        'reason', reason, 'evidence', json(evidence))
                 FROM decisions WHERE id = ?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|s| serde_json::from_str(&s).map_err(StoreError::from))
            .transpose()
    }
}

/// Allocate the next UID for `site`.
pub fn allocate_uid(tx: &Transaction<'_>, site: SiteCode) -> Result<Uid> {
    let seq: i64 = tx.query_row(
        "INSERT INTO uid_sequences (site, next_sequence) VALUES (?1, 2)
         ON CONFLICT(site) DO UPDATE SET next_sequence = next_sequence + 1
         RETURNING next_sequence - 1",
        [site.as_str()],
        |r| r.get(0),
    )?;
    Uid::new(site, seq as u64).map_err(|_| StoreError::UidExhausted(site.to_string()))
}

pub fn record_decision(tx: &Transaction<'_>, d: &Decision, at_ms: i64) -> Result<i64> {
    tx.execute(
        "INSERT INTO decisions (at_ms, actor, op, reason, evidence, before, after)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            at_ms,
            d.actor,
            d.op,
            d.reason,
            d.evidence.to_string(),
            d.before.as_ref().map(Value::to_string),
            d.after.as_ref().map(Value::to_string),
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

pub fn from_ms(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_once_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ot.db");
        assert_eq!(Db::open(&path).unwrap().schema_version().unwrap(), 3);
        // Re-opening applies nothing and keeps the version.
        assert_eq!(Db::open(&path).unwrap().schema_version().unwrap(), 3);
    }

    #[test]
    fn refuses_a_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ot.db");
        drop(Db::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        assert!(matches!(
            Db::open(&path),
            Err(StoreError::TooNew { found: 99, .. })
        ));
    }

    #[test]
    fn uids_are_sequential_per_site() {
        let mut db = Db::open_in_memory().unwrap();
        let a = SiteCode::new("OTK").unwrap();
        let b = SiteCode::new("SD1").unwrap();
        let got: Vec<String> = [a, a, b, a]
            .into_iter()
            .map(|s| db.write(|tx| allocate_uid(tx, s)).unwrap().to_string())
            .collect();
        assert_eq!(
            got,
            [
                "OTK000000001",
                "OTK000000002",
                "SD1000000001",
                "OTK000000003"
            ]
        );
    }

    #[test]
    fn create_system_track_links_source_and_records_decision() {
        let mut db = Db::open_in_memory().unwrap();
        let site = SiteCode::new("OTK").unwrap();
        let (uid, decision_id) = db
            .create_system_track(
                site,
                "synthetic",
                "T1",
                Decision::new("system", "create_system_track").reason("test"),
            )
            .unwrap();
        assert_eq!(uid.to_string(), "OTK000000001");

        let d = db.decision(decision_id).unwrap().unwrap();
        assert_eq!(d["op"], "create_system_track");
        assert_eq!(d["reason"], "test");

        let why = db.explain(uid).unwrap();
        assert_eq!(why.len(), 1);
        assert_eq!(why[0].kind, EdgeKind::ReportsFor);
        assert_eq!(why[0].src_key, "synthetic/T1");
        assert_eq!(why[0].decision_id, decision_id);
        assert!(why[0].valid_to_ms.is_none());
    }

    #[test]
    fn retiring_closes_live_edges_once() {
        let mut db = Db::open_in_memory().unwrap();
        let site = SiteCode::new("OTK").unwrap();
        let (uid, _) = db
            .create_system_track(
                site,
                "s",
                "k",
                Decision::new("system", "create_system_track"),
            )
            .unwrap();
        let d = db
            .retire_system_track(uid, Decision::new("op:test", "retire"))
            .unwrap();
        let why = db.explain(uid).unwrap();
        assert_eq!(why[0].ended_by, Some(d));
        assert!(why[0].valid_to_ms.is_some());
        assert!(matches!(
            db.retire_system_track(uid, Decision::new("op:test", "retire")),
            Err(StoreError::NotFound(_))
        ));
        // The source track is free to report for a new system track.
        db.create_system_track(
            site,
            "s",
            "k",
            Decision::new("system", "create_system_track"),
        )
        .unwrap();
    }

    #[test]
    fn a_source_track_reports_for_one_system_track_at_a_time() {
        let mut db = Db::open_in_memory().unwrap();
        let site = SiteCode::new("OTK").unwrap();
        db.create_system_track(
            site,
            "s",
            "k",
            Decision::new("system", "create_system_track"),
        )
        .unwrap();
        let err = db
            .create_system_track(
                site,
                "s",
                "k",
                Decision::new("system", "create_system_track"),
            )
            .unwrap_err();
        assert!(matches!(err, StoreError::Sqlite(_)), "{err}");
        // The failed transaction rolled back its UID allocation too.
        let next = db.write(|tx| allocate_uid(tx, site)).unwrap().to_string();
        assert_eq!(next, "OTK000000002");
    }

    #[test]
    fn raw_output_requires_recorded_consent() {
        let db = Db::open_in_memory().unwrap();
        let err = db.connection().execute(
            "INSERT INTO sources (id, name, transport, codec, raw_collection, created_at_ms, updated_at_ms)
             VALUES ('ais', 'AIS', 'websocket', 'json', 'tracks_raw_ais', 0, 0)",
            [],
        );
        assert!(err.is_err());
    }
}
