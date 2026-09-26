//! OpenTrack plugin hosts. A plugin (`wit/plugin.wit`) runs either as a
//! WebAssembly component inside OpenTrack ([`wasm`]: sandboxed, with the
//! [`Grants`] an operator gives it) or as its own program that serves the
//! same interface over a socket ([`external`]: for code that cannot run as
//! WebAssembly, such as Python with numpy, or a GPU). Either way it becomes
//! an [`ot_source::plugin::Plugin`], installed where sources and the engine
//! look plugins up.

use std::sync::Arc;

use ot_source::plugin::Plugin;
use sha2::{Digest, Sha256};

pub mod external;
mod grants;
pub mod wasm;

pub use grants::{DirGrant, Grants};

/// Where a plugin comes from.
#[derive(Debug, Clone)]
pub enum Source {
    /// A WebAssembly component (the file's bytes).
    Wasm(Vec<u8>),
    /// An external plugin's address: `host:port`, `tcp://host:port` or
    /// `unix:/path/to/socket`.
    External(String),
}

/// Load a plugin, ask for its manifest and check it.
pub fn load(source: &Source, grants: &Grants) -> anyhow::Result<Arc<dyn Plugin>> {
    let plugin: Arc<dyn Plugin> = match source {
        Source::Wasm(bytes) => Arc::new(wasm::WasmPlugin::load(bytes, grants)?),
        Source::External(address) => Arc::new(external::ExternalPlugin::connect(address, grants)?),
    };
    plugin
        .manifest()
        .validate()
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(plugin)
}

/// The SHA-256 of a plugin file, in hex: how the API and the decision log
/// name exactly which build ran.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
