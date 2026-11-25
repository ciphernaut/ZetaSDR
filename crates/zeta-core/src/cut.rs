//! The band cutter: one band of an IQ block, brought to baseband at the rate a job's channel
//! would run it at. The node holds one when the binary installs it
//! ([`crate::node::Node::set_cutter`]); `zeta-dsp` implements it, so the filter runs in the
//! crate the dev profile optimises and `zeta-core` carries no per-sample code.

use num_complex::Complex;

use crate::types::Hz;

pub trait BandCutter: Send + Sync {
    /// The band `bandwidth_hz` wide at `offset_hz` from the block's centre, at the rate a
    /// job's channel would run it at; `None` when it cannot be cut.
    ///
    /// - **The rate** is [`crate::ops::channel_rate_hz`]`(rate_hz, bandwidth_hz)`: the lowest
    ///   exact divisor of `rate_hz` at least as wide as the band, and never below
    ///   [`crate::ops::CHANNEL_RATE_MIN_HZ`] (the block's own rate when it is no wider).
    /// - **The samples** are centred on the band (it is at 0 Hz in them). With the rate cut
    ///   by `decim` there are `ceil(iq.len() / decim)` of them, the first taken from `iq[0]`.
    ///   The filters start from zeros, so their start-up delay (a few tens of output samples
    ///   at most) is left in at the front, and nothing is flushed at the end. An empty block
    ///   is `Some` with no samples.
    /// - **The band's edges are not guarded.** The filter is about −6 dB at `± bandwidth_hz / 2`,
    ///   and when the band fills the output rate, what lies just outside one edge folds onto
    ///   the other. A caller cutting a band to hold several (a union of rows' bands) passes it
    ///   wider by a guard, so each inner band is clear of the edges.
    /// - **Inside the block only.** `offset_hz ± bandwidth_hz / 2` must lie within
    ///   `± rate_hz / 2`; touching an edge is inside. A band reaching past either edge, or no
    ///   band or rate at all, is `None`. A cut never reads outside `iq`; a band touching an
    ///   edge has the block's far edge (what the radio's own filter left there) in its
    ///   transition.
    fn cut(
        &self,
        iq: &[Complex<f32>],
        rate_hz: Hz,
        offset_hz: i64,
        bandwidth_hz: Hz,
    ) -> Option<(Vec<Complex<f32>>, Hz)>;
}
