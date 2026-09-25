use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ot_core::{Affiliation, Domain, Identifier, Observation};
use serde_json::json;

use super::filter::moved;
use super::*;

fn at(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(1_790_000_000_000 + ms).unwrap()
}

fn spec(algorithm: &str) -> TrackerSpec {
    serde_json::from_value(json!({ "algorithm": algorithm })).unwrap()
}

/// A detection `s` seconds in of a boat moving 4 m/s east, claiming far
/// more than a tracker may pass on.
fn loud_plot(s: i64) -> Observation {
    let (lat, lon) = moved(63.44, 10.40, 0.0, 4.0 * s as f64);
    let mut o: Observation = serde_json::from_value(json!({
        "schema_version": 1, "source_id": "radar", "source_track_key": format!("p{s}"),
        "observed_at": at(s * 1000), "received_at": at(s * 1000),
        "position": {"latitude": lat, "longitude": lon},
        "uncertainty": {"circular_error_m": 5.0},
        "name": "NOT FROM A TRACKER", "callsign": "NOPE",
        "classification": {"cot_type": "a-h-S-C-L-D-D", "affiliation": "hostile", "sidc": "SHSPCLDD--*****"},
        "platform": {"class": "destroyer", "type_code": "DDG"},
        "provenance": {"sensor_code": "R1"},
        "ext": {"rcs": 30}
    }))
    .unwrap();
    o.identifiers = vec![Identifier::new("mmsi", "366999999")];
    o
}

fn run_all(t: &mut Tracker, plots: Vec<Observation>) -> Vec<Observation> {
    let mut out = Vec::new();
    for p in plots {
        let r = p.received_at;
        t.push(p, r);
        out.extend(t.run(r, true).into_iter().map(|(o, _)| o));
    }
    out
}

#[test]
fn every_tracker_classifies_no_higher_than_unknown_and_a_domain() {
    for algorithm in ["gnn", "mht"] {
        let mut t = Tracker::new(spec(algorithm)).unwrap();
        let out = run_all(&mut t, (0..8).map(loud_plot).collect());
        assert!(!out.is_empty(), "{algorithm}: the boat is tracked");
        for o in &out {
            assert_eq!(
                o.classification.affiliation,
                Some(Affiliation::Unknown),
                "{algorithm}"
            );
            // The plots' own symbol says surface: the track may know that much.
            assert_eq!(
                o.classification.domain,
                Some(Domain::Surface),
                "{algorithm}"
            );
            assert_eq!(o.classification.cot_type, None);
            assert_eq!(o.classification.sidc, None);
            assert_eq!(o.classification.cot_type_or_derived(), "a-u-S");
            assert!(o.identifiers.is_empty() && o.name.is_none() && o.callsign.is_none());
            assert_eq!(o.platform, Default::default());
            assert!(o.ext.is_empty());
            // Where the report came from is kept, and which tracker made it.
            assert_eq!(o.provenance.sensor_code.as_deref(), Some("R1"));
            assert_eq!(o.provenance.tracker.as_deref(), Some(t.version()));
            assert!(t.version().starts_with(algorithm));
            assert!(o.source_track_key.starts_with('T'));
            ot_core::Observation::validate(o).unwrap();
        }
        let last = out.last().unwrap();
        assert!(
            (last.kinematics.speed_mps.unwrap() - 4.0).abs() < 1.0,
            "{algorithm}: {last:?}"
        );
    }
}

#[test]
fn domain_comes_from_the_spec_or_the_plots_and_never_space() {
    let mut s = spec("gnn");
    s.domain = Some(TrackerDomain::Subsurface);
    let mut t = Tracker::new(s).unwrap();
    let out = run_all(&mut t, (0..4).map(loud_plot).collect());
    assert_eq!(out[0].classification.domain, Some(Domain::Subsurface));

    // Plots that say space: a tracker leaves the domain unset.
    let mut t = Tracker::new(spec("gnn")).unwrap();
    let plots: Vec<Observation> = (0..4)
        .map(|s| {
            let mut o = loud_plot(s);
            o.classification = Default::default();
            o.classification.domain = Some(Domain::Space);
            o
        })
        .collect();
    let out = run_all(&mut t, plots);
    assert_eq!(out[0].classification.domain, None);
    assert_eq!(tracker_classification(Some(Domain::Space)).domain, None);
}

#[test]
fn time_scans_wait_for_their_plots_and_late_ones_are_dropped() {
    let mut s = spec("gnn");
    s.scans = ScanGrouping::Time;
    s.confirm_hits = 1;
    let mut t = Tracker::new(s).unwrap();
    // Two plots of one scan in separate frames.
    let (a, mut b) = (loud_plot(0), loud_plot(0));
    b.position.latitude += 0.01;
    t.push(a, at(0));
    assert!(t.run(at(100), false).is_empty(), "held for more plots");
    t.push(b, at(200));
    assert_eq!(
        t.run(at(800), false).len(),
        2,
        "both reported once the hold passed"
    );
    // A plot from before that scan arrives late.
    let mut late = loud_plot(0);
    late.observed_at = at(-500);
    t.push(late, at(900));
    assert!(t.run(at(900), true).is_empty());
    assert_eq!(t.late, 1);
}

#[test]
fn clustering_merges_one_objects_plots() {
    let mut s = spec("gnn");
    s.confirm_hits = 1;
    s.cluster_m = 10.0;
    let mut t = Tracker::new(s).unwrap();
    // Two points 6 m apart on one hull, and one 80 m away.
    let a = loud_plot(0);
    let mut b = loud_plot(0);
    let (lat, lon) = moved(a.position.latitude, a.position.longitude, 6.0, 0.0);
    b.position.latitude = lat;
    b.position.longitude = lon;
    let mut c = loud_plot(0);
    c.position.latitude += 80.0 / 111_320.0;
    for p in [a, b, c] {
        t.push(p, at(0));
    }
    let out = t.run(at(0), true);
    assert_eq!(out.len(), 2);
    let (lat, _) = moved(63.44, 10.40, 3.0, 0.0);
    assert!(
        out.iter()
            .any(|(o, _)| (o.position.latitude - lat).abs() < 1e-6),
        "at the centroid"
    );
}

#[test]
fn every_tracker_says_when_it_drops_a_track() {
    for algorithm in ["gnn", "mht"] {
        let mut t = Tracker::new(spec(algorithm)).unwrap();
        let mut out = run_all(&mut t, (0..6).map(loud_plot).collect());
        // The boat vanishes; the sensor keeps scanning (one plot far away).
        for s in 6..20 {
            let mut far = loud_plot(s);
            far.position.latitude += 0.05;
            out.extend(run_all(&mut t, vec![far]));
        }
        let ends: Vec<&Observation> = out
            .iter()
            .filter(|o| o.state == Some(ot_core::TrackState::Dropped))
            .collect();
        assert_eq!(ends.len(), 1, "{algorithm}: {ends:?}");
        let key = &out[0].source_track_key;
        assert_eq!(&ends[0].source_track_key, key, "{algorithm}");
        assert_eq!(
            ends[0].classification.affiliation,
            Some(Affiliation::Unknown)
        );
        // Nothing more under that key after its end.
        let after = out.iter().skip_while(|o| o.state.is_none()).skip(1);
        assert!(
            after.filter(|o| &o.source_track_key == key).count() == 0,
            "{algorithm}"
        );
    }
}

#[test]
fn rejects_nonsense_settings() {
    let mut s = spec("mht");
    s.mht.detection_probability = 1.0;
    assert!(Tracker::new(s).is_err());
    let mut s = spec("gnn");
    s.gate = 0.0;
    assert!(Tracker::new(s).is_err());
    assert!(
        serde_json::from_value::<TrackerSpec>(json!({"algorithm": "gnn", "domain": "space"}))
            .is_err()
    );
}

// --- Autoferry: real lidar and radar detections ---

#[derive(serde::Deserialize)]
struct Recorded {
    feed: String,
    truth: Option<u64>,
    obs: Observation,
}

/// Per target: the reports of its best track and that track's purity; and
/// how many confirmed tracks follow clutter.
fn score(
    algorithm: &str,
    scenario: u32,
    feed: &str,
    spec_patch: serde_json::Value,
) -> (Vec<(usize, f64)>, usize) {
    let path = format!(
        "{}/../ot-server/tests/data/autoferry/scenario{scenario}-detections.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    let rows: Vec<Recorded> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|r: &Recorded| r.feed == feed)
        .collect();
    let label: HashMap<String, Option<u64>> = rows
        .iter()
        .map(|r| (r.obs.source_track_key.clone(), r.truth))
        .collect();
    let mut s = spec(algorithm);
    let mut v = serde_json::to_value(&s).unwrap();
    for (k, x) in spec_patch.as_object().unwrap() {
        v[k] = x.clone();
    }
    s = serde_json::from_value(v).unwrap();
    let mut t = Tracker::new(s).unwrap();
    // Each scan as one frame, as the dataset has them.
    let mut tracks: HashMap<String, HashMap<Option<u64>, usize>> = HashMap::new();
    let mut i = 0;
    while i < rows.len() {
        let when = rows[i].obs.observed_at;
        while i < rows.len() && rows[i].obs.observed_at == when {
            t.push(rows[i].obs.clone(), when);
            i += 1;
        }
        for (o, det) in t.run(when, true) {
            *tracks
                .entry(o.source_track_key)
                .or_default()
                .entry(label[&det.source_track_key])
                .or_default() += 1;
        }
    }
    let mut best = Vec::new();
    for k in [1, 2] {
        let b = tracks
            .values()
            .filter_map(|c| {
                let total: usize = c.values().sum();
                let n = *c.get(&Some(k)).unwrap_or(&0);
                (n * 2 > total).then_some((n, n as f64 / total as f64))
            })
            .max_by_key(|x| x.0)
            .unwrap_or((0, 0.0));
        best.push(b);
    }
    let false_tracks = tracks
        .values()
        .filter(|c| {
            let total: usize = c.values().sum();
            *c.get(&None).unwrap_or(&0) * 2 > total
        })
        .count();
    eprintln!(
        "scenario {scenario} {feed} {algorithm}: best track per target (reports, purity) {best:?}; {false_tracks} clutter tracks; {} total",
        tracks.len()
    );
    (best, false_tracks)
}

#[test]
fn autoferry_detections_become_clean_tracks() {
    // Both vessels get a long, pure track from each sensor, by both algorithms.
    let lidar = json!({"measurement_sigma_m": 4.0, "domain": "surface", "cluster_m": 10.0});
    let radar = json!({"measurement_sigma_m": 6.0, "domain": "surface",
                       "mht": {"detection_probability": 0.9, "clutter_density": 4e-6,
                               "birth_density": 1e-7, "n_scan": 3, "max_branches": 20}});
    for algorithm in ["gnn", "mht"] {
        for scenario in [2, 16] {
            for (feed, patch, min_reports) in [("lidar-det", &lidar, 60), ("radar-det", &radar, 30)]
            {
                let (best, _) = score(algorithm, scenario, feed, patch.clone());
                for (target, (n, purity)) in best.iter().enumerate() {
                    assert!(
                        *n >= min_reports && *purity >= 0.95,
                        "{algorithm} scenario {scenario} {feed} target {}: {n} reports, purity {purity:.2}",
                        target + 1
                    );
                }
            }
        }
    }
}
