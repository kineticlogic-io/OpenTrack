//! OpenTrack storage.
//!
//! SQLite holds everything a person decided or configured: sources, schema,
//! mappings, the registry, the decision log and the temporal track graph.
//! Redis holds everything the feeds produce: observation streams, live source
//! and system track state, history and metrics. SQLite writes stay at human
//! speed while Redis absorbs thousands of observations per second.

mod app_settings;
pub mod correlation;
pub mod graph;
pub mod groups;
pub mod keys;
pub mod probe;
pub mod redis_store;
pub mod registry;
pub mod schema;
pub mod sources;
pub mod sqlite;

pub use correlation::{DecisionRow, Suggestion};
pub use graph::{EdgeKind, NodeKind};
pub use groups::{Group, GroupSpec};
pub use keys::Keys;
pub use redis_store::{GroupBacklog, OutboxEntry, OutboxOp, RedisStore};
pub use registry::{
    AttrType, Attribute, Entity, EntityRevision, RegistryIdentifier, RegistryRow, new_entity_id,
};
pub use schema::SchemaVersion;
pub use sources::{SourceRevision, SourceRow, SourceWrite};
pub use sqlite::{Db, Decision, StoreError};
