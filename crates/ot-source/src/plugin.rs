//! Codec plugins: formats OpenTrack decodes through a common interface
//! rather than a built-in codec (`"codec": {"type": "plugin", "plugin": "stanag4607",
//! "options": {..}}`). A plugin turns a frame's bytes into JSON records, keeps
//! whatever state its format needs across frames of one stream, and says how
//! to split a byte stream of its format into frames.
//!
//! Plugins are compiled in today (see [`plugins`]). The interface is what a
//! sandboxed (WebAssembly) plugin will implement too, so a source's
//! configuration does not change when one is loaded instead.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::frame::Framing;

pub mod stanag4607;

/// A codec plugin.
pub trait CodecPlugin: Send + Sync {
    /// The name sources refer to it by.
    fn name(&self) -> &'static str;
    /// Version of its output (records for the same bytes); recorded with its records.
    fn version(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// The options it takes, for the UI; [`default_options`] are what a new
    /// source starts with.
    fn options(&self) -> Vec<PluginOption>;
    /// How to split a byte stream of this format into frames (TCP, files).
    fn framing(&self) -> Option<Framing>;
    /// A decoder for one stream, with these options (checked here).
    fn decoder(&self, options: &Value) -> Result<Box<dyn PluginDecoder>, String>;
}

/// One option a plugin takes.
#[derive(Debug, Clone, Serialize)]
pub struct PluginOption {
    pub name: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    #[serde(flatten)]
    pub kind: OptionKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OptionKind {
    Bool {
        default: bool,
    },
    Choice {
        choices: &'static [&'static str],
        default: &'static str,
    },
    /// A number; absent from the options when left empty.
    Number {
        default: Option<f64>,
        /// Unit shown next to the input, e.g. `m`.
        unit: &'static str,
        /// Smallest value allowed (the plugin says whether it is exclusive).
        min: Option<f64>,
    },
}

/// A plugin's options at their defaults (numbers only when they have one).
pub fn default_options(p: &dyn CodecPlugin) -> Value {
    let mut m = Map::new();
    for o in p.options() {
        let v = match o.kind {
            OptionKind::Bool { default } => Value::from(default),
            OptionKind::Choice { default, .. } => Value::from(default),
            OptionKind::Number { default, .. } => match default {
                Some(d) => Value::from(d),
                None => continue,
            },
        };
        m.insert(o.name.into(), v);
    }
    Value::Object(m)
}

/// A plugin as the API lists it.
pub fn describe(p: &dyn CodecPlugin) -> Value {
    serde_json::json!({
        "name": p.name(),
        "version": p.version(),
        "description": p.description(),
        "options": p.options(),
        "default_options": default_options(p),
        "framing": p.framing(),
    })
}

/// Decodes one stream's frames, keeping state between them.
pub trait PluginDecoder: Send {
    fn decode(&mut self, bytes: &[u8]) -> Result<Vec<Value>, String>;

    /// What the stream has shown about the sensor so far, for stages that
    /// adapt to it (a tracker's `auto_timing`).
    fn hints(&self) -> Option<StreamHints> {
        None
    }
}

/// What a decoder has learnt about its sensor from the stream.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StreamHints {
    /// How often the sensor comes back to the same ground (s).
    pub revisit_secs: f64,
    /// How the decoder knows, e.g. `measured` or `nominal`.
    pub revisit_source: &'static str,
}

/// Every plugin this build has.
pub fn plugins() -> Vec<&'static dyn CodecPlugin> {
    vec![&stanag4607::Stanag4607]
}

pub fn plugin(name: &str) -> Option<&'static dyn CodecPlugin> {
    plugins().into_iter().find(|p| p.name() == name)
}
