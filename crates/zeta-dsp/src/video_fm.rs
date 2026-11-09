//! Analog television timing and the CCIR 405 pre/de-emphasis shelf shared by the
//! synthesised test card ([`crate::synth`]) and the video stages.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Composite video level at sync tip, volts.
pub const SYNC_V: f32 = -0.3;
/// Composite video level at blanking, volts.
pub const BLANK_V: f32 = 0.0;
/// Composite video level at peak white, volts.
pub const WHITE_V: f32 = 0.7;

/// The line standard of a 625/50 (PAL) or 525/60 (NTSC) signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub enum VideoStandard {
    Pal,
    Ntsc,
}

/// The timing of one standard, in seconds and lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StandardTable {
    pub line_s: f64,
    pub sync_s: f64,
    pub active_start_s: f64,
    pub active_s: f64,
    pub lines_per_field: [u32; 2],
    pub eq_lines: f64,
    pub broad_lines: f64,
    pub first_active_line: u32,
    pub active_lines: u32,
    pub luma_lowpass_hz: f64,
    pub preemphasis_crossover_hz: f64,
}

static PAL: StandardTable = StandardTable {
    line_s: 64e-6,
    sync_s: 4.7e-6,
    active_start_s: 10.5e-6,
    active_s: 52e-6,
    lines_per_field: [312, 313],
    eq_lines: 2.5,
    broad_lines: 2.5,
    first_active_line: 23,
    active_lines: 288,
    luma_lowpass_hz: 3.0e6,
    preemphasis_crossover_hz: 1.5e6,
};

static NTSC: StandardTable = StandardTable {
    line_s: 63.556e-6,
    sync_s: 4.7e-6,
    active_start_s: 9.4e-6,
    active_s: 52.6e-6,
    lines_per_field: [262, 263],
    eq_lines: 3.0,
    broad_lines: 3.0,
    first_active_line: 22,
    active_lines: 240,
    luma_lowpass_hz: 2.8e6,
    preemphasis_crossover_hz: 2.1e6,
};

impl VideoStandard {
    pub fn table(self) -> &'static StandardTable {
        match self {
            VideoStandard::Pal => &PAL,
            VideoStandard::Ntsc => &NTSC,
        }
    }
}

/// One first-order section `y[n] = b0 x[n] + b1 x[n-1] - a1 y[n-1]`: coefficients in f64,
/// state in f32.
#[derive(Clone, Copy, Debug)]
struct Section {
    b0: f64,
    b1: f64,
    a1: f64,
    x1: f32,
    y1: f32,
}

impl Section {
    fn new(b0: f64, b1: f64, a1: f64) -> Self {
        Section {
            b0,
            b1,
            a1,
            x1: 0.0,
            y1: 0.0,
        }
    }

    fn run(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            let y = (self.b0 * f64::from(*s) + self.b1 * f64::from(self.x1)
                - self.a1 * f64::from(self.y1)) as f32;
            self.x1 = *s;
            self.y1 = y;
            *s = y;
        }
    }
}

/// A first-order shelf (pre-emphasis) and its exact inverse (de-emphasis), bilinear
/// transformed.
#[derive(Clone, Debug)]
pub struct Shelf {
    pre: Section,
    de: Section,
}

impl Shelf {
    /// The CCIR 405 network: 14 dB in all (-7 dB low, +7 dB high), unity at the
    /// standard's crossover. Pre-emphasis `(1 + s t1) / (sqrt5 (1 + s t2))` with
    /// `t1 = sqrt5 / w_c`, `t2 = t1 / 5`; de-emphasis is its inverse.
    pub fn ccir405(standard: VideoStandard, rate_hz: f64) -> Shelf {
        let wc = std::f64::consts::TAU * standard.table().preemphasis_crossover_hz;
        let t1 = 5f64.sqrt() / wc;
        let t2 = t1 / 5.0;
        let k = 2.0 * rate_hz;
        let (n0, n1) = (1.0 + k * t1, 1.0 - k * t1);
        let (d0, d1) = (1.0 + k * t2, 1.0 - k * t2);
        let r5 = 5f64.sqrt();
        Shelf {
            pre: Section::new(n0 / (r5 * d0), n1 / (r5 * d0), d1 / d0),
            de: Section::new(r5 * d0 / n0, r5 * d1 / n0, n1 / n0),
        }
    }

    /// The plain single-pole de-emphasis of `wfm::deemphasis` (`tau_us` microseconds),
    /// and its inverse as the pre-emphasis.
    pub fn one_pole(tau_us: f64, rate_hz: f64) -> Shelf {
        let a = 1.0 - (-1.0 / (rate_hz * tau_us * 1e-6)).exp();
        Shelf {
            pre: Section::new(1.0 / a, -(1.0 - a) / a, 0.0),
            de: Section::new(a, 0.0, -(1.0 - a)),
        }
    }

    /// Pre-emphasise `x` in place.
    pub fn pre(&mut self, x: &mut [f32]) {
        self.pre.run(x);
    }

    /// De-emphasise `x` in place.
    pub fn de(&mut self, x: &mut [f32]) {
        self.de.run(x);
    }

    /// Clear both directions' state.
    pub fn reset(&mut self) {
        for s in [&mut self.pre, &mut self.de] {
            s.x1 = 0.0;
            s.y1 = 0.0;
        }
    }
}
