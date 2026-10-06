// Floating point helpers for a `no_std` crate.
//
// `core` has no square root, no logarithm, no exponential and no trigonometry for `f64`, and the
// crate takes no dependencies, so the few functions the codec needs are written here. They work in
// f64 and aim at an error of one or two units in the last place over the ranges the codec uses: the
// encoder's decisions (global gain, TNS quantization, band energies in dB) compare such results with
// thresholds, and a sloppy logarithm would move those decisions away from what the specification's
// double precision reference does.
//
// The approach is the classic one: reduce the argument to a small interval with exact operations on
// the bit pattern (exponent split, multiple of pi/2), then evaluate a short series there.

pub(crate) const PI: f64 = core::f64::consts::PI;
const LN_2: f64 = core::f64::consts::LN_2;
const LOG2_E: f64 = core::f64::consts::LOG2_E;
const LOG2_10: f64 = core::f64::consts::LOG2_10;
const LOG10_2: f64 = core::f64::consts::LOG10_2;
const SQRT_2: f64 = core::f64::consts::SQRT_2;
const FRAC_PI_2: f64 = core::f64::consts::FRAC_PI_2;

// 2^52: every f64 of at least this magnitude is an integer.
const TWO_52: f64 = 4503599627370496.0;

pub(crate) fn abs(x: f64) -> f64 {
	f64::from_bits(x.to_bits() & !(1u64 << 63))
}

pub(crate) fn min(a: f64, b: f64) -> f64 {
	if b < a { b } else { a }
}

pub(crate) fn max(a: f64, b: f64) -> f64 {
	if b > a { b } else { a }
}

/// Rounds toward zero.
pub(crate) fn trunc(x: f64) -> f64 {
	// NaN, infinities and anything at least 2^52 in magnitude are returned unchanged.
	if x.is_nan() || abs(x) >= TWO_52 {
		return x;
	}
	(x as i64) as f64
}

/// Rounds toward minus infinity: the specification's floor bracket.
pub(crate) fn floor(x: f64) -> f64 {
	let t = trunc(x);
	if t > x { t - 1.0 } else { t }
}

/// Rounds toward plus infinity: the specification's ceiling bracket.
pub(crate) fn ceil(x: f64) -> f64 {
	let t = trunc(x);
	if t < x { t + 1.0 } else { t }
}

/// The specification's nint(): nearest integer, halves away from zero (nint(-4.5) = -5).
pub(crate) fn round(x: f64) -> f64 {
	let t = trunc(x);
	// x - t is exact here, so the comparison with one half is exact too.
	let d = x - t;
	if d >= 0.5 {
		t + 1.0
	} else if d <= -0.5 {
		t - 1.0
	} else {
		t
	}
}

/// nint() as an integer; the arguments the codec passes are far inside the i32 range.
pub(crate) fn nint(x: f64) -> i32 {
	round(x) as i32
}

pub(crate) fn sqrt(x: f64) -> f64 {
	if x == 0.0 || x == f64::INFINITY {
		return x;
	}
	if x.is_nan() || x < 0.0 {
		return f64::NAN;
	}
	if x < 1.0e-300 {
		// Subnormal and tiny arguments: scale by 2^600 so the start below stays close.
		return sqrt(x * pow2i(600)) * pow2i(-300);
	}
	// Halving the biased exponent gives a start within a few percent of the root; Newton's
	// iteration doubles the number of correct digits each step, so six steps are plenty.
	let mut y = f64::from_bits((x.to_bits() >> 1) + (0x3ff0_0000_0000_0000 >> 1));
	for _ in 0..6 {
		y = 0.5 * (y + x / y);
	}
	y
}

/// 2^n for an integer n, including the subnormal and overflow ranges.
fn pow2i(n: i32) -> f64 {
	if n > 1023 {
		f64::INFINITY
	} else if n >= -1022 {
		f64::from_bits(((n + 1023) as u64) << 52)
	} else if n >= -1074 {
		f64::from_bits(1u64 << (n + 1074))
	} else {
		0.0
	}
}

pub(crate) fn exp2(x: f64) -> f64 {
	if x.is_nan() {
		return x;
	}
	if x >= 1024.0 {
		return f64::INFINITY;
	}
	if x < -1080.0 {
		return 0.0;
	}
	// x = n + f with |f| <= 1/2, 2^f = e^(f ln 2) with |f ln 2| <= 0.347: the Taylor series to the
	// 14th power leaves a remainder below 1e-17.
	let n = round(x);
	let y = (x - n) * LN_2;
	let mut p = 1.0;
	let mut k = 14.0;
	while k > 0.0 {
		p = 1.0 + p * y / k;
		k -= 1.0;
	}
	// Two steps keep 2^n representable while p is still being scaled.
	let n = n as i32;
	let h = n / 2;
	p * pow2i(h) * pow2i(n - h)
}

pub(crate) fn exp(x: f64) -> f64 {
	if x.is_nan() || abs(x) >= 1000.0 {
		return exp2(x * LOG2_E);
	}
	// x = n ln 2 + r, |r| <= ln(2)/2, with ln 2 in two parts so that n times the first is exact
	// (it has 32 significant bits); then e^r by its Taylor series and the 2^n scaling of exp2.
	const LN2_HI: f64 = 0.6931471803691238;
	const LN2_LO: f64 = 1.9082149292705877e-10;
	let n = round(x * LOG2_E);
	let r = (x - n * LN2_HI) - n * LN2_LO;
	let mut p = 1.0;
	let mut k = 14.0;
	while k > 0.0 {
		p = 1.0 + p * r / k;
		k -= 1.0;
	}
	let n = n as i32;
	let h = n / 2;
	p * pow2i(h) * pow2i(n - h)
}

/// 10^x.
pub(crate) fn pow10(x: f64) -> f64 {
	exp2(x * LOG2_10)
}

pub(crate) fn log2(x: f64) -> f64 {
	if x.is_nan() || x < 0.0 {
		return f64::NAN;
	}
	if x == 0.0 {
		return f64::NEG_INFINITY;
	}
	if x == f64::INFINITY {
		return x;
	}
	let mut bits = x.to_bits();
	let mut e: i32 = -1023;
	if bits >> 52 == 0 {
		// Subnormal: scale into the normal range first.
		bits = (x * 18014398509481984.0).to_bits(); // 2^54
		e -= 54;
	}
	e += ((bits >> 52) & 0x7ff) as i32;
	let mut m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
	if m > SQRT_2 {
		m *= 0.5;
		e += 1;
	}
	// m in (0.707, 1.415]: ln m = 2 atanh(s), s = (m - 1) / (m + 1), |s| <= 0.172, and the odd
	// series in s converges by a factor s^2 < 0.03 per term.
	let s = (m - 1.0) / (m + 1.0);
	let s2 = s * s;
	let mut sum = 0.0;
	let mut k = 27.0;
	while k > 1.0 {
		sum = sum * s2 + 1.0 / k;
		k -= 2.0;
	}
	let ln_m = 2.0 * s * (1.0 + sum * s2);
	e as f64 + ln_m * LOG2_E
}

pub(crate) fn log10(x: f64) -> f64 {
	log2(x) * LOG10_2
}

/// sin(x) and cos(x) together.
pub(crate) fn sin_cos(x: f64) -> (f64, f64) {
	if x.is_nan() || abs(x) >= 1.0e9 {
		return (f64::NAN, f64::NAN);
	}
	// x = q pi/2 + r with |r| <= pi/4. pi/2 is split in three parts so that q times each of the
	// first two is exact for the |q| the codec ever uses (the first part has 33 significant bits).
	const P1: f64 = 1.5707963267341256;
	const P2: f64 = 6.077100506303966e-11;
	const P3: f64 = 2.0222662487959506e-21;
	let q = round(x * (2.0 / PI));
	let r = ((x - q * P1) - q * P2) - q * P3;
	let r2 = r * r;
	// Taylor series to r^19 and r^18: with |r| <= 0.786 the remainders are below 1e-19.
	let mut s = 0.0;
	let mut c = 0.0;
	let mut k = 19.0;
	while k > 1.0 {
		s = 1.0 - s * r2 / (k * (k - 1.0));
		c = 1.0 - c * r2 / ((k - 1.0) * (k - 2.0));
		k -= 2.0;
	}
	let s = r * s;
	match (q as i64) & 3 {
		0 => (s, c),
		1 => (c, -s),
		2 => (-s, -c),
		_ => (-c, s),
	}
}

pub(crate) fn sin(x: f64) -> f64 {
	sin_cos(x).0
}

/// arcsin on [-1, 1]; arguments outside are clamped (the codec only passes reflection
/// coefficients, which are inside).
pub(crate) fn asin(x: f64) -> f64 {
	if x.is_nan() {
		return x;
	}
	let a = min(abs(x), 1.0);
	let r = if a <= 0.5 {
		asin_series(a)
	} else {
		// asin(a) = pi/2 - 2 asin(sqrt((1 - a) / 2)), whose argument is at most 1/2.
		FRAC_PI_2 - 2.0 * asin_series(sqrt((1.0 - a) * 0.5))
	};
	if x < 0.0 { -r } else { r }
}

/// The Maclaurin series of arcsin for 0 <= a <= 1/2 (ratio of consecutive terms below 1/4).
fn asin_series(a: f64) -> f64 {
	let a2 = a * a;
	let mut term = a;
	let mut sum = a;
	let mut n = 0.0;
	while n < 60.0 {
		// term(n+1) = term(n) a^2 (2n+1)^2 / ((2n+2)(2n+3)), counting the 1/(2n+1) factor in.
		term *= a2 * (2.0 * n + 1.0) * (2.0 * n + 1.0) / ((2.0 * n + 2.0) * (2.0 * n + 3.0));
		sum += term;
		if term < 1.0e-18 * sum {
			break;
		}
		n += 1.0;
	}
	sum
}

#[cfg(test)]
mod tests {
	extern crate std;
	use super::*;

	fn close(a: f64, b: f64, rel: f64) -> bool {
		abs(a - b) <= rel * max(1.0e-300, max(abs(a), abs(b)))
	}

	#[test]
	fn rounding() {
		assert_eq!(round(-4.5), -5.0);
		assert_eq!(round(-3.2), -3.0);
		assert_eq!(round(3.2), 3.0);
		assert_eq!(round(4.5), 5.0);
		assert_eq!(round(0.49999999999999994), 0.0);
		assert_eq!(floor(-4.5), -5.0);
		assert_eq!(floor(-3.2), -4.0);
		assert_eq!(floor(3.2), 3.0);
		assert_eq!(floor(4.5), 4.0);
		assert_eq!(floor(-0.0), 0.0);
		assert_eq!(ceil(-4.5), -4.0);
		assert_eq!(ceil(-3.2), -3.0);
		assert_eq!(ceil(3.2), 4.0);
		assert_eq!(ceil(4.5), 5.0);
		assert_eq!(floor(1.0e300), 1.0e300);
		assert_eq!(nint(-0.5), -1);
	}

	#[test]
	fn square_root() {
		for i in 0..2000 {
			let x = exp2((i as f64) * 0.1 - 100.0);
			let r = sqrt(x);
			// Within one unit in the last place of the correctly rounded root.
			assert!(close(r, std::primitive::f64::sqrt(x), 2.3e-16), "sqrt({x}) = {r}");
		}
		assert_eq!(sqrt(0.0), 0.0);
		assert_eq!(sqrt(4.0), 2.0);
		assert!(sqrt(-1.0).is_nan());
		assert!(close(sqrt(1.0e-310), std::primitive::f64::sqrt(1.0e-310), 2.3e-16));
	}

	#[test]
	fn exponentials_and_logarithms() {
		// Against the host's libm, which is correctly rounded or within an ulp.
		let mut x = -60.0;
		while x < 60.0 {
			assert!(close(exp2(x), std::primitive::f64::exp2(x), 4.0e-16), "exp2({x})");
			assert!(close(pow10(x / 4.0), std::primitive::f64::powf(10.0, x / 4.0), 1.0e-14), "pow10({x})");
			assert!(close(exp(x / 8.0), std::primitive::f64::exp(x / 8.0), 4.0e-16), "exp({x})");
			x += 0.0137;
		}
		let mut x = 1.0e-40;
		while x < 1.0e40 {
			assert!(abs(log2(x) - std::primitive::f64::log2(x)) <= 4.0e-16 * max(1.0, abs(log2(x))), "log2({x})");
			assert!(abs(log10(x) - std::primitive::f64::log10(x)) <= 4.0e-16 * max(1.0, abs(log10(x))), "log10({x})");
			// Round trips.
			assert!(close(exp2(log2(x)), x, 2.0e-14), "exp2(log2({x}))");
			x *= 1.37;
		}
		assert_eq!(log2(1.0), 0.0);
		assert_eq!(log2(1024.0), 10.0);
		assert_eq!(exp2(10.0), 1024.0);
		assert_eq!(exp2(-1074.0), 5.0e-324);
		assert_eq!(log2(0.0), f64::NEG_INFINITY);
		assert!(close(log2(5.0e-324), -1074.0, 1.0e-15));
		assert!(log2(-1.0).is_nan());
	}

	#[test]
	fn trigonometry() {
		let mut x = -300.0;
		while x < 300.0 {
			let (s, c) = sin_cos(x);
			assert!(abs(s - std::primitive::f64::sin(x)) < 2.0e-16 * max(1.0, abs(x)), "sin({x})");
			assert!(abs(c - std::primitive::f64::cos(x)) < 2.0e-16 * max(1.0, abs(x)), "cos({x})");
			// Pythagoras and the double angle identities hold to rounding.
			assert!(abs(s * s + c * c - 1.0) < 4.0e-16);
			let (s2, c2) = sin_cos(2.0 * x);
			assert!(abs(s2 - 2.0 * s * c) < 1.0e-15 * max(1.0, abs(x)));
			assert!(abs(c2 - (c * c - s * s)) < 1.0e-15 * max(1.0, abs(x)));
			x += 0.01234;
		}
		assert_eq!(sin(0.0), 0.0);
		assert_eq!(sin_cos(0.0).1, 1.0);
		assert!(abs(sin(PI)) < 2.0e-16);
		assert!(abs(sin_cos(PI / 3.0).1 - 0.5) < 2.0e-16);
		let mut x = -1.0;
		while x <= 1.0 {
			let a = asin(x);
			assert!(abs(a - std::primitive::f64::asin(x)) < 1.0e-15, "asin({x})");
			assert!(abs(sin(a) - x) < 1.0e-15);
			x += 0.000731;
		}
		assert_eq!(asin(1.0), FRAC_PI_2);
		assert_eq!(asin(-1.0), -FRAC_PI_2);
	}
}
