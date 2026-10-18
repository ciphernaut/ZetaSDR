use std::path::{Path, PathBuf};

pub fn tone(dir: &Path) -> PathBuf {
    zeta_source_file::fixtures::write_tone(dir)
}
