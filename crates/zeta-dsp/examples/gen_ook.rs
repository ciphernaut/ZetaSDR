//! Writes the public synthesised OOK remote fixtures (`ook-ev1527-synth-*.sigmf-meta`/
//! `-data`) into the given directory: an EV1527-style fob with an invented ID at
//! 433.92 MHz. `crates/zeta-dsp/tests/data` holds the copies the tests and the README
//! quick start read; `ev1527_fixtures_match_their_generator` checks they are this
//! program's output.
use std::path::PathBuf;

use zeta_dsp::fixtures::{ev1527_fixtures, write_ev1527_fixture};

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: gen_ook <dir>"));
    std::fs::create_dir_all(&dir).expect("create dir");
    for f in ev1527_fixtures() {
        println!("{}", write_ev1527_fixture(&dir, &f).display());
    }
}
