//! A stage-by-stage record of what a pipeline did with a few frames, for the
//! pipeline designer's live preview.
//!
//! Tracing is opt-in: [`crate::pipeline::Pipeline::process_traced`] fills a
//! [`Trace`]; [`crate::pipeline::Pipeline::process`] takes no trace and
//! neither clones nor allocates for one.

use ot_core::Observation;
use serde::Serialize;
use serde_json::{Value, json};

use crate::frame::Frame;

/// Largest JSON item kept whole; bigger ones are cut to a marker.
pub const MAX_ITEM_BYTES: usize = 64 * 1024;

/// The most frames one trace follows.
pub const MAX_FRAMES: usize = 10;

/// The first frames through a pipeline, stage by stage.
#[derive(Debug, Default, Serialize)]
pub struct Trace {
    pub frames: Vec<FrameTrace>,
    #[serde(skip)]
    limit: usize,
}

/// One input frame and what became of it after each stage.
#[derive(Debug, Serialize)]
pub struct FrameTrace {
    /// The frame as received (the transport stage).
    pub frame: FrameView,
    /// The pipeline's stages after the transport, in order, only those it has.
    pub stages: Vec<StageTrace>,
    /// What the pipeline emitted from this frame, to show as published.
    #[serde(skip)]
    pub emitted: Vec<Observation>,
    /// Detections this frame gave the tracker stage.
    #[serde(skip)]
    pub(crate) plots: Vec<Observation>,
}

/// A raw frame, readable: JSON when it parses, else text, else escaped bytes.
#[derive(Debug, Serialize)]
pub struct FrameView {
    /// `json`, `text` or `binary`.
    pub format: &'static str,
    /// Length of the frame in bytes.
    pub bytes: usize,
    /// The parsed JSON, the text, or the bytes escaped (`\xNN`).
    pub content: Value,
}

/// A frame's records after one stage.
#[derive(Debug, Serialize)]
pub struct StageTrace {
    /// The stage id, as the UI names it (`decode`, `map`, `filter`…).
    pub id: &'static str,
    /// What this stage passed on: records up to the map stage, observations
    /// after it.
    pub items: Vec<Value>,
    /// Why records were dropped here, one entry per record.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<String>,
    /// The stage held the frame's records back (a tracker waiting for the
    /// rest of a scan); what it made of them later is in `items`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
}

impl Trace {
    /// Follow the first `frames` frames (at most [`MAX_FRAMES`]).
    pub fn new(frames: usize) -> Self {
        Self {
            frames: Vec::new(),
            limit: frames.min(MAX_FRAMES),
        }
    }

    /// Start tracing a frame, if the trace still has room.
    pub(crate) fn begin(&mut self, frame: &Frame, stages: &[&'static str]) -> Option<usize> {
        if self.frames.len() >= self.limit {
            return None;
        }
        self.frames.push(FrameTrace {
            frame: FrameView::of(&frame.bytes),
            stages: stages
                .iter()
                .map(|id| StageTrace {
                    id,
                    items: Vec::new(),
                    dropped: Vec::new(),
                    held: false,
                })
                .collect(),
            emitted: Vec::new(),
            plots: Vec::new(),
        });
        Some(self.frames.len() - 1)
    }
}

impl FrameTrace {
    pub fn stage(&mut self, id: &str) -> Option<&mut StageTrace> {
        self.stages.iter_mut().find(|s| s.id == id)
    }

    /// Record what `id` passed on (built only if the pipeline has that stage).
    pub(crate) fn item(&mut self, id: &str, v: impl FnOnce() -> Value) {
        if let Some(s) = self.stage(id) {
            s.items.push(capped(v()));
        }
    }

    /// Record an observation (or its JSON form) `id` passed on.
    pub(crate) fn obs<T: Serialize>(&mut self, id: &str, o: &T) {
        self.item(id, || serde_json::to_value(o).unwrap_or(Value::Null));
    }

    /// Record a record dropped at `id`.
    pub(crate) fn dropped(&mut self, id: &str, reason: impl Into<String>) {
        if let Some(s) = self.stage(id) {
            s.dropped.push(reason.into());
        }
    }
}

impl FrameView {
    fn of(bytes: &[u8]) -> Self {
        let n = bytes.len();
        match std::str::from_utf8(bytes) {
            Ok(text) => match serde_json::from_str::<Value>(text) {
                Ok(v) => Self {
                    format: "json",
                    bytes: n,
                    content: capped(v),
                },
                Err(_) => Self {
                    format: "text",
                    bytes: n,
                    content: Value::String(cut(text, MAX_ITEM_BYTES)),
                },
            },
            Err(_) => {
                let mut s = String::new();
                for &b in bytes {
                    if s.len() >= MAX_ITEM_BYTES {
                        s.push_str(" …");
                        break;
                    }
                    s.extend(std::ascii::escape_default(b).map(char::from));
                }
                Self {
                    format: "binary",
                    bytes: n,
                    content: Value::String(s),
                }
            }
        }
    }
}

/// `v`, or a marker with its start if it serialises to more than
/// [`MAX_ITEM_BYTES`].
pub fn capped(v: Value) -> Value {
    let text = v.to_string();
    if text.len() <= MAX_ITEM_BYTES {
        return v;
    }
    json!({
        "truncated": format!("{} bytes, over the {} KiB trace limit", text.len(), MAX_ITEM_BYTES / 1024),
        "start": cut(&text, 4096),
    })
}

/// The first `max` bytes of `s`, on a character boundary, marked if cut.
fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} …", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_shown_as_json_text_or_escaped_bytes() {
        let j = FrameView::of(br#"{"a":1}"#);
        assert_eq!((j.format, j.bytes, j.content), ("json", 7, json!({"a": 1})));
        let t = FrameView::of(b"!AIVDM,1");
        assert_eq!((t.format, t.content), ("text", json!("!AIVDM,1")));
        let b = FrameView::of(&[0x01, b'A', 0xff]);
        assert_eq!(
            (b.format, b.bytes, b.content),
            ("binary", 3, json!("\\x01A\\xff"))
        );
    }

    #[test]
    fn big_items_are_cut_to_a_marker() {
        let v = json!({"s": "x".repeat(MAX_ITEM_BYTES)});
        let c = capped(v);
        assert!(c["truncated"].as_str().unwrap().contains("64 KiB"), "{c}");
        assert!(c["start"].as_str().unwrap().len() < 5000);
        assert_eq!(capped(json!([1])), json!([1]));
    }
}
