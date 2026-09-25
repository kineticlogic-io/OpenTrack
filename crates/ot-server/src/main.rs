//! `opentrack`: one binary, one subcommand per server role.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use ot_peat::PeatClient;

mod api;
mod config;
mod control;
mod engine;
mod probe;
mod sources;
mod synthetic;
mod writer;

use config::Common;

#[derive(Parser)]
#[command(
    name = "opentrack",
    version,
    about = "Source-agnostic track management server"
)]
struct Cli {
    #[command(flatten)]
    common: Common,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create or upgrade the SQLite database, then exit.
    Migrate,
    /// Run the control plane (REST API and UI).
    Serve(ServeArgs),
    /// Run the peat writer (Redis outbox to peat-node).
    Writer(WriterArgs),
    /// Run every enabled source (transports and pipelines).
    Sources,
    /// Run the engine (source tracks to system tracks, lifecycle).
    Engine(EngineArgs),
    /// Run every role in one process: control plane, sources, engine, writer.
    All {
        #[command(flatten)]
        serve: ServeArgs,
        #[command(flatten)]
        writer: WriterArgs,
        #[command(flatten)]
        engine: EngineArgs,
    },
    /// Push synthetic tracks through the pipeline.
    Synthetic(synthetic::SyntheticArgs),
    /// Retire a system track: record the decision, close its graph links and
    /// tombstone its peat-node document.
    Retire {
        /// UID or `tms-<UID>` document id.
        uid: String,
        /// Reason recorded in the decision log.
        #[arg(long, default_value = "retired from the command line")]
        reason: String,
    },
}

#[derive(Debug, Clone, Args)]
struct ServeArgs {
    /// Address the control plane listens on.
    #[arg(long, env = "OT_BIND", default_value = "0.0.0.0:8090")]
    bind: SocketAddr,
    /// Built UI to serve at `/` (skipped if it has no index.html).
    #[arg(long, env = "OT_UI_DIR", default_value = "ui/dist")]
    ui_dir: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct WriterArgs {
    /// Consumer name within the writer group; unique per writer instance.
    #[arg(long, env = "OT_WRITER_CONSUMER", default_value = "writer-1")]
    consumer: String,
    /// Minimum seconds between writes of the same system track.
    #[arg(long, env = "OT_WRITE_MIN_INTERVAL_SECS", default_value_t = 5.0)]
    min_interval_secs: f64,
}

#[derive(Debug, Clone, Args)]
struct EngineArgs {
    /// Consumer name within the engine group.
    #[arg(long, env = "OT_ENGINE_CONSUMER", default_value = "engine-1")]
    engine_consumer: String,
    /// Observations before a tentative system track is confirmed.
    #[arg(long, env = "OT_CONFIRM_AFTER", default_value_t = 3)]
    confirm_after: u64,
    /// Hours without a report before a system track is dropped.
    #[arg(long, env = "OT_DROP_AFTER_HOURS", default_value_t = 6.0)]
    drop_after_hours: f64,
}

impl EngineArgs {
    fn settings(&self) -> engine::EngineSettings {
        engine::EngineSettings {
            consumer: self.engine_consumer.clone(),
            confirm_after: self.confirm_after.max(1),
            drop_after: Duration::from_secs_f64(self.drop_after_hours.max(0.01) * 3600.0),
            ..Default::default()
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let common = cli.common;
    match cli.command {
        Command::Migrate => {
            let db = common.open_db()?;
            println!(
                "{}: schema version {}",
                common.sqlite.display(),
                db.schema_version()?
            );
            Ok(())
        }
        Command::Serve(args) => serve(common, args).await,
        Command::Writer(args) => run_writer(common, args).await,
        Command::Sources => {
            common.open_db()?;
            sources::run(common, shutdown_signal()).await
        }
        Command::Engine(args) => run_engine(common, args).await,
        Command::All {
            serve: s,
            writer: w,
            engine: e,
        } => {
            // Migrate once before the roles open the database concurrently.
            common.open_db()?;
            tokio::try_join!(
                serve(common.clone(), s),
                run_writer(common.clone(), w),
                sources::run(common.clone(), shutdown_signal()),
                run_engine(common, e),
            )?;
            Ok(())
        }
        Command::Synthetic(args) => synthetic::run(&common, args).await,
        Command::Retire { uid, reason } => {
            let uid = ot_core::Uid::from_doc_id(&uid).or_else(|_| uid.parse())?;
            let decision = common.open_db()?.retire_system_track(
                uid,
                ot_store::Decision::new("cli", "retire_system_track").reason(reason),
            )?;
            common
                .open_redis()
                .await?
                .retire_system_track(uid, &common.tracks_collection)
                .await?;
            println!(
                "retired {} (decision {decision}); tombstone queued for the writer",
                uid.doc_id()
            );
            Ok(())
        }
    }
}

async fn serve(common: Common, args: ServeArgs) -> anyhow::Result<()> {
    let db = common.open_db()?;
    let state = control::AppState {
        db: Arc::new(Mutex::new(db)),
        redis: common.open_redis().await?,
        peat: PeatClient::connect_lazy(&common.peat)?,
        common,
    };
    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("binding {}", args.bind))?;
    tracing::info!(addr = %args.bind, "control plane listening");
    axum::serve(listener, control::router(state, Some(args.ui_dir)))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn run_writer(common: Common, args: WriterArgs) -> anyhow::Result<()> {
    let settings = writer::WriterSettings {
        consumer: args.consumer,
        collection: common.tracks_collection.clone(),
        min_interval: Duration::from_secs_f64(args.min_interval_secs.max(0.0)),
        retry_after: Duration::from_secs(5),
        batch: 500,
    };
    let peat = PeatClient::connect_lazy(&common.peat)?;
    match peat.status().await {
        Ok(s) => {
            tracing::info!(node_id = %s.node_id, peers = s.connected_peers, "peat-node online")
        }
        Err(e) => tracing::warn!(%e, "peat-node not answering yet; writes will retry"),
    }
    let w = writer::Writer::new(
        common.open_redis().await?,
        peat,
        common.publish_context(),
        settings,
    );
    w.run(shutdown_signal()).await?;
    Ok(())
}

async fn run_engine(common: Common, args: EngineArgs) -> anyhow::Result<()> {
    common.open_db()?;
    engine::Engine::new(common, args.settings())
        .await?
        .run(shutdown_signal())
        .await
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutting down");
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("OT_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if std::env::var("OT_LOG_FORMAT").as_deref() == Ok("json") {
        builder.json().init();
    } else {
        builder.init();
    }
}
