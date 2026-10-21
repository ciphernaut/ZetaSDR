//! POCSAG pager decoding: BCH(31,21) check, a streaming bit-level decoder,
//! a bit synchroniser for the NRZ discriminator output, and a one-call
//! decoder from complex baseband that tries every standard baud rate.
//!
//! A POCSAG codeword is 32 bits sent MSB first: bit 31 is the flag (0 =
//! address, 1 = message), bits 30..11 carry 20 data bits (for an address,
//! 18 address bits then 2 function bits), bits 10..1 are the BCH check bits
//! and bit 0 makes the word even parity. A batch is [`SYNC`] followed by 8
//! frames of 2 codewords; a pager's address codeword sits in frame
//! `address & 7`, which supplies the address's low 3 bits.

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
use crate::{Input, run_map};

/// Frame synchronisation codeword, starting every batch.
pub const SYNC: u32 = 0x7CD2_15D8;
/// Idle codeword, filling frames with nothing to send.
pub const IDLE: u32 = 0x7A89_C197;
/// The standard POCSAG baud rates.
pub const BAUDS: [u32; 3] = [512, 1200, 2400];
/// Nominal FSK deviation of a POCSAG transmitter.
pub const DEVIATION_HZ: u64 = 4_500;

/// BCH(31,21) generator `x^10+x^9+x^8+x^6+x^5+x^3+1`.
const BCH_POLY: u32 = 0x769;
/// Batches carry 16 codewords after the sync word.
const WORDS_PER_BATCH: u32 = 16;
/// Numeric pager character set, indexed by the (bit-reversed) BCD nibble.
const NUMERIC: &[u8; 16] = b"0123456789*U -)(";

/// Remainder of the 31-bit word `w` divided by [`BCH_POLY`].
fn bch_remainder(mut w: u32) -> u32 {
    for i in (10..31).rev() {
        if w & (1 << i) != 0 {
            w ^= BCH_POLY << (i - 10);
        }
    }
    w
}

/// True when `codeword`'s bits 31..1 are a BCH(31,21) codeword (zero
/// remainder) and bit 0 gives the 32 bits even parity. No error correction.
pub fn bch_ok(codeword: u32) -> bool {
    bch_remainder(codeword >> 1) == 0 && codeword.count_ones().is_multiple_of(2)
}

/// What a message carries, by its function bits and content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PocsagKind {
    Numeric,
    Alpha,
    Tone,
}

/// One decoded page.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PocsagMessage {
    /// 21-bit pager address (capcode).
    pub address: u32,
    /// Function bits 0..=3 of the address codeword.
    pub function: u8,
    /// Baud rate of the decoder that produced it.
    pub baud: u32,
    pub kind: PocsagKind,
    /// Decoded text; empty for [`PocsagKind::Tone`].
    pub text: String,
}

/// A message being collected: its address, function and data bits so far.
struct Pending {
    address: u32,
    function: u8,
    bits: Vec<u8>,
}

/// Streaming POCSAG decoder over NRZ bits; state carries across
/// [`feed`](Self::feed) calls.
///
/// Hunts for [`SYNC`] or its complement (after which every codeword is
/// complemented before use), then reads 16 codewords and expects the next
/// word to be a sync word again, or drops back to hunting. Address codewords
/// open a message; message codewords add 20 data bits; the next address
/// codeword, an idle codeword or loss of sync emits it. A codeword failing
/// [`bch_ok`] ends the current message (emitted only if it has data) and is
/// skipped; two failures in a row drop sync.
pub struct PocsagDecoder {
    baud: u32,
    /// Last 32 bits seen while hunting.
    shift: u32,
    synced: bool,
    inverted: bool,
    /// Bits of the codeword being read and how many.
    word: u32,
    nbits: u32,
    /// Codewords read since the last sync word (16 means the next is sync).
    index: u32,
    bch_failures: u32,
    pending: Option<Pending>,
}

impl PocsagDecoder {
    /// Decoder whose messages report `baud`.
    pub fn new(baud: u32) -> Self {
        Self {
            baud,
            shift: 0,
            synced: false,
            inverted: false,
            word: 0,
            nbits: 0,
            index: 0,
            bch_failures: 0,
            pending: None,
        }
    }

    /// Feed NRZ bits (0 or 1); returns the messages completed by them.
    pub fn feed(&mut self, bits: &[u8]) -> Vec<PocsagMessage> {
        let mut out = Vec::new();
        for &b in bits {
            let b = u32::from(b != 0);
            if !self.synced {
                self.shift = (self.shift << 1) | b;
                if self.shift == SYNC || self.shift == !SYNC {
                    self.synced = true;
                    self.inverted = self.shift != SYNC;
                    self.start_batch();
                }
                continue;
            }
            self.word = (self.word << 1) | b;
            self.nbits += 1;
            if self.nbits == 32 {
                let cw = if self.inverted { !self.word } else { self.word };
                self.word = 0;
                self.nbits = 0;
                self.codeword(cw, &mut out);
            }
        }
        out
    }

    fn start_batch(&mut self) {
        self.word = 0;
        self.nbits = 0;
        self.index = 0;
    }

    fn lose_sync(&mut self, out: &mut Vec<PocsagMessage>) {
        self.finish(true, out);
        self.synced = false;
        self.shift = 0;
        self.bch_failures = 0;
    }

    fn codeword(&mut self, cw: u32, out: &mut Vec<PocsagMessage>) {
        if self.index == WORDS_PER_BATCH {
            if cw == SYNC {
                self.start_batch();
            } else {
                self.lose_sync(out);
            }
            return;
        }
        let frame = self.index / 2;
        self.index += 1;
        if !bch_ok(cw) {
            self.finish(false, out);
            self.bch_failures += 1;
            if self.bch_failures >= 2 {
                self.lose_sync(out);
            }
            return;
        }
        self.bch_failures = 0;
        if cw == IDLE {
            self.finish(true, out);
        } else if cw & 0x8000_0000 == 0 {
            self.finish(true, out);
            self.pending = Some(Pending {
                address: (((cw >> 13) & 0x3FFFF) << 3) | frame,
                function: ((cw >> 11) & 3) as u8,
                bits: Vec::new(),
            });
        } else if let Some(p) = self.pending.as_mut() {
            let data = (cw >> 11) & 0xF_FFFF;
            p.bits
                .extend((0..20).rev().map(|i| ((data >> i) & 1) as u8));
        }
    }

    /// Emit the pending message, if any; one without data only when
    /// `tone_ok` (it ended cleanly rather than on a BCH failure).
    fn finish(&mut self, tone_ok: bool, out: &mut Vec<PocsagMessage>) {
        let Some(p) = self.pending.take() else {
            return;
        };
        let (kind, text) = if p.bits.is_empty() {
            if !tone_ok {
                return;
            }
            (PocsagKind::Tone, String::new())
        } else if p.function == 0 {
            (PocsagKind::Numeric, numeric_text(&p.bits))
        } else {
            (PocsagKind::Alpha, alpha_text(&p.bits))
        };
        out.push(PocsagMessage {
            address: p.address,
            function: p.function,
            baud: self.baud,
            kind,
            text,
        });
    }
}

/// 4-bit nibbles, each sent LSB first, through [`NUMERIC`]; trailing spaces
/// (the fill of the last codeword) are trimmed.
fn numeric_text(bits: &[u8]) -> String {
    let s: String = bits
        .chunks_exact(4)
        .map(|c| {
            let v = c.iter().rev().fold(0usize, |a, &b| (a << 1) | b as usize);
            NUMERIC[v] as char
        })
        .collect();
    s.trim_end_matches(' ').to_string()
}

/// 7-bit ASCII, each character sent LSB first; control characters other
/// than `\n` (including the EOT / NUL fill) are dropped.
fn alpha_text(bits: &[u8]) -> String {
    bits.chunks_exact(7)
        .map(|c| c.iter().rev().fold(0u8, |a, &b| (a << 1) | b) as char)
        .filter(|&c| c == '\n' || !c.is_ascii_control())
        .collect()
}

/// Bit synchroniser for NRZ discriminator output; state carries across
/// [`feed`](Self::feed) calls.
///
/// A one-pole 1 Hz high-pass removes DC (carrier offset), then a moving
/// average over half a bit smooths noise. A bit clock (phase in bit periods,
/// advancing `baud / rate` per sample) samples the sign of the smoothed
/// signal each time it passes mid-bit. At each zero crossing (located to a
/// fraction of a sample) the clock is nudged toward phase 0 by at most 1/8 of
/// a bit, never past it, which tracks a transmitter several percent off its
/// nominal rate.
pub struct BitSync {
    /// High-pass coefficient and state (previous input and output).
    hp_a: f32,
    hp_x: f32,
    hp_y: f32,
    /// Moving-average ring and running sum.
    ring: Vec<f32>,
    pos: usize,
    sum: f32,
    /// Last smoothed sample.
    last: f32,
    phase: f64,
    step: f64,
    /// Whether the current bit period has been sampled, so a backward nudge
    /// past mid-bit cannot sample it twice.
    sampled: bool,
}

/// Largest clock correction per zero crossing, in bit periods.
const MAX_NUDGE: f64 = 0.125;

impl BitSync {
    /// Synchroniser for `baud` NRZ at `rate_hz` samples per second.
    pub fn new(rate_hz: u64, baud: u32) -> Self {
        let rate = rate_hz.max(1) as f64;
        let spb = rate / baud.max(1) as f64;
        let len = ((spb / 2.0).round() as usize).max(1);
        Self {
            hp_a: (-2.0 * std::f64::consts::PI / rate).exp() as f32,
            hp_x: 0.0,
            hp_y: 0.0,
            ring: vec![0.0; len],
            pos: 0,
            sum: 0.0,
            last: 0.0,
            phase: 0.0,
            step: 1.0 / spb,
            sampled: false,
        }
    }

    /// Feed discriminator samples; returns the bits (0 or 1, positive = 1)
    /// sampled from them.
    pub fn feed(&mut self, nrz: &[f32]) -> Vec<u8> {
        let mut out = Vec::with_capacity((nrz.len() as f64 * self.step) as usize + 1);
        let n = self.ring.len();
        for &x in nrz {
            let y = self.hp_a * (self.hp_y + x - self.hp_x);
            self.hp_x = x;
            self.hp_y = y;
            self.sum += y - self.ring[self.pos];
            self.ring[self.pos] = y;
            self.pos = (self.pos + 1) % n;
            let s = self.sum / n as f32;

            self.phase += self.step;
            if !self.sampled && self.phase >= 0.5 {
                out.push(u8::from(s > 0.0));
                self.sampled = true;
            }
            if (self.last > 0.0) != (s > 0.0) && self.last != s {
                // Crossing at fraction t between the previous sample and this one.
                let t = (self.last / (self.last - s)).clamp(0.0, 1.0) as f64;
                let at = (self.phase - (1.0 - t) * self.step).rem_euclid(1.0);
                let err = if at < 0.5 { at } else { at - 1.0 };
                self.phase -= err.clamp(-MAX_NUDGE, MAX_NUDGE);
            }
            if self.phase >= 1.0 {
                self.phase -= 1.0;
                self.sampled = false;
            } else if self.phase < 0.0 {
                // Back into the previous bit, which was already sampled.
                self.phase += 1.0;
                self.sampled = true;
            }
            self.last = s;
        }
        out
    }
}

/// Decode POCSAG from complex baseband at `rate_hz`: discriminate at
/// [`DEVIATION_HZ`], then run a [`BitSync`] and [`PocsagDecoder`] for each of
/// [`BAUDS`]; the messages of all three, in that order.
pub fn decode_pocsag_iq(iq: &[Complex<f32>], rate_hz: u64) -> Vec<PocsagMessage> {
    let mut prev = Complex::new(0.0, 0.0);
    let audio = nbfm_demod(iq, rate_hz, DEVIATION_HZ, &mut prev);
    BAUDS
        .iter()
        .flat_map(|&baud| {
            let bits = BitSync::new(rate_hz, baud).feed(&audio);
            PocsagDecoder::new(baud).feed(&bits)
        })
        .collect()
}

/// `pocsag_decode` takes no parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PocsagParams {}

/// Decodes POCSAG pager messages from a channel's IQ into one Event per
/// BCH-clean message, at 512, 1200 and 2400 baud at once.
pub struct PocsagDecodeStage {
    spec: StageSpec,
}

impl PocsagDecodeStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "pocsag_decode",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(PocsagParams),
            },
        }
    }
}

impl Default for PocsagDecodeStage {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-run decoder state, built from the first frame's rate.
struct StageState {
    rate_hz: u64,
    prev: Complex<f32>,
    lanes: Vec<(BitSync, PocsagDecoder)>,
}

#[async_trait]
impl Stage for PocsagDecodeStage {
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
        let _: PocsagParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("pocsag_decode params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let mut state: Option<StageState> = None;
        run_map(self.spec().kind, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    state = None;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed("pocsag_decode expects iq frames".into()));
            };
            let rate = block.sample_rate_hz.0;
            let st = match &mut state {
                Some(s) if s.rate_hz != rate => {
                    return Err(StageError::Failed(format!(
                        "sample_rate_hz changed from {} to {rate}",
                        s.rate_hz
                    )));
                }
                Some(s) => s,
                None => state.insert(StageState {
                    rate_hz: rate,
                    prev: Complex::new(0.0, 0.0),
                    lanes: BAUDS
                        .iter()
                        .map(|&b| (BitSync::new(rate, b), PocsagDecoder::new(b)))
                        .collect(),
                }),
            };
            let audio = nbfm_demod(&block.samples, rate, DEVIATION_HZ, &mut st.prev);
            let mut events = Vec::new();
            for (sync, dec) in &mut st.lanes {
                let bits = sync.feed(&audio);
                for m in dec.feed(&bits) {
                    events.push(Frame::new(
                        out.clone(),
                        frame.provenance.clone(),
                        Payload::Event(serde_json::json!({
                            "protocol": "pocsag",
                            "address": m.address,
                            "function": m.function,
                            "baud": m.baud,
                            "kind": m.kind,
                            "text": m.text,
                        })),
                    ));
                }
            }
            Ok(events)
        })
        .await
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::fixtures::{
        awgn, pocsag_encode, pocsag_modulate, pocsag_packet_iq, resample_linear,
    };
    use num_complex::Complex;

    fn alpha(address: u32, baud: u32, text: &str) -> PocsagMessage {
        PocsagMessage {
            address,
            function: 3,
            baud,
            kind: PocsagKind::Alpha,
            text: text.into(),
        }
    }

    #[test]
    fn sync_and_idle_pass_bch() {
        assert!(bch_ok(SYNC));
        assert!(bch_ok(IDLE));
        assert!(!bch_ok(SYNC ^ 0x40));
        assert!(!bch_ok(SYNC ^ 1));
    }

    #[test]
    fn encoder_output_is_bch_clean() {
        let bits = pocsag_encode(&[(1234567, 3, "HELLO")], 1200);
        let words = &bits[576..];
        assert_eq!(words.len() % 32, 0);
        assert!(words.len() >= 17 * 32);
        for (i, w) in words.chunks(32).enumerate() {
            let cw = w.iter().fold(0u32, |a, &b| (a << 1) | b as u32);
            assert!(bch_ok(cw), "word {i}: {cw:08x}");
        }
        let first = words[..32].iter().fold(0u32, |a, &b| (a << 1) | b as u32);
        assert_eq!(first, SYNC);
    }

    #[test]
    fn roundtrip_alpha_and_numeric_at_each_baud() {
        for baud in BAUDS {
            let iq = pocsag_packet_iq(1234567, 3, "HELLO ZETA", baud, 48_000, 0.0);
            let m = decode_pocsag_iq(&iq, 48_000);
            assert_eq!(m, vec![alpha(1234567, baud, "HELLO ZETA")], "{baud}");
            let iq = pocsag_packet_iq(42, 0, "0123-456", baud, 48_000, 0.0);
            let m = decode_pocsag_iq(&iq, 48_000);
            assert_eq!(m.len(), 1, "{baud}: {m:?}");
            assert_eq!(m[0].text, "0123-456", "{baud}");
            assert_eq!(m[0].kind, PocsagKind::Numeric);
            assert_eq!((m[0].address, m[0].function, m[0].baud), (42, 0, baud));
        }
    }

    #[test]
    fn roundtrip_at_an_odd_channel_rate() {
        let iq = pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, 53_333, 0.0);
        assert_eq!(
            decode_pocsag_iq(&iq, 53_333),
            vec![alpha(1234567, 1200, "HELLO ZETA")]
        );
    }

    #[test]
    fn inverted_polarity_decodes() {
        let iq: Vec<Complex<f32>> = pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, 48_000, 0.0)
            .iter()
            .map(|c| c.conj())
            .collect();
        assert_eq!(
            decode_pocsag_iq(&iq, 48_000),
            vec![alpha(1234567, 1200, "HELLO ZETA")]
        );
    }

    #[test]
    fn noisy_fixture_decodes_and_noise_alone_does_not() {
        let mut iq = pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, 48_000, 0.0);
        awgn(&mut iq, 15.0, 7);
        assert_eq!(
            decode_pocsag_iq(&iq, 48_000),
            vec![alpha(1234567, 1200, "HELLO ZETA")]
        );
        let mut noise = vec![Complex::new(0.0f32, 0.0); 96_000];
        awgn(&mut noise, 0.0, 7);
        assert_eq!(decode_pocsag_iq(&noise, 48_000), vec![]);
    }

    #[test]
    fn two_messages_in_one_batch() {
        let bits = pocsag_encode(&[(8, 3, "AB"), (9, 3, "CD")], 1200);
        // Preamble, then exactly one batch: both messages fit in frames 0 and 1.
        assert_eq!(bits.len(), 576 + 17 * 32);
        let mut dec = PocsagDecoder::new(1200);
        assert_eq!(
            dec.feed(&bits),
            vec![alpha(8, 1200, "AB"), alpha(9, 1200, "CD")]
        );
    }

    #[test]
    fn bit_sync_tracks_one_percent_rate_error() {
        let bits = pocsag_encode(&[(1234567, 3, "HELLO ZETA")], 1200);
        // 40 samples a bit read as 48 kHz audio from 48 480 / 47 520 Hz:
        // 1188 and 1212 baud against a 1200-baud clock.
        for written_at in [48_480, 47_520] {
            let mut audio = vec![0.0f32; 4_800];
            audio.extend(pocsag_modulate(&bits, 1200, written_at));
            audio.extend(vec![0.0f32; 4_800]);
            let got = BitSync::new(48_000, 1200).feed(&audio);
            assert_eq!(
                PocsagDecoder::new(1200).feed(&got),
                vec![alpha(1234567, 1200, "HELLO ZETA")],
                "{written_at}"
            );
        }
    }

    #[test]
    fn multimon_ng_decodes_our_fixture() {
        let found = std::process::Command::new("which")
            .arg("multimon-ng")
            .output()
            .is_ok_and(|o| o.status.success());
        if !found {
            println!("skipped: no multimon-ng");
            return;
        }
        // multimon-ng reads 22 050 Hz, where 1200 baud is 18.375 samples a
        // bit; rounding to 18 (1225 baud, 2 % fast) is more than it tracks.
        // Modulate at 48 kHz (exactly 40 a bit) and resample instead.
        let bits = pocsag_encode(&[(1234567, 3, "HELLO ZETA")], 1200);
        let mut audio = vec![0.0f32; 12_000];
        audio.extend(pocsag_modulate(&bits, 1200, 48_000));
        audio.extend(vec![0.0f32; 12_000]);
        let audio = resample_linear(&audio, 48_000, 22_050);
        let raw: Vec<u8> = audio
            .iter()
            .flat_map(|&a| ((a * 32_767.0) as i16).to_le_bytes())
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pocsag1200.raw");
        std::fs::write(&path, raw).unwrap();
        let out = std::process::Command::new("multimon-ng")
            .args(["-t", "raw", "-a", "POCSAG1200", "-q"])
            .arg(&path)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("Alpha:"), "{stdout}");
        assert!(stdout.contains("HELLO ZETA"), "{stdout}");
        assert!(stdout.contains("1234567"), "{stdout}");
    }
}
