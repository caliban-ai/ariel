//! Server-sent events decoding for prospero's per-agent stream.
//!
//! [`FrameDecoder`] turns arbitrary byte chunks into dispatched SSE frames,
//! following the WHATWG event-stream rules Ariel needs: `event` and `data`
//! fields, comment lines (prospero's keepalive), CRLF or LF line endings, and
//! chunks that split a line or a UTF-8 sequence. [`StreamItem::from_frame`]
//! then interprets a frame as a fleet event or a gap signal.

use super::types::{FleetEvent, GapSignal};

/// One dispatched SSE frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The `event:` name, if any. Unnamed frames are `message` events.
    pub event: Option<String>,
    /// The `data:` lines, joined with `\n`.
    pub data: String,
}

/// Incremental SSE decoder. Feed it chunks in order with [`push`](Self::push).
#[derive(Debug, Default)]
pub struct FrameDecoder {
    /// Bytes of an incomplete trailing line.
    pending: Vec<u8>,
    event: Option<String>,
    data: String,
    has_data: bool,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume a chunk, returning every frame it completes.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Frame> {
        self.pending.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(frame) = self.line(&String::from_utf8_lossy(&line)) {
                frames.push(frame);
            }
        }
        frames
    }

    fn line(&mut self, line: &str) -> Option<Frame> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            // `id` and `retry` carry nothing prospero uses; unknown fields are
            // ignored per the event-stream rules.
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<Frame> {
        let event = self.event.take();
        if !self.has_data {
            return None;
        }
        self.has_data = false;
        Some(Frame {
            event,
            data: std::mem::take(&mut self.data),
        })
    }
}

/// A decoded frame from an agent stream.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamItem {
    Event(FleetEvent),
    Gap(GapSignal),
}

impl StreamItem {
    /// Interpret a frame. `Ok(None)` for event names Ariel does not know, which
    /// are skipped so a newer prosperod can add frame types.
    pub fn from_frame(frame: &Frame) -> Result<Option<Self>, serde_json::Error> {
        match frame.event.as_deref() {
            None | Some("message") => {
                serde_json::from_str(&frame.data).map(|e| Some(Self::Event(e)))
            }
            Some("gap") => serde_json::from_str(&frame.data).map(|g| Some(Self::Gap(g))),
            Some(_) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(event: Option<&str>, data: &str) -> Frame {
        Frame {
            event: event.map(str::to_owned),
            data: data.to_owned(),
        }
    }

    #[test]
    fn decodes_unnamed_and_named_frames() {
        let mut d = FrameDecoder::new();
        let frames = d.push(b"data: {\"a\":1}\n\nevent: gap\ndata: {\"b\":2}\n\n");
        assert_eq!(
            frames,
            [frame(None, "{\"a\":1}"), frame(Some("gap"), "{\"b\":2}")]
        );
    }

    #[test]
    fn keepalive_comments_produce_nothing() {
        let mut d = FrameDecoder::new();
        assert!(d.push(b":\n\n: ping\n\n").is_empty());
    }

    #[test]
    fn frames_split_across_chunks_and_crlf() {
        let mut d = FrameDecoder::new();
        assert!(d.push(b"da").is_empty());
        assert!(d.push(b"ta: x\r").is_empty());
        assert!(d.push(b"\n\r").is_empty());
        assert_eq!(d.push(b"\n"), [frame(None, "x")]);
    }

    #[test]
    fn utf8_sequence_split_across_chunks() {
        let bytes = "data: café\n\n".as_bytes();
        let split = bytes.iter().position(|&b| b == 0xc3).unwrap() + 1;
        let mut d = FrameDecoder::new();
        assert!(d.push(&bytes[..split]).is_empty());
        assert_eq!(d.push(&bytes[split..]), [frame(None, "café")]);
    }

    #[test]
    fn multiline_data_joins_with_newlines() {
        let mut d = FrameDecoder::new();
        assert_eq!(
            d.push(b"data: a\ndata:b\ndata\n\n"),
            [frame(None, "a\nb\n")]
        );
    }

    #[test]
    fn event_name_without_data_is_discarded() {
        let mut d = FrameDecoder::new();
        assert!(d.push(b"event: gap\n\n").is_empty());
        assert_eq!(d.push(b"data: x\n\n"), [frame(None, "x")]);
    }

    #[test]
    fn unknown_frame_names_are_skipped() {
        assert_eq!(
            StreamItem::from_frame(&frame(Some("hello"), "{}")).unwrap(),
            None
        );
    }

    #[test]
    fn gap_frame_decodes_to_gap_signal() {
        let item = StreamItem::from_frame(&frame(Some("gap"), r#"{"skipped":7,"last_seq":2}"#));
        assert_eq!(
            item.unwrap(),
            Some(StreamItem::Gap(GapSignal {
                skipped: 7,
                last_seq: 2
            }))
        );
    }

    #[test]
    fn malformed_event_data_is_an_error() {
        assert!(StreamItem::from_frame(&frame(None, "not json")).is_err());
    }
}
