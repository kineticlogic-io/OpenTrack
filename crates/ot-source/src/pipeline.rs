//! The per-source processing pipeline, free of I/O:
//!
//! frame → codec → records → reject → mapping → static join → registry →
//! affiliation → filter → throttle → observation.
//!
//! The worker feeds frames in and ships observations out; everything here is
//! deterministic given its inputs, so it can be replayed in tests and probes.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use chrono::{DateTime, Utc};
use ot_core::{Identifier, Observation};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codec::{Codec, CodecConfig};
use crate::expr::{Condition, ValueSpec, as_string};
use crate::frame::Frame;
use crate::mapping::{self, MapOutcome, MappingSpec, RuleKind};
use crate::registry::{RegistryLookup, RegistryStage};

/// Everything between the transport and the observation stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineSpec {
    pub codec: CodecConfig,
    pub mapping: MappingSpec,
    #[serde(default)]
    pub static_join: StaticJoinSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<RegistryStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affiliation: Option<AffiliationStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<FilterSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throttle: Option<ThrottleSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticJoinSpec {
    /// How long cached identity fields stay usable.
    #[serde(default = "week")]
    pub ttl_secs: u64,
}

fn week() -> u64 {
    7 * 24 * 3600
}

impl Default for StaticJoinSpec {
    fn default() -> Self {
        Self { ttl_secs: week() }
    }
}

/// Derive affiliation from a country code, e.g. a registry flag or the
/// ICAO address block. Country lists are configuration, not code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AffiliationStage {
    /// Value (over the observation) holding an ISO country code.
    pub country: ValueSpec,
    #[serde(default)]
    pub friendly: BTreeSet<String>,
    #[serde(default)]
    pub hostile: BTreeSet<String>,
    #[serde(default)]
    pub neutral: BTreeSet<String>,
    /// Affiliation when a country is known but in no list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otherwise: Option<String>,
    /// Affiliation when no country is known (null leaves it unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_country: Option<String>,
}

impl AffiliationStage {
    fn run(&self, obs: &mut Value) {
        let country = self.country.eval(obs);
        let code = as_string(&country).trim().to_uppercase();
        let aff = if code.is_empty() {
            self.unknown_country.clone()
        } else if self.friendly.contains(&code) {
            Some("friend".to_owned())
        } else if self.hostile.contains(&code) {
            Some("hostile".to_owned())
        } else if self.neutral.contains(&code) {
            Some("neutral".to_owned())
        } else {
            self.otherwise.clone()
        };
        if let Some(a) = aff {
            let path: crate::path::Path = "classification.affiliation".parse().expect("path");
            path.set(obs, Value::String(a));
        }
    }
}

/// Keep or drop mapped observations. Paths address the observation (and
/// `ext.registry.*` once the registry stage has run).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_if: Option<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_if: Option<Condition>,
}

impl FilterSpec {
    fn keep(&self, obs: &Value) -> bool {
        self.keep_if.as_ref().is_none_or(|c| c.eval(obs))
            && !self.drop_if.as_ref().is_some_and(|c| c.eval(obs))
    }
}

/// Per source track: never more often than `min_interval_secs`; between that
/// and `heartbeat_secs` only if it moved at least `min_move_m`; always after
/// the heartbeat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThrottleSpec {
    #[serde(default)]
    pub min_interval_secs: f64,
    #[serde(default = "ten_minutes")]
    pub heartbeat_secs: f64,
    #[serde(default)]
    pub min_move_m: f64,
}

fn ten_minutes() -> f64 {
    600.0
}

#[derive(Debug, Clone, Copy)]
struct LastWrite {
    at: DateTime<Utc>,
    lat: f64,
    lon: f64,
}

impl ThrottleSpec {
    fn due(&self, last: Option<&LastWrite>, obs: &Observation) -> bool {
        let Some(last) = last else { return true };
        let dt = (obs.observed_at - last.at).num_milliseconds() as f64 / 1000.0;
        if dt < self.min_interval_secs {
            return false;
        }
        if dt >= self.heartbeat_secs {
            return true;
        }
        self.min_move_m > 0.0
            && distance_m(
                last.lat,
                last.lon,
                obs.position.latitude,
                obs.position.longitude,
            ) >= self.min_move_m
    }
}

/// Equirectangular distance, good to well under 1% at throttle scales.
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dy = (lat2 - lat1) * 111_320.0;
    let dx = (lon2 - lon1) * 111_320.0 * ((lat1 + lat2) / 2.0).to_radians().cos();
    (dx * dx + dy * dy).sqrt()
}

/// Cached identity fields for one source track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StaticEntry {
    pub fields: Value,
    #[serde(default)]
    pub identifiers: Vec<Identifier>,
    pub updated_at: DateTime<Utc>,
}

/// Counters for one batch, merged into per-source metrics by the worker.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counts {
    pub frames: u64,
    pub decode_errors: u64,
    pub records: u64,
    pub rejected: HashMap<String, u64>,
    pub unmatched: u64,
    pub statics: u64,
    pub invalid: u64,
    pub filtered: u64,
    pub throttled: u64,
    pub emitted: u64,
    pub grades: HashMap<&'static str, u64>,
}

impl Counts {
    /// Flatten to `(metric, count)` pairs, skipping zeros.
    pub fn pairs(&self) -> Vec<(String, u64)> {
        let mut out: Vec<(String, u64)> = [
            ("frames", self.frames),
            ("decode_error", self.decode_errors),
            ("records", self.records),
            ("unmatched", self.unmatched),
            ("static", self.statics),
            ("invalid", self.invalid),
            ("filtered", self.filtered),
            ("throttled", self.throttled),
            ("emitted", self.emitted),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(k, n)| (k.to_owned(), n))
        .collect();
        let rejected: u64 = self.rejected.values().sum();
        if rejected > 0 {
            out.push(("rejected".into(), rejected));
        }
        for (reason, n) in &self.rejected {
            out.push((format!("rejected:{reason}"), *n));
        }
        for (grade, n) in &self.grades {
            out.push((format!("grade:{grade}"), *n));
        }
        out
    }
}

/// Output of one frame.
#[derive(Debug, Default)]
pub struct Output {
    pub observations: Vec<Observation>,
    /// Static cache entries that changed, for the worker to persist.
    pub statics: Vec<(String, StaticEntry)>,
    /// The last decode or validation error seen, for status reporting.
    pub last_error: Option<String>,
}

pub struct Pipeline {
    source_id: String,
    spec: PipelineSpec,
    codec: Codec,
    statics: HashMap<String, StaticEntry>,
    throttle: HashMap<String, LastWrite>,
    pub counts: Counts,
}

impl Pipeline {
    pub fn new(
        source_id: impl Into<String>,
        spec: PipelineSpec,
    ) -> Result<Self, mapping::MappingError> {
        spec.mapping.validate()?;
        Ok(Self {
            source_id: source_id.into(),
            codec: Codec::new(spec.codec.clone()),
            spec,
            statics: HashMap::new(),
            throttle: HashMap::new(),
            counts: Counts::default(),
        })
    }

    pub fn spec(&self) -> &PipelineSpec {
        &self.spec
    }

    /// Seed the static cache (e.g. from Redis after a restart).
    pub fn load_static(&mut self, key: String, entry: StaticEntry) {
        self.statics.insert(key, entry);
    }

    /// Take and reset the counters.
    pub fn take_counts(&mut self) -> Counts {
        std::mem::take(&mut self.counts)
    }

    /// Forget throttle and static state older than the static TTL.
    pub fn prune(&mut self, now: DateTime<Utc>) {
        let ttl = chrono::Duration::from_std(Duration::from_secs(self.spec.static_join.ttl_secs))
            .unwrap_or(chrono::Duration::MAX);
        self.statics.retain(|_, e| now - e.updated_at < ttl);
        let heartbeat = self
            .spec
            .throttle
            .as_ref()
            .map_or(0.0, |t| t.heartbeat_secs);
        let keep = chrono::Duration::milliseconds((heartbeat * 1000.0) as i64 * 2);
        self.throttle.retain(|_, w| now - w.at < keep);
    }

    pub fn process(&mut self, frame: &Frame, registry: &dyn RegistryLookup) -> Output {
        let mut out = Output::default();
        self.counts.frames += 1;
        let records = match self.codec.decode(frame) {
            Ok(r) => r,
            Err(e) => {
                self.counts.decode_errors += 1;
                out.last_error = Some(e.to_string());
                return out;
            }
        };
        for record in records {
            self.process_record(&record, frame.received_at, registry, &mut out);
        }
        out
    }

    fn process_record(
        &mut self,
        record: &Value,
        received_at: DateTime<Utc>,
        registry: &dyn RegistryLookup,
        out: &mut Output,
    ) {
        self.counts.records += 1;
        let mapped = match self.spec.mapping.apply(record) {
            MapOutcome::Rejected(reason) => {
                *self.counts.rejected.entry(reason).or_default() += 1;
                return;
            }
            MapOutcome::Unmatched => {
                self.counts.unmatched += 1;
                return;
            }
            MapOutcome::Mapped(m) => m,
        };
        // Static rules first, so a record carrying both (AIS msg 19) joins
        // its own identity fields.
        for m in mapped.iter().filter(|m| m.kind == RuleKind::Static) {
            self.counts.statics += 1;
            let entry = self
                .statics
                .entry(m.key.clone())
                .or_insert_with(|| StaticEntry {
                    fields: Value::Object(Default::default()),
                    identifiers: Vec::new(),
                    updated_at: received_at,
                });
            merge_over(&mut entry.fields, &m.fields);
            for id in &m.identifiers {
                if !entry.identifiers.contains(id) {
                    entry.identifiers.push(id.clone());
                }
            }
            entry.updated_at = received_at;
            out.statics.push((m.key.clone(), entry.clone()));
        }
        for m in mapped
            .into_iter()
            .filter(|m| m.kind == RuleKind::Observation)
        {
            let mut m = m;
            if let Some(s) = self.statics.get(&m.key) {
                mapping::fill_missing(&mut m.fields, &s.fields);
                for id in &s.identifiers {
                    if !m.identifiers.contains(id) {
                        m.identifiers.push(id.clone());
                    }
                }
            }
            // Stages below work on the observation's JSON form.
            let mut obs = match mapping::finalize(
                &self.source_id,
                self.spec.mapping.schema_version,
                &m,
                received_at,
            )
            .and_then(|o| {
                serde_json::to_value(&o).map_err(|e| mapping::FinalizeError::Invalid(e.to_string()))
            }) {
                Ok(v) => v,
                Err(e) => {
                    self.counts.invalid += 1;
                    out.last_error = Some(e.to_string());
                    continue;
                }
            };
            if let Some(stage) = &self.spec.registry {
                let grade = stage
                    .run(&mut obs, registry)
                    .map_or("none", |m| m.grade.as_str());
                *self.counts.grades.entry(grade).or_default() += 1;
            }
            if let Some(stage) = &self.spec.affiliation {
                stage.run(&mut obs);
            }
            if let Some(filter) = &self.spec.filter
                && !filter.keep(&obs)
            {
                self.counts.filtered += 1;
                continue;
            }
            let obs: Observation = match serde_json::from_value(obs) {
                Ok(o) => o,
                Err(e) => {
                    self.counts.invalid += 1;
                    out.last_error = Some(e.to_string());
                    continue;
                }
            };
            if let Some(t) = &self.spec.throttle {
                let key = obs.source_track_key.clone();
                if !t.due(self.throttle.get(&key), &obs) {
                    self.counts.throttled += 1;
                    continue;
                }
                self.throttle.insert(
                    key,
                    LastWrite {
                        at: obs.observed_at,
                        lat: obs.position.latitude,
                        lon: obs.position.longitude,
                    },
                );
            }
            self.counts.emitted += 1;
            out.observations.push(obs);
        }
    }
}

/// Overwrite `base` with every non-null value in `newer` (static updates).
fn merge_over(base: &mut Value, newer: &Value) {
    let (Value::Object(b), Value::Object(n)) = (base, newer) else {
        return;
    };
    for (k, v) in n {
        match (b.get_mut(k), v) {
            (_, Value::Null) => {}
            (Some(existing @ Value::Object(_)), Value::Object(_)) => merge_over(existing, v),
            _ => {
                b.insert(k.clone(), v.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::RegistryEntry;
    use serde_json::json;
    use std::collections::BTreeMap;

    type Reg = BTreeMap<(String, String), RegistryEntry>;

    fn spec() -> PipelineSpec {
        serde_json::from_value(json!({
            "codec": { "type": "json" },
            "mapping": { "rules": [
                { "name": "static", "kind": "static", "when": { "path": "t", "eq": "static" },
                  "key": "id", "fields": { "name": "name", "platform.flag": "flag" } },
                { "name": "pos", "when": { "path": "t", "eq": "pos" }, "key": "id",
                  "identifiers": [ { "scheme": "mmsi", "value": "id" } ],
                  "fields": { "position.latitude": "lat", "position.longitude": "lon",
                              "observed_at": { "path": "ts", "transforms": ["time"] },
                              "classification.cot_type": { "const": "a-u-S" } } }
            ] },
            "registry": { "markers": ["NAVY"],
                          "apply": { "classification.cot_type": "cot", "platform.flag": "flag" } },
            "affiliation": { "country": "platform.flag", "friendly": ["US"], "hostile": ["XX"] },
            "filter": { "keep_if": { "any": [
                { "path": "ext.registry.entity_id", "exists": true },
                { "path": "classification.cot_type", "matches": "^a-.-S-C" } ] } },
            "throttle": { "min_interval_secs": 30, "heartbeat_secs": 600, "min_move_m": 50 }
        }))
        .unwrap()
    }

    fn reg() -> Reg {
        let e = RegistryEntry {
            entity_id: "e1".into(),
            name: Some("USS EXAMPLE".into()),
            expected_name: Some("EXAMPLE".into()),
            fields: serde_json::from_value(
                json!({"cot": "a-u-S-C", "flag": "US", "hull_code": "DDG-1"}),
            )
            .unwrap(),
        };
        [(("mmsi".into(), "1".into()), e)].into()
    }

    fn feed(p: &mut Pipeline, reg: &Reg, rec: Value) -> Output {
        p.process(&Frame::new(rec.to_string().into_bytes()), reg)
    }

    fn pos(id: u32, ts: &str, lat: f64) -> Value {
        json!({"t": "pos", "id": id, "lat": lat, "lon": -117.0, "ts": ts})
    }

    #[test]
    fn join_registry_affiliation_filter_and_throttle() {
        let reg = reg();
        let mut p = Pipeline::new("ais", spec()).unwrap();
        let st = feed(
            &mut p,
            &reg,
            json!({"t": "static", "id": 1, "name": "EXAMPLE"}),
        );
        assert_eq!(st.statics.len(), 1);

        let out = feed(&mut p, &reg, pos(1, "2026-09-24T12:00:00Z", 32.0));
        assert_eq!(out.observations.len(), 1);
        let o = &out.observations[0];
        assert_eq!(o.name.as_deref(), Some("EXAMPLE"), "static join");
        assert_eq!(
            o.classification.cot_type.as_deref(),
            Some("a-u-S-C"),
            "registry applied"
        );
        assert_eq!(
            o.classification.affiliation,
            Some(ot_core::Affiliation::Friend)
        );
        assert_eq!(o.ext["registry"]["grade"], "exact");

        // 10 s later, moved ~111 m: inside the 30 s floor, throttled.
        let out = feed(&mut p, &reg, pos(1, "2026-09-24T12:00:10Z", 32.001));
        assert!(out.observations.is_empty());
        // 60 s later, moved: due.
        assert_eq!(
            feed(&mut p, &reg, pos(1, "2026-09-24T12:01:00Z", 32.002))
                .observations
                .len(),
            1
        );
        // 120 s later, stationary: not due until the heartbeat.
        assert!(
            feed(&mut p, &reg, pos(1, "2026-09-24T12:03:00Z", 32.002))
                .observations
                .is_empty()
        );
        assert_eq!(
            feed(&mut p, &reg, pos(1, "2026-09-24T12:11:00Z", 32.002))
                .observations
                .len(),
            1
        );

        // Not in the registry and not military: filtered out.
        assert!(
            feed(&mut p, &reg, pos(2, "2026-09-24T12:00:00Z", 33.0))
                .observations
                .is_empty()
        );

        let c = p.take_counts();
        assert_eq!(c.emitted, 3);
        assert_eq!(c.throttled, 2);
        assert_eq!(c.filtered, 1);
        assert_eq!(c.statics, 1);
        assert_eq!(c.grades.get("exact"), Some(&5));
        assert_eq!(c.grades.get("none"), Some(&1));
    }

    #[test]
    fn decode_errors_and_invalid_records_are_counted() {
        let reg = reg();
        let mut p = Pipeline::new("ais", spec()).unwrap();
        let out = p.process(&Frame::new(&b"not json"[..]), &reg);
        assert!(out.last_error.is_some());
        feed(&mut p, &reg, pos(1, "2026-09-24T12:00:00Z", 95.0));
        feed(&mut p, &reg, json!({"t": "other"}));
        let c = p.take_counts();
        assert_eq!((c.decode_errors, c.invalid, c.unmatched), (1, 1, 1));
    }
}
