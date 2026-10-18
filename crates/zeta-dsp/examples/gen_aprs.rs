//! Writes an APRS packet fixture (aprs.sigmf-meta/-data) into the given directory:
//! VK3ABC>APRS ">hi" as NBFM at 144.8 MHz, 48 kHz.
use std::path::PathBuf;

use zeta_dsp::fixtures::{aprs_packet_iq, write_fixture};

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: gen_aprs <dir>"));
    std::fs::create_dir_all(&dir).expect("create dir");
    let iq = aprs_packet_iq("VK3ABC", "APRS", ">hi", 48_000, 0.0);
    println!(
        "{}",
        write_fixture(&dir, "aprs", &iq, 48_000, 144_800_000).display()
    );
}
