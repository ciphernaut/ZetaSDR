//! ZetaSDR stages whose work is done by an external decoder program on the node's
//! host: `external_decode`, which runs a mode row's program ([`external`]), with
//! its process plumbing ([`proc`]) and output parsers ([`parsers`]);
//! `adsb_decode`, the alias that runs the `adsb_decode` row ([`adsb`]);
//! `sink_sondehub`, which uploads sonde telemetry ([`sink_sondehub`]); and the
//! `acma` knowledge provider over the operator's register service ([`acma`]).

pub mod acma;
pub mod adsb;
pub mod external;
pub mod mcp_client;
pub mod parsers;
pub mod proc;
pub mod sink_sondehub;

pub use acma::AcmaKnowledge;
pub use adsb::{ADSB_MODE, ADSB_RATE_HZ, AdsbDecodeStage, AdsbParams, find_dump1090};
pub use external::{EXTERNAL_DECODE_KIND, ExternalDecodeStage, ExternalParams, ModesHandle};
pub use parsers::sbs;
pub use proc::HANDLER_VERSION_TIMEOUT;
pub use sink_sondehub::{BATCH_MS, SONDEHUB_KIND, SONDEHUB_URL, SondehubParams, SondehubStage};
