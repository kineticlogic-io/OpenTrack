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
}

fn default_priority() -> i64 {
    100
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SourceError {
    #[error("source id must be 1-64 characters of a-z, 0-9, '-' or '_', got {0:?}")]
    BadId(String),
    #[error("source name must not be empty")]
    NoName,
    #[error(transparent)]
    Mapping(#[from] crate::mapping::MappingError),
    #[error("framing: {0}")]
    Framing(#[from] crate::frame::FrameError),
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
        if let TransportConfig::TcpClient { framing, .. }
        | TransportConfig::TcpServer { framing, .. } = &self.transport
        {
            crate::frame::Framer::new(framing.clone())?;
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
