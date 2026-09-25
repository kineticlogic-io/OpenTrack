//! OpenTrack storage.
//!
//! SQLite holds everything a person decided or configured: sources, schema,
//! mappings, the registry, the decision log and the temporal track graph.
//! Redis holds everything the feeds produce: observation streams, live source
//! and system track state, history and metrics. SQLite writes stay at human
//! speed while Redis absorbs thousands of observations per second.

pub mod graph;
pub mod keys;
pub mod redis_store;
pub mod registry;
pub mod schema;
pub mod sources;
pub mod sqlite;

pub use graph::{EdgeKind, NodeKind};
pub use keys::Keys;
pub use redis_store::{OutboxEntry, OutboxOp, RedisStore};
pub use registry::{RegistryEntity, RegistryIdentifier, RegistryRow};
pub use schema::SchemaVersion;
pub use sources::{SourceRevision, SourceRow, SourceWrite};
pub use sqlite::{Db, Decision, StoreError};
