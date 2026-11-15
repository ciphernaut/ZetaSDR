//! P25 phase 1 decode stage (design 38, 5.5): the 4-level FSK front end ([`crate::fsk4`],
//! C4FM) and the framer ([`crate::p25`]) turn a channel's IQ into data units; this stage
//! turns those into call and channel events and the call's speech.
//!
//! **Calls.** One open call at a time. A header opens one; so does a voice unit (LDU1,
//! LDU2) when none is open, or when its network access code or its link control names
//! another call. Two `type: call` events per call ([`crate::voice_call`]):
//! - the opening one when its link control and its encryption are both in hand: at the
//!   first LDU1 after a header (the header carries no calling radio), or within a
//!   superframe when the job joins mid-call;
//! - the closing one (`eot: true`) at its terminator, when sync is lost for longer than
//!   [`P25_SUPERFRAME_MS`], at a lagged input, a new sample rate or the input's end.
//!
//! The header's talkgroup is not reported: it is 0 for a private call, and the link control
//! names the destination of either kind.
//!
//! **Encryption.** A call is encrypted when the last encryption sync seen (the header's, then
//! each LDU2's) names an algorithm other than [`P25_ALGID_CLEAR`]. A call joined mid-call is
//! held silent until its first encryption sync: its first LDU1 feeds nothing and it opens on
//! the LDU2. An encrypted call gets its events and no vocoder; nothing here decrypts.
//!
//! **Speech.** `zeta-mbe imbe7200x4400` (the row's handler 0, optional) is looked up when a
//! call's encryption is first known to be clear, so a program installed while the job runs is
//! used from the next call. Each LDU's nine IMBE frames go to it, each with a 20 ms span from
//! the unit's time, through the vocoder seat ([`crate::vocoder`]), and its speech is the
//! job's audio. Without the program every call is reported with `audio: false`,
//! `silent: "no_vocoder"`, and `job_context.p25.vocoder` says how to install it, once. A
//! vocoder that fails mid-call ends that call's audio (`silent: "vocoder_failed"` on its
//! closing event); the next call starts it again.
//!
//! **A channel with no call.** A unit decoded with no call open (a trunking signalling unit,
//! a packet data unit, a stray terminator) gives one `type: channel` event, and another only
//! when its NAC changes or trunking signalling is first seen on it (`control`). Trunking and
//! packet units are counted in `job_context.p25` (`trunking_units`, `packet_units`), not
//! parsed.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, Span, StreamId, StreamKind, Timestamp};

use crate::fsk4::{Fsk4Demod, Fsk4Params, Shape};
use crate::p25::{
    EncSync, IMBE_FEED_BYTES, LDU_DIBITS, LinkControl, P25_ALGID_CLEAR, P25_BAUD, P25_DEVIATION_HZ,
    P25_SUPERFRAME_MS, P25Framer, P25Unit,
};
use crate::util::event;
use crate::vocoder::{Vocoder, VocoderEnv, VocoderSpec};
use crate::voice_call::{
    CallFields, MBE_FRAME_SAMPLES, MBE_RATE_HZ, Silent, ZETA_MBE, channel_event, no_vocoder_note,
};
use crate::{Input, run_map_ending};

/// The stage's kind and the row's id.
pub const P25_DECODE_KIND: &str = "p25_decode";
/// The events' `protocol` and the job context key.
const PROTOCOL: &str = "p25";
/// `zeta-mbe`'s argument for P25's full-rate IMBE.
pub const IMBE_CODEC: &str = "imbe7200x4400";
/// How long the vocoders of a finished input may take to hand back their speech.
pub const VOCODER_DRAIN: Duration = Duration::from_secs(2);
/// One IMBE frame, ns (20 ms).
const IMBE_FRAME_NS: u64 = MBE_FRAME_SAMPLES as u64 * 1_000_000_000 / MBE_RATE_HZ;
/// One LDU, ns (180 ms): the span of a call event.
const LDU_NS: u64 = dibits_ns(LDU_DIBITS);
/// How long a call's sync may be lost before it is over, ns.
const SUPERFRAME_NS: u64 = P25_SUPERFRAME_MS * 1_000_000;
/// Dibits in a header, status symbols included.
const HDU_DIBITS: usize = 396;

/// `n` dibits at 4800 baud, ns.
const fn dibits_ns(n: usize) -> u64 {
    n as u64 * 1_000_000_000 / P25_BAUD as u64
}

/// `p25_decode` takes no parameters.
#[derive(Default, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct P25Params {}

/// The receiver over one sample rate: the front end, the framer, and how many samples it
/// has taken (its positions count from its first).
struct Receiver {
    demod: Fsk4Demod,
    framer: P25Framer,
    fed: u64,
}

impl Receiver {
    fn new(rate_hz: u64) -> Result<Self, String> {
        let params = Fsk4Params {
            symbol_rate_baud: P25_BAUD,
            deviation_hz: P25_DEVIATION_HZ,
            shape: Shape::C4fm,
        };
        Ok(Self {
            demod: Fsk4Demod::new(rate_hz, params)
                .map_err(|e| format!("{P25_DECODE_KIND}: {e}"))?,
            framer: P25Framer::new(),
            fed: 0,
        })
    }

    /// The units `block` completes, each with its sync's time.
    fn feed(&mut self, block: &zeta_core::port::IqBlock) -> Vec<(u64, u16, P25Unit)> {
        let start = self.fed;
        self.fed += block.samples.len() as u64;
        let dibits = self.demod.feed(&block.samples);
        let units = self.framer.push(&dibits);
        self.demod.invert(self.framer.inverted().unwrap_or(false));
        let rate = self.demod.rate_hz;
        units
            .into_iter()
            .map(|(at, nac, u)| {
                let t = time_of(block.t0, at.round() as i64 - start as i64, rate);
                (t, nac, u)
            })
            .collect()
    }
}

/// What a call's speech is doing.
enum Voice {
    /// Its encryption is not known yet: nothing is fed.
    Held,
    Heard(Vocoder),
    Silent(Silent),
}

/// The call the stage is following.
struct Open {
    nac: u16,
    lc: Option<LinkControl>,
    /// The last encryption sync seen.
    enc: Option<EncSync>,
    voice: Voice,
    announced: bool,
    frames: u32,
    errors: u32,
    /// Whether any of its speech went to the audio output.
    heard: bool,
    /// When its last unit ended, ns.
    end_ns: u64,
}

impl Open {
    fn new(nac: u16) -> Self {
        Self {
            nac,
            lc: None,
            enc: None,
            voice: Voice::Held,
            announced: false,
            frames: 0,
            errors: 0,
            heard: false,
            end_ns: 0,
        }
    }

    fn encrypted(&self) -> bool {
        self.enc.is_some_and(|e| e.algid != P25_ALGID_CLEAR)
    }

    /// Why its speech is not heard now; `None` while a live vocoder has it.
    fn silent(&self) -> Option<Silent> {
        match &self.voice {
            Voice::Heard(v) if v.alive() => None,
            Voice::Heard(_) => Some(Silent::VocoderFailed),
            Voice::Silent(s) => Some(*s),
            // Never asked: a call is announced only once its speech is settled.
            Voice::Held => None,
        }
    }

    /// Whether a unit with `nac` and link control `lc` belongs to this call.
    fn takes(&self, nac: u16, lc: Option<&LinkControl>) -> bool {
        let same =
            |a: &LinkControl, b: &LinkControl| (a.call, a.src, a.dst) == (b.call, b.src, b.dst);
        nac == self.nac && !matches!((&self.lc, lc), (Some(a), Some(b)) if !same(a, b))
    }
}

/// The stage's calls: units in, events out, speech to the vocoder.
struct Calls {
    env: VocoderEnv,
    open: Option<Open>,
    /// Whether the missing vocoder has been noted since it was last found.
    said_missing: bool,
    /// The last channel event's NAC and `control`.
    channel: Option<(u16, bool)>,
    /// The NAC trunking signalling was last seen on.
    control_nac: Option<u16>,
    trunking_units: u64,
    packet_units: u64,
}

type Events = Vec<(u64, serde_json::Value)>;

impl Calls {
    /// The program to speak a clear call with, looked up now; `None` noted once.
    fn program(&mut self) -> Option<PathBuf> {
        let found = crate::vocoder::program(&self.env.ctx, 0, ZETA_MBE, false)
            .ok()
            .flatten();
        match &found {
            Some(_) => self.said_missing = false,
            None if !self.said_missing => {
                self.said_missing = true;
                let note = no_vocoder_note(&self.env.ctx);
                self.env
                    .ctx
                    .note(serde_json::json!({PROTOCOL: {"vocoder": note}}));
            }
            None => {}
        }
        found
    }

    /// Settle `open`'s speech on its encryption, now known: stop a vocoder when it turns
    /// encrypted, start one when it is (or turns) clear.
    fn settle(&mut self, open: &mut Open, prov: &Provenance) {
        let encrypted = open.encrypted();
        let voice = std::mem::replace(&mut open.voice, Voice::Held);
        open.voice = match (voice, encrypted) {
            (Voice::Heard(v), true) => {
                self.drain(v);
                Voice::Silent(Silent::Encrypted)
            }
            (_, true) => Voice::Silent(Silent::Encrypted),
            (Voice::Held | Voice::Silent(Silent::Encrypted), false) => self.start(prov),
            (other, false) => other,
        };
    }

    /// A vocoder for a clear call, or why there is none.
    fn start(&mut self, prov: &Provenance) -> Voice {
        let Some(program) = self.program() else {
            return Voice::Silent(Silent::NoVocoder);
        };
        let spec = VocoderSpec {
            name: ZETA_MBE,
            program,
            args: vec![IMBE_CODEC.into()],
            feed_bytes: IMBE_FEED_BYTES,
            feed_samples: MBE_FRAME_SAMPLES,
            rate_hz: MBE_RATE_HZ,
            context: PROTOCOL,
        };
        match Vocoder::spawn(&self.env, &spec, prov.clone()) {
            Ok(v) => Voice::Heard(v),
            Err(e) => {
                self.env
                    .ctx
                    .note(serde_json::json!({PROTOCOL: {"vocoder": e}}));
                Voice::Silent(Silent::VocoderFailed)
            }
        }
    }

    /// Close `v`'s input; its reader, still publishing, is awaited when the input ends.
    fn drain(&self, mut v: Vocoder) {
        if let Some(h) = v.finish() {
            let mut d = self.env.drains.lock().unwrap();
            d.retain(|h| !h.is_finished());
            d.push(h);
        }
    }

    /// `open`'s event; `eot` for its closing one.
    fn call_event(open: &Open, eot: bool) -> serde_json::Value {
        let (Some(lc), Some(enc)) = (open.lc, open.enc) else {
            unreachable!("a call is announced with its link control and encryption")
        };
        let silent = open.silent();
        let mut m = CallFields {
            protocol: PROTOCOL,
            src: lc.src,
            dst: lc.dst,
            call: lc.call.word(),
            encrypted: open.encrypted(),
            emergency: lc.emergency,
            silent,
            stream: None,
            program: if eot { open.heard } else { silent.is_none() },
            frames: open.frames,
            errors: open.errors,
            eot,
        }
        .to_json();
        m.insert("nac".into(), open.nac.into());
        m.insert("mfid".into(), lc.mfid.into());
        m.insert("algid".into(), enc.algid.into());
        m.insert("kid".into(), enc.kid.into());
        serde_json::Value::Object(m)
    }

    /// End the open call at `t`: its closing event, when it was announced.
    fn close(&mut self, t: u64, events: &mut Events) {
        let Some(open) = self.open.take() else {
            return;
        };
        if open.announced {
            events.push((t, Self::call_event(&open, true)));
        }
        if let Voice::Heard(v) = open.voice {
            self.drain(v);
        }
    }

    /// End the open call if its sync has been lost for over a superframe at `now`.
    fn expire(&mut self, now: u64, events: &mut Events) {
        if let Some(end) = self.open.as_ref().map(|o| o.end_ns)
            && now > end + SUPERFRAME_NS
        {
            self.close(end, events);
        }
    }

    /// The input stopped making sense (lagged, a new rate, its end): the open call closes
    /// at its last unit.
    fn interrupt(&mut self, events: &mut Events) {
        if let Some(end) = self.open.as_ref().map(|o| o.end_ns) {
            self.close(end, events);
        }
    }

    /// One unit from the framer, its sync at `t`.
    fn unit(&mut self, t: u64, nac: u16, unit: P25Unit, prov: &Provenance, events: &mut Events) {
        match unit {
            P25Unit::Header { enc, .. } => {
                self.close(t, events);
                let mut open = Open::new(nac);
                open.end_ns = t + dibits_ns(HDU_DIBITS);
                if let Some(enc) = enc {
                    open.enc = Some(enc);
                    self.settle(&mut open, prov);
                }
                self.open = Some(open);
            }
            P25Unit::Ldu1 { .. } | P25Unit::Ldu2 { .. } => {
                self.voice_unit(t, nac, unit, prov, events)
            }
            P25Unit::Terminator { .. } if self.open.as_ref().is_some_and(|o| o.nac == nac) => {
                self.close(t, events)
            }
            P25Unit::Terminator { .. } | P25Unit::Other(_) => self.no_call(t, nac, events),
            P25Unit::Trunking => {
                self.trunking_units += 1;
                self.control_nac = Some(nac);
                self.no_call(t, nac, events);
            }
            P25Unit::Packet => {
                self.packet_units += 1;
                self.no_call(t, nac, events);
            }
        }
    }

    /// An LDU at `t`: link control from an LDU1, encryption sync from an LDU2.
    fn voice_unit(
        &mut self,
        t: u64,
        nac: u16,
        unit: P25Unit,
        prov: &Provenance,
        events: &mut Events,
    ) {
        let (lc, enc, voice, errors) = match unit {
            P25Unit::Ldu1 { lc, voice, errors } => (lc, None, voice, errors),
            P25Unit::Ldu2 { enc, voice, errors } => (None, enc, voice, errors),
            _ => return,
        };
        let mut open = match self.open.take() {
            Some(o) if o.takes(nac, lc.as_ref()) => o,
            other => {
                self.open = other;
                self.close(t, events);
                Open::new(nac)
            }
        };
        open.end_ns = t + LDU_NS;
        open.frames += voice.len() as u32;
        open.errors += errors;
        if let Some(lc) = lc {
            open.lc = Some(lc);
        }
        if let Some(enc) = enc {
            open.enc = Some(enc);
            self.settle(&mut open, prov);
        }
        if let Voice::Heard(v) = &open.voice {
            if v.alive() {
                for (k, frame) in voice.iter().enumerate() {
                    let start = t + k as u64 * IMBE_FRAME_NS;
                    let span = Span {
                        start_ns: Timestamp(start),
                        end_ns: Timestamp(start + IMBE_FRAME_NS),
                    };
                    v.feed(span, frame);
                }
                open.heard = true;
            } else {
                // It exited or stopped answering (the seat noted why): this call stays
                // silent, the next starts it again.
                open.voice = Voice::Silent(Silent::VocoderFailed);
            }
        }
        if !open.announced && open.lc.is_some() && !matches!(open.voice, Voice::Held) {
            open.announced = true;
            events.push((t, Self::call_event(&open, false)));
        }
        self.open = Some(open);
    }

    /// A unit at `t` that is no call's: the channel event, when the network is new.
    fn no_call(&mut self, t: u64, nac: u16, events: &mut Events) {
        if self.open.is_some() {
            return;
        }
        let now = (nac, self.control_nac == Some(nac));
        if self.channel != Some(now) {
            self.channel = Some(now);
            let mut m = channel_event(PROTOCOL, now.1);
            m.insert("nac".into(), nac.into());
            events.push((t, serde_json::Value::Object(m)));
        }
    }

    /// The unit counts, into the job's context.
    fn note_counts(&self) {
        self.env.ctx.note(serde_json::json!({PROTOCOL: {
            "trunking_units": self.trunking_units,
            "packet_units": self.packet_units,
        }}));
    }
}

/// `t0` plus `samples` (maybe negative) at `rate_hz`, ns.
fn time_of(t0: Timestamp, samples: i64, rate_hz: u64) -> u64 {
    let dt = i128::from(samples) * 1_000_000_000 / i128::from(rate_hz);
    (i128::from(t0.0) + dt).clamp(0, i128::from(u64::MAX)) as u64
}

/// Decodes P25 phase 1 from a channel's IQ: two events per call, its speech, and a
/// channel event for a network with no call.
pub struct P25DecodeStage {
    spec: StageSpec,
}

impl P25DecodeStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: P25_DECODE_KIND,
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event, StreamKind::Audio],
                params_schema: schemars::schema_for!(P25Params),
            },
        }
    }
}

impl Default for P25DecodeStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for P25DecodeStage {
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
        let _: P25Params = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{P25_DECODE_KIND} params: {e}")))?;
        let [events_out, audio_out] = outputs.as_slice() else {
            return Err(StageError::Failed("expected 2 outputs".into()));
        };
        let events_out = events_out.clone();
        let drains = Arc::new(Mutex::new(Vec::new()));
        let mut calls = Calls {
            env: VocoderEnv {
                bus: bus.clone(),
                out: audio_out.clone(),
                ctx: ctx.clone(),
                drains: drains.clone(),
            },
            open: None,
            said_missing: false,
            channel: None,
            control_nac: None,
            trunking_units: 0,
            packet_units: 0,
        };
        let mut rx: Option<Receiver> = None;
        let mut last_prov: Option<Provenance> = None;
        let result = run_map_ending(self.spec().kind, inputs, bus, cancel, move |input| {
            let mut events = Vec::new();
            let frame = match input {
                Some(Input::Frame(f)) => f,
                Some(Input::Lagged { .. }) => {
                    // Start clean: the samples missed broke the symbol clock and the call.
                    rx = None;
                    calls.interrupt(&mut events);
                    return Ok(publish(&ctx, &events_out, last_prov.as_ref(), events));
                }
                None => {
                    // The input ended mid-call: it closes at its last unit, and its speech
                    // so far is still handed back.
                    calls.interrupt(&mut events);
                    return Ok(publish(&ctx, &events_out, last_prov.as_ref(), events));
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(format!(
                    "{P25_DECODE_KIND} expects iq frames"
                )));
            };
            let rate = block.sample_rate_hz.0;
            if rx.as_ref().is_some_and(|r| r.demod.rate_hz != rate) {
                // A new rate: the call so far closes and the receiver starts again.
                rx = None;
                calls.interrupt(&mut events);
            }
            let r = match &mut rx {
                Some(r) => r,
                None => rx.insert(Receiver::new(rate).map_err(StageError::Failed)?),
            };
            calls.expire(block.t0.0, &mut events);
            let counts = (calls.trunking_units, calls.packet_units);
            for (t, nac, unit) in r.feed(block) {
                calls.unit(t, nac, unit, &frame.provenance, &mut events);
            }
            let n = block.samples.len() as u64;
            calls.expire(time_of(block.t0, n as i64, rate), &mut events);
            if counts != (calls.trunking_units, calls.packet_units) {
                calls.note_counts();
            }
            last_prov = Some(frame.provenance.clone());
            Ok(publish(&ctx, &events_out, Some(&frame.provenance), events))
        })
        .await;
        let mut pending: Vec<JoinHandle<()>> = std::mem::take(&mut *drains.lock().unwrap());
        if result.is_ok() {
            // Every reader gets the one deadline; any still running after it is aborted
            // (its child is killed with it).
            let _ = tokio::time::timeout(VOCODER_DRAIN, async {
                for h in pending.iter_mut() {
                    let _ = h.await;
                }
            })
            .await;
        }
        pending.iter().for_each(JoinHandle::abort);
        result
    }
}

/// The events `(t, fields)` as frames on `out`, each in the row's envelope.
fn publish(
    ctx: &StageContext,
    out: &StreamId,
    prov: Option<&Provenance>,
    events: Events,
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
                end_ns: Timestamp(t + LDU_NS),
            };
            Frame::new(
                out.clone(),
                p,
                Payload::Event(event(ctx, t, PROTOCOL, fields)),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use num_complex::Complex;
    use serde_json::{Value, json};
    use tokio::sync::broadcast;
    use tokio::task::JoinHandle;
    use tokio_util::sync::CancellationToken;
    use zeta_core::port::IqBlock;
    use zeta_core::stage::{Stage, StageContext, StageError};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};

    use super::*;
    use crate::p25::{Call, encode_call, encode_control};
    use crate::synth::{awgn, c4fm_modulate, parse_params, synthesise};

    const RATE: u64 = 48_000;
    const NAC: u16 = 291;
    const SRC: u32 = 9_990_001;
    const DST: u32 = 65_000;
    /// The input's first block starts here, ns.
    const T0: u64 = 1_000_000_000;
    /// Samples per dibit at [`RATE`].
    const SPD: usize = RATE as usize / P25_BAUD as usize;
    /// One 100 ms input block at [`RATE`].
    const BLOCK: usize = RATE as usize / 10;
    /// Speech frames in a superframe.
    const SUPERFRAME_FRAMES: usize = 18;

    fn group() -> LinkControl {
        LinkControl {
            call: Call::Group,
            dst: DST,
            src: SRC,
            mfid: 0,
            emergency: false,
        }
    }

    fn clear() -> EncSync {
        EncSync {
            algid: P25_ALGID_CLEAR,
            kid: 0,
        }
    }

    /// `dibits` on the air at `rate`, then 10 ms with the carrier off.
    fn on_air(dibits: &[u8], rate: u64) -> Vec<Complex<f32>> {
        let mut iq = c4fm_modulate(dibits, P25_BAUD, P25_DEVIATION_HZ, rate as u32);
        iq.extend(std::iter::repeat_n(
            Complex::new(0.0, 0.0),
            rate as usize / 100,
        ));
        iq
    }

    /// A group call from [`SRC`] to [`DST`] of `superframes`, at [`RATE`].
    fn call(enc: EncSync, superframes: u32) -> Vec<Complex<f32>> {
        on_air(&encode_call(NAC, group(), enc, superframes), RATE)
    }

    /// `ms` of unmodulated carrier at [`RATE`].
    fn carrier(ms: usize) -> Vec<Complex<f32>> {
        vec![Complex::new(1.0, 0.0); RATE as usize * ms / 1000]
    }

    /// The synthesiser's signal for `p` at [`RATE`].
    fn synth(p: Value) -> Vec<Complex<f32>> {
        let (spec, imp) = parse_params(p.as_object().unwrap().clone()).unwrap();
        synthesise(&spec, &imp, RATE as u32, 0)
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

    fn iq_frame(samples: &[Complex<f32>], t0_ns: u64, rate: u64) -> Frame {
        Frame::new(
            StreamId::new(),
            prov(),
            Payload::Iq(IqBlock {
                t0: Timestamp(t0_ns),
                sample_rate_hz: Hz(rate),
                center_hz: Hz(450_000_000),
                samples: samples.to_vec().into(),
            }),
        )
    }

    /// The fake `zeta-mbe` (`tests/fake_zeta_mbe`) as the row's handler: a link in a
    /// temporary directory, absent until installed, so no test sets the process's
    /// environment and none sees another's program.
    struct Mbe {
        _dir: tempfile::TempDir,
        link: PathBuf,
    }

    impl Mbe {
        fn absent() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join(ZETA_MBE);
            Self { _dir: dir, link }
        }

        fn installed() -> Self {
            let m = Self::absent();
            m.install();
            m
        }

        fn install(&self) {
            let fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_zeta_mbe");
            std::os::unix::fs::symlink(fake, &self.link).unwrap();
        }

        /// The file the fake keeps beside its link.
        fn beside(&self, suffix: &str) -> PathBuf {
            PathBuf::from(format!("{}.{suffix}", self.link.display()))
        }

        /// The fake's next run exits 1 after 10 frames.
        fn die_once(&self) {
            std::fs::write(self.beside("die"), "").unwrap();
        }

        /// What the fake's runs logged: `start` per run, `eof` per input closed.
        fn log(&self) -> String {
            std::fs::read_to_string(self.beside("log")).unwrap_or_default()
        }

        /// The P25 row with its handler at the link.
        fn row(&self) -> Arc<zeta_core::modes::ModeRow> {
            let mut v: Value =
                serde_json::from_str(include_str!("../../../modes/p25_decode.json")).unwrap();
            v["decoder"]["handlers"][0]["programs"] = json!([self.link.to_str().unwrap()]);
            Arc::new(zeta_core::modes::parse(&v.to_string()).unwrap())
        }
    }

    /// The stage over the P25 row, and what it has published so far.
    struct Harness {
        tx: Option<broadcast::Sender<Frame>>,
        events_rx: broadcast::Receiver<Frame>,
        audio_rx: broadcast::Receiver<Frame>,
        notes: Arc<Mutex<Vec<Value>>>,
        stage: Option<JoinHandle<Result<(), StageError>>>,
        /// The next block's time, ns.
        t: u64,
        events: Vec<Value>,
        audio: Vec<Frame>,
    }

    impl Harness {
        /// The stage running with `mbe` as its vocoder, its input queue `capacity` deep.
        fn start(mbe: &Mbe, capacity: usize) -> Self {
            let bus = Arc::new(StreamBus::new(1024));
            let ev = bus.register(StreamKind::Event, prov());
            let au = bus.register(StreamKind::Audio, prov());
            let events_rx = bus.subscribe(&ev).unwrap();
            let audio_rx = bus.subscribe(&au).unwrap();
            let (tx, rx_in) = broadcast::channel(capacity);
            let notes = Arc::new(Mutex::new(Vec::new()));
            let n = notes.clone();
            let ctx = StageContext::new(SourceId("s".into()))
                .with_mode(mbe.row())
                .with_note(Arc::new(move |v| n.lock().unwrap().push(v)));
            let stage = tokio::spawn(async move {
                P25DecodeStage::new()
                    .run(
                        json!({}),
                        vec![rx_in],
                        vec![ev, au],
                        bus,
                        CancellationToken::new(),
                        ctx,
                    )
                    .await
            });
            Self {
                tx: Some(tx),
                events_rx,
                audio_rx,
                notes,
                stage: Some(stage),
                t: T0,
                events: vec![],
                audio: vec![],
            }
        }

        /// `iq` at `rate` in 100 ms blocks, contiguous in time, all at once: the stage
        /// (on this test's one thread) reads them when this task next waits.
        fn send(&mut self, iq: &[Complex<f32>], rate: u64) {
            for chunk in iq.chunks(rate as usize / 10) {
                let f = iq_frame(chunk, self.t, rate);
                self.tx.as_ref().unwrap().send(f).unwrap();
                self.t += chunk.len() as u64 * 1_000_000_000 / rate;
            }
        }

        /// As [`send`](Self::send), each block read by the stage before the next goes.
        async fn send_paced(&mut self, iq: &[Complex<f32>], rate: u64) {
            for chunk in iq.chunks(rate as usize / 10) {
                self.send(chunk, rate);
                while !self.tx.as_ref().unwrap().is_empty() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }
        }

        fn collect(&mut self) {
            while let Ok(f) = self.events_rx.try_recv() {
                let Payload::Event(v) = f.payload else {
                    panic!("expected an event")
                };
                self.events.push(v);
            }
            while let Ok(f) = self.audio_rx.try_recv() {
                self.audio.push(f);
            }
        }

        /// Poll every 10 ms for up to 10 s until `f` holds over what was published.
        async fn until(&mut self, f: impl Fn(&Self) -> bool) -> bool {
            for _ in 0..1000 {
                self.collect();
                if f(self) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            self.collect();
            f(self)
        }

        /// End the input, wait for the stage, then until `done`.
        async fn end(&mut self, done: impl Fn(&Self) -> bool) -> Result<(), StageError> {
            self.tx = None;
            let r = self.stage.take().unwrap().await.unwrap();
            self.until(done).await;
            r
        }

        /// The notes under `p25.vocoder`.
        fn vocoder_notes(&self) -> Vec<String> {
            self.notes
                .lock()
                .unwrap()
                .iter()
                .filter_map(|n| n[PROTOCOL]["vocoder"].as_str().map(String::from))
                .collect()
        }

        /// The last `p25.trunking_units` noted.
        fn trunking_units(&self) -> Option<u64> {
            self.notes
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find_map(|n| n[PROTOCOL]["trunking_units"].as_u64())
        }
    }

    /// `event` holds every field of `want`.
    fn has(event: &Value, want: Value) {
        for (k, v) in want.as_object().unwrap() {
            assert_eq!(&event[k], v, "{k}: {event}");
        }
    }

    fn t_ns(event: &Value) -> u64 {
        event["t_ns"].as_u64().unwrap()
    }

    /// Each audio frame is 20 ms of 48 kHz mono, 960 samples.
    fn check_audio(audio: &[Frame]) {
        for f in audio {
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = &f.payload
            else {
                panic!("expected audio")
            };
            assert_eq!(
                (sample_rate_hz.0, *channels, samples.len()),
                (48_000, 1, 960)
            );
            let span = &f.provenance.span;
            assert_eq!(span.end_ns.0 - span.start_ns.0, IMBE_FRAME_NS);
        }
    }

    #[tokio::test]
    async fn a_call_gives_two_events_and_audio() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        h.send(&call(clear(), 3), RATE);
        let r = h
            .end(|h| h.events.len() >= 2 && h.audio.len() >= 3 * SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.events.len(), 2, "{:?}", h.events);
        let (a, b) = (&h.events[0], &h.events[1]);
        has(
            a,
            json!({
                "type": "call", "protocol": "p25", "nac": 291, "src": SRC, "dst": DST,
                "call": "group", "encrypted": false, "emergency": false, "audio": true,
                "silent": null, "stream": null, "program": true, "eot": false,
                "mfid": 0, "algid": 128, "kid": 0, "access": "restricted"
            }),
        );
        assert_eq!(a["summary"], "9990001 > 65000: group", "{a}");
        has(
            b,
            json!({
                "type": "call", "src": SRC, "dst": DST, "eot": true, "frames": 54,
                "errors": 0, "audio": true, "silent": null, "program": true
            }),
        );
        assert_eq!(h.audio.len(), 54, "{} audio frames", h.audio.len());
        check_audio(&h.audio);
        // The speech starts with the first LDU1, the opening event's unit, and each
        // frame follows the last.
        let starts: Vec<u64> = h
            .audio
            .iter()
            .map(|f| f.provenance.span.start_ns.0)
            .collect();
        assert_eq!(starts[0], t_ns(a));
        assert!(
            starts.windows(2).all(|w| w[1] >= w[0] + IMBE_FRAME_NS),
            "{starts:?}"
        );
        assert!(h.vocoder_notes().is_empty(), "{:?}", h.vocoder_notes());
        // One vocoder, its input closed at the terminator.
        assert_eq!(mbe.log(), "start\neof\n");
    }

    #[tokio::test]
    async fn joining_mid_call_opens_within_a_superframe() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        let cut = RATE as usize * 400 / 1000;
        h.send(&call(clear(), 3)[cut..], RATE);
        let r = h.end(|h| h.events.len() >= 2).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.events.len(), 2, "{:?}", h.events);
        let (a, b) = (&h.events[0], &h.events[1]);
        has(
            a,
            json!({"src": SRC, "dst": DST, "nac": 291, "eot": false, "audio": true}),
        );
        has(b, json!({"src": SRC, "eot": true}));
        // The first unit whole after the cut: the second superframe's LDU1.
        let first_sync =
            T0 + ((HDU_DIBITS + 2 * LDU_DIBITS) * SPD - cut) as u64 * 1_000_000_000 / RATE;
        let opened = t_ns(a);
        assert!(
            opened + 1_000_000 >= first_sync && opened < first_sync + SUPERFRAME_NS,
            "opened {opened}, first sync {first_sync}"
        );
    }

    #[tokio::test]
    async fn an_encrypted_call_is_reported_and_silent() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        h.send(
            &call(
                EncSync {
                    algid: 0xAA,
                    kid: 7,
                },
                2,
            ),
            RATE,
        );
        let r = h.end(|h| h.events.len() >= 2).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.events.len(), 2, "{:?}", h.events);
        for e in &h.events {
            has(
                e,
                json!({
                    "src": SRC, "encrypted": true, "audio": false, "silent": "encrypted",
                    "algid": 170, "kid": 7, "program": false
                }),
            );
        }
        assert_eq!(h.events[0]["summary"], "9990001 > 65000: group encrypted");
        assert!(h.audio.is_empty(), "{} audio frames", h.audio.len());
        assert_eq!(mbe.log(), "", "the vocoder was started");
    }

    /// Two calls of one superframe, the second 200 ms of carrier after the first.
    fn two_calls() -> (Vec<Complex<f32>>, Vec<Complex<f32>>) {
        let mut second = carrier(200);
        second.extend(call(clear(), 1));
        (call(clear(), 1), second)
    }

    #[tokio::test]
    async fn no_vocoder_gives_events_and_says_so() {
        let mbe = Mbe::absent();
        let mut h = Harness::start(&mbe, 4096);
        let (first, second) = two_calls();
        h.send(&first, RATE);
        h.send(&second, RATE);
        let r = h.end(|h| h.events.len() >= 4).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.events.len(), 4, "{:?}", h.events);
        for e in &h.events {
            has(
                e,
                json!({"src": SRC, "audio": false, "silent": "no_vocoder", "program": false}),
            );
        }
        assert_eq!(h.events[0]["summary"], "9990001 > 65000: group no_vocoder");
        assert!(h.audio.is_empty(), "{} audio frames", h.audio.len());
        let notes = h.vocoder_notes();
        assert_eq!(notes.len(), 1, "once: {notes:?}");
        assert!(notes[0].contains("zeta-mbe"), "{notes:?}");
        assert!(notes[0].contains("tools/install-vocoders.sh"), "{notes:?}");
    }

    #[tokio::test]
    async fn a_vocoder_installed_mid_job_is_used_from_the_next_call() {
        let mbe = Mbe::absent();
        let mut h = Harness::start(&mbe, 4096);
        let (first, second) = two_calls();
        h.send(&first, RATE);
        assert!(h.until(|h| h.events.len() >= 2).await, "{:?}", h.events);
        mbe.install();
        h.send(&second, RATE);
        let r = h
            .end(|h| h.events.len() >= 4 && h.audio.len() >= SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.events.len(), 4, "{:?}", h.events);
        for e in &h.events[..2] {
            has(e, json!({"audio": false, "silent": "no_vocoder"}));
        }
        for e in &h.events[2..] {
            has(e, json!({"audio": true, "silent": null, "program": true}));
        }
        assert_eq!(h.audio.len(), SUPERFRAME_FRAMES);
        check_audio(&h.audio);
        let opened = t_ns(&h.events[2]);
        assert!(
            h.audio
                .iter()
                .all(|f| f.provenance.span.start_ns.0 >= opened)
        );
    }

    #[tokio::test]
    async fn a_dead_vocoder_marks_the_call() {
        let mbe = Mbe::installed();
        mbe.die_once();
        let mut h = Harness::start(&mbe, 4096);
        let first = call(clear(), 3);
        // The header and the first superframe: 18 frames, of which the fake answers 10.
        let split = (HDU_DIBITS + 2 * LDU_DIBITS + 60) * SPD;
        h.send(&first[..split], RATE);
        assert!(
            h.until(|h| h.vocoder_notes().iter().any(|n| n.contains("exited")))
                .await,
            "{:?}",
            h.vocoder_notes()
        );
        h.send(&first[split..], RATE);
        let mut second = carrier(200);
        second.extend(call(clear(), 1));
        h.send(&second, RATE);
        let r = h
            .end(|h| h.events.len() >= 4 && h.audio.len() >= 10 + SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.events.len(), 4, "{:?}", h.events);
        has(&h.events[0], json!({"audio": true, "silent": null}));
        has(
            &h.events[1],
            json!({
                "eot": true, "frames": 54, "audio": false, "silent": "vocoder_failed",
                "program": true
            }),
        );
        for e in &h.events[2..] {
            has(e, json!({"audio": true, "silent": null}));
        }
        assert_eq!(h.audio.len(), 10 + SUPERFRAME_FRAMES);
        let notes = h.vocoder_notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("zeta-mbe exited"), "{notes:?}");
    }

    #[tokio::test]
    async fn a_control_channel_gives_one_channel_event() {
        let mbe = Mbe::absent();
        let mut h = Harness::start(&mbe, 4096);
        h.send(&on_air(&encode_control(NAC, 40), RATE), RATE);
        let r = h
            .end(|h| !h.events.is_empty() && h.trunking_units() == Some(40))
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.events.len(), 1, "{:?}", h.events);
        let e = &h.events[0];
        has(
            e,
            json!({"type": "channel", "protocol": "p25", "nac": 291, "control": true}),
        );
        assert!(e.get("src").is_none(), "{e}");
        assert_eq!(h.trunking_units(), Some(40));
        assert!(h.vocoder_notes().is_empty(), "{:?}", h.vocoder_notes());
    }

    #[tokio::test]
    async fn a_call_cut_short_closes_after_its_timeout() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        // The header, three LDUs and part of the fourth, then a second of bare carrier;
        // the input stays open.
        let units = HDU_DIBITS + 3 * LDU_DIBITS;
        let mut iq = call(clear(), 3)[..(units + 400) * SPD].to_vec();
        iq.extend(carrier(1000));
        h.send(&iq, RATE);
        assert!(
            h.until(|h| h.events.len() >= 2 && mbe.log().contains("eof"))
                .await,
            "{:?} {:?}",
            h.events,
            mbe.log()
        );
        // The fourth LDU, its sync and NID whole, is read with the carrier as its second
        // half: four LDUs' frames, and the call closes where the fourth would have ended.
        let b = &h.events[1];
        has(
            b,
            json!({"src": SRC, "eot": true, "frames": 36, "audio": true}),
        );
        let ended = T0 + ((units + LDU_DIBITS) * SPD) as u64 * 1_000_000_000 / RATE;
        assert!(t_ns(b).abs_diff(ended) < 2_000_000, "{b}");
        assert!(h.until(|h| h.audio.len() >= 36).await);
        assert_eq!(h.audio.len(), 36);
        let r = h.end(|_| true).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.events.len(), 2, "{:?}", h.events);
    }

    #[tokio::test]
    async fn a_call_1_khz_off_centre_decodes() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        h.send(
            &synth(json!({
                "family": "p25", "nac": 291, "src": SRC, "dst": DST, "superframes": 3,
                "offset_hz": 1000.0
            })),
            RATE,
        );
        let r = h
            .end(|h| h.events.len() >= 2 && h.audio.len() >= 3 * SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.events.len(), 2, "{:?}", h.events);
        has(
            &h.events[0],
            json!({"src": SRC, "dst": DST, "nac": 291, "audio": true}),
        );
        has(&h.events[1], json!({"eot": true, "frames": 54}));
        assert_eq!(h.audio.len(), 54);
    }

    #[tokio::test]
    async fn other_signals_give_no_event() {
        let mut noise = carrier(2000);
        awgn(&mut noise, -10.0, 7);
        let signals = [
            ("noise", noise),
            (
                "nbfm tone",
                synth(json!({"family": "nbfm_tone", "tone_hz": 1000.0, "deviation_hz": 2500.0})),
            ),
            (
                "m17",
                synth(json!({
                    "family": "m17", "src": "ZETA1", "dst": "ALL", "can": 0, "frames": 25
                })),
            ),
            (
                "pocsag",
                synth(json!({
                    "family": "pocsag", "address": 1234567, "function": 3,
                    "text": "HELLO ZETA", "baud": 1200
                })),
            ),
        ];
        for (name, iq) in signals {
            let mbe = Mbe::installed();
            let mut h = Harness::start(&mbe, 4096);
            h.send(&iq, RATE);
            // Every event is published before the stage returns.
            let r = h.end(|_| true).await;
            assert!(r.is_ok(), "{name}: {r:?}");
            assert!(h.events.is_empty(), "{name}: {:?}", h.events);
            assert!(h.audio.is_empty(), "{name}");
            assert_eq!(mbe.log(), "", "{name}: the vocoder was started");
        }
    }

    /// The events' `eot`, in order.
    fn eots(events: &[Value]) -> Vec<bool> {
        events.iter().map(|e| e["eot"].as_bool().unwrap()).collect()
    }

    #[tokio::test]
    async fn a_lagged_input_closes_the_call() {
        let mbe = Mbe::absent();
        // A long call: its first 700 ms read, then 30 blocks into a queue of 8, so the
        // stage misses 22 of them and reads the rest with a new receiver.
        let iq = call(clear(), 20);
        let mut h = Harness::start(&mbe, 8);
        h.send_paced(&iq[..7 * BLOCK], RATE).await;
        assert!(h.until(|h| !h.events.is_empty()).await);
        h.send(&iq[7 * BLOCK..37 * BLOCK], RATE);
        h.send_paced(&iq[37 * BLOCK..], RATE).await;
        let r = h.end(|h| h.events.len() >= 4).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(
            eots(&h.events),
            [false, true, false, true],
            "{:?}",
            h.events
        );
        assert!(h.events.iter().all(|e| e["src"] == SRC), "{:?}", h.events);
        // Closed at its last unit before the lag; joined again after it.
        assert!(t_ns(&h.events[1]) < T0 + 800_000_000, "{}", h.events[1]);
        assert!(t_ns(&h.events[2]) > T0 + 2_900_000_000, "{}", h.events[2]);

        // A new rate mid-call: the call closes, and the next decodes at the new rate.
        let mut h = Harness::start(&mbe, 4096);
        h.send(&call(clear(), 3)[..7 * BLOCK], RATE);
        let slow = 24_000;
        h.send(&on_air(&encode_call(NAC, group(), clear(), 2), slow), slow);
        let r = h.end(|h| h.events.len() >= 4).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(
            eots(&h.events),
            [false, true, false, true],
            "{:?}",
            h.events
        );
        assert!(t_ns(&h.events[1]) < T0 + 800_000_000, "{}", h.events[1]);
        has(&h.events[3], json!({"src": SRC, "dst": DST, "frames": 36}));
    }

    #[test]
    fn params_are_strict() {
        assert!(serde_json::from_value::<P25Params>(json!({})).is_ok());
        assert!(serde_json::from_value::<P25Params>(json!({"x": 1})).is_err());
        let stage = P25DecodeStage::new();
        assert_eq!(stage.spec().kind, P25_DECODE_KIND);
        assert_eq!(stage.spec().inputs, [StreamKind::Iq]);
        assert_eq!(stage.spec().outputs, [StreamKind::Event, StreamKind::Audio]);
    }
}
