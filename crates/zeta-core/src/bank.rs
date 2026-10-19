//! Channel banks: one wideband channeliser per wide source, shared by every
//! job on it. The node feeds each bank its source's blocks; a channel stage
//! subscribes to the bin nearest its request instead of filtering the whole
//! capture itself. Implementations live outside core (see `zeta-dsp`).

use std::sync::Arc;

use crate::port::{IqBlock, IqReceiver, Tuning};
use crate::types::Hz;

/// Lowest source rate given a bank; narrower sources are filtered directly.
pub const PFB_MIN_RATE_HZ: u64 = 2_000_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BankError {
    #[error("bandwidth_hz {bandwidth_hz} is wider than one bank bin ({bin_hz} Hz)")]
    TooWide { bandwidth_hz: u64, bin_hz: u64 },
    #[error("center_hz {center_hz} with its bandwidth leaves the bank band {min_hz}..={max_hz}")]
    OutOfBand {
        center_hz: u64,
        min_hz: u64,
        max_hz: u64,
    },
}

/// A channeliser over one wide capture band, split into equally spaced bins.
pub trait ChannelBank: Send + Sync {
    /// The capture this bank splits: centre and complex sample rate.
    fn band(&self) -> Tuning;
    /// Bin spacing in hertz; the widest channel a bin serves.
    fn bin_hz(&self) -> u64;
    /// Subscribe to the bin nearest `center_hz`. Returns its block stream,
    /// its sample rate and its centre. `TooWide` when `bandwidth_hz` exceeds
    /// [`ChannelBank::bin_hz`]; `OutOfBand` when the channel leaves the band.
    fn subscribe(
        &self,
        center_hz: u64,
        bandwidth_hz: u64,
    ) -> Result<(IqReceiver, Hz, Hz), BankError>;
    /// Live subscribers across all bins.
    fn consumers(&self) -> usize;
    /// Feed one block of the source (called by the node, in order). Must not
    /// block: an implementation that cannot keep up drops the block and
    /// counts it in [`ChannelBank::dropped`].
    fn push(&self, block: &IqBlock);
    /// Input blocks dropped because the bank could not keep up.
    fn dropped(&self) -> u64 {
        0
    }
    /// Input was lost before reaching the bank (the node's feed lagged):
    /// start the filter afresh rather than run it across the gap.
    fn reset(&self) {}
}

/// Makes the bank for a capture band; `None` when it declines.
pub trait BankFactory: Send + Sync {
    fn make(&self, rate_hz: Hz, center_hz: Hz) -> Option<Arc<dyn ChannelBank>>;
}
