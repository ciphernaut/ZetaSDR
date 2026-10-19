//! Typed client for a ZetaSDR node.

pub mod guide;
pub mod remote;
pub mod tools;
pub mod ws;
pub use guide::{op_sections, system_prompt};
pub use remote::{ClientError, NodeClient, STT_HOP_HEADER};
pub use tools::{ToolDef, op_from_call, op_tools};
pub use ws::{CLIENT_INITIAL_CREDIT, CLIENT_TOPUP, Received, Subscription};

use serde_json::Value;
use zeta_core::ops::{Op, OpError};

/// Parse a JSON operation. The error carries serde's message, which names an
/// unknown or missing field, so the server and the agent report it alike.
pub fn parse_op(v: Value) -> Result<Op, OpError> {
    serde_json::from_value(v).map_err(|e| OpError::Other {
        message: format!("invalid operation: {e}"),
    })
}
