// Vendored (OpenStare's fork of samael 0.0.20): its own warnings are not ours to fix here.
#![allow(warnings)]
pub mod attribute;
pub mod crypto;
pub mod idp;
pub mod key_info;
pub mod metadata;
pub mod schema;
pub mod service_provider;
pub mod signature;

pub mod traits;

#[macro_use]
extern crate derive_builder;
