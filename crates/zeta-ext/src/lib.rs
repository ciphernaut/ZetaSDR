//! ZetaSDR stages whose work is done by an external decoder program on the node's
//! host: `external_decode`, which runs a mode row's program ([`external`]), with
//! its process plumbing ([`proc`]) and output parsers ([`parsers`]); and
//! `adsb_decode`, the alias that runs the `adsb_decode` row ([`adsb`]).

pub mod adsb;
pub mod external;
pub mod parsers;
pub mod proc;

pub use adsb::{ADSB_MODE, ADSB_RATE_HZ, AdsbDecodeStage, AdsbParams, find_dump1090};
pub use external::{EXTERNAL_DECODE_KIND, ExternalDecodeStage, ExternalParams, ModesHandle};
pub use parsers::sbs;
pub use proc::HANDLER_VERSION_TIMEOUT;
