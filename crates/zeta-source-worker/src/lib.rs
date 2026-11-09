//! The SoapySDR driver in its own process: the wire format the node and the
//! worker child speak over stdin and stdout, the child's side of it, and the
//! node's side: a port that spawns and supervises the child.

pub mod env;
pub mod port;
pub mod ring;
pub mod serve;
pub mod spawn;
mod supervisor;
pub mod wire;
