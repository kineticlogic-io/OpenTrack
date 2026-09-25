//! System track identity.
//!
//! Follows OTH-GOLD's UID: a 3-character site code followed by a 9-digit
//! sequence, e.g. `OTK000000042`. A system track is published as `tms-<UID>`,
//! stable for the life of the track.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Largest sequence a 9-digit UID can carry.
pub const MAX_SEQUENCE: u64 = 999_999_999;

/// Prefix of a system track's published id.
pub const DOC_ID_PREFIX: &str = "tms-";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UidError {
    #[error("site code must be exactly 3 characters A-Z or 0-9, got {0:?}")]
    BadSiteCode(String),
    #[error("sequence {0} is outside 1..={MAX_SEQUENCE}")]
    BadSequence(u64),
    #[error("not a UID: {0:?}")]
    Malformed(String),
}

/// The 3-character site code that owns a UID range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SiteCode([u8; 3]);

impl SiteCode {
    pub fn new(code: &str) -> Result<Self, UidError> {
        let bytes = code.as_bytes();
        let valid = bytes.len() == 3
            && bytes
                .iter()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        if !valid {
            return Err(UidError::BadSiteCode(code.to_owned()));
        }
        Ok(Self([bytes[0], bytes[1], bytes[2]]))
    }

    pub fn as_str(&self) -> &str {
        // Constructed only from validated ASCII.
        std::str::from_utf8(&self.0).expect("site code is ASCII")
    }
}

impl fmt::Display for SiteCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SiteCode {
    type Err = UidError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// A system track UID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Uid {
    site: SiteCode,
    sequence: u64,
}

impl Uid {
    pub fn new(site: SiteCode, sequence: u64) -> Result<Self, UidError> {
        if sequence == 0 || sequence > MAX_SEQUENCE {
            return Err(UidError::BadSequence(sequence));
        }
        Ok(Self { site, sequence })
    }

    pub fn site(&self) -> SiteCode {
        self.site
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// The published track id, `tms-<UID>`.
    pub fn doc_id(&self) -> String {
        format!("{DOC_ID_PREFIX}{self}")
    }

    /// Parse a published track id back into a UID.
    pub fn from_doc_id(doc_id: &str) -> Result<Self, UidError> {
        doc_id
            .strip_prefix(DOC_ID_PREFIX)
            .ok_or_else(|| UidError::Malformed(doc_id.to_owned()))?
            .parse()
    }
}

impl fmt::Display for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{:09}", self.site, self.sequence)
    }
}

impl FromStr for Uid {
    type Err = UidError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 12 || !s.is_ascii() {
            return Err(UidError::Malformed(s.to_owned()));
        }
        let (site, seq) = s.split_at(3);
        if !seq.bytes().all(|b| b.is_ascii_digit()) {
            return Err(UidError::Malformed(s.to_owned()));
        }
        let sequence = seq.parse().map_err(|_| UidError::Malformed(s.to_owned()))?;
        Self::new(SiteCode::new(site)?, sequence)
    }
}

impl Serialize for Uid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Uid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_as_site_plus_nine_digits() {
        let uid = Uid::new(SiteCode::new("OTK").unwrap(), 42).unwrap();
        assert_eq!(uid.to_string(), "OTK000000042");
        assert_eq!(uid.doc_id(), "tms-OTK000000042");
    }

    #[test]
    fn round_trips_through_text_and_doc_id() {
        let uid: Uid = "SD1123456789".parse().unwrap();
        assert_eq!(uid.site().as_str(), "SD1");
        assert_eq!(uid.sequence(), 123_456_789);
        assert_eq!(Uid::from_doc_id(&uid.doc_id()).unwrap(), uid);
        let json = serde_json::to_string(&uid).unwrap();
        assert_eq!(json, "\"SD1123456789\"");
        assert_eq!(serde_json::from_str::<Uid>(&json).unwrap(), uid);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(SiteCode::new("ot").is_err());
        assert!(SiteCode::new("otk").is_err());
        assert!(SiteCode::new("OT-").is_err());
        let site = SiteCode::new("OTK").unwrap();
        assert!(Uid::new(site, 0).is_err());
        assert!(Uid::new(site, MAX_SEQUENCE + 1).is_err());
        assert!("OTK00000004".parse::<Uid>().is_err());
        assert!("OTK00000004x".parse::<Uid>().is_err());
        assert!("OTK+00000042".parse::<Uid>().is_err());
        assert!(Uid::from_doc_id("ais-338924210").is_err());
    }
}
