//! P25 phase 1 decode stage (design 38, 5.5): the 4-level FSK front end ([`crate::fsk4`],
//! C4FM) and the framer ([`crate::p25`]) turn a channel's IQ into data units; this stage
//! turns those into call and channel events and the call's speech.
//!
//! **Calls.** One open call at a time. A header opens one; so does a voice unit (LDU1,
//! LDU2) when none is open, or when it does not follow on from the open call: another
//! network access code, another link control, or a sync more than [`CONTIGUOUS_NS`] off
//! the open call's unit grid (another call, its header and the open call's terminator
//! both lost). A voice unit on the grid after lost units (`end + k` LDUs, LDU1 and LDU2
//! still alternating) stays in the call, its speech held again until that unit's own
//! encryption word: a lost unit costs at most one more unit's speech and no events. A call
//! closed by another call's unit or a header closes at its own last unit's end. Two
//! `type: call` events per call ([`crate::voice_call`]):
//! - the opening one when its link control and its encryption are both in hand: at the
//!   first LDU1 after a header (the header carries no calling radio), or within about one
//!   and a half superframes when the job joins mid-call (at worst an LDU2 for the
//!   encryption, then an LDU1 for the radio);
//! - the closing one (`eot: true`) at its terminator, when no unit has come for longer than
//!   [`P25_SUPERFRAME_MS`] (noticed one unit's length later, when a unit that began within
//!   it would be whole), at a lagged input, a new sample rate or the input's end.
//!
//! The header's talkgroup is not reported: it is 0 for a private call, and the link control
//! names the destination of either kind.
//!
//! **Encryption.** Two words, each standing until a word of its own kind replaces it: the
//! last encryption sync (the header's, then each LDU2's), encrypted when it names an
//! algorithm other than [`P25_ALGID_CLEAR`]; and the last decodable LDU1 link control,
//! encrypted when marked protected (its format's bit, or its service options'). A call is
//! encrypted when either says so, and its speech is settled on them before any of a unit's
//! frames is fed. A call opened by a voice unit, or held again after lost units, stays
//! held until an encryption sync or a protected link control comes: a clear LDU1 feeds
//! nothing and the speech starts at the LDU2. A call that turns encrypted closes its
//! vocoder's input at once (the speech fed before is still handed back); one that turns
//! clear starts one. An encrypted call gets its events and no vocoder; nothing here
//! decrypts. One case feeds an encrypted call's frames: a second transmission whose first
//! LDU starts within [`CONTIGUOUS_NS`] of the open call's last unit, with no terminator or
//! header between them, and whose first LDU1's link control does not decode, is taken as
//! the open call, so up to nine frames of it (one LDU, 180 ms) reach the vocoder, as
//! ciphertext, before its LDU2's encryption sync stops it. A call whose link control is
//! itself encrypted names no radio and gives no event (reporting it is left for later).
//!
//! **Speech.** `zeta-mbe imbe7200x4400` (the row's handler 0, optional) is looked up when a
//! call's encryption is first known to be clear, so a program installed while the job runs is
//! used from the next call. Each LDU's nine IMBE frames go to it, each with a 20 ms span from
//! the unit's time, through the vocoder seat ([`crate::vocoder`]), and its speech is the
//! job's audio. Without the program every call is reported with `audio: false`,
//! `silent: "no_vocoder"`, and `job_context.p25.vocoder` says how to install it, once. A
//! vocoder that fails mid-call ends that call's audio (`silent: "vocoder_failed"` on its
//! closing event), held again or turned encrypted and clear again as the call may be; the
//! next call starts it again.
//!
//! **A channel with no call.** A unit decoded with no call open (a trunking signalling unit,
//! a packet data unit, a stray terminator) gives one `type: channel` event, and another only
//! when its NAC changes or trunking signalling is first seen on it (`control`). Trunking and
//! packet units are counted in `job_context.p25` (`trunking_units`, `packet_units`), not
//! parsed.
//!
//! **The receiver.** The front end's counters over the job are in `job_context.p25.receiver`
//! (`locks`, `false_locks`, `losses`, `off_deviation`), updated when they change: a
//! transmitter the receiver refuses for its deviation shows in `off_deviation` and nowhere
//! else.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, Span, StreamId, StreamKind, Timestamp};

use crate::fsk4::{Fsk4Demod, Fsk4Params, Fsk4Tally, Shape};
use crate::p25::{
    EncSync, IMBE_FEED_BYTES, LDU_DIBITS, LinkControl, P25_ALGID_CLEAR, P25_BAUD,
    P25_COAST_SEARCHES, P25_DEVIATION_HZ, P25_SUPERFRAME_MS, P25Framer, P25Unit,
};
use crate::util::time_of;
use crate::vocoder::{Vocoder, VocoderEnv, VocoderSpec};
use crate::voice_call::{
    CallFields, Events, MBE_FRAME_SAMPLES, MBE_RATE_HZ, ProgramLookup, Silent, ZETA_MBE,
    channel_event, publish,
};
use crate::{Input, run_map_ending};

/// The stage's kind and the row's id.
pub const P25_DECODE_KIND: &str = "p25_decode";
/// The events' `protocol` and the job context key.
const PROTOCOL: &str = "p25";
/// `zeta-mbe`'s argument for P25's full-rate IMBE.
pub const IMBE_CODEC: &str = "imbe7200x4400";
/// How much later than the open call's last unit ended a voice unit's sync may be and
/// still follow on from it, ns (48 symbols: the timing's wander, never a lost unit).
/// A voice unit later than that starts a new call, held silent until its own encryption
/// is known.
pub const CONTIGUOUS_NS: u64 = 10_000_000;
/// One IMBE frame, ns (20 ms).
const IMBE_FRAME_NS: u64 = MBE_FRAME_SAMPLES as u64 * 1_000_000_000 / MBE_RATE_HZ;
/// One LDU, ns (180 ms): the span of a call event.
const LDU_NS: u64 = dibits_ns(LDU_DIBITS);
/// How long a call's sync may be lost before it is over, ns.
const SUPERFRAME_NS: u64 = P25_SUPERFRAME_MS * 1_000_000;
/// How long after a call's last unit ended it is closed with no unit since, ns: a
/// superframe, plus one unit's length, since a unit whose sync came within the superframe
/// (one lost unit before it) is only handed on once it is whole.
const EXPIRE_NS: u64 = SUPERFRAME_NS + LDU_NS;
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
    /// The last block's start time and first sample, which a flush's units are timed by.
    last: Option<(Timestamp, u64)>,
}

impl Receiver {
    fn new(rate_hz: u64) -> Result<Self, String> {
        let params = Fsk4Params {
            symbol_rate_baud: P25_BAUD,
            deviation_hz: P25_DEVIATION_HZ,
            shape: Shape::C4fm,
            coast_searches: P25_COAST_SEARCHES,
        };
        Ok(Self {
            demod: Fsk4Demod::new(rate_hz, params)
                .map_err(|e| format!("{P25_DECODE_KIND}: {e}"))?,
            framer: P25Framer::new(),
            fed: 0,
            last: None,
        })
    }

    /// The units `block` completes, each with its sync's time.
    fn feed(&mut self, block: &zeta_core::port::IqBlock) -> Vec<(u64, u16, P25Unit)> {
        let start = self.fed;
        self.fed += block.samples.len() as u64;
        self.last = Some((block.t0, start));
        let dibits = self.demod.feed(&block.samples);
        self.units(&dibits)
    }

    /// The units the front end still held when the input ended.
    fn flush(&mut self) -> Vec<(u64, u16, P25Unit)> {
        let dibits = self.demod.flush();
        self.units(&dibits)
    }

    /// The units `dibits` complete, timed from the last block.
    fn units(&mut self, dibits: &[crate::fsk4::SoftDibit]) -> Vec<(u64, u16, P25Unit)> {
        let units = self.framer.push(dibits);
        self.demod.invert(self.framer.inverted().unwrap_or(false));
        let rate = self.demod.rate_hz;
        let Some((t0, start)) = self.last else {
            return Vec::new();
        };
        units
            .into_iter()
            .map(|(at, nac, u)| {
                let t = time_of(t0, at.round() as i64 - start as i64, rate);
                (t, nac, u)
            })
            .collect()
    }
}

/// What a call's speech is doing.
enum Voice {
    /// No encryption word covers its units yet: nothing is fed. `was` is why it was silent
    /// before it was held again after a lost unit (`None` when it was heard, or never
    /// settled), which its closing event reports if it ends held.
    Held {
        was: Option<Silent>,
    },
    Heard(Vocoder),
    Silent(Silent),
}

/// Which voice unit (or the header) a call's last one was, for the LDU1/LDU2 alternation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Header,
    Ldu1,
    Ldu2,
}

impl Kind {
    /// The voice unit `missed` units after this one: LDU1 and LDU2 alternate, an LDU1
    /// after the header.
    fn after(self, missed: u64) -> Kind {
        let next = match self {
            Kind::Header | Kind::Ldu2 => Kind::Ldu1,
            Kind::Ldu1 => Kind::Ldu2,
        };
        match (next, missed % 2) {
            (k, 0) => k,
            (Kind::Ldu1, _) => Kind::Ldu2,
            _ => Kind::Ldu1,
        }
    }
}

/// How a voice unit stands to the open call.
#[derive(Debug, PartialEq, Eq)]
enum Follows {
    /// Straight after its last unit.
    Next,
    /// On its unit grid, a unit or more lost in between: the same call, its speech held
    /// again until this unit's own encryption word.
    AfterLoss,
    /// Another call.
    No,
}

/// The call the stage is following.
struct Open {
    nac: u16,
    lc: Option<LinkControl>,
    /// The last encryption sync seen.
    enc: Option<EncSync>,
    /// The last decodable link control said the speech is encrypted. Independent of `enc`:
    /// only a decodable link control saying not protected clears it.
    protected: bool,
    /// Whether an encryption word (an encryption sync, or a link control marked
    /// protected) has come since the call opened or was last held again: until one has,
    /// its speech stays held.
    told: bool,
    voice: Voice,
    /// Its vocoder failed (to start, or during the call): it stays silent to its end,
    /// whatever its speech does after (held again, encrypted, clear again).
    failed: bool,
    announced: bool,
    frames: u32,
    errors: u32,
    /// Whether any of its speech went to the audio output.
    heard: bool,
    /// When its last unit ended, ns.
    end_ns: u64,
    /// Its last unit.
    last: Kind,
}

impl Open {
    fn new(nac: u16) -> Self {
        Self {
            nac,
            lc: None,
            enc: None,
            protected: false,
            told: false,
            voice: Voice::Held { was: None },
            failed: false,
            announced: false,
            frames: 0,
            errors: 0,
            heard: false,
            end_ns: 0,
            last: Kind::Header,
        }
    }

    /// Encrypted when either word says so: the last encryption sync, or the last decodable
    /// link control.
    fn encrypted(&self) -> bool {
        self.protected || self.enc.is_some_and(|e| e.algid != P25_ALGID_CLEAR)
    }

    /// Why its speech is not heard now; `None` while a live vocoder has it.
    fn silent(&self) -> Option<Silent> {
        match &self.voice {
            Voice::Heard(v) if v.alive() => None,
            Voice::Heard(_) => Some(Silent::VocoderFailed),
            Voice::Silent(s) => Some(*s),
            Voice::Held { was } => *was,
        }
    }

    /// How a voice unit of `kind` at `t` with `nac` and link control `lc` stands to this
    /// call. It is another call on another network or with another link control. On the
    /// same network with no other link control it is the next unit when its sync is within
    /// [`CONTIGUOUS_NS`] of this call's last unit's end; the same call after lost units when
    /// it lands that close to the unit grid after it (`end + k` LDUs, `k` LDUs short of a
    /// superframe, LDU1 and LDU2 still alternating); else another call (whose header, and
    /// this call's terminator, were both lost).
    fn follows(&self, t: u64, nac: u16, kind: Kind, lc: Option<&LinkControl>) -> Follows {
        let same =
            |a: &LinkControl, b: &LinkControl| (a.call, a.src, a.dst) == (b.call, b.src, b.dst);
        if nac != self.nac || matches!((&self.lc, lc), (Some(a), Some(b)) if !same(a, b)) {
            return Follows::No;
        }
        if t <= self.end_ns + CONTIGUOUS_NS {
            return Follows::Next;
        }
        let gap = t - self.end_ns;
        let missed = (gap + LDU_NS / 2) / LDU_NS;
        let on_grid = missed >= 1
            && missed * LDU_NS < SUPERFRAME_NS
            && gap.abs_diff(missed * LDU_NS) <= CONTIGUOUS_NS
            && self.last.after(missed) == kind;
        if on_grid {
            Follows::AfterLoss
        } else {
            Follows::No
        }
    }

    /// Lost units: hold the speech again until an encryption word covers this call's units
    /// once more. Its vocoder, if it had one, is parked (what it was fed is handed back).
    fn hold_again(&mut self, env: &VocoderEnv) {
        let voice = std::mem::replace(&mut self.voice, Voice::Held { was: None });
        let was = match voice {
            Voice::Heard(v) => {
                let was = (!v.alive()).then_some(Silent::VocoderFailed);
                self.failed |= was.is_some();
                env.park(v);
                was
            }
            Voice::Silent(s) => Some(s),
            Voice::Held { was } => was,
        };
        self.voice = Voice::Held { was };
        self.told = false;
    }
}

/// The stage's calls: units in, events out, speech to the vocoder.
struct Calls {
    env: VocoderEnv,
    open: Option<Open>,
    lookup: ProgramLookup,
    /// The last channel event's NAC and `control`.
    channel: Option<(u16, bool)>,
    /// The NAC trunking signalling was last seen on.
    control_nac: Option<u16>,
    trunking_units: u64,
    packet_units: u64,
}

impl Calls {
    /// Settle `open`'s speech on its encryption, once anything has told it: stop a vocoder
    /// when the call turns encrypted, start one when it is (or turns) clear. A call nothing
    /// has told stays held.
    fn settle(&mut self, open: &mut Open, prov: &Provenance) {
        if !open.told {
            return;
        }
        let encrypted = open.encrypted();
        let voice = std::mem::replace(&mut open.voice, Voice::Held { was: None });
        open.voice = match (voice, encrypted) {
            // Its input closed before this unit's frames: what it was fed came from units
            // nothing had yet marked encrypted, and that speech is still handed back.
            (Voice::Heard(v), true) => {
                self.env.park(v);
                Voice::Silent(Silent::Encrypted)
            }
            (_, true) => Voice::Silent(Silent::Encrypted),
            (Voice::Held { .. } | Voice::Silent(Silent::Encrypted), false) if open.failed => {
                Voice::Silent(Silent::VocoderFailed)
            }
            (Voice::Held { .. } | Voice::Silent(Silent::Encrypted), false) => self.start(prov),
            (other, false) => other,
        };
        if matches!(open.voice, Voice::Silent(Silent::VocoderFailed)) {
            open.failed = true;
        }
    }

    /// A vocoder for a clear call, or why there is none.
    fn start(&mut self, prov: &Provenance) -> Voice {
        let Some(program) = self.lookup.find(&self.env.ctx, PROTOCOL) else {
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

    /// `open`'s event; `eot` for its closing one. `algid` and `kid` are null until an
    /// encryption sync is seen (a call a protected link control marked encrypted).
    fn call_event(open: &Open, eot: bool) -> serde_json::Value {
        let Some(lc) = open.lc else {
            unreachable!("a call is announced with its link control")
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
        m.insert("algid".into(), open.enc.map(|e| e.algid).into());
        m.insert("kid".into(), open.enc.map(|e| e.kid).into());
        // The summary's channel text: none on a call, which reads as who calls whom.
        m.insert("network".into(), serde_json::Value::Null);
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
            self.env.park(v);
        }
    }

    /// End the open call because another call's unit came at `t`: at its own last unit's
    /// end, or at `t` if that is sooner.
    fn close_for(&mut self, t: u64, events: &mut Events) {
        if let Some(end) = self.open.as_ref().map(|o| o.end_ns) {
            self.close(end.min(t), events);
        }
    }

    /// End the open call, at its last unit's end, once no unit has followed for longer
    /// than a superframe at `now` ([`EXPIRE_NS`]).
    fn expire(&mut self, now: u64, events: &mut Events) {
        if let Some(end) = self.open.as_ref().map(|o| o.end_ns)
            && now > end + EXPIRE_NS
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
                self.close_for(t, events);
                let mut open = Open::new(nac);
                open.end_ns = t + dibits_ns(HDU_DIBITS);
                if let Some(enc) = enc {
                    open.enc = Some(enc);
                    open.told = true;
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
        let (kind, lc, protected, enc, voice, errors) = match unit {
            P25Unit::Ldu1 {
                lc,
                protected,
                voice,
                errors,
            } => (Kind::Ldu1, lc, protected, None, voice, errors),
            P25Unit::Ldu2 { enc, voice, errors } => (Kind::Ldu2, None, false, enc, voice, errors),
            _ => return,
        };
        let follows = self
            .open
            .as_ref()
            .map_or(Follows::No, |o| o.follows(t, nac, kind, lc.as_ref()));
        let mut open = match follows {
            Follows::Next => self.open.take().expect("an open call"),
            Follows::AfterLoss => {
                let mut o = self.open.take().expect("an open call");
                o.hold_again(&self.env);
                o
            }
            Follows::No => {
                // Not this call's: a new one, held until its own encryption is known.
                self.close_for(t, events);
                Open::new(nac)
            }
        };
        open.end_ns = t + LDU_NS;
        open.last = kind;
        open.frames += voice.len() as u32;
        open.errors += errors;
        if let Some(lc) = lc {
            open.lc = Some(lc);
        }
        // The two encryption words, each standing until its own kind says otherwise: a
        // decodable link control (or one itself encrypted) sets or clears `protected`, an
        // encryption sync that counts replaces `enc` (the framer gives no other: one that
        // needed too many corrections, or is the all-zero word a carrier decodes to, comes
        // as `None` and leaves the call as it was, held if nothing had told it). Either is
        // a word about this unit, and the speech is settled on them before any of its
        // frames is fed.
        if lc.is_some() || protected {
            open.protected = protected;
        }
        if protected {
            open.told = true;
        }
        if let Some(enc) = enc {
            open.enc = Some(enc);
            open.told = true;
        }
        self.settle(&mut open, prov);
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
                open.failed = true;
            }
        }
        if !open.announced && open.lc.is_some() && !matches!(open.voice, Voice::Held { .. }) {
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
            events.push((t, channel_fields(now.0, now.1)));
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

/// A channel event's fields: [`channel_event`]'s, the NAC, and `network`, the text the
/// row's summary shows for it (`channel, NAC 291` or `control channel, NAC 291`; a call
/// event's `network` is null, so its summary reads `src > dst: call silent`).
fn channel_fields(nac: u16, control: bool) -> serde_json::Value {
    let mut m = channel_event(PROTOCOL, control);
    m.insert("nac".into(), nac.into());
    let kind = if control {
        "control channel"
    } else {
        "channel"
    };
    m.insert("network".into(), format!("{kind}, NAC {nac}").into());
    serde_json::Value::Object(m)
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
        let env = VocoderEnv {
            bus: bus.clone(),
            out: audio_out.clone(),
            ctx: ctx.clone(),
            drains: Arc::new(Mutex::new(Vec::new())),
        };
        let mut calls = Calls {
            env: env.clone(),
            open: None,
            lookup: ProgramLookup::default(),
            channel: None,
            control_nac: None,
            trunking_units: 0,
            packet_units: 0,
        };
        let mut rx: Option<Receiver> = None;
        let mut tally = Fsk4Tally::default();
        let mut last_prov: Option<Provenance> = None;
        let result = run_map_ending(self.spec().kind, inputs, bus, cancel, move |input| {
            let mut events = Vec::new();
            let frame = match input {
                Some(Input::Frame(f)) => f,
                Some(Input::Lagged { .. }) => {
                    // Start clean: the samples missed broke the symbol clock and the call.
                    tally.retire(rx.take().map(|r| r.demod.counters()));
                    calls.interrupt(&mut events);
                    return Ok(publish(
                        &ctx,
                        &events_out,
                        last_prov.as_ref(),
                        PROTOCOL,
                        events,
                        LDU_NS,
                    ));
                }
                None => {
                    // The input ended: the units the front end still held (a terminator
                    // among them, perhaps) first, then a call still open closes at its last
                    // unit, and its speech so far is still handed back.
                    if let (Some(r), Some(prov)) = (&mut rx, last_prov.as_ref()) {
                        for (t, nac, unit) in r.flush() {
                            calls.unit(t, nac, unit, prov, &mut events);
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
                        LDU_NS,
                    ));
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
                tally.retire(rx.take().map(|r| r.demod.counters()));
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
            tally.note(&ctx, PROTOCOL, rx.as_ref().map(|r| r.demod.counters()));
            last_prov = Some(frame.provenance.clone());
            Ok(publish(
                &ctx,
                &events_out,
                Some(&frame.provenance),
                PROTOCOL,
                events,
                LDU_NS,
            ))
        })
        .await;
        env.drain_all(&result).await;
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
    use crate::p25::{
        Call, LcMark, coded_sync, damaged_sync, encode_call, encode_call_header, encode_call_lc,
        encode_call_syncs, encode_control, level_blocks,
    };
    use crate::synth::{awgn, c4fm_modulate, parse_params, synthesise};
    use crate::voice_call::fake_mbe;

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

    /// The fake `zeta-mbe` as the p25_decode row's handler ([`crate::voice_call::fake_mbe`]).
    struct P25Row;

    impl fake_mbe::Row for P25Row {
        const JSON: &'static str = include_str!("../../../modes/p25_decode.json");
    }

    type Mbe = fake_mbe::Mbe<P25Row>;

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

        /// The last `p25.receiver` noted.
        fn receiver(&self) -> Option<Value> {
            self.notes
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find_map(|n| n[PROTOCOL].get("receiver").cloned())
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
        assert_eq!(e["summary"], "control channel, NAC 291", "{e}");
        assert_eq!(h.trunking_units(), Some(40));
        assert!(h.vocoder_notes().is_empty(), "{:?}", h.vocoder_notes());
    }

    #[tokio::test]
    async fn every_summary_reads_without_null() {
        // A call, then a control channel on its NAC: the call's two events, then the
        // channel's.
        let mbe = Mbe::absent();
        let mut h = Harness::start(&mbe, 4096);
        h.send(&call(clear(), 1), RATE);
        h.send(&on_air(&encode_control(NAC, 10), RATE), RATE);
        let r = h.end(|_| true).await;
        assert!(r.is_ok(), "{r:?}");
        let summaries: Vec<&str> = h
            .events
            .iter()
            .map(|e| e["summary"].as_str().unwrap())
            .collect();
        assert_eq!(
            summaries,
            [
                "9990001 > 65000: group no_vocoder",
                "9990001 > 65000: group no_vocoder",
                "control channel, NAC 291"
            ]
        );
        // A channel with no trunking seen, through the row's envelope.
        let row = mbe.row();
        let e = zeta_core::modes::summary::envelope(&row, 0, channel_fields(NAC, false));
        summaries
            .iter()
            .copied()
            .chain([e["summary"].as_str().unwrap()])
            .for_each(|s| assert!(!s.contains("null"), "{s}"));
        assert_eq!(e["summary"], "channel, NAC 291", "{e}");
    }

    /// Dibits in a terminator with link control.
    const TDULC_DIBITS: usize = 216;
    /// The gap of carrier between the two calls of [`two_calls_cut`], ms.
    const CUT_GAP_MS: usize = 50;
    const AES_ENC: EncSync = EncSync {
        algid: 0xAA,
        kid: 7,
    };

    /// A clear call of two superframes with its terminator cut off, [`CUT_GAP_MS`] of
    /// carrier (off the first call's unit grid), then a call of two superframes encrypted
    /// with `enc`, its link control marked as `mark` says, with its header cut off; and the
    /// time the second call's first sync is at, ns.
    fn two_calls_cut(enc: EncSync, mark: LcMark) -> (Vec<Complex<f32>>, u64) {
        let first = encode_call(NAC, group(), clear(), 2);
        let mut iq = on_air(&first[..first.len() - TDULC_DIBITS], RATE);
        iq.extend(carrier(CUT_GAP_MS));
        let at = T0 + iq.len() as u64 * 1_000_000_000 / RATE;
        let second = encode_call_lc(NAC, group(), enc, 2, mark);
        iq.extend(on_air(&second[HDU_DIBITS..], RATE));
        (iq, at)
    }

    /// How far a unit's time (its sync's first symbol centre, as the front end finds it) may
    /// sit from where the test placed it, ns: well under one frame (20 ms).
    const SLACK_NS: u64 = 1_000_000;

    /// The latest end of any audio frame's span.
    fn audio_end(audio: &[Frame]) -> u64 {
        audio
            .iter()
            .map(|f| f.provenance.span.end_ns.0)
            .max()
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn an_encrypted_call_that_follows_a_cut_one_is_never_heard() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        let (iq, second) = two_calls_cut(AES_ENC, LcMark::ByAlgorithm);
        h.send(&iq, RATE);
        let r = h
            .end(|h| h.events.len() >= 4 && h.audio.len() >= 2 * SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        // Two calls, though the second has the first's radio and talkgroup.
        assert_eq!(
            eots(&h.events),
            [false, true, false, true],
            "{:?}",
            h.events
        );
        has(
            &h.events[1],
            json!({"audio": true, "silent": null, "frames": 36}),
        );
        for e in &h.events[2..] {
            has(
                e,
                json!({"src": SRC, "encrypted": true, "audio": false, "silent": "encrypted"}),
            );
        }
        has(&h.events[3], json!({"algid": 170, "kid": 7}));
        // The first call closes where its own last unit ended, not at the second's.
        let ended = T0 + ((HDU_DIBITS + 4 * LDU_DIBITS) * SPD) as u64 * 1_000_000_000 / RATE;
        assert!(
            t_ns(&h.events[1]).abs_diff(ended) < 2_000_000,
            "{}",
            h.events[1]
        );
        // Every frame of speech is the first call's.
        assert_eq!(h.audio.len(), 2 * SUPERFRAME_FRAMES);
        assert!(
            audio_end(&h.audio) <= second + SLACK_NS,
            "{} > {second}",
            audio_end(&h.audio)
        );
        assert_eq!(mbe.log(), "start\neof\n", "one vocoder, the first call's");
    }

    #[tokio::test]
    async fn a_call_with_its_link_control_encrypted_is_never_heard() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        let (iq, second) = two_calls_cut(AES_ENC, LcMark::Encrypted);
        h.send(&iq, RATE);
        let r = h
            .end(|h| h.events.len() >= 2 && h.audio.len() >= 2 * SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        // The first call closes whole and clear; the second, with no radio to name, gives
        // no event.
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(
            &h.events[1],
            json!({"audio": true, "silent": null, "encrypted": false, "frames": 36}),
        );
        assert_eq!(h.audio.len(), 2 * SUPERFRAME_FRAMES);
        assert!(
            audio_end(&h.audio) <= second + SLACK_NS,
            "{} > {second}",
            audio_end(&h.audio)
        );
        assert_eq!(mbe.log(), "start\neof\n", "one vocoder, the first call's");
    }

    #[tokio::test]
    async fn a_clear_call_that_follows_a_cut_one_is_heard_from_its_first_ldu2() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        let (iq, second) = two_calls_cut(clear(), LcMark::ByAlgorithm);
        h.send(&iq, RATE);
        // The second call: its first LDU1 held, then 27 frames heard.
        let r = h
            .end(|h| h.events.len() >= 4 && h.audio.len() >= 2 * SUPERFRAME_FRAMES + 27)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(
            eots(&h.events),
            [false, true, false, true],
            "{:?}",
            h.events
        );
        has(&h.events[2], json!({"audio": true, "silent": null}));
        has(&h.events[3], json!({"audio": true, "frames": 36}));
        let later: Vec<u64> = h
            .audio
            .iter()
            .map(|f| f.provenance.span.start_ns.0)
            .filter(|&s| s >= second)
            .collect();
        assert_eq!(later.len(), 27, "{later:?}");
        let first_ldu2 = second + LDU_NS;
        assert!(
            later.iter().min().unwrap().abs_diff(first_ldu2) < 2_000_000,
            "{later:?} vs {first_ldu2}"
        );
    }

    /// A call of `first` for one superframe that goes on, the same radio to the same
    /// talkgroup with no break, as `then` for another; and when the second superframe's
    /// LDU1 and LDU2 start, ns.
    fn turning(first: EncSync, then: EncSync) -> (Vec<Complex<f32>>, u64, u64) {
        let a = encode_call(NAC, group(), first, 1);
        let b = encode_call(NAC, group(), then, 1);
        let mut dibits = a[..HDU_DIBITS + 2 * LDU_DIBITS].to_vec();
        dibits.extend(&b[HDU_DIBITS..]);
        let at = |dibit: usize| T0 + (dibit * SPD) as u64 * 1_000_000_000 / RATE;
        let ldu1 = HDU_DIBITS + 2 * LDU_DIBITS;
        (on_air(&dibits, RATE), at(ldu1), at(ldu1 + LDU_DIBITS))
    }

    #[tokio::test]
    async fn a_call_that_turns_encrypted_stops_feeding_at_once() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        let (iq, turned, _) = turning(clear(), AES_ENC);
        h.send(&iq, RATE);
        let r = h
            .end(|h| h.events.len() >= 2 && h.audio.len() >= SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(&h.events[0], json!({"audio": true, "encrypted": false}));
        has(
            &h.events[1],
            json!({
                "encrypted": true, "audio": false, "silent": "encrypted", "algid": 170,
                "frames": 36, "program": true
            }),
        );
        // The first superframe's speech, all of it handed back, and none from the
        // protected LDU1 on: its frames never reached the vocoder.
        assert_eq!(h.audio.len(), SUPERFRAME_FRAMES);
        assert!(
            audio_end(&h.audio) <= turned + SLACK_NS,
            "{} > {turned}",
            audio_end(&h.audio)
        );
    }

    #[tokio::test]
    async fn an_encrypted_call_that_turns_clear_is_heard_from_then() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        let (iq, _, cleared) = turning(AES_ENC, clear());
        h.send(&iq, RATE);
        let r = h.end(|h| h.events.len() >= 2 && h.audio.len() >= 9).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(&h.events[0], json!({"audio": false, "silent": "encrypted"}));
        has(
            &h.events[1],
            json!({"encrypted": false, "audio": true, "silent": null, "program": true}),
        );
        // The second superframe's LDU2, whose encryption sync says clear, and nothing before.
        assert_eq!(h.audio.len(), 9);
        let first = h.audio.iter().map(|f| f.provenance.span.start_ns.0).min();
        assert!(
            first.unwrap().abs_diff(cleared) < 2_000_000,
            "{first:?} vs {cleared}"
        );
    }

    /// A call of three superframes, `enc` in its header, whose LDU2s send `sync(superframe,
    /// their own coded sync)`; its link control is marked by algorithm.
    fn synced(enc: EncSync, sync: &dyn Fn(u32, Vec<u8>) -> Vec<u8>) -> Vec<u8> {
        encode_call_syncs(NAC, group(), enc, 3, LcMark::ByAlgorithm, sync)
    }

    /// `second`'s dibits with its header cut off, after a clear call of two superframes cut
    /// short and [`CUT_GAP_MS`] of carrier; and when `second`'s first sync is, ns.
    fn after_a_cut_call(second: &[u8]) -> (Vec<Complex<f32>>, u64) {
        let first = encode_call(NAC, group(), clear(), 2);
        let mut iq = on_air(&first[..first.len() - TDULC_DIBITS], RATE);
        iq.extend(carrier(CUT_GAP_MS));
        let at = T0 + iq.len() as u64 * 1_000_000_000 / RATE;
        iq.extend(on_air(&second[HDU_DIBITS..], RATE));
        (iq, at)
    }

    /// The speech frames that start at or after `t`, by start.
    fn starts_from(audio: &[Frame], t: u64) -> Vec<u64> {
        let mut s: Vec<u64> = audio
            .iter()
            .map(|f| f.provenance.span.start_ns.0)
            .filter(|&s| s >= t)
            .collect();
        s.sort_unstable();
        s
    }

    /// A clear call of three superframes with its header, superframe `sf`'s LDU2 sending
    /// `sync` as its coded encryption sync.
    fn sync_in(sf: u32, sync: Vec<u8>) -> Vec<u8> {
        synced(clear(), &|n, es| if n == sf { sync.clone() } else { es })
    }

    /// A clear call of three superframes with its header, superframe `sf`'s LDU2 sending
    /// its encryption sync's blocks as `dibit` throughout (a carrier at that level).
    fn level_in(sf: usize, dibit: u8) -> Vec<u8> {
        let mut dibits = encode_call(NAC, group(), clear(), 3);
        level_blocks(&mut dibits, 2 * sf + 1, dibit);
        dibits
    }

    /// `dibits`, a clear call with its header whose second superframe's LDU2 sync decides
    /// nothing: the call stays clear and is heard whole.
    async fn stays_clear(dibits: Vec<u8>) {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        h.send(&on_air(&dibits, RATE), RATE);
        let r = h
            .end(|h| h.events.len() >= 2 && h.audio.len() >= 3 * SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(
            &h.events[1],
            json!({"encrypted": false, "algid": 128, "audio": true, "silent": null, "frames": 54}),
        );
        assert_eq!(h.audio.len(), 3 * SUPERFRAME_FRAMES);
    }

    /// `second`, a clear call joined after its header whose first LDU2 sync licenses
    /// nothing: the call's speech is held until the next LDU2's, and its first frame heard
    /// is that LDU2's.
    async fn licensed_only_later(second: Vec<u8>) {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        let (iq, at) = after_a_cut_call(&second);
        h.send(&iq, RATE);
        let r = h
            .end(|h| h.events.len() >= 4 && h.audio.len() >= 2 * SUPERFRAME_FRAMES + 27)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(
            eots(&h.events),
            [false, true, false, true],
            "{:?}",
            h.events
        );
        has(&h.events[3], json!({"encrypted": false, "audio": true}));
        // The second superframe's LDU2 and the third superframe: 27 frames, none before.
        let later = starts_from(&h.audio, at);
        assert_eq!(later.len(), 27, "{later:?}");
        let licensed = at + 3 * LDU_NS;
        assert!(
            later[0].abs_diff(licensed) < 2_000_000,
            "{later:?} vs {licensed}"
        );
    }

    #[tokio::test]
    async fn an_all_zero_encryption_sync_neither_marks_nor_licenses_a_call() {
        stays_clear(sync_in(1, coded_sync(None))).await;
        licensed_only_later(sync_in(0, coded_sync(None))).await;
    }

    #[tokio::test]
    async fn an_encryption_sync_past_the_correction_limit_decides_nothing() {
        // 3 symbols wrong: within the code's reach (4), past the limit (2).
        let wrong = [0, 9, 18];
        stays_clear(sync_in(1, damaged_sync(coded_sync(Some(AES_ENC)), &wrong))).await;
        licensed_only_later(sync_in(0, damaged_sync(coded_sync(Some(clear())), &wrong))).await;
    }

    #[tokio::test]
    async fn a_carrier_at_an_outer_level_neither_marks_nor_licenses_a_call() {
        // Dibit 01 is +3, 11 is -3: a carrier at +1800 or -1800 Hz for the sync's blocks.
        for dibit in [0b01, 0b11] {
            stays_clear(level_in(1, dibit)).await;
            licensed_only_later(level_in(0, dibit)).await;
        }
    }

    #[tokio::test]
    async fn a_header_past_its_correction_limit_licenses_nothing() {
        // The header's encryption fields need 5 or 8 symbols corrected: within the code's
        // reach (8), past the limit (4). The header then says nothing, the clear LDU1 after
        // it feeds nothing, and the speech starts at the first LDU2, whose sync counts.
        let cases: [&[usize]; 2] = [&[0, 3, 6, 9, 12], &[0, 2, 4, 6, 8, 10, 12, 14]];
        for wrong in cases {
            let mbe = Mbe::installed();
            let mut h = Harness::start(&mbe, 4096);
            let dibits =
                encode_call_header(NAC, group(), clear(), 3, &|hex| damaged_sync(hex, wrong));
            h.send(&on_air(&dibits, RATE), RATE);
            let r = h
                .end(|h| h.events.len() >= 2 && h.audio.len() >= 2 * SUPERFRAME_FRAMES + 9)
                .await;
            assert!(r.is_ok(), "{r:?}");
            let n = wrong.len();
            assert_eq!(eots(&h.events), [false, true], "{n}: {:?}", h.events);
            // Opened at the first LDU2, its 18th frame, once its sync said clear.
            has(
                &h.events[0],
                json!({"frames": 18, "audio": true, "silent": null, "algid": 128}),
            );
            has(&h.events[1], json!({"frames": 54, "audio": true}));
            // The first LDU1's 9 frames never fed; every frame from the first LDU2 on.
            assert_eq!(
                h.audio.len(),
                2 * SUPERFRAME_FRAMES + 9,
                "{n} symbols wrong"
            );
            let ldu2 = T0 + ((HDU_DIBITS + LDU_DIBITS) * SPD) as u64 * 1_000_000_000 / RATE;
            let starts = starts_from(&h.audio, 0);
            assert!(
                starts[0].abs_diff(ldu2) < 2_000_000,
                "{n}: {:?} vs {ldu2}",
                starts.first()
            );
            assert_eq!(t_ns(&h.events[0]), starts[0], "{n}");
            assert_eq!(runs(&mbe), 1, "{n}: {}", mbe.log());
        }
    }

    #[tokio::test]
    async fn an_encrypted_call_goes_silent_at_its_first_trustworthy_sync() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        // Clear in its header and its link control. The second superframe's LDU2 names AES
        // past the correction limit, the third's within it.
        let dibits = synced(clear(), &|sf, es| match sf {
            0 => es,
            1 => damaged_sync(coded_sync(Some(AES_ENC)), &[0, 9, 18]),
            _ => coded_sync(Some(AES_ENC)),
        });
        h.send(&on_air(&dibits, RATE), RATE);
        let r = h
            .end(|h| h.events.len() >= 2 && h.audio.len() >= 2 * SUPERFRAME_FRAMES + 9)
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(&h.events[0], json!({"encrypted": false, "audio": true}));
        has(
            &h.events[1],
            json!({"encrypted": true, "algid": 170, "kid": 7, "silent": "encrypted", "frames": 54}),
        );
        // Every frame up to the third superframe's LDU2, and none from it on.
        let turned = T0 + ((HDU_DIBITS + 5 * LDU_DIBITS) * SPD) as u64 * 1_000_000_000 / RATE;
        assert_eq!(h.audio.len(), 2 * SUPERFRAME_FRAMES + 9);
        assert!(
            audio_end(&h.audio) <= turned + SLACK_NS,
            "{} > {turned}",
            audio_end(&h.audio)
        );
    }

    #[tokio::test]
    async fn a_call_after_a_lead_in_opens_at_its_header() {
        let mut noise = carrier(1000);
        awgn(&mut noise, -10.0, 7);
        for (name, lead) in [("noise", noise), ("carrier", carrier(200))] {
            let mbe = Mbe::installed();
            let mut h = Harness::start(&mbe, 4096);
            let mut iq = lead.clone();
            iq.extend(call(clear(), 3));
            h.send(&iq, RATE);
            let r = h
                .end(|h| h.events.len() >= 2 && h.audio.len() >= 3 * SUPERFRAME_FRAMES)
                .await;
            assert!(r.is_ok(), "{name}: {r:?}");
            assert_eq!(eots(&h.events), [false, true], "{name}: {:?}", h.events);
            // Opened at the first LDU1 after the header (the mid-call path opens on an LDU2,
            // 18 frames in).
            has(&h.events[0], json!({"frames": 9, "audio": true}));
            let ldu1 = T0 + (lead.len() + HDU_DIBITS * SPD) as u64 * 1_000_000_000 / RATE;
            assert!(
                t_ns(&h.events[0]).abs_diff(ldu1) < 2_000_000,
                "{name}: {}",
                h.events[0]
            );
            has(&h.events[1], json!({"frames": 54}));
            assert_eq!(h.audio.len(), 54, "{name}");
        }
    }

    /// The LDU at `index` (0 the first LDU1) of an `encode_call` call lost: its sync and
    /// network identifier overwritten, so the framer never finds it, the rest kept so the
    /// front end stays locked and the next unit keeps its place.
    fn lose(dibits: &mut [u8], index: usize) {
        let at = HDU_DIBITS + index * LDU_DIBITS;
        for (k, d) in dibits[at..at + 57].iter_mut().enumerate() {
            *d = (k * 3 % 4) as u8;
        }
    }

    /// The vocoder runs the fake's log shows.
    fn runs(mbe: &Mbe) -> usize {
        mbe.log().lines().filter(|l| *l == "start").count()
    }

    #[tokio::test]
    async fn a_lost_ldu2_does_not_split_the_call() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        let mut dibits = encode_call(NAC, group(), clear(), 3);
        lose(&mut dibits, 3);
        h.send(&on_air(&dibits, RATE), RATE);
        // 54 frames less the lost LDU2's 9 and the next LDU1's 9, held until the LDU2
        // after it says clear.
        let r = h.end(|h| h.events.len() >= 2 && h.audio.len() >= 36).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(
            &h.events[1],
            json!({"frames": 45, "audio": true, "silent": null}),
        );
        assert_eq!(h.audio.len(), 36);
        assert!(h.vocoder_notes().is_empty(), "{:?}", h.vocoder_notes());
        // Its vocoder was parked at the loss and a new one started at the next word.
        assert_eq!(runs(&mbe), 2, "{}", mbe.log());
    }

    #[tokio::test]
    async fn a_lost_ldu1_does_not_split_the_call() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        let mut dibits = encode_call(NAC, group(), clear(), 3);
        lose(&mut dibits, 2);
        h.send(&on_air(&dibits, RATE), RATE);
        // 54 frames less the lost LDU1's 9: the LDU2 after it carries its own word.
        let r = h.end(|h| h.events.len() >= 2 && h.audio.len() >= 45).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(
            &h.events[1],
            json!({"frames": 45, "audio": true, "silent": null}),
        );
        assert_eq!(h.audio.len(), 45);
        assert!(h.vocoder_notes().is_empty(), "{:?}", h.vocoder_notes());
    }

    #[tokio::test]
    async fn a_failed_vocoder_stays_failed_for_the_call() {
        // The vocoder dies after 10 frames of the first superframe; the second superframe's
        // LDU2 is lost, so the call is held again until the third's LDU2 says clear: no
        // vocoder is started again in the call.
        let mbe = Mbe::installed();
        mbe.die_once();
        let mut dibits = encode_call(NAC, group(), clear(), 3);
        lose(&mut dibits, 3);
        let iq = on_air(&dibits, RATE);
        let mut h = Harness::start(&mbe, 4096);
        let split = (HDU_DIBITS + 2 * LDU_DIBITS + 60) * SPD;
        h.send(&iq[..split], RATE);
        assert!(
            h.until(|h| h.vocoder_notes().iter().any(|n| n.contains("exited")))
                .await,
            "{:?}",
            h.vocoder_notes()
        );
        h.send(&iq[split..], RATE);
        let r = h.end(|h| h.events.len() >= 2 && h.audio.len() >= 10).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.audio.len(), 10);
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(
            &h.events[1],
            json!({"silent": "vocoder_failed", "audio": false, "frames": 45}),
        );
        assert_eq!(runs(&mbe), 1, "{}", mbe.log());
    }

    #[tokio::test]
    async fn a_call_losing_three_units_is_still_one_call() {
        // Five superframes, LDUs 0 to 9; lost: 2 (an LDU1), 5 (an LDU2, so 6 is held) and 8
        // (an LDU1). Fed: 0, 1, 3, 4, 7, 9, 54 frames; received: 7 units, 63 frames.
        let mut dibits = encode_call(NAC, group(), clear(), 5);
        for index in [2, 5, 8] {
            lose(&mut dibits, index);
        }
        let iq = on_air(&dibits, RATE);
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        h.send(&iq, RATE);
        let r = h.end(|h| h.events.len() >= 2 && h.audio.len() >= 54).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(
            &h.events[1],
            json!({"src": SRC, "frames": 63, "audio": true}),
        );
        assert_eq!(h.audio.len(), 54);
        assert!(h.vocoder_notes().is_empty(), "{:?}", h.vocoder_notes());
        // Without a vocoder: still one call, and the missing program noted once.
        let mbe = Mbe::absent();
        let mut h = Harness::start(&mbe, 4096);
        h.send(&iq, RATE);
        let r = h.end(|h| h.events.len() >= 2).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(&h.events[1], json!({"frames": 63, "silent": "no_vocoder"}));
        assert_eq!(h.vocoder_notes().len(), 1, "{:?}", h.vocoder_notes());
    }

    #[tokio::test]
    async fn an_unmarked_encrypted_call_off_the_grid_is_never_heard() {
        // The second call's link control decodes and says nothing of its encryption: only
        // the split (it is off the first call's grid) keeps its first LDU1 from the first
        // call's vocoder.
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        let (iq, second) = two_calls_cut(AES_ENC, LcMark::Unmarked);
        h.send(&iq, RATE);
        let r = h
            .end(|h| h.events.len() >= 4 && h.audio.len() >= 2 * SUPERFRAME_FRAMES)
            .await;
        assert!(r.is_ok(), "{r:?}");
        // The speech first: no frame of the encrypted call.
        assert!(
            audio_end(&h.audio) <= second + SLACK_NS,
            "{} > {second}",
            audio_end(&h.audio)
        );
        assert_eq!(h.audio.len(), 2 * SUPERFRAME_FRAMES);
        assert_eq!(
            eots(&h.events),
            [false, true, false, true],
            "{:?}",
            h.events
        );
        for e in &h.events[2..] {
            has(
                e,
                json!({"encrypted": true, "audio": false, "silent": "encrypted"}),
            );
        }
    }

    #[tokio::test]
    async fn an_unmarked_encrypted_call_on_the_grid_is_never_heard() {
        // A clear call's header, LDU1, LDU2 and LDU1, a lost LDU2, then, just where the
        // next LDU1 belongs, an encrypted call with the same radio and talkgroup whose link
        // control says nothing of its encryption. It stays the same call; only the hold
        // after the loss keeps that LDU1 from the vocoder.
        let first = encode_call(NAC, group(), clear(), 2);
        let mut dibits = first[..HDU_DIBITS + 4 * LDU_DIBITS].to_vec();
        lose(&mut dibits, 3);
        let encrypted_from = HDU_DIBITS + 4 * LDU_DIBITS;
        let second = encode_call_lc(NAC, group(), AES_ENC, 2, LcMark::Unmarked);
        dibits.extend(&second[HDU_DIBITS..]);
        let at = T0 + (encrypted_from * SPD) as u64 * 1_000_000_000 / RATE;
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        h.send(&on_air(&dibits, RATE), RATE);
        let r = h.end(|h| h.events.len() >= 2 && h.audio.len() >= 27).await;
        assert!(r.is_ok(), "{r:?}");
        // The speech first: the first three LDUs' 27 frames, none of the encrypted part.
        assert!(
            audio_end(&h.audio) <= at + SLACK_NS,
            "{} > {at}",
            audio_end(&h.audio)
        );
        assert_eq!(h.audio.len(), 27);
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        has(
            &h.events[1],
            json!({"encrypted": true, "audio": false, "silent": "encrypted", "algid": 170}),
        );
    }

    #[tokio::test]
    async fn a_protected_link_control_outweighs_a_clear_encryption_sync() {
        // Every LDU1 marked protected, every LDU2 (and the header) saying clear: the call
        // is encrypted, and no frame is fed.
        let clear_call = encode_call(NAC, group(), clear(), 2);
        let marked = encode_call(NAC, group(), AES_ENC, 2);
        let mut dibits = clear_call[..HDU_DIBITS].to_vec();
        for sf in 0..2 {
            let ldu1 = HDU_DIBITS + 2 * sf * LDU_DIBITS;
            dibits.extend(&marked[ldu1..ldu1 + LDU_DIBITS]);
            dibits.extend(&clear_call[ldu1 + LDU_DIBITS..ldu1 + 2 * LDU_DIBITS]);
        }
        dibits.extend(&clear_call[HDU_DIBITS + 4 * LDU_DIBITS..]);
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        h.send(&on_air(&dibits, RATE), RATE);
        let r = h.end(|h| h.events.len() >= 2).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(eots(&h.events), [false, true], "{:?}", h.events);
        for e in &h.events {
            has(
                e,
                json!({"encrypted": true, "audio": false, "silent": "encrypted", "algid": 128}),
            );
        }
        assert!(h.audio.is_empty(), "{} audio frames", h.audio.len());
    }

    #[tokio::test]
    async fn the_receivers_counts_are_in_the_job_context() {
        // A call at the declared deviation: one lock at least.
        let mbe = Mbe::absent();
        let mut h = Harness::start(&mbe, 4096);
        h.send(&call(clear(), 1), RATE);
        let r = h
            .end(|h| h.receiver().is_some_and(|c| c["locks"].as_u64() >= Some(1)))
            .await;
        assert!(r.is_ok(), "{r:?}");
        let c = h.receiver().expect("p25.receiver noted");
        for k in ["locks", "false_locks", "losses", "off_deviation"] {
            assert!(c[k].is_u64(), "{k}: {c}");
        }
        assert_eq!(c["off_deviation"], 0, "{c}");
        // The clear call's header run does not drop or undo its lock: one lock that stood,
        // none false, and the one loss comes from the zero tail after the call, read after
        // the input ends (the carrier gone).
        assert_eq!(c["locks"], 1, "{c}");
        assert_eq!(c["losses"], 1, "{c}");
        assert_eq!(c["false_locks"], 0, "{c}");
        // A transmitter at well under a third of P25's deviation: refused, and counted.
        let mut h = Harness::start(&mbe, 4096);
        let dibits = encode_call(NAC, group(), clear(), 2);
        h.send(&c4fm_modulate(&dibits, P25_BAUD, 500.0, RATE as u32), RATE);
        let r = h
            .end(|h| {
                h.receiver()
                    .is_some_and(|c| c["off_deviation"].as_u64() >= Some(1))
            })
            .await;
        assert!(r.is_ok(), "{r:?}");
        assert!(h.events.is_empty(), "{:?}", h.events);
        let c = h.receiver().expect("p25.receiver noted");
        assert!(c["off_deviation"].as_u64() >= Some(1), "{c}");
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
    async fn a_call_ending_with_its_input_closes_at_its_terminator() {
        let mbe = Mbe::installed();
        let mut h = Harness::start(&mbe, 4096);
        // The call, 50 ms of carrier, then its terminator, and there the input ends: no
        // samples after it for the receive filters. The carrier puts the terminator 50 ms
        // past the last LDU's end, where the input's end alone would close the call.
        let dibits = encode_call(NAC, group(), clear(), 3);
        let body = HDU_DIBITS + 6 * LDU_DIBITS;
        let mut iq = c4fm_modulate(&dibits[..body], P25_BAUD, P25_DEVIATION_HZ, RATE as u32);
        let last = *iq.last().unwrap();
        let gap = RATE as usize / 20;
        iq.extend(std::iter::repeat_n(last, gap));
        let terminator = c4fm_modulate(&dibits[body..], P25_BAUD, P25_DEVIATION_HZ, RATE as u32);
        iq.extend(terminator.into_iter().map(|s| s * last));
        h.send(&iq, RATE);
        let r = h.end(|h| h.events.len() >= 2).await;
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(h.events.len(), 2, "{:?}", h.events);
        let b = &h.events[1];
        has(b, json!({"src": SRC, "eot": true, "frames": 54}));
        let at = T0 + (body * SPD + gap) as u64 * 1_000_000_000 / RATE;
        assert!(
            t_ns(b).abs_diff(at) < 2_000_000,
            "{b}: the terminator is at {at}"
        );
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
