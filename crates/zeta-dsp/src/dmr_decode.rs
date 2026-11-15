//! DMR decode stage (design 38, 5.5 and 5.10): the 4-level FSK front end ([`crate::fsk4`],
//! root raised cosine) and the framer ([`crate::dmr`]) turn a channel's IQ into bursts;
//! this stage turns those into call and channel events and both time slots' speech.
//!
//! **Calls.** One open call per slot: a repeater's slot 1 and slot 2, and a handheld's
//! bursts (no slot, `slot: null`), each followed on its own. A voice LC header opens one;
//! so does a privacy header, or a voice burst, when none is open on its slot or the burst
//! does not follow on from the open call: another link control, a burst A off the call's
//! superframes, or a burst more than [`GRID_NS`] off the slot's 60 ms grid or more than a
//! superframe after the call's last burst. A burst on the grid after lost ones stays in the
//! call, its speech held again (below): a lost burst costs speech and no events. A
//! terminator closes its slot's call at its time; another data burst on the slot (idle, a
//! control block) closes it at its last burst's end, and so does a new call on the slot.
//! Two `type: call` events per call ([`crate::voice_call`]):
//! - the opening one once its link control and its encryption are both in hand: at the
//!   first burst after a header (a voice burst, or the privacy header that marks it
//!   encrypted), or within a superframe when the job joins mid-call;
//! - the closing one (`eot: true`) at its terminator, when no burst has come for longer
//!   than [`DMR_SUPERFRAME_MS`] (noticed two periods later), at a lagged input, a new
//!   sample rate or the input's end, at the call's own end.
//!
//! **Encryption.** Two marks, either enough: the privacy bit of the call's link control (the
//! header's, then each one its embedded signalling or terminator gives; only a decodable
//! link control saying clear clears it), and a privacy header seen on the call's slot. The
//! EMB's PI bit is not one ([`crate::dmr`]). An encrypted call gets its events and no
//! vocoder; nothing here decrypts. A call that turns encrypted closes its vocoder's input
//! at once (the speech fed before is still handed back); one that turns clear starts one.
//!
//! **When speech is fed.** No voice frame reaches a vocoder before the word that says its
//! call is clear is in hand:
//! - A call opened by a voice LC header with a decodable link control is licensed by it:
//!   at the burst after the header (no privacy header came, so only the link control's mark
//!   can say encrypted) every burst is fed as it comes, while each comes one period after
//!   the last.
//! - Any other call (opened by a voice burst or a privacy header, or a header whose link
//!   control did not decode), and a header's call from its first lost burst on, holds each
//!   superframe's frames until that superframe's own word: a link control completed on one
//!   of its bursts from fragments all of this call's bursts since it opened or since its
//!   last loss (at burst E from B to E, or at B or D across the boundary from the previous
//!   superframe's), from which its encryption is settled; or, when the previous superframe
//!   was fed and nothing was lost since, an embedded message that is not a link control
//!   (a talker alias, GPS: -2 5.2.1.2, table 5.4), which keeps the call's link control and
//!   encryption as they were. Then that superframe's held frames and its later bursts are
//!   fed. A superframe whose word never comes is never fed. A link control is never taken
//!   from a superframe before the call's.
//!
//! Under a header's licence, a link control that turns the call encrypted mid-superframe
//! stops it from that burst, after at most bursts A-D of that superframe.
//!
//! **Speech.** Each slot has its own audio stream (`slot1`, `slot2`, output roles of the
//! same names; a handheld's speech is on `slot1`) and each clear call its own vocoder,
//! `zeta-mbe ambe3600x2450` (the row's handler 0, optional), looked up when the call is
//! first known to be clear, so a program installed while the job runs is used from the next
//! call. Each voice burst's three AMBE+2 frames go to it, each with a 20 ms span from the
//! burst's time, through the vocoder seat ([`crate::vocoder`]). Without the program every
//! call is reported with `audio: false`, `silent: "no_vocoder"`, and `job_context.dmr.vocoder`
//! says how to install it, once. A vocoder that fails mid-call ends that call's audio
//! (`silent: "vocoder_failed"` on its closing event), whatever its speech does after (held
//! again, encrypted, clear again); the next call starts it again.
//!
//! **The main stream.** The job's audio stream with no role carries what the job's audio
//! policy (the `audio` param) says, from both slots' speech ([`crate::main_stream`]): a call
//! holds or joins it from its first frame fed and leaves it when its audio ends (its call
//! ends, turns encrypted or its vocoder fails); a call with no audio never holds it, so a
//! call whose speech has not yet been fed (held, encrypted, without a vocoder) is never
//! given it. A call held again after a lost burst has had audio and is the same call: under
//! `first_active` and `follow` it keeps its place, so one lost burst does not hand the main
//! stream to the other slot mid-sentence; under `all` it stays in the mix. Either way every
//! frame a call is fed reaches the main stream when the policy puts it there: while a call's
//! speech is held the main stream waits for it, at most the four periods and [`GRID_NS`] a
//! held frame is kept, and never stalls. After one lost burst a call is licensed superframe
//! by superframe for the rest of the call, each superframe's speech waiting for its own
//! word: about 240 ms of added latency from then on, on every stream. Each call event's
//! `stream` names its slot's stream, and `program` says whether its speech is on the main
//! stream: now on the opening event, at any time on the closing one.
//!
//! **A channel with no call.** A data burst decoded with no call open on any slot (an idle
//! burst, a control block, a stray terminator), on a repeater once both slots' latest bursts
//! were such (so a call whose start was missed on the other slot is not taken for an idle
//! repeater), gives one `type: channel` event for its colour code, and another only when
//! the colour code changes or control blocks are first seen on it (`control`): a repeater
//! idle after a call gives one, the first time.
//!
//! **The receiver.** The front end's counters over the job are in `job_context.dmr.receiver`
//! (`locks`, `false_locks`, `losses`, `off_deviation`), updated when they change.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::ops::AudioPolicy;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, Span, StreamId, StreamKind, Timestamp};

use crate::dmr::{
    AMBE_FEED_BYTES, AmbeSoft, DMR_BAUD, DMR_COAST_SEARCHES, DMR_DEVIATION_HZ, DMR_RRC_ALPHA,
    DMR_SUPERFRAME_MS, DmrBurst, DmrContent, DmrFramer, FullLc, Origin,
};
use crate::fsk4::{Fsk4Demod, Fsk4Params, Fsk4Tally, Shape};
use crate::main_stream::MainStream;
use crate::util::time_of;
use crate::vocoder::{Vocoder, VocoderEnv, VocoderSpec};
use crate::voice_call::{
    CallFields, Events, MBE_FRAME_SAMPLES, MBE_RATE_HZ, ProgramLookup, Silent, ZETA_MBE,
    channel_event, publish,
};
use crate::{Input, run_map_ending};

/// The stage's kind and the row's id.
pub const DMR_DECODE_KIND: &str = "dmr_decode";
/// The events' `protocol` and the job context key.
const PROTOCOL: &str = "dmr";
/// `zeta-mbe`'s argument for DMR's AMBE+2 (3600 bit/s with FEC, 2450 of speech).
pub const AMBE_CODEC: &str = "ambe3600x2450";
/// The voices: the roles of the two slots' audio streams, in output order.
pub const VOICE_ROLES: [&str; 2] = ["slot1", "slot2"];
/// One AMBE+2 frame, ns (20 ms).
const FRAME_NS: u64 = MBE_FRAME_SAMPLES as u64 * 1_000_000_000 / MBE_RATE_HZ;
/// AMBE+2 frames in a voice burst (-1 6.1).
const BURST_FRAMES: u64 = 3;
/// From one burst of a slot to its next, ns: two 30 ms TDMA slots (-1 4.2.2). A burst's
/// three frames span it.
const PERIOD_NS: u64 = BURST_FRAMES * FRAME_NS;
/// Bursts in a voice superframe, A to F (-1 5.1.2).
const SUPERFRAME_BURSTS: u64 = 6;
/// A voice superframe, ns.
const SUPERFRAME_NS: u64 = DMR_SUPERFRAME_MS * 1_000_000;
/// How long after a call's last burst ended it is closed with no burst since, ns: a
/// superframe, and two periods, since a burst that began within it is only handed on once
/// it is whole and the front end gives held symbols out late.
const EXPIRE_NS: u64 = SUPERFRAME_NS + 2 * PERIOD_NS;
/// How far a burst may sit from where its slot's grid puts it and still follow on, ns:
/// the timing's wander, far less than the 30 ms to the other slot's bursts.
pub const GRID_NS: u64 = 10_000_000;
/// How long before the input's latest sample a burst not yet handed on can have started,
/// ns: the burst (27.5 ms) and the front end's held symbols (296 for DMR, 62 ms) once
/// whole, with margin.
const LATE_NS: u64 = 3 * PERIOD_NS;
/// Bursts of a superframe before E, whose fragments (B to E) complete a link control on E.
const E_SPAN_BURSTS: u64 = 3;
/// Bursts back to the oldest fragment of a link control completed across a superframe's
/// boundary, on B (C, D, E then B) or D (E then B, C, D).
const ACROSS_SPAN_BURSTS: u64 = 5;
/// Burst E's letter (0 for A).
const LETTER_E: u8 = 4;

/// `dmr_decode`'s one parameter: the job's audio policy for its main stream.
#[derive(Default, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct DmrParams {
    #[serde(default)]
    pub audio: AudioPolicy,
}

/// The receiver over one sample rate: the front end, the framer, and how many samples it
/// has taken (its positions count from its first).
struct Receiver {
    demod: Fsk4Demod,
    framer: DmrFramer,
    /// Whether the front end has been turned to the framer's polarity.
    turned: bool,
    fed: u64,
    /// The last block's start time and first sample, which a flush's bursts are timed by.
    last: Option<(Timestamp, u64)>,
}

impl Receiver {
    fn new(rate_hz: u64) -> Result<Self, String> {
        let params = Fsk4Params {
            symbol_rate_baud: DMR_BAUD,
            deviation_hz: DMR_DEVIATION_HZ,
            shape: Shape::Rrc {
                alpha: DMR_RRC_ALPHA,
            },
            coast_searches: DMR_COAST_SEARCHES,
        };
        Ok(Self {
            demod: Fsk4Demod::new(rate_hz, params)
                .map_err(|e| format!("{DMR_DECODE_KIND}: {e}"))?,
            framer: DmrFramer::new(),
            turned: false,
            fed: 0,
            last: None,
        })
    }

    /// The bursts `block` completes, each with its first symbol's time.
    fn feed(&mut self, block: &zeta_core::port::IqBlock) -> Vec<(u64, DmrBurst)> {
        let start = self.fed;
        self.fed += block.samples.len() as u64;
        self.last = Some((block.t0, start));
        let dibits = self.demod.feed(&block.samples);
        self.bursts(&dibits)
    }

    /// The bursts the front end still held when the input ended.
    fn flush(&mut self) -> Vec<(u64, DmrBurst)> {
        let dibits = self.demod.flush();
        self.bursts(&dibits)
    }

    /// The bursts `dibits` complete, timed from the last block.
    fn bursts(&mut self, dibits: &[crate::fsk4::SoftDibit]) -> Vec<(u64, DmrBurst)> {
        let bursts = self.framer.push(dibits);
        if let Some(inverted) = self.framer.inverted()
            && inverted != self.turned
        {
            self.demod.invert(inverted);
            self.turned = inverted;
        }
        let rate = self.demod.rate_hz;
        let Some((t0, start)) = self.last else {
            return Vec::new();
        };
        bursts
            .into_iter()
            .map(|b| (time_of(t0, b.at.round() as i64 - start as i64, rate), b))
            .collect()
    }
}

/// What a call's speech is doing.
enum Voice {
    /// Nothing has told its encryption yet: nothing is fed.
    Held,
    Heard(Vocoder),
    Silent(Silent),
}

/// What licenses a call's speech.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Licence {
    /// Its voice LC header, while each burst follows the last.
    Header,
    /// Each superframe's own word.
    Superframe,
}

/// How a voice burst stands to the open call on its slot.
#[derive(Debug, PartialEq, Eq)]
enum Follows {
    /// One period after its last burst.
    Next,
    /// On its grid, a burst or more lost in between: the same call, its speech held again.
    AfterLoss,
    /// Another call.
    No,
}

/// A slot's call. Times are ns.
struct Open {
    /// The slot's index in [`Calls::slots`]: 0 a handheld's, 1 and 2 a repeater's.
    key: usize,
    origin: Origin,
    color_code: Option<u8>,
    lc: Option<FullLc>,
    /// The last decodable link control's privacy bit.
    lc_encrypted: bool,
    /// A privacy header was seen for it.
    privacy: bool,
    licence: Licence,
    /// The burst A of the superframe last licensed by its own word.
    licensed: Option<u64>,
    voice: Voice,
    /// Its vocoder failed (to start, or during the call): it stays silent to its end,
    /// whatever its speech does after (held, encrypted, clear again).
    failed: bool,
    /// Frames of bursts not yet licensed, with their bursts' times, oldest first.
    held: Vec<(u64, Box<[AmbeSoft; 3]>)>,
    /// The burst A of the latest superframe whose phase is known.
    phase: Option<u64>,
    /// Since when its bursts have followed each other: its first, or its first after a loss.
    since: u64,
    /// Its last burst's time.
    last_at: u64,
    /// Where its last burst's period ends: its speech's end.
    end_ns: u64,
    announced: bool,
    frames: u32,
    errors: u32,
    /// The main stream's id for its audio, while its audio is live.
    audio: Option<u64>,
    /// The end of its last frame fed.
    fed_end: u64,
    /// Whether its speech has been on the main stream.
    program: bool,
}

impl Open {
    fn new(key: usize, origin: Origin, t: u64, licence: Licence) -> Self {
        Self {
            key,
            origin,
            color_code: None,
            lc: None,
            lc_encrypted: false,
            privacy: false,
            licence,
            licensed: None,
            voice: Voice::Held,
            failed: false,
            held: Vec::new(),
            phase: None,
            since: t,
            last_at: t,
            end_ns: t + PERIOD_NS,
            announced: false,
            frames: 0,
            errors: 0,
            audio: None,
            fed_end: 0,
            program: false,
        }
    }

    /// Its slot's voice: slot 2's stream, else slot 1's.
    fn voice_index(&self) -> usize {
        usize::from(self.key == 2)
    }

    fn encrypted(&self) -> bool {
        self.lc_encrypted || self.privacy
    }

    /// Why its speech is not heard now; `None` while a live vocoder has it.
    fn silent(&self) -> Option<Silent> {
        match &self.voice {
            Voice::Heard(v) if v.alive() => None,
            Voice::Heard(_) => Some(Silent::VocoderFailed),
            Voice::Silent(s) => Some(*s),
            Voice::Held => None,
        }
    }

    /// Whether a link control completed on a burst at `t` with letter `letter` came from
    /// fragments all of this call's bursts since it opened or was last lost.
    fn exact(&self, t: u64, letter: Option<u8>) -> bool {
        let back = if letter == Some(LETTER_E) {
            E_SPAN_BURSTS
        } else {
            ACROSS_SPAN_BURSTS
        };
        t + GRID_NS >= self.since + back * PERIOD_NS
    }

    /// How a voice burst at `t`, with `letter` and the link control `lc` (when it is this
    /// call's), stands to this call: another call under another link control, at a burst A
    /// off its superframes, off the slot's grid or more than a superframe after its last
    /// burst; else the next burst, or the same call after lost ones.
    fn follows(&self, t: u64, letter: Option<u8>, lc: Option<&FullLc>) -> Follows {
        let same = |a: &FullLc, b: &FullLc| (a.call, a.src, a.dst) == (b.call, b.src, b.dst);
        if matches!((&self.lc, lc), (Some(a), Some(b)) if !same(a, b)) || t <= self.last_at {
            return Follows::No;
        }
        let gap = t - self.last_at;
        let periods = (gap + PERIOD_NS / 2) / PERIOD_NS;
        if gap.abs_diff(periods * PERIOD_NS) > GRID_NS {
            return Follows::No;
        }
        if let (Some(l), Some(a)) = (letter, self.phase) {
            let at = (t.saturating_sub(a) + PERIOD_NS / 2) / PERIOD_NS % SUPERFRAME_BURSTS;
            if t + GRID_NS < a || at != u64::from(l) {
                return Follows::No;
            }
        }
        match periods {
            1 => Follows::Next,
            n if (n - 1) * PERIOD_NS <= SUPERFRAME_NS => Follows::AfterLoss,
            _ => Follows::No,
        }
    }

    /// Whether its speech is held now: its latest burst waits for a word.
    fn held_now(&self) -> bool {
        self.licence == Licence::Superframe && !self.in_licensed(self.last_at)
    }

    /// Whether the burst at `t` is in the superframe last licensed by its own word.
    fn in_licensed(&self, t: u64) -> bool {
        self.licensed
            .is_some_and(|a| t + GRID_NS >= a && t + GRID_NS < a + SUPERFRAME_NS)
    }
}

/// A burst's slot as an index: 0 for none (a handheld), else 1 or 2.
fn slot_key(slot: Option<u8>) -> usize {
    match slot {
        Some(s @ 1..=2) => usize::from(s),
        _ => 0,
    }
}

/// A voice burst.
struct VoiceBurst {
    origin: Origin,
    color_code: Option<u8>,
    letter: Option<u8>,
    lc: Option<FullLc>,
    other: Option<u8>,
    voice: Box<[AmbeSoft; 3]>,
    errors: u32,
}

/// The stage's calls: bursts in, events out, speech to the vocoders and the main stream.
struct Calls {
    /// Each voice's vocoder seat (slot 1's stream, slot 2's).
    envs: [VocoderEnv; 2],
    ctx: StageContext,
    main: MainStream,
    /// The open call of each slot: a handheld's, slot 1's, slot 2's.
    slots: [Option<Open>; 3],
    lookup: ProgramLookup,
    /// The colour code and `control` the last channel event named.
    reported: Option<(u8, bool)>,
    /// The colour code control blocks were last seen on.
    control_cc: Option<u8>,
    /// Whether each slot's latest burst was a data burst of no call (idle, a control block,
    /// a terminator): a handheld's, slot 1's, slot 2's.
    quiet: [bool; 3],
    /// The latest burst's time.
    last_burst: u64,
}

impl Calls {
    /// A vocoder for a clear call on `voice`, or why there is none.
    fn start(&mut self, voice: usize, prov: &Provenance) -> Voice {
        let Some(program) = self.lookup.find(&self.ctx, PROTOCOL) else {
            return Voice::Silent(Silent::NoVocoder);
        };
        let spec = VocoderSpec {
            name: ZETA_MBE,
            program,
            args: vec![AMBE_CODEC.into()],
            feed_bytes: AMBE_FEED_BYTES,
            feed_samples: MBE_FRAME_SAMPLES,
            rate_hz: MBE_RATE_HZ,
            context: PROTOCOL,
        };
        match Vocoder::spawn(&self.envs[voice], &spec, prov.clone()) {
            Ok(v) => Voice::Heard(v),
            Err(e) => {
                self.ctx.note(serde_json::json!({PROTOCOL: {"vocoder": e}}));
                Voice::Silent(Silent::VocoderFailed)
            }
        }
    }

    /// `o`'s audio ends: off the main stream, noting whether it was ever on it.
    fn end_audio(&mut self, o: &mut Open) {
        if let Some(id) = o.audio.take() {
            o.program |= self.main.end(id, o.fed_end);
        }
    }

    /// Settle `o`'s speech on its encryption now that a word has told it: stop its vocoder
    /// when it is encrypted, start one when it is (or turns) clear.
    fn settle(&mut self, o: &mut Open, prov: &Provenance) {
        let voice = std::mem::replace(&mut o.voice, Voice::Held);
        o.voice = match (voice, o.encrypted()) {
            // Its input closed before this burst's frames: what it was fed came from bursts
            // nothing had yet marked encrypted, and that speech is still handed back.
            (Voice::Heard(v), true) => {
                self.envs[o.voice_index()].park(v);
                self.end_audio(o);
                Voice::Silent(Silent::Encrypted)
            }
            (_, true) => Voice::Silent(Silent::Encrypted),
            (Voice::Held | Voice::Silent(Silent::Encrypted), false) if o.failed => {
                Voice::Silent(Silent::VocoderFailed)
            }
            (Voice::Held | Voice::Silent(Silent::Encrypted), false) => {
                self.start(o.voice_index(), prov)
            }
            (other, false) => other,
        };
        if matches!(o.voice, Voice::Silent(Silent::VocoderFailed)) {
            o.failed = true;
        }
    }

    /// Feed a burst's frames from `t` to `o`'s vocoder, if it has a live one.
    fn feed(&mut self, o: &mut Open, t: u64, frames: &[AmbeSoft; 3]) {
        let voice = o.voice_index();
        let Voice::Heard(v) = &o.voice else {
            return;
        };
        if !v.alive() {
            // It exited or stopped answering (the seat noted why): this call stays silent,
            // the next starts it again.
            o.voice = Voice::Silent(Silent::VocoderFailed);
            o.failed = true;
            self.main.gone(voice, o.fed_end);
            self.end_audio(o);
            return;
        }
        if o.audio.is_none() {
            o.audio = Some(self.main.begin(voice, t));
        }
        for (k, frame) in frames.iter().enumerate() {
            let start = t + k as u64 * FRAME_NS;
            let span = Span {
                start_ns: Timestamp(start),
                end_ns: Timestamp(start + FRAME_NS),
            };
            self.main.fed(voice, span.clone());
            v.feed(span, frame);
        }
        o.fed_end = t + PERIOD_NS;
    }

    /// `o`'s event; `eot` for its closing one.
    fn call_event(&self, o: &Open, eot: bool) -> serde_json::Value {
        let Some(lc) = o.lc else {
            unreachable!("a call is announced with its link control")
        };
        let silent = o.silent();
        let program = if eot {
            o.program
        } else {
            o.audio.is_some_and(|id| self.main.on_main(id))
        };
        let mut m = CallFields {
            protocol: PROTOCOL,
            src: lc.src,
            dst: lc.dst,
            call: lc.call.word(),
            encrypted: o.encrypted(),
            emergency: lc.emergency,
            silent,
            stream: Some(VOICE_ROLES[o.voice_index()]),
            program,
            frames: o.frames,
            errors: o.errors,
            eot,
        }
        .to_json();
        m.insert("color_code".into(), o.color_code.into());
        m.insert("slot".into(), (o.key != 0).then_some(o.key as u8).into());
        let origin = match o.origin {
            Origin::Repeater => "repeater",
            Origin::Handheld => "handheld",
        };
        m.insert("origin".into(), origin.into());
        m.insert("fid".into(), lc.fid.into());
        // The summary's channel text: none on a call, which reads as who calls whom.
        m.insert("network".into(), serde_json::Value::Null);
        serde_json::Value::Object(m)
    }

    /// Announce `o` once its link control and its encryption are in hand.
    fn announce(&mut self, o: &mut Open, t: u64, events: &mut Events) {
        if !o.announced && o.lc.is_some() && !matches!(o.voice, Voice::Held) {
            o.announced = true;
            events.push((t, self.call_event(o, false)));
        }
    }

    /// End `key`'s call at `t`: its closing event, when it was announced.
    fn close(&mut self, key: usize, t: u64, events: &mut Events) {
        let Some(mut o) = self.slots[key].take() else {
            return;
        };
        if let Voice::Heard(v) = &o.voice
            && !v.alive()
        {
            self.main.gone(o.voice_index(), o.fed_end);
        }
        self.end_audio(&mut o);
        if o.announced {
            let e = self.call_event(&o, true);
            events.push((t, e));
        }
        let voice = o.voice_index();
        if let Voice::Heard(v) = o.voice {
            self.envs[voice].park(v);
        }
    }

    /// End `key`'s call because another came at `t`: at its own end, or `t` if sooner.
    fn close_for(&mut self, key: usize, t: u64, events: &mut Events) {
        if let Some(end) = self.slots[key].as_ref().map(|o| o.end_ns) {
            self.close(key, end.min(t), events);
        }
    }

    /// End each call, at its own end, once no burst has followed for longer than a
    /// superframe at `now` ([`EXPIRE_NS`]).
    fn expire(&mut self, now: u64, events: &mut Events) {
        for key in 0..self.slots.len() {
            if let Some(end) = self.slots[key].as_ref().map(|o| o.end_ns)
                && now > end + EXPIRE_NS
            {
                self.close(key, end, events);
            }
        }
    }

    /// The input stopped making sense (lagged, a new rate, its end): each open call closes
    /// at its own end.
    fn interrupt(&mut self, events: &mut Events) {
        for key in 0..self.slots.len() {
            if let Some(end) = self.slots[key].as_ref().map(|o| o.end_ns) {
                self.close(key, end, events);
            }
        }
    }

    /// One burst from the framer, its first symbol at `t`.
    fn burst(&mut self, t: u64, b: DmrBurst, prov: &Provenance, events: &mut Events) {
        self.last_burst = self.last_burst.max(t);
        let key = slot_key(b.slot);
        self.quiet[key] = matches!(
            b.content,
            DmrContent::Terminator(_)
                | DmrContent::Idle
                | DmrContent::Control
                | DmrContent::Other(_)
        );
        match b.content {
            DmrContent::VoiceHeader(lc) => {
                self.close_for(key, t, events);
                let licence = if lc.is_some() {
                    Licence::Header
                } else {
                    Licence::Superframe
                };
                let mut o = Open::new(key, b.origin, t, licence);
                o.color_code = b.color_code;
                o.lc = lc;
                o.lc_encrypted = lc.is_some_and(|lc| lc.encrypted);
                self.slots[key] = Some(o);
            }
            DmrContent::PrivacyHeader => self.privacy_header(key, t, &b, prov, events),
            DmrContent::Voice {
                letter,
                lc,
                other,
                voice,
                errors,
                ..
            } => {
                let burst = VoiceBurst {
                    origin: b.origin,
                    color_code: b.color_code,
                    letter,
                    lc,
                    other,
                    voice,
                    errors,
                };
                self.voice(key, t, burst, prov, events);
            }
            DmrContent::Terminator(_) if self.slots[key].is_some() => self.close(key, t, events),
            DmrContent::Control => {
                self.control_cc = b.color_code;
                self.close_for(key, t, events);
                self.no_call(key, t, b.color_code, events);
            }
            DmrContent::Terminator(_) | DmrContent::Idle | DmrContent::Other(_) => {
                self.close_for(key, t, events);
                self.no_call(key, t, b.color_code, events);
            }
        }
    }

    /// A privacy header at `t`: the slot's call, or a new one, is encrypted.
    fn privacy_header(
        &mut self,
        key: usize,
        t: u64,
        b: &DmrBurst,
        prov: &Provenance,
        events: &mut Events,
    ) {
        let mut o = match self.slots[key].take() {
            Some(o) if t > o.last_at && t - o.last_at <= PERIOD_NS + GRID_NS => o,
            other => {
                self.slots[key] = other;
                self.close_for(key, t, events);
                Open::new(key, b.origin, t, Licence::Superframe)
            }
        };
        o.privacy = true;
        o.last_at = t;
        o.end_ns = t + PERIOD_NS;
        o.color_code = b.color_code.or(o.color_code);
        self.settle(&mut o, prov);
        self.announce(&mut o, t, events);
        self.slots[key] = Some(o);
    }

    /// A voice burst at `t` on slot `key`.
    fn voice(
        &mut self,
        key: usize,
        t: u64,
        burst: VoiceBurst,
        prov: &Provenance,
        events: &mut Events,
    ) {
        // A link control is the open call's only when its fragments all are.
        let theirs = |o: &Open| burst.lc.filter(|_| o.exact(t, burst.letter));
        let follows = self.slots[key].as_ref().map_or(Follows::No, |o| {
            o.follows(t, burst.letter, theirs(o).as_ref())
        });
        let mut o = match follows {
            Follows::Next => self.slots[key].take().expect("an open call"),
            Follows::AfterLoss => {
                let mut o = self.slots[key].take().expect("an open call");
                // Lost bursts could hide anything: each superframe from here needs its own
                // word, and what was held before the loss is no longer this stretch's.
                o.licence = Licence::Superframe;
                o.licensed = None;
                o.held.clear();
                o.since = t;
                o
            }
            Follows::No => {
                self.close_for(key, t, events);
                Open::new(key, burst.origin, t, Licence::Superframe)
            }
        };
        o.last_at = t;
        o.end_ns = t + PERIOD_NS;
        o.frames += BURST_FRAMES as u32;
        o.errors += burst.errors;
        o.color_code = burst.color_code.or(o.color_code);
        // This burst's superframe, by its burst A, when its letter is known.
        let sf = burst
            .letter
            .map(|l| t.saturating_sub(u64::from(l) * PERIOD_NS));
        if sf.is_some() {
            o.phase = sf;
        }
        // The word this burst brings, if any.
        let lc = burst.lc.filter(|_| o.exact(t, burst.letter));
        let other = burst.other.filter(|_| o.exact(t, burst.letter));
        let mut told = o.licence == Licence::Header;
        let mut licenses = None;
        if let Some(lc) = lc {
            o.lc = Some(lc);
            o.lc_encrypted = lc.encrypted;
            told = true;
            licenses = sf;
        }
        if other.is_some()
            && let (Some(a), Some(last)) = (sf, o.licensed)
            && a.abs_diff(last + SUPERFRAME_NS) <= GRID_NS
        {
            // A talker alias or GPS in place of the link control: the call goes on as it
            // was, licensed as its last superframe was.
            licenses = Some(a);
        }
        if o.licence == Licence::Superframe
            && let Some(a) = licenses
        {
            o.licensed = Some(a);
        }
        if told {
            self.settle(&mut o, prov);
        }
        match o.licence {
            Licence::Header => self.feed(&mut o, t, &burst.voice),
            Licence::Superframe if o.in_licensed(t) => {
                let held = std::mem::take(&mut o.held);
                for (at, frames) in held {
                    if o.in_licensed(at) {
                        self.feed(&mut o, at, &frames);
                    }
                }
                self.feed(&mut o, t, &burst.voice);
            }
            Licence::Superframe => {
                // No word licenses it yet; a later word licenses at most four bursts back.
                if matches!(
                    o.voice,
                    Voice::Held | Voice::Heard(_) | Voice::Silent(Silent::Encrypted)
                ) {
                    o.held.push((t, burst.voice));
                }
                let oldest = t.saturating_sub(4 * PERIOD_NS + GRID_NS);
                o.held.retain(|(at, _)| *at >= oldest);
            }
        }
        self.announce(&mut o, t, events);
        self.slots[key] = Some(o);
    }

    /// A data burst at `t` on slot `key` that is no call's: the channel event, when no
    /// call is open, a repeater's other slot's latest burst was one of no call too (not a
    /// call whose start was missed), and its colour code is not the one the last channel
    /// event named.
    fn no_call(&mut self, key: usize, t: u64, cc: Option<u8>, events: &mut Events) {
        let both = key == 0 || (self.quiet[1] && self.quiet[2]);
        if !both || self.slots.iter().any(Option::is_some) {
            return;
        }
        let Some(cc) = cc else {
            return;
        };
        let now = (cc, self.control_cc == Some(cc));
        if self.reported != Some(now) {
            self.reported = Some(now);
            events.push((t, channel_fields(now.0, now.1)));
        }
    }

    /// How far the main stream's decisions are final, the input read up to `now`: no frame
    /// will be fed, and no call's audio begin or end, before it. A burst still to come
    /// starts after the latest one (give or take a period) and no more than [`LATE_NS`]
    /// before `now`; a held frame may still be fed; a call whose audio is live may end
    /// where its last frame fed does.
    ///
    /// A held frame always holds the bound back, so no speech a call is fed is ever missing
    /// from the main stream; held frames are kept at most four periods and [`GRID_NS`]
    /// back, so a held call delays the main stream by at most that and never stalls it.
    /// Under `all` a call whose speech is held does not hold the bound back by its last
    /// frame fed (its end decides nothing for the mix); under `first_active` and `follow`
    /// it does, since where it ends decides who holds the main stream next.
    fn note_known(&mut self, now: u64) {
        let mut until = self
            .last_burst
            .saturating_sub(PERIOD_NS)
            .max(now.saturating_sub(LATE_NS));
        let mixing = self.main.policy() == AudioPolicy::All;
        for o in self.slots.iter().flatten() {
            if let Some((at, _)) = o.held.first() {
                until = until.min(*at);
            }
            if o.audio.is_some() && !(mixing && o.held_now()) {
                until = until.min(o.fed_end);
            }
        }
        self.main.known(until);
    }
}

/// A channel event's fields: [`channel_event`]'s, the colour code, and `network`, the
/// text the row's summary shows for it (`channel, colour code 5` or `control channel,
/// colour code 5`; a call event's `network` is null, so its summary reads `src > dst:
/// call silent`).
fn channel_fields(cc: u8, control: bool) -> serde_json::Value {
    let mut m = channel_event(PROTOCOL, control);
    m.insert("color_code".into(), cc.into());
    let kind = if control {
        "control channel"
    } else {
        "channel"
    };
    m.insert("network".into(), format!("{kind}, colour code {cc}").into());
    serde_json::Value::Object(m)
}

/// Decodes DMR from a channel's IQ: two events per call on either slot, each slot's
/// speech on its own stream and the main stream by the audio policy, and a channel event
/// for a repeater with no call.
pub struct DmrDecodeStage {
    spec: StageSpec,
}

impl DmrDecodeStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: DMR_DECODE_KIND,
                inputs: vec![StreamKind::Iq],
                outputs: vec![
                    StreamKind::Event,
                    StreamKind::Audio,
                    StreamKind::Audio,
                    StreamKind::Audio,
                ],
                params_schema: schemars::schema_for!(DmrParams),
            },
        }
    }
}

impl Default for DmrDecodeStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for DmrDecodeStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn output_roles(&self, _params: &serde_json::Value) -> Vec<Option<&'static str>> {
        vec![None, None, Some(VOICE_ROLES[0]), Some(VOICE_ROLES[1])]
    }

    fn voice_roles(&self, _params: &serde_json::Value) -> Vec<&'static str> {
        VOICE_ROLES.to_vec()
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
        let params: DmrParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{DMR_DECODE_KIND} params: {e}")))?;
        let [events_out, main_out, slot1, slot2] = outputs.as_slice() else {
            return Err(StageError::Failed("expected 4 outputs".into()));
        };
        let events_out = events_out.clone();
        let drains = Arc::new(Mutex::new(Vec::new()));
        let env = |out: &StreamId| VocoderEnv {
            bus: bus.clone(),
            out: out.clone(),
            ctx: ctx.clone(),
            drains: drains.clone(),
        };
        let envs = [env(slot1), env(slot2)];
        let seat = envs[0].clone();
        let Some((main, writer)) =
            MainStream::spawn(params.audio, &bus, main_out.clone(), [slot1, slot2])
        else {
            return Err(StageError::Failed(format!(
                "{DMR_DECODE_KIND}: a slot's audio stream is not registered"
            )));
        };
        let mut calls = Calls {
            envs,
            ctx: ctx.clone(),
            main,
            slots: [None, None, None],
            lookup: ProgramLookup::default(),
            reported: None,
            control_cc: None,
            quiet: [false; 3],
            last_burst: 0,
        };
        let mut rx: Option<Receiver> = None;
        let mut tally = Fsk4Tally::default();
        let mut last_prov: Option<Provenance> = None;
        let result = run_map_ending(self.spec().kind, inputs, bus, cancel, |input| {
            let mut events = Vec::new();
            let frame = match input {
                Some(Input::Frame(f)) => f,
                Some(Input::Lagged { .. }) => {
                    // Start clean: the samples missed broke the symbol clock and the calls.
                    tally.retire(rx.take().map(|r| r.demod.counters()));
                    calls.interrupt(&mut events);
                    return Ok(publish(
                        &ctx,
                        &events_out,
                        last_prov.as_ref(),
                        PROTOCOL,
                        events,
                        PERIOD_NS,
                    ));
                }
                None => {
                    // The input ended: the bursts the front end still held (a terminator
                    // among them, perhaps) first, then each call still open closes at its
                    // own end, and its speech so far is still handed back.
                    if let (Some(r), Some(prov)) = (&mut rx, last_prov.as_ref()) {
                        for (t, b) in r.flush() {
                            calls.burst(t, b, prov, &mut events);
                        }
                    }
                    tally.note(&ctx, PROTOCOL, rx.as_ref().map(|r| r.demod.counters()));
                    calls.interrupt(&mut events);
                    return Ok(publish(
                        &ctx,
                        &events_out,
                        last_prov.as_ref(),
                        PROTOCOL,
                        events,
                        PERIOD_NS,
                    ));
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(format!(
                    "{DMR_DECODE_KIND} expects iq frames"
                )));
            };
            let rate = block.sample_rate_hz.0;
            if rx.as_ref().is_some_and(|r| r.demod.rate_hz != rate) {
                // A new rate: the calls so far close and the receiver starts again.
                tally.retire(rx.take().map(|r| r.demod.counters()));
                calls.interrupt(&mut events);
            }
            let r = match &mut rx {
                Some(r) => r,
                None => rx.insert(Receiver::new(rate).map_err(StageError::Failed)?),
            };
            calls.expire(block.t0.0, &mut events);
            for (t, b) in r.feed(block) {
                calls.burst(t, b, &frame.provenance, &mut events);
            }
            let now = time_of(block.t0, block.samples.len() as i64, rate);
            calls.expire(now, &mut events);
            calls.note_known(now);
            tally.note(&ctx, PROTOCOL, rx.as_ref().map(|r| r.demod.counters()));
            last_prov = Some(frame.provenance.clone());
            Ok(publish(
                &ctx,
                &events_out,
                Some(&frame.provenance),
                PROTOCOL,
                events,
                PERIOD_NS,
            ))
        })
        .await;
        // Both slots' vocoders share one drain; then the main stream writes what it holds.
        seat.drain_all(&result).await;
        drop(calls);
        if result.is_ok() {
            let _ = writer.await;
        } else {
            writer.abort();
        }
        result
    }
}

#[cfg(test)]
mod tests {
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
    use crate::dmr::build::{self, Burst};
    use crate::dmr::{Call, DMR_BAUD, DMR_DEVIATION_HZ, DMR_RRC_ALPHA};
    use crate::fsk4::dibit_symbol;
    use crate::main_stream::sample_index;
    use crate::synth::{
        awgn, parse_params, rrc_modulate_4fsk, rrc_modulate_4fsk_gated, synthesise,
    };
    use crate::voice_call::fake_mbe;

    const RATE: u64 = 48_000;
    /// The test identities: invented, outside registered ranges.
    const CC: u8 = 5;
    const SRC: u32 = 9_990_001;
    const DST: u32 = 9_990;
    /// The other slot's call.
    const SRC_2: u32 = 9_990_002;
    const DST_2: u32 = 9_991;
    /// The input's first block starts here, ns.
    const T0: u64 = 1_000_000_000;
    /// One 100 ms input block at [`RATE`].
    const BLOCK: usize = RATE as usize / 10;
    /// A TDMA frame (60 ms: a CACH and a burst for each slot), samples.
    const TDMA: usize = 2880;
    /// Speech frames in a superframe.
    const SUPERFRAME_FRAMES: usize = 18;
    /// How far a burst's time (its first symbol, as the front end finds it) may sit from
    /// where the test placed it, ns: well under a frame (20 ms).
    const SLACK_NS: u64 = 2_000_000;

    fn group() -> FullLc {
        FullLc {
            call: Call::Group,
            dst: DST,
            src: SRC,
            fid: 0,
            encrypted: false,
            emergency: false,
        }
    }

    fn other() -> FullLc {
        FullLc {
            src: SRC_2,
            dst: DST_2,
            ..group()
        }
    }

    fn encrypted(lc: FullLc) -> FullLc {
        FullLc {
            encrypted: true,
            ..lc
        }
    }

    /// `n` samples at [`RATE`], ns.
    fn ns(samples: usize) -> u64 {
        samples as u64 * 1_000_000_000 / RATE
    }

    /// When slot `slot`'s burst of TDMA frame `frame` starts, from the input's start: after
    /// its CACH (12 dibits), and slot 2's half a frame later.
    fn burst_at(frame: usize, slot: u8) -> u64 {
        T0 + ns(frame * TDMA + 120 + usize::from(slot - 1) * TDMA / 2)
    }

    /// `dibits` on the air at [`RATE`], then 10 ms with the carrier off.
    fn on_air(dibits: &[u8]) -> Vec<Complex<f32>> {
        let symbols: Vec<u8> = dibits.iter().map(|&d| dibit_symbol(d)).collect();
        let mut iq = rrc_modulate_4fsk(
            &symbols,
            DMR_BAUD,
            DMR_DEVIATION_HZ,
            RATE as u32,
            DMR_RRC_ALPHA,
        );
        iq.extend(std::iter::repeat_n(
            Complex::new(0.0, 0.0),
            RATE as usize / 100,
        ));
        iq
    }

    /// A repeater's downlink of the two slots' bursts on the air.
    fn repeater(slot1: Vec<Burst>, slot2: Vec<Burst>) -> Vec<Complex<f32>> {
        on_air(&build::downlink(CC, [slot1, slot2]))
    }

    /// A handheld's bursts on the air, the carrier off between them.
    fn handheld(bursts: &[Burst]) -> Vec<Complex<f32>> {
        let symbols: Vec<Option<u8>> = build::handheld(bursts)
            .into_iter()
            .map(|d| d.map(dibit_symbol))
            .collect();
        let mut iq = rrc_modulate_4fsk_gated(
            &symbols,
            DMR_BAUD,
            DMR_DEVIATION_HZ,
            RATE as u32,
            DMR_RRC_ALPHA,
        );
        iq.extend(std::iter::repeat_n(
            Complex::new(0.0, 0.0),
            RATE as usize / 100,
        ));
        iq
    }

    /// A repeater call's bursts: the header (and a privacy header when encrypted), the
    /// superframes, the terminator.
    fn call(lc: FullLc, superframes: u32) -> Vec<Burst> {
        build::call(CC, &lc, superframes, Origin::Repeater, false)
    }

    fn idles(n: usize) -> Vec<Burst> {
        vec![build::idle(CC); n]
    }

    /// A burst nothing decodes.
    fn lost() -> Burst {
        let mut b = build::idle(CC);
        build::lose(&mut b);
        b
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

    /// The fake `zeta-mbe` as the dmr_decode row's handler ([`crate::voice_call::fake_mbe`]).
    struct DmrRow;

    impl fake_mbe::Row for DmrRow {
        const JSON: &'static str = include_str!("../../../modes/dmr_decode.json");
    }

    type Mbe = fake_mbe::Mbe<DmrRow>;

    /// The stage over the DMR row, and what it has published so far: the events, the main
    /// stream and each slot's.
    struct Harness {
        tx: Option<broadcast::Sender<Frame>>,
        events_rx: broadcast::Receiver<Frame>,
        audio_rx: [broadcast::Receiver<Frame>; 3],
        notes: Arc<Mutex<Vec<Value>>>,
        stage: Option<JoinHandle<Result<(), StageError>>>,
        /// The next block's time, ns.
        t: u64,
        /// The call events.
        events: Vec<Value>,
        /// The channel events.
        channels: Vec<Value>,
        main: Vec<Frame>,
        slot1: Vec<Frame>,
        slot2: Vec<Frame>,
    }

    impl Harness {
        /// The stage running with `mbe` as its vocoder, its input queue `capacity` deep,
        /// under the audio policy `policy` (its name).
        fn start(mbe: &Mbe, capacity: usize, policy: &str) -> Self {
            let bus = Arc::new(StreamBus::new(4096));
            let ev = bus.register(StreamKind::Event, prov());
            let audio = [(); 3].map(|_| bus.register(StreamKind::Audio, prov()));
            let events_rx = bus.subscribe(&ev).unwrap();
            let audio_rx = audio.clone().map(|a| bus.subscribe(&a).unwrap());
            let (tx, rx_in) = broadcast::channel(capacity);
            let notes = Arc::new(Mutex::new(Vec::new()));
            let n = notes.clone();
            let ctx = StageContext::new(SourceId("s".into()))
                .with_mode(mbe.row())
                .with_note(Arc::new(move |v| n.lock().unwrap().push(v)));
            let [main, slot1, slot2] = audio;
            let params = json!({"audio": policy});
            let stage = tokio::spawn(async move {
                DmrDecodeStage::new()
                    .run(
                        params,
                        vec![rx_in],
                        vec![ev, main, slot1, slot2],
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
                channels: vec![],
                main: vec![],
                slot1: vec![],
                slot2: vec![],
            }
        }

        /// `iq` at `rate` in 100 ms blocks, contiguous in time, all at once.
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

        /// As [`send`](Self::send), each 100 ms block sent when its time has come, as a
        /// radio's would be: where the main stream's cursor stands depends on how far the
        /// input has got, which the faster helpers outrun.
        async fn send_real_time(&mut self, iq: &[Complex<f32>], rate: u64) {
            let start = tokio::time::Instant::now();
            for (k, chunk) in iq.chunks(rate as usize / 10).enumerate() {
                tokio::time::sleep_until(start + Duration::from_millis(100 * k as u64)).await;
                self.send(chunk, rate);
            }
        }

        fn collect(&mut self) {
            while let Ok(f) = self.events_rx.try_recv() {
                let Payload::Event(v) = f.payload else {
                    panic!("expected an event")
                };
                if v["type"] == "channel" {
                    self.channels.push(v);
                } else {
                    self.events.push(v);
                }
            }
            let [m, a, b] = &mut self.audio_rx;
            for (rx, into) in [
                (m, &mut self.main),
                (a, &mut self.slot1),
                (b, &mut self.slot2),
            ] {
                while let Ok(f) = rx.try_recv() {
                    into.push(f);
                }
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

        /// The notes under `dmr.vocoder`.
        fn vocoder_notes(&self) -> Vec<String> {
            self.notes
                .lock()
                .unwrap()
                .iter()
                .filter_map(|n| n[PROTOCOL]["vocoder"].as_str().map(String::from))
                .collect()
        }

        /// The last `dmr.receiver` noted.
        fn receiver(&self) -> Option<Value> {
            self.notes
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find_map(|n| n[PROTOCOL].get("receiver").cloned())
        }

        /// The events of the call from `src`, in order.
        fn of(&self, src: u32) -> Vec<&Value> {
            self.events.iter().filter(|e| e["src"] == src).collect()
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

    /// The events' `eot`, in order.
    fn eots(events: &[Value]) -> Vec<bool> {
        events.iter().map(|e| e["eot"].as_bool().unwrap()).collect()
    }

    fn starts(audio: &[Frame]) -> Vec<u64> {
        audio.iter().map(|f| f.provenance.span.start_ns.0).collect()
    }

    fn samples(f: &Frame) -> &[f32] {
        let Payload::Audio { samples, .. } = &f.payload else {
            panic!("expected audio")
        };
        samples
    }

    /// The level of `audio`, one run of the fake: its last sample, which every sample
    /// after the first frame (the resampler's start) is within 2 % of.
    fn level(audio: &[Frame]) -> f32 {
        let l = *samples(audio.last().unwrap()).last().unwrap();
        assert!(l > 0.0, "silent");
        for f in &audio[1..] {
            assert!(
                samples(f).iter().all(|&s| (s - l).abs() < 0.02 * l),
                "one level: {l} vs {:?}",
                samples(f)
            );
        }
        l
    }

    /// Whether `a` and `b` carry the same speech: the same spans and samples.
    fn same(a: &[Frame], b: &[&Frame]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(x, y)| x.provenance.span == y.provenance.span && samples(x) == samples(y))
    }

    /// Each audio frame is 20 ms of 48 kHz mono, 960 samples, and each starts after the
    /// last.
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
            assert_eq!(span.end_ns.0 - span.start_ns.0, FRAME_NS);
        }
        let s = starts(audio);
        assert!(s.windows(2).all(|w| w[1] > w[0]), "{s:?}");
    }

    /// The latest end of any audio frame's span.
    fn audio_end(audio: &[Frame]) -> u64 {
        audio
            .iter()
            .map(|f| f.provenance.span.end_ns.0)
            .max()
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn a_repeater_call_gives_two_events_and_audio() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(&repeater(call(group(), 3), idles(20)), RATE);
        let r = h
            .end(|h| h.events.len() >= 2 && h.slot1.len() >= 54 && h.main.len() >= 54)
            .await;
        assert!(r.is_ok(), "{r:?}");
        // The speech first: each frame on slot 1's stream and the main stream, none on
        // slot 2's.
        assert_eq!(h.slot1.len(), 54, "{} slot 1 frames", h.slot1.len());
        assert_eq!(h.main.len(), 54, "{} main frames", h.main.len());
        assert!(h.slot2.is_empty(), "{} slot 2 frames", h.slot2.len());
        check_audio(&h.slot1);
        assert!(same(&h.main, &h.slot1.iter().collect::<Vec<_>>()));
        level(&h.slot1);
        assert_eq!(h.events.len(), 2, "{:?}", h.events);
        let (a, b) = (&h.events[0], &h.events[1]);
        has(
            a,
            json!({
                "type": "call", "protocol": "dmr", "color_code": 5, "slot": 1,
                "origin": "repeater", "src": SRC, "dst": DST, "call": "group", "fid": 0,
                "encrypted": false, "emergency": false, "audio": true, "silent": null,
                "stream": "slot1", "program": true, "eot": false, "access": "restricted"
            }),
        );
        assert_eq!(a["summary"], "9990001 > 9990: group", "{a}");
        has(
            b,
            json!({
                "type": "call", "src": SRC, "dst": DST, "eot": true, "frames": 54,
                "errors": 0, "audio": true, "silent": null, "stream": "slot1", "program": true
            }),
        );
        // The speech starts with the first voice burst, the opening event's, and the call
        // closes at its terminator.
        assert_eq!(h.slot1[0].provenance.span.start_ns.0, t_ns(a));
        assert!(t_ns(a).abs_diff(burst_at(1, 1)) < SLACK_NS, "{a}");
        assert!(t_ns(b).abs_diff(burst_at(19, 1)) < SLACK_NS, "{b}");
        assert!(h.vocoder_notes().is_empty(), "{:?}", h.vocoder_notes());
        // One vocoder, its input closed at the terminator.
        assert_eq!(mbe.log(), "start\neof\n");
        // Slot 2 idle throughout, slot 1 idle after the call: the idle repeater, once.
        assert_eq!(h.channels.len(), 1, "{:?}", h.channels);
        has(
            &h.channels[0],
            json!({"type": "channel", "color_code": 5, "control": false}),
        );
        assert!(t_ns(&h.channels[0]) > t_ns(b), "{}", h.channels[0]);
    }

    /// Slot 1's call of three superframes from the first TDMA frame; slot 2's of four from
    /// the seventh: it starts a superframe after slot 1's and ends two after it.
    fn two_slots() -> Vec<Complex<f32>> {
        let mut slot2 = idles(6);
        slot2.extend(call(other(), 4));
        repeater(call(group(), 3), slot2)
    }

    /// Each slot's stream carries its own call's frames and only those: `n` of them, one
    /// run of the fake each, inside the call's events.
    fn own_frames(h: &Harness) {
        for (audio, src, n) in [(&h.slot1, SRC, 54), (&h.slot2, SRC_2, 72)] {
            assert_eq!(audio.len(), n, "{src}");
            check_audio(audio);
            let call = h.of(src);
            let (from, to) = (t_ns(call[0]), t_ns(call[1]));
            assert!(
                starts(audio).iter().all(|&s| s >= from && s < to),
                "{src}: {:?} not in {from}..{to}",
                starts(audio)
            );
        }
        assert_ne!(level(&h.slot1), level(&h.slot2));
    }

    async fn run_two_slots(policy: &str) -> (Mbe, Harness) {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, policy);
        h.send(&two_slots(), RATE);
        let r = h
            .end(|h| h.events.len() >= 4 && h.slot1.len() >= 54 && h.slot2.len() >= 72)
            .await;
        assert!(r.is_ok(), "{policy}: {r:?}");
        (mbe, h)
    }

    #[tokio::test]
    async fn both_slots_decode_at_once() {
        for policy in ["first_active", "follow", "all"] {
            let (mbe, h) = run_two_slots(policy).await;
            own_frames(&h);
            assert_eq!(h.events.len(), 4, "{policy}: {:?}", h.events);
            for (src, dst, slot, frames) in [(SRC, DST, 1, 54), (SRC_2, DST_2, 2, 72)] {
                let call = h.of(src);
                assert_eq!(call.len(), 2, "{policy}");
                let stream = format!("slot{slot}");
                for e in &call {
                    has(
                        e,
                        json!({
                            "dst": dst, "slot": slot, "stream": stream, "audio": true,
                            "silent": null, "color_code": 5
                        }),
                    );
                }
                has(call[1], json!({"eot": true, "frames": frames}));
            }
            // Slot 2's call opens a superframe after slot 1's and ends two after it.
            let (one, two) = (h.of(SRC), h.of(SRC_2));
            assert!(t_ns(one[0]) < t_ns(two[0]) && t_ns(one[1]) < t_ns(two[1]));
            assert_eq!(mbe.log(), "start\nstart\neof\neof\n", "{policy}");
        }
    }

    #[tokio::test]
    async fn first_active_keeps_the_main_stream_on_the_first_call() {
        let (_mbe, h) = run_two_slots("first_active").await;
        // The speech first: the main stream is slot 1's call, frame for frame.
        assert!(same(&h.main, &h.slot1.iter().collect::<Vec<_>>()));
        let one = h.of(SRC);
        let (from, to) = (t_ns(one[0]), t_ns(one[1]));
        assert!(starts(&h.main).iter().all(|&s| s >= from && s < to));
        for e in one {
            has(e, json!({"program": true}));
        }
        for e in h.of(SRC_2) {
            has(e, json!({"program": false}));
        }
    }

    #[tokio::test]
    async fn follow_switches_when_the_first_call_ends() {
        let (_mbe, h) = run_two_slots("follow").await;
        // The speech first: slot 1's call, then slot 2's from the end of slot 1's last
        // frame to its own end.
        let ended = audio_end(&h.slot1);
        let want: Vec<&Frame> = h
            .slot1
            .iter()
            .chain(
                h.slot2
                    .iter()
                    .filter(|f| f.provenance.span.start_ns.0 >= ended),
            )
            .collect();
        assert!(
            same(&h.main, &want),
            "{:?} vs {:?}",
            starts(&h.main),
            want.iter()
                .map(|f| f.provenance.span.start_ns.0)
                .collect::<Vec<_>>()
        );
        let got = starts(&h.main);
        assert!(got.windows(2).all(|w| w[1] > w[0]), "{got:?}");
        for e in h.of(SRC) {
            has(e, json!({"program": true}));
        }
        let two = h.of(SRC_2);
        has(two[0], json!({"program": false}));
        has(two[1], json!({"program": true}));
    }

    #[tokio::test]
    async fn all_mixes_both() {
        let (_mbe, h) = run_two_slots("all").await;
        // The speech first: each main sample is the sum of the slots' samples there,
        // saturated; before slot 2's call and after slot 1's, the one running alone.
        let (one, two) = (place(&h.slot1), place(&h.slot2));
        let covered = |audio: &[Frame], i: u128| {
            audio.iter().any(|f| {
                let first = sample_index(f.provenance.span.start_ns.0);
                (first..first + 960).contains(&i)
            })
        };
        let (mut both, mut alone) = (0, 0);
        assert!(!h.main.is_empty());
        for f in &h.main {
            let base = sample_index(f.provenance.span.start_ns.0);
            for (k, &s) in samples(f).iter().enumerate() {
                let i = base + k as u128;
                let (a, b) = (one.get(&i), two.get(&i));
                let want = (a.unwrap_or(&0.0) + b.unwrap_or(&0.0)).clamp(-1.0, 1.0);
                assert!((s - want).abs() < 1e-6, "sample {i}: {s} vs {want}");
                match (a, b) {
                    (Some(_), Some(_)) => both += 1,
                    (Some(_), None) | (None, Some(_)) => alone += 1,
                    _ => {}
                }
            }
        }
        // Every sample of either slot is on the main stream.
        let total: usize = h.main.len() * 960;
        assert!(both > 0 && alone > 0, "{both} {alone}");
        assert!(both + alone <= total);
        let union = |i: u128| covered(&h.slot1, i) || covered(&h.slot2, i);
        let span = |audio: &[Frame]| {
            (
                sample_index(audio[0].provenance.span.start_ns.0),
                sample_index(audio_end(audio)),
            )
        };
        let (a0, a1) = span(&h.slot1);
        let (b0, b1) = span(&h.slot2);
        let slot_samples = (a0.min(b0)..a1.max(b1)).filter(|&i| union(i)).count();
        assert_eq!(both + alone, slot_samples);
        let spans = starts(&h.main);
        assert!(
            spans.windows(2).all(|w| w[1] == w[0] + FRAME_NS),
            "{spans:?}"
        );
        for call in [h.of(SRC), h.of(SRC_2)] {
            for e in call {
                has(e, json!({"program": true}));
            }
        }
    }

    #[tokio::test]
    async fn a_handheld_call_has_a_null_slot_and_uses_slot1() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(
            &handheld(&build::call(CC, &group(), 3, Origin::Handheld, false)),
            RATE,
        );
        let r = h
            .end(|h| h.events.len() >= 2 && h.slot1.len() >= 54 && h.main.len() >= 54)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.slot1.len(), 54);
        assert_eq!(h.main.len(), 54);
        assert!(h.slot2.is_empty());
        check_audio(&h.slot1);
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        for e in &h.events {
            has(
                e,
                json!({
                    "origin": "handheld", "slot": null, "stream": "slot1", "src": SRC,
                    "color_code": 5, "audio": true, "program": true
                }),
            );
        }
        has(&h.events[1], json!({"frames": 54}));
    }

    /// An encrypted call's both events, and no speech anywhere.
    fn silent_and_encrypted(h: &Harness, mbe: &Mbe, what: &str) {
        assert!(
            h.slot1.is_empty() && h.slot2.is_empty() && h.main.is_empty(),
            "{what}: audio"
        );
        assert_eq!(mbe.log(), "", "{what}: the vocoder was started");
        assert_eq!(eots(&h.events), [false, true], "{what}: {:?}", h.events);
        for e in &h.events {
            has(
                e,
                json!({
                    "src": SRC, "encrypted": true, "audio": false, "silent": "encrypted",
                    "program": false
                }),
            );
        }
        assert_eq!(
            h.events[0]["summary"], "9990001 > 9990: group encrypted",
            "{what}"
        );
    }

    #[tokio::test]
    async fn an_encrypted_call_is_reported_and_silent() {
        // Marked by its link control alone: the privacy header left out.
        let mut by_lc = call(encrypted(group()), 2);
        by_lc.remove(1);
        // Marked by a privacy header alone: the link control says clear.
        let mut by_header = call(group(), 2);
        by_header.insert(1, build::privacy_header(CC, Origin::Repeater));
        // Joined mid-call, marked by its link control alone: the speech is held until the
        // superframe's link control says encrypted. Slot 2's idle burst before the call is
        // heard is no channel event: slot 1's latest burst is not known to be idle.
        let mut joined = by_lc.clone();
        joined[0] = lost();
        for (what, bursts) in [
            ("link control", by_lc),
            ("privacy header", by_header),
            ("joined", joined),
        ] {
            let mbe = Mbe::installed();
            let mut h = Harness::start(&mbe, 4096, "first_active");
            h.send(&repeater(bursts, idles(14)), RATE);
            let r = h.end(|h| h.events.len() >= 2).await;
            assert!(r.is_ok(), "{what}: {r:?}");
            silent_and_encrypted(&h, &mbe, what);
            if let Some(c) = h.channels.first() {
                assert!(t_ns(c) > t_ns(&h.events[1]), "{what}: {c}");
            }
        }

        // Under follow an encrypted call that started first does not hold the main
        // stream: a clear call on slot 2 a superframe later has it from its first frame.
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "follow");
        let mut slot2 = idles(6);
        slot2.extend(call(other(), 2));
        h.send(&repeater(call(encrypted(group()), 3), slot2), RATE);
        let r = h
            .end(|h| h.events.len() >= 4 && h.slot2.len() >= 36 && h.main.len() >= 36)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert!(h.slot1.is_empty());
        assert_eq!(starts(&h.main), starts(&h.slot2));
        assert_eq!(h.slot2.len(), 36);
        for e in h.of(SRC) {
            has(e, json!({"encrypted": true, "program": false}));
        }
        for e in h.of(SRC_2) {
            has(
                e,
                json!({"encrypted": false, "audio": true, "program": true}),
            );
        }
    }

    #[tokio::test]
    async fn an_idle_repeater_gives_one_channel_event() {
        for (p, summary, control) in [
            (json!({"idle": true}), "channel, colour code 5", false),
            (
                json!({"control": true}),
                "control channel, colour code 5",
                true,
            ),
        ] {
            let mut p = p;
            for (k, v) in json!({
                "family": "dmr", "color_code": 5, "src": SRC, "dst": DST, "superframes": 3
            })
            .as_object()
            .unwrap()
            {
                p[k] = v.clone();
            }
            let mbe = Mbe::absent();
            let mut h = Harness::start(&mbe, 4096, "first_active");
            h.send(&synth(p), RATE);
            let r = h.end(|h| !h.channels.is_empty()).await;
            assert!(r.is_ok(), "{r:?}");
            assert!(h.events.is_empty(), "{:?}", h.events);
            assert_eq!(h.channels.len(), 1, "{:?}", h.channels);
            let e = &h.channels[0];
            has(
                e,
                json!({
                    "type": "channel", "protocol": "dmr", "color_code": 5, "control": control
                }),
            );
            assert!(e.get("src").is_none(), "{e}");
            assert_eq!(e["summary"], summary, "{e}");
            assert!(h.vocoder_notes().is_empty(), "{:?}", h.vocoder_notes());
        }
    }

    /// Two calls on slot 1 of a superframe each, three idle bursts between them.
    fn two_calls() -> Vec<Complex<f32>> {
        let mut slot1 = call(group(), 1);
        let first = slot1.len();
        slot1.extend(idles(3));
        slot1.extend(call(group(), 1));
        repeater(slot1, idles(2 * first + 3))
    }

    #[tokio::test]
    async fn no_vocoder_gives_events_and_says_so() {
        let mbe = Mbe::absent();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(&two_calls(), RATE);
        let r = h.end(|h| h.events.len() >= 4).await;
        assert!(r.is_ok(), "{r:?}");
        assert!(h.slot1.is_empty() && h.slot2.is_empty() && h.main.is_empty());
        assert_eq!(
            eots(&h.events),
            [false, true, false, true],
            "{:?}",
            h.events
        );
        for e in &h.events {
            has(
                e,
                json!({"src": SRC, "audio": false, "silent": "no_vocoder", "program": false}),
            );
        }
        assert_eq!(h.events[0]["summary"], "9990001 > 9990: group no_vocoder");
        let notes = h.vocoder_notes();
        assert_eq!(notes.len(), 1, "once: {notes:?}");
        assert!(notes[0].contains("zeta-mbe"), "{notes:?}");
        assert!(notes[0].contains("tools/install-vocoders.sh"), "{notes:?}");
    }

    #[tokio::test]
    async fn a_dead_vocoder_marks_the_call() {
        let mbe = Mbe::installed();
        mbe.die_once();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        let mut slot1 = call(group(), 3);
        let first = slot1.len();
        slot1.extend(idles(3));
        slot1.extend(call(group(), 1));
        let iq = repeater(slot1, idles(2 * first + 3));
        // The header and the first superframe: 18 frames, of which the fake answers 10.
        let split = 8 * TDMA;
        h.send(&iq[..split], RATE);
        assert!(
            h.until(|h| h.vocoder_notes().iter().any(|n| n.contains("exited")))
                .await,
            "{:?}",
            h.vocoder_notes()
        );
        h.send(&iq[split..], RATE);
        let r = h
            .end(|h| {
                h.events.len() >= 4
                    && h.slot1.len() >= 10 + SUPERFRAME_FRAMES
                    && h.main.len() >= 10 + SUPERFRAME_FRAMES
            })
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.slot1.len(), 10 + SUPERFRAME_FRAMES);
        assert_eq!(starts(&h.main), starts(&h.slot1));
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
            has(e, json!({"audio": true, "silent": null, "program": true}));
        }
        let notes = h.vocoder_notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("zeta-mbe exited"), "{notes:?}");
    }

    #[tokio::test]
    async fn joining_mid_call_opens_within_a_superframe() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        let cut = RATE as usize * 400 / 1000;
        h.send(&repeater(call(group(), 3), idles(20))[cut..], RATE);
        let r = h
            .end(|h| h.events.len() >= 2 && h.slot1.len() >= 2 * SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        // The first burst whole after the cut: the second superframe's A, TDMA frame 7.
        let first_sync = burst_at(7, 1) - ns(cut);
        // Its speech: the second and third superframes, each held until its own link
        // control and then fed from its burst A.
        assert_eq!(h.slot1.len(), 2 * SUPERFRAME_FRAMES);
        check_audio(&h.slot1);
        assert!(h.slot1[0].provenance.span.start_ns.0.abs_diff(first_sync) < SLACK_NS);
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        let (a, b) = (&h.events[0], &h.events[1]);
        has(
            a,
            json!({"src": SRC, "dst": DST, "color_code": 5, "slot": 1, "audio": true}),
        );
        has(b, json!({"src": SRC, "frames": 36}));
        let opened = t_ns(a);
        assert!(
            opened + SLACK_NS >= first_sync && opened < first_sync + SUPERFRAME_NS,
            "opened {opened}, first sync {first_sync}"
        );
    }

    #[tokio::test]
    async fn a_call_cut_short_closes_after_its_timeout() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        // The header and fifteen voice bursts whole (slot 1's burst of TDMA frame 15 ends
        // 30 ms into it), then a second of bare carrier; the input stays open.
        let mut iq = repeater(call(group(), 3), idles(20))[..15 * TDMA + 2000].to_vec();
        iq.extend(carrier(1000));
        h.send(&iq, RATE);
        assert!(
            h.until(|h| h.events.len() >= 2 && mbe.log().contains("eof"))
                .await,
            "{:?} {:?}",
            h.events,
            mbe.log()
        );
        let b = &h.events[1];
        has(
            b,
            json!({"src": SRC, "eot": true, "frames": 45, "audio": true}),
        );
        // Closed where its last burst's period ends.
        let ended = burst_at(15, 1) + PERIOD_NS;
        assert!(t_ns(b).abs_diff(ended) < SLACK_NS, "{b}");
        assert!(
            h.until(|h| h.slot1.len() >= 45 && h.main.len() >= 45).await,
            "{} {}",
            h.slot1.len(),
            h.main.len()
        );
        assert_eq!(h.slot1.len(), 45);
        let r = h.end(|_| true).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.events.len(), 2, "{:?}", h.events);
        assert_eq!(h.main.len(), 45);
    }

    #[tokio::test]
    async fn a_call_1_khz_off_centre_decodes() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(
            &synth(json!({
                "family": "dmr", "color_code": 5, "src": SRC, "dst": DST, "superframes": 3,
                "offset_hz": 1000.0
            })),
            RATE,
        );
        let r = h.end(|h| h.events.len() >= 2 && h.slot1.len() >= 54).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.slot1.len(), 54);
        assert_eq!(h.events.len(), 2, "{:?}", h.events);
        has(
            &h.events[0],
            json!({"src": SRC, "dst": DST, "color_code": 5, "audio": true}),
        );
        has(&h.events[1], json!({"eot": true, "frames": 54}));
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
            (
                "p25",
                synth(json!({
                    "family": "p25", "nac": 291, "src": SRC, "dst": 65000, "superframes": 3
                })),
            ),
        ];
        for (name, iq) in signals {
            let mbe = Mbe::installed();
            let mut h = Harness::start(&mbe, 4096, "first_active");
            h.send(&iq, RATE);
            // Every event is published before the stage returns.
            let r = h.end(|_| true).await;
            assert!(r.is_ok(), "{name}: {r:?}");
            assert!(h.events.is_empty(), "{name}: {:?}", h.events);
            assert!(h.channels.is_empty(), "{name}: {:?}", h.channels);
            assert!(
                h.slot1.is_empty() && h.slot2.is_empty() && h.main.is_empty(),
                "{name}"
            );
            assert_eq!(mbe.log(), "", "{name}: the vocoder was started");
        }
    }

    #[tokio::test]
    async fn a_lagged_input_closes_the_call() {
        let mbe = Mbe::absent();
        // A long call: its first 700 ms read, then 30 blocks into a queue of 8, so the
        // stage misses 22 of them and reads the rest with a new receiver.
        let iq = synth(json!({
            "family": "dmr", "color_code": 5, "src": SRC, "dst": DST, "superframes": 20
        }));
        let mut h = Harness::start(&mbe, 8, "first_active");
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
        // Closed at its last burst before the lag; joined again after it.
        assert!(t_ns(&h.events[1]) < T0 + 800_000_000, "{}", h.events[1]);
        assert!(t_ns(&h.events[2]) > T0 + 2_900_000_000, "{}", h.events[2]);

        // A new rate mid-call: the call closes, and the next decodes at the new rate.
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(&repeater(call(group(), 3), idles(20))[..7 * BLOCK], RATE);
        let slow = 24_000;
        let (spec, imp) = parse_params(
            json!({"family": "dmr", "color_code": 5, "src": SRC, "dst": DST, "superframes": 2})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        h.send(&synthesise(&spec, &imp, slow, 0), u64::from(slow));
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

    #[tokio::test]
    async fn the_receivers_counts_are_in_the_job_context() {
        let mbe = Mbe::absent();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(&repeater(call(group(), 1), idles(8)), RATE);
        let r = h
            .end(|h| h.receiver().is_some_and(|c| c["locks"].as_u64() >= Some(1)))
            .await;
        assert!(r.is_ok(), "{r:?}");
        let c = h.receiver().expect("dmr.receiver noted");
        for k in ["locks", "false_locks", "losses", "off_deviation"] {
            assert!(c[k].is_u64(), "{k}: {c}");
        }
        assert_eq!(c["off_deviation"], 0, "{c}");
    }

    #[tokio::test]
    async fn a_lost_burst_does_not_split_the_call() {
        // The second superframe's burst C lost (the header, then six bursts, then A, B).
        let mut slot1 = call(group(), 3);
        build::lose(&mut slot1[1 + 6 + 2]);
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(&repeater(slot1, idles(20)), RATE);
        // Fed: the first superframe, the second's A and B; its D to F held for its link
        // control, which the lost C's fragment never completes; the third from its own.
        let fed = SUPERFRAME_FRAMES + 6 + SUPERFRAME_FRAMES;
        let r = h
            .end(|h| h.events.len() >= 2 && h.slot1.len() >= fed && h.main.len() >= fed)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.slot1.len(), fed);
        assert_eq!(h.main.len(), fed);
        let lost_from = burst_at(9, 1);
        let third = burst_at(13, 1);
        assert!(
            starts(&h.slot1)
                .iter()
                .all(|&s| s < lost_from || s + SLACK_NS >= third),
            "{:?}",
            starts(&h.slot1)
        );
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(
            &h.events[1],
            json!({"frames": 51, "audio": true, "silent": null, "program": true}),
        );
        // One vocoder throughout.
        assert_eq!(mbe.log(), "start\neof\n");
    }

    #[tokio::test]
    async fn an_encrypted_call_on_the_grid_after_a_cut_one_is_never_heard() {
        // A clear call of two superframes whose terminator is lost, six bursts lost (the
        // hang time, the next call's header and privacy header among them), then, where
        // the next superframe belongs, a call from the same radio to the same talkgroup
        // whose link control says encrypted: the same call to the stage, the second part
        // held after the loss until its own link control.
        let mut slot1 = call(group(), 2);
        slot1.pop();
        slot1.extend(vec![lost(); 6]);
        let second = call(encrypted(group()), 2);
        slot1.extend(&second[2..]);
        let at = burst_at(19, 1);
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(&repeater(slot1, idles(36)), RATE);
        let r = h
            .end(|h| h.events.len() >= 2 && h.slot1.len() >= 2 * SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        // The speech first: the clear part's 36 frames, none of the encrypted part's.
        assert_eq!(h.slot1.len(), 2 * SUPERFRAME_FRAMES);
        assert!(
            audio_end(&h.slot1) <= at + SLACK_NS,
            "{} > {at}",
            audio_end(&h.slot1)
        );
        assert_eq!(h.main.len(), 2 * SUPERFRAME_FRAMES);
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(
            &h.events[1],
            json!({"encrypted": true, "audio": false, "silent": "encrypted", "frames": 72}),
        );
        assert_eq!(mbe.log(), "start\neof\n", "one vocoder, the clear part's");
    }

    #[tokio::test]
    async fn an_encrypted_call_off_the_grid_after_a_cut_one_is_never_heard() {
        // A handheld's clear call whose terminator is cut off, 35 ms of nothing (off the
        // first call's grid), then a call from the same radio whose header and privacy
        // header are cut off and whose link control says encrypted.
        let mut first = build::call(CC, &group(), 2, Origin::Handheld, false);
        first.pop();
        let mut iq = handheld(&first);
        iq.extend(vec![Complex::new(0.0, 0.0); RATE as usize / 40]);
        let at = T0 + ns(iq.len());
        let second = build::call(CC, &encrypted(group()), 2, Origin::Handheld, false);
        iq.extend(handheld(&second[2..]));
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(&iq, RATE);
        let r = h
            .end(|h| h.events.len() >= 4 && h.slot1.len() >= 2 * SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        // The speech first: no frame of the encrypted call.
        assert_eq!(h.slot1.len(), 2 * SUPERFRAME_FRAMES);
        assert!(
            audio_end(&h.slot1) <= at + SLACK_NS,
            "{} > {at}",
            audio_end(&h.slot1)
        );
        assert_eq!(
            eots(&h.events),
            [false, true, false, true],
            "{:?}",
            h.events
        );
        has(&h.events[1], json!({"audio": true, "encrypted": false}));
        for e in &h.events[2..] {
            has(
                e,
                json!({"encrypted": true, "audio": false, "silent": "encrypted"}),
            );
        }
        assert_eq!(mbe.log(), "start\neof\n", "one vocoder, the first call's");
    }

    #[tokio::test]
    async fn a_joined_call_never_feeds_a_superframe_before_its_own_link_control() {
        // Joined (the header lost): two clear superframes, then one whose link control,
        // the same radio and talkgroup, says encrypted. Each superframe waits for its own
        // link control, so none of the third's frames is fed.
        let clear = call(group(), 2);
        let turned = call(encrypted(group()), 1);
        let mut slot1 = vec![lost()];
        slot1.extend(&clear[1..13]);
        slot1.extend(&turned[2..]);
        let at = burst_at(13, 1);
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(&repeater(slot1, idles(20)), RATE);
        let r = h
            .end(|h| h.events.len() >= 2 && h.slot1.len() >= 2 * SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        // The speech first.
        assert_eq!(h.slot1.len(), 2 * SUPERFRAME_FRAMES);
        assert!(
            audio_end(&h.slot1) <= at + SLACK_NS,
            "{} > {at}",
            audio_end(&h.slot1)
        );
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(&h.events[0], json!({"encrypted": false, "audio": true}));
        has(
            &h.events[1],
            json!({"encrypted": true, "silent": "encrypted", "frames": 54, "program": true}),
        );
    }

    #[tokio::test]
    async fn a_talker_alias_is_not_a_new_call() {
        // The second superframe carries a talker alias, or GPS, in place of the link
        // control: with the header, and joined after it, where only the first superframe's
        // link control licenses the alias's.
        let aliased = build::call(CC, &group(), 3, Origin::Repeater, true);
        let gps = build::call_with_gps(CC, &group(), 3);
        let joined = |mut b: Vec<Burst>| {
            b[0] = lost();
            b
        };
        for (what, bursts) in [
            ("talker alias, header", aliased.clone()),
            ("talker alias, joined", joined(aliased)),
            ("gps, header", gps.clone()),
            ("gps, joined", joined(gps)),
        ] {
            let mbe = Mbe::installed();
            let mut h = Harness::start(&mbe, 4096, "first_active");
            h.send(&repeater(bursts, idles(20)), RATE);
            let r = h.end(|h| h.events.len() >= 2 && h.slot1.len() >= 54).await;
            assert!(r.is_ok(), "{what}: {r:?}");
            assert_eq!(h.slot1.len(), 54, "{what}");
            assert_eq!(h.main.len(), 54, "{what}");
            assert_eq!(eots(&h.events), [false, true], "{what}: {:?}", h.events);
            for e in &h.events {
                has(e, json!({"src": SRC, "dst": DST, "audio": true}));
            }
            has(&h.events[1], json!({"frames": 54}));
            assert_eq!(mbe.log(), "start\neof\n", "{what}");
        }
    }

    #[tokio::test]
    async fn a_call_opened_by_a_privacy_header_is_reported_and_silent() {
        // No voice header (lost), a privacy header, then the voice under a link control that
        // says clear: the privacy header alone marks it.
        let mut bursts = call(group(), 2);
        bursts[0] = lost();
        bursts.insert(1, build::privacy_header(CC, Origin::Repeater));
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(&repeater(bursts, idles(16)), RATE);
        let r = h.end(|h| h.events.len() >= 2).await;
        assert!(r.is_ok(), "{r:?}");
        silent_and_encrypted(&h, &mbe, "privacy header first");
    }

    #[tokio::test]
    async fn a_header_call_that_turns_encrypted_mid_superframe_stops_from_that_burst() {
        // A header's call: one clear superframe, then one whose link control (at its E)
        // says encrypted, then its terminator. Bursts A to D of the second were fed under
        // the header's licence; none from E on.
        let clear = call(group(), 1);
        let turned = call(encrypted(group()), 1);
        let mut slot1 = clear[..7].to_vec();
        slot1.extend(&turned[2..]);
        let e = burst_at(7 + 4, 1);
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096, "first_active");
        h.send(&repeater(slot1, idles(16)), RATE);
        let r = h
            .end(|h| h.events.len() >= 2 && mbe.log().contains("eof"))
            .await;
        assert!(r.is_ok(), "{r:?}");
        // The speech first: the first superframe and at most four bursts of the second.
        assert!(
            h.slot1.len() <= SUPERFRAME_FRAMES + 4 * 3,
            "{}",
            h.slot1.len()
        );
        assert!(
            starts(&h.slot1).iter().all(|&s| s + SLACK_NS < e),
            "{:?} vs {e}",
            starts(&h.slot1)
        );
        assert_eq!(h.slot1.len(), SUPERFRAME_FRAMES + 4 * 3);
        // One vocoder, its input closed at E.
        assert_eq!(mbe.log(), "start\neof\n");
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(&h.events[0], json!({"encrypted": false, "audio": true}));
        has(
            &h.events[1],
            json!({"encrypted": true, "silent": "encrypted", "frames": 36}),
        );
    }

    /// Slot 1's call with its second superframe's burst C lost, so its speech is held from
    /// D to the third superframe's E; slot 2's call from the fourth TDMA frame. With
    /// `slot1` false slot 1 is idle.
    fn held_beside(slot1: bool) -> Vec<Complex<f32>> {
        let mut one = call(group(), 3);
        build::lose(&mut one[1 + 6 + 2]);
        let mut two = idles(3);
        two.extend(call(other(), 3));
        if !slot1 {
            one = idles(one.len());
        }
        repeater(one, two)
    }

    #[tokio::test]
    async fn under_all_a_held_call_delays_the_mix_at_most_its_hold() {
        // Read in real time up to slot 1's third superframe's C, while slot 1 is held: the
        // mix is behind where it is with slot 1 idle by at most the held frames' reach.
        let cut = 15 * TDMA;
        let read_to = T0 + ns(cut);
        let hold = 4 * PERIOD_NS + GRID_NS;
        let mut reach = Vec::new();
        for slot1 in [false, true] {
            let mbe = Mbe::installed();
            let mut h = Harness::start(&mbe, 4096, "all");
            h.send_real_time(&held_beside(slot1)[..cut], RATE).await;
            let low = match reach.first() {
                None => read_to - LATE_NS - PERIOD_NS,
                Some(&idle) => idle - hold,
            };
            assert!(
                h.until(|h| audio_end(&h.main) >= low).await,
                "slot 1 {}: the mix reached {} of {low}",
                if slot1 { "held" } else { "idle" },
                audio_end(&h.main)
            );
            // Settled: what the stage has made final is written.
            tokio::time::sleep(Duration::from_millis(200)).await;
            h.collect();
            reach.push(audio_end(&h.main));
            if slot1 {
                // Slot 1 was held throughout: nothing of its has been fed since its loss.
                let lost_at = burst_at(9, 1);
                assert!(
                    starts(&h.slot1).iter().all(|&s| s < lost_at),
                    "{:?}",
                    starts(&h.slot1)
                );
            }
            let r = h.end(|_| true).await;
            assert!(r.is_ok(), "{r:?}");
        }
        assert!(reach[1] + hold >= reach[0], "{reach:?}");
    }

    /// Slot 1's call of four superframes with its second superframe's burst C lost, beside
    /// slot 2's of five from the fourth TDMA frame, running on after it.
    fn lost_beside() -> Vec<Complex<f32>> {
        let mut one = call(group(), 4);
        build::lose(&mut one[1 + 6 + 2]);
        let mut two = idles(3);
        two.extend(call(other(), 5));
        repeater(one, two)
    }

    /// Each slot's samples by their index at 48 kHz, a later frame's over an earlier's.
    fn place(audio: &[Frame]) -> std::collections::HashMap<u128, f32> {
        let mut at = std::collections::HashMap::new();
        for f in audio {
            let first = sample_index(f.provenance.span.start_ns.0);
            for (k, &s) in samples(f).iter().enumerate() {
                at.insert(first + k as u128, s);
            }
        }
        at
    }

    /// The frames of `of` whose every sample is in the mix: the slots' sum there.
    fn mixed<'a>(h: &Harness, of: &'a [Frame]) -> Vec<&'a Frame> {
        let (main, one, two) = (place(&h.main), place(&h.slot1), place(&h.slot2));
        of.iter()
            .filter(|f| {
                let first = sample_index(f.provenance.span.start_ns.0);
                (first..first + 960).all(|i| {
                    let want = (one.get(&i).unwrap_or(&0.0) + two.get(&i).unwrap_or(&0.0))
                        .clamp(-1.0, 1.0);
                    main.get(&i).is_some_and(|m| (m - want).abs() < 1e-6)
                })
            })
            .collect()
    }

    #[tokio::test]
    async fn under_all_no_speech_a_held_call_is_fed_is_lost_from_the_mix() {
        // In real time: slot 1 held after a lost burst (from then on each superframe waits
        // for its own word), and slot 1 joined without its voice header (each superframe
        // waits from the first), each beside slot 2's running call.
        let mut joined = call(group(), 3);
        joined[0] = lost();
        let mut two = idles(3);
        two.extend(call(other(), 4));
        for (what, iq, from) in [
            ("a lost burst", lost_beside(), burst_at(9, 1)),
            ("joined", repeater(joined, two), T0),
        ] {
            let mbe = Mbe::installed();
            let mut h = Harness::start(&mbe, 4096, "all");
            h.send_real_time(&iq, RATE).await;
            let r = h
                .end(|h| h.events.len() >= 4 && !h.slot1.is_empty() && !h.slot2.is_empty())
                .await;
            assert!(r.is_ok(), "{what}: {r:?}");
            let fed: Vec<Frame> = h
                .slot1
                .iter()
                .filter(|f| f.provenance.span.start_ns.0 >= from)
                .cloned()
                .collect();
            assert!(
                fed.len() >= 2 * SUPERFRAME_FRAMES,
                "{what}: {} fed",
                fed.len()
            );
            let got = mixed(&h, &fed).len();
            assert_eq!(
                got,
                fed.len(),
                "{what}: {got} of {} fed in the mix",
                fed.len()
            );
            // Slot 2's too, all of it.
            assert_eq!(mixed(&h, &h.slot2).len(), h.slot2.len(), "{what}");
        }
    }

    #[tokio::test]
    async fn under_follow_and_first_active_the_holder_loses_nothing() {
        // In real time, slot 1 holding the main stream and held after a lost burst: every
        // frame it is fed is on the main stream as it was published.
        for policy in ["first_active", "follow"] {
            let mbe = Mbe::installed();
            let mut h = Harness::start(&mbe, 4096, policy);
            h.send_real_time(&lost_beside(), RATE).await;
            let r = h
                .end(|h| h.events.len() >= 4 && !h.slot1.is_empty() && !h.slot2.is_empty())
                .await;
            assert!(r.is_ok(), "{policy}: {r:?}");
            assert!(
                h.slot1.len() >= 3 * SUPERFRAME_FRAMES,
                "{policy}: {}",
                h.slot1.len()
            );
            let on_main: Vec<&Frame> = h
                .main
                .iter()
                .filter(|m| {
                    h.slot1
                        .iter()
                        .any(|f| f.provenance.span == m.provenance.span && samples(f) == samples(m))
                })
                .collect();
            assert_eq!(on_main.len(), h.slot1.len(), "{policy}");
            let s = starts(&h.main);
            assert!(s.windows(2).all(|w| w[1] > w[0]), "{policy}: {s:?}");
        }
    }

    #[tokio::test]
    async fn a_failed_vocoder_stays_failed_for_the_call() {
        // A header's call whose vocoder dies after 10 frames; its second superframe turns
        // it encrypted and its third clear again: no vocoder is started again in the call.
        let mbe = Mbe::installed();
        mbe.die_once();
        let clear = call(group(), 3);
        let turned = call(encrypted(group()), 1);
        let mut slot1 = clear[..7].to_vec();
        slot1.extend(&turned[2..8]);
        slot1.extend(&clear[13..]);
        let iq = repeater(slot1, idles(20));
        let mut h = Harness::start(&mbe, 4096, "first_active");
        let split = 8 * TDMA;
        h.send(&iq[..split], RATE);
        assert!(
            h.until(|h| h.vocoder_notes().iter().any(|n| n.contains("exited")))
                .await,
            "{:?}",
            h.vocoder_notes()
        );
        h.send(&iq[split..], RATE);
        let r = h.end(|h| h.events.len() >= 2 && h.slot1.len() >= 10).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.slot1.len(), 10);
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(
            &h.events[1],
            json!({"silent": "vocoder_failed", "audio": false, "frames": 54}),
        );
        assert_eq!(mbe.log().matches("start").count(), 1, "{}", mbe.log());
    }

    #[test]
    fn a_link_control_is_the_calls_only_when_its_fragments_all_are() {
        // A call whose stretch of bursts began with a burst A at T0.
        let o = Open::new(1, Origin::Repeater, T0, Licence::Superframe);
        let at = |bursts: u64| T0 + bursts * PERIOD_NS;
        // Completed on its first E, from B to E: all its own.
        assert!(o.exact(at(4), Some(4)));
        // On an E two bursts in: that E's B came before the stretch.
        assert!(!o.exact(at(2), Some(4)));
        // Across a boundary on B or D: from the previous superframe's C or E, five bursts
        // back; its own only once the stretch reaches back that far.
        assert!(o.exact(at(5), Some(1)));
        assert!(!o.exact(at(4), Some(1)));
        assert!(!o.exact(at(4), Some(3)));
        assert!(o.exact(at(5), Some(3)));
    }

    #[test]
    fn a_burst_follows_on_the_slots_grid_only() {
        let mut o = Open::new(1, Origin::Repeater, T0, Licence::Header);
        o.lc = Some(group());
        o.phase = Some(T0);
        let at = |bursts: u64| T0 + bursts * PERIOD_NS;
        assert_eq!(o.follows(at(1), Some(1), None), Follows::Next);
        assert_eq!(o.follows(at(1) + GRID_NS / 2, Some(1), None), Follows::Next);
        assert_eq!(o.follows(at(1), Some(1), Some(&group())), Follows::Next);
        // A burst or more lost: the same call, up to a superframe's worth.
        assert_eq!(o.follows(at(3), Some(3), None), Follows::AfterLoss);
        assert_eq!(o.follows(at(7), Some(1), None), Follows::AfterLoss);
        assert_eq!(o.follows(at(8), Some(2), None), Follows::No);
        // Off the grid, another link control, a burst on the wrong letter, or time going
        // back: another call.
        assert_eq!(o.follows(at(1) + 2 * GRID_NS, Some(1), None), Follows::No);
        assert_eq!(o.follows(at(1), Some(1), Some(&other())), Follows::No);
        assert_eq!(o.follows(at(1), Some(0), None), Follows::No);
        assert_eq!(o.follows(T0, None, None), Follows::No);
        // With no letter (the phase unknown), the grid alone.
        assert_eq!(o.follows(at(2), None, None), Follows::AfterLoss);
    }

    #[test]
    fn params_are_strict() {
        for (p, want) in [
            (json!({}), AudioPolicy::FirstActive),
            (json!({"audio": "follow"}), AudioPolicy::Follow),
            (json!({"audio": "all"}), AudioPolicy::All),
        ] {
            let got: DmrParams = serde_json::from_value(p).unwrap();
            assert_eq!(got.audio, want);
        }
        assert!(serde_json::from_value::<DmrParams>(json!({"audio": "loudest"})).is_err());
        assert!(serde_json::from_value::<DmrParams>(json!({"x": 1})).is_err());
        let stage = DmrDecodeStage::new();
        assert_eq!(stage.spec().kind, DMR_DECODE_KIND);
        assert_eq!(stage.spec().inputs, [StreamKind::Iq]);
        assert_eq!(
            stage.spec().outputs,
            [
                StreamKind::Event,
                StreamKind::Audio,
                StreamKind::Audio,
                StreamKind::Audio
            ]
        );
        assert_eq!(
            stage.output_roles(&json!({})),
            [None, None, Some("slot1"), Some("slot2")]
        );
        assert_eq!(stage.voice_roles(&json!({})), ["slot1", "slot2"]);
    }
}
