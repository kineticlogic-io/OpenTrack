//! Transports: getting frames off the wire.
//!
//! Each transport runs until its connection ends or fails; the worker's
//! supervisor restarts it with backoff. Framing is part of the transport's
//! configuration. Secrets are never stored: any string setting may reference
//! an environment variable as `${env:NAME}`, resolved when the source starts.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use bytes::BytesMut;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::frame::{Frame, Framer, Framing};
use crate::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransportConfig {
    /// Connect out to a TCP server.
    TcpClient {
        host: String,
        port: u16,
        #[serde(default = "lines")]
        framing: Framing,
        /// Sent once after connecting (e.g. a login or subscribe line).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        send_on_connect: Option<String>,
    },
    /// Listen for TCP clients; every connection feeds the same source.
    TcpServer {
        bind: String,
        #[serde(default = "lines")]
        framing: Framing,
    },
    /// One frame per datagram.
    Udp {
        bind: String,
        /// Join this IPv4 multicast group.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        multicast_group: Option<Ipv4Addr>,
        /// Interface address for the multicast join (default any).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        multicast_interface: Option<Ipv4Addr>,
    },
    /// Poll an HTTP endpoint; one frame per response body.
    HttpPoll {
        url: String,
        #[serde(default = "five")]
        interval_secs: f64,
        #[serde(default = "get")]
        method: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default = "twenty")]
        timeout_secs: f64,
    },
    /// WebSocket client; one frame per message.
    Websocket {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        /// Sent after connecting: a JSON value (sent as text) or a string.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subscribe: Option<Value>,
        /// If a JSON message has a non-null value here, the server reported
        /// an error in-band: reconnect.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_path: Option<Path>,
        #[serde(default = "twenty")]
        ping_secs: f64,
    },
    /// MQTT 3.1.1 client; one frame per PUBLISH. The message's topic is
    /// available to mappings as `_frame.topic` (and `_frame.topic_levels`,
    /// the topic split on `/`).
    Mqtt {
        /// `mqtt://host[:1883]` or `mqtts://host[:8883]` (TLS).
        url: String,
        /// Topic filters; `+` and `#` wildcards allowed.
        topics: Vec<String>,
        /// Subscription QoS: 0 (at most once) or 1 (at least once).
        #[serde(default)]
        qos: u8,
        /// Defaults to a unique id per connection. Required for a persistent
        /// session (`clean_session: false`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
        #[serde(default = "yes")]
        clean_session: bool,
        #[serde(default = "thirty")]
        keepalive_secs: f64,
        /// PEM file of the CA to trust for `mqtts://` (default: system roots).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ca_file: Option<String>,
    },
    /// Replay recorded data: a file, or every file in a directory (by name,
    /// optionally only those with `extension`), split by `framing`. The file
    /// name is available to mappings as `_frame.file`.
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extension: Option<String>,
        #[serde(default = "message")]
        framing: Framing,
        /// Pace the replay (frames per second); unset: as fast as the pipeline takes them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frames_per_second: Option<f64>,
        /// Start again from the first file after the last.
        #[serde(default)]
        repeat: bool,
    },
}

fn message() -> Framing {
    Framing::Message
}

fn lines() -> Framing {
    Framing::Lines {
        max_len: crate::frame::DEFAULT_MAX_FRAME,
    }
}
fn five() -> f64 {
    5.0
}
fn twenty() -> f64 {
    20.0
}
fn get() -> String {
    "GET".into()
}
fn thirty() -> f64 {
    30.0
}
fn yes() -> bool {
    true
}

/// Largest MQTT message accepted, matching the frame ceiling.
const MQTT_MAX_PACKET: usize = crate::frame::DEFAULT_MAX_FRAME;

impl TransportConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            TransportConfig::TcpClient { .. } => "tcp_client",
            TransportConfig::TcpServer { .. } => "tcp_server",
            TransportConfig::Udp { .. } => "udp",
            TransportConfig::HttpPoll { .. } => "http_poll",
            TransportConfig::Websocket { .. } => "websocket",
            TransportConfig::Mqtt { .. } => "mqtt",
            TransportConfig::File { .. } => "file",
        }
    }

    /// Settings that cannot be caught by the type alone.
    pub fn check(&self) -> Result<(), String> {
        if let TransportConfig::File {
            path,
            frames_per_second,
            ..
        } = self
        {
            if path.trim().is_empty() {
                return Err("a file transport needs a path".into());
            }
            if frames_per_second.is_some_and(|f| !(f.is_finite() && f > 0.0)) {
                return Err("frames_per_second must be a positive number".into());
            }
        }
        if let TransportConfig::Mqtt {
            url,
            topics,
            qos,
            client_id,
            clean_session,
            ..
        } = self
        {
            if !(url.starts_with("mqtt://")
                || url.starts_with("mqtts://")
                || url.starts_with("${env:"))
            {
                return Err(format!(
                    "MQTT url must start with mqtt:// or mqtts://, got {url:?}"
                ));
            }
            if topics.is_empty() || topics.iter().any(|t| t.trim().is_empty()) {
                return Err("MQTT needs at least one topic filter".into());
            }
            if *qos > 1 {
                return Err(format!("MQTT qos must be 0 or 1, got {qos}"));
            }
            if !clean_session && client_id.as_deref().is_none_or(|c| c.trim().is_empty()) {
                return Err(
                    "a persistent MQTT session (clean_session false) needs a client_id".into(),
                );
            }
        }
        Ok(())
    }

    /// Where it connects or listens, with secrets left unresolved.
    pub fn endpoint(&self) -> String {
        match self {
            TransportConfig::TcpClient { host, port, .. } => format!("{host}:{port}"),
            TransportConfig::TcpServer { bind, .. } | TransportConfig::Udp { bind, .. } => {
                bind.clone()
            }
            TransportConfig::HttpPoll { url, .. }
            | TransportConfig::Websocket { url, .. }
            | TransportConfig::Mqtt { url, .. } => url.clone(),
            TransportConfig::File { path, .. } => path.clone(),
        }
    }
}

/// Live connection state, shared with the control plane via the worker.
#[derive(Debug, Default, Clone, Serialize)]
pub struct LinkStatus {
    pub connected: bool,
    pub connects: u64,
    pub errors: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_frame_at: Option<chrono::DateTime<Utc>>,
}

pub type SharedStatus = Arc<Mutex<LinkStatus>>;

fn set(status: &SharedStatus, f: impl FnOnce(&mut LinkStatus)) {
    if let Ok(mut s) = status.lock() {
        f(&mut s);
    }
}

/// Replace `${env:NAME}` references with environment values.
pub fn resolve_env(s: &str) -> anyhow::Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${env:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 6..];
        let end = after.find('}').context("unterminated ${env:...}")?;
        let name = &after[..end];
        let value = std::env::var(name)
            .with_context(|| format!("environment variable {name} is not set"))?;
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn resolve_value(v: &Value) -> anyhow::Result<Value> {
    Ok(match v {
        Value::String(s) => Value::String(resolve_env(s)?),
        Value::Array(items) => {
            Value::Array(items.iter().map(resolve_value).collect::<Result<_, _>>()?)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), resolve_value(v)?)))
                .collect::<anyhow::Result<_>>()?,
        ),
        other => other.clone(),
    })
}

async fn emit(tx: &mpsc::Sender<Frame>, status: &SharedStatus, frame: Frame) -> anyhow::Result<()> {
    set(status, |s| s.last_frame_at = Some(frame.received_at));
    tx.send(frame)
        .await
        .map_err(|_| anyhow::anyhow!("pipeline closed"))
}

/// Run the transport until the connection ends. `Ok` means a clean end
/// (peer closed); either way the supervisor decides when to reconnect.
pub async fn run(
    config: &TransportConfig,
    tx: mpsc::Sender<Frame>,
    status: SharedStatus,
) -> anyhow::Result<()> {
    match config {
        TransportConfig::TcpClient {
            host,
            port,
            framing,
            send_on_connect,
        } => {
            let host = resolve_env(host)?;
            let mut stream = tokio::net::TcpStream::connect((host.as_str(), *port))
                .await
                .with_context(|| format!("connecting to {host}:{port}"))?;
            connected(&status);
            if let Some(line) = send_on_connect {
                use tokio::io::AsyncWriteExt;
                stream.write_all(resolve_env(line)?.as_bytes()).await?;
            }
            let origin = format!("{host}:{port}");
            read_stream(stream, framing, &tx, &status, &origin).await
        }
        TransportConfig::TcpServer { bind, framing } => {
            let listener = tokio::net::TcpListener::bind(resolve_env(bind)?.as_str())
                .await
                .with_context(|| format!("binding {bind}"))?;
            connected(&status);
            loop {
                let (stream, peer) = listener.accept().await?;
                let (tx, status, framing) = (tx.clone(), status.clone(), framing.clone());
                tokio::spawn(async move {
                    let origin = peer.to_string();
                    if let Err(e) = read_stream(stream, &framing, &tx, &status, &origin).await {
                        tracing::debug!(%peer, error = %e, "tcp client ended");
                    }
                });
            }
        }
        TransportConfig::Udp {
            bind,
            multicast_group,
            multicast_interface,
        } => {
            let addr: SocketAddr = resolve_env(bind)?
                .parse()
                .context("udp bind must be ip:port")?;
            let socket = udp_socket(addr, *multicast_group, *multicast_interface)?;
            connected(&status);
            let mut buf = vec![0u8; 65_536];
            loop {
                let (n, peer) = socket.recv_from(&mut buf).await?;
                let mut frame = Frame::new(buf[..n].to_vec());
                frame.origin = Some(peer.to_string());
                emit(&tx, &status, frame).await?;
            }
        }
        TransportConfig::HttpPoll {
            url,
            interval_secs,
            method,
            headers,
            body,
            timeout_secs,
        } => {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs_f64(*timeout_secs))
                .user_agent(concat!("opentrack/", env!("CARGO_PKG_VERSION")))
                .build()?;
            let url = resolve_env(url)?;
            let method: reqwest::Method = method.parse().context("bad HTTP method")?;
            let mut hdrs = reqwest::header::HeaderMap::new();
            for (k, v) in headers {
                hdrs.insert(
                    reqwest::header::HeaderName::from_bytes(k.as_bytes())?,
                    reqwest::header::HeaderValue::from_str(&resolve_env(v)?)?,
                );
            }
            let body = body.as_deref().map(resolve_env).transpose()?;
            let interval = Duration::from_secs_f64(interval_secs.max(0.1));
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Extra wait while the server is rate limiting or overloaded.
            let mut backoff = Duration::ZERO;
            loop {
                ticker.tick().await;
                if !backoff.is_zero() {
                    tokio::time::sleep(backoff).await;
                }
                let mut req = client.request(method.clone(), &url).headers(hdrs.clone());
                if let Some(b) = &body {
                    req = req.body(b.clone());
                }
                let res = async {
                    let resp = req.send().await?;
                    let code = resp.status();
                    if code == reqwest::StatusCode::TOO_MANY_REQUESTS
                        || code == reqwest::StatusCode::SERVICE_UNAVAILABLE
                    {
                        let retry_after = resp
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.trim().parse::<f64>().ok());
                        return Ok(Err((code, retry_after)));
                    }
                    let resp = resp.error_for_status()?;
                    anyhow::Ok(Ok(resp.bytes().await?))
                }
                .await;
                match res {
                    Ok(Ok(bytes)) => {
                        backoff = Duration::ZERO;
                        connected(&status);
                        let mut frame = Frame::new(bytes);
                        frame.origin = Some(url.clone());
                        emit(&tx, &status, frame).await?;
                    }
                    Ok(Err((code, retry_after))) => {
                        // Rate limited: honour Retry-After, else back off
                        // exponentially (to five minutes) so the limit can clear.
                        backoff = match retry_after {
                            Some(secs) => Duration::from_secs_f64(secs.clamp(0.0, 3600.0)),
                            None => (backoff * 2).max(interval).min(Duration::from_secs(300)),
                        };
                        tracing::warn!(%url, %code, ?backoff, "poll rate limited; backing off");
                        set(&status, |s| {
                            s.connected = false;
                            s.errors += 1;
                            s.last_error =
                                Some(format!("{code}; next poll in {}s", backoff.as_secs()));
                        });
                    }
                    Err(e) => {
                        // A failed poll is not fatal; count it and poll again.
                        tracing::warn!(%url, error = %e, "poll failed");
                        set(&status, |s| {
                            s.connected = false;
                            s.errors += 1;
                            s.last_error = Some(e.to_string());
                        });
                    }
                }
            }
        }
        TransportConfig::Websocket {
            url,
            headers,
            subscribe,
            error_path,
            ping_secs,
        } => {
            let url = resolve_env(url)?;
            let mut request = url.as_str().into_client_request()?;
            for (k, v) in headers {
                request.headers_mut().insert(
                    tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(k.as_bytes())?,
                    resolve_env(v)?.parse()?,
                );
            }
            let (mut ws, _) = tokio_tungstenite::connect_async(request)
                .await
                .with_context(|| format!("connecting to {}", redact(&url)))?;
            if let Some(sub) = subscribe {
                let text = match resolve_value(sub)? {
                    Value::String(s) => s,
                    other => other.to_string(),
                };
                ws.send(Message::text(text)).await?;
            }
            connected(&status);
            let mut ping = tokio::time::interval(Duration::from_secs_f64(ping_secs.max(1.0)));
            ping.tick().await;
            loop {
                tokio::select! {
                    _ = ping.tick() => ws.send(Message::Ping(Default::default())).await?,
                    msg = ws.next() => {
                        let Some(msg) = msg else { return Ok(()) };
                        let bytes = match msg? {
                            Message::Text(t) => bytes::Bytes::from(t.as_str().to_owned()),
                            Message::Binary(b) => b,
                            Message::Close(_) => return Ok(()),
                            _ => continue,
                        };
                        if let Some(path) = error_path
                            && let Ok(v) = serde_json::from_slice::<Value>(&bytes)
                            && path.get(&v).is_some_and(|e| !e.is_null())
                        {
                            bail!("server error: {}", path.get(&v).map(Value::to_string).unwrap_or_default());
                        }
                        let mut frame = Frame::new(bytes);
                        frame.origin = Some(redact(&url));
                        emit(&tx, &status, frame).await?;
                    }
                }
            }
        }
        TransportConfig::Mqtt { .. } => run_mqtt(config, &tx, &status).await,
        TransportConfig::File {
            path,
            extension,
            framing,
            frames_per_second,
            repeat,
        } => {
            let files = replay_files(&resolve_env(path)?, extension.as_deref())?;
            connected(&status);
            let pause = frames_per_second.map(|f| Duration::from_secs_f64(1.0 / f));
            loop {
                for file in &files {
                    let f = file.clone();
                    let bytes = tokio::task::spawn_blocking(move || std::fs::read(&f))
                        .await?
                        .with_context(|| format!("reading {}", file.display()))?;
                    let name = file
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let mut framer = Framer::new(framing.clone())?;
                    let mut buf = BytesMut::from(&bytes[..]);
                    loop {
                        match framer.next(&mut buf) {
                            Ok(Some(bytes)) => {
                                let mut frame = Frame::new(bytes);
                                frame.origin = Some(file.display().to_string());
                                frame
                                    .meta
                                    .insert("file".into(), serde_json::Value::String(name.clone()));
                                emit(&tx, &status, frame).await?;
                                if let Some(p) = pause {
                                    tokio::time::sleep(p).await;
                                }
                            }
                            Ok(None) => break,
                            Err(e) => {
                                tracing::warn!(file = %file.display(), error = %e, "framing error; rest of file skipped");
                                set(&status, |s| {
                                    s.errors += 1;
                                    s.last_error = Some(format!("{}: {e}", file.display()));
                                });
                                break;
                            }
                        }
                    }
                    if !buf.is_empty() {
                        set(&status, |s| {
                            s.errors += 1;
                            s.last_error = Some(format!(
                                "{}: {} trailing bytes are not a whole frame",
                                file.display(),
                                buf.len()
                            ));
                        });
                    }
                }
                if !repeat {
                    // Done: stay "connected" so the supervisor does not replay it again.
                    std::future::pending::<()>().await;
                }
            }
        }
    }
}

/// Split `mqtt[s]://host[:port]` into (tls, host, port).
fn mqtt_endpoint(url: &str) -> anyhow::Result<(bool, String, u16)> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("mqtts://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("mqtt://") {
        (false, r)
    } else {
        bail!("MQTT url must start with mqtt:// or mqtts://");
    };
    let rest = rest.trim_end_matches('/');
    let default_port = if tls { 8883 } else { 1883 };
    // `[v6]:port`, `[v6]`, `host:port` or `host`.
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (h, after) = v6.split_once(']').context("MQTT url: unclosed [")?;
        (h, after.strip_prefix(':'))
    } else {
        match rest.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (rest, None),
        }
    };
    let port = match port {
        Some(p) => p.parse().with_context(|| format!("MQTT port {p:?}"))?,
        None => default_port,
    };
    if host.is_empty() {
        bail!("MQTT url has no host");
    }
    Ok((tls, host.to_owned(), port))
}

async fn run_mqtt(
    config: &TransportConfig,
    tx: &mpsc::Sender<Frame>,
    status: &SharedStatus,
) -> anyhow::Result<()> {
    use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, SubscribeReasonCode, Transport};
    let TransportConfig::Mqtt {
        url,
        topics,
        qos,
        client_id,
        username,
        password,
        clean_session,
        keepalive_secs,
        ca_file,
    } = config
    else {
        unreachable!("run_mqtt called with another transport");
    };
    config.check().map_err(anyhow::Error::msg)?;
    let url = resolve_env(url)?;
    let (tls, host, port) = mqtt_endpoint(&url)?;
    let id = match client_id {
        Some(c) if !c.trim().is_empty() => resolve_env(c)?,
        _ => format!(
            "opentrack-{:x}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default() ^ i64::from(std::process::id())
        ),
    };
    let mut opts = MqttOptions::new(id, host.clone(), port);
    opts.set_keep_alive(Duration::from_secs_f64(keepalive_secs.max(5.0)))
        .set_clean_session(*clean_session)
        .set_max_packet_size(MQTT_MAX_PACKET, 64 * 1024);
    if let Some(user) = username {
        let pass = password.as_deref().map(resolve_env).transpose()?;
        opts.set_credentials(resolve_env(user)?, pass.unwrap_or_default());
    }
    if tls {
        opts.set_transport(match ca_file {
            Some(path) => {
                let path = resolve_env(path)?;
                let ca = std::fs::read(&path).with_context(|| format!("reading CA file {path}"))?;
                Transport::tls(ca, None, None)
            }
            None => Transport::tls_with_default_config(),
        });
    }
    let qos = if *qos == 1 {
        QoS::AtLeastOnce
    } else {
        QoS::AtMostOnce
    };
    let origin = format!("{}{host}:{port}", if tls { "mqtts://" } else { "mqtt://" });
    let (client, mut events) = AsyncClient::new(opts, 64);
    let filters: Vec<String> = topics.iter().map(|t| t.trim().to_owned()).collect();
    loop {
        // Any connection error ends the run; the supervisor reconnects with
        // backoff (and re-subscribes, since each run starts afresh).
        let event = events
            .poll()
            .await
            .with_context(|| format!("MQTT {origin}"))?;
        match event {
            Event::Incoming(Packet::ConnAck(_)) => {
                for f in &filters {
                    client.subscribe(f.clone(), qos).await?;
                }
            }
            Event::Incoming(Packet::SubAck(ack)) => {
                if ack
                    .return_codes
                    .iter()
                    .any(|c| matches!(c, SubscribeReasonCode::Failure))
                {
                    bail!(
                        "MQTT broker refused a subscription ({})",
                        filters.join(", ")
                    );
                }
                connected(status);
            }
            Event::Incoming(Packet::Publish(p)) => {
                let mut meta = serde_json::Map::new();
                meta.insert(
                    "topic_levels".into(),
                    Value::Array(
                        p.topic
                            .split('/')
                            .map(|l| Value::String(l.to_owned()))
                            .collect(),
                    ),
                );
                meta.insert("topic".into(), Value::String(p.topic));
                if p.retain {
                    meta.insert("retained".into(), Value::Bool(true));
                }
                let mut frame = Frame::new(p.payload).with_meta(meta);
                frame.origin = Some(origin.clone());
                emit(tx, status, frame).await?;
            }
            Event::Incoming(Packet::Disconnect) => return Ok(()),
            _ => {}
        }
    }
}

fn connected(status: &SharedStatus) {
    set(status, |s| {
        if !s.connected {
            s.connects += 1;
        }
        s.connected = true;
    });
}

/// Hide query strings, which often carry keys, in logs.
fn redact(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?…"),
        None => url.to_owned(),
    }
}

/// The files a file transport replays: `path` itself, or the files in it
/// (sorted by name, optionally by extension).
fn replay_files(path: &str, extension: Option<&str>) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let p = std::path::Path::new(path);
    let mut files = if p.is_dir() {
        std::fs::read_dir(p)
            .with_context(|| format!("listing {path}"))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|f| f.is_file())
            .filter(|f| {
                extension.is_none_or(|x| {
                    f.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case(x.trim_start_matches('.')))
                })
            })
            .collect::<Vec<_>>()
    } else if p.is_file() {
        vec![p.to_path_buf()]
    } else {
        anyhow::bail!("{path} is not a file or directory");
    };
    files.sort();
    if files.is_empty() {
        anyhow::bail!("no files to replay in {path}");
    }
    Ok(files)
}

async fn read_stream(
    mut stream: tokio::net::TcpStream,
    framing: &Framing,
    tx: &mpsc::Sender<Frame>,
    status: &SharedStatus,
    origin: &str,
) -> anyhow::Result<()> {
    let mut framer = Framer::new(framing.clone())?;
    let mut buf = BytesMut::with_capacity(64 * 1024);
    loop {
        let n = stream.read_buf(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        loop {
            match framer.next(&mut buf) {
                Ok(Some(bytes)) => {
                    let mut frame = Frame::new(bytes);
                    frame.origin = Some(origin.to_owned());
                    emit(tx, status, frame).await?;
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(%origin, error = %e, "framing error; frame skipped");
                    set(status, |s| {
                        s.errors += 1;
                        s.last_error = Some(e.to_string());
                    });
                }
            }
        }
    }
}

fn udp_socket(
    addr: SocketAddr,
    group: Option<Ipv4Addr>,
    interface: Option<Ipv4Addr>,
) -> anyhow::Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_broadcast(true)?;
    socket.set_recv_buffer_size(4 * 1024 * 1024).ok();
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    if let Some(g) = group {
        socket.join_multicast_v4(&g, &interface.unwrap_or(Ipv4Addr::UNSPECIFIED))?;
    }
    Ok(tokio::net::UdpSocket::from_std(socket.into())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn env_references_resolve_and_fail_loudly() {
        // set_var is unsafe in edition 2024, so use PATH, which is always set.
        let path = std::env::var("PATH").unwrap();
        assert_eq!(resolve_env("x${env:PATH}y").unwrap(), format!("x{path}y"));
        assert!(resolve_env("${env:OT_SURELY_UNSET_VAR}").is_err());
        assert!(resolve_env("${env:PATH").is_err());
        assert_eq!(resolve_env("plain").unwrap(), "plain");
    }

    #[test]
    fn configs_parse_with_defaults() {
        let t: TransportConfig = serde_json::from_value(serde_json::json!({
            "type": "http_poll", "url": "https://api.example/v2/mil"
        }))
        .unwrap();
        assert!(
            matches!(t, TransportConfig::HttpPoll { interval_secs, .. } if interval_secs == 5.0)
        );
        let t: TransportConfig = serde_json::from_value(serde_json::json!({
            "type": "tcp_server", "bind": "0.0.0.0:8087", "framing": { "type": "end_tag", "tag": "</event>" }
        }))
        .unwrap();
        assert_eq!(t.kind(), "tcp_server");
        assert!(
            serde_json::from_value::<TransportConfig>(
                serde_json::json!({"type": "carrier_pigeon"})
            )
            .is_err()
        );
        assert_eq!(redact("wss://x/y?key=secret"), "wss://x/y?…");
    }

    #[tokio::test]
    async fn tcp_server_frames_cot_from_a_client() {
        // Bind to an ephemeral port first to learn a free one.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);
        let config = TransportConfig::TcpServer {
            bind: addr.to_string(),
            framing: Framing::EndTag {
                tag: "</event>".into(),
                max_len: 1 << 20,
            },
        };
        let (tx, mut rx) = mpsc::channel(8);
        let status = SharedStatus::default();
        let task = tokio::spawn(async move { run(&config, tx, status).await });
        let mut client = loop {
            match tokio::net::TcpStream::connect(addr).await {
                Ok(c) => break c,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        client
            .write_all(b"<event uid=\"a\"></event><event uid=\"b\"></ev")
            .await
            .unwrap();
        client.write_all(b"ent>").await.unwrap();
        let a = rx.recv().await.unwrap();
        let b = rx.recv().await.unwrap();
        assert_eq!(&a.bytes[..], b"<event uid=\"a\"></event>");
        assert_eq!(&b.bytes[..], b"<event uid=\"b\"></event>");
        task.abort();
    }

    #[tokio::test]
    async fn udp_datagrams_are_frames() {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);
        let config = TransportConfig::Udp {
            bind: addr.to_string(),
            multicast_group: None,
            multicast_interface: None,
        };
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(async move { run(&config, tx, SharedStatus::default()).await });
        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let frame = loop {
            sender.send_to(b"{\"id\":1}", addr).await.unwrap();
            if let Ok(Some(f)) = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                break f;
            }
        };
        assert_eq!(&frame.bytes[..], b"{\"id\":1}");
        task.abort();
    }

    #[test]
    fn mqtt_config_urls_and_checks() {
        let t: TransportConfig = serde_json::from_value(serde_json::json!({
            "type": "mqtt", "url": "mqtt://broker.local", "topics": ["ais/+/pos"]
        }))
        .unwrap();
        assert_eq!(t.kind(), "mqtt");
        assert!(matches!(
            &t,
            TransportConfig::Mqtt { qos: 0, clean_session: true, keepalive_secs, .. } if *keepalive_secs == 30.0
        ));
        assert_eq!(t.check(), Ok(()));

        assert_eq!(
            mqtt_endpoint("mqtt://broker.local").unwrap(),
            (false, "broker.local".into(), 1883)
        );
        assert_eq!(
            mqtt_endpoint("mqtts://broker.local:9883/").unwrap(),
            (true, "broker.local".into(), 9883)
        );
        assert_eq!(
            mqtt_endpoint("mqtts://[::1]").unwrap(),
            (true, "::1".into(), 8883)
        );
        assert_eq!(
            mqtt_endpoint("mqtt://[::1]:1884").unwrap(),
            (false, "::1".into(), 1884)
        );
        assert!(mqtt_endpoint("tcp://x").is_err());
        assert!(mqtt_endpoint("mqtt://x:notaport").is_err());
        assert!(mqtt_endpoint("mqtt://:1883").is_err());

        let bad = |patch: serde_json::Value| {
            let mut v = serde_json::json!({
                "type": "mqtt", "url": "mqtt://b", "topics": ["t"]
            });
            v.as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            serde_json::from_value::<TransportConfig>(v)
                .unwrap()
                .check()
                .unwrap_err()
        };
        assert!(bad(serde_json::json!({"topics": []})).contains("topic"));
        assert!(bad(serde_json::json!({"qos": 2})).contains("qos"));
        assert!(bad(serde_json::json!({"clean_session": false})).contains("client_id"));
        assert!(bad(serde_json::json!({"url": "http://b"})).contains("mqtt://"));
    }

    /// Against a real broker: set `OT_TEST_MQTT_URL` (e.g. `mqtt://127.0.0.1:11883`).
    #[tokio::test]
    async fn mqtt_messages_are_frames_with_their_topic() {
        let Ok(url) = std::env::var("OT_TEST_MQTT_URL") else {
            eprintln!("skipped: OT_TEST_MQTT_URL not set");
            return;
        };
        let run_id = format!("ottest{}", Utc::now().timestamp_micros());
        let config = TransportConfig::Mqtt {
            url: url.clone(),
            topics: vec![format!("{run_id}/+/pos")],
            qos: 1,
            client_id: None,
            username: None,
            password: None,
            clean_session: true,
            keepalive_secs: 30.0,
            ca_file: None,
        };
        let (tx, mut rx) = mpsc::channel(8);
        let status = SharedStatus::default();
        let task_status = status.clone();
        let task = tokio::spawn(async move { run(&config, tx, task_status).await });
        for _ in 0..100 {
            if status.lock().unwrap().connected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(status.lock().unwrap().connected, "never subscribed");

        let (_, host, port) = mqtt_endpoint(&url).unwrap();
        let (client, mut events) = rumqttc::AsyncClient::new(
            rumqttc::MqttOptions::new(format!("{run_id}-pub"), host, port),
            16,
        );
        let publisher = tokio::spawn(async move { while events.poll().await.is_ok() {} });
        client
            .publish(
                format!("{run_id}/other/vel"),
                rumqttc::QoS::AtLeastOnce,
                false,
                "no",
            )
            .await
            .unwrap();
        client
            .publish(
                format!("{run_id}/366123456/pos"),
                rumqttc::QoS::AtLeastOnce,
                false,
                r#"{"lat":32.7}"#,
            )
            .await
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("no frame")
            .unwrap();
        assert_eq!(&frame.bytes[..], br#"{"lat":32.7}"#);
        assert_eq!(frame.meta["topic"], format!("{run_id}/366123456/pos"));
        assert_eq!(frame.meta["topic_levels"][1], "366123456");
        // The non-matching topic never arrived.
        assert!(rx.try_recv().is_err());
        task.abort();
        publisher.abort();
    }
}
