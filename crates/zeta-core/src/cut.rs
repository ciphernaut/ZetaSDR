//! The band cutter: one band of an IQ block, brought to baseband at the lowest rate that
//! holds it. The node holds one when the binary installs it ([`crate::node::Node::set_cutter`]);
//! `zeta-dsp` implements it, so the filter runs in the crate the dev profile optimises and
//! `zeta-core` carries no per-sample code.

use num_complex::Complex;

use crate::types::Hz;

pub trait BandCutter: Send + Sync {
    /// The band `bandwidth_hz` wide at `offset_hz` from the block's centre, at the lowest exact
    /// divisor of `rate_hz` that holds it; `None` when it cannot be cut.
    ///
    /// `iq` is sampled at `rate_hz`. The samples returned are centred on the band (it is at
    /// 0 Hz in them) and sampled at the returned rate. The band must lie inside the block,
    /// `offset_hz ± bandwidth_hz / 2` within `± rate_hz / 2` (touching an edge is inside);
    /// a band reaching past either edge, or no band or rate at all, is `None`. A cut never
    /// reads outside `iq`.
    fn cut(
        &self,
        iq: &[Complex<f32>],
        rate_hz: Hz,
        offset_hz: i64,
        bandwidth_hz: Hz,
    ) -> Option<(Vec<Complex<f32>>, Hz)>;
}
