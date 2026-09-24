//! Settings shared by every role. All come from flags or `OT_*` environment
//! variables; docker compose is the one place a deployment declares them.

use std::path::PathBuf;

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

    /// peat-node sidecar gRPC address.
    #[arg(long, env = "OT_PEAT_ADDR", default_value = "http://127.0.0.1:50051")]
    pub peat: String,

    /// GOLD site code that prefixes every system track UID (3 chars, A-Z 0-9).
    #[arg(long, env = "OT_SITE_CODE", default_value = "OTK", value_parser = parse_site)]
    pub site: SiteCode,

    /// peat-node collection for system tracks.
    #[arg(long, env = "OT_TRACKS_COLLECTION", default_value = ot_core::peat::TRACKS_COLLECTION)]
    pub tracks_collection: String,
}

impl Common {
    /// This instance's identity on the mesh, stamped into `source.node_id`.
    pub fn node_id(&self) -> String {
        format!("opentrack-{}", self.site)
    }

    pub fn publish_context(&self) -> ot_core::peat::PublishContext {
        ot_core::peat::PublishContext {
            node_id: self.node_id(),
            model_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    pub fn keys(&self) -> ot_store::Keys {
        ot_store::Keys::new(&self.redis_namespace)
    }

    /// Open (and migrate) the SQLite database, creating its directory.
    pub fn open_db(&self) -> anyhow::Result<ot_store::Db> {
        if let Some(dir) = self.sqlite.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        Ok(ot_store::Db::open(&self.sqlite)?)
    }

    pub async fn open_redis(&self) -> anyhow::Result<ot_store::RedisStore> {
        Ok(ot_store::RedisStore::connect(&self.redis, self.keys()).await?)
    }
}

fn parse_site(s: &str) -> Result<SiteCode, String> {
    SiteCode::new(s).map_err(|e| e.to_string())
}
