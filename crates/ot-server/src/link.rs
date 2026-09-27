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
    pub counts: LinkCounts,
}

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
            counts: LinkCounts::default(),
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
        self.heard.insert(m.site, Instant::now());
        match m.body {
            Body::Decisions(entries) => {
                self.counts.decisions_in += entries.len() as u64;
                let redis = self.common.open_redis().await?;
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
                self.answer(m.site, w).await
            }
            // Tracks: step 3.
            Body::Reports(_) | Body::Attrs(_) | Body::Release(_) => Ok(Vec::new()),
        }
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
    let mut tick = tokio::time::interval(Duration::from_secs(1));
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
                if now >= next_summary {
                    next_summary = now + summary_every;
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
