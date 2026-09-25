//! System tracks: the correlated picture OpenTrack publishes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::schema::{Observation, TrackState};
use crate::uid::Uid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingType {
    /// Paired by the correlation engine.
    Auto,
    /// Paired by an operator.
    Manual,
}

/// A source track reporting for a system track (a `REPORTS_FOR` edge).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contributor {
    pub source_id: String,
    pub source_track_key: String,
    pub pairing: PairingType,
    pub confidence: f64,
    pub last_report: DateTime<Utc>,
}

/// A card value that differs from what a feed reports for the same
/// attribute. The card wins; the difference is surfaced to operators.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttributeNotice {
    /// Output schema field key.
    pub key: String,
    /// The value on the entity's card (published).
    pub card: serde_json::Value,
    /// What the feed reported (not published).
    pub feed: serde_json::Value,
    pub source_id: String,
}

/// Current state of one system track, as held in `tms:sys:<uid>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemTrack {
    pub uid: Uid,
    pub state: TrackState,
    /// The best-source view of the object. Until best-source selection
    /// arrives with correlation (phase 3) this is the latest observation.
    pub view: Observation,
    /// Which contributor supplied each field group, keyed by group name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub provenance: std::collections::BTreeMap<String, String>,
    pub contributors: Vec<Contributor>,
    /// UIDs merged into this track, still resolvable as aliases.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<Uid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// Registry entity (baseball card) this track resolves to, when the
    /// registry match is corroborated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    /// Output schema values, resolved from the card, the feed and built-ins;
    /// published as `attributes`.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub attributes: serde_json::Map<String, serde_json::Value>,
    /// Card values that differ from the feed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<AttributeNotice>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub observation_count: u64,
}

impl SystemTrack {
    /// A new tentative system track seeded from one observation.
    pub fn from_first_observation(uid: Uid, obs: Observation) -> Self {
        let contributor = Contributor {
            source_id: obs.source_id.clone(),
            source_track_key: obs.source_track_key.clone(),
            pairing: PairingType::Auto,
            confidence: 1.0,
            last_report: obs.observed_at,
        };
        Self {
            uid,
            state: TrackState::Tentative,
            first_seen: obs.observed_at,
            last_seen: obs.observed_at,
            observation_count: 1,
            view: obs,
            provenance: Default::default(),
            contributors: vec![contributor],
            aliases: Vec::new(),
            groups: Vec::new(),
            entity_id: None,
            attributes: Default::default(),
            notices: Vec::new(),
        }
    }
}
