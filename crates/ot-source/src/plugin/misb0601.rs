//! MISB ST 0601 (the UAS Datalink Local Set, the KLV metadata in STANAG 4609
//! drone video) as a codec plugin, on the `misb` decoder. Each packet gives
//! a `platform` record: the aircraft's position, course and speed, its name
//! and where the camera looks. With `emit: "all"` it also gives a
//! `frame_center` record, the ground point at the center of the picture.
//!
//! A frame is one datagram of the KLV stream (MPEG-TS demuxed, or KLV sent
//! on its own); it may hold several packets. Each packet's checksum (tag 1)
//! is verified unless `verify_checksum` is false; packets that fail are
//! dropped.

use chrono::{DateTime, Utc};
use misb::st0601::{Packet, Tag};
use serde_json::{Map, Value, json};

use super::{Kind, Manifest, OptionKind, Plugin, PluginDecoder, PluginOption};

pub struct Misb0601 {
    manifest: Manifest,
}

impl Misb0601 {
    pub fn new() -> Self {
        let options = vec![
            PluginOption {
                name: "emit".into(),
                label: "Records".into(),
                help:
                    "platform: one per packet; all: also the frame center (where the camera looks)"
                        .into(),
                kind: OptionKind::Choice {
                    choices: ["platform", "all"].map(String::from).to_vec(),
                    default: "platform".into(),
                },
            },
            PluginOption {
                name: "verify_checksum".into(),
                label: "Verify checksum".into(),
                help: "Drop packets whose checksum (tag 1) does not match".into(),
                kind: OptionKind::Bool { default: true },
            },
        ];
        Self {
            manifest: Manifest {
                name: "misb0601".into(),
                version: "1".into(),
                description:
                    "MISB ST 0601 UAS Datalink Local Set (STANAG 4609 video metadata): platform and frame center records"
                        .into(),
                kinds: vec![Kind::Codec],
                options,
                framing: None,
            },
        }
    }
}

impl Default for Misb0601 {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for Misb0601 {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn decoder(&self, options: &Value) -> Result<Box<dyn PluginDecoder>, String> {
        let opts = match options {
            Value::Null => Map::new(),
            Value::Object(m) => m.clone(),
            other => return Err(format!("misb0601 options must be an object, got {other}")),
        };
        let mut frame_center = false;
        let mut verify = true;
        for (k, v) in &opts {
            match (k.as_str(), v) {
                ("emit", Value::String(s)) if s == "platform" => frame_center = false,
                ("emit", Value::String(s)) if s == "all" => frame_center = true,
                ("emit", Value::Null) => {}
                ("emit", other) => {
                    return Err(format!("emit must be \"platform\" or \"all\", got {other}"));
                }
                ("verify_checksum", Value::Bool(b)) => verify = *b,
                ("verify_checksum", Value::Null) => {}
                ("verify_checksum", other) => {
                    return Err(format!(
                        "verify_checksum must be true or false, got {other}"
                    ));
                }
                (other, _) => return Err(format!("misb0601 has no option {other}")),
            }
        }
        Ok(Box::new(Decoder {
            frame_center,
            verify,
        }))
    }
}

struct Decoder {
    frame_center: bool,
    verify: bool,
}

/// The platform's name: tail number, call sign, designation or mission ID,
/// the first one the packet has.
fn platform_id(p: &Packet<'_>) -> String {
    [
        Tag::PlatformTailNumber,
        Tag::PlatformCallSign,
        Tag::PlatformDesignation,
        Tag::MissionId,
    ]
    .into_iter()
    .filter_map(|t| p.text(t))
    .map(str::trim)
    .find(|s| !s.is_empty())
    .unwrap_or("UAS")
    .to_string()
}

fn put(r: &mut Map<String, Value>, k: &str, v: Option<f64>) {
    if let Some(v) = v.filter(|v| v.is_finite()) {
        r.insert(k.into(), json!(v));
    }
}

fn put_text(r: &mut Map<String, Value>, k: &str, v: Option<&str>) {
    if let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) {
        r.insert(k.into(), json!(v));
    }
}

impl Decoder {
    fn records(&self, p: &Packet<'_>, received_at: DateTime<Utc>, out: &mut Vec<Value>) {
        let time = p
            .timestamp()
            .and_then(|us| i64::try_from(us).ok())
            .and_then(DateTime::from_timestamp_micros)
            .unwrap_or(received_at)
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let id = platform_id(p);

        if let Some(pos) = p.sensor_position() {
            let mut r = Map::new();
            r.insert("record".into(), json!("platform"));
            r.insert("time".into(), json!(time));
            r.insert("platform_id".into(), json!(id));
            r.insert("lat".into(), json!(pos.lat));
            r.insert("lon".into(), json!(pos.lon));
            // Height above the ellipsoid when the packet has it, else MSL.
            match p.sensor_hae() {
                Some(h) => put(&mut r, "alt_hae_m", Some(h)),
                None => put(&mut r, "alt_msl_m", pos.height),
            }
            let heading = p.platform_heading();
            put(&mut r, "heading_deg", heading);
            put(
                &mut r,
                "track_deg",
                p.float(Tag::PlatformCourseAngle).or(heading),
            );
            put(&mut r, "speed_mps", p.float(Tag::PlatformGroundSpeed));
            put(&mut r, "pitch_deg", p.platform_pitch());
            put(&mut r, "roll_deg", p.platform_roll());
            put_text(&mut r, "mission_id", p.text(Tag::MissionId));
            put_text(
                &mut r,
                "platform_designation",
                p.text(Tag::PlatformDesignation),
            );
            put_text(&mut r, "image_source", p.text(Tag::ImageSourceSensor));
            let mut sensor = Map::new();
            put(
                &mut sensor,
                "azimuth_deg",
                heading
                    .zip(p.float(Tag::SensorRelativeAzimuthAngle))
                    .map(|(h, a)| (h + a).rem_euclid(360.0)),
            );
            put(
                &mut sensor,
                "hfov_deg",
                p.float(Tag::SensorHorizontalFieldOfView),
            );
            put(
                &mut sensor,
                "vfov_deg",
                p.float(Tag::SensorVerticalFieldOfView),
            );
            put(&mut sensor, "slant_range_m", p.float(Tag::SlantRange));
            if !sensor.is_empty() {
                r.insert("sensor".into(), Value::Object(sensor));
            }
            if let Some(fc) = p.frame_center() {
                r.insert(
                    "frame_center".into(),
                    json!({"lat": fc.lat, "lon": fc.lon, "alt_msl_m": fc.height}),
                );
            }
            out.push(Value::Object(r));
        }

        if self.frame_center
            && let Some(fc) = p.frame_center()
        {
            let mut r = Map::new();
            r.insert("record".into(), json!("frame_center"));
            r.insert("time".into(), json!(time));
            r.insert("platform_id".into(), json!(id));
            r.insert("lat".into(), json!(fc.lat));
            r.insert("lon".into(), json!(fc.lon));
            match p.float(Tag::FrameCenterHeightAboveEllipsoid) {
                Some(h) => put(&mut r, "alt_hae_m", Some(h)),
                None => put(&mut r, "alt_msl_m", fc.height),
            }
            // CE90 to a one-sigma circular error.
            put(
                &mut r,
                "circular_error_m",
                p.float(Tag::TargetErrorEstimateCe90)
                    .map(|ce90| ce90 / 2.146),
            );
            out.push(Value::Object(r));
        }
    }
}

impl PluginDecoder for Decoder {
    fn decode(&mut self, bytes: &[u8], received_at: DateTime<Utc>) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        let mut first_error = None;
        for unit in misb::klv::split(bytes) {
            if !misb::klv::same_key(unit.key, &misb::st0601::KEY) {
                continue;
            }
            let parsed = if self.verify {
                Packet::parse(unit.raw)
            } else {
                Packet::parse_unverified(unit.raw)
            };
            match parsed {
                Ok((p, _)) => self.records(&p, received_at, &mut out),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_error {
            Some(e) if out.is_empty() => Err(format!("ST 0601 packet: {e}")),
            _ => Ok(out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use misb::st0601::Builder;

    fn packet() -> Vec<u8> {
        let mut b = Builder::new(1_700_000_000_000_000);
        b.text(Tag::PlatformTailNumber, "N101")
            .unwrap()
            .float(Tag::PlatformHeadingAngle, 90.0)
            .unwrap()
            .float(Tag::SensorLatitude, 54.68)
            .unwrap()
            .float(Tag::SensorLongitude, -110.17)
            .unwrap()
            .float(Tag::SensorTrueAltitude, 1532.0)
            .unwrap()
            .float(Tag::PlatformGroundSpeed, 46.0)
            .unwrap()
            .float(Tag::SensorRelativeAzimuthAngle, 45.0)
            .unwrap()
            .float(Tag::FrameCenterLatitude, 54.75)
            .unwrap()
            .float(Tag::FrameCenterLongitude, -110.05)
            .unwrap();
        b.finish()
    }

    #[test]
    fn manifest_is_valid() {
        let m = Misb0601::new();
        m.manifest().validate().unwrap();
        assert!(m.manifest().provides(Kind::Codec));
        let d = super::super::default_options(m.manifest());
        assert_eq!(d["emit"], "platform");
        assert_eq!(d["verify_checksum"], true);
        assert!(m.decoder(&d).is_ok());
    }

    #[test]
    fn options_are_checked() {
        let m = Misb0601::new();
        for bad in [
            json!({"emit": "targets"}),
            json!({"verify_checksum": "yes"}),
            json!({"x": 1}),
            json!(3),
        ] {
            assert!(m.decoder(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn decodes_platform_and_frame_center() {
        let mut d = Misb0601::new().decoder(&json!({"emit": "all"})).unwrap();
        // two packets in one datagram
        let mut bytes = packet();
        bytes.extend(packet());
        let records = d.decode(&bytes, Utc::now()).unwrap();
        assert_eq!(records.len(), 4);
        let p = &records[0];
        assert_eq!(p["record"], "platform");
        assert_eq!(p["platform_id"], "N101");
        assert_eq!(p["time"], "2023-11-14T22:13:20.000Z");
        assert!((p["lat"].as_f64().unwrap() - 54.68).abs() < 1e-6);
        assert!((p["alt_msl_m"].as_f64().unwrap() - 1532.0).abs() < 0.5);
        assert_eq!(p["speed_mps"], 46.0);
        assert!((p["sensor"]["azimuth_deg"].as_f64().unwrap() - 135.0).abs() < 0.01);
        let fc = &records[1];
        assert_eq!(fc["record"], "frame_center");
        assert!((fc["lon"].as_f64().unwrap() + 110.05).abs() < 1e-6);
    }

    #[test]
    fn bad_checksum_is_dropped_unless_told_otherwise() {
        let mut bytes = packet();
        let n = bytes.len();
        bytes[n - 1] ^= 0xFF;
        let mut d = Misb0601::new().decoder(&Value::Null).unwrap();
        let e = d.decode(&bytes, Utc::now()).unwrap_err();
        assert!(e.contains("checksum"), "{e}");
        let mut d = Misb0601::new()
            .decoder(&json!({"verify_checksum": false}))
            .unwrap();
        assert_eq!(d.decode(&bytes, Utc::now()).unwrap().len(), 1);
        // a good packet next to a bad one still comes through
        let mut mixed = bytes.clone();
        mixed.extend(packet());
        let mut d = Misb0601::new().decoder(&Value::Null).unwrap();
        assert_eq!(d.decode(&mixed, Utc::now()).unwrap().len(), 1);
    }
}
