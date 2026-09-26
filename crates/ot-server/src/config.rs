//! Settings shared by every role. All come from flags or `OT_*` environment
//! variables; docker compose is the one place a deployment declares them.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use clap::Args;
use ot_core::SiteCode;

#[derive(Debug, Clone, Args)]
pub struct Common {
    /// SQLite database file (created and migrated on start).
    #[arg(long, env = "OT_SQLITE_PATH", default_value = "data/opentrack.db")]
    pub sqlite: PathBuf,

    /// Redis URL.
    #[arg(long, env = "OT_REDIS_URL", default_value = "redis://127.0.0.1:6379")]
    pub redis: String,

    /// Prefix for every Redis key OpenTrack owns.
    #[arg(long, env = "OT_REDIS_NAMESPACE", default_value = "tms")]
    pub redis_namespace: String,

    /// GOLD site code that prefixes every system track UID (3 chars, A-Z 0-9).
    #[arg(long, env = "OT_SITE_CODE", default_value = "OTK", value_parser = parse_site)]
    pub site: SiteCode,

    #[command(flatten)]
    pub nats: NatsArgs,

    /// Tracker profiles that ship with OpenTrack (read only). Profiles
    /// imported in the UI go to `profiles/trackers` beside the database.
    #[arg(long, env = "OT_PROFILES_DIR", default_value = "profiles/trackers")]
    pub profiles_dir: PathBuf,

    /// A connection a long-running role keeps open for `open_db` to hand out.
    #[arg(skip)]
    pub shared_db: SharedDb,
}

/// One SQLite connection kept open and shared. Opening a connection per
/// write costs the open and migration check, and closing the last one
/// checkpoints and deletes the WAL: about 6 ms, which capped the engine at
/// some 150 new tracks a second.
#[derive(Clone, Default)]
pub struct SharedDb(Option<Arc<Mutex<ot_store::Db>>>);

impl std::fmt::Debug for SharedDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "SharedDb(open)"
        } else {
            "SharedDb(none)"
        })
    }
}

/// A database connection from `open_db`: its own, or the shared one.
pub enum DbHandle<'a> {
    Own(ot_store::Db),
    Shared(MutexGuard<'a, ot_store::Db>),
}

impl std::ops::Deref for DbHandle<'_> {
    type Target = ot_store::Db;
    fn deref(&self) -> &ot_store::Db {
        match self {
            Self::Own(db) => db,
            Self::Shared(db) => db,
        }
    }
}

impl std::ops::DerefMut for DbHandle<'_> {
    fn deref_mut(&mut self) -> &mut ot_store::Db {
        match self {
            Self::Own(db) => db,
            Self::Shared(db) => db,
        }
    }
}

/// Where system tracks are published.
#[derive(Debug, Clone, Args)]
pub struct NatsArgs {
    /// NATS server URL(s), comma separated.
    #[arg(
        long = "nats-url",
        env = "OT_NATS_URL",
        default_value = "nats://127.0.0.1:4222"
    )]
    pub url: String,

    /// NATS credentials file (JWT + NKey).
    #[arg(long = "nats-creds", env = "OT_NATS_CREDS")]
    pub creds: Option<PathBuf>,

    /// NATS auth token.
    #[arg(long = "nats-token", env = "OT_NATS_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// NATS user (with `--nats-password`).
    #[arg(long = "nats-user", env = "OT_NATS_USER")]
    pub user: Option<String>,

    #[arg(
        long = "nats-password",
        env = "OT_NATS_PASSWORD",
        hide_env_values = true
    )]
    pub password: Option<String>,

    /// JetStream stream that holds system tracks (created if missing).
    #[arg(long = "nats-stream", env = "OT_NATS_STREAM", default_value = "TRACKS")]
    pub stream: String,

    /// Subject prefix: each track is published on `<prefix>.tms-<UID>`.
    #[arg(long = "nats-tracks-subject", env = "OT_NATS_TRACKS_SUBJECT", default_value = ot_core::wire::TRACKS_SUBJECT)]
    pub tracks_subject: String,

    /// Hours a message stays in a stream OpenTrack creates without being
    /// replaced. Live tracks are republished long before this.
    #[arg(
        long = "nats-max-age-hours",
        env = "OT_NATS_MAX_AGE_HOURS",
        default_value_t = 24.0
    )]
    pub max_age_hours: f64,
}

impl Common {
    /// This instance's identity, stamped into each message's `publisher`.
    pub fn node_id(&self) -> String {
        format!("opentrack-{}", self.site)
    }

    pub fn publish_context(&self) -> ot_core::wire::PublishContext {
        ot_core::wire::PublishContext {
            node_id: self.node_id(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            correlation: crate::correlate::VERSION.to_owned(),
        }
    }

    pub fn nats_settings(&self) -> ot_nats::NatsSettings {
        let n = &self.nats;
        ot_nats::NatsSettings {
            url: n.url.clone(),
            name: self.node_id(),
            creds_file: n.creds.clone(),
            token: n.token.clone(),
            user: n.user.clone(),
            password: n.password.clone(),
            stream: n.stream.clone(),
            tracks_subject: n.tracks_subject.clone(),
            max_age: std::time::Duration::from_secs_f64(n.max_age_hours.max(0.1) * 3600.0),
        }
    }

    /// Connect to NATS. Returns at once; the client reconnects in the background.
    pub async fn connect_nats(&self) -> anyhow::Result<ot_nats::Nats> {
        Ok(ot_nats::Nats::connect(self.nats_settings()).await?)
    }

    pub fn keys(&self) -> ot_store::Keys {
        ot_store::Keys::new(&self.redis_namespace)
    }

    /// The SQLite database: the shared connection when this role keeps one
    /// (see [`Common::share_db`]), else a new one (opened, migrated, its
    /// directory created).
    pub fn open_db(&self) -> anyhow::Result<DbHandle<'_>> {
        match &self.shared_db.0 {
            Some(db) => Ok(DbHandle::Shared(
                db.lock()
                    .map_err(|_| anyhow::anyhow!("database lock poisoned"))?,
            )),
            None => Ok(DbHandle::Own(self.open_new_db()?)),
        }
    }

    /// A new connection to the SQLite database.
    pub fn open_new_db(&self) -> anyhow::Result<ot_store::Db> {
        if let Some(dir) = self.sqlite.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        Ok(ot_store::Db::open(&self.sqlite)?)
    }

    /// Keep one connection open for every later `open_db` of this (and
    /// every cloned) `Common`.
    pub fn share_db(&mut self) -> anyhow::Result<()> {
        if self.shared_db.0.is_none() {
            self.shared_db = SharedDb(Some(Arc::new(Mutex::new(self.open_new_db()?))));
        }
        Ok(())
    }

    pub async fn open_redis(&self) -> anyhow::Result<ot_store::RedisStore> {
        Ok(ot_store::RedisStore::connect(&self.redis, self.keys()).await?)
    }
}

fn parse_site(s: &str) -> Result<SiteCode, String> {
    SiteCode::new(s).map_err(|e| e.to_string())
}
