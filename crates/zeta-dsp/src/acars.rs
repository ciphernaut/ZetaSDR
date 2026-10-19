//! ACARS (ARINC 618) decoding: 2400 bps MSK audio on an AM voice channel.
//!
//! The tones are 1200 Hz (half a cycle per bit) and 2400 Hz (a full
//! cycle), phase-continuous. As acarsdec (the reference decoder) reads it,
//! the tone carries the change between successive bits: [`MARK_HZ`] sends a
//! bit equal to the one before it, [`SPACE_HZ`] the inverse, so the pre-key
//! of ones is a steady 2400 Hz tone. Characters are 7-bit ASCII sent LSB
//! first with an odd-parity bit 7. A block is pre-key, `+` `*` SYN SYN SOH,
//! mode, address (7), technical acknowledgement, label (2), block id, STX,
//! text, ETX or ETB, two CRC bytes, DEL.

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

use crate::am::{DcBlock, am_demod};
use crate::{Input, run_map};

/// Bit rate.
pub const BAUD: u32 = 2_400;
/// Mark tone, one cycle per bit: the bit equals the previous bit.
pub const MARK_HZ: f64 = 2_400.0;
/// Space tone, half a cycle per bit: the bit inverts the previous bit.
pub const SPACE_HZ: f64 = 1_200.0;
/// Lowest audio rate [`MskDemod`] accepts.
pub const MIN_RATE_HZ: u64 = 9_600;

pub const SOH: u8 = 0x01;
pub const STX: u8 = 0x02;
pub const ETX: u8 = 0x03;
pub const NAK: u8 = 0x15;
pub const SYN: u8 = 0x16;
pub const ETB: u8 = 0x17;
pub const DEL: u8 = 0x7F;

/// Longest text of one block.
const MAX_TEXT: usize = 220;
/// Mode, address, acknowledgement, label, block id, STX/ETX.
const HEADER_LEN: usize = 13;
/// SYN SYN SOH as received (LSB-first characters, oldest in the low byte).
const SYNC_WORD: u32 = (SOH as u32) << 16 | (SYN as u32) << 8 | SYN as u32;
/// Clock correction per tone transition, as a fraction of the timing error.
const TIMING_GAIN: f64 = 0.5;

/// Tone of each bit of `bits`, the bit before the first taken as 1.
pub fn bit_tones(bits: &[u8]) -> Vec<f64> {
    let mut prev = 1u8;
    bits.iter()
        .map(|&b| {
            let b = u8::from(b != 0);
            let t = if b == prev { MARK_HZ } else { SPACE_HZ };
            prev = b;
            t
        })
        .collect()
}

/// CRC-16 as acarsdec checks it: CCITT polynomial `0x1021` processed
/// LSB first (reflected form `0x8408`), init `0x0000`, no final XOR
/// (CRC-16/KERMIT). Sent low byte first, so the CRC of a block followed by
/// its CRC bytes is zero.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in bytes {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// One CRC-clean ACARS block.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AcarsBlock {
    pub mode: char,
    /// Aircraft address with its leading dots stripped.
    pub reg: String,
    /// Technical acknowledgement; `None` for NAK.
    pub ack: Option<char>,
    pub label: String,
    /// `None` for a NUL block id (a squitter).
    pub block_id: Option<char>,
    /// Downlink (digit block id) message number, the text's first 4 chars.
    pub msg_no: Option<String>,
    /// Downlink flight id, the text's next 6 chars.
    pub flight: Option<String>,
    pub text: String,
}

/// Streaming MSK demodulator for ACARS audio; state carries across
/// [`feed`](Self::feed) calls.
///
/// Two references at [`MARK_HZ`] and [`SPACE_HZ`] run phase-continuously
/// and the audio is correlated against each over a sliding window of one
/// bit; `d = (|S_space|² − |S_mark|²) / (|S_space|² + |S_mark|²)` is the
/// tone decision. A bit clock samples `d` when the window lines up with a
/// bit and is pulled toward each zero crossing of `d` (a tone change puts
/// the window half over each bit there); the steady mark of the pre-key
/// leaves it free-running until the first space. Each sampled tone is turned
/// into a bit by the mark-repeats / space-inverts rule.
pub struct MskDemod {
    step_mark: f64,
    step_space: f64,
    ph_mark: f64,
    ph_space: f64,
    ring: Vec<(Complex<f64>, Complex<f64>)>,
    pos: usize,
    sum_mark: Complex<f64>,
    sum_space: Complex<f64>,
    last_d: f64,
    /// Bit clock: 0 when the window ends mid-bit, 0.5 when it covers a bit.
    phase: f64,
    step: f64,
    sampled: bool,
    bit: u8,
}

impl MskDemod {
    /// Demodulator for audio at `rate_hz`, at least [`MIN_RATE_HZ`].
    pub fn new(rate_hz: u64) -> Result<Self, String> {
        if rate_hz < MIN_RATE_HZ {
            return Err(format!(
                "ACARS needs audio at {MIN_RATE_HZ} Hz or more, got {rate_hz}"
            ));
        }
        let rate = rate_hz as f64;
        let spb = rate / BAUD as f64;
        let len = (spb.round() as usize).max(1);
        Ok(Self {
            step_mark: 2.0 * std::f64::consts::PI * MARK_HZ / rate,
            step_space: 2.0 * std::f64::consts::PI * SPACE_HZ / rate,
            ph_mark: 0.0,
            ph_space: 0.0,
            ring: vec![(Complex::default(), Complex::default()); len],
            pos: 0,
            sum_mark: Complex::default(),
            sum_space: Complex::default(),
            last_d: 0.0,
            phase: 0.0,
            step: 1.0 / spb,
            sampled: false,
            bit: 1,
        })
    }

    /// Feed audio (DC removed); returns the bits (0 or 1) it completed, in
    /// the order sent (LSB first per character).
    pub fn feed(&mut self, audio: &[f32]) -> Vec<u8> {
        let mut out = Vec::with_capacity((audio.len() as f64 * self.step) as usize + 1);
        let tau = 2.0 * std::f64::consts::PI;
        for &x in audio {
            let x = f64::from(x);
            let m = Complex::from_polar(x, -self.ph_mark);
            let s = Complex::from_polar(x, -self.ph_space);
            self.ph_mark = (self.ph_mark + self.step_mark) % tau;
            self.ph_space = (self.ph_space + self.step_space) % tau;
            let (om, os) = self.ring[self.pos];
            self.ring[self.pos] = (m, s);
            self.pos = (self.pos + 1) % self.ring.len();
            self.sum_mark += m - om;
            self.sum_space += s - os;
            let (em, es) = (self.sum_mark.norm_sqr(), self.sum_space.norm_sqr());
            let d = if em + es > 0.0 {
                (es - em) / (es + em)
            } else {
                0.0
            };

            self.phase += self.step;
            if !self.sampled && self.phase >= 0.5 {
                let space = d > 0.0;
                self.bit ^= u8::from(space);
                out.push(self.bit);
                self.sampled = true;
            }
            if (self.last_d > 0.0) != (d > 0.0) && self.last_d != d {
                let t = (self.last_d / (self.last_d - d)).clamp(0.0, 1.0);
                let at = (self.phase - (1.0 - t) * self.step).rem_euclid(1.0);
                let err = if at < 0.5 { at } else { at - 1.0 };
                self.phase -= TIMING_GAIN * err;
            }
            if self.phase >= 1.0 {
                self.phase -= 1.0;
                self.sampled = false;
            } else if self.phase < 0.0 {
                self.phase += 1.0;
                self.sampled = true;
            }
            self.last_d = d;
        }
        out
    }
}

/// Where the framer is in a block.
enum FrameState {
    /// Looking for SYN SYN SOH (or its complement).
    Hunt,
    /// Reading mode..ETX/ETB.
    Body,
    /// Reading the two CRC bytes.
    Crc,
}

/// Streaming ACARS block framer over demodulated bits; state carries
/// across [`feed`](Self::feed) calls.
///
/// Hunts for SYN SYN SOH, or its complement (a demodulator that started on
/// the wrong bit sense; every later bit is then inverted), reads 8-bit
/// characters up to ETX or ETB, then two CRC bytes. A character with even
/// parity, or a body longer than a block can be, drops the block; a block is
/// returned only when [`crc16`] of its body matches.
pub struct AcarsFramer {
    state: FrameState,
    shift: u32,
    invert: bool,
    ch: u8,
    nbits: u8,
    body: Vec<u8>,
    crc: Vec<u8>,
}

impl Default for AcarsFramer {
    fn default() -> Self {
        Self::new()
    }
}

impl AcarsFramer {
    pub fn new() -> Self {
        Self {
            state: FrameState::Hunt,
            shift: 0,
            invert: false,
            ch: 0,
            nbits: 0,
            body: Vec::new(),
            crc: Vec::new(),
        }
    }

    /// Feed bits (0 or 1, LSB first per character); returns the blocks they
    /// completed.
    pub fn feed(&mut self, bits: &[u8]) -> Vec<AcarsBlock> {
        let mut out = Vec::new();
        for &b in bits {
            let b = u8::from(b != 0);
            if let FrameState::Hunt = self.state {
                self.shift = (self.shift >> 1) | (u32::from(b) << 23);
                if self.shift == SYNC_WORD || self.shift == !SYNC_WORD & 0xFF_FFFF {
                    self.invert = self.shift != SYNC_WORD;
                    self.state = FrameState::Body;
                    self.ch = 0;
                    self.nbits = 0;
                    self.body.clear();
                    self.crc.clear();
                }
                continue;
            }
            let b = b ^ u8::from(self.invert);
            self.ch = (self.ch >> 1) | (b << 7);
            self.nbits += 1;
            if self.nbits < 8 {
                continue;
            }
            self.nbits = 0;
            let c = self.ch;
            match self.state {
                FrameState::Body => {
                    if c.count_ones().is_multiple_of(2) || self.body.len() > HEADER_LEN + MAX_TEXT {
                        self.hunt();
                        continue;
                    }
                    self.body.push(c);
                    let c7 = c & 0x7F;
                    if (c7 == ETX || c7 == ETB) && self.body.len() >= HEADER_LEN {
                        self.state = FrameState::Crc;
                    }
                }
                FrameState::Crc => {
                    self.crc.push(c);
                    if self.crc.len() == 2 {
                        let sent = u16::from_le_bytes([self.crc[0], self.crc[1]]);
                        if crc16(&self.body) == sent
                            && let Some(blk) = parse_block(&self.body)
                        {
                            out.push(blk);
                        }
                        self.hunt();
                    }
                }
                FrameState::Hunt => unreachable!(),
            }
        }
        out
    }

    fn hunt(&mut self) {
        self.state = FrameState::Hunt;
        self.shift = 0;
    }
}

/// Fields of a CRC-clean body (mode..ETX/ETB, parity bits still set), as
/// acarsdec splits them; `None` when the start-of-text is neither STX nor
/// ETX or ETX/ETB comes early.
fn parse_block(body: &[u8]) -> Option<AcarsBlock> {
    let b: Vec<u8> = body.iter().map(|&c| c & 0x7F).collect();
    let (head, rest) = b.split_at(HEADER_LEN - 1);
    let text: &[u8] = match rest {
        [ETX] => &[],
        [STX, text @ .., end] if *end == ETX || *end == ETB => text,
        _ => return None,
    };
    if text.iter().any(|&c| c == ETX || c == ETB) {
        return None;
    }
    let s = |x: &[u8]| x.iter().map(|&c| c as char).collect::<String>();
    let mut label = s(&head[9..11]);
    if head[10] == DEL {
        label.replace_range(1.., "d");
    }
    let block_id = (head[11] != 0).then_some(head[11] as char);
    let (mut msg_no, mut flight, mut text) = (None, None, text);
    if block_id.is_some_and(|c| c.is_ascii_digit())
        && let Some((no, t)) = text.split_at_checked(4)
    {
        msg_no = Some(s(no));
        text = t;
        if let Some((fid, t)) = text.split_at_checked(6) {
            flight = Some(s(fid));
            text = t;
        }
    }
    Some(AcarsBlock {
        mode: head[0] as char,
        reg: s(&head[1..8]).trim_start_matches('.').to_string(),
        ack: (head[8] != NAK).then_some(head[8] as char),
        label,
        block_id,
        msg_no,
        flight,
        text: s(text),
    })
}

/// The whole ACARS chain on complex baseband: [`am_demod`], DC removal,
/// [`MskDemod`] and [`AcarsFramer`], with every piece's state carried
/// across [`feed`](Self::feed) calls.
pub struct AcarsPipeline {
    rate_hz: u64,
    dc: DcBlock,
    demod: MskDemod,
    framer: AcarsFramer,
}

impl AcarsPipeline {
    /// Pipeline for baseband at `rate_hz`; an error below [`MIN_RATE_HZ`].
    pub fn new(rate_hz: u64) -> Result<Self, String> {
        Ok(Self {
            rate_hz,
            dc: DcBlock::new(rate_hz),
            demod: MskDemod::new(rate_hz)?,
            framer: AcarsFramer::new(),
        })
    }

    /// The next block of baseband; the ACARS blocks it completed.
    pub fn feed(&mut self, iq: &[Complex<f32>]) -> Vec<AcarsBlock> {
        let mut audio = am_demod(iq);
        self.dc.process(&mut audio);
        let bits = self.demod.feed(&audio);
        self.framer.feed(&bits)
    }
}

/// Decode ACARS from complex baseband at `rate_hz` (an AM channel) with an
/// [`AcarsPipeline`]; the CRC-clean blocks in order. Empty when the rate is
/// below [`MIN_RATE_HZ`].
pub fn decode_acars_iq(iq: &[Complex<f32>], rate_hz: u64) -> Vec<AcarsBlock> {
    match AcarsPipeline::new(rate_hz) {
        Ok(mut p) => p.feed(iq),
        Err(_) => Vec::new(),
    }
}

/// `acars_decode` takes no parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct AcarsParams {}

/// Decodes ACARS from an AM channel's IQ (give it about 10 kHz), one Event
/// per CRC-clean [`AcarsBlock`]. The input sample rate is fixed by the first
/// frame: a later frame at another rate fails the stage, since the
/// demodulator is built for one rate.
pub struct AcarsDecodeStage {
    spec: StageSpec,
}

impl AcarsDecodeStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "acars_decode",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(AcarsParams),
            },
        }
    }
}

impl Default for AcarsDecodeStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for AcarsDecodeStage {
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
        let _: AcarsParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("acars_decode params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let mut pipeline: Option<AcarsPipeline> = None;
        run_map(inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    pipeline = None;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed("acars_decode expects iq frames".into()));
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
                None => pipeline.insert(AcarsPipeline::new(rate).map_err(StageError::Failed)?),
            };
            Ok(p.feed(&block.samples)
                .into_iter()
                .map(|b| {
                    let mut v = serde_json::to_value(&b).expect("AcarsBlock serialises");
                    v["protocol"] = "acars".into();
                    v["crc_ok"] = true.into();
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
    use crate::fixtures::{acars_block_bytes, acars_modulate, acars_packet_iq, am_modulate, awgn};

    const TEXT: &str = "#M1BPOS/ZETASDR TEST";

    fn expected() -> AcarsBlock {
        AcarsBlock {
            mode: '2',
            reg: "VH-ZET".into(),
            ack: None,
            label: "H1".into(),
            block_id: Some('A'),
            msg_no: None,
            flight: None,
            text: TEXT.into(),
        }
    }

    #[test]
    fn crc16_known_vector() {
        assert_eq!(crc16(b"123456789"), 0x2189);
    }

    #[test]
    fn roundtrip_block() {
        let iq = acars_packet_iq("VH-ZET", "H1", TEXT, 48_000, 0.0);
        assert_eq!(decode_acars_iq(&iq, 48_000), vec![expected()]);
    }

    #[test]
    fn roundtrip_at_an_odd_channel_rate() {
        let iq = acars_packet_iq("VH-ZET", "H1", TEXT, 53_333, 1_500.0);
        assert_eq!(decode_acars_iq(&iq, 53_333), vec![expected()]);
    }

    #[test]
    fn noisy_fixture_decodes_and_noise_alone_does_not() {
        let mut iq = acars_packet_iq("VH-ZET", "H1", TEXT, 48_000, 0.0);
        awgn(&mut iq, 15.0, 7);
        assert_eq!(decode_acars_iq(&iq, 48_000), vec![expected()]);
        let mut noise = vec![Complex::new(0.0f32, 0.0); 96_000];
        awgn(&mut noise, 0.0, 7);
        assert_eq!(decode_acars_iq(&noise, 48_000), vec![]);
    }

    #[test]
    fn pipeline_carries_state_across_feeds() {
        let iq = acars_packet_iq("VH-ZET", "H1", TEXT, 48_000, 0.0);
        let mut p = AcarsPipeline::new(48_000).unwrap();
        let got: Vec<AcarsBlock> = iq.chunks(1_001).flat_map(|c| p.feed(c)).collect();
        assert_eq!(got, vec![expected()]);
        assert!(AcarsPipeline::new(8_000).is_err());
    }

    #[test]
    fn corrupt_crc_is_dropped() {
        let mut bytes = acars_block_bytes('2', "VH-ZET", "H1", 'A', TEXT);
        let n = bytes.len();
        bytes[n - 2] ^= 0x10;
        let mut audio = vec![0.0f32; 9_600];
        audio.extend(acars_modulate(&bytes, 48_000));
        audio.extend(vec![0.0f32; 9_600]);
        let iq = am_modulate(&audio, 48_000, 48_000, 0.9, 0.0);
        assert_eq!(decode_acars_iq(&iq, 48_000), vec![]);
    }

    #[test]
    fn downlink_text_splits_msg_no_and_flight() {
        let bytes = acars_block_bytes('2', ".N123AB", "H1", '3', "M01AZT0042HELLO");
        let mut audio = vec![0.0f32; 9_600];
        audio.extend(acars_modulate(&bytes, 48_000));
        audio.extend(vec![0.0f32; 9_600]);
        let iq = am_modulate(&audio, 48_000, 48_000, 0.9, 0.0);
        let got = decode_acars_iq(&iq, 48_000);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].reg, "N123AB");
        assert_eq!(got[0].block_id, Some('3'));
        assert_eq!(got[0].msg_no.as_deref(), Some("M01A"));
        assert_eq!(got[0].flight.as_deref(), Some("ZT0042"));
        assert_eq!(got[0].text, "HELLO");
    }

    fn iq_of(blocks: &[Vec<u8>]) -> Vec<Complex<f32>> {
        let mut audio = vec![0.0f32; 9_600];
        for b in blocks {
            audio.extend(acars_modulate(b, 48_000));
            audio.extend(vec![0.0f32; 4_800]);
        }
        audio.extend(vec![0.0f32; 9_600]);
        am_modulate(&audio, 48_000, 48_000, 0.9, 0.0)
    }

    #[test]
    fn parity_error_drops_the_block() {
        let mut bytes = acars_block_bytes('2', "VH-ZET", "H1", 'A', TEXT);
        // Flip the parity bit of a text character.
        bytes[20] ^= 0x80;
        assert_eq!(decode_acars_iq(&iq_of(&[bytes]), 48_000), vec![]);
    }

    #[test]
    fn two_blocks_back_to_back_both_decode() {
        let a = acars_block_bytes('2', "VH-ZET", "H1", 'A', TEXT);
        let b = acars_block_bytes('2', "VH-ZEU", "Q0", 'B', "SECOND");
        let got = decode_acars_iq(&iq_of(&[a, b]), 48_000);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(
            (got[0].reg.as_str(), got[0].text.as_str()),
            ("VH-ZET", TEXT)
        );
        assert_eq!(
            (got[1].reg.as_str(), got[1].text.as_str()),
            ("VH-ZEU", "SECOND")
        );
    }

    #[test]
    fn framer_resyncs_after_garbage_bits() {
        let bytes = acars_block_bytes('2', "VH-ZET", "H1", 'A', TEXT);
        let mut bits: Vec<u8> = (0..1_000u32)
            .map(|i| ((i.wrapping_mul(2_654_435_761) >> 13) & 1) as u8)
            .collect();
        bits.extend(std::iter::repeat_n(1u8, 128));
        bits.extend(
            bytes
                .iter()
                .flat_map(|&b| (0..8).map(move |i| (b >> i) & 1)),
        );
        bits.extend([1u8; 64]);
        let mut framer = AcarsFramer::new();
        assert_eq!(framer.feed(&bits), vec![expected()]);
    }

    /// 44-byte canonical PCM WAV header: mono, s16le, `rate` Hz.
    fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend(b"RIFF");
        out.extend((36 + data_len).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes()); // PCM
        out.extend(1u16.to_le_bytes()); // mono
        out.extend(rate.to_le_bytes());
        out.extend((rate * 2).to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend(16u16.to_le_bytes());
        out.extend(b"data");
        out.extend(data_len.to_le_bytes());
        out.extend(
            samples
                .iter()
                .flat_map(|&s| ((s * 32_767.0) as i16).to_le_bytes()),
        );
        out
    }

    #[test]
    fn acarsdec_decodes_our_fixture() {
        let found = std::process::Command::new("which")
            .arg("acarsdec")
            .output()
            .is_ok_and(|o| o.status.success());
        if !found {
            println!("skipped: no acarsdec");
            return;
        }
        let bytes = acars_block_bytes('2', "VH-ZET", "H1", 'A', TEXT);
        let mut audio = vec![0.0f32; 9_600];
        audio.extend(acars_modulate(&bytes, 48_000));
        // acarsdec exits at end of file without waiting for its decoder
        // thread; 2 s of trailing silence lets the block reach the output
        // even on a loaded host (the test flaked under the full suite).
        audio.extend(vec![0.0f32; 2 * 48_000]);
        let dir = tempfile::tempdir().unwrap();
        let wav_path = dir.path().join("acars.wav");
        let out_path = dir.path().join("acars.txt");
        std::fs::write(&wav_path, wav(&audio, 48_000)).unwrap();
        let run = std::process::Command::new("acarsdec")
            .arg("--sndfile")
            .arg(&wav_path)
            .arg("--output")
            .arg(format!("oneline:file:path={}", out_path.display()))
            .output()
            .unwrap();
        let got = std::fs::read_to_string(&out_path).unwrap_or_default();
        assert!(
            got.contains("VH-ZET") && got.contains("H1") && got.contains(TEXT),
            "acarsdec output: {got:?}\nstderr: {}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
}
