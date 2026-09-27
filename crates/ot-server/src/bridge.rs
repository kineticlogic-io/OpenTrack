//! `opentrack bridge`: carry sync messages between nodes' NATS boundaries
//! (see `docs/sync-icd.md`), for server sites that reach each other over IP
//! and have no networking package of their own, and for tests.
//!
//! Every message a node puts on `<prefix>.out.<kind>` goes to every other
//! node's `<prefix>.in.<kind>` (only to the one named in `OT-To`, when it
//! is addressed and the bridge knows that node's site code). It can also
//! stand in for a poor link: drop, delay, duplicate, cap each node's
//! sending rate, and cut groups of nodes off from each other for a time.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use ot_nats::async_nats;
use serde::Serialize;

use crate::link::HEADER_TO;

#[derive(Debug, Clone, clap::Args)]
pub struct BridgeArgs {
    /// A node's side of the link: `<nats url>`, or `<nats url>#<prefix>`
    /// when its prefix is not `ot.sync`. Give one per node.
    #[arg(long = "node", required = true)]
    pub nodes: Vec<String>,
    /// Probability each message is lost on its way to each node.
    #[arg(long, default_value_t = 0.0)]
    pub loss: f64,
    /// Delay added to every message, ms, plus up to `--jitter-ms` more
    /// (which reorders messages).
    #[arg(long, default_value_t = 0)]
    pub delay_ms: u64,
    #[arg(long, default_value_t = 0)]
    pub jitter_ms: u64,
    /// Probability a message arrives twice.
    #[arg(long, default_value_t = 0.0)]
    pub duplicate: f64,
    /// What each node may send, kbit/s (0: no cap). Past it, messages are
    /// dropped, as on a saturated radio.
    #[arg(long, default_value_t = 0.0)]
    pub rate_kbps: f64,
    /// Cut groups of nodes off from each other for a time, seconds after
    /// the bridge starts: `<from>-<to>:<sites>|<sites>`, e.g.
    /// `60-180:AAA,BBB|CCC`. Repeatable.
    #[arg(long)]
    pub partition: Vec<String>,
    /// Write the counters here (JSON) every 5 s.
    #[arg(long)]
    pub stats: Option<PathBuf>,
    /// Seed for the losses, delays and duplicates.
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Partition {
    pub from_s: f64,
    pub to_s: f64,
    pub sides: Vec<Vec<String>>,
}

impl Partition {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let bad = || anyhow::anyhow!("partition {s:?} is not <from>-<to>:<sites>|<sites>");
        let (span, groups) = s.split_once(':').ok_or_else(bad)?;
        let (a, b) = span.split_once('-').ok_or_else(bad)?;
        let sides: Vec<Vec<String>> = groups
            .split('|')
            .map(|g| g.split(',').map(|x| x.trim().to_owned()).collect())
            .collect();
        if sides.len() < 2 {
            return Err(bad());
        }
        Ok(Self {
            from_s: a.trim().parse().map_err(|_| bad())?,
            to_s: b.trim().parse().map_err(|_| bad())?,
            sides,
        })
    }

    /// Whether it separates `a` from `b` at `t` seconds.
    pub fn cuts(&self, t: f64, a: &str, b: &str) -> bool {
        if t < self.from_s || t >= self.to_s {
            return false;
        }
        let side = |x: &str| self.sides.iter().position(|g| g.iter().any(|s| s == x));
        matches!((side(a), side(b)), (Some(i), Some(j)) if i != j)
    }
}

/// Counters per node (by site code once known, else by its position).
#[derive(Debug, Default, Clone, Serialize)]
pub struct SideCounts {
    pub sent_messages: u64,
    pub sent_bytes: u64,
    pub report_bytes: u64,
    pub delivered: u64,
    pub lost: u64,
    pub over_rate: u64,
    pub partitioned: u64,
}

/// A small deterministic generator (xorshift64*), enough for link noise.
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

struct Side {
    client: async_nats::Client,
    prefix: String,
    /// Learned from the messages it sends.
    site: Option<String>,
    /// Sending allowance, bytes (token bucket), and when last topped up.
    allowance: f64,
    topped: Instant,
    counts: SideCounts,
}

pub async fn run(
    args: BridgeArgs,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    let partitions = args
        .partition
        .iter()
        .map(|p| Partition::parse(p))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut clients: HashMap<String, async_nats::Client> = HashMap::new();
    let mut sides = Vec::new();
    for n in &args.nodes {
        let (url, prefix) = n.split_once('#').unwrap_or((n.as_str(), "ot.sync"));
        let client = match clients.get(url) {
            Some(c) => c.clone(),
            None => {
                let c = async_nats::ConnectOptions::new()
                    .name("opentrack-bridge")
                    .retry_on_initial_connect()
                    .connect(url)
                    .await?;
                clients.insert(url.to_owned(), c.clone());
                c
            }
        };
        sides.push(Side {
            client,
            prefix: prefix.to_owned(),
            site: None,
            allowance: 0.0,
            topped: Instant::now(),
            counts: SideCounts::default(),
        });
    }
    let mut subs = futures_util::stream::SelectAll::new();
    for (i, s) in sides.iter().enumerate() {
        let sub = s.client.subscribe(format!("{}.out.>", s.prefix)).await?;
        subs.push(sub.map(move |m| (i, m)));
    }
    tracing::info!(
        nodes = sides.len(),
        loss = args.loss,
        rate_kbps = args.rate_kbps,
        partitions = partitions.len(),
        "bridge running"
    );
    let sides = Arc::new(Mutex::new(sides));
    let started = Instant::now();
    let mut rng = Rng(args.seed.max(1));
    let rate = args.rate_kbps * 1000.0 / 8.0;
    let mut stats_tick = tokio::time::interval(Duration::from_secs(5));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = stats_tick.tick() => {
                if let Some(path) = &args.stats {
                    write_stats(path, &sides, started)?;
                }
            }
            next = subs.next() => {
                let Some((i, msg)) = next else { break };
                let kind = msg.subject.rsplit('.').next().unwrap_or("report").to_owned();
                let to = msg
                    .headers
                    .as_ref()
                    .and_then(|h| h.get(HEADER_TO))
                    .map(|v| v.as_str().to_owned());
                let t = started.elapsed().as_secs_f64();
                let mut deliveries = Vec::new();
                {
                    let mut sides = sides.lock().expect("bridge state");
                    let n = sides.len();
                    let from_site = std::str::from_utf8(msg.payload.get(4..7).unwrap_or_default())
                        .ok()
                        .map(str::to_owned);
                    let s = &mut sides[i];
                    if s.site.is_none() {
                        s.site = from_site.clone();
                    }
                    let len = msg.payload.len();
                    s.counts.sent_messages += 1;
                    s.counts.sent_bytes += len as u64;
                    if kind == "report" {
                        s.counts.report_bytes += len as u64;
                    }
                    if rate > 0.0 {
                        let now = Instant::now();
                        s.allowance = (s.allowance + now.duration_since(s.topped).as_secs_f64() * rate)
                            .min(rate.max(len as f64));
                        s.topped = now;
                        if s.allowance < len as f64 {
                            s.counts.over_rate += 1;
                            continue;
                        }
                        s.allowance -= len as f64;
                    }
                    let from = s.site.clone().unwrap_or_default();
                    for j in (0..n).filter(|j| *j != i) {
                        let dest = sides[j].site.clone();
                        if let (Some(to), Some(d)) = (&to, &dest)
                            && to != d
                        {
                            continue;
                        }
                        if let Some(d) = &dest
                            && partitions.iter().any(|p| p.cuts(t, &from, d))
                        {
                            sides[i].counts.partitioned += 1;
                            continue;
                        }
                        if rng.unit() < args.loss {
                            sides[i].counts.lost += 1;
                            continue;
                        }
                        let copies = if rng.unit() < args.duplicate { 2 } else { 1 };
                        let delay = args.delay_ms
                            + if args.jitter_ms > 0 {
                                (rng.unit() * args.jitter_ms as f64) as u64
                            } else {
                                0
                            };
                        sides[i].counts.delivered += 1;
                        let d = &sides[j];
                        deliveries.push((
                            d.client.clone(),
                            format!("{}.in.{kind}", d.prefix),
                            copies,
                            delay,
                        ));
                    }
                }
                for (client, subject, copies, delay) in deliveries {
                    let payload = msg.payload.clone();
                    let send = async move {
                        if delay > 0 {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                        }
                        for _ in 0..copies {
                            let _ = client.publish(subject.clone(), payload.clone()).await;
                        }
                    };
                    if delay > 0 {
                        tokio::spawn(send);
                    } else {
                        send.await;
                    }
                }
            }
        }
    }
    if let Some(path) = &args.stats {
        write_stats(path, &sides, started)?;
    }
    Ok(())
}

fn write_stats(
    path: &std::path::Path,
    sides: &Arc<Mutex<Vec<Side>>>,
    started: Instant,
) -> anyhow::Result<()> {
    let sides = sides.lock().expect("bridge state");
    let by: serde_json::Map<String, serde_json::Value> = sides
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                s.site.clone().unwrap_or_else(|| format!("#{i}")),
                serde_json::to_value(&s.counts).unwrap_or_default(),
            )
        })
        .collect();
    let doc = serde_json::json!({ "elapsed_s": started.elapsed().as_secs_f64(), "nodes": by });
    std::fs::write(path, serde_json::to_vec_pretty(&doc)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partition_cuts_only_across_its_sides_and_only_for_its_time() {
        let p = Partition::parse("60-180:AAA,BBB|CCC").unwrap();
        assert!(p.cuts(60.0, "AAA", "CCC"));
        assert!(p.cuts(100.0, "CCC", "BBB"));
        assert!(!p.cuts(100.0, "AAA", "BBB"));
        assert!(!p.cuts(59.9, "AAA", "CCC"));
        assert!(!p.cuts(180.0, "AAA", "CCC"));
        assert!(
            !p.cuts(100.0, "AAA", "DDD"),
            "a node in no group is not cut"
        );
        assert!(Partition::parse("60:AAA|BBB").is_err());
        assert!(Partition::parse("1-2:AAA").is_err());
    }

    #[test]
    fn the_noise_is_uniform_and_repeatable() {
        let (mut a, mut b) = (Rng(7), Rng(7));
        let xs: Vec<f64> = (0..1000).map(|_| a.unit()).collect();
        assert!(xs.iter().all(|x| (0.0..1.0).contains(x)));
        assert_eq!(xs[..5], (0..5).map(|_| b.unit()).collect::<Vec<_>>()[..]);
        let mean = xs.iter().sum::<f64>() / 1000.0;
        assert!((mean - 0.5).abs() < 0.05);
    }
}
