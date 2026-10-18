//! Bell 202 AFSK1200 demodulator: audio samples in, NRZI-decoded data bits out.
//!
//! Each sample enters a ring buffer one bit period long. The window is
//! correlated against quadrature references at 1200 Hz (mark) and 2200 Hz
//! (space); the larger energy `I^2 + Q^2` gives a sample-level line level. A bit
//! clock (phase in `[0, 1)`, advancing `1200 / rate` per sample) samples that
//! level on the sample nearest each wrap. Every level transition pulls the
//! phase halfway toward `0.5 + step`: the decision lags the audio by half a
//! window, so a wrap half a bit after a transition is the instant the window
//! lies wholly inside one bit; aiming one sample early keeps the last bit of a
//! buffer inside that buffer. Sampled levels are NRZI-decoded: no change is a
//! 1, a change is a 0.

use std::f32::consts::PI;

/// Baud rate and mark/space tones of Bell 202.
const BAUD: f32 = 1200.0;
const MARK_HZ: f32 = 1200.0;
const SPACE_HZ: f32 = 2200.0;
/// Fraction of the clock's distance from mid-bit removed on each transition.
const CLOCK_GAIN: f32 = 0.5;

/// Streaming AFSK1200 demodulator; state carries across [`feed`](Self::feed) calls.
pub struct Afsk1200Demod {
    /// Last `n` samples, oldest at `pos`.
    ring: Vec<f32>,
    pos: usize,
    /// Reference tables over one window: [mark cos, mark sin, space cos, space sin].
    refs: [Vec<f32>; 4],
    /// Bit-clock phase in bit periods, sampled when it wraps past 1.
    phase: f32,
    step: f32,
    /// Current sample-level decision (true = mark).
    level: bool,
    /// Level at the previous bit sample, for NRZI.
    last_bit_level: bool,
}

impl Afsk1200Demod {
    /// Demodulator for audio at `audio_rate_hz` (at least 1200 Hz).
    pub fn new(audio_rate_hz: u32) -> Self {
        assert!(
            audio_rate_hz >= 1200,
            "Afsk1200Demod: audio rate below 1200 Hz"
        );
        let rate = audio_rate_hz as f32;
        let n = (audio_rate_hz / 1200) as usize;
        let table = |f: f32, sin: bool| -> Vec<f32> {
            (0..n)
                .map(|k| {
                    let a = 2.0 * PI * f * k as f32 / rate;
                    if sin { a.sin() } else { a.cos() }
                })
                .collect()
        };
        Self {
            ring: vec![0.0; n],
            pos: 0,
            refs: [
                table(MARK_HZ, false),
                table(MARK_HZ, true),
                table(SPACE_HZ, false),
                table(SPACE_HZ, true),
            ],
            phase: 0.0,
            step: BAUD / rate,
            level: true,
            last_bit_level: true,
        }
    }

    /// Feed audio; returns the NRZI-decoded data bits (0 or 1) recovered from it.
    pub fn feed(&mut self, audio: &[f32]) -> Vec<u8> {
        let n = self.ring.len();
        let mut out = Vec::with_capacity(audio.len() / n + 1);
        for &x in audio {
            self.ring[self.pos] = x;
            self.pos = (self.pos + 1) % n;
            // Correlate the window, oldest sample first, against each reference.
            let mut acc = [0.0f32; 4];
            for k in 0..n {
                let s = self.ring[(self.pos + k) % n];
                for (a, r) in acc.iter_mut().zip(&self.refs) {
                    *a += s * r[k];
                }
            }
            let mark = acc[0] * acc[0] + acc[1] * acc[1];
            let space = acc[2] * acc[2] + acc[3] * acc[3];
            let level = mark > space;
            if level != self.level {
                self.level = level;
                self.phase += (0.5 + self.step - self.phase) * CLOCK_GAIN;
            }
            self.phase += self.step;
            // Sample on the sample nearest the clock instant (phase 1.0).
            if self.phase >= 1.0 - self.step * 0.5 {
                self.phase -= 1.0;
                out.push(u8::from(self.level == self.last_bit_level));
                self.last_bit_level = self.level;
            }
        }
        out
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::ax25::{HdlcDeframer, parse_ax25_ui};
    use crate::fixtures::{
        afsk1200_modulate, awgn_real, ax25_ui_frame, hdlc_frame_bits, nrzi_encode,
    };

    const INFO: &[u8] = b"!3748.12S/14458.33E>";

    fn frame() -> Vec<u8> {
        ax25_ui_frame("VK3ABC-9", "APRS", &["WIDE1-1", "WIDE2-1"], INFO)
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn afsk_roundtrip_recovers_bits() {
        let bits = hdlc_frame_bits(&frame(), 8, 2);
        let audio = afsk1200_modulate(&nrzi_encode(&bits), 48_000);
        let mut demod = Afsk1200Demod::new(48_000);
        // Feed in uneven chunks so state must carry across calls.
        let mut out = Vec::new();
        for chunk in audio.chunks(777) {
            out.extend(demod.feed(chunk));
        }
        assert!(
            contains(&out, &bits),
            "recovered bits do not contain the sent stream"
        );
    }

    #[test]
    fn afsk_roundtrip_survives_10db_snr() {
        let f = frame();
        let bits = hdlc_frame_bits(&f, 8, 2);
        let mut audio = vec![0.0f32; 4_800];
        audio.extend(afsk1200_modulate(&nrzi_encode(&bits), 48_000));
        audio.extend(vec![0.0f32; 4_800]);
        awgn_real(&mut audio, 10.0, 0x5eed);
        let mut demod = Afsk1200Demod::new(48_000);
        let mut deframer = HdlcDeframer::new();
        let frames = deframer.feed(&demod.feed(&audio));
        assert_eq!(frames, vec![f[..f.len() - 2].to_vec()]);
        let ax = parse_ax25_ui(&frames[0]).expect("parse");
        assert_eq!(ax.src, "VK3ABC-9");
        assert_eq!(ax.info, INFO);
    }
}
