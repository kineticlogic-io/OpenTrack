//! The link role: this node's side of the boundary with whatever carries
//! sync messages between nodes (see `docs/multi-node.md` and
//! `docs/sync-icd.md`). OpenTrack does not do the networking: it publishes
//! what it has to say on the node's own NATS, `<prefix>.out.<kind>`, and
//! hears other nodes on `<prefix>.in.<kind>`; a networking package (or
//! `opentrack bridge`, for server sites) moves the bytes.
//!
//! Nothing here depends on the link delivering everything. Decision logs
//! converge by anti-entropy: every few seconds each node says which
//! decisions it holds from each site (`summary`), and a node that hears of
//! decisions it lacks asks whoever said so (`want`), whichever node made
//! them.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use ot_core::SiteCode;
use ot_sync::wire::{self, Body, Kind, Message, Summary, Want};
use ot_sync::{Entry, Hlc};
use serde_json::json;

use crate::config::Common;
use crate::settings_api::{SyncSettings, sync_settings};

/// Header on an `out` message meant for one node, with its site code.
pub const HEADER_TO: &str = "OT-To";

#[derive(Debug, Clone, clap::Args)]
pub struct LinkArgs {
    /// Subject prefix of the sync boundary: `<prefix>.out.<kind>` and
    /// `<prefix>.in.<kind>`.
    #[arg(long, env = "OT_SYNC_PREFIX", default_value = "ot.sync")]
    pub sync_prefix: String,
    /// Seconds between summaries.
    #[arg(long, env = "OT_SYNC_SUMMARY_SECS", default_value_t = 5.0)]
    pub summary_secs: f64,
}

/// A message to send: its kind, the node it is for (none: everyone), and
/// its bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Out {
    pub kind: Kind,
    pub to: Option<SiteCode>,
    pub bytes: Vec<u8>,
}

/// Ranges asked for per request, and how long before asking a site again.
const MOST_RANGES: usize = 16;
const ASK_AGAIN: Duration = Duration::from_secs(5);

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct LinkCounts {
    pub received: u64,
    pub unreadable: u64,
    /// From a site not in the trusted list.
    pub untrusted: u64,
    pub reports_in: u64,
    pub decisions_in: u64,
    pub decisions_out: u64,
    pub wants_in: u64,
    pub wants_out: u64,
}

/// The link's state, apart from NATS.
pub struct Link {
    common: Common,
    settings: SyncSettings,
    /// This node's own entries sent so far (by sequence).
    sent: u64,
    /// When each site's missing decisions were last asked for.
    asked: HashMap<SiteCode, Instant>,
    /// When each peer was last heard.
    pub heard: HashMap<SiteCode, Instant>,
    /// What each peer last said about its tracks' identity and attributes
    /// (sent only when they change), put on every report of theirs.
    identity: HashMap<(SiteCode, ot_core::Uid), Identity>,
    pub counts: LinkCounts,
    redis: Option<ot_store::RedisStore>,
}

#[derive(Debug, Default, Clone)]
struct Identity {
    identifiers: Vec<ot_core::schema::Identifier>,
    attrs: Option<serde_json::Value>,
}

/// A peer not heard for this long is asked for a snapshot when it returns.
const SILENT: Duration = Duration::from_secs(30);

impl Link {
    pub async fn new(common: Common) -> anyhow::Result<Self> {
        let site = common.site;
        let c = common.clone();
        let sent = tokio::task::spawn_blocking(move || -> anyhow::Result<u64> {
            Ok(c.open_db()?.sync_next_seq(site)? - 1)
        })
        .await??;
        let mut link = Self {
            common,
            settings: SyncSettings::default(),
            sent,
            asked: HashMap::new(),
            heard: HashMap::new(),
            identity: HashMap::new(),
            counts: LinkCounts::default(),
            redis: None,
        };
        link.refresh().await?;
        Ok(link)
    }

    pub fn enabled(&self) -> bool {
        self.settings.enabled
    }

    async fn db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut ot_store::Db) -> ot_store::sqlite::Result<T> + Send + 'static,
    ) -> anyhow::Result<T> {
        let c = self.common.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<T> {
            let mut db = c.open_db()?;
            Ok(f(&mut db)?)
        })
        .await?
    }

    /// Reload the settings (peers, on or off).
    pub async fn refresh(&mut self) -> anyhow::Result<()> {
        let saved = self.db(|db| db.app_settings()).await?;
        self.settings = sync_settings(&saved);
        Ok(())
    }

    async fn redis(&mut self) -> anyhow::Result<ot_store::RedisStore> {
        if self.redis.is_none() {
            self.redis = Some(self.common.open_redis().await?);
        }
        Ok(self.redis.clone().expect("opened above"))
    }

    /// Messages the engine queued (reports, attributes, releases).
    pub async fn engine_messages(&mut self) -> anyhow::Result<Vec<Out>> {
        let redis = self.redis().await?;
        let mut outs = Vec::new();
        loop {
            let batch = redis.pop_sync_out(500).await?;
            let n = batch.len();
            outs.extend(batch.into_iter().filter_map(|bytes| {
                let kind = bytes.get(3).copied().and_then(kind_of)?;
                Some(Out {
                    kind,
                    to: None,
                    bytes,
                })
            }));
            if n < 500 {
                return Ok(outs);
            }
        }
    }

    fn now_hlc(&self) -> Hlc {
        Hlc::new(chrono::Utc::now().timestamp_millis() as u64, 0)
    }

    fn trusted(&self, site: SiteCode) -> bool {
        self.settings.peers.iter().any(|p| p == site.as_str())
    }

    /// This node's entries made since the last call, for everyone.
    pub async fn new_decisions(&mut self) -> anyhow::Result<Vec<Out>> {
        let (site, after) = (self.common.site, self.sent);
        let entries: Vec<Entry> = self.db(move |db| db.sync_since(site, after, 255)).await?;
        let Some(last) = entries.last() else {
            return Ok(Vec::new());
        };
        self.sent = last.id.seq;
        self.counts.decisions_out += entries.len() as u64;
        Ok(wire::encode_decisions(site, self.now_hlc(), &entries)
            .into_iter()
            .map(|bytes| Out {
                kind: Kind::Decision,
                to: None,
                bytes,
            })
            .collect())
    }

    /// How the link is going, for the Settings page.
    pub async fn write_status(&mut self) -> anyhow::Result<()> {
        let now = chrono::Utc::now();
        let heard: serde_json::Map<String, serde_json::Value> = self
            .heard
            .iter()
            .map(|(site, at)| {
                let ago = chrono::Duration::from_std(at.elapsed()).unwrap_or_default();
                (site.to_string(), json!(now - ago))
            })
            .collect();
        let status = json!({ "at": now, "heard": heard, "counts": self.counts });
        self.redis().await?.put_sync_status("link", &status).await?;
        Ok(())
    }

    /// What this node holds, for everyone.
    pub async fn summary(&self) -> anyhow::Result<Out> {
        let heads = self.db(|db| db.sync_heads()).await?;
        let m = Message::new(
            self.common.site,
            self.now_hlc(),
            Body::Summary(Summary {
                heads,
                reporting: 0,
            }),
        );
        Ok(Out {
            kind: Kind::Summary,
            to: None,
            bytes: m.encode(),
        })
    }

    /// Handle a message another node sent; returns what to send back.
    pub async fn handle(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<Out>> {
        self.counts.received += 1;
        let m = match Message::decode(bytes) {
            Ok(m) => m,
            Err(e) => {
                self.counts.unreadable += 1;
                tracing::debug!(error = %e, "unreadable sync message");
                return Ok(Vec::new());
            }
        };
        if m.site == self.common.site {
            return Ok(Vec::new());
        }
        if !self.trusted(m.site) {
            self.counts.untrusted += 1;
            return Ok(Vec::new());
        }
        let returning = self
            .heard
            .insert(m.site, Instant::now())
            .is_none_or(|at| at.elapsed() > SILENT);
        let mut outs = Vec::new();
        if returning {
            // A node new to this one (or back): ask for its whole picture.
            outs.push(Out {
                kind: Kind::Want,
                to: Some(m.site),
                bytes: Message::new(
                    self.common.site,
                    self.now_hlc(),
                    Body::Want(Want {
                        decisions: Vec::new(),
                        snapshot: true,
                    }),
                )
                .encode(),
            });
        }
        outs.extend(self.dispatch(m).await?);
        Ok(outs)
    }

    async fn dispatch(&mut self, m: Message) -> anyhow::Result<Vec<Out>> {
        match m.body {
            Body::Decisions(entries) => {
                self.counts.decisions_in += entries.len() as u64;
                let redis = self.redis().await?;
                for entry in entries {
                    // Only trusted sites' decisions, whoever relays them.
                    if entry.id.site != self.common.site && self.trusted(entry.id.site) {
                        redis
                            .push_command(&json!({ "op": "sync_entry", "entry": entry }))
                            .await?;
                    }
                }
                Ok(Vec::new())
            }
            Body::Summary(s) => self.wants_for(m.site, s).await,
            Body::Want(w) => {
                self.counts.wants_in += 1;
                if w.snapshot {
                    self.redis()
                        .await?
                        .push_command(&json!({ "op": "sync_snapshot" }))
                        .await?;
                }
                self.answer(m.site, w).await
            }
            Body::Reports(reports) => {
                self.counts.reports_in += reports.len() as u64;
                let obs: Vec<ot_core::Observation> = reports
                    .into_iter()
                    .map(|r| self.observation(m.site, r))
                    .collect();
                self.redis()
                    .await?
                    .append_observations(&format!("peer:{}", m.site), &obs)
                    .await?;
                Ok(Vec::new())
            }
            Body::Attrs(items) => {
                for (uid, v) in items {
                    self.identity.entry((m.site, uid)).or_default().attrs = Some(v);
                }
                Ok(Vec::new())
            }
            Body::Release(uids) => {
                let uids: Vec<String> = uids.iter().map(|u| u.to_string()).collect();
                self.redis()
                    .await?
                    .push_command(
                        &json!({ "op": "sync_release", "site": m.site.as_str(), "uids": uids }),
                    )
                    .await?;
                Ok(Vec::new())
            }
        }
    }

    /// A peer's report as an observation of source `peer:<site>`, keyed by
    /// the track's UID, with the identity the peer last gave it.
    fn observation(&mut self, site: SiteCode, r: wire::Report) -> ot_core::Observation {
        use ot_core::schema::{Ellipse, Identifier, Uncertainty};
        let key = (site, r.uid);
        if r.state == ot_core::TrackState::Dropped {
            self.identity.remove(&key);
        } else if let Some(ids) = &r.identifiers {
            self.identity.entry(key).or_default().identifiers = ids
                .iter()
                .map(|(s, v)| Identifier::new(s.clone(), v.clone()))
                .collect();
        }
        let who = self.identity.get(&key).cloned().unwrap_or_default();
        let at = chrono::DateTime::from_timestamp_millis(r.time_ms).unwrap_or_default();
        let known = |m: f64| m.is_finite().then_some(m);
        let uncertainty = known(r.error.major_m).map(|major| Uncertainty {
            ellipse: Some(Ellipse {
                semi_major_m: major,
                semi_minor_m: known(r.error.minor_m).unwrap_or(major).min(major),
                orientation_deg: r.error.orientation_deg,
            }),
            circular_error_m: None,
            vertical_error_m: None,
            covariance: None,
        });
        let mut v = json!({
            "schema_version": 1,
            "source_id": format!("peer:{site}"),
            "source_track_key": r.uid.to_string(),
            "observed_at": at,
            "received_at": chrono::Utc::now(),
            "position": {"latitude": r.lat, "longitude": r.lon, "altitude_hae_m": r.alt_m},
            "kinematics": {"course_deg": r.course_deg, "speed_mps": r.speed_mps},
            "classification": {"domain": r.domain},
            "state": r.state,
            "identifiers": who.identifiers,
        });
        if let Some(a) = &who.attrs {
            for k in ["name", "callsign"] {
                if a[k].is_string() {
                    v[k] = a[k].clone();
                }
            }
            if let Some(c) = a["classification"].as_object() {
                let mut c = c.clone();
                if r.domain.is_some() {
                    c.insert("domain".into(), json!(r.domain));
                }
                v["classification"] = serde_json::Value::Object(c);
            }
            if a["attributes"].is_object() {
                v["ext"] = a["attributes"].clone();
            }
        }
        let mut obs: ot_core::Observation =
            serde_json::from_value(strip_nulls(v)).expect("a peer report is an observation");
        obs.uncertainty = uncertainty;
        obs.origin = r
            .origin_ms
            .and_then(chrono::DateTime::from_timestamp_millis);
        obs
    }

    /// Ask `from` for the decisions its summary shows this node lacks.
    async fn wants_for(&mut self, from: SiteCode, s: Summary) -> anyhow::Result<Vec<Out>> {
        let mut decisions = Vec::new();
        for (site, head) in s.heads {
            if site == self.common.site || !self.trusted(site) {
                continue;
            }
            if self
                .asked
                .get(&site)
                .is_some_and(|at| at.elapsed() < ASK_AGAIN)
            {
                continue;
            }
            let missing = self
                .db(move |db| db.sync_missing(site, head, MOST_RANGES))
                .await?;
            if !missing.is_empty() {
                self.asked.insert(site, Instant::now());
                decisions.push((site, missing));
            }
        }
        if decisions.is_empty() {
            return Ok(Vec::new());
        }
        self.counts.wants_out += 1;
        let m = Message::new(
            self.common.site,
            self.now_hlc(),
            Body::Want(Want {
                decisions,
                snapshot: false,
            }),
        );
        Ok(vec![Out {
            kind: Kind::Want,
            to: Some(from),
            bytes: m.encode(),
        }])
    }

    /// Send `to` the decisions it asked for that this node holds.
    async fn answer(&mut self, to: SiteCode, w: Want) -> anyhow::Result<Vec<Out>> {
        let mut entries = Vec::new();
        for (site, ranges) in w.decisions {
            for (a, b) in ranges.into_iter().take(MOST_RANGES) {
                let b = b.min(a + 1000);
                entries.extend(self.db(move |db| db.sync_range(site, a, b)).await?);
            }
        }
        self.counts.decisions_out += entries.len() as u64;
        Ok(
            wire::encode_decisions(self.common.site, self.now_hlc(), &entries)
                .into_iter()
                .map(|bytes| Out {
                    kind: Kind::Decision,
                    to: Some(to),
                    bytes,
                })
                .collect(),
        )
    }
}

fn kind_of(b: u8) -> Option<Kind> {
    [
        Kind::Report,
        Kind::Attrs,
        Kind::Decision,
        Kind::Summary,
        Kind::Want,
        Kind::Release,
    ]
    .into_iter()
    .find(|k| *k as u8 == b)
}

/// Drop null members (absent optional fields).
fn strip_nulls(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(m) => serde_json::Value::Object(
            m.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_nulls(v)))
                .collect(),
        ),
        other => other,
    }
}

/// Run the link against the node's NATS until shutdown.
pub async fn run(
    common: Common,
    args: LinkArgs,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    let nats = ot_nats::Nats::connect(common.nats_settings()).await?;
    let client = nats.client().clone();
    let mut sub = client
        .subscribe(format!("{}.in.>", args.sync_prefix))
        .await?;
    let mut link = Link::new(common).await?;
    tracing::info!(prefix = %args.sync_prefix, enabled = link.enabled(), "link running");
    let send = |outs: Vec<Out>| {
        let client = client.clone();
        let prefix = args.sync_prefix.clone();
        async move {
            for o in outs {
                let subject = format!("{prefix}.out.{}", o.kind.name());
                let mut headers = ot_nats::async_nats::HeaderMap::new();
                if let Some(to) = o.to {
                    headers.insert(HEADER_TO, to.as_str());
                }
                if let Err(e) = client
                    .publish_with_headers(subject, headers, o.bytes.into())
                    .await
                {
                    tracing::warn!(error = %e, "sync publish failed");
                }
            }
        }
    };
    let summary_every = Duration::from_secs_f64(args.summary_secs.max(0.5));
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let (mut next_summary, mut next_refresh) = (Instant::now(), Instant::now());
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            msg = sub.next() => {
                let Some(msg) = msg else { break };
                if !link.enabled() {
                    continue;
                }
                match link.handle(&msg.payload).await {
                    Ok(outs) => send(outs).await,
                    Err(e) => tracing::warn!(error = %format!("{e:#}"), "sync message failed"),
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                if now >= next_refresh {
                    next_refresh = now + Duration::from_secs(5);
                    if let Err(e) = link.refresh().await {
                        tracing::warn!(error = %format!("{e:#}"), "sync settings refresh failed");
                    }
                }
                if !link.enabled() {
                    continue;
                }
                match link.new_decisions().await {
                    Ok(outs) => send(outs).await,
                    Err(e) => tracing::warn!(error = %format!("{e:#}"), "sending decisions failed"),
                }
                match link.engine_messages().await {
                    Ok(outs) => send(outs).await,
                    Err(e) => tracing::warn!(error = %format!("{e:#}"), "sending tracks failed"),
                }
                if now >= next_summary {
                    next_summary = now + summary_every;
                    if let Err(e) = link.write_status().await {
                        tracing::warn!(error = %format!("{e:#}"), "sync status failed");
                    }
                    match link.summary().await {
                        Ok(o) => send(vec![o]).await,
                        Err(e) => tracing::warn!(error = %format!("{e:#}"), "summary failed"),
                    }
                }
            }
        }
    }
    let _ = client.flush().await;
    tracing::info!(counts = ?link.counts, "link stopped");
    Ok(())
}
