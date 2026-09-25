//! The authoritative track schema.
//!
//! Every observation from every source is mapped into an [`Observation`]: a
//! fixed core that correlation and published tracks depend on, plus admin-defined
//! extension fields carried in [`Observation::ext`]. Units are SI throughout
//! (metres, metres per second, degrees true).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::sidc::{Sidc, SidcStandard};

/// A named identifier a source reports for an object, e.g. `mmsi:338924210`,
/// `icao:ae1234` or `cot-uid:ANDROID-1234`. Identifier matches always pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Identifier {
    pub scheme: String,
    pub value: String,
}

impl Identifier {
    pub fn new(scheme: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            scheme: scheme.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub latitude: f64,
    pub longitude: f64,
    /// Height above the WGS84 ellipsoid, metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub altitude_hae_m: Option<f64>,
}

/// A GOLD XPOS-style uncertainty ellipse.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Ellipse {
    pub semi_major_m: f64,
    pub semi_minor_m: f64,
    /// Orientation of the major axis, degrees clockwise from true north.
    pub orientation_deg: f64,
}

impl Ellipse {
    /// Circular error probable approximated from the ellipse, for consumers
    /// that only handle a circular error.
    pub fn cep_m(&self) -> f64 {
        0.59 * (self.semi_major_m + self.semi_minor_m)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Uncertainty {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ellipse: Option<Ellipse>,
    /// Used when a source reports only a circular error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circular_error_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_error_m: Option<f64>,
}

impl Uncertainty {
    /// The circular error to publish: the reported one, else derived from the
    /// ellipse.
    pub fn cep_m(&self) -> Option<f64> {
        self.circular_error_m
            .or_else(|| self.ellipse.map(|e| e.cep_m()))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Kinematics {
    /// Course over ground, degrees true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub course_deg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_mps: Option<f64>,
    /// Where the platform points, which can differ from its course.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_deg: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_rate_mps: Option<f64>,
}

/// Operating domain, matching the CoT / MIL-STD-2525 battle dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Air,
    Surface,
    Subsurface,
    Ground,
    Space,
}

impl Domain {
    /// The CoT battle-dimension atom (3rd atom of an `a-` type).
    pub fn cot_atom(self) -> char {
        match self {
            Domain::Air => 'A',
            Domain::Surface => 'S',
            Domain::Subsurface => 'U',
            Domain::Ground => 'G',
            Domain::Space => 'P',
        }
    }

    pub fn from_cot_atom(atom: &str) -> Option<Self> {
        Some(match atom {
            "A" => Domain::Air,
            "S" => Domain::Surface,
            "U" => Domain::Subsurface,
            "G" => Domain::Ground,
            "P" => Domain::Space,
            _ => return None,
        })
    }
}

/// Standard identity, matching the CoT affiliation atom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Affiliation {
    Pending,
    Unknown,
    AssumedFriend,
    Friend,
    Neutral,
    Suspect,
    Hostile,
    Joker,
    Faker,
    None,
}

impl Affiliation {
    /// The CoT affiliation atom (2nd atom of an `a-` type).
    pub fn cot_atom(self) -> char {
        match self {
            Affiliation::Pending => 'p',
            Affiliation::Unknown => 'u',
            Affiliation::AssumedFriend => 'a',
            Affiliation::Friend => 'f',
            Affiliation::Neutral => 'n',
            Affiliation::Suspect => 's',
            Affiliation::Hostile => 'h',
            Affiliation::Joker => 'j',
            Affiliation::Faker => 'k',
            Affiliation::None => 'o',
        }
    }

    pub fn from_cot_atom(atom: &str) -> Option<Self> {
        Some(match atom {
            "p" => Affiliation::Pending,
            "u" => Affiliation::Unknown,
            "a" => Affiliation::AssumedFriend,
            "f" => Affiliation::Friend,
            "n" => Affiliation::Neutral,
            "s" => Affiliation::Suspect,
            "h" => Affiliation::Hostile,
            "j" => Affiliation::Joker,
            "k" => Affiliation::Faker,
            "o" => Affiliation::None,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Classification {
    /// CoT type, e.g. `a-f-S-C-L-D-D` (MIL-STD-2525 based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cot_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<Domain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affiliation: Option<Affiliation>,
    /// Symbol identification code as a feed reports it: MIL-STD-2525C,
    /// MIL-STD-2525D or a CoT type (see [`crate::sidc`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidc: Option<String>,
}

impl Classification {
    fn parsed_sidc(&self) -> Option<Sidc> {
        Sidc::parse(self.sidc.as_deref()?)
    }

    /// Domain, falling back to the CoT type's battle dimension, then the SIDC.
    pub fn effective_domain(&self) -> Option<Domain> {
        self.domain
            .or_else(|| {
                cot_atoms(self.cot_type.as_deref()?)
                    .2
                    .and_then(Domain::from_cot_atom)
            })
            .or_else(|| self.parsed_sidc()?.domain())
    }

    /// Affiliation, falling back to the CoT type's affiliation atom, then the
    /// SIDC's standard identity.
    pub fn effective_affiliation(&self) -> Option<Affiliation> {
        self.affiliation
            .or_else(|| {
                cot_atoms(self.cot_type.as_deref()?)
                    .1
                    .and_then(Affiliation::from_cot_atom)
            })
            .or_else(|| self.parsed_sidc()?.affiliation())
    }

    /// The SIDC to publish: the feed's (2525C, 2525D or CoT) with its
    /// standard identity rewritten when an affiliation is set explicitly,
    /// else the CoT type OpenTrack derives. Always present.
    pub fn sidc_or_derived(&self) -> Sidc {
        match self.parsed_sidc() {
            Some(s) => match self.affiliation {
                Some(a) => s.with_affiliation(a),
                None => s,
            },
            None => Sidc {
                standard: SidcStandard::Cot,
                code: self.cot_type_or_derived(),
            },
        }
    }

    /// The CoT type to publish. An explicit type wins, with its affiliation
    /// atom replaced when an affiliation is set explicitly (affiliation policy
    /// outranks a feed's default `u`); otherwise one is built from domain and
    /// affiliation (`a-<affiliation>-<domain>`).
    pub fn cot_type_or_derived(&self) -> String {
        if let Some(t) = self.cot_type.as_deref().filter(|t| !t.is_empty()) {
            let mut parts: Vec<&str> = t.split('-').collect();
            return match self.affiliation {
                Some(aff) if parts.len() >= 2 && parts[0] == "a" => {
                    let atom = aff.cot_atom().to_string();
                    parts[1] = &atom;
                    parts.join("-")
                }
                _ => t.to_owned(),
            };
        }
        let aff = self.effective_affiliation().unwrap_or(Affiliation::Unknown);
        match self.effective_domain() {
            Some(d) => format!("a-{}-{}", aff.cot_atom(), d.cot_atom()),
            None => format!("a-{}", aff.cot_atom()),
        }
    }
}

/// First three atoms of a CoT type, when it is an `a-` (atom) type.
fn cot_atoms(cot_type: &str) -> (Option<&str>, Option<&str>, Option<&str>) {
    let mut parts = cot_type.split('-');
    let first = parts.next().filter(|p| *p == "a");
    if first.is_none() {
        return (None, None, None);
    }
    (first, parts.next(), parts.next())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Platform {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hull: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_code: Option<String>,
    /// The source's own confidence in this report, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// OTH-GOLD track type (CTC field 13): whether the track is real-world
/// tactical data or training/simulation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackType {
    /// Real-world track (GOLD's default, a null entry).
    #[default]
    Tactical,
    /// Real-world friendly track designated as a training track (GOLD 2).
    LiveTraining,
    /// Manually created or generated as part of a training scenario (GOLD 3).
    SimulatedTraining,
    /// Demand entry: an actual unit that receivers should not filter (GOLD 4).
    DemandEntry,
}

/// Track lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackState {
    Tentative,
    Confirmed,
    Lost,
    Dropped,
}

/// One report about one object from one source, in the authoritative schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    /// Extension schema version the source's mapping targets.
    pub schema_version: u32,
    pub source_id: String,
    /// The source's own key for the object (its track number, MMSI, CoT uid...).
    pub source_track_key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<Identifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub position: Position,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<Uncertainty>,
    #[serde(default)]
    pub kinematics: Kinematics,
    #[serde(default)]
    pub classification: Classification,
    #[serde(default)]
    pub platform: Platform,
    #[serde(default)]
    pub provenance: Provenance,
    /// State reported by the source, if it reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TrackState>,
    /// OTH-GOLD track type; tactical when the source does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_type: Option<TrackType>,
    /// Admin-defined extension fields, published under `attributes_json.ext`.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub ext: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{field} = {value} is outside {range}")]
    OutOfRange {
        field: &'static str,
        value: f64,
        range: &'static str,
    },
    #[error("{0} is not a finite number")]
    NotFinite(&'static str),
    #[error("ellipse semi-minor axis exceeds semi-major axis")]
    EllipseAxes,
}

impl Observation {
    /// Check the core invariants the rest of the system relies on. Mapping
    /// rejects an observation that fails, and counts it per source.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.source_id.trim().is_empty() {
            return Err(ValidationError::Missing("source_id"));
        }
        if self.source_track_key.trim().is_empty() {
            return Err(ValidationError::Missing("source_track_key"));
        }
        range("latitude", self.position.latitude, -90.0, 90.0, "[-90, 90]")?;
        range(
            "longitude",
            self.position.longitude,
            -180.0,
            180.0,
            "[-180, 180]",
        )?;
        finite_opt("altitude_hae_m", self.position.altitude_hae_m)?;

        let k = &self.kinematics;
        opt_range("course_deg", k.course_deg, 0.0, 360.0, "[0, 360]")?;
        opt_range("heading_deg", k.heading_deg, 0.0, 360.0, "[0, 360]")?;
        opt_range("speed_mps", k.speed_mps, 0.0, f64::MAX, "[0, inf)")?;
        finite_opt("vertical_rate_mps", k.vertical_rate_mps)?;

        if let Some(u) = &self.uncertainty {
            opt_range(
                "circular_error_m",
                u.circular_error_m,
                0.0,
                f64::MAX,
                "[0, inf)",
            )?;
            opt_range(
                "vertical_error_m",
                u.vertical_error_m,
                0.0,
                f64::MAX,
                "[0, inf)",
            )?;
            if let Some(e) = &u.ellipse {
                range("semi_major_m", e.semi_major_m, 0.0, f64::MAX, "[0, inf)")?;
                range("semi_minor_m", e.semi_minor_m, 0.0, f64::MAX, "[0, inf)")?;
                range("orientation_deg", e.orientation_deg, 0.0, 360.0, "[0, 360]")?;
                if e.semi_minor_m > e.semi_major_m {
                    return Err(ValidationError::EllipseAxes);
                }
            }
        }
        opt_range("confidence", self.provenance.confidence, 0.0, 1.0, "[0, 1]")?;
        Ok(())
    }
}

fn range(
    field: &'static str,
    value: f64,
    lo: f64,
    hi: f64,
    label: &'static str,
) -> Result<(), ValidationError> {
    if !value.is_finite() {
        return Err(ValidationError::NotFinite(field));
    }
    if value < lo || value > hi {
        return Err(ValidationError::OutOfRange {
            field,
            value,
            range: label,
        });
    }
    Ok(())
}

fn opt_range(
    field: &'static str,
    value: Option<f64>,
    lo: f64,
    hi: f64,
    label: &'static str,
) -> Result<(), ValidationError> {
    value.map_or(Ok(()), |v| range(field, v, lo, hi, label))
}

fn finite_opt(field: &'static str, value: Option<f64>) -> Result<(), ValidationError> {
    match value {
        Some(v) if !v.is_finite() => Err(ValidationError::NotFinite(field)),
        _ => Ok(()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::TimeZone;

    pub(crate) fn sample() -> Observation {
        let t = Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap();
        Observation {
            schema_version: 1,
            source_id: "synthetic".into(),
            source_track_key: "T00001".into(),
            identifiers: vec![Identifier::new("mmsi", "338924210")],
            name: Some("TED STEVENS".into()),
            callsign: None,
            observed_at: t,
            received_at: t,
            position: Position {
                latitude: 32.68,
                longitude: -117.23,
                altitude_hae_m: None,
            },
            uncertainty: Some(Uncertainty {
                ellipse: Some(Ellipse {
                    semi_major_m: 100.0,
                    semi_minor_m: 50.0,
                    orientation_deg: 45.0,
                }),
                ..Default::default()
            }),
            kinematics: Kinematics {
                course_deg: Some(270.0),
                speed_mps: Some(5.0),
                heading_deg: Some(268.0),
                vertical_rate_mps: None,
            },
            classification: Classification {
                cot_type: None,
                domain: Some(Domain::Surface),
                affiliation: Some(Affiliation::Friend),
                sidc: None,
            },
            platform: Platform::default(),
            provenance: Provenance {
                confidence: Some(0.9),
                ..Default::default()
            },
            state: None,
            track_type: None,
            ext: Default::default(),
        }
    }

    #[test]
    fn sample_is_valid_and_round_trips() {
        let obs = sample();
        obs.validate().unwrap();
        let json = serde_json::to_string(&obs).unwrap();
        assert_eq!(serde_json::from_str::<Observation>(&json).unwrap(), obs);
    }

    #[test]
    fn rejects_out_of_range_and_non_finite() {
        let mut obs = sample();
        obs.position.latitude = 91.0;
        assert!(matches!(
            obs.validate(),
            Err(ValidationError::OutOfRange {
                field: "latitude",
                ..
            })
        ));

        let mut obs = sample();
        obs.kinematics.speed_mps = Some(f64::NAN);
        assert_eq!(obs.validate(), Err(ValidationError::NotFinite("speed_mps")));

        let mut obs = sample();
        obs.source_track_key = " ".into();
        assert_eq!(
            obs.validate(),
            Err(ValidationError::Missing("source_track_key"))
        );

        let mut obs = sample();
        obs.uncertainty
            .as_mut()
            .unwrap()
            .ellipse
            .as_mut()
            .unwrap()
            .semi_minor_m = 200.0;
        assert_eq!(obs.validate(), Err(ValidationError::EllipseAxes));
    }

    #[test]
    fn cep_prefers_reported_circular_error() {
        let e = Ellipse {
            semi_major_m: 100.0,
            semi_minor_m: 50.0,
            orientation_deg: 0.0,
        };
        assert!((e.cep_m() - 88.5).abs() < 1e-9);
        let u = Uncertainty {
            ellipse: Some(e),
            circular_error_m: Some(10.0),
            vertical_error_m: None,
        };
        assert_eq!(u.cep_m(), Some(10.0));
    }

    #[test]
    fn cot_type_derivation_and_fallbacks() {
        let c = Classification {
            domain: Some(Domain::Surface),
            affiliation: Some(Affiliation::Hostile),
            ..Default::default()
        };
        assert_eq!(c.cot_type_or_derived(), "a-h-S");

        let c = Classification {
            cot_type: Some("a-f-A-M-F".into()),
            ..Default::default()
        };
        assert_eq!(c.effective_domain(), Some(Domain::Air));
        assert_eq!(c.effective_affiliation(), Some(Affiliation::Friend));
        assert_eq!(c.cot_type_or_derived(), "a-f-A-M-F");

        let c = Classification {
            cot_type: Some("a-u-S-C".into()),
            affiliation: Some(Affiliation::Hostile),
            ..Default::default()
        };
        assert_eq!(c.cot_type_or_derived(), "a-h-S-C");

        let c = Classification {
            cot_type: Some("b-m-p-s-p-i".into()),
            ..Default::default()
        };
        assert_eq!(c.effective_domain(), None);
        assert_eq!(Classification::default().cot_type_or_derived(), "a-u");
    }
}
