//! M17 decode stage (design 7): 4FSK symbols from a channel's IQ, the sync bursts, the
//! LSF (whole, or rebuilt from the six LICH chunks of the stream frames), two events per
//! stream (one when the LSF is in hand, one at the end), and the stream's Codec 2 speech
//! through the external `c2dec` (the row's handler 0) as ordinary job audio.
//!
//! The receiver: the FM discriminator ([`nbfm_demod`], scaled so the outer symbols sit at
//! ±1), a root-raised-cosine matched filter (completing the transmitter's raised cosine),
//! then a sync hunt sample by sample: a sync burst's 8 symbols sliced at each candidate
//! position, at most [`M17_SYNC_MAX_ERRORS`] wrong, the timing then set at the best
//! correlation within half a symbol. Once a frame is found, the next sync burst is looked
//! for 192 symbols on (within [`LOCK_SEARCH_SAMPLES`]), which keeps the symbol clock. A
//! stream frame found by the hunt counts only when its LICH decodes and the next sync
//! burst confirms it; an LSF counts when its CRC holds. The codes are [`crate::m17`]'s.
//!
//! The vocoder: `c2dec 3200 - -` (voice) or `c2dec 1600 - -` (voice and data, the 8 data
//! bytes of each frame dropped), spawned per stream with an empty environment. Each
//! stream frame's Codec 2 bytes go to its stdin; its 16-bit 8 kHz speech is resampled to
//! 48 kHz and published on the stage's audio output, one frame of audio per stream frame
//! with that frame's span. An encrypted stream feeds it nothing. Its exit fails nothing but
//! that stream's audio: the events go on, `job_context.m17.vocoder` says why once, and the
//! next LSF starts it again.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, Span, StreamId, StreamKind, Timestamp};

use crate::m17::{
    DATA_TYPE_VOICE, DATA_TYPE_VOICE_DATA, ENCRYPTION_NONE, EOT, FRAME_BITS, FRAME_SYMBOLS,
    LICH_CHUNK_BYTES, LICH_CHUNKS, Lsf, META_SUBTYPE_GNSS, STREAM_PAYLOAD_BYTES, SYNC_LSF,
    SYNC_PACKET, SYNC_STREAM, SYNC_SYMBOLS, StreamFrame, decode_lsf_frame, decode_stream_frame,
    encryption_name, gnss_position, lich_assemble, symbols_to_bits, type_name, word_symbols,
};
use crate::nbfm::nbfm_demod;
use crate::resample::Resampler;
use crate::synth::{M17_BAUD, M17_DEVIATION_HZ, M17_RRC_ALPHA, RRC_SPAN_SYMBOLS, rrc};
use crate::util::{StreamingFir, event};
use crate::{Input, run_map_ending, sps_check};

/// The stage's kind and the row's id.
pub const M17_DECODE_KIND: &str = "m17_decode";
/// The discriminator is scaled for this many times [`M17_DEVIATION_HZ`]: the
/// root-raised-cosine pulses overshoot the outer tones, and [`nbfm_demod`] clips at its
/// deviation.
pub const M17_DISCRIMINATOR_HEADROOM: u64 = 2;
/// Most wrong symbols a sync burst may have (of its 8).
pub const M17_SYNC_MAX_ERRORS: usize = 1;
/// Samples either side of the expected position the next sync burst is looked for.
pub const LOCK_SEARCH_SAMPLES: i64 = 2;
/// Frames without a sync burst after which a stream that lost its lock is over.
pub const M17_TRACK_TIMEOUT_FRAMES: u64 = 3;
/// Codec 2's speech rate, Hz.
pub const C2_RATE_HZ: u64 = 8_000;
/// The rate of the stage's audio, Hz.
pub const AUDIO_RATE_HZ: u64 = 48_000;
/// The vocoder's program (the row's handler 0).
pub const C2DEC: &str = "c2dec";
/// How long the vocoders of a finished input may take to hand back their speech.
pub const VOCODER_DRAIN: Duration = Duration::from_secs(2);
/// One frame (192 symbols at 4800 baud), ns.
const FRAME_NS: u64 = FRAME_SYMBOLS as u64 * 1_000_000_000 / M17_BAUD as u64;
/// Codec 2 speech samples per stream frame (40 ms at 8 kHz).
const C2_FRAME_SAMPLES: usize = C2_RATE_HZ as usize * FRAME_SYMBOLS / M17_BAUD as usize;
/// The bytes of a stream frame's payload that are speech at 1600 bps (one Codec 2 frame;
/// the rest is data).
const C2_1600_BYTES: usize = 8;

/// `m17_decode` takes no parameters.
#[derive(Default, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct M17Params {}

/// What the receiver hands on.
#[derive(Clone, Debug, PartialEq)]
pub enum M17Frame {
    /// The stream's LSF: from an LSF frame, or rebuilt from the LICH (then just before
    /// the stream frame that completed it).
    Lsf(Lsf),
    /// A stream frame.
    Stream {
        frame_number: u16,
        last: bool,
        payload: [u8; STREAM_PAYLOAD_BYTES],
    },
    /// The end-of-transmission marker, or a stream lost for
    /// [`M17_TRACK_TIMEOUT_FRAMES`] frames.
    Eot,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Lsf,
    Stream,
    Packet,
    Eot,
}

impl Kind {
    /// The sync burst of a frame of this kind.
    fn word(self) -> u16 {
        match self {
            Kind::Lsf => SYNC_LSF,
            Kind::Stream => SYNC_STREAM,
            Kind::Packet => SYNC_PACKET,
            Kind::Eot => EOT,
        }
    }
}

/// The sync bursts the hunt looks for.
const HUNT_WORDS: [(u16, Kind); 2] = [(SYNC_LSF, Kind::Lsf), (SYNC_STREAM, Kind::Stream)];
/// The bursts that may follow a frame.
const LOCK_WORDS: [(u16, Kind); 4] = [
    (SYNC_STREAM, Kind::Stream),
    (EOT, Kind::Eot),
    (SYNC_LSF, Kind::Lsf),
    (SYNC_PACKET, Kind::Packet),
];

/// The receiver's place in the signal.
enum State {
    /// Trying each sample position from `at` as a sync burst's first symbol.
    Hunt { at: u64 },
    /// A sync burst of `kind` whose first symbol is at `at`; its frame waits for its
    /// samples. `locked` when it followed a frame, `pending` a stream frame found by the
    /// hunt that this burst confirms.
    Frame {
        at: f64,
        kind: Kind,
        locked: bool,
        pending: Option<(f64, StreamFrame)>,
    },
}

/// The stream being received: whether its LSF was handed on, and its LICH chunks.
struct Track {
    lsf: bool,
    chunks: [Option<[u8; LICH_CHUNK_BYTES]>; LICH_CHUNKS],
}

/// A streaming M17 receiver over one channel's IQ; state carries across
/// [`feed`](Self::feed) calls.
pub struct M17Demod {
    rate_hz: u64,
    sps: f64,
    prev: Complex<f32>,
    filter: StreamingFir,
    /// The matched filter's delay, samples.
    delay: f64,
    /// Filtered discriminator output from absolute sample `base`.
    buf: Vec<f32>,
    base: u64,
    state: State,
    track: Option<Track>,
    /// Where a locked stream lost its sync bursts.
    lost_at: Option<f64>,
}

impl M17Demod {
    /// A receiver for IQ at `rate_hz`; refused under
    /// [`crate::MIN_SAMPLES_PER_SYMBOL`] samples per symbol, naming the rate.
    pub fn new(rate_hz: u64) -> Result<Self, String> {
        sps_check(M17_DECODE_KIND, rate_hz, M17_BAUD)?;
        let sps = rate_hz as f64 / f64::from(M17_BAUD);
        let reach = (RRC_SPAN_SYMBOLS / 2.0 * sps).floor() as i64;
        let taps: Vec<f64> = (-reach..=reach)
            .map(|j| rrc(j as f64 / sps, M17_RRC_ALPHA))
            .collect();
        let sum: f64 = taps.iter().sum();
        Ok(Self {
            rate_hz,
            sps,
            prev: Complex::new(0.0, 0.0),
            filter: StreamingFir::new(taps.iter().map(|t| (t / sum) as f32).collect()),
            delay: reach as f64,
            buf: Vec::new(),
            base: 0,
            state: State::Hunt { at: 0 },
            track: None,
            lost_at: None,
        })
    }

    /// The frames `iq` completes.
    pub fn feed(&mut self, iq: &[Complex<f32>]) -> Vec<M17Frame> {
        self.feed_timed(iq).into_iter().map(|(_, f)| f).collect()
    }

    /// The frames `iq` completes, each with the sample its frame starts at, counted from
    /// the start of `iq` (negative for one that started in an earlier call).
    pub fn feed_timed(&mut self, iq: &[Complex<f32>]) -> Vec<(i64, M17Frame)> {
        let start = self.end();
        let gain = M17_DISCRIMINATOR_HEADROOM as f32;
        let mut audio = nbfm_demod(
            iq,
            self.rate_hz,
            M17_DEVIATION_HZ as u64 * M17_DISCRIMINATOR_HEADROOM,
            &mut self.prev,
        );
        audio.iter_mut().for_each(|x| *x *= gain);
        self.buf.extend(self.filter.process(&audio));
        let mut out = Vec::new();
        while self.step(&mut out) {}
        self.trim();
        out.into_iter()
            .map(|(at, f)| {
                // A frame starts half a symbol before its first symbol's centre, the
                // matched filter's delay earlier.
                let s = at - self.sps / 2.0 - self.delay;
                (s.round() as i64 - start as i64, f)
            })
            .collect()
    }

    /// One past the last sample held.
    fn end(&self) -> u64 {
        self.base + self.buf.len() as u64
    }

    /// Whether the sample at `pos` (and the one after, for interpolation) is held.
    fn has(&self, pos: f64) -> bool {
        pos >= self.base as f64 && (pos.floor() as u64) + 1 < self.end()
    }

    /// The filtered discriminator at `pos`, linearly interpolated.
    fn y(&self, pos: f64) -> f32 {
        let i = (pos.floor() as u64 - self.base) as usize;
        let f = (pos - pos.floor()) as f32;
        self.buf[i] * (1.0 - f) + self.buf[i + 1] * f
    }

    /// The level of symbol `s` (0..=3 for -3, -1, +1, +3), outer levels at ±1.
    fn level(s: u8) -> f32 {
        (f32::from(s) * 2.0 - 3.0) / 3.0
    }

    /// The symbol `y` slices to, `offset` taken off first.
    fn slice(y: f32, offset: f32) -> u8 {
        let v = y - offset;
        if v < -2.0 / 3.0 {
            0
        } else if v < 0.0 {
            1
        } else if v < 2.0 / 3.0 {
            2
        } else {
            3
        }
    }

    /// The carrier offset a burst of `word` starting at `at` measures (every sample
    /// held): the mean of its symbols' distance from their levels.
    fn offset(&self, at: f64, word: u16) -> f32 {
        word_symbols(word)
            .iter()
            .enumerate()
            .map(|(k, &s)| self.y(at + k as f64 * self.sps) - Self::level(s))
            .sum::<f32>()
            / SYNC_SYMBOLS as f32
    }

    /// The wrong symbols and the correlation of `word`'s burst starting at `at` (every
    /// sample held), the burst's own offset taken off.
    fn burst(&self, at: f64, word: u16) -> (usize, f32) {
        let offset = self.offset(at, word);
        word_symbols(word)
            .iter()
            .enumerate()
            .fold((0, 0.0), |(errors, corr), (k, &s)| {
                let y = self.y(at + k as f64 * self.sps);
                (
                    errors + usize::from(Self::slice(y, offset) != s),
                    corr + (y - offset) * Self::level(s),
                )
            })
    }

    /// Advance as far as the samples held allow; false when more are needed.
    fn step(&mut self, out: &mut Vec<(f64, M17Frame)>) -> bool {
        let burst_len = (SYNC_SYMBOLS - 1) as f64 * self.sps;
        match std::mem::replace(&mut self.state, State::Hunt { at: 0 }) {
            State::Hunt { at } => {
                let half = (self.sps / 2.0).ceil();
                let p = at.max(self.base) as f64;
                if !self.has(p + burst_len + half) {
                    self.state = State::Hunt { at: p as u64 };
                    return false;
                }
                self.expire_track(p, out);
                for (word, kind) in HUNT_WORDS {
                    if self.burst(p, word).0 <= M17_SYNC_MAX_ERRORS {
                        let best = (0..=half as u64)
                            .map(|d| p + d as f64)
                            .map(|q| (q, self.burst(q, word)))
                            .max_by(|a, b| a.1.1.total_cmp(&b.1.1))
                            .expect("one position");
                        if best.1.0 <= M17_SYNC_MAX_ERRORS {
                            self.state = State::Frame {
                                at: best.0,
                                kind,
                                locked: false,
                                pending: None,
                            };
                            return true;
                        }
                    }
                }
                self.state = State::Hunt { at: p as u64 + 1 };
                true
            }
            State::Frame {
                at,
                kind,
                locked,
                pending,
            } => {
                let next = at + FRAME_SYMBOLS as f64 * self.sps;
                let search = LOCK_SEARCH_SAMPLES as f64;
                if kind == Kind::Eot {
                    // The marker (only ever found after a frame) needs none of the
                    // samples after its burst.
                    self.track = None;
                    out.push((at, M17Frame::Eot));
                    self.lost_at = None;
                    self.state = State::Hunt {
                        at: (next - search).max(0.0) as u64,
                    };
                    return true;
                }
                if !self.has(next + search + burst_len) {
                    self.state = State::Frame {
                        at,
                        kind,
                        locked,
                        pending,
                    };
                    return false;
                }
                if let Some((t, f)) = pending {
                    // The hunt's stream frame, confirmed by this burst.
                    self.on_stream(t, f, out);
                }
                let resume = State::Hunt {
                    at: at.floor() as u64 + 1,
                };
                let bits = self.frame_bits(at, kind.word());
                let mut held = None;
                match kind {
                    Kind::Lsf => match decode_lsf_frame(&bits).and_then(|b| Lsf::parse(&b)) {
                        Some(lsf) => {
                            if self.track.take().is_some() {
                                out.push((at, M17Frame::Eot));
                            }
                            self.track = Some(Track {
                                lsf: true,
                                chunks: Default::default(),
                            });
                            self.lost_at = None;
                            out.push((at, M17Frame::Lsf(lsf)));
                        }
                        None => {
                            self.lose(at, locked);
                            self.state = resume;
                            return true;
                        }
                    },
                    Kind::Stream => {
                        let f = decode_stream_frame(&bits);
                        if locked {
                            self.on_stream(at, f, out);
                        } else if f.lich.is_some() {
                            held = Some((at, f));
                        } else {
                            self.state = resume;
                            return true;
                        }
                    }
                    Kind::Packet => {}
                    Kind::Eot => unreachable!("handled before the frame's samples"),
                }
                // The burst after this frame.
                let found = (-LOCK_SEARCH_SAMPLES..=LOCK_SEARCH_SAMPLES)
                    .map(|d| next + d as f64)
                    .flat_map(|q| LOCK_WORDS.map(|(w, k)| (q, k, self.burst(q, w))))
                    .filter(|(_, _, (e, _))| *e <= M17_SYNC_MAX_ERRORS)
                    .max_by(|a, b| a.2.1.total_cmp(&b.2.1));
                self.state = match found {
                    Some((q, k, _)) => State::Frame {
                        at: q,
                        kind: k,
                        locked: true,
                        pending: held,
                    },
                    None => {
                        self.lose(next, locked || kind == Kind::Lsf);
                        State::Hunt {
                            at: (next - search).max(0.0) as u64,
                        }
                    }
                };
                true
            }
        }
    }

    /// The 368 payload bits of the frame whose sync burst starts at `at`, sliced with the
    /// offset the burst measures.
    fn frame_bits(&self, at: f64, word: u16) -> [u8; FRAME_BITS] {
        let offset = self.offset(at, word);
        let symbols: Vec<u8> = (SYNC_SYMBOLS..FRAME_SYMBOLS)
            .map(|k| Self::slice(self.y(at + k as f64 * self.sps), offset))
            .collect();
        symbols_to_bits(&symbols)
            .try_into()
            .expect("184 symbols are 368 bits")
    }

    /// A stream frame received at `at`: its LICH chunk kept, the LSF handed on when the
    /// chunks complete it, then the frame.
    fn on_stream(&mut self, at: f64, f: StreamFrame, out: &mut Vec<(f64, M17Frame)>) {
        self.lost_at = None;
        let track = self.track.get_or_insert_with(|| Track {
            lsf: false,
            chunks: Default::default(),
        });
        if let Some((chunk, count)) = f.lich {
            track.chunks[usize::from(count)] = Some(chunk);
        }
        if !track.lsf
            && let Some(lsf) = lich_assemble(&track.chunks).and_then(|b| Lsf::parse(&b))
        {
            track.lsf = true;
            out.push((at, M17Frame::Lsf(lsf)));
        }
        out.push((
            at,
            M17Frame::Stream {
                frame_number: f.frame_number,
                last: f.last,
                payload: f.payload,
            },
        ));
        if f.last {
            self.track = None;
        }
    }

    /// The sync bursts stopped at `at`: a locked stream starts its timeout.
    fn lose(&mut self, at: f64, locked: bool) {
        if locked && self.track.is_some() && self.lost_at.is_none() {
            self.lost_at = Some(at);
        }
    }

    /// A stream whose bursts stopped [`M17_TRACK_TIMEOUT_FRAMES`] frames before `p` is
    /// over.
    fn expire_track(&mut self, p: f64, out: &mut Vec<(f64, M17Frame)>) {
        let timeout = (M17_TRACK_TIMEOUT_FRAMES * FRAME_SYMBOLS as u64) as f64 * self.sps;
        if let Some(lost) = self.lost_at
            && p > lost + timeout
        {
            self.lost_at = None;
            if self.track.take().is_some() {
                out.push((lost, M17Frame::Eot));
            }
        }
    }

    /// Drop the samples nothing will look at again.
    fn trim(&mut self) {
        let keep = match &self.state {
            State::Hunt { at } => *at,
            State::Frame { at, pending, .. } => {
                let from = pending.as_ref().map_or(*at, |(t, _)| t.min(*at));
                (from - LOCK_SEARCH_SAMPLES as f64 - 1.0).max(0.0) as u64
            }
        };
        let frame = (FRAME_SYMBOLS as f64 * self.sps) as u64;
        if keep > self.base + 2 * frame {
            let n = ((keep - self.base) as usize).min(self.buf.len());
            self.buf.drain(..n);
            self.base += n as u64;
        }
    }
}

/// The Codec 2 mode a stream's LSF asks for: its `c2dec` argument and the payload bytes
/// of each frame that are speech; `None` for a stream with no voice, or packet mode.
fn codec(lsf: &Lsf) -> Option<(&'static str, usize)> {
    match (lsf.stream, lsf.data_type) {
        (true, DATA_TYPE_VOICE) => Some(("3200", STREAM_PAYLOAD_BYTES)),
        (true, DATA_TYPE_VOICE_DATA) => Some(("1600", C2_1600_BYTES)),
        _ => None,
    }
}

/// An event's fields for `lsf` at frame `frame`.
fn fields(lsf: &Lsf, frame: u16, eot: bool) -> serde_json::Value {
    let gnss = lsf.encryption == ENCRYPTION_NONE && lsf.encryption_subtype == META_SUBTYPE_GNSS;
    let pos = gnss.then(|| gnss_position(&lsf.meta)).flatten();
    serde_json::json!({
        "src": lsf.src,
        "dst": lsf.dst,
        "can": lsf.can,
        "type": type_name(lsf),
        "encryption": encryption_name(lsf),
        "codec": codec(lsf).map(|(mode, _)| mode),
        "frame": frame,
        "eot": eot,
        "lat": pos.map(|p| p.0),
        "lon": pos.map(|p| p.1),
    })
}

/// What a vocoder needs from its stage.
#[derive(Clone)]
struct VocoderEnv {
    program: PathBuf,
    bus: Arc<StreamBus>,
    out: StreamId,
    ctx: StageContext,
    /// The reader tasks of finished vocoders, awaited when the input ends.
    drains: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

/// One stream's `c2dec`: frames in on its stdin, speech out on its stdout to the audio
/// output.
struct Vocoder {
    frames: Option<mpsc::UnboundedSender<Vec<u8>>>,
    spans: Option<mpsc::UnboundedSender<Span>>,
    /// The payload bytes of each frame that are speech.
    bytes: usize,
    closing: Arc<AtomicBool>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
}

impl Vocoder {
    /// `c2dec <mode> - -` for a stream, publishing on `env.out` with `prov`.
    fn spawn(env: &VocoderEnv, mode: &str, bytes: usize, prov: Provenance) -> Result<Self, String> {
        let mut child = tokio::process::Command::new(&env.program)
            .args([mode, "-", "-"])
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("{C2DEC} failed to start: {e}"))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let mut stdout = child.stdout.take().expect("piped stdout");
        let (frames, mut frames_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (spans, mut spans_rx) = mpsc::unbounded_channel::<Span>();
        let closing = Arc::new(AtomicBool::new(false));
        let writer = tokio::spawn(async move {
            while let Some(b) = frames_rx.recv().await {
                if stdin.write_all(&b).await.is_err() {
                    break;
                }
            }
        });
        let (bus, out, ctx, done) = (
            env.bus.clone(),
            env.out.clone(),
            env.ctx.clone(),
            closing.clone(),
        );
        let reader = tokio::spawn(async move {
            let mut resampler = Resampler::new(C2_RATE_HZ, AUDIO_RATE_HZ).expect("fixed rates");
            let mut raw = Vec::new();
            let mut speech: Vec<f32> = Vec::new();
            let mut buf = [0u8; 4096];
            let mut last_end = 0u64;
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => raw.extend_from_slice(&buf[..n]),
                }
                let whole = raw.len() / 2 * 2;
                speech.extend(
                    raw.drain(..whole)
                        .collect::<Vec<u8>>()
                        .chunks(2)
                        .map(|b| f32::from(i16::from_ne_bytes([b[0], b[1]])) / 32_768.0),
                );
                while speech.len() >= C2_FRAME_SAMPLES {
                    let chunk: Vec<f32> = speech.drain(..C2_FRAME_SAMPLES).collect();
                    let span = spans_rx.try_recv().unwrap_or(Span {
                        start_ns: Timestamp(last_end),
                        end_ns: Timestamp(last_end + FRAME_NS),
                    });
                    last_end = span.end_ns.0;
                    let samples = resampler.process(&chunk);
                    if samples.is_empty() {
                        continue;
                    }
                    let mut p = prov.clone();
                    p.span = span;
                    bus.publish(Frame::new(
                        out.clone(),
                        p,
                        Payload::Audio {
                            sample_rate_hz: Hz(AUDIO_RATE_HZ),
                            channels: 1,
                            samples: samples.into(),
                        },
                    ));
                }
            }
            let status = child.wait().await;
            let why = match status {
                Ok(s) if s.success() && done.load(Ordering::SeqCst) => return,
                Ok(s) => s.to_string(),
                Err(e) => e.to_string(),
            };
            ctx.note(serde_json::json!({"m17": {"vocoder": format!("{C2DEC} exited: {why}")}}));
        });
        Ok(Self {
            frames: Some(frames),
            spans: Some(spans),
            bytes,
            closing,
            writer: Some(writer),
            reader: Some(reader),
        })
    }

    /// Hand a stream frame's speech to the vocoder.
    fn feed(&self, span: Span, payload: &[u8; STREAM_PAYLOAD_BYTES]) {
        if let (Some(s), Some(f)) = (&self.spans, &self.frames) {
            let _ = s.send(span);
            let _ = f.send(payload[..self.bytes].to_vec());
        }
    }

    /// Close the vocoder's input; its reader, still handing back speech, is returned.
    fn finish(&mut self) -> Option<JoinHandle<()>> {
        self.closing.store(true, Ordering::SeqCst);
        self.frames = None;
        self.spans = None;
        self.writer = None;
        self.reader.take()
    }
}

impl Drop for Vocoder {
    fn drop(&mut self) {
        // Not finished: the stage failed or was cancelled, so the child goes too.
        for h in [self.writer.take(), self.reader.take()]
            .into_iter()
            .flatten()
        {
            h.abort();
        }
    }
}

/// The stream the stage is following.
struct Open {
    /// The LSF, once announced.
    lsf: Option<Lsf>,
    /// An LSF the LICH completed, announced at the stream frame that follows it.
    pending_lsf: Option<Lsf>,
    last_fn: u16,
    vocoder: Option<Vocoder>,
}

/// The stage's streams: frames in, events out, speech to the vocoder.
struct Streams {
    env: VocoderEnv,
    open: Option<Open>,
}

impl Streams {
    /// Announce `lsf` at frame `frame` (time `t`), starting its vocoder.
    fn announce(
        &mut self,
        open: &mut Open,
        lsf: Lsf,
        frame: u16,
        t: u64,
        prov: &Provenance,
        events: &mut Vec<(u64, serde_json::Value)>,
    ) {
        events.push((t, fields(&lsf, frame, false)));
        if let Some((mode, bytes)) = codec(&lsf)
            && encryption_name(&lsf).is_none()
        {
            match Vocoder::spawn(&self.env, mode, bytes, prov.clone()) {
                Ok(v) => open.vocoder = Some(v),
                Err(e) => self
                    .env
                    .ctx
                    .note(serde_json::json!({"m17": {"vocoder": e}})),
            }
        }
        open.lsf = Some(lsf);
    }

    /// End the open stream at time `t`: its EOT event, when it was announced.
    fn close(&mut self, t: u64, events: &mut Vec<(u64, serde_json::Value)>) {
        let Some(mut open) = self.open.take() else {
            return;
        };
        if let Some(lsf) = &open.lsf {
            events.push((t, fields(lsf, open.last_fn, true)));
        }
        if let Some(h) = open.vocoder.as_mut().and_then(Vocoder::finish) {
            let mut d = self.env.drains.lock().unwrap();
            d.retain(|h| !h.is_finished());
            d.push(h);
        }
    }

    /// One frame from the receiver, at time `t`.
    fn frame(
        &mut self,
        f: M17Frame,
        t: u64,
        prov: &Provenance,
        events: &mut Vec<(u64, serde_json::Value)>,
    ) {
        match f {
            M17Frame::Lsf(lsf) => match self.open.take() {
                // Joined mid-stream: the LICH completed it.
                Some(mut open) if open.lsf.is_none() => {
                    open.pending_lsf = Some(lsf);
                    self.open = Some(open);
                }
                other => {
                    self.open = other;
                    self.close(t, events);
                    let mut open = Open {
                        lsf: None,
                        pending_lsf: None,
                        last_fn: 0,
                        vocoder: None,
                    };
                    self.announce(&mut open, lsf, 0, t, prov, events);
                    self.open = Some(open);
                }
            },
            M17Frame::Stream {
                frame_number,
                last,
                payload,
            } => {
                let mut open = self.open.take().unwrap_or(Open {
                    lsf: None,
                    pending_lsf: None,
                    last_fn: frame_number,
                    vocoder: None,
                });
                open.last_fn = frame_number;
                if let Some(lsf) = open.pending_lsf.take() {
                    self.announce(&mut open, lsf, frame_number, t, prov, events);
                }
                if let Some(v) = &open.vocoder {
                    v.feed(
                        Span {
                            start_ns: Timestamp(t),
                            end_ns: Timestamp(t + FRAME_NS),
                        },
                        &payload,
                    );
                }
                self.open = Some(open);
                if last {
                    self.close(t, events);
                }
            }
            M17Frame::Eot => self.close(t, events),
        }
    }
}

/// The vocoder program for the stage's row (its handler 0), else `c2dec` found the same
/// way for a stage run without one.
fn vocoder_program(ctx: &StageContext) -> Result<PathBuf, String> {
    match &ctx.mode {
        Some(row) => zeta_core::modes::handlers::resolve(row, 0),
        None => zeta_core::modes::handlers::find_handler(&[C2DEC.to_string()])
            .ok_or_else(|| format!("{M17_DECODE_KIND}: handler {C2DEC} not found (tried {C2DEC})")),
    }
}

/// `t0` plus `samples` (maybe negative) at `rate_hz`, ns.
fn time_of(t0: Timestamp, samples: i64, rate_hz: u64) -> u64 {
    let dt = i128::from(samples) * 1_000_000_000 / i128::from(rate_hz);
    (i128::from(t0.0) + dt).clamp(0, i128::from(u64::MAX)) as u64
}

/// Decodes M17 from a channel's IQ: two events per stream and its speech.
pub struct M17DecodeStage {
    spec: StageSpec,
}

impl M17DecodeStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: M17_DECODE_KIND,
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event, StreamKind::Audio],
                params_schema: schemars::schema_for!(M17Params),
            },
        }
    }
}

impl Default for M17DecodeStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for M17DecodeStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
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
        let _: M17Params = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{M17_DECODE_KIND} params: {e}")))?;
        let [events_out, audio_out] = outputs.as_slice() else {
            return Err(StageError::Failed("expected 2 outputs".into()));
        };
        let events_out = events_out.clone();
        let program = vocoder_program(&ctx).map_err(StageError::Failed)?;
        let drains = Arc::new(Mutex::new(Vec::new()));
        let mut streams = Streams {
            env: VocoderEnv {
                program,
                bus: bus.clone(),
                out: audio_out.clone(),
                ctx: ctx.clone(),
                drains: drains.clone(),
            },
            open: None,
        };
        let mut demod: Option<M17Demod> = None;
        let mut last_t = 0u64;
        let mut last_prov: Option<Provenance> = None;
        let result = run_map_ending(self.spec().kind, inputs, bus, cancel, move |input| {
            let mut events = Vec::new();
            let frame = match input {
                Some(Input::Frame(f)) => f,
                Some(Input::Lagged { .. }) => {
                    demod = None;
                    streams.close(last_t, &mut events);
                    return Ok(publish(&ctx, &events_out, last_prov.as_ref(), events));
                }
                None => {
                    // The input ended mid-stream: its speech so far is still handed back.
                    if let Some(v) = streams.open.as_mut().and_then(|o| o.vocoder.as_mut())
                        && let Some(h) = v.finish()
                    {
                        streams.env.drains.lock().unwrap().push(h);
                    }
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(format!(
                    "{M17_DECODE_KIND} expects iq frames"
                )));
            };
            let rate = block.sample_rate_hz.0;
            let d = match &mut demod {
                Some(d) if d.rate_hz != rate => {
                    return Err(StageError::Failed(format!(
                        "sample_rate_hz changed from {} to {rate}",
                        d.rate_hz
                    )));
                }
                Some(d) => d,
                None => demod.insert(M17Demod::new(rate).map_err(StageError::Failed)?),
            };
            for (at, f) in d.feed_timed(&block.samples) {
                let t = time_of(block.t0, at, rate);
                last_t = t;
                streams.frame(f, t, &frame.provenance, &mut events);
            }
            last_prov = Some(frame.provenance.clone());
            Ok(publish(&ctx, &events_out, Some(&frame.provenance), events))
        })
        .await;
        let pending: Vec<JoinHandle<()>> = std::mem::take(&mut *drains.lock().unwrap());
        if result.is_ok() {
            for h in pending {
                let _ = tokio::time::timeout(VOCODER_DRAIN, h).await;
            }
        } else {
            pending.iter().for_each(JoinHandle::abort);
        }
        result
    }
}

/// The events `(t, fields)` as frames on `out`, each in the row's envelope.
fn publish(
    ctx: &StageContext,
    out: &StreamId,
    prov: Option<&Provenance>,
    events: Vec<(u64, serde_json::Value)>,
) -> Vec<Frame> {
    let Some(prov) = prov else {
        return Vec::new();
    };
    events
        .into_iter()
        .map(|(t, fields)| {
            let mut p = prov.clone();
            p.span = Span {
                start_ns: Timestamp(t),
                end_ns: Timestamp(t + FRAME_NS),
            };
            Frame::new(out.clone(), p, Payload::Event(event(ctx, t, "m17", fields)))
        })
        .collect()
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use num_complex::Complex;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use zeta_core::port::IqBlock;
    use zeta_core::stage::{Stage, StageContext, StageError};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};

    use super::*;
    use crate::m17::{
        DATA_TYPE_VOICE, ENCRYPTION_NONE, ENCRYPTION_SCRAMBLER, FRAME_SYMBOLS, Lsf, META_BYTES,
        STREAM_PAYLOAD_BYTES, eot_symbols, lsf_frame_symbols, preamble_symbols,
        stream_frame_symbols,
    };
    use crate::synth::{
        M17_BAUD, M17_DEVIATION_HZ, M17_RRC_ALPHA, parse_params, rrc_modulate_4fsk, synthesise,
    };

    const RATE: u64 = 48_000;
    /// One M17 frame, ns.
    const FRAME_NS: u64 = 40_000_000;

    /// The env override is process-wide: one test at a time sets it.
    static VOCODER_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn fake_c2dec() -> String {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_c2dec").into()
    }

    /// A program that exits 1 at once.
    fn dead_c2dec() -> String {
        ["/usr/bin/false", "/bin/false"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("false")
            .into()
    }

    fn set_vocoder(path: &str) {
        // SAFETY: every test that reads or writes this variable holds VOCODER_ENV.
        unsafe { std::env::set_var("ZETA_HANDLER_C2DEC", path) };
    }

    fn prov() -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(0),
            },
        }
    }

    fn iq_frame(samples: &[Complex<f32>], t0_ns: u64) -> Frame {
        Frame::new(
            StreamId::new(),
            prov(),
            Payload::Iq(IqBlock {
                t0: Timestamp(t0_ns),
                sample_rate_hz: Hz(RATE),
                center_hz: Hz(145_000_000),
                samples: samples.to_vec().into(),
            }),
        )
    }

    fn mode() -> Arc<zeta_core::modes::ModeRow> {
        let mut stages = zeta_core::stage::StageRegistry::new();
        crate::register_all(&mut stages);
        let loaded = zeta_core::modes::ModeRegistry::load(&stages, None)
            .get(M17_DECODE_KIND)
            .expect("row")
            .clone();
        Arc::new(loaded.row)
    }

    /// The `m17` family's stream from VK4ZET to ALL, `frames` long, at [`RATE`].
    fn m17_capture(frames: u32) -> Vec<Complex<f32>> {
        let p = serde_json::json!({
            "family": "m17", "src": "VK4ZET", "dst": "ALL", "can": 0, "frames": frames
        });
        let (spec, imp) = parse_params(p.as_object().unwrap().clone()).unwrap();
        synthesise(&spec, &imp, RATE as u32, 0)
    }

    /// A stream of `lsf` with `frames` zero-payload frames, between 100 ms of carrier.
    fn capture_of(lsf: &Lsf, frames: u16) -> Vec<Complex<f32>> {
        let mut symbols = preamble_symbols();
        symbols.extend(lsf_frame_symbols(lsf));
        for n in 0..frames {
            symbols.extend(stream_frame_symbols(
                lsf,
                n,
                &[0; STREAM_PAYLOAD_BYTES],
                n + 1 == frames,
            ));
        }
        symbols.extend(eot_symbols());
        let carrier = vec![Complex::new(1.0f32, 0.0); RATE as usize / 10];
        let mut iq = carrier.clone();
        iq.extend(rrc_modulate_4fsk(
            &symbols,
            M17_BAUD,
            M17_DEVIATION_HZ,
            RATE as u32,
            M17_RRC_ALPHA,
        ));
        iq.extend(carrier);
        iq
    }

    fn lsf(encryption: u8) -> Lsf {
        Lsf {
            dst: "ALL".into(),
            src: "VK4ZET".into(),
            stream: true,
            data_type: DATA_TYPE_VOICE,
            encryption,
            encryption_subtype: 0,
            can: 0,
            meta: [0; META_BYTES],
        }
    }

    /// What one run of the stage gave: its result, its events, its audio frames and its
    /// notes.
    struct Run {
        result: Result<(), StageError>,
        events: Vec<serde_json::Value>,
        audio: Vec<Frame>,
        notes: Vec<serde_json::Value>,
    }

    /// Poll `f` every 10 ms for up to 5 s until it holds.
    async fn until(mut f: impl FnMut() -> bool) -> bool {
        for _ in 0..500 {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        f()
    }

    /// Run the stage over `iq` (4 800-sample blocks), then collect what it published,
    /// waiting until `done` holds over the run.
    async fn run_stage(iq: &[Complex<f32>], done: impl Fn(&Run) -> bool) -> Run {
        let bus = Arc::new(StreamBus::new(1024));
        let ev = bus.register(StreamKind::Event, prov());
        let au = bus.register(StreamKind::Audio, prov());
        let mut rx_ev = bus.subscribe(&ev).unwrap();
        let mut rx_au = bus.subscribe(&au).unwrap();
        let (tx, rx_in) = broadcast::channel(4096);
        let notes = Arc::new(Mutex::new(Vec::new()));
        let n = notes.clone();
        let ctx = StageContext::new(SourceId("s".into()))
            .with_mode(mode())
            .with_note(Arc::new(move |v| n.lock().unwrap().push(v)));
        let h = tokio::spawn(async move {
            M17DecodeStage::new()
                .run(
                    serde_json::json!({}),
                    vec![rx_in],
                    vec![ev, au],
                    bus,
                    CancellationToken::new(),
                    ctx,
                )
                .await
        });
        let block = 4_800usize;
        for (i, chunk) in iq.chunks(block).enumerate() {
            let t0 = 1_000_000_000 + (i * block) as u64 * 1_000_000_000 / RATE;
            tx.send(iq_frame(chunk, t0)).unwrap();
        }
        drop(tx);
        let result = h.await.unwrap();
        let mut run = Run {
            result,
            events: vec![],
            audio: vec![],
            notes: vec![],
        };
        until(|| {
            while let Ok(f) = rx_ev.try_recv() {
                let Payload::Event(v) = f.payload else {
                    panic!("expected an event")
                };
                run.events.push(v);
            }
            while let Ok(f) = rx_au.try_recv() {
                run.audio.push(f);
            }
            run.notes = notes.lock().unwrap().clone();
            done(&run)
        })
        .await;
        run
    }

    fn vocoder_note(notes: &[serde_json::Value]) -> Option<String> {
        notes
            .iter()
            .find_map(|n| n["m17"]["vocoder"].as_str().map(String::from))
    }

    fn audio_ns(audio: &[Frame]) -> u64 {
        audio
            .iter()
            .map(|f| {
                let Payload::Audio {
                    sample_rate_hz,
                    channels,
                    ..
                } = &f.payload
                else {
                    panic!("expected audio")
                };
                assert_eq!((sample_rate_hz.0, *channels), (48_000, 1));
                f.provenance.span.end_ns.0 - f.provenance.span.start_ns.0
            })
            .sum()
    }

    #[tokio::test]
    async fn a_stream_gives_two_events_and_audio() {
        let _env = VOCODER_ENV.lock().await;
        set_vocoder(&fake_c2dec());
        let iq = m17_capture(25);
        let run = run_stage(&iq, |r| {
            r.events.len() >= 2 && audio_ns(&r.audio) + FRAME_NS >= 25 * FRAME_NS
        })
        .await;
        assert!(run.result.is_ok(), "{:?}", run.result);
        assert_eq!(run.events.len(), 2, "{:?}", run.events);
        let (a, b) = (&run.events[0], &run.events[1]);
        assert_eq!(a["protocol"], "m17", "{a}");
        assert_eq!(a["src"], "VK4ZET", "{a}");
        assert_eq!(a["dst"], "ALL", "{a}");
        assert_eq!(a["can"], 0, "{a}");
        assert_eq!(a["type"], "voice", "{a}");
        assert_eq!(a["encryption"], serde_json::Value::Null, "{a}");
        assert_eq!(a["codec"], "3200", "{a}");
        assert_eq!(a["frame"], 0, "{a}");
        assert_eq!(a["eot"], false, "{a}");
        assert_eq!(a["lat"], serde_json::Value::Null, "{a}");
        assert!(
            a["summary"]
                .as_str()
                .unwrap()
                .starts_with("VK4ZET > ALL: voice"),
            "{a}"
        );
        assert_eq!(b["eot"], true, "{b}");
        assert_eq!(b["frame"], 24, "{b}");
        assert_eq!(b["src"], "VK4ZET", "{b}");
        let ns = audio_ns(&run.audio);
        assert!(
            ns.abs_diff(25 * FRAME_NS) <= FRAME_NS,
            "{ns} ns of audio in {} frames",
            run.audio.len()
        );
        assert_eq!(vocoder_note(&run.notes), None, "{:?}", run.notes);
    }

    /// With the real `c2dec`, when the host has it: the zero payload renders as a
    /// second of speech-rate audio.
    #[tokio::test]
    async fn the_real_c2dec_gives_the_streams_speech() {
        let Some(c2dec) = zeta_core::modes::handlers::find_program(
            &[C2DEC.to_string()],
            None,
            std::env::var_os("PATH"),
        ) else {
            eprintln!("c2dec not on this host");
            return;
        };
        let _env = VOCODER_ENV.lock().await;
        set_vocoder(c2dec.to_str().unwrap());
        let run = run_stage(&m17_capture(25), |r| {
            r.events.len() >= 2 && audio_ns(&r.audio) + FRAME_NS >= 25 * FRAME_NS
        })
        .await;
        assert_eq!(run.events.len(), 2, "{:?}", run.events);
        let ns = audio_ns(&run.audio);
        assert!(ns.abs_diff(25 * FRAME_NS) <= FRAME_NS, "{ns} ns of audio");
        assert_eq!(vocoder_note(&run.notes), None, "{:?}", run.notes);
    }

    #[tokio::test]
    async fn joining_mid_stream_finds_the_lsf_from_the_lich() {
        let _env = VOCODER_ENV.lock().await;
        set_vocoder(&fake_c2dec());
        let iq = m17_capture(25);
        // Cut after the preamble, the LSF frame and two stream frames.
        let sps = (RATE / u64::from(M17_BAUD)) as usize;
        let cut = 4 * FRAME_SYMBOLS * sps;
        let run = run_stage(&iq[cut..], |r| r.events.len() >= 2).await;
        assert!(run.result.is_ok(), "{:?}", run.result);
        let a = &run.events[0];
        assert_eq!(
            (a["src"].as_str(), a["dst"].as_str()),
            (Some("VK4ZET"), Some("ALL")),
            "{a}"
        );
        assert_eq!(a["eot"], false, "{a}");
        let frame = a["frame"].as_u64().unwrap();
        // The six LICH chunks: by the sixth frame after the one cut into.
        assert!((2..=2 + 6).contains(&frame), "{a}");
        assert_eq!(run.events[1]["eot"], true, "{:?}", run.events);
        assert_eq!(run.events[1]["frame"], 24, "{:?}", run.events);
    }

    #[tokio::test]
    async fn an_encrypted_stream_is_reported_and_not_decoded() {
        let _env = VOCODER_ENV.lock().await;
        set_vocoder(&fake_c2dec());
        let iq = capture_of(&lsf(ENCRYPTION_SCRAMBLER), 10);
        let run = run_stage(&iq, |r| r.events.len() >= 2).await;
        assert!(run.result.is_ok(), "{:?}", run.result);
        assert_eq!(run.events.len(), 2, "{:?}", run.events);
        let a = &run.events[0];
        assert_eq!(a["encryption"], "scrambler", "{a}");
        assert_eq!(a["type"], "voice", "{a}");
        assert!(
            a["summary"].as_str().unwrap().contains("voice scrambler"),
            "{a}"
        );
        // Give a vocoder the time it would take to answer: still no audio.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(run.audio.is_empty(), "{} audio frames", run.audio.len());
    }

    #[tokio::test]
    async fn a_dead_vocoder_fails_no_event() {
        let _env = VOCODER_ENV.lock().await;
        set_vocoder(&dead_c2dec());
        let iq = capture_of(&lsf(ENCRYPTION_NONE), 25);
        let run = run_stage(&iq, |r| {
            r.events.len() >= 2 && vocoder_note(&r.notes).is_some()
        })
        .await;
        assert!(run.result.is_ok(), "{:?}", run.result);
        assert_eq!(run.events.len(), 2, "{:?}", run.events);
        assert_eq!(run.events[0]["eot"], false);
        assert_eq!(run.events[1]["eot"], true);
        assert_eq!(run.events[1]["frame"], 24);
        assert!(run.audio.is_empty(), "{} audio frames", run.audio.len());
        let note = vocoder_note(&run.notes).expect("a vocoder note");
        assert!(note.starts_with("c2dec exited"), "{note}");
        let said = run
            .notes
            .iter()
            .filter(|n| n["m17"]["vocoder"].is_string())
            .count();
        assert_eq!(said, 1, "once: {:?}", run.notes);
    }

    #[tokio::test]
    async fn the_next_lsf_restarts_a_dead_vocoder() {
        let _env = VOCODER_ENV.lock().await;
        set_vocoder(&dead_c2dec());
        let mut iq = capture_of(&lsf(ENCRYPTION_NONE), 5);
        iq.extend(capture_of(&lsf(ENCRYPTION_NONE), 5));
        let run = run_stage(&iq, |r| {
            r.events.len() >= 4
                && r.notes
                    .iter()
                    .filter(|n| n["m17"]["vocoder"].is_string())
                    .count()
                    >= 2
        })
        .await;
        assert_eq!(run.events.len(), 4, "{:?}", run.events);
        let said = run
            .notes
            .iter()
            .filter(|n| n["m17"]["vocoder"].is_string())
            .count();
        assert_eq!(said, 2, "one per stream: {:?}", run.notes);
    }

    #[tokio::test]
    async fn a_missing_vocoder_fails_the_stage_naming_it() {
        let _env = VOCODER_ENV.lock().await;
        set_vocoder("/nonexistent/zeta-test/c2dec");
        let run = run_stage(&[], |_| true).await;
        match run.result {
            Err(StageError::Failed(m)) => {
                assert!(m.contains("handler c2dec not found"), "{m}")
            }
            r => panic!("expected Failed, got {r:?}"),
        }
    }

    #[test]
    fn a_low_rate_is_refused() {
        let e = M17Demod::new(16_000).err().expect("refused");
        assert!(e.contains("16000"), "{e}");
        assert!(M17Demod::new(RATE).is_ok());
    }

    #[test]
    fn the_demodulator_gives_the_lsf_and_every_frame() {
        let mut d = M17Demod::new(RATE).unwrap();
        let iq = m17_capture(8);
        let frames: Vec<M17Frame> = iq.chunks(1000).flat_map(|c| d.feed(c)).collect();
        let M17Frame::Lsf(l) = &frames[0] else {
            panic!("{frames:?}")
        };
        assert_eq!((l.src.as_str(), l.dst.as_str()), ("VK4ZET", "ALL"));
        let numbers: Vec<(u16, bool)> = frames
            .iter()
            .filter_map(|f| match f {
                M17Frame::Stream {
                    frame_number,
                    last,
                    payload,
                } => {
                    assert_eq!(payload, &[0; STREAM_PAYLOAD_BYTES]);
                    Some((*frame_number, *last))
                }
                _ => None,
            })
            .collect();
        let want: Vec<(u16, bool)> = (0..8).map(|n| (n, n == 7)).collect();
        assert_eq!(numbers, want);
        assert!(matches!(frames.last(), Some(M17Frame::Eot)), "{frames:?}");
    }

    #[test]
    fn the_demodulator_survives_noise_and_an_offset() {
        let p = serde_json::json!({
            "family": "m17", "src": "VK4ZET", "dst": "ALL", "can": 0, "frames": 25,
            "snr_db": 14.0, "offset_hz": 700.0
        });
        let (spec, imp) = parse_params(p.as_object().unwrap().clone()).unwrap();
        let iq = synthesise(&spec, &imp, RATE as u32, 0);
        let mut d = M17Demod::new(RATE).unwrap();
        let frames: Vec<M17Frame> = iq.chunks(4_800).flat_map(|c| d.feed(c)).collect();
        assert!(
            matches!(&frames[0], M17Frame::Lsf(l) if l.src == "VK4ZET"),
            "{frames:?}"
        );
        let streams = frames
            .iter()
            .filter(|f| matches!(f, M17Frame::Stream { .. }))
            .count();
        assert!(streams >= 24, "{streams} of 25 stream frames");
    }

    #[test]
    fn a_stream_cut_short_ends_after_its_timeout() {
        let iq = m17_capture(25);
        let sps = (RATE / u64::from(M17_BAUD)) as usize;
        // The preamble, the LSF and 10 stream frames, then a second of carrier.
        let mut cut = iq[..12 * FRAME_SYMBOLS * sps].to_vec();
        cut.extend(vec![Complex::new(1.0f32, 0.0); RATE as usize]);
        let mut d = M17Demod::new(RATE).unwrap();
        let frames: Vec<M17Frame> = cut.chunks(4_800).flat_map(|c| d.feed(c)).collect();
        let streams = frames
            .iter()
            .filter(|f| matches!(f, M17Frame::Stream { .. }))
            .count();
        assert_eq!(streams, 10, "{frames:?}");
        assert!(matches!(frames.last(), Some(M17Frame::Eot)), "{frames:?}");
    }

    #[tokio::test]
    async fn a_gnss_meta_gives_the_position() {
        let _env = VOCODER_ENV.lock().await;
        set_vocoder(&fake_c2dec());
        let mut l = lsf(ENCRYPTION_NONE);
        l.encryption_subtype = crate::m17::META_SUBTYPE_GNSS;
        l.meta = crate::m17::gnss_meta(-27.3842, 153.1175);
        let run = run_stage(&capture_of(&l, 6), |r| r.events.len() >= 2).await;
        let a = &run.events[0];
        assert!((a["lat"].as_f64().unwrap() + 27.3842).abs() < 0.001, "{a}");
        assert!((a["lon"].as_f64().unwrap() - 153.1175).abs() < 0.001, "{a}");
        assert!(a["position"].is_object(), "{a}");
    }

    #[test]
    fn noise_gives_no_frame() {
        let mut d = M17Demod::new(RATE).unwrap();
        let mut iq = vec![Complex::new(1.0f32, 0.0); RATE as usize * 2];
        crate::synth::awgn(&mut iq, -10.0, 7);
        assert!(d.feed(&iq).is_empty());
    }

    #[test]
    fn params_are_strict() {
        assert!(serde_json::from_value::<M17Params>(serde_json::json!({})).is_ok());
        assert!(serde_json::from_value::<M17Params>(serde_json::json!({"x": 1})).is_err());
        let stage = M17DecodeStage::new();
        assert_eq!(stage.spec().kind, M17_DECODE_KIND);
        assert_eq!(stage.spec().outputs, [StreamKind::Event, StreamKind::Audio]);
    }
}
