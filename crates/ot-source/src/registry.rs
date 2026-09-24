//! Registry lookup: resolve a track's identifiers against the identity
//! registry and grade how well the live report corroborates the entry.
//!
//! Grades, strongest first:
//! * `exact`: the broadcast name equals the identifier's expected name.
//! * `hull`: a token of the broadcast name is one of the entry's hull numbers.
//! * `name`: the distinctive (non-generic) tokens of the broadcast name are
//!   all in the entry's name, and add up to at least four characters.
//! * `generic`: the broadcast name is only generic words including a marker
//!   (e.g. "NAVY"), and the entry is a warship (has a hull code or a military
//!   classification).
//! * `stale`: the identifier is registered, but nothing corroborates it.
//!
//! The token lists and which entry fields are applied at which grades are
//! per-source configuration.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::expr::{RegexString, as_string};
use crate::path::Path;

/// One registry entity as seen through one identifier.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub entity_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Name the platform is expected to broadcast under this identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_name: Option<String>,
    /// All other entity fields (cot, flag, hull_code, ship_class, ...).
    #[serde(default)]
    pub fields: Map<String, Value>,
}

impl RegistryEntry {
    pub fn field(&self, name: &str) -> Option<&Value> {
        match name {
            "entity_id" => None,
            _ => self.fields.get(name),
        }
    }

    fn text(&self, name: &str) -> Option<String> {
        self.field(name)
            .map(as_string)
            .filter(|s| !s.trim().is_empty())
    }
}

/// Anything that can resolve identifiers. The worker holds a snapshot of the
/// SQLite registry that implements this.
pub trait RegistryLookup: Send + Sync {
    fn lookup(&self, scheme: &str, value: &str) -> Option<RegistryEntry>;
}

impl RegistryLookup for BTreeMap<(String, String), RegistryEntry> {
    fn lookup(&self, scheme: &str, value: &str) -> Option<RegistryEntry> {
        self.get(&(scheme.to_owned(), value.to_owned())).cloned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    Exact,
    Hull,
    Name,
    Generic,
    Stale,
}

impl Grade {
    pub fn as_str(self) -> &'static str {
        match self {
            Grade::Exact => "exact",
            Grade::Hull => "hull",
            Grade::Name => "name",
            Grade::Generic => "generic",
            Grade::Stale => "stale",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryStage {
    /// Identifier scheme to resolve (e.g. `mmsi`, `icao`).
    pub scheme: String,
    /// Observation field holding the broadcast name to grade.
    #[serde(default = "default_name_field")]
    pub broadcast_name: Path,
    /// Words that do not identify a platform (prefixes, nationalities...).
    #[serde(default)]
    pub generic_tokens: BTreeSet<String>,
    /// Generic words that still indicate a warship.
    #[serde(default)]
    pub markers: BTreeSet<String>,
    /// Classification pattern that makes an entry military for the `generic`
    /// grade (besides having a hull code).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub military_cot: Option<RegexString>,
    /// Grades at which `apply` is used; default exact, hull, name, generic.
    #[serde(default = "corroborated")]
    pub apply_grades: BTreeSet<Grade>,
    /// Observation field ← registry entry field, applied (overwriting) at the
    /// grades above. `name` and `entity_id` are available as fields too.
    #[serde(default)]
    pub apply: BTreeMap<String, String>,
}

fn default_name_field() -> Path {
    "name".parse().expect("path")
}

fn corroborated() -> BTreeSet<Grade> {
    [Grade::Exact, Grade::Hull, Grade::Name, Grade::Generic].into()
}

/// What the stage found, as recorded on the observation under `ext.registry`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegistryMatch {
    pub entity_id: String,
    pub grade: Grade,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub applied: bool,
}

fn tokens(s: &str) -> Vec<String> {
    static SPLIT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    SPLIT
        .get_or_init(|| Regex::new(r"[^A-Z0-9]+").expect("regex"))
        .split(&s.to_uppercase())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

fn hull_numbers(hull: &str) -> HashSet<String> {
    static DIGITS: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = DIGITS.get_or_init(|| Regex::new(r"\d+").expect("regex"));
    let mut out = HashSet::new();
    for m in re.find_iter(hull) {
        out.insert(m.as_str().to_owned());
        let stripped = m.as_str().trim_start_matches('0');
        if !stripped.is_empty() {
            out.insert(stripped.to_owned());
        }
    }
    out
}

impl RegistryStage {
    /// Grade a broadcast name against an entry.
    pub fn grade(&self, broadcast: Option<&str>, entry: &RegistryEntry) -> Grade {
        let live = broadcast.unwrap_or("").trim().to_uppercase();
        if live.is_empty() {
            return Grade::Stale;
        }
        if entry
            .expected_name
            .as_deref()
            .is_some_and(|e| !e.trim().is_empty() && e.trim().to_uppercase() == live)
        {
            return Grade::Exact;
        }
        let live_tokens = tokens(&live);
        let hull = entry.text("hull_code");
        if let Some(h) = &hull {
            let numbers = hull_numbers(h);
            if live_tokens.iter().any(|t| numbers.contains(t)) {
                return Grade::Hull;
            }
        }
        let distinctive: Vec<&String> = live_tokens
            .iter()
            .filter(|t| !self.generic_tokens.contains(*t))
            .collect();
        if !distinctive.is_empty() {
            let entry_tokens: HashSet<String> = entry
                .name
                .as_deref()
                .map(tokens)
                .unwrap_or_default()
                .into_iter()
                .collect();
            let len: usize = distinctive.iter().map(|t| t.len()).sum();
            if len >= 4 && distinctive.iter().all(|t| entry_tokens.contains(*t)) {
                return Grade::Name;
            }
            return Grade::Stale;
        }
        let marked = live_tokens.iter().any(|t| self.markers.contains(t));
        let military = hull.is_some()
            || self
                .military_cot
                .as_ref()
                .is_some_and(|re| entry.text("cot").is_some_and(|c| re.0.0.is_match(&c)));
        if marked && military {
            Grade::Generic
        } else {
            Grade::Stale
        }
    }

    /// Look up the observation's identifier of this stage's scheme, grade it,
    /// apply the entry at corroborated grades, and record the match under
    /// `ext.registry`. Returns the match, if any.
    pub fn run(&self, obs: &mut Value, registry: &dyn RegistryLookup) -> Option<RegistryMatch> {
        let value = obs
            .get("identifiers")
            .and_then(Value::as_array)?
            .iter()
            .find(|i| i.get("scheme").and_then(Value::as_str) == Some(self.scheme.as_str()))
            .and_then(|i| i.get("value"))
            .map(as_string)?;
        let entry = registry.lookup(&self.scheme, &value)?;
        let broadcast = self.broadcast_name.get(obs).map(as_string);
        let grade = self.grade(broadcast.as_deref(), &entry);
        let applied = entry.name.is_some() && self.apply_grades.contains(&grade);
        if applied {
            for (target, source) in &self.apply {
                let v = match source.as_str() {
                    "name" => entry.name.clone().map(Value::String),
                    "entity_id" => Some(Value::String(entry.entity_id.clone())),
                    other => entry.field(other).cloned(),
                };
                if let Some(v) = v.filter(|v| !v.is_null() && !as_string(v).trim().is_empty())
                    && let Ok(path) = target.parse::<Path>()
                {
                    path.set(obs, v);
                }
            }
        }
        let m = RegistryMatch {
            entity_id: entry.entity_id.clone(),
            grade,
            name: entry.name.clone(),
            applied,
        };
        let ext = obs
            .as_object_mut()
            .expect("observation is an object")
            .entry("ext")
            .or_insert_with(|| json!({}));
        if let Some(ext) = ext.as_object_mut() {
            ext.insert(
                "registry".into(),
                serde_json::to_value(&m).expect("match serialises"),
            );
        }
        Some(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage() -> RegistryStage {
        serde_json::from_value(json!({
            "scheme": "mmsi",
            "generic_tokens": ["USS", "USNS", "NAVY", "WARSHIP", "US", "GOV", "THE"],
            "markers": ["NAVY", "WARSHIP", "GOV", "USS"],
            "military_cot": "^a-.-(U|S-C)",
            "apply": { "classification.cot_type": "cot", "callsign": "name", "platform.flag": "flag" }
        }))
        .unwrap()
    }

    fn entry() -> RegistryEntry {
        RegistryEntry {
            entity_id: "e1".into(),
            name: Some("USS TED STEVENS".into()),
            expected_name: Some("TED STEVENS".into()),
            fields: serde_json::from_value(
                json!({"cot": "a-f-S-C-L-D-D", "flag": "US", "hull_code": "DDG-128"}),
            )
            .unwrap(),
        }
    }

    #[test]
    fn grades() {
        let s = stage();
        let e = entry();
        assert_eq!(s.grade(Some("ted stevens"), &e), Grade::Exact);
        assert_eq!(s.grade(Some("WARSHIP 128"), &e), Grade::Hull);
        assert_eq!(s.grade(Some("USS STEVENS"), &e), Grade::Name);
        assert_eq!(s.grade(Some("US NAVY"), &e), Grade::Generic);
        assert_eq!(s.grade(Some("FISHING BOAT"), &e), Grade::Stale);
        assert_eq!(s.grade(Some("  "), &e), Grade::Stale);
        assert_eq!(s.grade(None, &e), Grade::Stale);
        // Short distinctive tokens do not count as a name match.
        let mut short = e.clone();
        short.name = Some("USS ABC".into());
        assert_eq!(s.grade(Some("ABC"), &short), Grade::Stale);
        // Generic needs a warship entry.
        let mut civil = e.clone();
        civil.fields = serde_json::from_value(json!({"cot": "a-u-S-X"})).unwrap();
        assert_eq!(s.grade(Some("NAVY"), &civil), Grade::Stale);
    }

    #[test]
    fn hull_numbers_ignore_leading_zeros() {
        let mut e = entry();
        e.fields.insert("hull_code".into(), json!("FFG-062"));
        assert_eq!(stage().grade(Some("WARSHIP 62"), &e), Grade::Hull);
    }

    #[test]
    fn run_applies_only_when_corroborated() {
        let reg: BTreeMap<(String, String), RegistryEntry> =
            [(("mmsi".to_string(), "338924210".to_string()), entry())].into();
        let mut obs = json!({"identifiers": [{"scheme": "mmsi", "value": "338924210"}],
                             "name": "TED STEVENS", "classification": {"cot_type": "a-u-S"}});
        let m = stage().run(&mut obs, &reg).unwrap();
        assert_eq!(m.grade, Grade::Exact);
        assert_eq!(obs["classification"]["cot_type"], "a-f-S-C-L-D-D");
        assert_eq!(obs["callsign"], "USS TED STEVENS");
        assert_eq!(obs["platform"]["flag"], "US");
        assert_eq!(obs["ext"]["registry"]["grade"], "exact");

        let mut stale = json!({"identifiers": [{"scheme": "mmsi", "value": "338924210"}],
                               "name": "SOMETHING ELSE", "classification": {"cot_type": "a-u-S"}});
        let m = stage().run(&mut stale, &reg).unwrap();
        assert_eq!(m.grade, Grade::Stale);
        assert!(!m.applied);
        assert_eq!(stale["classification"]["cot_type"], "a-u-S");
        assert_eq!(stale["ext"]["registry"]["entity_id"], "e1");

        let mut miss = json!({"identifiers": [{"scheme": "mmsi", "value": "1"}]});
        assert!(stage().run(&mut miss, &reg).is_none());
    }
}
