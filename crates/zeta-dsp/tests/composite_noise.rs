//! `composite_decode` on the test card with white noise added.
#![cfg(feature = "test-fixtures")]

use zeta_dsp::composite::{CompositeDecoder, CompositeParams, Scale, StandardChoice};
use zeta_dsp::fixtures::awgn_real;
use zeta_dsp::synth::composite_test_card;
use zeta_dsp::video_fm::VideoStandard;

const RATE: u32 = 8_000_000;

#[test]
fn awgn_at_10_20_30_db_still_locks() {
    for snr in [10.0f32, 20.0, 30.0] {
        let mut card = composite_test_card(VideoStandard::Pal, RATE, 8);
        awgn_real(&mut card, snr, 0x5eed + snr as u64);
        let mut dec = CompositeDecoder::new(&CompositeParams {
            standard: StandardChoice::Auto,
            scale: Scale::Full,
            report_s: 5,
        });
        let block = (RATE / 1000) as usize;
        let mut fields = 0;
        for (i, c) in card.chunks(block).enumerate() {
            let t = (i * block) as u64 * 125;
            fields += dec.push(c, u64::from(RATE), t).len();
        }
        let l = dec.lock();
        eprintln!("snr {snr} dB: {l:?}, {fields} fields");
        assert!(l.locked, "snr {snr}: {l:?}");
        assert_eq!(l.standard, Some(VideoStandard::Pal), "snr {snr}");
        assert!(fields > 0, "snr {snr}: no fields");
        if snr >= 30.0 {
            assert!(l.snr_db >= 20, "snr_db {} at 30 dB", l.snr_db);
        }
    }
}
