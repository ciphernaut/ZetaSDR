//! AX.25 / AFSK vectors produced by a third-party implementation (Dire Wolf 1.7), so a mistake we
//! share with our own generator cannot hide.
//!
//! `tests/data/direwolf-1.7-vk3abc9-48k.wav` (67 446 bytes, SHA-256
//! `bc1209f1ee87e730bd0a7e9180015b83ff791d3203a07a5a5a5bcdb06f9f146b`) was made with
//! `printf '%s\n' 'VK3ABC-9>APRS,WIDE1-1,WIDE2-1:!3748.12S/14458.33E>ZetaSDR test' > pkt.txt &&
//! gen_packets -r 48000 -o direwolf-1.7-vk3abc9-48k.wav pkt.txt`; `atest -h` decodes it to the
//! frame below. The audio test skips when the file is absent.
#![cfg(feature = "test-fixtures")]

use zeta_dsp::afsk::Afsk1200Demod;
use zeta_dsp::ax25::{HdlcDeframer, parse_ax25_ui};
use zeta_dsp::crc16_x25;
use zeta_dsp::fixtures::hdlc_frame_bits;

/// The 63-byte frame without FCS, as printed by `atest -h`.
const DW_FRAME_HEX: &str = "82a0a4a64040e0ac9666828486f2ae92888a624062ae92888a64406303f021333734382e3132532f31343435382e3333453e5a65746153445220746573740a";

fn frame() -> Vec<u8> {
    (0..DW_FRAME_HEX.len() / 2)
        .map(|i| u8::from_str_radix(&DW_FRAME_HEX[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

#[test]
fn direwolf_frame_parses() {
    let f = frame();
    assert_eq!(f.len(), 63);
    let p = parse_ax25_ui(&f).expect("UI frame");
    assert_eq!(p.src, "VK3ABC-9");
    assert_eq!(p.dst, "APRS");
    assert_eq!(p.path, ["WIDE1-1", "WIDE2-1"]);
    assert_eq!(p.info, b"!3748.12S/14458.33E>ZetaSDR test\n");
    assert_eq!(crc16_x25(&f), 0x02F6);
}

#[test]
fn direwolf_frame_through_deframer() {
    let f = frame();
    let mut good = f.clone();
    good.extend_from_slice(&[0xf6, 0x02]);
    let mut d = HdlcDeframer::new();
    assert_eq!(d.feed(&hdlc_frame_bits(&good, 8, 2)), vec![f.clone()]);
    assert_eq!(d.fcs_failures(), 0);

    let mut bad = f;
    bad.extend_from_slice(&[0x02, 0xf6]);
    let mut d = HdlcDeframer::new();
    assert!(d.feed(&hdlc_frame_bits(&bad, 8, 2)).is_empty());
    assert_eq!(d.fcs_failures(), 1);
}

#[test]
fn direwolf_audio_decodes() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/direwolf-1.7-vk3abc9-48k.wav");
    let Ok(wav) = std::fs::read(&path) else {
        eprintln!("skipping: {} not present", path.display());
        return;
    };
    assert!(wav.len() > 44);
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "channels");
    assert_eq!(
        u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
        48_000
    );
    assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16, "bits");
    let samples: Vec<f32> = wav[44..]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect();
    let bits = Afsk1200Demod::new(48_000)
        .expect("48 kHz supported")
        .feed(&samples);
    let frames = HdlcDeframer::new().feed(&bits);
    assert_eq!(frames, vec![frame()]);
}
