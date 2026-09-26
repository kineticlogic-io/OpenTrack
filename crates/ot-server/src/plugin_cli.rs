//! `opentrack plugin`: check a plugin (a `.wasm` file or an external
//! plugin's address) before adding it, add it, or list what is added.

use std::path::PathBuf;
use std::time::Instant;

use chrono::Utc;
use clap::Subcommand;
use ot_plugin::{Grants, Source};
use ot_source::plugin::{Kind, default_options};
use serde_json::{Value, json};

use crate::config::Common;

#[derive(Debug, Subcommand)]
pub enum PluginCommand {
    /// Load a plugin and open each kind it provides with its default
    /// options; prints its manifest and what worked. Changes nothing.
    Check {
        /// A WebAssembly component (.wasm), or an external plugin's address
        /// (host:port, tcp://host:port, unix:/path).
        plugin: String,
        /// Grants to check it with (JSON), e.g. '{"memory_mb": 512}'.
        #[arg(long)]
        grants: Option<String>,
    },
    /// Add a plugin (or, with --replace, a new build of one), as Settings →
    /// Plugins does.
    Add {
        plugin: String,
        #[arg(long)]
        grants: Option<String>,
        #[arg(long)]
        replace: bool,
    },
    /// The plugins added, and whether each is enabled.
    List,
}

/// A plugin argument: a file that exists is a component, anything else an address.
pub fn source_arg(arg: &str) -> anyhow::Result<Source> {
    let path = PathBuf::from(arg);
    if path.is_file() {
        return Ok(Source::Wasm(std::fs::read(&path)?));
    }
    if arg.ends_with(".wasm") {
        anyhow::bail!("{arg}: no such file");
    }
    Ok(Source::External(arg.to_owned()))
}

fn grants_arg(raw: Option<&str>) -> anyhow::Result<Grants> {
    let g: Grants = match raw {
        Some(r) => serde_json::from_str(r)?,
        None => Grants::default(),
    };
    g.validate().map_err(anyhow::Error::msg)?;
    Ok(g)
}

/// Load, then open each kind with default options (and run it once).
pub fn smoke(source: &Source, grants: &Grants) -> Value {
    let started = Instant::now();
    let plugin = match ot_plugin::load(source, grants) {
        Ok(p) => p,
        Err(e) => return json!({ "ok": false, "error": format!("{e:#}") }),
    };
    let load_ms = started.elapsed().as_millis() as u64;
    let m = plugin.manifest().clone();
    let options = default_options(&m);
    let mut kinds = serde_json::Map::new();
    let mut ok = true;
    for kind in &m.kinds {
        let result: Result<(), String> = match kind {
            Kind::Codec => plugin.decoder(&options).map(|_| ()),
            Kind::Tracker => plugin
                .tracker(&options)
                .and_then(|mut t| t.run(Utc::now(), true).map(|_| ())),
            Kind::Scorer => plugin.scorer(&options).map(|_| ()),
        };
        ok &= result.is_ok();
        let key = serde_json::to_value(kind).unwrap_or_default();
        kinds.insert(
            key.as_str().unwrap_or_default().to_owned(),
            match result {
                Ok(()) => json!("ok"),
                Err(e) => json!({ "error": e }),
            },
        );
    }
    json!({
        "ok": ok, "manifest": m, "default_options": options, "kinds": kinds, "load_ms": load_ms,
    })
}

pub fn run(common: &Common, cmd: PluginCommand) -> anyhow::Result<()> {
    match cmd {
        PluginCommand::Check { plugin, grants } => {
            let report = smoke(&source_arg(&plugin)?, &grants_arg(grants.as_deref())?);
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report["ok"] != true {
                std::process::exit(1);
            }
        }
        PluginCommand::Add {
            plugin,
            grants,
            replace,
        } => {
            let source = source_arg(&plugin)?;
            let grants = grants_arg(grants.as_deref())?;
            let p = ot_plugin::load(&source, &grants)?;
            let m = p.manifest().clone();
            let mut db = common.open_db()?;
            if !replace && db.get_plugin(&m.name)?.is_some() {
                anyhow::bail!("plugin {} exists: add it with --replace", m.name);
            }
            let sha = match &source {
                Source::Wasm(b) => Some(ot_plugin::sha256_hex(b)),
                Source::External(_) => None,
            };
            let (wasm, address) = match &source {
                Source::Wasm(b) => (Some(b.as_slice()), None),
                Source::External(a) => (None, Some(a.as_str())),
            };
            let row = db.put_plugin(
                &ot_store::PluginWrite {
                    name: &m.name,
                    version: &m.version,
                    manifest: &serde_json::to_value(&m)?,
                    wasm: wasm.zip(sha.as_deref()),
                    address,
                    grants: &serde_json::to_value(&grants)?,
                },
                "cli",
            )?;
            println!("added {} {} ({})", row.name, row.version, row.runtime);
        }
        PluginCommand::List => {
            for r in common.open_db()?.list_plugins()? {
                println!(
                    "{:<24} {:<10} {:<9} {:<8} {}",
                    r.name,
                    r.version,
                    r.runtime,
                    if r.enabled { "enabled" } else { "disabled" },
                    r.manifest["kinds"]
                );
            }
        }
    }
    Ok(())
}
