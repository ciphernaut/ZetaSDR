//! Another ZetaSDR node's source, reached over its HTTP and websocket API, as a
//! local push [`zeta_core::port::Port`].

mod port;

pub use port::{
    NODE_DEFAULT_MAX_BANDWIDTH_HZ, NodePort, NodeSourceConfig, TUNE_TIMEOUT, open,
    proxy_capabilities,
};
