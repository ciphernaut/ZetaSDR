//! RDS (Radio Data System, EN 50067 / RBDS) decoding from broadcast FM.
//!
//! RDS rides the FM composite as a 57 kHz DSB-SC subcarrier, phase-locked to
//! the third harmonic of the 19 kHz stereo pilot, carrying differentially
//! encoded biphase (Manchester) symbols at 1187.5 Bd. Data comes in 104-bit
//! groups of four 26-bit blocks, each 16 data bits and a 10-bit check word
//! (CRC with generator `x^10+x^8+x^7+x^5+x^4+x^3+1`) to which one of five
//! offset words is added: A, B, C (or C' in version B groups), D.
//!
//! [`RdsDemod`] turns the discriminator composite into data bits,
//! [`RdsSync`] finds group boundaries, [`RdsState`] collects the programme
//! service name (PS, type 0 groups) and radio text (RT, type 2), and
//! [`decode_rds_iq`] runs the three on complex baseband.

use std::f64::consts::PI;
use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{StreamId, StreamKind};

use crate::nbfm::nbfm_demod;
use crate::resample::Resampler;
use crate::{Input, run_map};

/// Rate [`RdsDemod`] works at; other composite rates are resampled to it.
pub const RDS_RATE_HZ: u64 = 240_000;
/// RDS subcarrier, the pilot's third harmonic.
pub const SUBCARRIER_HZ: f64 = 57_000.0;
/// Broadcast FM stereo pilot.
pub const PILOT_HZ: f64 = 19_000.0;
/// RDS symbol (data bit) rate.
pub const BAUD: f64 = 1_187.5;

/// FM deviation that puts the composite in ±1 units.
const DEVIATION_HZ: u64 = 75_000;
/// Check-word generator `x^10+x^8+x^7+x^5+x^4+x^3+1`.
const POLY: u32 = 0x5B9;
/// Half-width of the pilot band-pass.
const PILOT_BW_HZ: f64 = 100.0;
/// Half-width of the RDS band around the subcarrier, and the baseband low-pass.
const RDS_BW_HZ: f64 = 2_400.0;
/// Baseband is decimated by this after the low-pass: 24 kHz, ~20.2 samples a symbol.
const DECIM: usize = 10;
/// Pilot PLL natural frequency and damping.
const PLL_WN_HZ: f64 = 20.0;
const PLL_ZETA: f64 = 0.707;
/// Early-late gate: correction per symbol as a fraction of a symbol, times
/// the normalised (late - early) error.
const TIMING_GAIN: f64 = 0.02;
/// Smoothing of per-symbol magnitudes used to catch a half-symbol slip.
const SLIP_ALPHA: f64 = 0.02;
/// Symbols between half-symbol slip checks; a slip needs the half-offset
/// magnitude this much larger than the on-time one.
const SLIP_HOLDOFF: u32 = 64;
const SLIP_RATIO: f64 = 1.3;
/// Lowest composite rate holding the RDS band below Nyquist.
const MIN_RATE_HZ: u64 = 2 * (SUBCARRIER_HZ as u64 + RDS_BW_HZ as u64);

/// The five RDS offset words, naming a block's position in its group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Offset {
    A,
    B,
    C,
    /// C', block 3 of a version B group (which repeats the PI there).
    Cp,
    D,
}

impl Offset {
    const ALL: [Offset; 5] = [Offset::A, Offset::B, Offset::C, Offset::Cp, Offset::D];

    /// The 10-bit offset word added to the check word.
    fn word(self) -> u16 {
        match self {
            Offset::A => 0x0FC,
            Offset::B => 0x198,
            Offset::C => 0x168,
            Offset::Cp => 0x350,
            Offset::D => 0x1B4,
        }
    }
}

/// Remainder of the polynomial `w` (any width up to 32 bits) modulo [`POLY`].
fn remainder(mut w: u32) -> u16 {
    for i in (10..32).rev() {
        if w & (1 << i) != 0 {
            w ^= POLY << (i - 10);
        }
    }
    w as u16
}

/// 10-bit check word for `data` in the block position `offset`: the
/// remainder of `data · x^10` modulo the generator, plus the offset word.
pub fn checkword(data: u16, offset: Offset) -> u16 {
    remainder((data as u32) << 10) ^ offset.word()
}

/// Which offset the 26-bit `block` (data then check word, in the low bits)
/// carries: its syndrome, the remainder of the whole block, is the offset
/// word itself when error-free. `None` when it matches no offset. No error
/// correction.
pub fn block_offset(block: u32) -> Option<Offset> {
    let syn = remainder(block & 0x3FF_FFFF);
    Offset::ALL.into_iter().find(|o| o.word() == syn)
}

/// Transposed direct-form II biquad (RBJ cookbook designs), run in f64.
#[derive(Clone, Debug)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn from_rbj(b: [f64; 3], a: [f64; 3]) -> Self {
        Biquad {
            b: [b[0] / a[0], b[1] / a[0], b[2] / a[0]],
            a: [a[1] / a[0], a[2] / a[0]],
            z: [0.0; 2],
        }
    }

    /// Band-pass, 0 dB and zero phase at `f0`, quality `q`.
    fn bandpass(f0: f64, q: f64, rate: f64) -> Self {
        let w = 2.0 * PI * f0 / rate;
        let alpha = w.sin() / (2.0 * q);
        Self::from_rbj(
            [alpha, 0.0, -alpha],
            [1.0 + alpha, -2.0 * w.cos(), 1.0 - alpha],
        )
    }

    /// Low-pass at `fc`, quality `q`.
    fn lowpass(fc: f64, q: f64, rate: f64) -> Self {
        let w = 2.0 * PI * fc / rate;
        let (sn, cs) = w.sin_cos();
        let alpha = sn / (2.0 * q);
        let b1 = 1.0 - cs;
        Self::from_rbj(
            [b1 / 2.0, b1, b1 / 2.0],
            [1.0 + alpha, -2.0 * cs, 1.0 - alpha],
        )
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Fourth-order Butterworth low-pass as two biquads.
fn butter4(fc: f64, rate: f64) -> [Biquad; 2] {
    [
        Biquad::lowpass(fc, 0.541_196_1, rate),
        Biquad::lowpass(fc, 1.306_563, rate),
    ]
}

/// Streaming RDS demodulator: composite (discriminator output, ±1 at 75 kHz
/// deviation) in, data bits out.
///
/// At [`RDS_RATE_HZ`]: a band-pass of the pilot (19 kHz ±100 Hz) feeds a
/// second-order PLL; three times its phase is the 57 kHz reference, which
/// mixes the RDS band (57 kHz ±2.4 kHz band-pass) to complex baseband,
/// low-passed at 2.4 kHz and decimated to 24 kHz. The standard allows the
/// subcarrier in phase or in quadrature with the pilot's third harmonic, so
/// the baseband is rotated onto the real axis by half the angle of its
/// smoothed square (the BPSK phase, its 180° ambiguity removed by the
/// differential code). A biphase correlator (first half-symbol minus second)
/// is sampled once a symbol at [`BAUD`], the instant kept on the peak by an
/// early-late gate, and moved half a symbol if the half-offset instant is
/// consistently stronger (biphase looks alike half a symbol out when
/// neighbouring symbols repeat). Each symbol's sign, differentially decoded,
/// is one data bit. A discriminator sign flip inverts pilot and subcarrier
/// together, so the output does not depend on it.
pub struct RdsDemod {
    resampler: Option<Resampler>,
    pilot_bp: Biquad,
    pilot_pow: f64,
    /// PLL phase (cycles of the pilot, wrapped) and frequency (cycles a sample).
    theta: f64,
    freq: f64,
    integ: f64,
    kp: f64,
    ki: f64,
    rds_bp: Biquad,
    lp_re: [Biquad; 2],
    lp_im: [Biquad; 2],
    decim_phase: usize,
    /// Smoothed square of the baseband and the current BPSK rotation.
    sq_avg: Complex<f64>,
    rot_angle: f64,
    /// Last `2 * half` real baseband samples, for the biphase correlator.
    hist: Vec<f64>,
    hist_pos: usize,
    half: usize,
    /// Correlator output buffer: `corr[0]` is decimated sample `corr_base`.
    corr: Vec<f64>,
    corr_base: u64,
    corr_count: u64,
    /// Next sampling instant, in decimated samples, and samples a symbol.
    next_t: f64,
    sps: f64,
    on_avg: f64,
    off_avg: f64,
    since_slip: u32,
    prev_sym: u8,
}

impl RdsDemod {
    /// Demodulator for a composite at `rate_hz`, at least 118.8 kHz so the
    /// RDS band is below Nyquist; resampled to [`RDS_RATE_HZ`] when different.
    pub fn new(rate_hz: u64) -> Result<Self, String> {
        if rate_hz < MIN_RATE_HZ {
            return Err(format!(
                "rds: composite rate {rate_hz} Hz is below {MIN_RATE_HZ} Hz"
            ));
        }
        let resampler = if rate_hz == RDS_RATE_HZ {
            None
        } else {
            Some(Resampler::new(rate_hz, RDS_RATE_HZ)?)
        };
        let fs = RDS_RATE_HZ as f64;
        let wn = 2.0 * PI * PLL_WN_HZ / fs;
        let dec_rate = fs / DECIM as f64;
        let sps = dec_rate / BAUD;
        let half = (sps / 2.0).round() as usize;
        Ok(RdsDemod {
            resampler,
            pilot_bp: Biquad::bandpass(PILOT_HZ, PILOT_HZ / (2.0 * PILOT_BW_HZ), fs),
            pilot_pow: 0.0,
            theta: 0.0,
            freq: PILOT_HZ / fs,
            integ: 0.0,
            // Loop gains in cycles: phase error e (radians) / 2π.
            kp: 2.0 * PLL_ZETA * wn / (2.0 * PI),
            ki: wn * wn / (2.0 * PI),
            rds_bp: Biquad::bandpass(SUBCARRIER_HZ, SUBCARRIER_HZ / (2.0 * RDS_BW_HZ), fs),
            lp_re: butter4(RDS_BW_HZ, fs),
            lp_im: butter4(RDS_BW_HZ, fs),
            decim_phase: 0,
            sq_avg: Complex::new(0.0, 0.0),
            rot_angle: 0.0,
            hist: vec![0.0; 2 * half],
            hist_pos: 0,
            half,
            corr: Vec::new(),
            corr_base: 0,
            corr_count: 0,
            next_t: (2 * half) as f64 + sps,
            sps,
            on_avg: 0.0,
            off_avg: 0.0,
            since_slip: 0,
            prev_sym: 0,
        })
    }

    /// Demodulate the next block of composite; the data bits (0 or 1) whose
    /// symbols completed in it.
    pub fn feed(&mut self, composite: &[f32]) -> Vec<u8> {
        let resampled;
        let x = match &mut self.resampler {
            Some(r) => {
                resampled = r.process(composite);
                &resampled[..]
            }
            None => composite,
        };
        for &s in x {
            if let Some(z) = self.mix(s as f64) {
                self.push_baseband(z);
            }
        }
        self.symbols()
    }

    /// One composite sample through the pilot PLL and the RDS mixer; the
    /// low-passed complex baseband on every `DECIM`th sample.
    fn mix(&mut self, s: f64) -> Option<Complex<f64>> {
        let p = self.pilot_bp.run(s);
        self.pilot_pow += 1e-4 * (p * p - self.pilot_pow);
        let amp = (2.0 * self.pilot_pow).sqrt();
        // NCO estimate sin(2π θ); p · cos ≈ (A/2) sin(phase error).
        let phase = 2.0 * PI * self.theta;
        let e = if amp > 1e-9 {
            (2.0 * p * phase.cos() / amp).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self.integ += self.ki * e;
        self.theta = (self.theta + self.freq + self.integ + self.kp * e).rem_euclid(1.0);

        let r = self.rds_bp.run(s);
        let (sn, cs) = (3.0 * phase).sin_cos();
        let (mut re, mut im) = (r * sn, r * cs);
        for f in &mut self.lp_re {
            re = f.run(re);
        }
        for f in &mut self.lp_im {
            im = f.run(im);
        }
        self.decim_phase += 1;
        if self.decim_phase < DECIM {
            return None;
        }
        self.decim_phase = 0;
        Some(Complex::new(re, im))
    }

    /// Rotate a decimated baseband sample onto the real axis and run the
    /// biphase correlator over it.
    fn push_baseband(&mut self, z: Complex<f64>) {
        self.sq_avg += (z * z - self.sq_avg) * 2e-3;
        // Of the two angles with this square, the one nearer the last.
        let a = self.sq_avg.arg() / 2.0;
        let d = (a - self.rot_angle).rem_euclid(PI);
        self.rot_angle += if d > PI / 2.0 { d - PI } else { d };
        let v = (z * Complex::from_polar(1.0, -self.rot_angle)).re;

        // Correlator: the older half-window minus the newer one.
        let n = self.hist.len();
        self.hist[self.hist_pos] = v;
        self.hist_pos = (self.hist_pos + 1) % n;
        let mut c = 0.0;
        for k in 0..n {
            // Oldest first starting at hist_pos.
            let x = self.hist[(self.hist_pos + k) % n];
            c += if k < self.half { x } else { -x };
        }
        self.corr.push(c);
        self.corr_count += 1;
    }

    /// Correlator output at fractional decimated index `t`, linearly interpolated.
    fn corr_at(&self, t: f64) -> f64 {
        let i = (t.floor() as u64).saturating_sub(self.corr_base) as usize;
        let f = t - t.floor();
        let a = self.corr.get(i).copied().unwrap_or(0.0);
        let b = self.corr.get(i + 1).copied().unwrap_or(a);
        a + (b - a) * f
    }

    /// Take every symbol instant the correlator buffer now covers.
    fn symbols(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let delta = self.half as f64 / 2.0;
        let reach = self.sps / 2.0 + 2.0;
        while self.next_t + reach < self.corr_count as f64 {
            let t = self.next_t;
            let on = self.corr_at(t);
            let early = self.corr_at(t - delta).abs();
            let late = self.corr_at(t + delta).abs();
            let off = self.corr_at(t + self.sps / 2.0).abs();
            let sym = u8::from(on > 0.0);
            out.push(sym ^ self.prev_sym);
            self.prev_sym = sym;

            let err = (late - early) / (late + early + 1e-12);
            self.next_t += self.sps * (1.0 + TIMING_GAIN * err);
            self.on_avg += SLIP_ALPHA * (on.abs() - self.on_avg);
            self.off_avg += SLIP_ALPHA * (off - self.off_avg);
            self.since_slip += 1;
            if self.since_slip >= SLIP_HOLDOFF && self.off_avg > SLIP_RATIO * self.on_avg {
                self.next_t += self.sps / 2.0;
                (self.on_avg, self.off_avg) = (self.off_avg, self.on_avg);
                self.since_slip = 0;
            }
        }
        // Keep what the next instants (and their early gate) may still read.
        let keep_from = (self.next_t - self.sps).max(0.0) as u64;
        if keep_from > self.corr_base {
            let drop = ((keep_from - self.corr_base) as usize).min(self.corr.len());
            self.corr.drain(..drop);
            self.corr_base += drop as u64;
        }
        out
    }
}

/// Group synchroniser over the data bits.
///
/// Unsynchronised, it slides one bit at a time over the last 104 bits until
/// they are four blocks with offsets A, B, C or C', D; it then emits the
/// group's four data words, and from there on takes the bits 104 at a time
/// (block-aligned), emitting each group whose four offsets check, until two
/// consecutive groups fail and it slides again.
#[derive(Default)]
pub struct RdsSync {
    /// The bits not yet consumed, at most 104.
    window: std::collections::VecDeque<u8>,
    synced: bool,
    failures: u8,
}

impl RdsSync {
    /// Feed data bits (0 or 1); the groups completed, as their data words.
    pub fn feed(&mut self, bits: &[u8]) -> Vec<[u16; 4]> {
        let mut out = Vec::new();
        for &b in bits {
            self.window.push_back(b & 1);
            if self.window.len() > 104 {
                self.window.pop_front();
            }
            if self.window.len() < 104 {
                continue;
            }
            match self.group() {
                Some(g) => {
                    out.push(g);
                    self.window.clear();
                    self.synced = true;
                    self.failures = 0;
                }
                None if self.synced => {
                    self.failures += 1;
                    if self.failures >= 2 {
                        // Slide from here, keeping the window.
                        self.synced = false;
                        self.failures = 0;
                    } else {
                        self.window.clear();
                    }
                }
                None => {}
            }
        }
        out
    }

    /// The window as a group, if its four blocks carry A, B, C|C', D.
    fn group(&self) -> Option<[u16; 4]> {
        let mut words = [0u16; 4];
        for (i, w) in words.iter_mut().enumerate() {
            let block = self
                .window
                .range(26 * i..26 * (i + 1))
                .fold(0u32, |a, &b| (a << 1) | b as u32);
            let ok = matches!(
                (i, block_offset(block)?),
                (0, Offset::A) | (1, Offset::B) | (2, Offset::C | Offset::Cp) | (3, Offset::D)
            );
            if !ok {
                return None;
            }
            *w = (block >> 10) as u16;
        }
        Some(words)
    }
}

/// A station's decoded RDS state, built up group by group. `Default` is
/// written out because `[Option<char>; 64]` has no derived one.
pub struct RdsState {
    pi: Option<u16>,
    pty: u8,
    tp: bool,
    ps: [Option<char>; 8],
    rt: [Option<char>; 64],
    rt_ab: Option<bool>,
}

impl Default for RdsState {
    fn default() -> Self {
        RdsState {
            pi: None,
            pty: 0,
            tp: false,
            ps: [None; 8],
            rt: [None; 64],
            rt_ab: None,
        }
    }
}

/// A change worth reporting in a station's RDS: the state after the group
/// that caused it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RdsEvent {
    /// Programme identification, 4 upper-case hex digits.
    pub pi: String,
    /// Programme type, 0..=31.
    pub pty: u8,
    /// Traffic programme flag.
    pub tp: bool,
    /// Programme service name, once all 8 characters are in.
    pub ps: Option<String>,
    /// Radio text up to its `\r` terminator (or all of it when full),
    /// trailing spaces trimmed, once every character before that is in.
    pub rt: Option<String>,
    /// The group type that caused the event, e.g. "0A".
    pub group: String,
}

/// An RDS character: printable ASCII as is, anything else `?`. The basic
/// character table agrees with ASCII over 0x20..=0x7E (bar a few symbols).
fn rds_char(b: u8) -> char {
    if (0x20..=0x7E).contains(&b) {
        b as char
    } else {
        '?'
    }
}

impl RdsState {
    /// Apply one group's data words (blocks A..D). Returns an event when the
    /// PI first appears or changes (which also clears PS and RT), when the PS
    /// becomes complete or changes once complete, or when the RT becomes
    /// complete (gains its `\r` or fills) or changes once complete; at most
    /// one event a group.
    pub fn apply(&mut self, group: [u16; 4]) -> Option<RdsEvent> {
        let [a, b, c, d] = group;
        let pi_changed = self.pi != Some(a);
        if pi_changed {
            *self = RdsState {
                pi: Some(a),
                ..RdsState::default()
            };
        }
        let gtype = b >> 12;
        let version_b = (b >> 11) & 1 == 1;
        self.pty = ((b >> 5) & 0x1F) as u8;
        self.tp = (b >> 10) & 1 == 1;
        let ps_before = self.ps_text();
        let rt_before = self.rt_text(version_b);
        match (gtype, version_b) {
            (0, _) => {
                let seg = (b & 3) as usize;
                for (i, byte) in d.to_be_bytes().into_iter().enumerate() {
                    self.ps[2 * seg + i] = Some(rds_char(byte));
                }
            }
            (2, _) => {
                let ab = (b >> 4) & 1 == 1;
                if self.rt_ab.is_some_and(|x| x != ab) {
                    self.rt = [None; 64];
                }
                self.rt_ab = Some(ab);
                let seg = (b & 0xF) as usize;
                let bytes: Vec<u8> = if version_b {
                    d.to_be_bytes().to_vec()
                } else {
                    [c.to_be_bytes(), d.to_be_bytes()].concat()
                };
                let at = seg * bytes.len();
                for (i, byte) in bytes.into_iter().enumerate() {
                    self.rt[at + i] = Some(if byte == 0x0D { '\r' } else { rds_char(byte) });
                }
            }
            _ => {}
        }
        let ps = self.ps_text();
        let rt = self.rt_text(version_b);
        let fire =
            pi_changed || (ps.is_some() && ps != ps_before) || (rt.is_some() && rt != rt_before);
        fire.then(|| RdsEvent {
            pi: format!("{a:04X}"),
            pty: self.pty,
            tp: self.tp,
            ps,
            rt,
            group: format!("{gtype}{}", if version_b { 'B' } else { 'A' }),
        })
    }

    /// The PS once all 8 characters are in.
    fn ps_text(&self) -> Option<String> {
        self.ps.iter().copied().collect()
    }

    /// The RT up to its `\r` (or its full 64, or 32 for version B text) once
    /// every character before that is in, trailing spaces trimmed.
    fn rt_text(&self, version_b: bool) -> Option<String> {
        let limit = if version_b { 32 } else { 64 };
        let mut text = String::new();
        for slot in &self.rt[..limit] {
            match slot {
                Some('\r') => break,
                Some(ch) => text.push(*ch),
                None => return None,
            }
        }
        Some(text.trim_end_matches(' ').to_string())
    }
}

/// The whole RDS chain on complex baseband: FM discriminator at 75 kHz
/// deviation, [`RdsDemod`], [`RdsSync`] and [`RdsState`], with every piece's
/// state carried across [`feed`](Self::feed) calls.
pub struct RdsPipeline {
    prev: Complex<f32>,
    rate_hz: u64,
    demod: RdsDemod,
    sync: RdsSync,
    state: RdsState,
}

impl RdsPipeline {
    /// Pipeline for baseband at `rate_hz`; an error when the rate is too low
    /// for the RDS band.
    pub fn new(rate_hz: u64) -> Result<Self, String> {
        Ok(RdsPipeline {
            prev: Complex::new(0.0, 0.0),
            rate_hz,
            demod: RdsDemod::new(rate_hz)?,
            sync: RdsSync::default(),
            state: RdsState::default(),
        })
    }

    /// The next block of baseband; the events it completed.
    pub fn feed(&mut self, iq: &[Complex<f32>]) -> Vec<RdsEvent> {
        let composite = nbfm_demod(iq, self.rate_hz, DEVIATION_HZ, &mut self.prev);
        let bits = self.demod.feed(&composite);
        let state = &mut self.state;
        self.sync
            .feed(&bits)
            .into_iter()
            .filter_map(|g| state.apply(g))
            .collect()
    }
}

/// Decode RDS from complex baseband at `rate_hz` (a broadcast FM channel,
/// carrier at centre) with an [`RdsPipeline`]; the events in order. Empty
/// when the rate is too low for the RDS band.
pub fn decode_rds_iq(iq: &[Complex<f32>], rate_hz: u64) -> Vec<RdsEvent> {
    match RdsPipeline::new(rate_hz) {
        Ok(mut p) => p.feed(iq),
        Err(_) => Vec::new(),
    }
}

/// `rds_decode` takes no parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct RdsParams {}

/// Reads RDS from a broadcast FM channel's IQ (at least 118.8 kHz wide; give
/// it the `wfm_demod` band), one Event per [`RdsEvent`]. The input sample
/// rate is fixed by the first frame: a later frame at another rate fails the
/// stage, since the demodulator's filters and loops are built for one rate.
pub struct RdsDecodeStage {
    spec: StageSpec,
}

impl RdsDecodeStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "rds_decode",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(RdsParams),
            },
        }
    }
}

impl Default for RdsDecodeStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for RdsDecodeStage {
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
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        let _: RdsParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("rds_decode params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let mut pipeline: Option<RdsPipeline> = None;
        run_map(self.spec().kind, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    pipeline = None;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed("rds_decode expects iq frames".into()));
            };
            let rate = block.sample_rate_hz.0;
            let p = match &mut pipeline {
                Some(p) if p.rate_hz != rate => {
                    return Err(StageError::Failed(format!(
                        "sample_rate_hz changed from {} to {rate}",
                        p.rate_hz
                    )));
                }
                Some(p) => p,
                None => pipeline.insert(RdsPipeline::new(rate).map_err(StageError::Failed)?),
            };
            Ok(p.feed(&block.samples)
                .into_iter()
                .map(|e| {
                    let mut v = serde_json::to_value(&e).expect("RdsEvent serialises");
                    v["protocol"] = "rds".into();
                    Frame::new(out.clone(), frame.provenance.clone(), Payload::Event(v))
                })
                .collect())
        })
        .await
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::fixtures::{awgn, rds_group_bits, rds_station_iq};

    const PI_CODE: u16 = 0x2AB1;
    const PS: &str = "ZETA FM ";
    const RT: &str = "HELLO FROM ZETASDR";

    fn block(data: u16, o: Offset) -> u32 {
        ((data as u32) << 10) | checkword(data, o) as u32
    }

    /// The events carry the fixture's PS, and later an RT starting with its
    /// text, all under PI 2AB1.
    fn assert_station(events: &[RdsEvent]) {
        assert!(!events.is_empty(), "no events");
        assert!(events.iter().all(|e| e.pi == "2AB1"), "{events:?}");
        let ps_at = events
            .iter()
            .position(|e| e.ps.as_deref() == Some(PS))
            .unwrap_or_else(|| panic!("no PS event: {events:?}"));
        assert!(
            events[ps_at..]
                .iter()
                .any(|e| e.rt.as_deref().is_some_and(|t| t.starts_with(RT))),
            "no RT event after PS: {events:?}"
        );
        let e = &events[ps_at];
        assert_eq!((e.pty, e.tp), (10, true));
    }

    #[test]
    fn checkword_known_vector() {
        let b = block(0x1234, Offset::A);
        assert_eq!(b >> 10, 0x1234);
        assert_eq!(block_offset(b), Some(Offset::A));
        for i in 0..26 {
            assert_eq!(block_offset(b ^ (1 << i)), None, "bit {i}");
        }
        for o in [Offset::A, Offset::B, Offset::C, Offset::Cp, Offset::D] {
            for data in [0x0000, 0x1234, 0xFFFF, 0xA5C3] {
                assert_eq!(block_offset(block(data, o)), Some(o), "{o:?} {data:04x}");
            }
        }
        // Data 0 has a zero CRC, so the check word is the offset word itself.
        assert_eq!(checkword(0, Offset::A), 0x0FC);
        assert_eq!(checkword(0, Offset::D), 0x1B4);
    }

    #[test]
    fn checkword_nonzero_vector() {
        // Computed independently by long division, generator 0x5B9.
        let cases = [
            (0x1234, [0x06A, 0x10E, 0x1FE, 0x3C6, 0x122]),
            (0x2AB1, [0x11D, 0x079, 0x089, 0x2B1, 0x055]),
        ];
        for (data, want) in cases {
            for (o, w) in [Offset::A, Offset::B, Offset::C, Offset::Cp, Offset::D]
                .into_iter()
                .zip(want)
            {
                assert_eq!(checkword(data, o), w, "{o:?} {data:04x}");
            }
        }
    }

    #[test]
    fn sync_and_state_decode_fixture_bits() {
        // Undo the differential encoding, then the bit-level path alone.
        let tx = rds_group_bits(PI_CODE, PS, RT);
        assert_eq!(tx.len(), 3 * 20 * 104);
        let mut prev = 0;
        let bits: Vec<u8> = tx
            .iter()
            .map(|&t| {
                let b = t ^ prev;
                prev = t;
                b
            })
            .collect();
        let groups = RdsSync::default().feed(&bits);
        assert_eq!(groups.len(), 60);
        let mut st = RdsState::default();
        let events: Vec<RdsEvent> = groups.iter().filter_map(|&g| st.apply(g)).collect();
        assert_station(&events);
        assert_eq!(events.last().unwrap().group, "2A");
    }

    #[test]
    fn roundtrip_ps_and_rt_at_240k() {
        let iq = rds_station_iq(PI_CODE, PS, RT, 240_000);
        assert_station(&decode_rds_iq(&iq, 240_000));
    }

    #[test]
    fn roundtrip_at_an_odd_channel_rate() {
        let iq = rds_station_iq(PI_CODE, PS, RT, 200_781);
        assert_station(&decode_rds_iq(&iq, 200_781));
    }

    #[test]
    fn inverted_polarity_decodes() {
        let iq: Vec<Complex<f32>> = rds_station_iq(PI_CODE, PS, RT, 240_000)
            .iter()
            .map(|c| c.conj())
            .collect();
        assert_station(&decode_rds_iq(&iq, 240_000));
    }

    #[test]
    fn noisy_fixture_decodes() {
        let mut iq = rds_station_iq(PI_CODE, PS, RT, 240_000);
        awgn(&mut iq, 20.0, 5);
        assert_station(&decode_rds_iq(&iq, 240_000));
    }

    #[test]
    fn chunked_feed_matches_one_call() {
        let iq = rds_station_iq(PI_CODE, PS, RT, 200_781);
        let mut prev = Complex::new(0.0, 0.0);
        let comp = nbfm_demod(&iq, 200_781, DEVIATION_HZ, &mut prev);
        let whole = RdsDemod::new(200_781).unwrap().feed(&comp);
        let mut d = RdsDemod::new(200_781).unwrap();
        let chunked: Vec<u8> = comp.chunks(4093).flat_map(|c| d.feed(c)).collect();
        assert_eq!(chunked, whole);
        // All but the first group (acquisition) and the last (cut short).
        assert!(RdsSync::default().feed(&whole).len() >= 58);
    }

    #[test]
    fn noise_alone_yields_nothing() {
        let mut noise = vec![Complex::new(0.0f32, 0.0); 480_000];
        awgn(&mut noise, 0.0, 11);
        assert_eq!(decode_rds_iq(&noise, 240_000), vec![]);
    }

    fn ps_group(seg: u16, chars: &[u8; 2]) -> [u16; 4] {
        let b = (10 << 5) | (1 << 10) | seg;
        [PI_CODE, b, 0xE0E0, u16::from_be_bytes(*chars)]
    }

    #[test]
    fn ps_needs_all_four_segments() {
        let mut st = RdsState::default();
        // The first group gives a PI, which is an event on its own.
        let first = st.apply(ps_group(0, b"ZE")).expect("PI event");
        assert_eq!((first.pi.as_str(), first.ps.as_deref()), ("2AB1", None));
        assert_eq!(first.group, "0A");
        assert_eq!(st.apply(ps_group(1, b"TA")), None);
        assert_eq!(st.apply(ps_group(2, b" F")), None);
        assert_eq!(st.apply(ps_group(1, b"TA")), None);
        let e = st.apply(ps_group(3, b"M ")).expect("PS complete");
        assert_eq!(e.ps.as_deref(), Some("ZETA FM "));
        // Repeating a segment unchanged is silent; changing one is an event.
        assert_eq!(st.apply(ps_group(3, b"M ")), None);
        let e = st.apply(ps_group(3, b"X\x01")).expect("PS changed");
        assert_eq!(e.ps.as_deref(), Some("ZETA FX?"));
    }

    fn rt_group(ab: bool, seg: u16, chars: &[u8; 4]) -> [u16; 4] {
        let b = (2 << 12) | (u16::from(ab) << 4) | seg;
        [
            PI_CODE,
            b,
            u16::from_be_bytes([chars[0], chars[1]]),
            u16::from_be_bytes([chars[2], chars[3]]),
        ]
    }

    #[test]
    fn rt_terminator_and_ab_flag() {
        let mut st = RdsState::default();
        assert!(st.apply(rt_group(false, 0, b"HI T")).is_some()); // PI
        let e = st.apply(rt_group(false, 1, b"HERE")).map(|e| e.rt);
        assert_eq!(e, None);
        let e = st.apply(rt_group(false, 2, b"  \r ")).expect("terminated");
        assert_eq!(e.rt.as_deref(), Some("HI THERE"));
        assert_eq!(e.group, "2A");
        // A/B flip clears the text: segment 1 alone is not a complete text.
        assert_eq!(st.apply(rt_group(true, 1, b"AB\r ")), None);
        let e = st.apply(rt_group(true, 0, b"NEW ")).expect("new text");
        assert_eq!(e.rt.as_deref(), Some("NEW AB"));
    }

    #[test]
    fn pi_change_resets_and_reports() {
        let mut st = RdsState::default();
        for seg in 0..4 {
            st.apply(ps_group(seg, b"AB"));
        }
        let mut g = ps_group(0, b"CD");
        g[0] = 0xC0DE;
        let e = st.apply(g).expect("PI change");
        assert_eq!((e.pi.as_str(), e.ps), ("C0DE", None));
    }
}
