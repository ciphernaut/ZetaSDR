//! Writes synthesised P25 phase 1 and DMR captures as FM discriminator audio, one 48 kHz
//! mono 16-bit WAV per capture, into the given directory, for checking them against
//! another decoder (one that reads discriminator audio, as receivers' "48 kHz WAV" input
//! does). All are clean: no noise, no offset, no clock error.
//!
//! The discriminator is the phase step between successive IQ samples, as hertz. Full
//! scale (32767) is 5 kHz, a receiver's usual narrowband FM maximum deviation, so the
//! outer P25 symbols (±1800 Hz) sit at about 36% of full scale and the DMR pulses' peaks
//! (±1944 Hz and the RRC overshoot) well inside it. 200 ms of silence goes before and
//! after each capture, so a decoder's filters settle before the first sync and give out
//! the last symbols before the file ends.
//!
//!   cargo run -p zeta-dsp --features test-fixtures --example gen_voice_wav -- <dir>
use std::path::{Path, PathBuf};

use num_complex::Complex;
use zeta_dsp::dmr::Origin;
use zeta_dsp::p25::{
    Call, EncSync, LinkControl, P25_ALGID_CLEAR, P25_BAUD, P25_DEVIATION_HZ, encode_call,
};
use zeta_dsp::synth::{DmrSecondCall, Impairments, SynthSpec, c4fm_modulate, synthesise};

/// The rate of the captures and of the WAVs.
const RATE_HZ: u32 = 48_000;
/// The discriminator's full scale, Hz.
const FULL_SCALE_HZ: f64 = 5_000.0;
/// Silence before and after each capture, ms.
const PAD_MS: u32 = 200;
/// Superframes per call.
const SUPERFRAMES: u32 = 3;
/// The P25 test identities (outside registered ranges).
const P25_NAC: u16 = 0x123;
const P25_SRC: u32 = 9_990_001;
const P25_TALKGROUP: u32 = 65_000;
/// The called radio of the private call.
const P25_PRIVATE_DST: u32 = 9_990_002;
/// The encrypted call's algorithm and key IDs.
const P25_ENC: EncSync = EncSync {
    algid: 0xAA,
    kid: 7,
};
/// The DMR test identities.
const DMR_COLOR_CODE: u8 = 5;
const DMR_SRC: u32 = 9_990_001;
const DMR_DST: u32 = 9_990;
const DMR_SECOND_SRC: u32 = 9_990_002;
const DMR_SECOND_DST: u32 = 9_991;

const USAGE: &str = "usage: gen_voice_wav <dir>

Writes p25_group, p25_tg1, p25_private, p25_encrypted, dmr_slot1, dmr_two_calls,
dmr_handheld and dmr_encrypted (.wav, 48 kHz mono s16, FM discriminator audio with
5 kHz full scale) into <dir>.";

fn main() {
    let arg = std::env::args().nth(1);
    let Some(dir) = arg.filter(|a| a != "--help" && a != "-h") else {
        println!("{USAGE}");
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("create dir");
    let clear = EncSync {
        algid: P25_ALGID_CLEAR,
        kid: 0,
    };
    let group = |dst| LinkControl {
        call: Call::Group,
        dst,
        src: P25_SRC,
        mfid: 0,
        emergency: false,
    };
    let private = LinkControl {
        call: Call::Private,
        ..group(P25_PRIVATE_DST)
    };
    // The group call through the family itself; the others need link controls and an
    // algorithm the family does not offer, so they go through its encoder and modulator.
    write(
        &dir,
        "p25_group",
        &family(&SynthSpec::P25 {
            nac: P25_NAC,
            src: P25_SRC,
            dst: P25_TALKGROUP,
            superframes: SUPERFRAMES,
            encrypted: false,
            control: false,
        }),
    );
    write(&dir, "p25_tg1", &p25(group(1), clear));
    write(&dir, "p25_private", &p25(private, clear));
    write(&dir, "p25_encrypted", &p25(group(P25_TALKGROUP), P25_ENC));
    let dmr = |origin, second, encrypted| SynthSpec::Dmr {
        color_code: DMR_COLOR_CODE,
        src: DMR_SRC,
        dst: DMR_DST,
        superframes: SUPERFRAMES,
        origin,
        second,
        encrypted,
        idle: false,
        control: false,
        talker_alias: false,
    };
    let second = DmrSecondCall {
        src: DMR_SECOND_SRC,
        dst: DMR_SECOND_DST,
        encrypted: false,
    };
    write(
        &dir,
        "dmr_slot1",
        &family(&dmr(Origin::Repeater, None, false)),
    );
    write(
        &dir,
        "dmr_two_calls",
        &family(&dmr(Origin::Repeater, Some(second), false)),
    );
    write(
        &dir,
        "dmr_handheld",
        &family(&dmr(Origin::Handheld, None, false)),
    );
    write(
        &dir,
        "dmr_encrypted",
        &family(&dmr(Origin::Repeater, None, true)),
    );
}

/// A synthesiser family's clean capture.
fn family(spec: &SynthSpec) -> Vec<Complex<f32>> {
    synthesise(spec, &Impairments::default(), RATE_HZ, 0)
}

/// A P25 call of [`SUPERFRAMES`] on [`P25_NAC`], modulated as the `p25` family does (its
/// 10 ms carrier-off tail left to the silence [`write`] adds).
fn p25(lc: LinkControl, enc: EncSync) -> Vec<Complex<f32>> {
    let dibits = encode_call(P25_NAC, lc, enc, SUPERFRAMES);
    c4fm_modulate(&dibits, P25_BAUD, P25_DEVIATION_HZ, RATE_HZ)
}

/// Writes `iq`'s discriminator audio to `dir/name.wav` and prints the path.
fn write(dir: &Path, name: &str, iq: &[Complex<f32>]) {
    let pad = vec![0i16; (RATE_HZ * PAD_MS / 1000) as usize];
    let mut samples = pad.clone();
    samples.extend(discriminate(iq));
    samples.extend(pad);
    let path = dir.join(format!("{name}.wav"));
    std::fs::write(&path, wav(&samples)).expect("write wav");
    println!("{}", path.display());
}

/// The instantaneous frequency of `iq`, scaled so [`FULL_SCALE_HZ`] is full scale. Where
/// the carrier is off (a zero sample) the frequency is zero.
fn discriminate(iq: &[Complex<f32>]) -> Vec<i16> {
    let per_hz = f64::from(i16::MAX) / FULL_SCALE_HZ;
    let mut last = Complex::new(0.0f32, 0.0);
    iq.iter()
        .map(|&s| {
            let step = s * last.conj();
            last = s;
            let hz = if step.norm_sqr() > 0.0 {
                f64::from(step.arg()) * f64::from(RATE_HZ) / std::f64::consts::TAU
            } else {
                0.0
            };
            (hz * per_hz)
                .round()
                .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
        })
        .collect()
}

/// A 48 kHz mono 16-bit PCM WAV of `samples`.
fn wav(samples: &[i16]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE_HZ.to_le_bytes());
    out.extend_from_slice(&(RATE_HZ * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
