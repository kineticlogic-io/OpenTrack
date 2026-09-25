//! A source: transport + pipeline, stored as one JSON document.

use serde::{Deserialize, Serialize};

use crate::pipeline::PipelineSpec;
use crate::transport::TransportConfig;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpec {
    /// Stable id: lowercase letters, digits, `-` and `_` (max 64). It names
    /// Redis keys and prefixes source track ids, so it never changes.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub transport: TransportConfig,
    pub pipeline: PipelineSpec,
    /// Higher wins best-source selection (phase 3).
    #[serde(default = "default_priority")]
    pub priority: i64,
    /// Whether the feed reports tracks (a key per object) or detections
    /// (anonymous plots, associated with system tracks by the engine).
    #[serde(default, skip_serializing_if = "Reports::is_tracks")]
    pub reports: Reports,
}

fn default_priority() -> i64 {
    100
}

/// What a source's observations are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reports {
    /// Each key is one object over time (AIS, ADS-B, TAK, a radar's tracks).
    #[default]
    Tracks,
    /// Each report is a single detection with no identity of its own.
    Detections,
}

impl Reports {
    fn is_tracks(&self) -> bool {
        *self == Reports::Tracks
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SourceError {
    #[error("source id must be 1-64 characters of a-z, 0-9, '-' or '_', got {0:?}")]
    BadId(String),
    #[error("source name must not be empty")]
    NoName,
    #[error("{0}")]
    Transport(String),
    #[error(transparent)]
    Mapping(#[from] crate::mapping::MappingError),
    #[error("framing: {0}")]
    Framing(#[from] crate::frame::FrameError),
    #[error("mapping targets schema version {wanted}, but it is {found}")]
    SchemaVersion { wanted: u32, found: String },
    #[error("rule {rule:?} maps ext.{key}, which schema version {version} does not define")]
    UndeclaredExtension {
        rule: String,
        key: String,
        version: u32,
    },
}

impl SourceSpec {
    pub fn validate(&self) -> Result<(), SourceError> {
        let id_ok = !self.id.is_empty()
            && self.id.len() <= 64
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if !id_ok {
            return Err(SourceError::BadId(self.id.clone()));
        }
        if self.name.trim().is_empty() {
            return Err(SourceError::NoName);
        }
        self.pipeline.mapping.validate()?;
        self.transport.check().map_err(SourceError::Transport)?;
        if let TransportConfig::TcpClient { framing, .. }
        | TransportConfig::TcpServer { framing, .. } = &self.transport
        {
            crate::frame::Framer::new(framing.clone())?;
        }
        Ok(())
    }

    /// Validate against the extension schema version the mapping targets
    /// (`None` if that version is not published).
    pub fn validate_against(
        &self,
        schema: Option<&crate::schema::ExtensionSchema>,
    ) -> Result<(), SourceError> {
        self.validate()?;
        let wanted = self.pipeline.mapping.schema_version;
        let Some(schema) = schema.filter(|s| s.version == wanted) else {
            return Err(SourceError::SchemaVersion {
                wanted,
                found: "not published".into(),
            });
        };
        for (key, rules) in crate::schema::mapped_ext_keys(&self.pipeline.mapping) {
            if schema.field(&key).is_none() {
                return Err(SourceError::UndeclaredExtension {
                    rule: rules[0].clone(),
                    key,
                    version: wanted,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(id: &str) -> serde_json::Value {
        json!({
            "id": id, "name": "Test",
            "transport": { "type": "udp", "bind": "0.0.0.0:6969" },
            "pipeline": { "codec": { "type": "cot_xml" }, "mapping": { "rules": [
                { "name": "event", "key": "event.@uid", "fields": {
                    "position.latitude": "event.point.@lat", "position.longitude": "event.point.@lon" } } ] } }
        })
    }

    #[test]
    fn ids_are_restricted() {
        let ok: SourceSpec = serde_json::from_value(spec("cot-udp_1")).unwrap();
        ok.validate().unwrap();
        assert_eq!(ok.priority, 100);
        for bad in ["", "CoT", "a b", "x:y"] {
            let s: SourceSpec = serde_json::from_value(spec(bad)).unwrap();
            assert!(matches!(s.validate(), Err(SourceError::BadId(_))), "{bad}");
        }
    }
}
