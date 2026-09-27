//! Track management between nodes (see `docs/multi-node.md`). A command a
//! person runs here that holds on every node (see [`ot_sync::replicated`])
//! is logged under a global id with this node's clock, and the local
//! decisions it made are tagged with that id. Another node's entry is kept,
//! judged against what was applied here (a later decision on the same
//! tracks wins), and applied once every track it names is here; until then
//! it waits.

use chrono::Utc;
use ot_core::{Observation, Uid};
use ot_store::SyncStatus;
use ot_sync::{Entry, GlobalId, Verdict};
use serde_json::{Value, json};

use super::Engine;

/// Source ids of other nodes' reports: `peer:<site>`.
pub const PEER: &str = "peer:";

/// How long another node's entry waits for its tracks.
const PENDING_FOR_MS: i64 = 12 * 3600 * 1000;

/// A command's logged form, from what it was given and what it answered:
/// UIDs it minted or resolved are filled in, and who ran it is left to the
/// entry.
fn canonical(cmd: &Value, result: &Value) -> Option<Value> {
    let op = cmd["op"].as_str()?;
    if !ot_sync::replicated(op) || op == "undo" {
        return None;
    }
    let mut c = cmd.clone();
    let o = c.as_object_mut()?;
    for k in ["id", "actor", "role"] {
        o.remove(k);
    }
    match op {
        "merge" => {
            o.insert("into".into(), result["merged_into"].clone());
        }
        "delete" => {
            o.insert("tracks".into(), result["deleted"].clone());
        }
        "group_create" => {
            o.insert("group".into(), result["group"].clone());
        }
        _ => {}
    }
    Some(c)
}

fn uid_strings(v: &Value) -> Vec<String> {
    let one = |v: &Value| {
        v.as_str()?
            .trim_start_matches(ot_core::uid::DOC_ID_PREFIX)
            .parse::<Uid>()
            .ok()
            .map(|u| u.to_string())
    };
    match v {
        Value::Array(a) => a.iter().filter_map(one).collect(),
        other => one(other).into_iter().collect(),
    }
}

impl Engine {
    /// Run an operator command, logging it for every node when it is track
    /// management that replicates. Another node's entry arrives as
    /// `{"op": "sync_entry", "entry": ...}`.
    pub(super) async fn command(&mut self, cmd: &Value) -> anyhow::Result<Value> {
        let op = cmd["op"].as_str().unwrap_or_default();
        if op == "sync_entry" {
            let entry: Entry = serde_json::from_value(cmd["entry"].clone())
                .map_err(|e| anyhow::anyhow!("entry: {e}"))?;
            let status = self.receive(entry).await?;
            return Ok(json!({ "status": status.as_str() }));
        }
        if !(ot_sync::replicated(op) || matches!(op, "accept" | "reject")) {
            return self.run_command(cmd).await;
        }
        let before = self.db(|db| db.max_decision_id()).await?;
        self.canonical = None;
        let result = self.run_command(cmd).await?;
        let logged = match self.canonical.take() {
            Some(c) => Some(c),
            None if op == "undo" => {
                // An undo holds everywhere when what it undid did.
                let undone = cmd["decision"].as_i64().unwrap_or_default();
                self.db(move |db| db.sync_of_decision(undone))
                    .await?
                    .map(|(id, part)| {
                        json!({ "op": "undo", "decision": id.to_string(), "part": part,
                                "reason": cmd["reason"] })
                    })
            }
            None => canonical(cmd, &result),
        };
        if let Some(command) = logged {
            let actor = cmd["actor"].as_str().unwrap_or("operator").to_owned();
            let role = cmd["role"].as_str().unwrap_or("track_manager").to_owned();
            let parts = if command["op"] == "delete" {
                uid_strings(&command["tracks"])
            } else {
                Vec::new()
            };
            let site = self.common.site;
            let hlc = self.hlc.tick(Utc::now().timestamp_millis() as u64);
            let id = self
                .db(move |db| {
                    let id = GlobalId {
                        site,
                        seq: db.sync_next_seq(site)?,
                    };
                    let entry = Entry {
                        id,
                        hlc,
                        actor: actor.clone(),
                        role,
                        command,
                    };
                    db.sync_record(&entry, SyncStatus::Applied, None)?;
                    db.tag_decisions(before, &actor, id, &parts)?;
                    Ok(id)
                })
                .await?;
            tracing::debug!(%id, %op, "logged for every node");
        }
        Ok(result)
    }

    /// Another node's entry: keep it once, and apply it if it can be.
    async fn receive(&mut self, entry: Entry) -> anyhow::Result<SyncStatus> {
        if entry.id.site == self.common.site {
            // Our own, heard back.
            return Ok(SyncStatus::Applied);
        }
        self.hlc.observe(entry.hlc);
        let e = entry.clone();
        let fresh = self
            .db(move |db| db.sync_record(&e, SyncStatus::Pending, None))
            .await?;
        if !fresh {
            let id = entry.id;
            let status = self.db(move |db| db.sync_entry(id)).await?;
            return Ok(status.map_or(SyncStatus::Failed, |(_, s)| s));
        }
        self.apply_entry(&entry).await
    }

    /// Apply entries still waiting for their tracks, and give up on old ones.
    pub(super) async fn retry_pending(&mut self) -> anyhow::Result<()> {
        let cutoff = Utc::now().timestamp_millis() - PENDING_FOR_MS;
        let pending = self
            .db(move |db| {
                db.sync_expire_pending(cutoff)?;
                db.sync_pending()
            })
            .await?;
        for entry in pending {
            self.apply_entry(&entry).await?;
        }
        Ok(())
    }

    /// Another node's report of its track: it reports for the system track
    /// of the same UID here, made for it if this node has none.
    pub(super) async fn adopt(
        &mut self,
        obs: &Observation,
        uid: Uid,
        counts: &mut super::EngineCounts,
    ) -> anyhow::Result<Uid> {
        let (source, key) = (obs.source_id.clone(), obs.source_track_key.clone());
        if self.tracks.contains_key(&uid) {
            let attrs = json!({ "pairing": "auto", "confidence": 1.0 });
            let d = super::engine_decision("pair")
                .reason(format!("{source} reports its track {}", uid.doc_id()));
            let (s2, k2) = (source.clone(), key.clone());
            self.db(move |db| db.pair_source_track(&s2, &k2, uid, &attrs, d))
                .await?;
            if let Some(t) = self.tracks.get_mut(&uid) {
                t.contributors.push(ot_core::Contributor {
                    source_id: source.clone(),
                    source_track_key: key.clone(),
                    pairing: ot_core::PairingType::Auto,
                    confidence: 1.0,
                    last_report: obs.observed_at,
                    existence: obs.provenance.confidence,
                });
            }
            counts.paired += 1;
        } else {
            let d = super::engine_decision("create_system_track")
                .reason(format!("{source} reports its track {}", uid.doc_id()));
            let (s2, k2) = (source.clone(), key.clone());
            self.db(move |db| db.adopt_system_track(uid, &s2, &k2, d))
                .await?;
            counts.created += 1;
        }
        self.reports.insert(format!("{source}/{key}"), uid);
        Ok(uid)
    }

    fn known(&self, uid: Uid) -> bool {
        self.tracks.contains_key(&uid) || self.groups.contains_key(&uid)
    }

    async fn set_status(
        &self,
        id: GlobalId,
        status: SyncStatus,
        detail: Option<String>,
    ) -> anyhow::Result<SyncStatus> {
        self.db(move |db| db.sync_set_status(id, status, detail.as_deref()))
            .await?;
        Ok(status)
    }

    /// Apply a kept entry of another node's here, recording where it stands.
    async fn apply_entry(&mut self, entry: &Entry) -> anyhow::Result<SyncStatus> {
        let id = entry.id;
        if let Some(by) = self.db(move |db| db.sync_undone_by(id)).await? {
            // Undone before it could be applied here: it never will be.
            self.set_status(by, SyncStatus::Applied, None).await?;
            return self
                .set_status(id, SyncStatus::Superseded, Some(format!("undone by {by}")))
                .await;
        }
        let hlc = entry.hlc;
        let later = self.db(move |db| db.sync_applied_after(hlc)).await?;
        if let Verdict::Superseded(by) = ot_sync::judge(entry, &later) {
            return self
                .set_status(
                    id,
                    SyncStatus::Superseded,
                    Some(format!("{by} decided later on the same tracks")),
                )
                .await;
        }
        let mut cmd = entry.command.clone();
        cmd["actor"] = json!(entry.actor);
        cmd["role"] = json!(entry.role);
        let op = entry.op().to_owned();
        let mut parts = Vec::new();
        match op.as_str() {
            "undo" => {
                let target: GlobalId = cmd["decision"]
                    .as_str()
                    .unwrap_or_default()
                    .parse()
                    .map_err(|e| anyhow::anyhow!("undo: {e}"))?;
                let part = cmd["part"].as_str().map(str::to_owned);
                let local = self
                    .db(move |db| db.decision_of_sync(target, part.as_deref()))
                    .await?;
                match local {
                    Some(d) => cmd["decision"] = json!(d),
                    None => {
                        let target_entry = self.db(move |db| db.sync_entry(target)).await?;
                        return match target_entry {
                            // Not here yet: when it comes, it is superseded.
                            None => Ok(SyncStatus::Pending),
                            Some((_, SyncStatus::Applied)) => {
                                self.set_status(
                                    id,
                                    SyncStatus::Failed,
                                    Some(format!("{target} made no decision here")),
                                )
                                .await
                            }
                            Some(_) => {
                                self.set_status(
                                    target,
                                    SyncStatus::Superseded,
                                    Some(format!("undone by {id}")),
                                )
                                .await?;
                                self.set_status(id, SyncStatus::Applied, None).await
                            }
                        };
                    }
                }
            }
            "delete" => {
                // Whatever of it is here; the rest never arrived or went already.
                let here: Vec<Uid> = ot_sync::targets(&cmd)
                    .into_iter()
                    .filter(|u| self.known(*u))
                    .collect();
                if here.is_empty() {
                    return Ok(SyncStatus::Pending);
                }
                cmd["tracks"] = json!(here.iter().map(|u| u.doc_id()).collect::<Vec<_>>());
                parts = here.iter().map(|u| u.to_string()).collect();
            }
            _ => {
                let mut names = ot_sync::targets(&cmd);
                if op == "group_create" {
                    for g in uid_strings(&cmd["group"]) {
                        let g: Uid = g.parse()?;
                        if self.groups.contains_key(&g) {
                            return self
                                .set_status(id, SyncStatus::Applied, Some("formed already".into()))
                                .await;
                        }
                        names.remove(&g);
                    }
                }
                if !names.iter().all(|u| self.known(*u)) {
                    return Ok(SyncStatus::Pending);
                }
            }
        }
        let actor = entry.actor.clone();
        let before = self.db(|db| db.max_decision_id()).await?;
        self.remote = true;
        let result = self.run_command(&cmd).await;
        self.remote = false;
        match result {
            Ok(_) => {
                self.db(move |db| db.tag_decisions(before, &actor, id, &parts))
                    .await?;
                tracing::info!(%id, %op, actor = %entry.actor, "applied another node's decision");
                self.set_status(id, SyncStatus::Applied, None).await
            }
            Err(e) => {
                tracing::warn!(%id, %op, error = %format!("{e:#}"), "another node's decision failed here");
                self.set_status(id, SyncStatus::Failed, Some(format!("{e:#}")))
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_logged_with_what_it_resolved() {
        let c = canonical(
            &json!({"op": "merge", "from": "tms-AAA000000001", "into": "tms-AAA000000002",
                    "actor": "bob", "id": "x"}),
            &json!({"merged_into": "tms-AAA000000002"}),
        )
        .unwrap();
        assert_eq!(
            c,
            json!({"op": "merge", "from": "tms-AAA000000001", "into": "tms-AAA000000002"})
        );
        let c = canonical(
            &json!({"op": "group_create", "members": ["tms-AAA000000001"], "spec": {"name": "G"}}),
            &json!({"group": "tms-AAA000000009"}),
        )
        .unwrap();
        assert_eq!(c["group"], "tms-AAA000000009");
        assert!(canonical(&json!({"op": "split"}), &json!({})).is_none());
        assert!(canonical(&json!({"op": "purge"}), &json!({})).is_none());
    }
}
