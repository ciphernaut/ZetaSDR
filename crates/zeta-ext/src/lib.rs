//! ZetaSDR stages whose work is done by an external decoder program on the
//! node's host. So far: `adsb_decode`, running dump1090.

pub mod adsb;
pub mod sbs;

pub use adsb::{ADSB_RATE_HZ, AdsbDecodeStage, AdsbParams, PASS_ENV, PROGRAMS, find_dump1090};
