//! The KiwiSDR waterfall sweep's plan: which zoom, which views, how long a dwell.
//!
//! Pure arithmetic over the server's facts (`docs/kiwi-waterfall.md`): a frame is
//! 1024 pixels over a view of `bandwidth >> zoom` Hz starting at an `x_bin` that
//! counts in `bandwidth / (1024 << zoom_max)` Hz, the server zeroes the first
//! pixels, and up to zoom 10 a frame is sampled after the change that asked for it.

use zeta_core::port::Tuning;

use crate::proto::WF_PIXELS;

/// The finest zoom the sweep uses: above it the server samples overlapped.
pub(crate) const WF_ZOOM_MAX: u8 = 10;
/// Frames a second at `wf_speed=4`.
pub(crate) const WF_FPS: u32 = 23;
/// Pixels the odd sweeps shift the views by, over the server's zeroed pixels (4 at
/// most, at zoom 0, where nothing shifts).
pub(crate) const WF_SHIFT_PIXELS: u64 = 8;
/// Frames dropped after the first one that names a new view.
pub(crate) const WF_SETTLE_FRAMES: u32 = 1;

/// Pixels as `u64`, for arithmetic on Hz.
const PIXELS: u64 = WF_PIXELS as u64;

/// A sweep's zoom, views and dwell.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WfPlan {
    pub zoom: u8,
    /// The width of one view, Hz.
    pub view_hz: u64,
    /// The width of one pixel, Hz.
    pub bin_hz: f64,
    /// The first Hz of each view of an even sweep, ascending and distinct.
    pub starts: Vec<u64>,
    /// How far an odd sweep's views sit below an even sweep's, Hz.
    pub shift_hz: u64,
    /// Frames read at each view.
    pub frames: u32,
    /// The dwell those frames make, ms.
    pub dwell_ms: u64,
}

impl WfPlan {
    /// The first Hz of the view at `step` in sweep number `sweep`: odd sweeps
    /// sit `shift_hz` lower (never under 0), so a pixel the server zeroed in the
    /// even sweep is read clean in the odd one.
    pub(crate) fn start(&self, sweep: u64, step: usize) -> u64 {
        let start = self.starts[step];
        if sweep % 2 == 1 {
            start.saturating_sub(self.shift_hz)
        } else {
            start
        }
    }

    /// The server's `x_bin` of a view starting at `start_hz`:
    /// `round(start × (1024 << zoom_max) / bandwidth)`.
    pub(crate) fn x_bin(&self, start_hz: u64, bandwidth_hz: u64, zoom_max: u8) -> u32 {
        let num = u128::from(start_hz) * (u128::from(PIXELS) << zoom_max);
        let den = u128::from(bandwidth_hz.max(1));
        ((2 * num + den) / (2 * den)).min(u128::from(u32::MAX)) as u32
    }
}

/// Plans a sweep of `span` at pixels of about `bin_hz` or finer, dwelling about
/// `dwell_ms` at each view, on a Kiwi whose ADC covers `bandwidth_hz`.
///
/// The zoom is the larger of the coarsest one whose pixel is at or under
/// `bin_hz` and the finest one whose view still holds the whole span, never over
/// [`WF_ZOOM_MAX`]. A view that holds the span is centred on it; otherwise the
/// views tile it from its low edge. Every start is clamped to the band and a
/// repeated start is dropped.
pub(crate) fn wf_plan(span: Tuning, bin_hz: u64, dwell_ms: u64, bandwidth_hz: u64) -> WfPlan {
    let view = |z: u8| bandwidth_hz >> z;
    let width = span.bandwidth_hz.0;
    let z_target = (0..=WF_ZOOM_MAX)
        .find(|z| view(*z) <= bin_hz.saturating_mul(PIXELS))
        .unwrap_or(WF_ZOOM_MAX);
    let z_fit = (0..=WF_ZOOM_MAX)
        .rev()
        .find(|z| view(*z) >= width)
        .unwrap_or(0);
    let zoom = z_target.max(z_fit);
    let view_hz = view(zoom);
    let lo = span.center_hz.0.saturating_sub(width / 2);
    let raw: Vec<u64> = if view_hz >= width {
        vec![span.center_hz.0.saturating_sub(view_hz / 2)]
    } else {
        (0..width.div_ceil(view_hz))
            .map(|k| lo.saturating_add(k * view_hz))
            .collect()
    };
    let last = bandwidth_hz.saturating_sub(view_hz);
    let mut starts: Vec<u64> = Vec::with_capacity(raw.len());
    for start in raw.into_iter().map(|s| s.min(last)) {
        if !starts.contains(&start) {
            starts.push(start);
        }
    }
    let frames = (dwell_ms * u64::from(WF_FPS))
        .div_ceil(1000)
        .clamp(1, u64::from(u32::MAX)) as u32;
    WfPlan {
        zoom,
        view_hz,
        bin_hz: view_hz as f64 / WF_PIXELS as f64,
        starts,
        // Zoom 0 is the whole band: no view to shift to.
        shift_hz: if zoom == 0 {
            0
        } else {
            WF_SHIFT_PIXELS * view_hz / PIXELS
        },
        frames,
        dwell_ms: (u64::from(frames) * 2000 + u64::from(WF_FPS)) / (2 * u64::from(WF_FPS)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeta_core::types::Hz;

    const BW: u64 = 30_000_000;

    fn span(lo: u64, hi: u64) -> Tuning {
        Tuning {
            center_hz: Hz((lo + hi) / 2),
            bandwidth_hz: Hz(hi - lo),
        }
    }

    #[test]
    fn the_whole_band_at_6_khz_is_zoom_3_in_eight_steps() {
        let p = wf_plan(span(0, BW), 6000, 100, BW);
        assert_eq!(p.zoom, 3);
        assert_eq!(p.view_hz, 3_750_000);
        assert!((p.bin_hz - 3662.1).abs() < 0.1, "{}", p.bin_hz);
        let want: Vec<u64> = (0..8).map(|k| k * 3_750_000).collect();
        assert_eq!(p.starts, want);
        assert_eq!(p.shift_hz, 29_296);
    }

    #[test]
    fn at_8_khz_it_is_zoom_2_in_four() {
        let p = wf_plan(span(0, BW), 8000, 100, BW);
        assert_eq!(p.zoom, 2);
        let want: Vec<u64> = (0..4).map(|k| k * 7_500_000).collect();
        assert_eq!(p.starts, want);
    }

    #[test]
    fn the_coarsest_bins_are_zoom_0_in_one_step_with_no_shift() {
        let p = wf_plan(span(0, BW), 30_000, 100, BW);
        assert_eq!(p.zoom, 0);
        assert_eq!(p.starts, vec![0]);
        assert_eq!(p.shift_hz, 0);
        assert_eq!(p.start(1, 0), 0);
    }

    #[test]
    fn a_narrow_span_takes_the_finest_zoom_that_holds_it() {
        // 300 kHz wide at 7.1 MHz: view(6) = 468 750 holds it, view(7) does not.
        let p = wf_plan(span(6_950_000, 7_250_000), 5000, 100, BW);
        assert_eq!(p.zoom, 6);
        assert_eq!(p.view_hz, 468_750);
        assert_eq!(p.starts, vec![7_100_000 - 234_375]);
    }

    #[test]
    fn views_clamp_at_the_band_edges_without_a_repeated_step() {
        let p = wf_plan(span(29_000_000, 30_000_000), 500, 100, BW);
        assert_eq!((p.zoom, p.view_hz), (6, 468_750));
        assert_eq!(p.starts, vec![29_000_000, 29_468_750, 29_531_250]);
        let p = wf_plan(span(29_000_000, 31_000_000), 500, 100, BW);
        assert_eq!((p.zoom, p.view_hz), (6, 468_750));
        assert_eq!(p.starts, vec![29_000_000, 29_468_750, 29_531_250]);
    }

    #[test]
    fn the_zoom_never_passes_10() {
        let p = wf_plan(span(10_000_000, 10_010_000), 1, 100, BW);
        assert_eq!(p.zoom, WF_ZOOM_MAX);
        assert_eq!(p.view_hz, BW >> 10);
    }

    #[test]
    fn the_odd_sweep_sees_each_zeroed_pixel_clean() {
        for (lo, hi, bin) in [
            (0, BW, 6000),
            (1_000_000, 29_000_000, 6000),
            (7_000_000, 14_000_000, 3000),
            (14_000_000, 14_200_000, 200),
            // Clamped at the top edge: the last start is 37 500 Hz after the one before.
            (29_000_000, 30_000_000, 500),
            (29_000_000, 31_000_000, 500),
        ] {
            let p = wf_plan(span(lo, hi), bin, 100, BW);
            // Exact, in 1/1024 Hz: a pixel is `view_hz` of them, the server zeroes
            // 4. No slack: this compares the plan's own Hz, not an `x_bin` (whose
            // rounding is under a unit of bandwidth / (1024 << zoom_max), well under
            // a pixel, and never moves a view the plan placed).
            let zeroed = 4 * p.view_hz;
            // The zeroed pixels of the view at `z` lie in the clean ones of the
            // view at `s`: pixels 4.. of 1024.
            let clean = |z: u64, s: u64| {
                z * 1024 >= s * 1024 + zeroed && z * 1024 + zeroed <= (s + p.view_hz) * 1024
            };
            for step in 0..p.starts.len() {
                let even = p.start(0, step);
                let odd = p.start(1, step);
                if even > 0 {
                    assert!(clean(even, odd), "{lo}-{hi} step {step}: even in odd");
                }
                if step > 0 && odd > 0 {
                    // The preceding step's own even view must cover them.
                    assert!(
                        clean(odd, p.start(0, step - 1)),
                        "{lo}-{hi} step {step}: odd in the preceding even view"
                    );
                }
            }
        }
    }

    #[test]
    fn a_dwell_is_whole_frames_at_least_one() {
        let at = |ms| {
            let p = wf_plan(span(0, BW), 6000, ms, BW);
            (p.frames, p.dwell_ms)
        };
        assert_eq!(at(20), (1, 43));
        assert_eq!(at(250), (6, 261));
        assert_eq!(at(0), (1, 43));
    }

    #[test]
    fn x_bin_counts_in_the_servers_units() {
        let p = wf_plan(span(0, BW), 6000, 100, BW);
        assert_eq!(p.x_bin(0, BW, 14), 0);
        assert_eq!(p.x_bin(3_750_000, BW, 14), 2_097_152);
        assert_eq!(p.x_bin(BW, BW, 14), 16_777_216);
        // A unit is 1.79 Hz on 30 MHz: 1 Hz is 0.56 of one and rounds up, 2 Hz
        // is 1.12 and rounds down.
        assert_eq!(p.x_bin(1, BW, 14), 1);
        assert_eq!(p.x_bin(2, BW, 14), 1);
        assert_eq!(p.x_bin(3, BW, 14), 2);
    }
}
