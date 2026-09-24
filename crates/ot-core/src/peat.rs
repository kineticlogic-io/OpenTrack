//! Encoding of system tracks as `peat.track.v1.Track` documents.
//!
//! peat-node validates documents against its type registry. The rules below
//! were established against a live node and are easy to break:
//!
//! * `track_id`, `position` and `source` are required.
//! * A sub-object is optional as a whole, but once present every key in it is
//!   mandatory; a partial `position` fails validation. Optional sub-objects
//!   are therefore omitted, never sent half-filled or as null.
//! * Enums are proto integers, and are not range-checked by the node.
//! * Timestamps are prost `{seconds, nanos}`, not RFC 3339.
//! * `attributes_json` is a JSON *string*, not an object.
//! * Numbers must be finite; JSON has no NaN.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use crate::track::SystemTrack;

/// Default authoritative collection for system tracks.
pub const TRACKS_COLLECTION: &str = "tracks";

/// `peat.track.v1.SourceType::FUSED`: a system track is correlated output.
const SOURCE_TYPE_FUSED: i64 = 4;

/// Who is publishing, stamped into each document's `source`.
#[derive(Debug, Clone)]
pub struct PublishContext {
    /// Identifies this OpenTrack instance on the mesh, e.g. `opentrack-OTK`.
    pub node_id: String,
    /// OpenTrack version, published as `source.model_version`.
    pub model_version: String,
}

/// Encode a system track as a peat-node `tracks` document.
pub fn to_document(track: &SystemTrack, ctx: &PublishContext) -> Value {
    let v = &track.view;
    let unc = v.uncertainty.unwrap_or_default();
    let cep = finite(unc.cep_m()).unwrap_or(0.0);
    let vertical_error = finite(unc.vertical_error_m).unwrap_or(0.0);

    let best_sensor = track
        .contributors
        .iter()
        .max_by_key(|c| c.last_report)
        .map(|c| c.source_id.clone())
        .unwrap_or_else(|| v.source_id.clone());

    let mut doc = Map::new();
    doc.insert("track_id".into(), json!(track.uid.doc_id()));
    doc.insert(
        "classification".into(),
        json!(v.classification.cot_type_or_derived()),
    );
    doc.insert(
        "confidence".into(),
        json!(finite(v.provenance.confidence).unwrap_or(0.0)),
    );
    doc.insert("state".into(), json!(track.state.wire_value()));
    doc.insert(
        "position".into(),
        json!({
            "latitude": v.position.latitude,
            "longitude": v.position.longitude,
            "altitude": finite(v.position.altitude_hae_m).unwrap_or(0.0),
            "cep_m": cep,
            "vertical_error_m": vertical_error,
        }),
    );
    doc.insert(
        "source".into(),
        json!({
            "node_id": ctx.node_id,
            "sensor_id": best_sensor,
            "model_version": ctx.model_version,
            "source_type": SOURCE_TYPE_FUSED,
        }),
    );
    doc.insert(
        "attributes_json".into(),
        json!(attributes(track).to_string()),
    );
    doc.insert("observation_count".into(), json!(track.observation_count));
    doc.insert("first_seen".into(), timestamp(track.first_seen));
    doc.insert("last_seen".into(), timestamp(track.last_seen));

    let k = &v.kinematics;
    if let (Some(course), Some(speed)) = (finite(k.course_deg), finite(k.speed_mps)) {
        doc.insert(
            "velocity".into(),
            json!({
                "bearing": course,
                "speed_mps": speed,
                "vertical_speed_mps": finite(k.vertical_rate_mps).unwrap_or(0.0),
            }),
        );
    }
    if let Some(heading) = finite(k.heading_deg) {
        doc.insert(
            "kinematics".into(),
            json!({
                "velocity": finite(k.speed_mps).unwrap_or(0.0),
                "heading": heading,
                "acceleration": 0.0,
                "vertical_speed": finite(k.vertical_rate_mps).unwrap_or(0.0),
            }),
        );
    }
    if v.uncertainty.is_some() {
        let linear = unc
            .ellipse
            .and_then(|e| finite(Some(e.semi_major_m)))
            .unwrap_or(cep);
        doc.insert(
            "position_error".into(),
            json!({
                "circular_error": cep,
                "linear_error": linear,
                "vertical_error": vertical_error,
            }),
        );
    }
    Value::Object(doc)
}

/// Everything beyond the mapped core: identity, the full ellipse, platform,
/// contributors, provenance, aliases, groups, schema version and extensions.
fn attributes(track: &SystemTrack) -> Value {
    let v = &track.view;
    let mut a = Map::new();
    let mut put = |k: &str, val: Value| {
        if !val.is_null() {
            a.insert(k.to_owned(), val);
        }
    };
    put("uid", json!(track.uid));
    put("name", json!(v.name));
    put("callsign", json!(v.callsign));
    if !v.identifiers.is_empty() {
        put("identifiers", json!(v.identifiers));
    }
    put("domain", json!(v.classification.effective_domain()));
    put(
        "affiliation",
        json!(v.classification.effective_affiliation()),
    );
    put("cot_type", json!(v.classification.cot_type));
    if let Some(e) = v.uncertainty.and_then(|u| u.ellipse) {
        put("ellipse", json!(e));
    }
    if v.platform != Default::default() {
        put("platform", json!(v.platform));
    }
    if v.provenance.sensor_code.is_some() || v.provenance.source_code.is_some() {
        put("provenance", json!(v.provenance));
    }
    put("contributors", json!(track.contributors));
    if !track.provenance.is_empty() {
        put("field_sources", json!(track.provenance));
    }
    if !track.aliases.is_empty() {
        put(
            "aliases",
            json!(track.aliases.iter().map(|u| u.doc_id()).collect::<Vec<_>>()),
        );
    }
    if !track.groups.is_empty() {
        put("groups", json!(track.groups));
    }
    put("schema_version", json!(v.schema_version));
    put("core_schema_version", json!(crate::CORE_SCHEMA_VERSION));
    if !v.ext.is_empty() {
        put("ext", Value::Object(v.ext.clone()));
    }
    Value::Object(a)
}

fn finite(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite())
}

/// prost `Timestamp` encoding.
pub fn timestamp(t: DateTime<Utc>) -> Value {
    json!({ "seconds": t.timestamp(), "nanos": t.timestamp_subsec_nanos() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::sample;
    use crate::track::SystemTrack;
    use crate::uid::Uid;

    fn ctx() -> PublishContext {
        PublishContext {
            node_id: "opentrack-OTK".into(),
            model_version: "0.1.0".into(),
        }
    }

    fn keys(v: &Value) -> Vec<&str> {
        let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        k.sort_unstable();
        k
    }

    #[test]
    fn full_track_has_complete_sub_objects() {
        let uid: Uid = "OTK000000001".parse().unwrap();
        let doc = to_document(&SystemTrack::from_first_observation(uid, sample()), &ctx());

        assert_eq!(doc["track_id"], "tms-OTK000000001");
        assert_eq!(doc["classification"], "a-f-S");
        assert_eq!(doc["state"], 1);
        assert_eq!(
            keys(&doc["position"]),
            [
                "altitude",
                "cep_m",
                "latitude",
                "longitude",
                "vertical_error_m"
            ]
        );
        assert!((doc["position"]["cep_m"].as_f64().unwrap() - 88.5).abs() < 1e-9);
        assert_eq!(
            keys(&doc["source"]),
            ["model_version", "node_id", "sensor_id", "source_type"]
        );
        assert_eq!(doc["source"]["sensor_id"], "synthetic");
        assert_eq!(doc["source"]["source_type"], 4);
        assert_eq!(
            keys(&doc["velocity"]),
            ["bearing", "speed_mps", "vertical_speed_mps"]
        );
        assert_eq!(
            keys(&doc["kinematics"]),
            ["acceleration", "heading", "velocity", "vertical_speed"]
        );
        assert_eq!(
            keys(&doc["position_error"]),
            ["circular_error", "linear_error", "vertical_error"]
        );
        assert_eq!(doc["first_seen"]["seconds"], 1_790_251_200_i64);
        assert_eq!(doc["first_seen"]["nanos"], 0);

        let attrs: Value = serde_json::from_str(doc["attributes_json"].as_str().unwrap()).unwrap();
        assert_eq!(attrs["uid"], "OTK000000001");
        assert_eq!(attrs["name"], "TED STEVENS");
        assert_eq!(attrs["domain"], "surface");
        assert_eq!(attrs["affiliation"], "friend");
        assert_eq!(attrs["ellipse"]["semi_major_m"], 100.0);
        assert_eq!(attrs["identifiers"][0]["scheme"], "mmsi");
        assert!(attrs.get("callsign").is_none());
    }

    #[test]
    fn optional_sub_objects_are_omitted_not_partial() {
        let mut obs = sample();
        obs.kinematics = Default::default();
        obs.kinematics.speed_mps = Some(3.0);
        obs.uncertainty = None;
        let uid: Uid = "OTK000000002".parse().unwrap();
        let doc = to_document(
            &SystemTrack::from_first_observation(uid, obs.clone()),
            &ctx(),
        );
        for k in ["velocity", "kinematics", "position_error"] {
            assert!(doc.get(k).is_none(), "{k} should be omitted");
        }
        assert_eq!(doc["position"]["cep_m"], 0.0);

        // Heading alone still publishes kinematics, with speed 0 if unknown.
        obs.kinematics = Default::default();
        obs.kinematics.heading_deg = Some(90.0);
        let doc = to_document(&SystemTrack::from_first_observation(uid, obs), &ctx());
        assert_eq!(doc["kinematics"]["velocity"], 0.0);
        assert!(doc.get("velocity").is_none());
        // Serialises without NaN.
        serde_json::to_string(&doc).unwrap();
    }
}
