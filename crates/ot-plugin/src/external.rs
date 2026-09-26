//! External plugins: a program of its own serving the plugin interface over
//! a socket, for code that cannot run as WebAssembly (Python with numpy or
//! Stone Soup, a GPU, a licensed library). It runs where the operator
//! starts it, with whatever it needs; OpenTrack only connects.
//!
//! The protocol is JSON lines. Each request is `{"id", "method", "params"}`
//! and each reply `{"id", "result"}` or `{"id", "error"}`. Every decoder,
//! tracker or scorer OpenTrack opens is one connection: it starts with
//! `open` (`{"kind": "codec" | "tracker" | "scorer", "options"}`), and its
//! state lives with that connection on the plugin's side.
//!
//! | method | params | result |
//! |---|---|---|
//! | `describe` | | the manifest |
//! | `open` | `kind`, `options` | null |
//! | `decode` | `frame` (base64), `received_at_ms` | records |
//! | `hints` | | hints or null |
//! | `push` | `plot` (observation), `received_at_ms` | null |
//! | `run` | `now_ms`, `force` | tracks (observations) |
//! | `score` | `report`, `candidates` | scores |

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::Context;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use ot_core::Observation;
use ot_source::plugin::{
    Manifest, Plugin, PluginDecoder, PluginScorer, PluginTracker, Score, ScoreCandidate,
    StreamHints,
};
use serde_json::{Value, json};

use crate::Grants;

/// Time allowed to connect.
const CONNECT: Duration = Duration::from_secs(5);

pub struct ExternalPlugin {
    address: String,
    manifest: Manifest,
    timeout: Duration,
}

impl ExternalPlugin {
    /// Connect, and read the plugin's manifest.
    pub fn connect(address: &str, grants: &Grants) -> anyhow::Result<Self> {
        let timeout = Duration::from_millis(grants.call_timeout_ms.max(1));
        let mut c = Conn::open(address, timeout)?;
        let manifest = c
            .call("describe", Value::Null)
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            address: address.to_owned(),
            manifest: serde_json::from_value(manifest).context("the plugin's manifest")?,
            timeout,
        })
    }

    fn session(&self, kind: &str, options: &Value) -> Result<Conn, String> {
        let mut c = Conn::open(&self.address, self.timeout).map_err(|e| format!("{e:#}"))?;
        let options = if options.is_null() {
            json!({})
        } else {
            options.clone()
        };
        c.call("open", json!({ "kind": kind, "options": options }))?;
        Ok(c)
    }
}

struct Conn {
    reader: BufReader<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
    next: u64,
    broken: Option<String>,
}

impl Conn {
    fn open(address: &str, timeout: Duration) -> anyhow::Result<Self> {
        let (reader, writer): (Box<dyn Read + Send>, Box<dyn Write + Send>) =
            if let Some(path) = address.strip_prefix("unix:") {
                let s = UnixStream::connect(path).with_context(|| format!("connect {address}"))?;
                s.set_read_timeout(Some(timeout))?;
                (Box::new(s.try_clone()?), Box::new(s))
            } else {
                let hostport = address.strip_prefix("tcp://").unwrap_or(address);
                let addr = hostport
                    .to_socket_addrs()
                    .with_context(|| format!("resolve {address}"))?
                    .next()
                    .with_context(|| format!("resolve {address}"))?;
                let s = TcpStream::connect_timeout(&addr, CONNECT)
                    .with_context(|| format!("connect {address}"))?;
                s.set_read_timeout(Some(timeout))?;
                s.set_nodelay(true)?;
                (Box::new(s.try_clone()?), Box::new(s))
            };
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            next: 1,
            broken: None,
        })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if let Some(e) = &self.broken {
            return Err(format!("stopped after an earlier failure: {e}"));
        }
        let r = self.exchange(method, params);
        if let Err(e) = &r
            && !e.starts_with("plugin: ")
        {
            self.broken = Some(e.clone());
        }
        r
    }

    fn exchange(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        let mut line = json!({ "id": id, "method": method, "params": params }).to_string();
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .and_then(|_| self.writer.flush())
            .map_err(|e| format!("send {method}: {e}"))?;
        let mut reply = String::new();
        let n = self
            .reader
            .read_line(&mut reply)
            .map_err(|e| format!("{method}: no reply ({e})"))?;
        if n == 0 {
            return Err(format!("{method}: the plugin closed the connection"));
        }
        let mut v: Value = serde_json::from_str(&reply)
            .map_err(|e| format!("{method}: a reply is not JSON: {e}"))?;
        if v["id"] != json!(id) {
            return Err(format!("{method}: reply to {} instead of {id}", v["id"]));
        }
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            return Err(format!(
                "plugin: {}",
                e.as_str().map_or_else(|| e.to_string(), str::to_owned)
            ));
        }
        Ok(v["result"].take())
    }
}

impl Plugin for ExternalPlugin {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn decoder(&self, options: &Value) -> Result<Box<dyn PluginDecoder>, String> {
        Ok(Box::new(Decoder {
            conn: self.session("codec", options)?,
            hints: None,
        }))
    }

    fn tracker(&self, options: &Value) -> Result<Box<dyn PluginTracker>, String> {
        Ok(Box::new(Session(self.session("tracker", options)?)))
    }

    fn scorer(&self, options: &Value) -> Result<Box<dyn PluginScorer>, String> {
        Ok(Box::new(Session(self.session("scorer", options)?)))
    }
}

struct Decoder {
    conn: Conn,
    hints: Option<StreamHints>,
}

impl PluginDecoder for Decoder {
    fn decode(&mut self, bytes: &[u8], received_at: DateTime<Utc>) -> Result<Vec<Value>, String> {
        let frame = base64::engine::general_purpose::STANDARD.encode(bytes);
        let records = self.conn.call(
            "decode",
            json!({ "frame": frame, "received_at_ms": received_at.timestamp_millis() }),
        )?;
        if let Ok(h) = self.conn.call("hints", Value::Null) {
            self.hints = StreamHints::from_json(&h);
        }
        match records {
            Value::Array(r) => Ok(r),
            Value::Null => Ok(Vec::new()),
            other => Err(format!("decode returned {other}, not a list")),
        }
    }

    fn hints(&self) -> Option<StreamHints> {
        self.hints.clone()
    }
}

struct Session(Conn);

impl PluginTracker for Session {
    fn push(&mut self, plot: Observation, received_at: DateTime<Utc>) {
        let _ = self.0.call(
            "push",
            json!({ "plot": plot, "received_at_ms": received_at.timestamp_millis() }),
        );
    }

    fn run(&mut self, now: DateTime<Utc>, force: bool) -> Result<Vec<Observation>, String> {
        let tracks = self.0.call(
            "run",
            json!({ "now_ms": now.timestamp_millis(), "force": force }),
        )?;
        match tracks {
            Value::Null => Ok(Vec::new()),
            v => {
                serde_json::from_value(v).map_err(|e| format!("a track is not an observation: {e}"))
            }
        }
    }
}

impl PluginScorer for Session {
    fn score(
        &mut self,
        report: &Observation,
        candidates: &[ScoreCandidate],
    ) -> Result<Vec<Score>, String> {
        let scores = self.0.call(
            "score",
            json!({ "report": report, "candidates": candidates }),
        )?;
        serde_json::from_value(scores).map_err(|e| format!("scores: {e}"))
    }
}
