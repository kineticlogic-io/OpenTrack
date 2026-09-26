//! External plugins: the Python SDK's example scorer served over a socket,
//! and a plugin that stops answering.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ot_core::Observation;
use ot_plugin::{Grants, Source};
use ot_source::plugin::ScoreCandidate;
use serde_json::json;

struct Served(Child);

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// sdk/python/examples/domain_scorer.py, served; None without python3.
fn serve_python_scorer() -> Option<(Served, String)> {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/python");
    let address = format!("127.0.0.1:{}", free_port());
    let mut child = Command::new("python3")
        .arg(sdk.join("examples/domain_scorer.py"))
        .args(["serve", "--address", &address])
        .env("PYTHONPATH", &sdk)
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // It says when it is listening.
    let mut line = String::new();
    BufReader::new(child.stderr.take()?).read_line(&mut line).ok()?;
    if !line.contains("serving") {
        eprintln!("skipped: the Python example did not start: {line}");
        let _ = child.kill();
        return None;
    }
    Some((Served(child), address))
}

fn obs(domain: &str, course: f64) -> Observation {
    serde_json::from_value(json!({
        "schema_version": 2, "source_id": "s", "source_track_key": "k",
        "observed_at": "2026-09-26T12:00:00Z", "received_at": "2026-09-26T12:00:00Z",
        "position": {"latitude": 36.9, "longitude": -76.2},
        "kinematics": {"course_deg": course, "speed_mps": 8.0},
        "classification": {"domain": domain}
    }))
    .unwrap()
}

#[test]
fn a_python_scorer_served_over_a_socket() {
    let Some((_server, address)) = serve_python_scorer() else {
        return;
    };
    let p = ot_plugin::load(&Source::External(address), &Grants::default()).unwrap();
    assert_eq!(p.manifest().name, "domain-scorer");
    assert!(p.decoder(&json!({})).is_err(), "not a codec");
    let mut s = p.scorer(&json!({"max_course_diff_deg": 45.0})).unwrap();
    let k = json!({"ln_lr": 3.0, "pass": true});
    let candidate = |o: Observation| ScoreCandidate {
        view: serde_json::to_value(o).unwrap(),
        kinematic: k.clone(),
    };
    let scores = s
        .score(
            &obs("surface", 90.0),
            &[candidate(obs("air", 90.0)), candidate(obs("surface", 200.0)), candidate(obs("surface", 100.0))],
        )
        .unwrap();
    assert_eq!(scores[0].evidence["veto"], "domain");
    assert_eq!(scores[1].evidence["veto"], "course");
    assert!(scores[2].pass && (scores[2].ln_lr - 3.0).abs() < 1e-9, "{scores:?}");
}

#[test]
fn a_plugin_that_stops_answering_times_out() {
    // Answers describe, then never again.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut r = BufReader::new(stream.try_clone().unwrap());
                let mut w = stream;
                let mut line = String::new();
                while r.read_line(&mut line).unwrap_or(0) > 0 {
                    let req: serde_json::Value = serde_json::from_str(&line).unwrap();
                    line.clear();
                    let result = match req["method"].as_str() {
                        Some("describe") => json!({"name": "stuck", "version": "1", "kinds": ["tracker"]}),
                        Some("open") => json!(null),
                        _ => {
                            std::thread::sleep(Duration::from_secs(5));
                            continue;
                        }
                    };
                    use std::io::Write;
                    writeln!(w, "{}", json!({"id": req["id"], "result": result})).unwrap();
                }
            });
        }
    });
    let grants = Grants {
        call_timeout_ms: 300,
        ..Grants::default()
    };
    let p = ot_plugin::load(&Source::External(address), &grants).unwrap();
    let mut t = p.tracker(&json!({})).unwrap();
    let started = std::time::Instant::now();
    let e = t.run(chrono::Utc::now(), false).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2), "{e}");
    // The session is not used again.
    let again = t.run(chrono::Utc::now(), false).unwrap_err();
    assert!(again.contains("stopped after an earlier failure"), "{again}");
}
