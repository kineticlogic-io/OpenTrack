//! `opentrack synthetic`: push synthetic system tracks through the full
//! pipeline (SQLite decision and graph, Redis state and outbox, peat writer,
//! peat-node) and optionally verify they land. This is phase 0's exit test.

use std::time::Duration;

use anyhow::{Context, bail};
use chrono::Utc;
use clap::Args;
use ot_core::{
    Affiliation, Classification, Domain, Kinematics, Observation, Position, Provenance,
    SystemTrack, TrackState, Uid,
};
use ot_peat::PeatClient;
use ot_store::Decision;

use crate::config::Common;

#[derive(Debug, Clone, Args)]
pub struct SyntheticArgs {
    /// How many tracks to create.
    #[arg(long, default_value_t = 1)]
    pub count: u32,
    /// Latitude of the first track.
    #[arg(long, default_value_t = 32.70, allow_negative_numbers = true)]
    pub lat: f64,
    /// Longitude of the first track.
    #[arg(long, default_value_t = -117.25, allow_negative_numbers = true)]
    pub lon: f64,
    /// Keep moving the tracks for this many seconds (one update per second).
    #[arg(long, default_value_t = 0)]
    pub move_secs: u64,
    /// Wait until each track's document appears on peat-node.
    #[arg(long)]
    pub verify: bool,
    /// Afterwards retire the tracks and wait for their documents to go.
    #[arg(long)]
    pub retire: bool,
    /// How long `--verify` / `--retire` wait.
    #[arg(long, default_value_t = 30)]
    pub timeout_secs: u64,
}

pub async fn run(common: &Common, args: SyntheticArgs) -> anyhow::Result<()> {
    let redis = common.open_redis().await?;
    let peat = PeatClient::connect_lazy(&common.peat)?;
    let site = common.site;
    let db_path = common.clone();

    // Keys are unique per run: a source track that still reports for a live
    // system track cannot be given a second one.
    let run = Utc::now().timestamp();
    let mut tracks = Vec::new();
    for i in 0..args.count {
        let key = format!("SYN-{run}-{:02}", i + 1);
        let obs = observation(&key, args.lat + f64::from(i) * 0.01, args.lon);
        obs.validate().context("synthetic observation is invalid")?;
        let (uid, decision_id) = {
            let key = key.clone();
            let common = db_path.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<(Uid, i64)> {
                let mut db = common.open_db()?;
                Ok(db.create_system_track(
                    site,
                    "synthetic",
                    &key,
                    Decision::new("system", "create_system_track")
                        .reason("synthetic track (opentrack synthetic)"),
                )?)
            })
            .await??
        };
        let mut track = SystemTrack::from_first_observation(uid, obs.clone());
        track.state = TrackState::Confirmed;
        redis.append_observation(&obs).await?;
        redis.put_system_track(&track, true).await?;
        println!(
            "created {} (decision {decision_id}) for synthetic/{key}",
            uid.doc_id()
        );
        tracks.push(track);
    }

    for tick in 1..=args.move_secs {
        tokio::time::sleep(Duration::from_secs(1)).await;
        for t in &mut tracks {
            let mut obs = t.view.clone();
            // ~5 m/s due west.
            obs.position.longitude -= 5.0 / (111_320.0 * obs.position.latitude.to_radians().cos());
            obs.observed_at = Utc::now();
            obs.received_at = obs.observed_at;
            t.last_seen = obs.observed_at;
            t.observation_count += 1;
            t.view = obs.clone();
            redis.append_observation(&obs).await?;
            redis.put_system_track(t, false).await?;
        }
        if tick % 5 == 0 {
            println!("moved {} track(s) for {tick}s", tracks.len());
        }
    }

    let timeout = Duration::from_secs(args.timeout_secs);
    if args.verify {
        for t in &tracks {
            let doc = wait_for(&peat, &common.tracks_collection, &t.uid, true, timeout).await?;
            let doc: serde_json::Value = serde_json::from_str(&doc.unwrap_or_default())?;
            println!(
                "verified {}/{} on peat-node: lat {} lon {} state {}",
                common.tracks_collection,
                t.uid.doc_id(),
                doc["position"]["latitude"],
                doc["position"]["longitude"],
                doc["state"],
            );
        }
    }
    if args.retire {
        for t in &tracks {
            let (uid, common2) = (t.uid, common.clone());
            tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
                Ok(common2.open_db()?.retire_system_track(
                    uid,
                    Decision::new("system", "retire_system_track")
                        .reason("synthetic track (opentrack synthetic --retire)"),
                )?)
            })
            .await??;
            redis
                .retire_system_track(t.uid, &common.tracks_collection)
                .await?;
        }
        for t in &tracks {
            wait_for(&peat, &common.tracks_collection, &t.uid, false, timeout).await?;
            println!(
                "retired {} and confirmed its document is gone",
                t.uid.doc_id()
            );
        }
    }
    Ok(())
}

fn observation(key: &str, lat: f64, lon: f64) -> Observation {
    let now = Utc::now();
    Observation {
        schema_version: 1,
        source_id: "synthetic".into(),
        source_track_key: key.into(),
        identifiers: vec![ot_core::Identifier::new("synthetic", key)],
        name: Some(format!("OPENTRACK SYNTHETIC {key}")),
        callsign: None,
        observed_at: now,
        received_at: now,
        position: Position {
            latitude: lat,
            longitude: lon,
            altitude_hae_m: None,
        },
        uncertainty: None,
        kinematics: Kinematics {
            course_deg: Some(270.0),
            speed_mps: Some(5.0),
            heading_deg: Some(270.0),
            vertical_rate_mps: None,
        },
        classification: Classification {
            cot_type: None,
            domain: Some(Domain::Surface),
            affiliation: Some(Affiliation::Pending),
        },
        platform: Default::default(),
        provenance: Provenance {
            source_code: Some("synthetic".into()),
            confidence: Some(1.0),
            ..Default::default()
        },
        state: None,
        ext: Default::default(),
    }
}

/// Poll peat-node until the document is present (or absent).
async fn wait_for(
    peat: &PeatClient,
    collection: &str,
    uid: &Uid,
    present: bool,
    timeout: Duration,
) -> anyhow::Result<Option<String>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let doc = peat.get(collection, &uid.doc_id()).await?;
        if doc.is_some() == present {
            return Ok(doc);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "{collection}/{} still {} after {timeout:?}; is `opentrack writer` running?",
                uid.doc_id(),
                if present { "missing" } else { "present" },
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
