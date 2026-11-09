//! Writes an analog video fixture (video-pal-8m.sigmf-meta/-data) into the given directory
//! (`scratch` by default): 25 fields of the PAL test card, frequency-modulated at 1.5 MHz per
//! volt, at 8 MHz centred on 100 MHz, with noise 25 dB under the carrier.
use std::path::PathBuf;

use zeta_dsp::fixtures::write_fixture;
use zeta_dsp::synth::{Impairments, SynthSpec, synthesise};
use zeta_dsp::video_fm::VideoStandard;

const RATE_HZ: u32 = 8_000_000;
const CENTER_HZ: u64 = 100_000_000;

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "scratch".into()));
    std::fs::create_dir_all(&dir).expect("create dir");
    let spec = SynthSpec::Video {
        standard: VideoStandard::Pal,
        deviation_hz: 1.5e6,
        subcarrier_hz: None,
        rate_hz: RATE_HZ,
        fields: 25,
        preemphasis: true,
    };
    let imp = Impairments {
        snr_db: Some(25.0),
        ..Impairments::default()
    };
    let iq = synthesise(&spec, &imp, RATE_HZ, CENTER_HZ);
    println!(
        "{}",
        write_fixture(&dir, "video-pal-8m", &iq, RATE_HZ, CENTER_HZ).display()
    );
}
