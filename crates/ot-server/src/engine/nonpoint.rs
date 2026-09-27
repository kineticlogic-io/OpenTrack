//! Lines of bearing (see `docs/non-point-contacts.md`).
//!
//! A bearing goes to the one track that lies along it (within its gate,
//! clearly better than any other, its emitter identity not in conflict),
//! where it adds evidence and identity but never moves the track. A bearing
//! no track takes waits, for a minute, to be crossed with bearings from
//! other sensors into a position: a fix, which enters the picture as a
//! report of the built-in source `fix`. Ghosts (lines crossing where
//! nothing is) are kept out: bearings with an emitter identity only fix
//! with the same identity; without one, a fix needs three sensors to agree.

use chrono::{DateTime, Utc};
use ot_core::geometry::{bearing_to, local, offset};
use ot_core::{BearingContact, Geometry, Observation, Uid};
use serde_json::json;

use super::{Engine, EngineCounts};

/// Bearings wait this long to be crossed.
const WINDOW_S: i64 = 60;
/// Associated bearings are shown for this long.
const KEEP_S: i64 = 300;
/// Three sigma, one degree of freedom.
const GATE_D2: f64 = 9.0;
/// How much better the best track must fit than the next.
const AMBIGUITY: f64 = 10.0;
/// Lines must cross at least this steeply for a useful fix.
const MIN_CROSS_DEG: f64 = 20.0;
/// Sensors closer than this are one position (no baseline).
const MIN_BASELINE_M: f64 = 200.0;
/// A sensor's range when it does not say.
const DEFAULT_RANGE_M: f64 = 250_000.0;
/// Source id of cross-fixes.
pub const FIX: &str = "fix";

/// A bearing's parts.
#[derive(Debug, Clone)]
struct Lob {
    obs: Observation,
    bearing: f64,
    sigma: f64,
    range: f64,
}

impl Lob {
    fn of(obs: Observation) -> Option<Self> {
        match obs.geometry {
            Some(Geometry::Bearing {
                bearing_deg,
                sigma_deg,
                max_range_m,
                ..
            }) => Some(Self {
                bearing: bearing_deg,
                sigma: sigma_deg,
                range: max_range_m.unwrap_or(DEFAULT_RANGE_M),
                obs,
            }),
            _ => None,
        }
    }

    fn origin(&self) -> (f64, f64) {
        (self.obs.position.latitude, self.obs.position.longitude)
    }

    /// The emitter identity it carries, e.g. `elnot:A123`.
    fn identity(&self) -> Option<String> {
        identity(&self.obs.identifiers)
    }
}

fn identity(ids: &[ot_core::schema::Identifier]) -> Option<String> {
    ids.first().map(|i| format!("{}:{}", i.scheme, i.value))
}

/// Angle difference in (-180, 180].
fn wrap(d: f64) -> f64 {
    let d = d.rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

/// State the engine keeps for bearings.
#[derive(Debug, Default)]
pub(super) struct Bearings {
    waiting: Vec<Lob>,
    /// Recent fixes: key, where, when (to give the next fix of the same
    /// object the same source track).
    fixes: Vec<(String, f64, f64, DateTime<Utc>)>,
    seq: u64,
}

/// A fix: position, covariance `[nn, ne, ee]` and the bearings in it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Fix {
    pub lat: f64,
    pub lon: f64,
    pub cov: [f64; 3],
}

/// Least-squares crossing of bearings, weighted by each one's angular
/// error at its range; `None` when the lines do not cross ahead of every
/// sensor or cross too flat.
fn cross(lobs: &[&Lob]) -> Option<Fix> {
    let (lat0, lon0) = lobs[0].origin();
    let pts: Vec<(f64, f64, f64, f64)> = lobs
        .iter()
        .map(|l| {
            let (n, e) = local(lat0, lon0, l.origin().0, l.origin().1);
            let t = l.bearing.to_radians();
            (n, e, t, l.sigma.to_radians())
        })
        .collect();
    // Steep enough between at least one pair.
    let steep = lobs.iter().enumerate().any(|(i, a)| {
        lobs[i + 1..].iter().any(|b| {
            let d = wrap(a.bearing - b.bearing).abs();
            (MIN_CROSS_DEG..=180.0 - MIN_CROSS_DEG).contains(&d)
        })
    });
    if !steep {
        return None;
    }
    let mut ranges = vec![10_000.0_f64; pts.len()];
    let mut x = (0.0, 0.0);
    let mut cov = [0.0; 3];
    for _ in 0..4 {
        let (mut a11, mut a12, mut a22, mut b1, mut b2) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for ((n, e, t, s), r) in pts.iter().zip(&ranges) {
            // Normal to the line (north, east components).
            let (mn, me) = (-t.sin(), t.cos());
            let w = 1.0 / (s * r.max(100.0)).powi(2);
            let c = mn * n + me * e;
            a11 += w * mn * mn;
            a12 += w * mn * me;
            a22 += w * me * me;
            b1 += w * mn * c;
            b2 += w * me * c;
        }
        let det = a11 * a22 - a12 * a12;
        if det.abs() < 1e-30 {
            return None;
        }
        x = ((a22 * b1 - a12 * b2) / det, (a11 * b2 - a12 * b1) / det);
        cov = [a22 / det, -a12 / det, a11 / det];
        for ((n, e, _, _), r) in pts.iter().zip(ranges.iter_mut()) {
            *r = ((x.0 - n).powi(2) + (x.1 - e).powi(2)).sqrt();
        }
    }
    let (lat, lon) = offset(lat0, lon0, x.0, x.1);
    // Ahead of every sensor, within its range, and each line within its gate.
    for l in lobs {
        let (b, d) = bearing_to(l.origin().0, l.origin().1, lat, lon);
        let r = wrap(l.bearing - b);
        if r.abs() > 90.0 || d > l.range || (r / l.sigma).powi(2) > GATE_D2 {
            return None;
        }
    }
    Some(Fix { lat, lon, cov })
}

/// Whether two bearings come from different places (a baseline to fix on).
fn apart(a: &Lob, b: &Lob) -> bool {
    let (n, e) = local(a.origin().0, a.origin().1, b.origin().0, b.origin().1);
    n.hypot(e) >= MIN_BASELINE_M
}

impl Bearings {
    /// Try to fix the newest waiting bearing with the others. Returns the
    /// bearings used and the fix.
    fn try_fix(&mut self) -> Option<(Vec<Lob>, Fix)> {
        let n = self.waiting.len().checked_sub(1)?;
        let new = &self.waiting[n];
        let id = new.identity();
        let others: Vec<usize> = (0..n)
            .filter(|&j| apart(new, &self.waiting[j]))
            .filter(|&j| match (&id, self.waiting[j].identity()) {
                (Some(a), Some(b)) => *a == b,
                _ => true,
            })
            .collect();
        for &j in &others {
            let pair = [new, &self.waiting[j]];
            let Some(fix) = cross(&pair) else {
                continue;
            };
            let same_identity = id.is_some() && id == self.waiting[j].identity();
            // Everything else waiting that agrees (and is from yet another place).
            let mut members = vec![n, j];
            for &k in &others {
                if k == j
                    || !members
                        .iter()
                        .all(|&m| apart(&self.waiting[m], &self.waiting[k]))
                {
                    continue;
                }
                let mut set: Vec<&Lob> = members.iter().map(|&m| &self.waiting[m]).collect();
                set.push(&self.waiting[k]);
                if cross(&set).is_some() {
                    members.push(k);
                }
            }
            if !same_identity && members.len() < 3 {
                continue;
            }
            let set: Vec<&Lob> = members.iter().map(|&m| &self.waiting[m]).collect();
            let fix = cross(&set).unwrap_or(fix);
            members.sort_unstable_by(|a, b| b.cmp(a));
            let used = members
                .into_iter()
                .map(|m| self.waiting.remove(m))
                .collect();
            return Some((used, fix));
        }
        None
    }
}

impl Engine {
    /// A line of bearing: to a track, or waiting to be fixed. Returns the
    /// fixes it completed, as reports for the picture.
    pub(super) async fn bearing(
        &mut self,
        obs: Observation,
        counts: &mut EngineCounts,
    ) -> anyhow::Result<Vec<Observation>> {
        let Some(lob) = Lob::of(obs) else {
            return Ok(Vec::new());
        };
        if let Some(uid) = self.lob_track(&lob) {
            counts.paired += 1;
            self.save(uid, false).await?;
            return Ok(Vec::new());
        }
        let now = lob.obs.observed_at;
        let window = chrono::Duration::seconds(WINDOW_S);
        self.bearings
            .waiting
            .retain(|l| (now - l.obs.observed_at).abs() <= window);
        self.bearings.waiting.push(lob);
        let Some((used, fix)) = self.bearings.try_fix() else {
            return Ok(Vec::new());
        };
        Ok(vec![self.fix_report(&used, &fix)])
    }

    /// The one track a bearing points at, if exactly one fits clearly best;
    /// the bearing is recorded on it.
    fn lob_track(&mut self, lob: &Lob) -> Option<Uid> {
        let (olat, olon) = lob.origin();
        let at = lob.obs.observed_at;
        let id = lob.identity();
        let mut fits: Vec<(Uid, f64, f64)> = Vec::new();
        for t in self.tracks.values() {
            // A track only fixes keep up is kept up by fixes, not bearings.
            if !t.kind.is_track()
                || t.state == ot_core::TrackState::Lost
                || t.contributors.iter().all(|c| c.source_id == FIX)
            {
                continue;
            }
            let dt =
                ((at - t.view.observed_at).num_milliseconds() as f64 / 1000.0).clamp(-300.0, 300.0);
            let (mut lat, mut lon) = (t.view.position.latitude, t.view.position.longitude);
            if let (Some(c), Some(v)) = (t.view.kinematics.course_deg, t.view.kinematics.speed_mps)
            {
                let d = v * dt;
                (lat, lon) = offset(lat, lon, d * c.to_radians().cos(), d * c.to_radians().sin());
            }
            let (b, range) = bearing_to(olat, olon, lat, lon);
            let r = wrap(lob.bearing - b);
            if range > lob.range || r.abs() > 90.0 {
                continue;
            }
            // The track's own error across the line, and its drift since.
            let across = t
                .view
                .uncertainty
                .as_ref()
                .and_then(|u| u.position_covariance())
                .map(|[nn, ne, ee]| {
                    let (s, c) = b.to_radians().sin_cos();
                    // Unit normal to the line: (-sin, cos) in (north, east).
                    (nn * s * s - 2.0 * ne * s * c + ee * c * c).max(0.0).sqrt()
                })
                .unwrap_or(100.0)
                + dt.abs();
            let sigma = (lob.sigma.powi(2) + (across / range.max(1.0)).to_degrees().powi(2)).sqrt();
            let d2 = (r / sigma).powi(2);
            if d2 > GATE_D2 {
                continue;
            }
            // Emitter identity: a different one is a veto, the same one decisive.
            let theirs: Vec<String> = t
                .bearings
                .iter()
                .filter_map(|b| identity(&b.identifiers))
                .chain(
                    t.view
                        .identifiers
                        .iter()
                        .map(|i| format!("{}:{}", i.scheme, i.value)),
                )
                .collect();
            let mut score = (-0.5 * d2).exp() / sigma;
            if let Some(id) = &id {
                let scheme = id.split(':').next().unwrap_or_default();
                if theirs.iter().any(|x| x == id) {
                    score *= 1e6;
                } else if theirs.iter().any(|x| x.split(':').next() == Some(scheme)) {
                    continue;
                }
            }
            fits.push((t.uid, score, r));
        }
        fits.sort_by(|a, b| b.1.total_cmp(&a.1));
        let (uid, best, residual) = *fits.first()?;
        if fits.get(1).is_some_and(|s| best < AMBIGUITY * s.1) {
            return None;
        }
        let keep = chrono::Duration::seconds(KEEP_S);
        let t = self.tracks.get_mut(&uid)?;
        t.bearings.retain(|b| {
            !(b.source_id == lob.obs.source_id && b.source_track_key == lob.obs.source_track_key)
                && (at - b.observed_at).abs() <= keep
        });
        t.bearings.push(BearingContact {
            source_id: lob.obs.source_id.clone(),
            source_track_key: lob.obs.source_track_key.clone(),
            observed_at: at,
            latitude: olat,
            longitude: olon,
            bearing_deg: lob.bearing,
            sigma_deg: lob.sigma,
            max_range_m: (lob.range != DEFAULT_RANGE_M).then_some(lob.range),
            residual_deg: (residual * 100.0).round() / 100.0,
            identifiers: lob.obs.identifiers.clone(),
        });
        Some(uid)
    }

    /// A fix as a report of the `fix` source: the same object's successive
    /// fixes report under one key (its emitter identity, or the nearest
    /// recent fix within its error).
    fn fix_report(&mut self, used: &[Lob], fix: &Fix) -> Observation {
        let at = used
            .iter()
            .map(|l| l.obs.observed_at)
            .max()
            .unwrap_or_else(Utc::now);
        let id = used.iter().find_map(|l| l.identity());
        let recent = chrono::Duration::seconds(WINDOW_S * 5);
        self.bearings.fixes.retain(|f| (at - f.3).abs() <= recent);
        let spread = (fix.cov[0] + fix.cov[2]).sqrt() * 3.0 + 1000.0;
        let key = match &id {
            Some(id) => format!("id:{id}"),
            None => self
                .bearings
                .fixes
                .iter()
                .filter(|f| !f.0.starts_with("id:"))
                .map(|f| {
                    let (n, e) = local(f.1, f.2, fix.lat, fix.lon);
                    (n.hypot(e), f.0.clone())
                })
                .filter(|(d, _)| *d <= spread)
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, k)| k)
                .unwrap_or_else(|| {
                    self.bearings.seq += 1;
                    format!("fix-{}", self.bearings.seq)
                }),
        };
        self.bearings.fixes.retain(|f| f.0 != key);
        self.bearings
            .fixes
            .push((key.clone(), fix.lat, fix.lon, at));
        let mut sensors: Vec<&str> = used.iter().map(|l| l.obs.source_id.as_str()).collect();
        sensors.sort_unstable();
        sensors.dedup();
        let ids: Vec<_> = {
            let mut v: Vec<ot_core::schema::Identifier> = used
                .iter()
                .flat_map(|l| l.obs.identifiers.clone())
                .collect();
            v.dedup();
            v
        };
        let domain = used.iter().find_map(|l| l.obs.classification.domain);
        serde_json::from_value(json!({
            "schema_version": used[0].obs.schema_version,
            "source_id": FIX,
            "source_track_key": key,
            "observed_at": at,
            "received_at": Utc::now(),
            "position": {"latitude": fix.lat, "longitude": fix.lon},
            "uncertainty": {"covariance": {"position": fix.cov}},
            "identifiers": ids,
            "classification": {"domain": domain},
            "provenance": {"sensor_code": "crossfix", "source_code": sensors.join(",")},
        }))
        .expect("a fix is an observation")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lob(source: &str, lat: f64, lon: f64, to: (f64, f64), sigma: f64, id: Option<&str>) -> Lob {
        let (b, _) = bearing_to(lat, lon, to.0, to.1);
        let obs: Observation = serde_json::from_value(json!({
            "schema_version": 1, "source_id": source, "source_track_key": "e1",
            "observed_at": "2026-09-27T12:00:00Z", "received_at": "2026-09-27T12:00:00Z",
            "position": {"latitude": lat, "longitude": lon},
            "identifiers": id.map(|v| vec![json!({"scheme": "elnot", "value": v})]).unwrap_or_default(),
            "geometry": {"type": "bearing", "bearing_deg": b, "sigma_deg": sigma}
        }))
        .unwrap();
        Lob::of(obs).unwrap()
    }

    const SHIP: (f64, f64) = (50.70, -1.30);

    #[test]
    fn crossing_bearings_fix_the_emitter_with_an_honest_error() {
        let a = lob("esm-a", 50.60, -1.60, SHIP, 1.0, None);
        let b = lob("esm-b", 50.85, -1.45, SHIP, 1.0, None);
        let fix = cross(&[&a, &b]).unwrap();
        let (n, e) = local(SHIP.0, SHIP.1, fix.lat, fix.lon);
        // On the ship to within the flat-earth approximation (0.1 degree at
        // 20 km), far inside the fix's own error.
        assert!(
            n.hypot(e) < 100.0,
            "exact bearings meet on the ship: {n} {e}"
        );
        // ~20 km ranges at 1 degree: a few hundred metres, not kilometres.
        let s = (fix.cov[0] + fix.cov[2]).sqrt();
        assert!((100.0..2000.0).contains(&s), "{s}");
        // Parallel lines do not fix; nor do lines crossing behind a sensor.
        let c = lob("esm-c", 50.60, -1.59, SHIP, 1.0, None);
        assert!(cross(&[&a, &c]).is_none());
        let mut behind = lob("esm-b", 50.85, -1.45, SHIP, 1.0, None);
        behind.bearing = (behind.bearing + 180.0) % 360.0;
        assert!(cross(&[&a, &behind]).is_none());
    }

    #[test]
    fn two_sensors_fix_only_with_the_same_emitter_identity() {
        let mut w = Bearings::default();
        w.waiting.push(lob("esm-a", 50.60, -1.60, SHIP, 1.0, None));
        w.waiting.push(lob("esm-b", 50.85, -1.45, SHIP, 1.0, None));
        assert!(w.try_fix().is_none(), "two anonymous lines may be a ghost");
        w.waiting.push(lob("esm-c", 50.55, -1.10, SHIP, 1.0, None));
        let (used, fix) = w.try_fix().unwrap();
        assert_eq!(used.len(), 3, "three sensors agreeing");
        let (n, e) = local(SHIP.0, SHIP.1, fix.lat, fix.lon);
        assert!(n.hypot(e) < 100.0, "{n} {e}");
        assert!(w.waiting.is_empty());

        let mut w = Bearings::default();
        w.waiting
            .push(lob("esm-a", 50.60, -1.60, SHIP, 1.0, Some("A123")));
        w.waiting
            .push(lob("esm-b", 50.85, -1.45, SHIP, 1.0, Some("B999")));
        assert!(
            w.try_fix().is_none(),
            "different emitters never fix together"
        );
        w.waiting
            .push(lob("esm-c", 50.55, -1.10, SHIP, 1.0, Some("A123")));
        let (used, _) = w.try_fix().unwrap();
        assert_eq!(used.len(), 2, "the same emitter from two sensors");
        assert!(
            used.iter()
                .all(|l| l.identity().as_deref() == Some("elnot:A123"))
        );
    }

    #[test]
    fn a_line_that_misses_is_not_forced_into_a_fix() {
        let mut w = Bearings::default();
        w.waiting.push(lob("esm-a", 50.60, -1.60, SHIP, 1.0, None));
        w.waiting.push(lob("esm-b", 50.85, -1.45, SHIP, 1.0, None));
        // A third sensor looking at something else, 25 km away.
        w.waiting
            .push(lob("esm-c", 50.55, -1.10, (50.90, -1.05), 1.0, None));
        assert!(w.try_fix().is_none());
        assert_eq!(w.waiting.len(), 3);
    }
}
