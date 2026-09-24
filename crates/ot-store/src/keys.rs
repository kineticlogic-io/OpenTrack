//! Redis key layout. Every key starts with a namespace (default `tms`) so
//! OpenTrack can share a Redis with other services, and tests can isolate
//! themselves under a unique namespace.
//!
//! | Key                               | Holds                                          |
//! |-----------------------------------|------------------------------------------------|
//! | `<ns>:obs:<source>`               | stream of mapped observations (time-trimmed)   |
//! | `<ns>:src:<source>:<key>`         | current state of a source track                |
//! | `<ns>:sys:<uid>`                  | current state of a system track                |
//! | `<ns>:hist:<source>:<key>`        | capped report history of a source track        |
//! | `<ns>:out`                        | stream of publish/tombstone work for the writer |
//! | `<ns>:metrics:<source>:<minute>`  | per-source, per-minute counters                |
//! | `<ns>:cache:*`, `<ns>:throttle:*` | static-join cache and write throttles (TTL)    |

pub const DEFAULT_NAMESPACE: &str = "tms";

#[derive(Debug, Clone)]
pub struct Keys {
    ns: String,
}

impl Default for Keys {
    fn default() -> Self {
        Self::new(DEFAULT_NAMESPACE)
    }
}

impl Keys {
    pub fn new(ns: impl Into<String>) -> Self {
        Self { ns: ns.into() }
    }

    pub fn namespace(&self) -> &str {
        &self.ns
    }

    pub fn obs_stream(&self, source: &str) -> String {
        format!("{}:obs:{source}", self.ns)
    }

    pub fn source_track(&self, source: &str, key: &str) -> String {
        format!("{}:src:{source}:{key}", self.ns)
    }

    pub fn system_track(&self, uid: &str) -> String {
        format!("{}:sys:{uid}", self.ns)
    }

    pub fn history(&self, source: &str, key: &str) -> String {
        format!("{}:hist:{source}:{key}", self.ns)
    }

    pub fn outbox(&self) -> String {
        format!("{}:out", self.ns)
    }

    /// `minute` is Unix minutes (epoch seconds / 60).
    pub fn metrics(&self, source: &str, minute: i64) -> String {
        format!("{}:metrics:{source}:{minute}", self.ns)
    }

    /// Pattern matching every key in the namespace, for SCAN.
    pub fn all(&self) -> String {
        format!("{}:*", self.ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_the_design() {
        let k = Keys::default();
        assert_eq!(k.obs_stream("ais"), "tms:obs:ais");
        assert_eq!(k.source_track("ais", "338924210"), "tms:src:ais:338924210");
        assert_eq!(k.system_track("OTK000000001"), "tms:sys:OTK000000001");
        assert_eq!(k.history("ais", "1"), "tms:hist:ais:1");
        assert_eq!(k.outbox(), "tms:out");
        assert_eq!(k.metrics("ais", 29_837_520), "tms:metrics:ais:29837520");
        assert_eq!(Keys::new("t1").all(), "t1:*");
    }
}
