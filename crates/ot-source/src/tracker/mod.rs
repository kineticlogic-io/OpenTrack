//! The optional tracker stage: turns a source's detections (anonymous plots)
//! into source tracks before correlation, with global nearest neighbour
//! (GNN) or multiple hypothesis tracking (MHT) association.
//!
//! Both share one Kalman filter ([`filter`]) and one track lifecycle: a track
//! is confirmed after `confirm_hits` plots within `confirm_within_secs`, and
//! dropped after going `drop_tentative_secs` (unconfirmed) or
//! `drop_confirmed_secs` without one. Only confirmed tracks are reported,
//! each time a plot updates them.
//!
//! A tracker knows where something is and how it moves, not what it is. Its
//! tracks carry no identity, and at most an unknown affiliation and a domain
//! (see [`tracker_classification`]): every tracker's output goes through it.

pub mod assign;
pub mod filter;
mod gnn;
mod mht;

use chrono::{DateTime, Utc};
use ot_core::{
    Affiliation, Classification, Domain, Kinematics, Observation, Position, Uncertainty,
};
use serde::{Deserialize, Serialize};

use filter::Kf;

/// Settings of the tracker stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackerSpec {
    pub algorithm: Algorithm,
    /// Plot standard deviation per axis (m), when a detection reports no uncertainty.
    #[serde(default = "d_sigma")]
    pub measurement_sigma_m: f64,
    /// White-acceleration process noise (m/s²): how hard targets manoeuvre.
    #[serde(default = "d_q")]
    pub process_noise_mps2: f64,
    /// Velocity standard deviation of a new track (m/s).
    #[serde(default = "d_v0")]
    pub initial_speed_sigma_mps: f64,
    /// Chi-square gate (2 degrees of freedom; 13.8 keeps 99.9% of true plots).
    #[serde(default = "d_gate")]
    pub gate: f64,
    #[serde(default = "d_confirm_hits")]
    pub confirm_hits: usize,
    #[serde(default = "d_confirm_secs")]
    pub confirm_within_secs: f64,
    #[serde(default = "d_drop_tentative")]
    pub drop_tentative_secs: f64,
    #[serde(default = "d_drop_confirmed")]
    pub drop_confirmed_secs: f64,
    /// The domain of everything this sensor sees (a surface radar: surface).
    /// Without it, a track takes the domain most of its plots report, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<TrackerDomain>,
    /// What makes a scan: the plots of one frame, or plots with the same time.
    #[serde(default)]
    pub scans: ScanGrouping,
    /// With `scans: time`, how long to wait for more plots of the newest scan (s).
    #[serde(default = "d_hold")]
    pub scan_hold_secs: f64,
    /// Merge a scan's plots closer than this (m) into one, for sensors that
    /// return several points off one large object (lidar on a ship). 0: off.
    #[serde(default)]
    pub cluster_m: f64,
    /// Track keys are this prefix and a number (`T12`).
    #[serde(default = "d_prefix")]
    pub key_prefix: String,
    #[serde(default)]
    pub mht: MhtSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Algorithm {
    /// Global nearest neighbour: each scan, the least-cost assignment of plots to tracks.
    Gnn,
    /// Multiple hypothesis tracking: competing assignments kept for a few scans.
    Mht,
}

/// The domains a tracker may assign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackerDomain {
    Ground,
    Air,
    Surface,
    Subsurface,
}

impl TrackerDomain {
    const ALL: [TrackerDomain; 4] = [
        TrackerDomain::Ground,
        TrackerDomain::Air,
        TrackerDomain::Surface,
        TrackerDomain::Subsurface,
    ];

    pub fn domain(self) -> Domain {
        match self {
            TrackerDomain::Ground => Domain::Ground,
            TrackerDomain::Air => Domain::Air,
            TrackerDomain::Surface => Domain::Surface,
            TrackerDomain::Subsurface => Domain::Subsurface,
        }
    }

    fn of(d: Domain) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.domain() == d)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanGrouping {
    /// Each frame is a scan (plots with different times in it are separate scans).
    #[default]
    Frame,
    /// Plots with the same time are a scan, across frames.
    Time,
}

/// MHT settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MhtSpec {
    /// Chance the sensor detects a target in a scan.
    #[serde(default = "d_pd")]
    pub detection_probability: f64,
    /// False plots per square metre per scan.
    #[serde(default = "d_clutter")]
    pub clutter_density: f64,
    /// New targets per square metre per scan.
    #[serde(default = "d_birth")]
    pub birth_density: f64,
    /// Scans before an association is final.
    #[serde(default = "d_n_scan")]
    pub n_scan: usize,
    /// Hypotheses kept per possible target.
    #[serde(default = "d_branches")]
    pub max_branches: usize,
}

impl Default for MhtSpec {
    fn default() -> Self {
        serde_json::from_value(serde_json::json!({})).expect("defaults")
    }
}

fn d_sigma() -> f64 {
    10.0
}
fn d_q() -> f64 {
    0.5
}
fn d_v0() -> f64 {
    5.0
}
fn d_gate() -> f64 {
    13.8
}
fn d_confirm_hits() -> usize {
    3
}
fn d_confirm_secs() -> f64 {
    5.0
}
fn d_drop_tentative() -> f64 {
    3.0
}
fn d_drop_confirmed() -> f64 {
    8.0
}
fn d_hold() -> f64 {
    0.5
}
fn d_prefix() -> String {
    "T".into()
}
fn d_pd() -> f64 {
    0.9
}
fn d_clutter() -> f64 {
    1e-6
}
fn d_birth() -> f64 {
    1e-7
}
fn d_n_scan() -> usize {
    3
}
fn d_branches() -> usize {
    20
}

impl TrackerSpec {
    pub fn validate(&self) -> Result<(), String> {
        let positive = [
            ("measurement_sigma_m", self.measurement_sigma_m),
            ("process_noise_mps2", self.process_noise_mps2),
            ("initial_speed_sigma_mps", self.initial_speed_sigma_mps),
            ("gate", self.gate),
            ("confirm_within_secs", self.confirm_within_secs),
            ("drop_tentative_secs", self.drop_tentative_secs),
            ("drop_confirmed_secs", self.drop_confirmed_secs),
            ("mht.clutter_density", self.mht.clutter_density),
            ("mht.birth_density", self.mht.birth_density),
        ];
        for (name, v) in positive {
            if !(v.is_finite() && v > 0.0) {
                return Err(format!("tracker {name} must be a positive number"));
            }
        }
        let pd = self.mht.detection_probability;
        if !(pd > 0.0 && pd < 1.0) {
            return Err("tracker mht.detection_probability must be between 0 and 1".into());
        }
        if self.confirm_hits == 0 || self.mht.n_scan == 0 || self.mht.max_branches == 0 {
            return Err(
                "tracker confirm_hits, mht.n_scan and mht.max_branches must be at least 1".into(),
            );
        }
        if !(self.scan_hold_secs >= 0.0 && self.cluster_m >= 0.0) {
            return Err("tracker scan_hold_secs and cluster_m must not be negative".into());
        }
        Ok(())
    }
}

/// The only classification any tracker gives a track: unknown affiliation,
/// and a ground, air, surface or subsurface domain when it has one. Never an
/// identity, a platform type or a more specific symbol.
pub fn tracker_classification(domain: Option<Domain>) -> Classification {
    Classification {
        affiliation: Some(Affiliation::Unknown),
        domain: domain
            .and_then(TrackerDomain::of)
            .map(TrackerDomain::domain),
        cot_type: None,
        sidc: None,
    }
}

/// One detection as the trackers see it.
#[derive(Debug, Clone, Copy)]
pub struct Plot {
    pub lat: f64,
    pub lon: f64,
    /// Standard deviation per axis, metres.
    pub sigma: f64,
    pub domain: Option<TrackerDomain>,
}

/// A confirmed track updated by a plot in this scan.
#[derive(Debug, Clone)]
pub struct Report {
    pub id: u64,
    pub kf: Kf,
    pub domain: Option<TrackerDomain>,
    /// Index of the plot that updated it.
    pub plot: usize,
}

/// A track's confirmation, age and the domains its plots reported.
#[derive(Debug, Clone)]
struct Life {
    hits: Vec<DateTime<Utc>>,
    confirmed: bool,
    domains: [u32; 4],
}

impl Life {
    fn new(t: DateTime<Utc>, plot: &Plot, spec: &TrackerSpec) -> Self {
        let mut l = Self {
            hits: Vec::new(),
            confirmed: false,
            domains: [0; 4],
        };
        l.hit(t, plot, spec);
        l
    }

    fn hit(&mut self, t: DateTime<Utc>, plot: &Plot, spec: &TrackerSpec) {
        self.hits.push(t);
        let window = secs(spec.confirm_within_secs);
        let recent = self.hits.iter().filter(|h| t - **h <= window).count();
        self.confirmed |= recent >= spec.confirm_hits;
        // Only the recent hits matter (confirmation and the last one).
        if self.hits.len() > spec.confirm_hits.max(2) * 2 {
            self.hits.remove(0);
        }
        if let Some(d) = plot.domain {
            let i = TrackerDomain::ALL
                .iter()
                .position(|x| *x == d)
                .expect("listed");
            self.domains[i] += 1;
        }
    }

    fn alive(&self, t: DateTime<Utc>, spec: &TrackerSpec) -> bool {
        let limit = if self.confirmed {
            spec.drop_confirmed_secs
        } else {
            spec.drop_tentative_secs
        };
        self.hits.last().is_some_and(|h| t - *h <= secs(limit))
    }

    /// The configured domain, else the one most plots reported.
    fn domain(&self, spec: &TrackerSpec) -> Option<TrackerDomain> {
        spec.domain.or_else(|| {
            let (i, n) = self
                .domains
                .iter()
                .enumerate()
                .max_by_key(|(_, n)| **n)
                .expect("four domains");
            (*n > 0).then_some(TrackerDomain::ALL[i])
        })
    }
}

fn secs(s: f64) -> chrono::Duration {
    chrono::Duration::microseconds((s * 1e6) as i64)
}

/// Scan-by-scan association: GNN or MHT.
trait Associate: Send {
    fn scan(&mut self, t: DateTime<Utc>, plots: &[Plot]) -> Vec<Report>;
}

/// The tracker stage of one source's pipeline.
pub struct Tracker {
    spec: TrackerSpec,
    inner: Box<dyn Associate>,
    /// Plots waiting for their scan to complete, with when they arrived.
    buffer: Vec<(Observation, DateTime<Utc>)>,
    last_scan: Option<DateTime<Utc>>,
    /// Plots older than the last processed scan, dropped.
    pub late: u64,
}

impl Tracker {
    pub fn new(spec: TrackerSpec) -> Result<Self, String> {
        spec.validate()?;
        let inner: Box<dyn Associate> = match spec.algorithm {
            Algorithm::Gnn => Box::new(gnn::Gnn::new(spec.clone())),
            Algorithm::Mht => Box::new(mht::Mht::new(spec.clone())),
        };
        Ok(Self {
            spec,
            inner,
            buffer: Vec::new(),
            last_scan: None,
            late: 0,
        })
    }

    pub fn spec(&self) -> &TrackerSpec {
        &self.spec
    }

    /// Queue a detection.
    pub fn push(&mut self, obs: Observation, received_at: DateTime<Utc>) {
        self.buffer.push((obs, received_at));
    }

    /// Run every complete scan (and with `force`, the newest one too),
    /// returning track reports and the detection behind each.
    pub fn run(&mut self, now: DateTime<Utc>, force: bool) -> Vec<(Observation, Observation)> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let mut buffer = std::mem::take(&mut self.buffer);
        buffer.sort_by_key(|(o, _)| o.observed_at);
        let newest = buffer
            .last()
            .map(|(o, _)| o.observed_at)
            .expect("non-empty");
        let hold = self.spec.scans == ScanGrouping::Time && !force && {
            let arrived = buffer
                .iter()
                .filter(|(o, _)| o.observed_at == newest)
                .map(|(_, r)| *r)
                .max()
                .expect("non-empty");
            now - arrived < secs(self.spec.scan_hold_secs)
        };
        let mut out = Vec::new();
        let mut i = 0;
        while i < buffer.len() {
            let t = buffer[i].0.observed_at;
            let j = i + buffer[i..]
                .iter()
                .take_while(|(o, _)| o.observed_at == t)
                .count();
            if hold && t == newest {
                self.buffer.extend(buffer.drain(i..));
                break;
            }
            let scan: Vec<Observation> = buffer[i..j].iter().map(|(o, _)| o.clone()).collect();
            i = j;
            if self.last_scan.is_some_and(|l| t <= l) {
                self.late += scan.len() as u64;
                continue;
            }
            self.last_scan = Some(t);
            let (plots, first) = self.plots(&scan);
            for r in self.inner.scan(t, &plots) {
                let det = &scan[first[r.plot]];
                out.push((self.report(&r, det), det.clone()));
            }
        }
        out
    }

    /// A scan's plots, merged within `cluster_m` (at their centroid, with the
    /// smallest sigma), and for each the first detection in it.
    fn plots(&self, scan: &[Observation]) -> (Vec<Plot>, Vec<usize>) {
        let mut groups: Vec<(Vec<usize>, f64, f64)> = Vec::new();
        for (i, o) in scan.iter().enumerate() {
            let (lat, lon) = (o.position.latitude, o.position.longitude);
            let near = if self.spec.cluster_m > 0.0 {
                groups.iter().position(|(_, glat, glon)| {
                    let (dn, de) = filter::offset_m(*glat, *glon, lat, lon);
                    dn.hypot(de) < self.spec.cluster_m
                })
            } else {
                None
            };
            match near {
                Some(g) => {
                    let (members, glat, glon) = &mut groups[g];
                    let n = members.len() as f64;
                    *glat = (*glat * n + lat) / (n + 1.0);
                    *glon = (*glon * n + lon) / (n + 1.0);
                    members.push(i);
                }
                None => groups.push((vec![i], lat, lon)),
            }
        }
        groups
            .into_iter()
            .map(|(members, lat, lon)| {
                let each: Vec<Plot> = members.iter().map(|&i| self.plot(&scan[i])).collect();
                let sigma = each.iter().map(|p| p.sigma).fold(f64::INFINITY, f64::min);
                let domain = each.iter().find_map(|p| p.domain);
                (
                    Plot {
                        lat,
                        lon,
                        sigma,
                        domain,
                    },
                    members[0],
                )
            })
            .unzip()
    }

    fn plot(&self, o: &Observation) -> Plot {
        Plot {
            lat: o.position.latitude,
            lon: o.position.longitude,
            sigma: o
                .uncertainty
                .and_then(|u| u.cep_m())
                .map(|c| c / 1.1774)
                .unwrap_or(self.spec.measurement_sigma_m),
            domain: o
                .classification
                .effective_domain()
                .and_then(TrackerDomain::of),
        }
    }

    /// A track report as an observation: position and motion from the
    /// filter, the source and sensor details of the detection, and nothing
    /// that says what the object is beyond [`tracker_classification`].
    fn report(&self, r: &Report, det: &Observation) -> Observation {
        let (course, speed) = r.kf.course_speed();
        Observation {
            schema_version: det.schema_version,
            source_id: det.source_id.clone(),
            source_track_key: format!("{}{}", self.spec.key_prefix, r.id),
            identifiers: Vec::new(),
            name: None,
            callsign: None,
            observed_at: r.kf.t,
            received_at: det.received_at,
            position: Position {
                latitude: r.kf.lat,
                longitude: r.kf.lon,
                altitude_hae_m: det.position.altitude_hae_m,
            },
            uncertainty: Some(Uncertainty {
                circular_error_m: Some((r.kf.cep_m() * 10.0).round() / 10.0),
                ..Default::default()
            }),
            kinematics: Kinematics {
                course_deg: Some((course * 10.0).round() / 10.0),
                speed_mps: Some((speed * 100.0).round() / 100.0),
                ..Default::default()
            },
            classification: tracker_classification(r.domain.map(TrackerDomain::domain)),
            platform: Default::default(),
            provenance: det.provenance.clone(),
            state: None,
            track_type: det.track_type,
            ext: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests;
