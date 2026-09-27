//! OpenTrack core types.
//!
//! Everything that crosses a process boundary lives here: the authoritative
//! track schema every source is mapped into ([`schema`]), system track
//! identity ([`uid`]), system tracks themselves ([`track`]) and the message
//! they are published as ([`wire`]).

pub mod geometry;
pub mod gold;
pub mod schema;
pub mod sidc;
pub mod track;
pub mod uid;
pub mod wire;

pub use geometry::Geometry;
pub use schema::{
    Affiliation, Classification, Covariance, Domain, Ellipse, Identifier, Kinematics, Observation,
    Platform, Position, Provenance, SecurityLabel, TrackState, TrackType, Uncertainty,
    ValidationError,
};
pub use sidc::{Sidc, SidcStandard};
pub use track::{
    AttributeNotice, BearingContact, Contributor, PairingType, SystemTrack, TrackKind,
};
pub use uid::{SiteCode, Uid, UidError};

/// Version of the core schema. Extension-field schema versions are separate
/// and admin-managed; this only moves when the fixed core changes.
pub const CORE_SCHEMA_VERSION: u32 = 1;
