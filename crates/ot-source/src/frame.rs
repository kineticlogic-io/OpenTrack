//! Framing: cutting a byte stream into frames.
//!
//! Framing is transport configuration, not codec code, so any codec runs over
//! any transport that can frame its bytes. Message-oriented transports (UDP,
//! HTTP, WebSocket, brokers) already deliver one frame per message and use
//! [`Framing::Message`].

use bytes::{Buf, Bytes, BytesMut};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Default ceiling on one frame, to bound memory against a misbehaving peer.
pub const DEFAULT_MAX_FRAME: usize = 4 * 1024 * 1024;

/// One unit of input for a codec.
#[derive(Debug, Clone)]
pub struct Frame {
    pub bytes: Bytes,
    pub received_at: DateTime<Utc>,
    /// Where it came from, e.g. the peer address; for probes and debugging.
    pub origin: Option<String>,
    /// Transport metadata (e.g. an MQTT `topic`), added to every decoded
    /// record under `_frame` so mappings can use it.
    pub meta: serde_json::Map<String, serde_json::Value>,
}

impl Frame {
    pub fn new(bytes: impl Into<Bytes>) -> Self {
        Self {
            bytes: bytes.into(),
            received_at: Utc::now(),
            origin: None,
            meta: Default::default(),
        }
    }

    pub fn with_meta(mut self, meta: serde_json::Map<String, serde_json::Value>) -> Self {
        self.meta = meta;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrefixWidth {
    U8,
    U16,
    U32,
    /// Protobuf-style base-128 varint.
    Varint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Endian {
    #[default]
    Big,
    Little,
}

/// How a byte stream is split into frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Framing {
    /// The transport delivers whole messages (datagrams, bodies, WebSocket messages).
    Message,
    /// Newline-terminated; a trailing `\r` is stripped and empty lines skipped.
    Lines {
        #[serde(default = "default_max")]
        max_len: usize,
    },
    /// A length header followed by that many bytes.
    LengthPrefix {
        width: PrefixWidth,
        #[serde(default)]
        endian: Endian,
        #[serde(default = "default_max")]
        max_len: usize,
    },
    /// Frames separated by a delimiter, which is not part of the frame.
    Delimiter {
        delimiter: String,
        #[serde(default = "default_max")]
        max_len: usize,
    },
    /// Frames that end with a closing tag, which is kept (e.g. `</event>` for CoT).
    /// Bytes before the first opening `<` of a frame are discarded.
    EndTag {
        tag: String,
        #[serde(default = "default_max")]
        max_len: usize,
    },
}

fn default_max() -> usize {
    DEFAULT_MAX_FRAME
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame of {len} bytes exceeds the {max}-byte limit")]
    TooLong { len: usize, max: usize },
    #[error("malformed varint length prefix")]
    BadVarint,
    #[error("delimiter or tag must not be empty")]
    EmptyDelimiter,
}

/// Incremental splitter over a stream buffer.
#[derive(Debug, Clone)]
pub struct Framer {
    framing: Framing,
}

impl Framer {
    pub fn new(framing: Framing) -> Result<Self, FrameError> {
        match &framing {
            Framing::Delimiter { delimiter, .. } if delimiter.is_empty() => {
                return Err(FrameError::EmptyDelimiter);
            }
            Framing::EndTag { tag, .. } if tag.is_empty() => {
                return Err(FrameError::EmptyDelimiter);
            }
            _ => {}
        }
        Ok(Self { framing })
    }

    /// Take the next complete frame from `buf`, if there is one. On an
    /// oversize frame the buffer is cleared and an error returned, so the
    /// stream can continue with the next frame.
    pub fn next(&mut self, buf: &mut BytesMut) -> Result<Option<Bytes>, FrameError> {
        let res = self.split(buf);
        if res.is_err() {
            buf.clear();
        }
        res
    }

    fn split(&mut self, buf: &mut BytesMut) -> Result<Option<Bytes>, FrameError> {
        match &self.framing {
            Framing::Message => {
                if buf.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(buf.split().freeze()))
                }
            }
            Framing::Lines { max_len } => loop {
                let Some(pos) = buf.iter().position(|b| *b == b'\n') else {
                    check_len(buf.len(), *max_len)?;
                    return Ok(None);
                };
                let mut line = buf.split_to(pos + 1);
                line.truncate(pos);
                if line.last() == Some(&b'\r') {
                    line.truncate(line.len() - 1);
                }
                check_len(line.len(), *max_len)?;
                if !line.iter().all(u8::is_ascii_whitespace) {
                    return Ok(Some(line.freeze()));
                }
            },
            Framing::Delimiter { delimiter, max_len } => loop {
                let delim = unescape(delimiter);
                let Some(pos) = find(buf, &delim) else {
                    check_len(buf.len(), *max_len)?;
                    return Ok(None);
                };
                let frame = buf.split_to(pos);
                buf.advance(delim.len());
                check_len(frame.len(), *max_len)?;
                if !frame.is_empty() {
                    return Ok(Some(frame.freeze()));
                }
            },
            Framing::EndTag { tag, max_len } => {
                // Drop leading noise (whitespace, keep-alives) before the frame.
                match buf.iter().position(|b| *b == b'<') {
                    Some(start) => buf.advance(start),
                    None => {
                        buf.clear();
                        return Ok(None);
                    }
                }
                let Some(pos) = find(buf, tag.as_bytes()) else {
                    check_len(buf.len(), *max_len)?;
                    return Ok(None);
                };
                let end = pos + tag.len();
                check_len(end, *max_len)?;
                Ok(Some(buf.split_to(end).freeze()))
            }
            Framing::LengthPrefix {
                width,
                endian,
                max_len,
            } => {
                let Some((header, len)) = read_prefix(buf, *width, *endian)? else {
                    return Ok(None);
                };
                check_len(len, *max_len)?;
                if buf.len() < header + len {
                    return Ok(None);
                }
                buf.advance(header);
                Ok(Some(buf.split_to(len).freeze()))
            }
        }
    }
}

fn check_len(len: usize, max: usize) -> Result<(), FrameError> {
    if len > max {
        Err(FrameError::TooLong { len, max })
    } else {
        Ok(())
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `\n`, `\r`, `\t`, `\0` and `\xHH` escapes in configured delimiters.
fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut b = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('0') => out.push(0),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(b) => out.push(b),
                    Err(_) => out.extend_from_slice(format!("\\x{hex}").as_bytes()),
                }
            }
            Some(other) => {
                out.push(b'\\');
                let mut b = [0; 4];
                out.extend_from_slice(other.encode_utf8(&mut b).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
}

/// Returns (header length, payload length) once the header is complete.
fn read_prefix(
    buf: &[u8],
    width: PrefixWidth,
    endian: Endian,
) -> Result<Option<(usize, usize)>, FrameError> {
    let fixed = |n: usize| -> Option<(usize, usize)> {
        if buf.len() < n {
            return None;
        }
        let bytes = &buf[..n];
        let v = match endian {
            Endian::Big => bytes.iter().fold(0usize, |acc, b| (acc << 8) | *b as usize),
            Endian::Little => bytes
                .iter()
                .rev()
                .fold(0usize, |acc, b| (acc << 8) | *b as usize),
        };
        Some((n, v))
    };
    Ok(match width {
        PrefixWidth::U8 => fixed(1),
        PrefixWidth::U16 => fixed(2),
        PrefixWidth::U32 => fixed(4),
        PrefixWidth::Varint => {
            let mut value = 0usize;
            for (i, b) in buf.iter().enumerate().take(10) {
                value |= ((b & 0x7f) as usize) << (7 * i);
                if b & 0x80 == 0 {
                    return Ok(Some((i + 1, value)));
                }
            }
            if buf.len() >= 10 {
                return Err(FrameError::BadVarint);
            }
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(framing: Framing, chunks: &[&[u8]]) -> Vec<String> {
        let mut f = Framer::new(framing).unwrap();
        let mut buf = BytesMut::new();
        let mut out = Vec::new();
        for c in chunks {
            buf.extend_from_slice(c);
            while let Some(frame) = f.next(&mut buf).unwrap() {
                out.push(String::from_utf8(frame.to_vec()).unwrap());
            }
        }
        out
    }

    #[test]
    fn lines_across_chunk_boundaries() {
        let got = all(
            Framing::Lines { max_len: 100 },
            &[b"{\"a\":1}\r\n{\"b\"", b":2}\n\n  \n{\"c\":3}"],
        );
        assert_eq!(got, ["{\"a\":1}", "{\"b\":2}"]);
    }

    #[test]
    fn end_tag_keeps_the_tag_and_skips_noise() {
        let got = all(
            Framing::EndTag {
                tag: "</event>".into(),
                max_len: 1000,
            },
            &[
                b"\n<event uid=\"a\"/></eve",
                b"nt>  <event uid=\"b\"></event>",
            ],
        );
        assert_eq!(
            got,
            ["<event uid=\"a\"/></event>", "<event uid=\"b\"></event>"]
        );
    }

    #[test]
    fn delimiter_with_escapes() {
        let got = all(
            Framing::Delimiter {
                delimiter: "\\x03".into(),
                max_len: 100,
            },
            &[b"one\x03two\x03\x03thr", b"ee\x03"],
        );
        assert_eq!(got, ["one", "two", "three"]);
    }

    #[test]
    fn length_prefixes() {
        let got = all(
            Framing::LengthPrefix {
                width: PrefixWidth::U16,
                endian: Endian::Big,
                max_len: 100,
            },
            &[b"\x00\x03ab", b"c\x00\x02de"],
        );
        assert_eq!(got, ["abc", "de"]);
        let got = all(
            Framing::LengthPrefix {
                width: PrefixWidth::U32,
                endian: Endian::Little,
                max_len: 100,
            },
            &[b"\x02\x00\x00\x00hi"],
        );
        assert_eq!(got, ["hi"]);
        // 300 = 0xAC 0x02 as a varint.
        let mut payload = vec![0xAC, 0x02];
        payload.extend([b'x'; 300]);
        let got = all(
            Framing::LengthPrefix {
                width: PrefixWidth::Varint,
                endian: Endian::Big,
                max_len: 1000,
            },
            &[&payload],
        );
        assert_eq!(got[0].len(), 300);
    }

    #[test]
    fn oversize_frames_are_rejected_and_the_stream_recovers() {
        let mut f = Framer::new(Framing::Lines { max_len: 4 }).unwrap();
        let mut buf = BytesMut::from(&b"toolong\n"[..]);
        assert!(matches!(f.next(&mut buf), Err(FrameError::TooLong { .. })));
        assert!(buf.is_empty());
        buf.extend_from_slice(b"ok\n");
        assert_eq!(
            f.next(&mut buf).unwrap().unwrap(),
            Bytes::from_static(b"ok")
        );
    }

    #[test]
    fn empty_delimiters_are_refused() {
        assert!(
            Framer::new(Framing::Delimiter {
                delimiter: String::new(),
                max_len: 1
            })
            .is_err()
        );
    }
}
