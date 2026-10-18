//! Writes the 1 kHz test tone fixture (tone.sigmf-meta/-data) into the given directory.
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("usage: gen_tone <dir>"));
    std::fs::create_dir_all(&dir).expect("create dir");
    println!("{}", zeta_source_file::fixtures::write_tone(&dir).display());
}
