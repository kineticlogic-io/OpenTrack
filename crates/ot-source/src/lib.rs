//! OpenTrack source framework.
//!
//! A source is a transport plus a codec plus a mapping, all configuration.
//! Each stage hands the next a plain value: frames (bytes), records (decoded
//! trees), then observations (records in the track schema).

pub mod codec;
pub mod expr;
pub mod frame;
pub mod mapping;
pub mod path;
pub mod pipeline;
pub mod registry;
pub mod source;
pub mod transport;
