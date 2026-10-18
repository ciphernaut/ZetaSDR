//! Typed client for a ZetaSDR node.

use serde_json::Value;
use zeta_core::ops::{Op, OpError};

/// Parse a JSON operation. The error carries serde's message, which names an
/// unknown or missing field, so the server and the agent report it alike.
pub fn parse_op(v: Value) -> Result<Op, OpError> {
    serde_json::from_value(v).map_err(|e| OpError::Other {
        message: format!("invalid operation: {e}"),
    })
}
