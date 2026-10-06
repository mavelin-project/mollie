//! Math functions of programs, the same on every platform: they're computed
//! in `f64` with only basic IEEE operations (no platform `libm`, which differs
//! between systems), then rounded to `f32`. Argument reduction and series in
//! `f64` are far more precise than `f32` needs.

use std::f64::consts::{FRAC_PI_2, LN_2, PI, SQRT_2};

/// `ln(2)` in two parts, the first with zeroes in its low bits, so `k * HI`
/// is exact for the `k` used.
const LN_2_HI: f64 = 6.931_471_803_691_238e-1;
const LN_2_LO: f64 = 1.908_214_929_270_587_7e-10;

/// `π / 2` in two parts (like fdlibm's `pio2_1` and `pio2_1t`).
const FRAC_PI_2_HI: f64 = 1.570_796_326_734_125_6;
const FRAC_PI_2_LO: f64 = 6.077_100_506_506_192e-11;

/// `x * 2^k`, exactly unless it overflows or underflows.
fn scale(mut x: f64, mut k: i32) -> f64 {
    let power = |k: i32| f64::from_bits(u64::try_from(1023 + k).unwrap_or(0) << 52);

    while k > 1000 {
        x *= power(1000);
        k -= 1000;
    }

    while k < -1000 {
        x *= power(-1000);
        k += 1000;
    }

    x * power(k)
}

pub fn exp(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }

    if x > 710.0 {
        return f64::INFINITY;
    }

    if x < -746.0 {
        return 0.0;
    }

    // x = k ln 2 + r, with |r| <= ln 2 / 2.
    let k = (x / LN_2).round();
    let r = k.mul_add(-LN_2_LO, k.mul_add(-LN_2_HI, x));
    let mut term = 1.0;
    let mut sum = 1.0;

    for n in 1..=17 {
        term *= r / f64::from(n);
        sum += term;
    }

    #[allow(clippy::cast_possible_truncation, reason = "|k| <= 1077")]
    scale(sum, k as i32)
}

pub fn ln(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }

    if x == 0.0 {
        return f64::NEG_INFINITY;
    }

    if x.is_infinite() {
        return x;
    }

    // x = m 2^e, with m in [sqrt(2) / 2, sqrt(2)).
    let (mut bits, mut e) = (x.to_bits(), 0);

    if (bits >> 52).trailing_zeros() >= 11 {
        // Subnormal: normalized first.
        bits = scale(x, 54).to_bits();
        e -= 54;
    }

    #[allow(clippy::cast_possible_truncation, reason = "11 bits")]
    let exponent = ((bits >> 52) & 0x7FF) as i32;

    e += exponent - 1023;

    let mut m = f64::from_bits((bits & ((1 << 52) - 1)) | (1023 << 52));

    if m > SQRT_2 {
        m /= 2.0;
        e += 1;
    }

    // ln(m) = 2 atanh(s), with |s| <= 0.172.
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let mut term = s;
    let mut sum = 0.0;

    for n in 0..13 {
        sum += term / f64::from(2 * n + 1);
        term *= s2;
    }

    f64::mul_add(f64::from(e), LN_2, 2.0 * sum)
}

/// Sine and cosine.
pub fn sin_cos(x: f64) -> (f64, f64) {
    if !x.is_finite() {
        return (f64::NAN, f64::NAN);
    }

    // x = k π/2 + r, with |r| <= π/4.
    let k = (x / FRAC_PI_2).round();
    let r = k.mul_add(-FRAC_PI_2_LO, k.mul_add(-FRAC_PI_2_HI, x));
    let r2 = r * r;
    let (mut sin, mut cos) = (0.0, 0.0);
    let (mut sin_term, mut cos_term) = (r, 1.0);

    for n in 0..10 {
        sin += sin_term;
        cos += cos_term;
        sin_term *= -r2 / f64::from((2 * n + 2) * (2 * n + 3));
        cos_term *= -r2 / f64::from((2 * n + 1) * (2 * n + 2));
    }

    #[allow(clippy::cast_possible_truncation, reason = "only the quadrant matters")]
    match (k as i64).rem_euclid(4) {
        0 => (sin, cos),
        1 => (cos, -sin),
        2 => (-sin, -cos),
        _ => (-cos, sin),
    }
}

pub fn atan(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }

    let (sign, x) = if x < 0.0 { (-1.0, -x) } else { (1.0, x) };
    // atan(x) = π/2 - atan(1/x).
    let (offset, x) = if x > 1.0 { (FRAC_PI_2, -1.0 / x) } else { (0.0, x) };
    // atan(x) = 2 atan(x / (1 + sqrt(1 + x²))), twice: |y| <= tan(π/16).
    let y = x / (1.0 + f64::mul_add(x, x, 1.0).sqrt());
    let y = y / (1.0 + f64::mul_add(y, y, 1.0).sqrt());
    let y2 = y * y;
    let mut term = y;
    let mut sum = 0.0;

    for n in 0..14 {
        let value = term / f64::from(2 * n + 1);

        sum += if n % 2 == 0 { value } else { -value };
        term *= y2;
    }

    sign * 4f64.mul_add(sum, offset)
}

pub fn atan2(y: f64, x: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }

    if x.is_infinite() && y.is_infinite() {
        let angle = if x > 0.0 { PI / 4.0 } else { 3.0 * PI / 4.0 };

        return angle.copysign(y);
    }

    if x == 0.0 {
        return if y == 0.0 {
            if x.is_sign_negative() { PI.copysign(y) } else { 0f64.copysign(y) }
        } else {
            FRAC_PI_2.copysign(y)
        };
    }

    let angle = atan(y / x);

    if x > 0.0 { angle } else { angle + PI.copysign(y) }
}

pub fn pow(x: f64, y: f64) -> f64 {
    if y == 0.0 || x == 1.0 {
        return 1.0;
    }

    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }

    if x == 0.0 {
        return if y < 0.0 { f64::INFINITY } else { 0.0 };
    }

    if x < 0.0 {
        // Only integer powers of negative numbers are real.
        if y.fract() != 0.0 {
            return f64::NAN;
        }

        let magnitude = exp(y * ln(-x));
        let odd = (y / 2.0).fract() != 0.0;

        return if odd { -magnitude } else { magnitude };
    }

    exp(y * ln(x))
}

/// Rounds to `f32`.
#[allow(clippy::cast_possible_truncation, reason = "rounding to f32 is the point")]
const fn single(x: f64) -> f32 {
    x as f32
}

pub(crate) extern "C" fn sin_f32(x: f32) -> f32 {
    single(sin_cos(f64::from(x)).0)
}

pub(crate) extern "C" fn cos_f32(x: f32) -> f32 {
    single(sin_cos(f64::from(x)).1)
}

pub(crate) extern "C" fn tan_f32(x: f32) -> f32 {
    let (sin, cos) = sin_cos(f64::from(x));

    single(sin / cos)
}

pub(crate) extern "C" fn atan2_f32(y: f32, x: f32) -> f32 {
    single(atan2(f64::from(y), f64::from(x)))
}

pub(crate) extern "C" fn exp_f32(x: f32) -> f32 {
    single(exp(f64::from(x)))
}

pub(crate) extern "C" fn ln_f32(x: f32) -> f32 {
    single(ln(f64::from(x)))
}

pub(crate) extern "C" fn pow_f32(x: f32, y: f32) -> f32 {
    single(pow(f64::from(x), f64::from(y)))
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    fn close(found: f64, expected: f64) {
        let tolerance = 1e-12 * expected.abs().max(1.0);

        assert!((found - expected).abs() <= tolerance, "found {found}, expected {expected}");
    }

    #[test]
    fn matches_std() {
        for &x in &[-100.0, -20.5, -3.0, -1.0, -0.5, -1e-8, 0.0, 1e-8, 0.3, 0.5, 1.0, 2.0, PI, 10.0, 88.0, 700.0] {
            close(exp(x), x.exp());

            let (sin, cos) = sin_cos(x);

            close(sin, x.sin());
            close(cos, x.cos());
            close(atan(x), x.atan());

            if x > 0.0 {
                close(ln(x), x.ln());
                close(pow(x, 1.7), x.powf(1.7));
            }
        }

        close(ln(f64::MIN_POSITIVE / 1024.0), (f64::MIN_POSITIVE / 1024.0).ln());
        close(atan2(-1.0, -1.0), (-1f64).atan2(-1.0));
        close(atan2(1.0, 0.0), 1f64.atan2(0.0));
        close(pow(-2.0, 3.0), -8.0);
        assert!(pow(-2.0, 0.5).is_nan());
    }
}
