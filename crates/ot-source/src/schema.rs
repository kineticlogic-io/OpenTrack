//! Admin-defined extension fields: the versioned part of the track schema.
//!
//! The core schema is fixed (correlation depends on it). Extensions are
//! defined by the administrator, grouped into versions that are immutable
//! once published, mapped as `ext.<key>`, and published under
//! `attributes_json.ext.<key>`. Every mapping names the version it targets.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
            field.enum_values.contains(&s).then(|| Value::String(s))
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
}
