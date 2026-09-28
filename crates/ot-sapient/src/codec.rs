use prost_reflect::{DescriptorPool, DynamicMessage, Kind, MessageDescriptor, Value};
use serde_json::{Map, Value as JsonValue};

use crate::error::SapientError;

/// SAPIENT (BSI Flex 335) decoder.
///
/// Decodes raw protobuf bytes carrying a `SapientMessage` envelope into one or
/// more `serde_json::Value` records for the downstream mapping stage.
///
/// Uses `prost-reflect` with the file-descriptor-set shipped by `sapient-rs`
/// so no compile-time codegen beyond `sapient-rs` itself is required.
pub struct Decoder {
    #[allow(dead_code)]
    pool: DescriptorPool,
    message_desc: MessageDescriptor,
}

impl Decoder {
    /// Construct a decoder backed by the `sapient-rs` file-descriptor-set.
    pub fn new() -> Result<Self, SapientError> {
        let pool = DescriptorPool::decode(sapient_rs::FILE_DESCRIPTOR_SET_BYTES)
            .map_err(|e| SapientError::Descriptor(e.to_string()))?;

        let message_desc = pool
            .get_message_by_name("sapient_msg.bsi_flex_335_v2_0.SapientMessage")
            .ok_or_else(|| {
                SapientError::Descriptor("SapientMessage (v2_0) descriptor not found".into())
            })?;

        Ok(Self { pool, message_desc })
    }

    /// Decode one SAPIENT protobuf message (a complete `SapientMessage`) into
    /// records. Transport metadata is not attached here — callers are
    /// responsible for merging `frame.meta` into each record under `_frame`.
    pub fn decode(&self, bytes: &[u8]) -> Result<Vec<JsonValue>, SapientError> {
        let msg = DynamicMessage::decode(self.message_desc.clone(), bytes)
            .map_err(|e| SapientError::Decode(e.to_string()))?;

        let value = message_to_json(&msg).map_err(SapientError::Decode)?;
        Ok(vec![value])
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new().expect("SAPIENT descriptor pool is statically known-valid")
    }
}

fn message_to_json(msg: &DynamicMessage) -> Result<JsonValue, String> {
    let mut obj = Map::new();
    for (field, val) in msg.fields() {
        let kind = field.kind();

        if !field.supports_presence() && val.is_default(&kind) {
            continue;
        }
        if field.supports_presence() && !msg.has_field(&field) {
            continue;
        }

        let key = field.name();
        let json_val = value_to_json(val, &kind)?;
        obj.insert(key.to_owned(), json_val);
    }
    Ok(JsonValue::Object(obj))
}

fn value_to_json(val: &Value, kind: &Kind) -> Result<JsonValue, String> {
    match val {
        Value::Bool(b) => Ok(JsonValue::Bool(*b)),
        Value::I32(v) => Ok(JsonValue::Number((*v).into())),
        Value::I64(v) => Ok(JsonValue::Number(serde_json::Number::from(*v))),
        Value::U32(v) => Ok(JsonValue::Number((*v).into())),
        Value::U64(v) => Ok(JsonValue::Number(serde_json::Number::from(*v))),
        Value::F32(v) => float_to_json(*v as f64),
        Value::F64(v) => float_to_json(*v),
        Value::String(s) => Ok(JsonValue::String(s.to_owned())),
        Value::Bytes(b) => Ok(JsonValue::String(hex_encode(b))),
        Value::EnumNumber(n) => {
            if let Kind::Enum(enum_desc) = kind
                && let Some(val_desc) = enum_desc.get_value(*n)
            {
                return Ok(JsonValue::String(val_desc.name().to_owned()));
            }
            Ok(JsonValue::Number((*n).into()))
        }
        Value::Message(m) => message_to_json(m),
        Value::List(items) => {
            let arr: Result<Vec<_>, _> = items.iter().map(|v| value_to_json(v, kind)).collect();
            Ok(JsonValue::Array(arr?))
        }
        Value::Map(entries) => {
            let mut map = Map::new();
            for (k, v) in entries {
                let k_str = map_key_to_string(k);
                map.insert(k_str, value_to_json(v, kind)?);
            }
            Ok(JsonValue::Object(map))
        }
    }
}

fn float_to_json(v: f64) -> Result<JsonValue, String> {
    serde_json::Number::from_f64(v)
        .map(JsonValue::Number)
        .ok_or_else(|| format!("cannot encode float {v} as JSON"))
}

fn map_key_to_string(key: &prost_reflect::MapKey) -> String {
    match key {
        prost_reflect::MapKey::Bool(b) => b.to_string(),
        prost_reflect::MapKey::I32(v) => v.to_string(),
        prost_reflect::MapKey::I64(v) => v.to_string(),
        prost_reflect::MapKey::U32(v) => v.to_string(),
        prost_reflect::MapKey::U64(v) => v.to_string(),
        prost_reflect::MapKey::String(s) => s.clone(),
    }
}

const HEX_CHARS: &[u8] = b"0123456789abcdef";

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX_CHARS[(*b >> 4) as usize] as char);
        s.push(HEX_CHARS[(*b & 0xf) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use prost_types::Timestamp;
    use sapient_rs::bsi_flex_335_v2_0::{DetectionReport, sapient_message::Content};
    use sapient_rs::bsi_flex_335_v2_0::{
        EnuVelocity, Location, SapientMessage,
        detection_report::{LocationOneof, VelocityOneof},
    };

    fn build_test_message() -> SapientMessage {
        SapientMessage {
            timestamp: Some(Timestamp::date_time(2024, 1, 15, 10, 30, 0).unwrap()),
            node_id: Some("sensor-001".to_string()),
            destination_id: None,
            additional_information: None,
            content: Some(Content::DetectionReport(DetectionReport {
                report_id: Some("r-001".to_string()),
                object_id: Some("obj-123".to_string()),
                task_id: None,
                state: None,
                detection_confidence: Some(0.95),
                track_info: vec![],
                prediction_location: None,
                object_info: vec![],
                classification: vec![],
                behaviour: vec![],
                associated_file: vec![],
                signal: vec![],
                associated_detection: vec![],
                derived_detection: vec![],
                colour: None,
                id: None,
                location_oneof: Some(LocationOneof::Location(Location {
                    x: Some(37.7749),
                    y: Some(-122.4194),
                    z: Some(100.0),
                    x_error: None,
                    y_error: None,
                    z_error: None,
                    coordinate_system: None,
                    datum: None,
                    utm_zone: None,
                })),
                velocity_oneof: Some(VelocityOneof::EnuVelocity(EnuVelocity {
                    east_rate: Some(1.0),
                    north_rate: Some(2.0),
                    up_rate: Some(-0.5),
                    east_rate_error: None,
                    north_rate_error: None,
                    up_rate_error: None,
                })),
            })),
        }
    }

    #[test]
    fn decodes_detection_report_with_location_and_velocity() {
        let mut buf = Vec::new();
        build_test_message().encode(&mut buf).unwrap();

        let decoder = Decoder::default();
        let records = decoder.decode(&buf).unwrap();

        assert_eq!(records.len(), 1);
        let rec = &records[0];
        let obj = rec.as_object().unwrap();

        assert_eq!(
            obj.get("node_id"),
            Some(&JsonValue::String("sensor-001".into()))
        );
        assert!(
            obj.get("destination_id").is_none(),
            "unset proto3 field omitted"
        );

        let dr = obj
            .get("detection_report")
            .expect("detection_report present");
        let dr_obj = dr.as_object().unwrap();

        assert_eq!(
            dr_obj.get("object_id"),
            Some(&JsonValue::String("obj-123".into()))
        );
        let conf = dr_obj
            .get("detection_confidence")
            .and_then(JsonValue::as_f64)
            .expect("confidence is a number");
        assert!((conf - 0.95).abs() < 1e-6, "expected ~0.95 got {conf}");

        let loc = dr_obj.get("location").expect("location oneof set");
        let loc_obj = loc.as_object().unwrap();
        let lx = loc_obj.get("x").and_then(JsonValue::as_f64).unwrap();
        let ly = loc_obj.get("y").and_then(JsonValue::as_f64).unwrap();
        let lz = loc_obj.get("z").and_then(JsonValue::as_f64).unwrap();
        assert!((lx - 37.7749).abs() < 1e-4, "x: {lx}");
        assert!((ly - (-122.4194)).abs() < 1e-4, "y: {ly}");
        assert!((lz - 100.0).abs() < 1e-3, "z: {lz}");

        let vel = dr_obj.get("enu_velocity").expect("enu_velocity oneof set");
        let vel_obj = vel.as_object().unwrap();
        let er = vel_obj
            .get("east_rate")
            .and_then(JsonValue::as_f64)
            .unwrap();
        let nr = vel_obj
            .get("north_rate")
            .and_then(JsonValue::as_f64)
            .unwrap();
        let ur = vel_obj.get("up_rate").and_then(JsonValue::as_f64).unwrap();
        assert!((er - 1.0).abs() < 1e-6, "east_rate: {er}");
        assert!((nr - 2.0).abs() < 1e-6, "north_rate: {nr}");
        assert!((ur - (-0.5)).abs() < 1e-6, "up_rate: {ur}");
    }

    #[test]
    fn skips_unset_proto3_scalar_fields() {
        let msg = SapientMessage {
            timestamp: None,
            node_id: Some("bare-node".to_string()),
            destination_id: None,
            additional_information: None,
            content: None,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf).unwrap();

        let decoder = Decoder::default();
        let records = decoder.decode(&buf).unwrap();
        let obj = records[0].as_object().unwrap();

        assert_eq!(
            obj.get("node_id"),
            Some(&JsonValue::String("bare-node".into()))
        );
        assert!(obj.get("destination_id").is_none());
        assert!(obj.get("additional_information").is_none());
        assert!(obj.get("registration").is_none());
        assert!(obj.get("detection_report").is_none());
    }

    #[test]
    fn rejects_invalid_protobuf_bytes() {
        let decoder = Decoder::default();
        let bad = [0xFF, 0xFF, 0xFF, 0xFF];
        let err = decoder.decode(&bad).unwrap_err();
        assert!(matches!(err, SapientError::Decode(_)));
    }
}
