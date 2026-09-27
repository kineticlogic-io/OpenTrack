//! The replicated decision log (see `ot_sync`): every node's
//! track-management decisions, and which local decisions each one made.

use ot_core::SiteCode;
use ot_sync::{Entry, GlobalId, Hlc};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::sqlite::{Db, Result, now_ms};

/// Where an entry stands on this node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Applied,
    /// Names a track this node does not have yet.
    Pending,
    /// A later decision on the same tracks was applied first.
    Superseded,
    Failed,
}

impl SyncStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncStatus::Applied => "applied",
            SyncStatus::Pending => "pending",
            SyncStatus::Superseded => "superseded",
            SyncStatus::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "applied" => SyncStatus::Applied,
            "pending" => SyncStatus::Pending,
            "superseded" => SyncStatus::Superseded,
            _ => SyncStatus::Failed,
        }
    }
}

const COLUMNS: &str = "site, seq, hlc, actor, role, command";

fn entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    let site: String = r.get(0)?;
    let seq: i64 = r.get(1)?;
    let command: String = r.get(5)?;
    let conv = |e: String| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    };
    Ok(Entry {
        id: GlobalId {
            site: site
                .parse()
                .map_err(|e: ot_core::UidError| conv(e.to_string()))?,
            seq: seq as u64,
        },
        hlc: Hlc::from_i64(r.get(2)?),
        actor: r.get(3)?,
        role: r.get(4)?,
        command: serde_json::from_str(&command).map_err(|e| conv(e.to_string()))?,
    })
}

impl Db {
    /// The sequence this node's next entry gets.
    pub fn sync_next_seq(&self, site: SiteCode) -> Result<u64> {
        let max: Option<i64> = self.connection().query_row(
            "SELECT max(seq) FROM sync_log WHERE site = ?1",
            [site.as_str()],
            |r| r.get(0),
        )?;
        Ok(max.unwrap_or(0) as u64 + 1)
    }

    /// The latest stamp this node has given or seen, to start its clock after.
    pub fn sync_last_hlc(&self) -> Result<Hlc> {
        let max: Option<i64> =
            self.connection()
                .query_row("SELECT max(hlc) FROM sync_log", [], |r| r.get(0))?;
        Ok(Hlc::from_i64(max.unwrap_or(0)))
    }

    /// Keep an entry. False if it was here already (entries arrive twice).
    pub fn sync_record(
        &mut self,
        e: &Entry,
        status: SyncStatus,
        detail: Option<&str>,
    ) -> Result<bool> {
        let n = self.connection().execute(
            "INSERT OR IGNORE INTO sync_log
               (site, seq, hlc, actor, role, command, received_at_ms, status, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                e.id.site.as_str(),
                e.id.seq as i64,
                e.hlc.as_i64(),
                e.actor,
                e.role,
                e.command.to_string(),
                now_ms(),
                status.as_str(),
                detail,
            ],
        )?;
        Ok(n == 1)
    }

    pub fn sync_set_status(
        &mut self,
        id: GlobalId,
        status: SyncStatus,
        detail: Option<&str>,
    ) -> Result<()> {
        self.connection().execute(
            "UPDATE sync_log SET status = ?3, detail = ?4 WHERE site = ?1 AND seq = ?2",
            params![id.site.as_str(), id.seq as i64, status.as_str(), detail],
        )?;
        Ok(())
    }

    pub fn sync_entry(&self, id: GlobalId) -> Result<Option<(Entry, SyncStatus)>> {
        Ok(self
            .connection()
            .query_row(
                &format!("SELECT {COLUMNS}, status FROM sync_log WHERE site = ?1 AND seq = ?2"),
                params![id.site.as_str(), id.seq as i64],
                |r| Ok((entry(r)?, SyncStatus::parse(&r.get::<_, String>(6)?))),
            )
            .optional()?)
    }

    /// Applied entries stamped later than `hlc`: what an entry that arrives
    /// now is judged against.
    pub fn sync_applied_after(&self, hlc: Hlc) -> Result<Vec<Entry>> {
        let mut st = self.connection().prepare(&format!(
            "SELECT {COLUMNS} FROM sync_log WHERE status = 'applied' AND hlc > ?1 ORDER BY hlc"
        ))?;
        Ok(st
            .query_map([hlc.as_i64()], entry)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// A site's entries after sequence `after`, in order (what a peer lacks).
    pub fn sync_since(&self, site: SiteCode, after: u64, limit: usize) -> Result<Vec<Entry>> {
        let mut st = self.connection().prepare(&format!(
            "SELECT {COLUMNS} FROM sync_log WHERE site = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3"
        ))?;
        Ok(st
            .query_map(params![site.as_str(), after as i64, limit as i64], entry)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Entries waiting for their tracks, oldest first.
    pub fn sync_pending(&self) -> Result<Vec<Entry>> {
        let mut st = self.connection().prepare(&format!(
            "SELECT {COLUMNS} FROM sync_log WHERE status = 'pending' ORDER BY hlc"
        ))?;
        Ok(st.query_map([], entry)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Give up on pending entries received before `before_ms`.
    pub fn sync_expire_pending(&mut self, before_ms: i64) -> Result<usize> {
        Ok(self.connection().execute(
            "UPDATE sync_log SET status = 'failed', detail = 'its tracks never arrived'
              WHERE status = 'pending' AND received_at_ms < ?1",
            [before_ms],
        )?)
    }

    /// The undo naming entry `id`, if one has arrived.
    pub fn sync_undone_by(&self, id: GlobalId) -> Result<Option<GlobalId>> {
        let row: Option<(String, i64)> = self
            .connection()
            .query_row(
                "SELECT site, seq FROM sync_log
                  WHERE json_extract(command, '$.op') = 'undo'
                    AND json_extract(command, '$.decision') = ?1
                  ORDER BY hlc LIMIT 1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(site, seq)| {
            Some(GlobalId {
                site: site.parse().ok()?,
                seq: seq as u64,
            })
        }))
    }

    pub fn max_decision_id(&self) -> Result<i64> {
        let max: Option<i64> =
            self.connection()
                .query_row("SELECT max(id) FROM decisions", [], |r| r.get(0))?;
        Ok(max.unwrap_or(0))
    }

    /// Mark the decisions `actor` made after decision `after` as entry
    /// `id`'s, the n-th with `parts[n]` when there is one decision per part.
    /// Returns their ids.
    pub fn tag_decisions(
        &mut self,
        after: i64,
        actor: &str,
        id: GlobalId,
        parts: &[String],
    ) -> Result<Vec<i64>> {
        self.write(|tx| {
            let ids: Vec<i64> = {
                let mut st = tx
                    .prepare("SELECT id FROM decisions WHERE id > ?1 AND actor = ?2 ORDER BY id")?;
                st.query_map(params![after, actor], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            let parts = (parts.len() == ids.len() && !parts.is_empty()).then_some(parts);
            for (n, d) in ids.iter().enumerate() {
                let mut tag = serde_json::json!({ "sync": id.to_string() });
                if let Some(p) = parts {
                    tag["sync_part"] = Value::String(p[n].clone());
                }
                tx.execute(
                    "UPDATE decisions SET evidence = json_patch(evidence, ?2) WHERE id = ?1",
                    params![d, tag.to_string()],
                )?;
            }
            Ok(ids)
        })
    }

    /// The entry a local decision was made for, and its part.
    pub fn sync_of_decision(&self, decision: i64) -> Result<Option<(GlobalId, Option<String>)>> {
        let row: Option<(Option<String>, Option<String>)> = self
            .connection()
            .query_row(
                "SELECT sync_id, json_extract(evidence, '$.sync_part') FROM decisions WHERE id = ?1",
                [decision],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(id, part)| Some((id?.parse().ok()?, part))))
    }

    /// The local decision an entry made (its `part`'s, when it made several).
    pub fn decision_of_sync(&self, id: GlobalId, part: Option<&str>) -> Result<Option<i64>> {
        Ok(self
            .connection()
            .query_row(
                "SELECT id FROM decisions WHERE sync_id = ?1
                   AND (?2 IS NULL OR json_extract(evidence, '$.sync_part') = ?2)
                 ORDER BY id LIMIT 1",
                params![id.to_string(), part],
                |r| r.get(0),
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Decision;

    fn e(id: &str, hlc: u64, command: Value) -> Entry {
        Entry {
            id: id.parse().unwrap(),
            hlc: Hlc::new(hlc, 0),
            actor: "tm".into(),
            role: "track_manager".into(),
            command,
        }
    }

    #[test]
    fn entries_are_kept_once_and_judged_against_later_ones() {
        let mut db = Db::open_in_memory().unwrap();
        let site: SiteCode = "AAA".parse().unwrap();
        assert_eq!(db.sync_next_seq(site).unwrap(), 1);
        let a = e(
            "AAA:1",
            100,
            json!({"op": "pair", "tracks": ["AAA000000001", "BBB000000001"]}),
        );
        let b = e(
            "BBB:4",
            200,
            json!({"op": "delete", "tracks": ["AAA000000001"]}),
        );
        assert!(db.sync_record(&a, SyncStatus::Applied, None).unwrap());
        assert!(!db.sync_record(&a, SyncStatus::Applied, None).unwrap());
        db.sync_record(&b, SyncStatus::Pending, None).unwrap();
        assert_eq!(db.sync_next_seq(site).unwrap(), 2);
        assert_eq!(db.sync_last_hlc().unwrap(), Hlc::new(200, 0));
        assert_eq!(
            db.sync_applied_after(Hlc::new(50, 0)).unwrap(),
            vec![a.clone()]
        );
        assert!(db.sync_applied_after(Hlc::new(100, 0)).unwrap().is_empty());
        assert_eq!(db.sync_pending().unwrap(), vec![b.clone()]);
        db.sync_set_status(b.id, SyncStatus::Applied, None).unwrap();
        assert_eq!(db.sync_entry(b.id).unwrap().unwrap().1, SyncStatus::Applied);
    }

    #[test]
    fn decisions_are_tagged_with_their_entry_and_found_by_it() {
        let mut db = Db::open_in_memory().unwrap();
        let before = db.max_decision_id().unwrap();
        db.record(&Decision::new("tm", "delete_track")).unwrap();
        db.record(&Decision::new("tm", "delete_track")).unwrap();
        db.record(&Decision::new("someone else", "update_settings"))
            .unwrap();
        let id: GlobalId = "AAA:3".parse().unwrap();
        let parts = ["AAA000000001".to_owned(), "AAA000000002".to_owned()];
        let tagged = db.tag_decisions(before, "tm", id, &parts).unwrap();
        assert_eq!(tagged.len(), 2);
        assert_eq!(
            db.decision_of_sync(id, Some("AAA000000002")).unwrap(),
            Some(tagged[1])
        );
        assert_eq!(db.decision_of_sync(id, None).unwrap(), Some(tagged[0]));
        assert_eq!(
            db.sync_of_decision(tagged[1]).unwrap(),
            Some((id, Some("AAA000000002".to_owned())))
        );
        assert_eq!(db.sync_of_decision(tagged[1] + 1).unwrap(), None);
    }
}
