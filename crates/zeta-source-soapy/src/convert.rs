//! Wire format to cf32 conversion (same scaling as the file source).

use num_complex::Complex;

pub fn cs8_to_cf32(x: &[Complex<i8>], out: &mut [Complex<f32>]) {
    for (o, s) in out.iter_mut().zip(x) {
        *o = Complex::new(f32::from(s.re) / 128.0, f32::from(s.im) / 128.0);
    }
}

/// [`zeta_core::port::sc16_to_cf32`], the one conversion of 16-bit samples:
/// a worker's `sc16` ring is converted at the node by the same function, so a
/// capture is bit-identical whichever side converts.
pub fn cs16_to_cf32(x: &[Complex<i16>], out: &mut [Complex<f32>]) {
    zeta_core::port::sc16_to_cf32(x, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_scale_to_unit() {
        let mut o = [Complex::new(0.0, 0.0); 2];
        cs8_to_cf32(&[Complex::new(-128, 127), Complex::new(0, 64)], &mut o);
        assert_eq!(o[0], Complex::new(-1.0, 0.9921875));
        assert_eq!(o[1], Complex::new(0.0, 0.5));
        cs16_to_cf32(&[Complex::new(-32768, 16384), Complex::new(0, 0)], &mut o);
        assert_eq!(o[0], Complex::new(-1.0, 0.5));
    }
}
