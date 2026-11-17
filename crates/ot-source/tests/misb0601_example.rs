//! The MISB ST 0601 example source: a UAS Datalink packet through the
//! built-in `misb0601` codec plugin and the example mapping, to a friendly
//! air track.

use chrono::Utc;
use misb::st0601::{Builder, Tag};
use ot_source::mapping::{MapOutcome, finalize};
use ot_source::plugin;
use ot_source::source::SourceSpec;

#[test]
fn misb0601_example_maps_the_platform() {
    let source: SourceSpec =
        serde_json::from_str(include_str!("../../../docs/examples/misb0601-udp.json"))
            .expect("valid source JSON");
    source.validate().expect("valid source specification");

    // Values as in the public "Day Flight.mpg" sample (samples.ffmpeg.org).
    let mut b = Builder::new(1_245_257_585_099_653);
    b.text(Tag::PlatformTailNumber, "UAS-7")
        .unwrap()
        .float(Tag::PlatformHeadingAngle, 86.1)
        .unwrap()
        .float(Tag::SensorLatitude, 54.6813233)
        .unwrap()
        .float(Tag::SensorLongitude, -110.1685598)
        .unwrap()
        .float(Tag::SensorEllipsoidHeightExtended, 1520.0)
        .unwrap()
        .float(Tag::PlatformGroundSpeed, 46.0)
        .unwrap();
    let bytes = b.finish();

    let codec = plugin::plugin("misb0601").expect("built in");
    let mut decoder = codec
        .decoder(&serde_json::json!({"emit": "platform", "verify_checksum": true}))
        .unwrap();
    let records = decoder.decode(&bytes, Utc::now()).unwrap();
    assert_eq!(records.len(), 1);

    let MapOutcome::Mapped(mapped) = source
        .pipeline
        .mappings()
        .next()
        .unwrap()
        .apply(&records[0])
    else {
        panic!("platform record did not match the source mapping");
    };
    let o = finalize(&source.id, 1, &mapped[0], Utc::now()).expect("valid track");
    assert_eq!(o.source_track_key, "UAS-7");
    assert_eq!(o.callsign.as_deref(), Some("UAS-7"));
    assert!((o.position.latitude - 54.6813233).abs() < 1e-6);
    assert!((o.position.longitude + 110.1685598).abs() < 1e-6);
    assert!((o.position.altitude_hae_m.unwrap() - 1520.0).abs() < 0.01);
    assert_eq!(o.kinematics.speed_mps, Some(46.0));
    assert!((o.kinematics.course_deg.unwrap() - 86.1).abs() < 0.01);
    assert_eq!(
        o.observed_at
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "2009-06-17T16:53:05.099Z"
    );
}
