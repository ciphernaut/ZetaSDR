//! Frames cut from a bit stream by a [`Framing`].
//!
//! Reads `slicer`'s `bits` events (after a `correlate`, or alone), forwards every event
//! it reads unchanged, and publishes one `frame` event per frame [`decode_frames`](crate::framing::decode_frames)
//! cuts. So it may sit before `bit_view`, as `correlate` does.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, Span, StreamId, StreamKind, Timestamp};

use crate::correlate::BitsIn;
use crate::framing::{Decoded, DecodedFrame, Framing, decode_from};
use crate::util::event;
use crate::{Input, is_seam, run_map_ending};

/// The stage kind, as graphs name it.
pub const FRAME_DECODE_KIND: &str = "frame_decode";

/// Most bits the stage holds while a frame is open; the oldest are dropped beyond it.
pub const BUFFER_BITS_MAX: usize = 1 << 20;

/// `frame_decode`'s parameters: the framing.
pub type FrameDecodeParams = Framing;

const NS_PER_S: f64 = 1e9;

/// One `bits` event's bits in the buffer: where they start and their clock.
struct Piece {
    /// The first bit's index among every bit the stage has buffered.
    abs: u64,
    t_ns: u64,
    bit_ns: f64,
    prov: Provenance,
}

/// A frame cut, with its time, provenance and the counts after it.
pub(crate) struct Found {
    pub frame: DecodedFrame,
    pub t_ns: u64,
    pub prov: Provenance,
    pub note: serde_json::Value,
}

/// The counts the stage notes.
#[derive(Clone, Copy, Default, PartialEq)]
struct Counts {
    frames: u64,
    crc_ok: u64,
    crc_bad: u64,
    sync_hits: u64,
    bad_length: u64,
    dropped_bits: u64,
}

impl Counts {
    fn note(&self, stream: &str) -> serde_json::Value {
        serde_json::json!({FRAME_DECODE_KIND: {
            "frames": self.frames,
            "crc_ok": self.crc_ok,
            "crc_bad": self.crc_bad,
            "sync_hits": self.sync_hits,
            "bad_length": self.bad_length,
            "dropped_bits": self.dropped_bits,
            "stream": stream,
        }})
    }
}

/// The decoder's streaming state over one segment of bits.
pub(crate) struct FrameState {
    f: Framing,
    /// The unsettled bits.
    buf: Vec<u8>,
    /// Every bit ever buffered: `buf` ends here.
    total: u64,
    pieces: VecDeque<Piece>,
    /// The previous `bits` event's end: its offset and time.
    end: Option<(u64, u64)>,
    /// Hits, frames and drops already counted lie below these, in buffered bits.
    hits_upto: u64,
    frames_upto: u64,
    bad_upto: u64,
    counts: Counts,
    /// The stream the notes name.
    stream: String,
    pub dropped_bits: u64,
    /// `buf` starts mid-segment (bits were settled or dropped before it): its first
    /// bit is line context, not a bit to hunt at.
    continued: bool,
}

impl FrameState {
    pub fn new(f: Framing) -> Self {
        Self {
            f,
            buf: Vec::new(),
            total: 0,
            pieces: VecDeque::new(),
            end: None,
            hits_upto: 0,
            frames_upto: 0,
            bad_upto: 0,
            counts: Counts::default(),
            stream: String::new(),
            dropped_bits: 0,
            continued: false,
        }
    }

    /// The bits held.
    #[cfg(test)]
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// The buffered index of `buf[0]`.
    fn base(&self) -> u64 {
        self.total - self.buf.len() as u64
    }

    /// The time of buffered bit `p`, from the piece holding it (extrapolated past the
    /// last).
    fn time(&self, p: u64) -> (u64, &Provenance) {
        let piece = self
            .pieces
            .iter()
            .rev()
            .find(|pc| pc.abs <= p)
            .or(self.pieces.front())
            .expect("a frame lies in a piece");
        let t = piece.t_ns + ((p - piece.abs.min(p)) as f64 * piece.bit_ns) as u64;
        (t, &piece.prov)
    }

    /// [`FrameState::feed_split`]'s frames cut after `b`'s bits alone.
    #[cfg(test)]
    pub fn feed(&mut self, b: BitsIn) -> Vec<Found> {
        self.feed_split(b).1
    }

    /// Append `b`'s bits and cut what can be cut: the frames a new segment closes (to
    /// come before `b`'s event) and those cut after `b`'s bits.
    fn feed_split(&mut self, b: BitsIn) -> (Vec<Found>, Vec<Found>) {
        let rate_hz = if b.bit_ns > 0.0 {
            (NS_PER_S / b.bit_ns).round() as u64
        } else {
            0
        };
        let before = match self.end {
            Some((off, t)) if off == b.offset_bits && !is_seam(t, b.t_ns, rate_hz) => Vec::new(),
            Some(_) => self.flush(),
            None => Vec::new(),
        };
        let t_end = b.time(b.bits.len());
        self.end = Some((b.end(), t_end));
        self.pieces.push_back(Piece {
            abs: self.total,
            t_ns: b.t_ns,
            bit_ns: b.bit_ns,
            prov: b.prov,
        });
        self.total += b.bits.len() as u64;
        self.buf.extend_from_slice(&b.bits);
        if self.buf.len() > BUFFER_BITS_MAX {
            let excess = self.buf.len() - BUFFER_BITS_MAX;
            self.buf.drain(..excess);
            self.continued = true;
            self.dropped_bits += excess as u64;
            self.counts.dropped_bits = self.dropped_bits;
        }
        let d = decode_from(&self.buf, &self.f, self.continued);
        let settled = d.settled;
        let after = self.take(d, false);
        self.buf.drain(..settled);
        self.continued |= settled > 0;
        let base = self.base();
        while self.pieces.len() > 1 && self.pieces[1].abs <= base {
            self.pieces.pop_front();
        }
        (before, after)
    }

    /// Close the segment: cut what the input's end closes (an open `to_next_sync`
    /// frame), count the frames whose rest never came as bad lengths, and clear.
    pub fn flush(&mut self) -> Vec<Found> {
        let found = if self.buf.is_empty() {
            Vec::new()
        } else {
            let d = decode_from(&self.buf, &self.f, self.continued);
            self.take(d, true)
        };
        self.buf.clear();
        self.continued = false;
        self.pieces.clear();
        self.end = None;
        found
    }

    /// Count `d`'s hits and drops and turn its frames into [`Found`]s, skipping what
    /// an earlier pass over the same bits counted; at the input's end the open frame
    /// is cut and the pending ones are dropped.
    fn take(&mut self, d: Decoded, at_end: bool) -> Vec<Found> {
        let base = self.base();
        for h in d.hits {
            let a = base + h as u64;
            if a >= self.hits_upto {
                self.counts.sync_hits += 1;
                self.hits_upto = a + 1;
            }
        }
        for b in d.bad {
            let a = base + b as u64;
            if a >= self.bad_upto {
                self.counts.bad_length += 1;
                self.bad_upto = a + 1;
            }
        }
        let mut frames = d.frames;
        if at_end {
            // A pending frame is never counted before the end, so each counts here.
            self.counts.bad_length += d.pending.len() as u64;
            match d.open {
                Some((_, Some(f))) => frames.push(f),
                Some((_, None)) => self.counts.bad_length += 1,
                None => {}
            }
        }
        let mut out = Vec::new();
        for fr in frames {
            let a = base + fr.at_bit as u64;
            if a < self.frames_upto {
                continue;
            }
            self.frames_upto = a + 1;
            self.counts.frames += 1;
            match fr.crc_ok {
                Some(true) => self.counts.crc_ok += 1,
                Some(false) => self.counts.crc_bad += 1,
                None => {}
            }
            let end = base + fr.end_bit(&self.f) as u64;
            let (t, prov) = self.time(a);
            let mut prov = prov.clone();
            prov.span = Span {
                start_ns: Timestamp(t),
                end_ns: Timestamp(self.time(end).0),
            };
            out.push(Found {
                frame: fr,
                t_ns: t,
                prov,
                note: self.counts.note(&self.stream),
            });
        }
        out
    }
}

/// The `frame` event of `found`, in the job's mode-row envelope when it runs one.
fn frame_event(ctx: &StageContext, sync: &str, found: &Found) -> serde_json::Value {
    let fr = &found.frame;
    let mut bytes = String::with_capacity(fr.bytes.len() * 2);
    for b in &fr.bytes {
        let _ = write!(bytes, "{b:02x}");
    }
    let n = fr.bytes.len();
    let crc = match fr.crc_ok {
        Some(true) => "ok",
        Some(false) => "bad",
        None => "none",
    };
    event(
        ctx,
        found.t_ns,
        "frame",
        serde_json::json!({
            "protocol": "frame",
            "t_ns": found.t_ns,
            "sync": sync,
            "length_bytes": n,
            "bytes": bytes,
            "crc_ok": fr.crc_ok,
            "payload": "opaque",
            "summary": format!("frame {n} B, crc {crc}"),
        }),
    )
}

/// Cuts frames from `bits` events by the framing in its params and publishes one
/// `frame` event per frame (`t_ns` the sync's first bit, `sync`, `length_bytes`,
/// `bytes` in hex, `crc_ok`, `payload: "opaque"`, `summary`; the frame's span as
/// provenance), in the row's envelope when the job runs a mode row. The bits of one
/// segment are buffered until settled; a new segment (an `offset_bits` that is not the
/// previous event's end), a time seam or a lag closes the segment (cutting an open
/// `to_next_sync` frame) and clears the buffer, as the input's end does. The buffer
/// holds at most [`BUFFER_BITS_MAX`] bits, the oldest dropped and counted. Every event
/// read is forwarded unchanged, the frames its bits close right after it. Notes
/// `frame_decode` (`frames`, `crc_ok`, `crc_bad`, `sync_hits`, `bad_length`,
/// `dropped_bits`, `stream` its output) after each frame, at the first `bits` event
/// and whenever a count changes.
pub struct FrameDecodeStage {
    spec: StageSpec,
}

impl FrameDecodeStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: FRAME_DECODE_KIND,
                inputs: vec![StreamKind::Event],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(FrameDecodeParams),
            },
        }
    }
}

impl Default for FrameDecodeStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for FrameDecodeStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: FrameDecodeParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        p.check()
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        self.check_params(&params).map_err(StageError::Failed)?;
        let f: FrameDecodeParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{FRAME_DECODE_KIND} params: {e}")))?;
        let [events] = <[StreamId; 1]>::try_from(outputs)
            .map_err(|o| StageError::Failed(format!("expected 1 output, got {}", o.len())))?;
        let sync = f.sync.hex.to_ascii_lowercase();
        let mut state = FrameState::new(f);
        state.stream = events.0.clone();
        let mut noted: Option<Counts> = None;
        run_map_ending(FRAME_DECODE_KIND, inputs, bus, cancel, move |input| {
            let mut out = Vec::new();
            let publish = |found: Vec<Found>, out: &mut Vec<Frame>| {
                for fd in found {
                    let ev = frame_event(&ctx, &sync, &fd);
                    out.push(Frame::new(events.clone(), fd.prov, Payload::Event(ev)));
                    ctx.note(fd.note);
                }
            };
            let mut bits_seen = false;
            match input {
                None | Some(Input::Lagged { .. }) => publish(state.flush(), &mut out),
                Some(Input::Frame(frame)) => {
                    let forward = Frame::new(
                        events.clone(),
                        frame.provenance.clone(),
                        frame.payload.clone(),
                    );
                    match BitsIn::parse(FRAME_DECODE_KIND, frame)? {
                        None => out.push(forward),
                        Some(b) => {
                            bits_seen = true;
                            let (before, after) = state.feed_split(b);
                            publish(before, &mut out);
                            out.push(forward);
                            publish(after, &mut out);
                        }
                    }
                }
            }
            let counts = state.counts;
            if noted != Some(counts) && (bits_seen || noted.is_some()) {
                ctx.note(counts.note(&state.stream));
                noted = Some(counts);
            }
            Ok(out)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use zeta_core::stage::Stage;
    use zeta_core::stream::{Frame, Payload};
    use zeta_core::types::{JobId, Provenance, SourceId, Span, StreamId, Timestamp};

    use super::{BUFFER_BITS_MAX, FrameDecodeStage, FrameState};
    use crate::bit_view::test_streams::{BAUD, T0, bits_frames, bits_frames_at, run_events};
    use crate::correlate::BitsIn;
    use crate::framing::test_support::{bodies, bodies_of, fx};
    use crate::framing::{Length, LineCode, decode_frames, encode_frames};

    const SECOND_NS: u64 = 1_000_000_000;

    fn frames_of(events: &[Value]) -> Vec<Value> {
        events
            .iter()
            .filter(|e| e["protocol"] == "frame")
            .cloned()
            .collect()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The time of bit `k` of a segment from `t0`, as `bits_frames` stamps it.
    fn bit_time(t0: u64, k: usize) -> u64 {
        t0 + (k as u128 * SECOND_NS as u128 / BAUD as u128) as u64
    }

    #[tokio::test]
    async fn frames_are_decoded_across_split_bits_events() {
        let f = fx("2dd4", 16, Length::Fixed { fixed_bits: 272 });
        let bs = bodies(8, 32, 11);
        let bits = encode_frames(&bs, &f);
        const PERIOD: usize = 16 + 272;
        let splits = [2 * PERIOD + 8, 5 * PERIOD + 100, 6 * PERIOD + 3];
        let (events, context) = run_events(
            &FrameDecodeStage::new(),
            serde_json::to_value(&f).unwrap(),
            bits_frames(&bits, &splits),
        )
        .await;
        let frames = frames_of(&events);
        assert_eq!(frames.len(), 8, "{events:?}");
        for (k, (fr, b)) in frames.iter().zip(&bs).enumerate() {
            assert_eq!(fr["crc_ok"], true);
            assert_eq!(fr["sync"], "2dd4");
            assert_eq!(fr["length_bytes"], 34);
            assert_eq!(fr["payload"], "opaque");
            assert_eq!(fr["summary"], "frame 34 B, crc ok");
            assert!(fr["bytes"].as_str().unwrap().starts_with(&hex(b)));
            let t = fr["t_ns"].as_u64().unwrap();
            assert!(
                t.abs_diff(bit_time(T0, k * PERIOD)) <= 1,
                "frame {k} at {t}"
            );
        }
        let note = &context["frame_decode"];
        assert_eq!(note["frames"], 8);
        assert_eq!(note["crc_ok"], 8);
        assert_eq!(note["crc_bad"], 0);
        assert_eq!(note["sync_hits"], 8);
        assert_eq!(note["bad_length"], 0);
        assert_eq!(note["dropped_bits"], 0);
        assert!(note["stream"].is_string());
        // Every bits event read comes out too.
        assert_eq!(
            events.iter().filter(|e| e["protocol"] == "bits").count(),
            splits.len() + 1
        );
    }

    #[tokio::test]
    async fn nrzi_frames_are_decoded_across_events_split_at_frame_ends() {
        // A run-heavy sync admitting one error: the last bit of a frame and the sync's
        // first seven bits are within one error of it, so a continuation that hunted
        // the context bit it keeps would cut a garbage frame there.
        for enc in [LineCode::Nrzi, LineCode::Differential] {
            let mut f = fx("7f", 8, Length::Fixed { fixed_bits: 272 });
            f.encoding = enc;
            f.max_errors = 1;
            let bs = bodies(8, 32, 15);
            let bits = encode_frames(&bs, &f);
            const PERIOD: usize = 8 + 272;
            assert_eq!(decode_frames(&bits, &f).0.len(), 8, "{enc:?} batch");
            let splits: Vec<usize> = (1..8).map(|k| k * PERIOD).collect();
            let (events, context) = run_events(
                &FrameDecodeStage::new(),
                serde_json::to_value(&f).unwrap(),
                bits_frames(&bits, &splits),
            )
            .await;
            let frames = frames_of(&events);
            assert_eq!(frames.len(), 8, "{enc:?}: {frames:?}");
            for (fr, b) in frames.iter().zip(&bs) {
                assert_eq!(fr["crc_ok"], true, "{enc:?}");
                assert!(fr["bytes"].as_str().unwrap().starts_with(&hex(b)));
            }
            assert_eq!(context["frame_decode"]["sync_hits"], 8, "{enc:?}");
        }
    }

    #[tokio::test]
    async fn a_seam_resets_the_buffer_and_other_events_pass_through() {
        let f = fx("2dd4", 16, Length::Fixed { fixed_bits: 272 });
        let mut first = encode_frames(&bodies(3, 32, 12), &f);
        // The first segment ends 50 bits into its third frame.
        first.truncate(2 * 288 + 50);
        let second = encode_frames(&bodies(3, 32, 13), &f);
        let t1 = T0 + 10 * SECOND_NS;
        let segment = json!({"protocol": "segment", "t_ns": t1});
        let mut input = bits_frames(&first, &[300]);
        input.push(Frame::new(
            StreamId::new(),
            Provenance {
                source: SourceId("s".into()),
                job: Some(JobId("job".into())),
                channel: None,
                span: Span {
                    start_ns: Timestamp(t1),
                    end_ns: Timestamp(t1),
                },
            },
            Payload::Event(segment.clone()),
        ));
        input.extend(bits_frames_at(&second, &[400], t1));
        let (events, context) = run_events(
            &FrameDecodeStage::new(),
            serde_json::to_value(&f).unwrap(),
            input,
        )
        .await;
        let frames = frames_of(&events);
        assert_eq!(frames.len(), 5, "{frames:?}");
        assert!(frames.iter().all(|fr| fr["crc_ok"] == true));
        let t: Vec<u64> = frames
            .iter()
            .map(|fr| fr["t_ns"].as_u64().unwrap())
            .collect();
        assert!(t[1] < t1 && t[2] >= t1, "{t:?}");
        assert_eq!(events.iter().filter(|e| **e == segment).count(), 1);
        // The cut third frame of the first segment is a bad length, not a frame.
        assert_eq!(context["frame_decode"]["bad_length"], 1);
        assert_eq!(context["frame_decode"]["frames"], 5);
    }

    #[tokio::test]
    async fn to_next_sync_closes_on_the_next_sync_and_at_the_end() {
        let f = fx("1acffc1d", 32, Length::ToNextSync { to_next_sync: true });
        let bs = bodies_of(&[10, 30, 20], 14);
        let bits = encode_frames(&bs, &f);
        let start3 = 2 * 32 + 8 * (10 + 2 + 30 + 2);
        let (events, _) = run_events(
            &FrameDecodeStage::new(),
            serde_json::to_value(&f).unwrap(),
            bits_frames(&bits, &[start3 + 40]),
        )
        .await;
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| e["protocol"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["bits", "frame", "frame", "bits", "frame"]);
        let frames = frames_of(&events);
        for (fr, b) in frames.iter().zip(&bs) {
            assert_eq!(fr["crc_ok"], true);
            assert_eq!(fr["length_bytes"], b.len() + 2);
            assert!(fr["bytes"].as_str().unwrap().starts_with(&hex(b)));
        }
    }

    #[tokio::test]
    async fn an_unclosed_to_next_sync_frame_is_bounded() {
        let f = fx("2dd4", 16, Length::ToNextSync { to_next_sync: true });
        let mut bits = crate::slicer::bits_from_hex("2dd4", 16);
        bits.extend(std::iter::repeat_n(0u8, 2_000_000));
        let splits: Vec<usize> = (1..bits.len() / 65_536).map(|k| k * 65_536).collect();
        let input = bits_frames(&bits, &splits);
        let mut state = FrameState::new(f.clone());
        for fr in &input {
            let b = BitsIn::parse("frame_decode", fr.clone()).unwrap().unwrap();
            assert!(state.feed(b).is_empty());
            assert!(state.buffered() <= BUFFER_BITS_MAX);
        }
        assert!(state.dropped_bits > 0);
        let (events, context) = run_events(
            &FrameDecodeStage::new(),
            serde_json::to_value(&f).unwrap(),
            input,
        )
        .await;
        assert!(frames_of(&events).is_empty());
        let note = &context["frame_decode"];
        assert!(note["dropped_bits"].as_u64().unwrap() > 0, "{note}");
        assert_eq!(note["frames"], 0);
    }

    #[test]
    fn stage_params_are_the_framing_struct() {
        let s = FrameDecodeStage::new();
        assert_eq!(
            s.check_params(
                &json!({"sync": {"hex": "2dd4", "bits": 16}, "length": {"fixed_bits": 272}})
            ),
            Ok(())
        );
        let e = s.check_params(&json!({})).unwrap_err();
        assert!(e.contains("sync"), "{e}");
    }
}
