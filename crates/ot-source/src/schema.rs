//! The admin-designed output schema: the versioned part of the track schema.
//!
//! The core schema is fixed (correlation and the OTH-GOLD minimum depend on
//! it). The output schema is designed by the administrator: each field has a
//! key, a type and notes. Fields are grouped into versions that are immutable
//! once published. A field's value for a track comes from, in order of
//! authority:
//!
//! 1. the entity's card (the baseball card an admin fills in),
//! 2. a feed, through a source mapping that targets `ext.<key>`,
//! 3. an OpenTrack built-in the field is linked to (state, speed, ...).
//!
//! When the card and a feed disagree the card wins and an
//! [`AttributeNotice`] records the difference. Resolved values are published
//! under `attributes`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use ot_core::{AttributeNotice, SystemTrack};

use crate::expr::{as_f64, as_string, parse_time_value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtType {
    String,
    Integer,
    Number,
    Boolean,
    /// One of `enum_values`.
    Enum,
    /// Stored as RFC 3339.
    Timestamp,
    /// `{ "latitude": .., "longitude": .. }`.
    Position,
    /// Any JSON value, kept as is.
    Json,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionField {
    /// `a-z` then `a-z`, `0-9` or `_`, at most 64 characters.
    pub key: String,
    #[serde(rename = "type")]
    pub kind: ExtType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<String>,
    /// Notes for the people who maintain and consume the schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Fill this field from an OpenTrack value instead of a feed or card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<Builtin>,
}

/// OpenTrack values an output field can be linked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Builtin {
    /// Lifecycle state: tentative, confirmed, lost, dropped.
    State,
    CotType,
    CourseDeg,
    SpeedMps,
    HeadingDeg,
    VerticalRateMps,
    AltitudeHaeM,
    CepM,
    Callsign,
    /// Every identifier, `[{scheme, value}]`.
    Identifiers,
    PlatformType,
    PlatformFlag,
    PlatformHull,
    SensorCode,
    /// Probability that the track is a real object, from its sources'
    /// existence and pairing confidences (`SystemTrack::confidence`).
    Confidence,
    /// Ids of the sources reporting for the track.
    Sources,
    /// Source tracks reporting for the track, with pairing and last report.
    Contributors,
    ObservationCount,
    FirstSeen,
    LastSeen,
    /// The registry entity (card) the track resolves to.
    EntityId,
}

impl Builtin {
    pub const ALL: [Builtin; 21] = [
        Builtin::State,
        Builtin::CotType,
        Builtin::CourseDeg,
        Builtin::SpeedMps,
        Builtin::HeadingDeg,
        Builtin::VerticalRateMps,
        Builtin::AltitudeHaeM,
        Builtin::CepM,
        Builtin::Callsign,
        Builtin::Identifiers,
        Builtin::PlatformType,
        Builtin::PlatformFlag,
        Builtin::PlatformHull,
        Builtin::SensorCode,
        Builtin::Confidence,
        Builtin::Sources,
        Builtin::Contributors,
        Builtin::ObservationCount,
        Builtin::FirstSeen,
        Builtin::LastSeen,
        Builtin::EntityId,
    ];

    /// The field type a linked field must declare.
    pub fn kind(self) -> ExtType {
        use Builtin::*;
        match self {
            State | CotType | Callsign | PlatformType | PlatformFlag | PlatformHull
            | SensorCode | EntityId => ExtType::String,
            CourseDeg | SpeedMps | HeadingDeg | VerticalRateMps | AltitudeHaeM | CepM
            | Confidence => ExtType::Number,
            ObservationCount => ExtType::Integer,
            FirstSeen | LastSeen => ExtType::Timestamp,
            Identifiers | Sources | Contributors => ExtType::Json,
        }
    }

    /// The observation fields (mapping targets) this built-in reads; empty
    /// for values OpenTrack computes itself (state, sources, counts...).
    pub fn reads(self) -> &'static [&'static str] {
        use Builtin::*;
        match self {
            CotType => &[
                "classification.cot_type",
                "classification.domain",
                "classification.affiliation",
            ],
            CourseDeg => &["kinematics.course_deg"],
            SpeedMps => &["kinematics.speed_mps"],
            HeadingDeg => &["kinematics.heading_deg"],
            VerticalRateMps => &["kinematics.vertical_rate_mps"],
            AltitudeHaeM => &["position.altitude_hae_m"],
            CepM => &[
                "uncertainty.circular_error_m",
                "uncertainty.ellipse.semi_major_m",
                "uncertainty.ellipse.semi_minor_m",
            ],
            Callsign => &["callsign"],
            Identifiers => &["identifiers"],
            PlatformType => &["platform.type_code"],
            PlatformFlag => &["platform.flag"],
            PlatformHull => &["platform.hull"],
            SensorCode => &["provenance.sensor_code"],
            State | Confidence | Sources | Contributors | ObservationCount | FirstSeen
            | LastSeen | EntityId => &[],
        }
    }

    /// The value for a track, if it has one.
    pub fn value(self, t: &SystemTrack) -> Option<Value> {
        use Builtin::*;
        let v = &t.view;
        let num = |x: Option<f64>| x.filter(|x| x.is_finite()).map(|x| json!(x));
        let text = |s: &Option<String>| {
            s.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| json!(s))
        };
        match self {
            State => serde_json::to_value(t.state).ok(),
            CotType => Some(json!(v.classification.cot_type_or_derived())),
            CourseDeg => num(v.kinematics.course_deg),
            SpeedMps => num(v.kinematics.speed_mps),
            HeadingDeg => num(v.kinematics.heading_deg),
            VerticalRateMps => num(v.kinematics.vertical_rate_mps),
            AltitudeHaeM => num(v.position.altitude_hae_m),
            CepM => num(v.uncertainty.and_then(|u| u.cep_m())),
            Callsign => text(&v.callsign),
            Identifiers => (!v.identifiers.is_empty()).then(|| json!(v.identifiers)),
            PlatformType => text(&v.platform.type_code),
            PlatformFlag => text(&v.platform.flag),
            PlatformHull => text(&v.platform.hull),
            SensorCode => text(&v.provenance.sensor_code),
            Confidence => Some(json!(t.confidence())),
            Sources => {
                let mut ids: Vec<&str> = t
                    .contributors
                    .iter()
                    .map(|c| c.source_id.as_str())
                    .collect();
                ids.sort_unstable();
                ids.dedup();
                Some(json!(ids))
            }
            Contributors => Some(json!(t.contributors)),
            ObservationCount => Some(json!(t.observation_count)),
            FirstSeen => Some(json!(t.first_seen)),
            LastSeen => Some(json!(t.last_seen)),
            EntityId => t.entity_id.as_ref().map(|e| json!(e)),
        }
    }
}

/// Whether two values of a field are the same (numbers within rounding).
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0),
        _ => a == b,
    }
}

/// Resolve a track's attributes against the output schema: card first, then
/// the feed (the track's mapped `ext` values), then linked built-ins. Returns
/// the values to publish and the card values that differ from the feed.
pub fn resolve_attributes(
    schema: &ExtensionSchema,
    track: &SystemTrack,
    card: Option<&Map<String, Value>>,
) -> (Map<String, Value>, Vec<AttributeNotice>) {
    let mut out = Map::new();
    let mut notices = Vec::new();
    for f in &schema.fields {
        if let Some(b) = f.builtin {
            if let Some(v) = b.value(track) {
                out.insert(f.key.clone(), v);
            }
            continue;
        }
        let feed = track.view.ext.get(&f.key).filter(|v| !v.is_null());
        let carded = card
            .and_then(|c| c.get(&f.key))
            .filter(|v| !v.is_null())
            .and_then(|v| coerce(f, v));
        match (carded, feed) {
            (Some(c), Some(fv)) => {
                if !same(&c, fv) {
                    notices.push(AttributeNotice {
                        key: f.key.clone(),
                        card: c.clone(),
                        feed: fv.clone(),
                        source_id: track.view.source_id.clone(),
                    });
                }
                out.insert(f.key.clone(), c);
            }
            (Some(c), None) => {
                out.insert(f.key.clone(), c);
            }
            (None, Some(fv)) => {
                out.insert(f.key.clone(), fv.clone());
            }
            (None, None) => {}
        }
    }
    (out, notices)
}

/// Check card values against the schema: every key must be a field that is
/// not linked to a built-in, and every value must fit its type. Empty values
/// (null or blank text) are removed. Returns the cleaned values.
pub fn check_card(
    schema: &ExtensionSchema,
    values: &Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (k, v) in values {
        let f = schema
            .field(k)
            .ok_or_else(|| format!("{k} is not a field of schema version {}", schema.version))?;
        if f.builtin.is_some() {
            return Err(format!(
                "{k} is filled by OpenTrack and cannot be set on a card"
            ));
        }
        if v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty()) {
            continue;
        }
        let c = coerce(f, v).ok_or_else(|| format!("{k}: {v} is not a valid {:?}", f.kind))?;
        out.insert(k.clone(), c);
    }
    Ok(out)
}

/// Extension keys written by OpenTrack itself, which admins cannot define.
pub const RESERVED_KEYS: &[&str] = &["registry"];

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SchemaError {
    #[error("extension key {0:?} is reserved")]
    Reserved(String),
    #[error("extension key {0:?} must be a-z then a-z, 0-9 or _ (max 64)")]
    BadKey(String),
    #[error("extension key {0:?} is defined twice")]
    Duplicate(String),
    #[error("enum field {0:?} needs enum_values")]
    NoEnumValues(String),
    #[error("default for {key:?} is not a valid {kind:?}")]
    BadDefault { key: String, kind: ExtType },
    #[error("{key:?} is linked to {builtin:?}, which is a {expected:?}, not a {kind:?}")]
    BuiltinType {
        key: String,
        builtin: Builtin,
        expected: ExtType,
        kind: ExtType,
    },
}

/// One schema version's extension fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtensionSchema {
    pub version: u32,
    pub fields: Vec<ExtensionField>,
}

impl ExtensionSchema {
    pub fn validate(&self) -> Result<(), SchemaError> {
        let mut seen = std::collections::HashSet::new();
        for f in &self.fields {
            let ok = !f.key.is_empty()
                && f.key.len() <= 64
                && f.key.as_bytes()[0].is_ascii_lowercase()
                && f.key
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !ok {
                return Err(SchemaError::BadKey(f.key.clone()));
            }
            if RESERVED_KEYS.contains(&f.key.as_str()) {
                return Err(SchemaError::Reserved(f.key.clone()));
            }
            if !seen.insert(f.key.as_str()) {
                return Err(SchemaError::Duplicate(f.key.clone()));
            }
            if f.kind == ExtType::Enum && f.enum_values.is_empty() {
                return Err(SchemaError::NoEnumValues(f.key.clone()));
            }
            if let Some(b) = f.builtin
                && b.kind() != f.kind
            {
                return Err(SchemaError::BuiltinType {
                    key: f.key.clone(),
                    builtin: b,
                    expected: b.kind(),
                    kind: f.kind,
                });
            }
            if let Some(d) = &f.default
                && coerce(f, d).is_none()
            {
                return Err(SchemaError::BadDefault {
                    key: f.key.clone(),
                    kind: f.kind,
                });
            }
        }
        Ok(())
    }

    pub fn field(&self, key: &str) -> Option<&ExtensionField> {
        self.fields.iter().find(|f| f.key == key)
    }

    /// Coerce mapped extension values to their declared types, fill
    /// defaults, and check required fields. Values that cannot be coerced
    /// are dropped and reported; a missing required field is an error.
    pub fn apply(&self, ext: &mut Map<String, Value>) -> Result<Vec<String>, String> {
        let mut dropped = Vec::new();
        for f in &self.fields {
            match ext.get(&f.key) {
                Some(v) if !v.is_null() => match coerce(f, v) {
                    Some(c) => {
                        ext.insert(f.key.clone(), c);
                    }
                    None => {
                        ext.remove(&f.key);
                        dropped.push(f.key.clone());
                    }
                },
                _ => {
                    ext.remove(&f.key);
                }
            }
            if !ext.contains_key(&f.key) {
                if let Some(d) = &f.default {
                    ext.insert(f.key.clone(), d.clone());
                } else if f.required {
                    return Err(format!("required extension field {} is missing", f.key));
                }
            }
        }
        Ok(dropped)
    }
}

/// Coerce one value to a field's type, or `None` if it cannot be.
pub fn coerce(field: &ExtensionField, v: &Value) -> Option<Value> {
    match field.kind {
        ExtType::String => {
            Some(Value::String(as_string(v))).filter(|s| !s.as_str().is_some_and(str::is_empty))
        }
        ExtType::Integer => as_f64(v)
            .filter(|x| x.fract() == 0.0)
            .map(|x| json!(x as i64)),
        ExtType::Number => as_f64(v)
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number),
        ExtType::Boolean => match v {
            Value::Bool(_) => Some(v.clone()),
            _ => match as_string(v).trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => Some(json!(true)),
                "false" | "0" | "no" => Some(json!(false)),
                _ => None,
            },
        },
        ExtType::Enum => {
            let s = as_string(v);
            field.enum_values.contains(&s).then_some(Value::String(s))
        }
        ExtType::Timestamp => Some(parse_time_value(v)).filter(|t| !t.is_null()),
        ExtType::Position => {
            let lat = v.get("latitude").and_then(as_f64)?;
            let lon = v.get("longitude").and_then(as_f64)?;
            ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon))
                .then(|| json!({ "latitude": lat, "longitude": lon }))
        }
        ExtType::Json => Some(v.clone()),
    }
}

/// Extension keys a mapping writes (`ext.<key>` targets), by rule.
pub fn mapped_ext_keys(mapping: &crate::mapping::MappingSpec) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rule in &mapping.rules {
        for target in rule.fields.keys() {
            if let Some(key) = target.strip_prefix("ext.") {
                out.entry(key.to_owned())
                    .or_default()
                    .push(rule.name.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> ExtensionSchema {
        serde_json::from_value(json!({"version": 1, "fields": [
            {"key": "squawk", "type": "string"},
            {"key": "length_m", "type": "number", "unit": "m"},
            {"key": "persons", "type": "integer"},
            {"key": "nav_status", "type": "enum", "enum_values": ["0", "1", "5"]},
            {"key": "eta", "type": "timestamp"},
            {"key": "emergency", "type": "boolean", "default": false},
            {"key": "mission", "type": "string", "required": true}
        ]}))
        .unwrap()
    }

    #[test]
    fn validates_definitions() {
        schema().validate().unwrap();
        let bad = |fields: Value| {
            serde_json::from_value::<ExtensionSchema>(json!({"version": 2, "fields": fields}))
                .unwrap()
                .validate()
                .unwrap_err()
        };
        assert!(matches!(
            bad(json!([{"key": "Bad", "type": "string"}])),
            SchemaError::BadKey(_)
        ));
        assert!(matches!(
            bad(json!([{"key": "a", "type": "string"}, {"key": "a", "type": "number"}])),
            SchemaError::Duplicate(_)
        ));
        assert!(matches!(
            bad(json!([{"key": "e", "type": "enum"}])),
            SchemaError::NoEnumValues(_)
        ));
        assert!(matches!(
            bad(json!([{"key": "n", "type": "integer", "default": "x"}])),
            SchemaError::BadDefault { .. }
        ));
    }

    #[test]
    fn coerces_defaults_and_requires() {
        let s = schema();
        let mut ext: Map<String, Value> = serde_json::from_value(json!({
            "squawk": 7700, "length_m": "150.5", "persons": 3.5, "nav_status": 5,
            "eta": "2026-09-25 12:00:00 +0000 UTC", "mission": "ISR", "unmapped": 1
        }))
        .unwrap();
        let dropped = s.apply(&mut ext).unwrap();
        assert_eq!(dropped, ["persons"]);
        assert_eq!(ext["squawk"], "7700");
        assert_eq!(ext["length_m"], 150.5);
        assert_eq!(ext["nav_status"], "5");
        assert_eq!(ext["eta"], "2026-09-25T12:00:00.000Z");
        assert_eq!(ext["emergency"], false);
        assert_eq!(ext["unmapped"], 1, "undeclared keys are left alone");

        let mut missing: Map<String, Value> = Map::new();
        assert!(s.apply(&mut missing).unwrap_err().contains("mission"));
    }

    fn output_schema() -> ExtensionSchema {
        serde_json::from_value(json!({"version": 3, "fields": [
            {"key": "destination", "type": "string", "description": "Voyage destination"},
            {"key": "contact_phone", "type": "string", "description": "Ship's contact number"},
            {"key": "length_m", "type": "number", "unit": "m"},
            {"key": "state", "type": "string", "builtin": "state"},
            {"key": "speed_mps", "type": "number", "builtin": "speed_mps"},
            {"key": "sources", "type": "json", "builtin": "sources"}
        ]}))
        .unwrap()
    }

    fn track() -> SystemTrack {
        let now = chrono::Utc::now();
        let obs: ot_core::Observation = serde_json::from_value(json!({
            "schema_version": 3, "source_id": "ais", "source_track_key": "366123456",
            "observed_at": now, "received_at": now,
            "position": {"latitude": 32.7, "longitude": -117.2},
            "kinematics": {"speed_mps": 6.1},
            "ext": {"destination": "SAN DIEGO", "length_m": 210.0, "unmapped": "x"}
        }))
        .unwrap();
        SystemTrack::from_first_observation("OTK000000001".parse().unwrap(), obs)
    }

    #[test]
    fn card_wins_over_feed_and_differences_are_noticed() {
        let schema = output_schema();
        schema.validate().unwrap();
        let t = track();

        // No card: feed values and built-ins; unknown ext keys are not published.
        let (attrs, notices) = resolve_attributes(&schema, &t, None);
        assert_eq!(
            Value::Object(attrs),
            json!({"destination": "SAN DIEGO", "length_m": 210.0, "state": "tentative",
                   "speed_mps": 6.1, "sources": ["ais"]})
        );
        assert!(notices.is_empty());

        // The card adds a phone number, agrees on length (as text) and
        // disagrees on the destination: the card wins, with a notice.
        let card = json!({"contact_phone": "+1 555 0100", "destination": "LONG BEACH",
                          "length_m": "210"});
        let (attrs, notices) = resolve_attributes(&schema, &t, card.as_object());
        assert_eq!(attrs["contact_phone"], "+1 555 0100");
        assert_eq!(attrs["destination"], "LONG BEACH");
        assert_eq!(attrs["length_m"], 210.0);
        assert_eq!(
            notices,
            [AttributeNotice {
                key: "destination".into(),
                card: json!("LONG BEACH"),
                feed: json!("SAN DIEGO"),
                source_id: "ais".into(),
            }]
        );
    }

    #[test]
    fn cards_are_checked_against_the_schema() {
        let schema = output_schema();
        let ok = check_card(
            &schema,
            json!({"contact_phone": "+1 555 0100", "length_m": "210", "destination": ""})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            Value::Object(ok),
            json!({"contact_phone": "+1 555 0100", "length_m": 210.0})
        );
        let err = |v: Value| check_card(&schema, v.as_object().unwrap()).unwrap_err();
        assert!(err(json!({"nope": 1})).contains("not a field"));
        assert!(err(json!({"state": "confirmed"})).contains("filled by OpenTrack"));
        assert!(err(json!({"length_m": "long"})).contains("not a valid"));
    }

    #[test]
    fn linked_fields_must_declare_the_builtin_type() {
        let bad: ExtensionSchema = serde_json::from_value(json!({"version": 2, "fields": [
            {"key": "speed", "type": "string", "builtin": "speed_mps"}
        ]}))
        .unwrap();
        assert!(matches!(
            bad.validate(),
            Err(SchemaError::BuiltinType { .. })
        ));
    }
}
