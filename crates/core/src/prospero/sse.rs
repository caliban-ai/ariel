//! Server-sent events decoding for prospero's event streams.
//!
//! [`FrameDecoder`] turns arbitrary byte chunks into dispatched SSE frames,
//! following the WHATWG event-stream rules Ariel needs: `event`, `data` and
//! `id` fields, comment lines (prospero's keepalive), CRLF or LF line endings,
//! and chunks that split a line or a UTF-8 sequence. [`StreamItem::from_frame`]
//! then interprets a frame from the per-agent stream as a fleet event or a gap
//! signal, and [`CursoredEvent::from_frame`] reads a fleet-stream frame as an
//! event with the cursor that resumes after it.

use super::types::{FleetEvent, GapSignal};

/// One dispatched SSE frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The `event:` name, if any. Unnamed frames are `message` events.
    pub event: Option<String>,
    /// The `data:` lines, joined with `\n`.
    pub data: String,
    /// The last `id:` seen at dispatch. On the fleet stream this is the
    /// **fleet cursor** a client hands back as `?from=` to resume
    /// (prospero#219); the per-agent stream sets no `id:` and leaves it `None`.
    pub id: Option<String>,
}

/// Incremental SSE decoder. Feed it chunks in order with [`push`](Self::push).
#[derive(Debug, Default)]
pub struct FrameDecoder {
    /// Bytes of an incomplete trailing line.
    pending: Vec<u8>,
    event: Option<String>,
    data: String,
    has_data: bool,
    /// The last event ID. Unlike `event` and `data` this survives dispatch, as
    /// the event-stream rules require.
    last_id: Option<String>,
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
            "id" => self.last_id = Some(value.to_owned()),
            // `retry` carries nothing prospero uses; unknown fields are ignored
            // per the event-stream rules.
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
            id: self.last_id.clone(),
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

/// One event from the fleet-wide stream, with the cursor that resumes after it.
#[derive(Debug, Clone, PartialEq)]
pub struct CursoredEvent {
    /// The fleet cursor the event arrived with, from the SSE `id:` field.
    ///
    /// `None` only if prosperod sent a frame with no usable id, which it does
    /// not do. The event is still delivered, and a reconnect resumes from the
    /// last cursor that had one — never losing an event to a missing cursor.
    pub cursor: Option<u64>,
    pub event: FleetEvent,
}

impl CursoredEvent {
    /// Interpret a fleet-stream frame. `Ok(None)` for event names Ariel does
    /// not know, which are skipped so a newer prosperod can add frame types.
    ///
    /// The fleet stream reads from the durable store rather than the live bus,
    /// so it has no `gap` frame to interpret: there is nothing to miss.
    pub fn from_frame(frame: &Frame) -> Result<Option<Self>, serde_json::Error> {
        match frame.event.as_deref() {
            None | Some("message") => Ok(Some(Self {
                cursor: frame.id.as_deref().and_then(|id| id.parse().ok()),
                event: serde_json::from_str(&frame.data)?,
            })),
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
            id: None,
        }
    }

    fn with_id(id: &str, event: Option<&str>, data: &str) -> Frame {
        Frame {
            id: Some(id.to_owned()),
            ..frame(event, data)
        }
    }

    #[test]
    fn id_field_is_captured() {
        let mut d = FrameDecoder::new();
        assert_eq!(
            d.push(b"id: 4821\ndata: x\n\n"),
            [with_id("4821", None, "x")]
        );
    }

    #[test]
    fn last_event_id_persists_until_replaced() {
        // WHATWG keeps the last event ID across dispatches, unlike `data` and
        // `event`. prosperod sets `id:` on every fleet event, so this only
        // decides what an id-less frame inherits — the cursor already reached.
        let mut d = FrameDecoder::new();
        assert_eq!(d.push(b"id: 7\ndata: a\n\n"), [with_id("7", None, "a")]);
        assert_eq!(d.push(b"data: b\n\n"), [with_id("7", None, "b")]);
        assert_eq!(d.push(b"id: 9\ndata: c\n\n"), [with_id("9", None, "c")]);
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

    const SPAWNED: &str = r#"{"seq":1,"ts":"2026-06-18T00:00:00+00:00","repo":"caliban","agent_id":"a1","kind":{"kind":"agent_spawned"}}"#;

    #[test]
    fn cursored_event_reads_the_id_as_its_cursor() {
        let item = CursoredEvent::from_frame(&with_id("4821", None, SPAWNED))
            .unwrap()
            .unwrap();
        assert_eq!(item.cursor, Some(4821));
        assert_eq!(item.event.agent_id, "a1");
    }

    #[test]
    fn cursored_event_without_a_numeric_id_still_delivers_the_event() {
        // The cursor is how a reconnect resumes; losing it must not lose the
        // event, so the watcher keeps the position it already had.
        for id in [None, Some("not-a-number")] {
            let frame = match id {
                Some(id) => with_id(id, None, SPAWNED),
                None => frame(None, SPAWNED),
            };
            let item = CursoredEvent::from_frame(&frame).unwrap().unwrap();
            assert_eq!(item.cursor, None, "id {id:?}");
            assert_eq!(item.event.agent_id, "a1");
        }
    }

    #[test]
    fn cursored_unknown_frame_names_are_skipped() {
        let frame = with_id("1", Some("something-new"), "{}");
        assert_eq!(CursoredEvent::from_frame(&frame).unwrap(), None);
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
