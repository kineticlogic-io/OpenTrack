//! The published track message: `opentrack.track.v1`.
//!
//! Every system track is published as JSON on its own subject,
//! `<prefix>.<track_id>` (by default `tracks.tms-OTK000000123`). A consumer
//! subscribes to `tracks.>`; the stream keeps the latest message per subject,
//! so a consumer that starts late still receives the whole current picture.
//!
//! Two operations, carried both in the `OT-Op` header and in the body's `op`:
//!
//! * `upsert`: the full current state of the track. Replaces anything the
//!   consumer holds for that `track_id`.
//! * `delete`: the track was retired (dropped, merged away or deleted by an
//!   operator). The consumer removes it. The delete stays in the stream as
//!   that subject's latest message until it ages out.
//!
//! Conventions: timestamps are RFC 3339 UTC, units are SI (metres, metres per
//! second, degrees true), enums are lower-case strings, and absent values are
//! omitted rather than sent as null.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::schema::{
    Affiliation, Domain, Ellipse, Identifier, Kinematics, Platform, TrackState, Uncertainty,
};
use crate::track::{Contributor, SystemTrack};
use crate::uid::Uid;

/// Schema name, in the `OT-Schema` header and the body's `schema`.
pub const TRACK_SCHEMA: &str = "opentrack.track.v1";

/// Default subject prefix for system tracks.
pub const TRACKS_SUBJECT: &str = "tracks";

/// Header names.
pub const HEADER_OP: &str = "OT-Op";
pub const HEADER_SCHEMA: &str = "OT-Schema";

/// Who is publishing, stamped into each message's `publisher`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishContext {
    /// Identifies this OpenTrack instance, e.g. `opentrack-OTK`.
    pub node_id: String,
    /// OpenTrack version.
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Upsert,
    Delete,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Upsert => "upsert",
            Op::Delete => "delete",
        }
    }
}

/// The subject a track is published on.
pub fn subject(prefix: &str, uid: Uid) -> String {
    format!("{prefix}.{}", uid.doc_id())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackMessage {
    pub schema: String,
    pub op: Op,
    /// `tms-<UID>`: the id every consumer keys on.
    pub track_id: String,
    pub uid: Uid,
    pub state: TrackState,
    pub classification: WireClassification,
    #[serde(default, skip_serializing_if = "WireIdentity::is_empty")]
    pub identity: WireIdentity,
    pub position: WirePosition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<WireUncertainty>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub kinematics: Kinematics,
    #[serde(default, skip_serializing_if = "is_default")]
    pub platform: Platform,
    /// Confidence in the track, 0 to 1, when the source reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// When the published position was observed.
    pub observed_at: DateTime<Utc>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub observation_count: u64,
    /// Source tracks reporting for this track.
    pub contributors: Vec<Contributor>,
    /// Which contributor supplied each field group.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub field_sources: BTreeMap<String, String>,
    /// Track ids merged into this one; consumers should treat them as this track.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// Extension schema version `ext` follows.
    pub ext_schema_version: u32,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub ext: Map<String, Value>,
    pub publisher: PublishContext,
    pub published_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WireClassification {
    /// CoT type, with the affiliation atom applied (e.g. `a-f-S`).
    pub cot_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<Domain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affiliation: Option<Affiliation>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WireIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,
    /// Every identifier, each with its scheme (`mmsi`, `icao`, `elnot`, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<Identifier>,
}

impl WireIdentity {
    fn is_empty(&self) -> bool {
        self.name.is_none() && self.callsign.is_none() && self.identifiers.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WirePosition {
    pub lat: f64,
    pub lon: f64,
    /// Height above the WGS84 ellipsoid, metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_hae_m: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WireUncertainty {
    /// Circular error probable: as reported, else derived from the ellipse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cep_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ellipse: Option<Ellipse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_error_m: Option<f64>,
}

/// Body of a `delete`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteMessage {
    pub schema: String,
    pub op: Op,
    pub track_id: String,
    pub uid: Uid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub deleted_at: DateTime<Utc>,
    pub publisher: PublishContext,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

fn finite(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite())
}

/// Encode a system track as an `upsert`.
pub fn to_message(
    track: &SystemTrack,
    ctx: &PublishContext,
    published_at: DateTime<Utc>,
) -> TrackMessage {
    let v = &track.view;
    let k = v.kinematics;
    TrackMessage {
        schema: TRACK_SCHEMA.into(),
        op: Op::Upsert,
        track_id: track.uid.doc_id(),
        uid: track.uid,
        state: track.state,
        classification: WireClassification {
            cot_type: v.classification.cot_type_or_derived(),
            domain: v.classification.effective_domain(),
            affiliation: v.classification.effective_affiliation(),
        },
        identity: WireIdentity {
            name: v.name.clone(),
            callsign: v.callsign.clone(),
            identifiers: v.identifiers.clone(),
        },
        position: WirePosition {
            lat: v.position.latitude,
            lon: v.position.longitude,
            alt_hae_m: finite(v.position.altitude_hae_m),
        },
        uncertainty: v.uncertainty.map(|u: Uncertainty| WireUncertainty {
            cep_m: finite(u.cep_m()),
            ellipse: u.ellipse,
            vertical_error_m: finite(u.vertical_error_m),
        }),
        kinematics: Kinematics {
            course_deg: finite(k.course_deg),
            speed_mps: finite(k.speed_mps),
            heading_deg: finite(k.heading_deg),
            vertical_rate_mps: finite(k.vertical_rate_mps),
        },
        platform: v.platform.clone(),
        confidence: finite(v.provenance.confidence),
        observed_at: v.observed_at,
        first_seen: track.first_seen,
        last_seen: track.last_seen,
        observation_count: track.observation_count,
        contributors: track.contributors.clone(),
        field_sources: track.provenance.clone(),
        aliases: track.aliases.iter().map(|u| u.doc_id()).collect(),
        groups: track.groups.clone(),
        ext_schema_version: v.schema_version,
        ext: v.ext.clone(),
        publisher: ctx.clone(),
        published_at,
    }
}

/// Encode a retired track as a `delete`.
pub fn delete_message(
    uid: Uid,
    reason: Option<String>,
    ctx: &PublishContext,
    deleted_at: DateTime<Utc>,
) -> DeleteMessage {
    DeleteMessage {
        schema: TRACK_SCHEMA.into(),
        op: Op::Delete,
        track_id: uid.doc_id(),
        uid,
        reason,
        deleted_at,
        publisher: ctx.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::sample;

    fn ctx() -> PublishContext {
        PublishContext {
            node_id: "opentrack-OTK".into(),
            version: "0.1.0".into(),
        }
    }

    fn at() -> DateTime<Utc> {
        "2026-09-25T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn upsert_is_clean_json() {
        let uid: Uid = "OTK000000001".parse().unwrap();
        let track = SystemTrack::from_first_observation(uid, sample());
        let v = serde_json::to_value(to_message(&track, &ctx(), at())).unwrap();

        assert_eq!(subject(TRACKS_SUBJECT, uid), "tracks.tms-OTK000000001");
        assert_eq!(v["schema"], "opentrack.track.v1");
        assert_eq!(v["op"], "upsert");
        assert_eq!(v["track_id"], "tms-OTK000000001");
        assert_eq!(v["uid"], "OTK000000001");
        assert_eq!(v["state"], "tentative");
        assert_eq!(v["classification"]["cot_type"], "a-f-S");
        assert_eq!(v["classification"]["domain"], "surface");
        assert_eq!(v["classification"]["affiliation"], "friend");
        assert_eq!(v["identity"]["name"], "TED STEVENS");
        assert_eq!(v["identity"]["identifiers"][0]["scheme"], "mmsi");
        assert!(v["identity"].get("callsign").is_none());
        assert!(v["position"]["lat"].is_f64());
        assert!((v["uncertainty"]["cep_m"].as_f64().unwrap() - 88.5).abs() < 1e-9);
        assert_eq!(v["uncertainty"]["ellipse"]["semi_major_m"], 100.0);
        assert_eq!(v["first_seen"], "2026-09-24T12:00:00Z");
        assert_eq!(v["published_at"], "2026-09-25T00:00:00Z");
        assert_eq!(v["publisher"]["node_id"], "opentrack-OTK");
        assert_eq!(v["contributors"][0]["source_id"], "synthetic");
        // Round-trips, so consumers written in Rust can use the same types.
        let back: TrackMessage = serde_json::from_value(v).unwrap();
        assert_eq!(back.uid, uid);
    }

    #[test]
    fn absent_values_are_omitted_not_null() {
        let mut obs = sample();
        obs.kinematics = Kinematics {
            speed_mps: Some(f64::NAN),
            ..Default::default()
        };
        obs.uncertainty = None;
        obs.name = None;
        obs.identifiers.clear();
        obs.platform = Default::default();
        let uid: Uid = "OTK000000002".parse().unwrap();
        let text = serde_json::to_string(&to_message(
            &SystemTrack::from_first_observation(uid, obs),
            &ctx(),
            at(),
        ))
        .unwrap();
        assert!(!text.contains("null"), "{text}");
        let v: Value = serde_json::from_str(&text).unwrap();
        for k in ["uncertainty", "kinematics", "identity", "platform"] {
            assert!(v.get(k).is_none(), "{k} should be omitted");
        }
    }

    #[test]
    fn delete_carries_the_id_and_reason() {
        let uid: Uid = "OTK000000003".parse().unwrap();
        let v = serde_json::to_value(delete_message(uid, Some("dropped".into()), &ctx(), at()))
            .unwrap();
        assert_eq!(v["op"], "delete");
        assert_eq!(v["track_id"], "tms-OTK000000003");
        assert_eq!(v["reason"], "dropped");
        assert_eq!(v["deleted_at"], "2026-09-25T00:00:00Z");
    }
}
