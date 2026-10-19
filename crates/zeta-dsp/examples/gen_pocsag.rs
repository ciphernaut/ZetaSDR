//! Writes a POCSAG fixture (pocsag.sigmf-meta/-data) into the given directory:
//! address 1234567, function 3, "HELLO ZETA" at 1200 baud as NBFM at 148.6375 MHz, 48 kHz.
use std::path::PathBuf;

use zeta_dsp::fixtures::{pocsag_packet_iq, write_fixture};

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: gen_pocsag <dir>"));
    std::fs::create_dir_all(&dir).expect("create dir");
    let iq = pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, 48_000, 0.0);
    println!(
        "{}",
        write_fixture(&dir, "pocsag", &iq, 48_000, 148_637_500).display()
    );
}
