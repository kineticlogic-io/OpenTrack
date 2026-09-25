//! Correlation: deciding which source tracks report the same object, and
//! building a system track's view from its contributors.
//!
//! Pure functions over observations, so every rule is unit-tested without
//! Redis or SQLite. The engine calls them and records each decision with the
//! evidence these functions return.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ot_core::{Domain, Observation};
use serde::Serialize;

const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Keys under which an observation claims an identity: each identifier as
/// `<scheme>:<value>` (lowercase), and `entity:<id>` when the registry
/// resolved it to an entity with corroboration. Two tracks sharing a key
/// are the same object, subject to the sanity gate.
pub fn identity_keys(obs: &Observation) -> Vec<String> {
    let mut out: Vec<String> = obs
        .identifiers
        .iter()
        .filter(|i| !i.value.trim().is_empty())
        .map(|i| {
            format!(
                "{}:{}",
                i.scheme.trim().to_lowercase(),
                i.value.trim().to_lowercase()
            )
        })
        .collect();
    if let Some(id) = trusted_entity(obs) {
        out.push(format!("entity:{id}"));
    }
    out.sort();
    out.dedup();
    out
}

/// The registry entity an observation resolved to, when the match is
/// trustworthy: corroborated (`applied` on older records) and without
/// conflicting identifiers. The same rule decides whether a card applies.
pub fn trusted_entity(obs: &Observation) -> Option<String> {
    let r = obs.ext.get("registry")?;
    let trusted = r
        .get("corroborated")
        .or_else(|| r.get("applied"))
        .and_then(|v| v.as_bool())
        == Some(true);
    let conflicted = r
        .get("conflicts")
        .and_then(|c| c.as_array())
        .is_some_and(|c| !c.is_empty());
    (trusted && !conflicted)
        .then(|| {
            r.get("entity_id")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .flatten()
}

/// Great-circle distance in metres.
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().asin()
}

/// Where `obs` would be at `at`, moving at its reported course and speed
/// (for at most `max_secs`; unchanged when it reports neither).
pub fn dead_reckon(obs: &Observation, at: DateTime<Utc>, max_secs: f64) -> (f64, f64) {
    let (lat, lon) = (obs.position.latitude, obs.position.longitude);
    let (Some(course), Some(speed)) = (obs.kinematics.course_deg, obs.kinematics.speed_mps) else {
        return (lat, lon);
    };
    let dt = ((at - obs.observed_at).num_milliseconds() as f64 / 1000.0).clamp(-max_secs, max_secs);
    let d = speed * dt;
    let (c, lat_r) = (course.to_radians(), lat.to_radians());
    let dlat = d * c.cos() / EARTH_RADIUS_M;
    let dlon = d * c.sin() / (EARTH_RADIUS_M * lat_r.cos().max(1e-6));
    (lat + dlat.to_degrees(), lon + dlon.to_degrees())
}

/// The fastest a platform of this domain plausibly moves (m/s), for the
/// sanity gate's allowance over time.
fn max_speed(domain: Option<Domain>) -> f64 {
    match domain {
        Some(Domain::Surface) | Some(Domain::Subsurface) => 40.0,
        Some(Domain::Ground) => 70.0,
        Some(Domain::Space) => 8_000.0,
        Some(Domain::Air) | None => 400.0,
    }
}

/// Settings of the kinematic sanity gate on identifier matches.
#[derive(Debug, Clone, Copy)]
pub struct GateSettings {
    /// Allowed distance at zero time difference (reporting error, lag).
    pub base_m: f64,
    /// Longest dead-reckoning extrapolation.
    pub max_extrapolation_secs: f64,
}

impl Default for GateSettings {
    fn default() -> Self {
        Self {
            base_m: 10_000.0,
            max_extrapolation_secs: 600.0,
        }
    }
}

/// Whether a track reported at `obs` can be the object a system track shows
/// in `view`: the distance after dead-reckoning the view to the observation's
/// time must be within `base + max speed × time apart`. Stops a reassigned or
/// spoofed identifier from pulling two distant objects together.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Gate {
    pub distance_m: f64,
    pub dt_s: f64,
    pub gate_m: f64,
    pub pass: bool,
}

pub fn sanity_gate(obs: &Observation, view: &Observation, s: &GateSettings) -> Gate {
    let (lat, lon) = dead_reckon(view, obs.observed_at, s.max_extrapolation_secs);
    let distance_m = distance_m(obs.position.latitude, obs.position.longitude, lat, lon);
    let dt_s = ((obs.observed_at - view.observed_at).num_milliseconds() as f64 / 1000.0).abs();
    let domain = obs
        .classification
        .effective_domain()
        .or(view.classification.effective_domain());
    let gate_m = s.base_m + max_speed(domain) * dt_s;
    Gate {
        distance_m: distance_m.round(),
        dt_s: (dt_s * 10.0).round() / 10.0,
        gate_m: gate_m.round(),
        pass: distance_m <= gate_m,
    }
}

/// Settings of kinematic pairing (tracks with no shared identity) and of
/// detection association.
#[derive(Debug, Clone, Copy)]
pub struct KinematicSettings {
    /// Chi-square gate on the normalised distance (2 degrees of freedom;
    /// 9.21 keeps 99% of true matches).
    pub chi2_gate: f64,
    /// Floor on a report's position standard deviation, for sources that
    /// claim more precision than they have (or report none).
    pub min_sigma_m: f64,
    /// Position uncertainty growth per second of dead-reckoning (m/s).
    pub drift_mps: f64,
    /// Pair when `m` of the last `n` comparisons pass the gate...
    pub m: usize,
    pub n: usize,
    /// ...within this many seconds.
    pub window_secs: f64,
    /// Views older than this are not compared.
    pub max_age_secs: f64,
}

impl Default for KinematicSettings {
    fn default() -> Self {
        Self {
            chi2_gate: 9.21,
            min_sigma_m: 5.0,
            drift_mps: 1.0,
            m: 4,
            n: 5,
            window_secs: 30.0,
            max_age_secs: 30.0,
        }
    }
}

/// How pairing decides, beyond shared identifiers (which always pair).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Approach {
    /// Only shared identifiers pair tracks.
    Identifiers,
    /// Kinematic agreement over time, whatever else the tracks report.
    Kinematics,
    /// Kinematic agreement, vetoed by conflicting identifiers or domains.
    #[default]
    KinematicsMetadata,
}

/// A report's position standard deviation per axis: its circular error
/// (CEP = 1.1774 σ for a circular normal), no smaller than the floor.
fn sigma_m(obs: &Observation, floor: f64) -> f64 {
    obs.uncertainty
        .and_then(|u| u.cep_m())
        .map(|c| c / 1.1774)
        .unwrap_or(floor)
        .max(floor)
}

/// How well a report agrees with a view: the view dead-reckoned to the
/// report's time, and the distance normalised by both uncertainties.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Kinematic {
    pub distance_m: f64,
    pub dt_s: f64,
    /// Combined standard deviation per axis.
    pub sigma_m: f64,
    /// Squared normalised distance (chi-square, 2 degrees of freedom).
    pub d2: f64,
    pub pass: bool,
}

pub fn kinematic(obs: &Observation, view: &Observation, s: &KinematicSettings) -> Kinematic {
    let (lat, lon) = dead_reckon(view, obs.observed_at, s.max_age_secs);
    let distance = distance_m(obs.position.latitude, obs.position.longitude, lat, lon);
    let dt = ((obs.observed_at - view.observed_at).num_milliseconds() as f64 / 1000.0).abs();
    let var = sigma_m(obs, s.min_sigma_m).powi(2)
        + sigma_m(view, s.min_sigma_m).powi(2)
        + (s.drift_mps * dt).powi(2);
    let d2 = distance * distance / var;
    Kinematic {
        distance_m: (distance * 10.0).round() / 10.0,
        dt_s: (dt * 10.0).round() / 10.0,
        sigma_m: (var.sqrt() * 10.0).round() / 10.0,
        d2: (d2 * 100.0).round() / 100.0,
        pass: d2 <= s.chi2_gate,
    }
}

/// Why two tracks cannot be the same object whatever their kinematics:
/// different values for the same identifier scheme, or different domains.
pub fn veto(a: &Observation, b: &Observation) -> Option<String> {
    for x in &a.identifiers {
        let scheme = x.scheme.trim().to_lowercase();
        for y in &b.identifiers {
            if y.scheme.trim().to_lowercase() == scheme
                && !x.value.trim().eq_ignore_ascii_case(y.value.trim())
            {
                return Some(format!("{scheme} {} vs {}", x.value.trim(), y.value.trim()));
            }
        }
    }
    match (
        a.classification.effective_domain(),
        b.classification.effective_domain(),
    ) {
        (Some(x), Some(y)) if x != y => Some(format!("domain {x:?} vs {y:?}")),
        _ => None,
    }
}

/// The recent gate results of one pair of tracks, for M-of-N persistence.
#[derive(Debug, Clone, Default)]
pub struct Persistence {
    results: std::collections::VecDeque<(DateTime<Utc>, bool)>,
}

impl Persistence {
    /// Record a comparison at `at`; returns (passes, comparisons) over the
    /// last `n` within the window.
    pub fn record(
        &mut self,
        at: DateTime<Utc>,
        pass: bool,
        s: &KinematicSettings,
    ) -> (usize, usize) {
        self.results.push_back((at, pass));
        let window = chrono::Duration::milliseconds((s.window_secs * 1000.0) as i64);
        let newest = self.results.iter().map(|r| r.0).max().unwrap_or(at);
        self.results.retain(|r| newest - r.0 <= window);
        while self.results.len() > s.n {
            self.results.pop_front();
        }
        let hits = self.results.iter().filter(|r| r.1).count();
        (hits, self.results.len())
    }

    /// The newest comparison, to forget pairs that stopped being compared.
    pub fn last(&self) -> Option<DateTime<Utc>> {
        self.results.back().map(|r| r.0)
    }
}

/// A coarse spatial index of system tracks (0.05° cells, about 5 km of
/// latitude), so pairing and association compare against neighbours only.
#[derive(Debug)]
pub struct Grid<K> {
    cell_of: std::collections::HashMap<K, (i32, i32)>,
    cells: std::collections::HashMap<(i32, i32), Vec<K>>,
}

impl<K> Default for Grid<K> {
    fn default() -> Self {
        Self {
            cell_of: Default::default(),
            cells: Default::default(),
        }
    }
}

const CELL_DEG: f64 = 0.05;

fn cell(lat: f64, lon: f64) -> (i32, i32) {
    (
        (lat / CELL_DEG).floor() as i32,
        (lon / CELL_DEG).floor() as i32,
    )
}

impl<K: Copy + Eq + std::hash::Hash> Grid<K> {
    pub fn put(&mut self, k: K, lat: f64, lon: f64) {
        let c = cell(lat, lon);
        if self.cell_of.get(&k) == Some(&c) {
            return;
        }
        self.remove(k);
        self.cell_of.insert(k, c);
        self.cells.entry(c).or_default().push(k);
    }

    pub fn remove(&mut self, k: K) {
        if let Some(c) = self.cell_of.remove(&k)
            && let Some(v) = self.cells.get_mut(&c)
        {
            v.retain(|x| *x != k);
            if v.is_empty() {
                self.cells.remove(&c);
            }
        }
    }

    /// Everything in the cell of (lat, lon) and the eight around it.
    pub fn near(&self, lat: f64, lon: f64) -> Vec<K> {
        let (a, b) = cell(lat, lon);
        let mut out = Vec::new();
        for i in a - 1..=a + 1 {
            for j in b - 1..=b + 1 {
                if let Some(v) = self.cells.get(&(i, j)) {
                    out.extend(v.iter().copied());
                }
            }
        }
        out
    }
}

/// One contributor's latest report, for best-source selection.
#[derive(Debug, Clone, Copy)]
pub struct Contribution<'a> {
    pub obs: &'a Observation,
    /// The source's priority; higher wins.
    pub priority: i64,
}

impl Contribution<'_> {
    fn key(&self) -> String {
        format!("{}/{}", self.obs.source_id, self.obs.source_track_key)
    }

    fn corroborated(&self) -> bool {
        trusted_entity(self.obs).is_some()
    }
}

/// Specificity of a CoT type: its number of atoms (`a-f-S-C-L` beats `a-f-S`).
fn cot_specificity(cot: Option<&str>) -> usize {
    cot.map(|c| c.split('-').filter(|a| !a.is_empty()).count())
        .unwrap_or(0)
}

/// The system track's view from its contributors' latest reports, and which
/// contributor supplied each field group:
///
/// - `position` (position, kinematics, uncertainty, time): among reports
///   within `window_secs` of the newest, the smallest uncertainty, then the
///   highest priority, then the newest.
/// - `identity` (name, callsign, platform): a registry-corroborated report,
///   else the highest priority, else the newest; gaps filled from the others
///   in the same order.
/// - `classification`: a registry-corroborated report's, else the most
///   specific CoT type, then priority.
/// - extension fields: each from the highest-priority report that has it.
/// - identifiers: every contributor's.
pub fn best_view(
    contribs: &[Contribution<'_>],
    window_secs: f64,
) -> (Observation, BTreeMap<String, String>) {
    assert!(
        !contribs.is_empty(),
        "a system track has at least one contributor"
    );
    let mut provenance = BTreeMap::new();
    if contribs.len() == 1 {
        return (contribs[0].obs.clone(), provenance);
    }
    let newest = contribs
        .iter()
        .map(|c| c.obs.observed_at)
        .max()
        .expect("non-empty");
    let window = chrono::Duration::milliseconds((window_secs * 1000.0) as i64);
    let cep = |c: &Contribution<'_>| {
        c.obs
            .uncertainty
            .and_then(|u| u.cep_m())
            .unwrap_or(f64::INFINITY)
    };

    let position = contribs
        .iter()
        .filter(|c| newest - c.obs.observed_at <= window)
        .min_by(|a, b| {
            cep(a)
                .total_cmp(&cep(b))
                .then(b.priority.cmp(&a.priority))
                .then(b.obs.observed_at.cmp(&a.obs.observed_at))
        })
        .expect("the newest report is within the window");
    let mut view = position.obs.clone();
    provenance.insert("position".into(), position.key());

    // Identity: ordered by preference, the first supplies, the rest fill gaps.
    let mut by_identity: Vec<&Contribution<'_>> = contribs.iter().collect();
    by_identity.sort_by(|a, b| {
        b.corroborated()
            .cmp(&a.corroborated())
            .then(b.priority.cmp(&a.priority))
            .then(b.obs.observed_at.cmp(&a.obs.observed_at))
    });
    let lead = by_identity[0];
    view.name = by_identity.iter().find_map(|c| c.obs.name.clone());
    view.callsign = by_identity.iter().find_map(|c| c.obs.callsign.clone());
    let pick =
        |f: fn(&Observation) -> &Option<String>| by_identity.iter().find_map(|c| f(c.obs).clone());
    view.platform.name = pick(|o| &o.platform.name);
    view.platform.class = pick(|o| &o.platform.class);
    view.platform.type_code = pick(|o| &o.platform.type_code);
    view.platform.flag = pick(|o| &o.platform.flag);
    view.platform.hull = pick(|o| &o.platform.hull);
    provenance.insert("identity".into(), lead.key());

    let class = contribs
        .iter()
        .max_by(|a, b| {
            a.corroborated()
                .cmp(&b.corroborated())
                .then(
                    cot_specificity(a.obs.classification.cot_type.as_deref())
                        .cmp(&cot_specificity(b.obs.classification.cot_type.as_deref())),
                )
                .then(a.priority.cmp(&b.priority))
        })
        .expect("non-empty");
    view.classification = class.obs.classification.clone();
    provenance.insert("classification".into(), class.key());

    let mut by_priority: Vec<&Contribution<'_>> = contribs.iter().collect();
    by_priority.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(b.obs.observed_at.cmp(&a.obs.observed_at))
    });
    let mut ext = serde_json::Map::new();
    for c in &by_priority {
        for (k, v) in &c.obs.ext {
            if !v.is_null() && !ext.contains_key(k) {
                ext.insert(k.clone(), v.clone());
            }
        }
    }
    view.ext = ext;

    let mut identifiers = Vec::new();
    for c in &by_priority {
        for id in &c.obs.identifiers {
            if !identifiers.contains(id) {
                identifiers.push(id.clone());
            }
        }
    }
    view.identifiers = identifiers;
    (view, provenance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ot_core::{Identifier, Uncertainty};
    use serde_json::json;

    /// An observation with no kinematics, identifiers, classification or extensions.
    fn sample() -> Observation {
        serde_json::from_value(json!({
            "schema_version": 1,
            "source_id": "s",
            "source_track_key": "k",
            "observed_at": "2026-09-24T12:00:00Z",
            "received_at": "2026-09-24T12:00:00Z",
            "position": {"latitude": 0.0, "longitude": 0.0}
        }))
        .expect("minimal observation")
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    fn obs(source: &str, key: &str, t: i64, lat: f64, lon: f64) -> Observation {
        let mut o = sample();
        o.source_id = source.into();
        o.source_track_key = key.into();
        o.observed_at = at(t);
        o.position.latitude = lat;
        o.position.longitude = lon;
        o.kinematics.course_deg = None;
        o.kinematics.speed_mps = None;
        o.ext = Default::default();
        o
    }

    #[test]
    fn identity_keys_normalise_and_include_corroborated_entities() {
        let mut o = obs("ais", "1", 0, 0.0, 0.0);
        o.identifiers = vec![
            Identifier::new("MMSI", " 366123456 "),
            Identifier::new("imo", ""),
        ];
        o.ext.insert(
            "registry".into(),
            json!({"entity_id": "ent-1", "corroborated": true}),
        );
        assert_eq!(identity_keys(&o), ["entity:ent-1", "mmsi:366123456"]);
        o.ext.insert(
            "registry".into(),
            json!({"entity_id": "ent-1", "corroborated": false}),
        );
        assert_eq!(identity_keys(&o), ["mmsi:366123456"]);
    }

    #[test]
    fn dead_reckoning_moves_along_course() {
        let mut o = obs("ais", "1", 0, 0.0, 0.0);
        o.kinematics.course_deg = Some(90.0);
        o.kinematics.speed_mps = Some(10.0);
        let (lat, lon) = dead_reckon(&o, at(100), 600.0);
        assert!(lat.abs() < 1e-9);
        assert!((distance_m(0.0, 0.0, lat, lon) - 1000.0).abs() < 1.0);
    }

    #[test]
    fn sanity_gate_grows_with_time_and_domain() {
        let view = obs("ais", "1", 0, 32.0, -117.0);
        // 20 km away at the same moment: too far for any domain.
        let far = obs("tak", "x", 0, 32.18, -117.0);
        assert!(!sanity_gate(&far, &view, &GateSettings::default()).pass);
        // The same 20 km five minutes later: an aircraft could, a ship could not.
        let mut later = obs("tak", "x", 300, 32.18, -117.0);
        later.classification.domain = Some(Domain::Air);
        assert!(sanity_gate(&later, &view, &GateSettings::default()).pass);
        later.classification.domain = Some(Domain::Surface);
        let g = sanity_gate(&later, &view, &GateSettings::default());
        assert!(g.pass, "{g:?}: 10 km + 40 m/s × 300 s = 22 km");
        let mut far_later = obs("tak", "x", 60, 32.18, -117.0);
        far_later.classification.domain = Some(Domain::Surface);
        assert!(!sanity_gate(&far_later, &view, &GateSettings::default()).pass);
    }

    #[test]
    fn best_view_takes_each_group_from_the_best_contributor() {
        let mut ais = obs("ais", "366", 0, 32.0, -117.0);
        ais.uncertainty = Some(Uncertainty {
            circular_error_m: Some(10.0),
            ..Default::default()
        });
        ais.name = Some("TED STEVENS".into());
        ais.classification.cot_type = Some("a-f-S".into());
        ais.identifiers = vec![Identifier::new("mmsi", "366")];
        ais.ext.insert("imo".into(), json!("9876543"));

        let mut radar = obs("radar", "t7", 5, 32.001, -117.0);
        radar.uncertainty = Some(Uncertainty {
            circular_error_m: Some(150.0),
            ..Default::default()
        });
        radar.classification.cot_type = Some("a-f-S-C-L-D-D".into());
        radar.identifiers = vec![Identifier::new("track", "t7")];
        radar.ext.insert("imo".into(), json!("0000000"));
        radar.ext.insert("rcs".into(), json!(12));

        let (v, p) = best_view(
            &[
                Contribution {
                    obs: &ais,
                    priority: 50,
                },
                Contribution {
                    obs: &radar,
                    priority: 100,
                },
            ],
            60.0,
        );
        // Both within the window: AIS has the smaller uncertainty.
        assert_eq!(p["position"], "ais/366");
        assert_eq!(v.position.latitude, 32.0);
        // Radar has the higher priority, but no name: AIS fills it.
        assert_eq!(p["identity"], "radar/t7");
        assert_eq!(v.name.as_deref(), Some("TED STEVENS"));
        assert_eq!(p["classification"], "radar/t7");
        assert_eq!(v.classification.cot_type.as_deref(), Some("a-f-S-C-L-D-D"));
        assert_eq!(v.ext["imo"], "0000000");
        assert_eq!(v.ext["rcs"], 12);
        assert_eq!(v.identifiers.len(), 2);

        // AIS a minute and a half older: outside the window, radar supplies position.
        let mut old = ais.clone();
        old.observed_at = at(-90);
        let (_, p) = best_view(
            &[
                Contribution {
                    obs: &old,
                    priority: 50,
                },
                Contribution {
                    obs: &radar,
                    priority: 100,
                },
            ],
            60.0,
        );
        assert_eq!(p["position"], "radar/t7");

        // A registry-corroborated report supplies identity and classification.
        let mut reg = ais.clone();
        reg.ext.insert(
            "registry".into(),
            json!({"entity_id": "ent-1", "corroborated": true}),
        );
        let (_, p) = best_view(
            &[
                Contribution {
                    obs: &reg,
                    priority: 50,
                },
                Contribution {
                    obs: &radar,
                    priority: 100,
                },
            ],
            60.0,
        );
        assert_eq!(p["identity"], "ais/366");
        assert_eq!(p["classification"], "ais/366");
    }

    #[test]
    fn kinematic_gate_normalises_by_uncertainty_and_time() {
        let s = KinematicSettings::default();
        let mut view = obs("track", "1", 0, 63.44, 10.40);
        view.uncertainty = Some(Uncertainty {
            circular_error_m: Some(10.0),
            ..Default::default()
        });
        // 20 m apart, both about 8.5 m sigma: d2 = 400 / 144 < 9.21.
        let mut radar = obs("radar", "r", 0, 63.44018, 10.40);
        radar.uncertainty = view.uncertainty;
        let k = kinematic(&radar, &view, &s);
        assert!(k.pass, "{k:?}");
        // 60 m apart: outside.
        radar.position.latitude = 63.44054;
        assert!(!kinematic(&radar, &view, &s).pass);
        // The same 60 m after 60 s without kinematics: the drift allowance admits it.
        radar.observed_at = at(60);
        assert!(kinematic(&radar, &view, &s).pass);
    }

    #[test]
    fn vetoes_conflicting_identifiers_and_domains() {
        let mut a = obs("ais", "1", 0, 0.0, 0.0);
        let mut b = obs("tak", "2", 0, 0.0, 0.0);
        assert_eq!(veto(&a, &b), None);
        a.identifiers = vec![Identifier::new("mmsi", "366")];
        b.identifiers = vec![Identifier::new("MMSI", "367")];
        assert_eq!(veto(&a, &b).as_deref(), Some("mmsi 366 vs 367"));
        b.identifiers = vec![Identifier::new("hull", "DDG-51")];
        assert_eq!(veto(&a, &b), None);
        a.classification.domain = Some(Domain::Surface);
        b.classification.domain = Some(Domain::Air);
        assert!(veto(&a, &b).is_some());
    }

    #[test]
    fn persistence_counts_m_of_n_within_the_window() {
        let s = KinematicSettings::default();
        let mut p = Persistence::default();
        assert_eq!(p.record(at(0), true, &s), (1, 1));
        assert_eq!(p.record(at(2), false, &s), (1, 2));
        p.record(at(4), true, &s);
        p.record(at(6), true, &s);
        assert_eq!(p.record(at(8), true, &s), (4, 5));
        // Only the last five count.
        assert_eq!(p.record(at(10), false, &s), (3, 5));
        // Results older than the window fall out.
        assert_eq!(p.record(at(60), true, &s), (1, 1));
    }

    #[test]
    fn grid_finds_neighbours_across_cells() {
        let mut g = Grid::default();
        g.put(1, 63.449, 10.449);
        g.put(2, 63.451, 10.451);
        g.put(3, 64.0, 10.4);
        let mut near = g.near(63.45, 10.45);
        near.sort();
        assert_eq!(near, [1, 2]);
        g.put(2, 64.0, 10.4);
        assert_eq!(g.near(63.45, 10.45), [1]);
        g.remove(1);
        assert!(g.near(63.45, 10.45).is_empty());
    }
}
