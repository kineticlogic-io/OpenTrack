//! NATS output: publishes system tracks to a JetStream stream.
//!
//! Tracks go to `<prefix>.<track_id>` (by default `tracks.tms-OTK000000123`)
//! in a stream that keeps one message per subject, so the stream always holds
//! the current picture: one `upsert` per live track, plus recent `delete`s
//! until they age out. The stream is created if missing and never modified if
//! present; an operator-managed stream is left exactly as configured.
//!
//! Every publish waits for the JetStream acknowledgement, so the writer only
//! acknowledges its outbox once the server has stored the message.

/// The NATS client, for roles that publish outside the tracks stream.
pub use async_nats;
use std::path::PathBuf;
use std::time::Duration;

use async_nats::jetstream::{self, stream};
use async_nats::{ConnectOptions, HeaderMap};

mod creds;
use bytes::Bytes;
use serde::Serialize;

/// How long a publish waits for its JetStream acknowledgement.
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// Window within which JetStream drops a re-sent message with the same id.
const DUPLICATE_WINDOW: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub struct NatsSettings {
    /// Server URL(s), comma separated, e.g. `nats://127.0.0.1:4222`.
    pub url: String,
    /// Client name shown in the server's connection list.
    pub name: String,
    /// Credentials file (JWT + NKey), if the server uses decentralised auth.
    pub creds_file: Option<PathBuf>,
    pub token: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    /// TLS to the server: trust this CA (PEM) and require TLS.
    pub tls_ca: Option<PathBuf>,
    /// A client certificate and key (PEM) for mutual TLS.
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    /// JetStream stream holding system tracks.
    pub stream: String,
    /// Subject prefix for system tracks; the stream captures `<prefix>.>`.
    pub tracks_subject: String,
    /// How long a message stays in the stream without being replaced.
    pub max_age: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum NatsError {
    #[error("NATS connect: {0}")]
    Connect(String),
    #[error("NATS stream {stream}: {message}")]
    Stream { stream: String, message: String },
    #[error("NATS publish to {subject} failed: {message}")]
    Publish {
        subject: String,
        message: String,
        transient: bool,
    },
}

impl NatsError {
    /// Whether retrying could help. A message the server refuses outright
    /// (too large) is permanent; connectivity, timeouts and a missing stream
    /// are not.
    pub fn is_transient(&self) -> bool {
        match self {
            NatsError::Publish { transient, .. } => *transient,
            NatsError::Connect(_) | NatsError::Stream { .. } => true,
        }
    }
}

/// One message to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub subject: String,
    /// Unique per logical update and stable across re-delivery, so a message
    /// re-sent after a crash is dropped by JetStream as a duplicate.
    pub msg_id: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Bytes,
}

/// Where the writer sends messages. The NATS implementation is [`Nats`];
/// tests substitute their own.
pub trait TrackSink: Send + Sync {
    /// Make sure the destination exists (for NATS, the stream). Called before
    /// the first publish and again after a publish fails.
    fn prepare(&self) -> impl Future<Output = Result<(), NatsError>> + Send {
        async { Ok(()) }
    }

    fn publish(&self, msg: Outgoing) -> impl Future<Output = Result<(), NatsError>> + Send;

    /// Publish a perishable message on core NATS (not stored in a stream),
    /// e.g. a line of bearing no track took.
    fn publish_live(
        &self,
        _subject: String,
        _body: Bytes,
    ) -> impl Future<Output = Result<(), NatsError>> + Send {
        async { Ok(()) }
    }
}

/// Connection and stream state, for the status endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct NatsStatus {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    pub stream: String,
    pub tracks_subject: String,
    /// Messages in the stream (live tracks plus recent deletes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_messages: Option<u64>,
    /// Bytes the stream stores.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_error: Option<String>,
}

#[derive(Clone)]
pub struct Nats {
    client: async_nats::Client,
    js: jetstream::Context,
    settings: NatsSettings,
}

impl Nats {
    /// Connect without waiting for the server: the client keeps retrying in
    /// the background, and publishes fail (transiently) until it is up.
    pub async fn connect(settings: NatsSettings) -> Result<Self, NatsError> {
        // `.creds` sign-in signs the server's nonce in the FIPS module.
        let base = match &settings.creds_file {
            Some(path) => {
                let bad =
                    |e: String| NatsError::Connect(format!("credentials {}: {e}", path.display()));
                let text = tokio::fs::read_to_string(path)
                    .await
                    .map_err(|e| bad(e.to_string()))?;
                let creds = std::sync::Arc::new(creds::Creds::parse(&text).map_err(bad)?);
                ConnectOptions::with_auth_callback(move |nonce| {
                    let creds = creds.clone();
                    async move {
                        let mut auth = async_nats::Auth::new();
                        auth.jwt = Some(creds.jwt.clone());
                        auth.signature = Some(creds.sign(&nonce));
                        Ok(auth)
                    }
                })
            }
            None => ConnectOptions::new(),
        };
        let mut opts = base
            .name(&settings.name)
            .retry_on_initial_connect()
            .connection_timeout(Duration::from_secs(5))
            .max_reconnects(None);
        if let Some(token) = &settings.token {
            opts = opts.token(token.clone());
        }
        if let (Some(user), Some(pass)) = (&settings.user, &settings.password) {
            opts = opts.user_and_password(user.clone(), pass.clone());
        }
        if let Some(ca) = &settings.tls_ca {
            opts = opts.add_root_certificates(ca.clone()).require_tls(true);
        }
        match (&settings.tls_cert, &settings.tls_key) {
            (Some(cert), Some(key)) => {
                opts = opts
                    .add_client_certificate(cert.clone(), key.clone())
                    .require_tls(true);
            }
            (None, None) => {}
            _ => {
                return Err(NatsError::Connect(
                    "a client certificate needs its key, and the other way round".into(),
                ));
            }
        }
        let client = opts
            .connect(settings.url.as_str())
            .await
            .map_err(|e| NatsError::Connect(e.to_string()))?;
        let js = jetstream::new(client.clone());
        Ok(Self {
            client,
            js,
            settings,
        })
    }

    pub fn settings(&self) -> &NatsSettings {
        &self.settings
    }

    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }

    /// The stream configuration OpenTrack creates when none exists.
    pub fn stream_config(&self) -> stream::Config {
        stream::Config {
            name: self.settings.stream.clone(),
            description: Some("OpenTrack system tracks: latest message per track".into()),
            subjects: vec![format!("{}.>", self.settings.tracks_subject)],
            max_messages_per_subject: 1,
            max_age: self.settings.max_age,
            storage: stream::StorageType::File,
            duplicate_window: DUPLICATE_WINDOW,
            ..Default::default()
        }
    }

    fn stream_error(&self, message: impl ToString) -> NatsError {
        NatsError::Stream {
            stream: self.settings.stream.clone(),
            message: message.to_string(),
        }
    }

    /// Create the tracks stream if it does not exist. An existing stream is
    /// used as is; a warning is logged if it does not fit.
    pub async fn ensure_stream(&self) -> Result<(), NatsError> {
        let mut s = self
            .js
            .get_or_create_stream(self.stream_config())
            .await
            .map_err(|e| self.stream_error(e))?;
        let info = s.info().await.map_err(|e| self.stream_error(e))?;
        let wanted = format!("{}.>", self.settings.tracks_subject);
        if !info
            .config
            .subjects
            .iter()
            .any(|s| s == &wanted || s == ">")
        {
            tracing::warn!(stream = %self.settings.stream, subjects = ?info.config.subjects,
                %wanted, "existing stream does not capture the tracks subject; publishes will fail");
        }
        if info.config.max_messages_per_subject != 1 {
            tracing::warn!(stream = %self.settings.stream,
                max_per_subject = info.config.max_messages_per_subject,
                "existing stream keeps more than one message per track");
        }
        Ok(())
    }

    pub async fn status(&self) -> NatsStatus {
        let connected = self.client.connection_state() == async_nats::connection::State::Connected;
        let info = connected.then(|| self.client.server_info());
        let (stream_state, stream_error) = if connected {
            match self.js.get_stream(&self.settings.stream).await {
                Ok(mut s) => match s.info().await {
                    Ok(i) => (Some((i.state.messages, i.state.bytes)), None),
                    Err(e) => (None, Some(e.to_string())),
                },
                Err(e) => {
                    let missing = matches!(e.kind(), jetstream::context::GetStreamErrorKind::JetStream(ref je)
                        if je.error_code() == jetstream::ErrorCode::STREAM_NOT_FOUND);
                    let msg = if missing {
                        format!(
                            "stream {} does not exist yet; the writer creates it on start",
                            self.settings.stream
                        )
                    } else {
                        e.to_string()
                    };
                    (None, Some(msg))
                }
            }
        } else {
            (None, None)
        };
        NatsStatus {
            connected,
            server_name: info.as_ref().map(|i| i.server_name.clone()),
            server_version: info.as_ref().map(|i| i.version.clone()),
            stream: self.settings.stream.clone(),
            tracks_subject: self.settings.tracks_subject.clone(),
            stream_messages: stream_state.map(|(m, _)| m),
            stream_bytes: stream_state.map(|(_, b)| b),
            stream_error,
        }
    }

    /// The latest stored message on `subject`, as (headers, body).
    pub async fn last_message(
        &self,
        subject: &str,
    ) -> Result<Option<(Vec<(String, String)>, Bytes)>, NatsError> {
        let s = self
            .js
            .get_stream(&self.settings.stream)
            .await
            .map_err(|e| self.stream_error(e))?;
        match s.get_last_raw_message_by_subject(subject).await {
            Ok(m) => {
                let headers = m
                    .headers
                    .iter()
                    .flat_map(|(k, vs)| vs.iter().map(move |v| (k.to_string(), v.to_string())))
                    .collect();
                Ok(Some((headers, m.payload)))
            }
            Err(e) if matches!(e.kind(), stream::LastRawMessageErrorKind::NoMessageFound) => {
                Ok(None)
            }
            Err(e) => Err(self.stream_error(e)),
        }
    }
}

impl TrackSink for Nats {
    async fn prepare(&self) -> Result<(), NatsError> {
        self.ensure_stream().await
    }

    async fn publish_live(&self, subject: String, body: Bytes) -> Result<(), NatsError> {
        self.client
            .publish(subject.clone(), body)
            .await
            .map_err(|e| NatsError::Publish {
                subject,
                message: e.to_string(),
                transient: true,
            })
    }

    async fn publish(&self, msg: Outgoing) -> Result<(), NatsError> {
        let subject = msg.subject;
        let mut headers = HeaderMap::new();
        headers.insert("Nats-Msg-Id", msg.msg_id.as_str());
        for (k, v) in &msg.headers {
            headers.insert(*k, v.as_str());
        }
        let classify = |e: jetstream::context::PublishError| {
            let permanent = matches!(
                e.kind(),
                jetstream::context::PublishErrorKind::MaxPayloadExceeded
            );
            NatsError::Publish {
                subject: subject.clone(),
                message: e.to_string(),
                transient: !permanent,
            }
        };
        let ack = self
            .js
            .publish_with_headers(subject.clone(), headers, msg.body)
            .await
            .map_err(classify)?;
        match tokio::time::timeout(ACK_TIMEOUT, ack).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(classify(e)),
            Err(_) => Err(NatsError::Publish {
                subject: subject.clone(),
                message: "timed out waiting for the stream to acknowledge".into(),
                transient: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Against a real server (set `OT_TEST_NATS_URL`, e.g.
    //! `nats://127.0.0.1:14222`), in a stream and subject unique to the run.
    use super::*;

    async fn nats() -> Option<Nats> {
        let url = std::env::var("OT_TEST_NATS_URL").ok()?;
        let run = format!(
            "{}{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros()
        );
        Some(
            Nats::connect(NatsSettings {
                url,
                name: "opentrack-test".into(),
                creds_file: None,
                token: None,
                user: None,
                password: None,
                tls_ca: None,
                tls_cert: None,
                tls_key: None,
                stream: format!("OT_TEST_{run}"),
                tracks_subject: format!("ottest{run}"),
                max_age: Duration::from_secs(300),
            })
            .await
            .unwrap(),
        )
    }

    #[tokio::test]
    async fn a_client_certificate_needs_its_key() {
        let r = Nats::connect(NatsSettings {
            url: "nats://127.0.0.1:9".into(),
            name: "opentrack-test".into(),
            creds_file: None,
            token: None,
            user: None,
            password: None,
            tls_ca: None,
            tls_cert: Some("client.pem".into()),
            tls_key: None,
            stream: "T".into(),
            tracks_subject: "t".into(),
            max_age: Duration::from_secs(300),
        })
        .await;
        assert!(matches!(r, Err(NatsError::Connect(m)) if m.contains("key")));
    }

    fn msg(n: &Nats, id: &str, msg_id: &str, body: &str) -> Outgoing {
        Outgoing {
            subject: format!("{}.{id}", n.settings().tracks_subject),
            msg_id: msg_id.into(),
            headers: vec![("OT-Op", "upsert".into())],
            body: Bytes::from(body.to_owned()),
        }
    }

    #[tokio::test]
    async fn keeps_the_latest_message_per_track_and_drops_duplicates() {
        let Some(n) = nats().await else {
            eprintln!("skipped: OT_TEST_NATS_URL not set");
            return;
        };
        n.ensure_stream().await.unwrap();
        // Idempotent: a second call finds the stream and leaves it alone.
        n.ensure_stream().await.unwrap();

        n.publish(msg(&n, "tms-A", "1", "a1")).await.unwrap();
        n.publish(msg(&n, "tms-A", "2", "a2")).await.unwrap();
        n.publish(msg(&n, "tms-B", "3", "b1")).await.unwrap();
        // Same id as an earlier message: stored once only.
        n.publish(msg(&n, "tms-A", "2", "a2-again")).await.unwrap();

        let subject = format!("{}.tms-A", n.settings().tracks_subject);
        let (headers, body) = n.last_message(&subject).await.unwrap().unwrap();
        assert_eq!(body.as_ref(), b"a2");
        assert!(headers.contains(&("OT-Op".into(), "upsert".into())));
        let none = format!("{}.tms-Z", n.settings().tracks_subject);
        assert!(n.last_message(&none).await.unwrap().is_none());

        let st = n.status().await;
        assert!(st.connected);
        assert_eq!(st.stream_messages, Some(2));

        n.js.delete_stream(&n.settings().stream).await.unwrap();
    }

    #[tokio::test]
    async fn publishing_without_a_stream_is_transient() {
        let Some(n) = nats().await else {
            eprintln!("skipped: OT_TEST_NATS_URL not set");
            return;
        };
        let e = n.publish(msg(&n, "tms-A", "1", "x")).await.unwrap_err();
        assert!(e.is_transient(), "{e}");
    }
}
