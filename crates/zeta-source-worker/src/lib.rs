//! The SoapySDR driver in its own process: the wire format the node and the
//! worker child speak over stdin and stdout, and the child's side of it.

pub mod serve;
pub mod wire;
