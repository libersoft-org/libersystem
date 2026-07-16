const COS_PI_8_MINUS_ONE: i64 = 20_091;
const SIN_PI_8: i64 = 35_468;
const FORWARD_C1: i64 = 2_217;
const FORWARD_C2: i64 = 5_352;

pub(super) fn forward(block: &mut [i32; 16]) {
	for row in 0..4usize {
		let base = row * 4;
		let sum_outer = (i64::from(block[base]) + i64::from(block[base + 3])) * 8;
		let sum_inner = (i64::from(block[base + 1]) + i64::from(block[base + 2])) * 8;
		let difference_inner = (i64::from(block[base + 1]) - i64::from(block[base + 2])) * 8;
		let difference_outer = (i64::from(block[base]) - i64::from(block[base + 3])) * 8;
		block[base] = (sum_outer + sum_inner) as i32;
		block[base + 2] = (sum_outer - sum_inner) as i32;
		block[base + 1] = ((difference_inner * FORWARD_C1 + difference_outer * FORWARD_C2 + 14_500) >> 12) as i32;
		block[base + 3] = ((difference_outer * FORWARD_C1 - difference_inner * FORWARD_C2 + 7_500) >> 12) as i32;
	}

	for column in 0..4usize {
		let sum_outer = i64::from(block[column]) + i64::from(block[12 + column]);
		let sum_inner = i64::from(block[4 + column]) + i64::from(block[8 + column]);
		let difference_inner = i64::from(block[4 + column]) - i64::from(block[8 + column]);
		let difference_outer = i64::from(block[column]) - i64::from(block[12 + column]);
		block[column] = ((sum_outer + sum_inner + 7) >> 4) as i32;
		block[8 + column] = ((sum_outer - sum_inner + 7) >> 4) as i32;
		block[4 + column] = (((difference_inner * FORWARD_C1 + difference_outer * FORWARD_C2 + 12_000) >> 16) + i64::from(difference_outer != 0)) as i32;
		block[12 + column] = ((difference_outer * FORWARD_C1 - difference_inner * FORWARD_C2 + 51_000) >> 16) as i32;
	}
}

pub(super) fn inverse(block: &mut [i32; 16]) {
	for column in 0..4usize {
		let even_sum = i64::from(block[column]) + i64::from(block[8 + column]);
		let even_difference = i64::from(block[column]) - i64::from(block[8 + column]);
		let odd_a = (i64::from(block[4 + column]) * SIN_PI_8) >> 16;
		let odd_b = i64::from(block[12 + column]) + ((i64::from(block[12 + column]) * COS_PI_8_MINUS_ONE) >> 16);
		let odd_difference = odd_a - odd_b;
		let odd_c = i64::from(block[4 + column]) + ((i64::from(block[4 + column]) * COS_PI_8_MINUS_ONE) >> 16);
		let odd_d = (i64::from(block[12 + column]) * SIN_PI_8) >> 16;
		let odd_sum = odd_c + odd_d;
		block[column] = (even_sum + odd_sum) as i32;
		block[4 + column] = (even_difference + odd_difference) as i32;
		block[8 + column] = (even_difference - odd_difference) as i32;
		block[12 + column] = (even_sum - odd_sum) as i32;
	}

	for row in 0..4usize {
		let base = row * 4;
		let even_sum = i64::from(block[base]) + i64::from(block[base + 2]);
		let even_difference = i64::from(block[base]) - i64::from(block[base + 2]);
		let odd_a = (i64::from(block[base + 1]) * SIN_PI_8) >> 16;
		let odd_b = i64::from(block[base + 3]) + ((i64::from(block[base + 3]) * COS_PI_8_MINUS_ONE) >> 16);
		let odd_difference = odd_a - odd_b;
		let odd_c = i64::from(block[base + 1]) + ((i64::from(block[base + 1]) * COS_PI_8_MINUS_ONE) >> 16);
		let odd_d = (i64::from(block[base + 3]) * SIN_PI_8) >> 16;
		let odd_sum = odd_c + odd_d;
		block[base] = ((even_sum + odd_sum + 4) >> 3) as i32;
		block[base + 1] = ((even_difference + odd_difference + 4) >> 3) as i32;
		block[base + 2] = ((even_difference - odd_difference + 4) >> 3) as i32;
		block[base + 3] = ((even_sum - odd_sum + 4) >> 3) as i32;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn forward_and_inverse_preserve_an_unquantized_block() {
		let source = [38, 6, 210, 107, 42, 125, 185, 151, 241, 224, 125, 233, 227, 8, 57, 96];
		let mut transformed = source;
		forward(&mut transformed);
		inverse(&mut transformed);
		assert_eq!(transformed, source);
	}
}
