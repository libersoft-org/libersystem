use super::*;

#[test]
fn test_low_neighbor() {
	let v = [1, 4, 2, 3, 6, 5];
	// 0 will panic
	assert_eq!(low_neighbor(&v, 1), (0, 1));
	assert_eq!(low_neighbor(&v, 2), (0, 1));
	assert_eq!(low_neighbor(&v, 3), (2, 2));
	assert_eq!(low_neighbor(&v, 4), (1, 4));
	assert_eq!(low_neighbor(&v, 5), (1, 4));
}

#[test]
fn test_high_neighbor() {
	let v = [1, 4, 2, 3, 6, 5];
	// 0, 1 will panic
	assert_eq!(high_neighbor(&v, 2), (1, 4));
	assert_eq!(high_neighbor(&v, 3), (1, 4));
	// 4 will panic
	assert_eq!(high_neighbor(&v, 5), (4, 6));
}

#[test]
fn test_high_neighbor_ex() {
	// Data extracted from example file
	let v = [0, 128, 12, 46, 4, 8, 16, 23, 33, 70, 2, 6, 10, 14, 19, 28, 39, 58, 90];

	// 0, 1 will panic
	assert_eq!(high_neighbor(&v, 2), (1, 128));
	assert_eq!(high_neighbor(&v, 3), (1, 128));
	assert_eq!(high_neighbor(&v, 4), (2, 12));
	assert_eq!(high_neighbor(&v, 5), (2, 12));
	assert_eq!(high_neighbor(&v, 6), (3, 46));
	assert_eq!(high_neighbor(&v, 7), (3, 46));
	assert_eq!(high_neighbor(&v, 8), (3, 46));
	assert_eq!(high_neighbor(&v, 9), (1, 128));
	assert_eq!(high_neighbor(&v, 10), (4, 4));
	assert_eq!(high_neighbor(&v, 11), (5, 8));
	assert_eq!(high_neighbor(&v, 12), (2, 12));
	assert_eq!(high_neighbor(&v, 13), (6, 16));
	assert_eq!(high_neighbor(&v, 14), (7, 23));
	assert_eq!(high_neighbor(&v, 15), (8, 33));
	assert_eq!(high_neighbor(&v, 16), (3, 46));
	assert_eq!(high_neighbor(&v, 17), (9, 70));
	assert_eq!(high_neighbor(&v, 18), (1, 128));
}

#[test]
#[should_panic]
fn test_high_neighbor_panic() {
	high_neighbor(&[1, 4, 3, 2, 6, 5], 4);
}

#[test]
#[should_panic]
fn test_low_neighbor_panic() {
	low_neighbor(&[2, 4, 3, 1, 6, 5], 3);
}

#[test]
fn test_render_point() {
	// Test data taken from real life ogg/vorbis file.
	assert_eq!(render_point(0, 28, 128, 67, 12), 31);
	assert_eq!(render_point(12, 38, 128, 67, 46), 46);
	assert_eq!(render_point(0, 28, 12, 38, 4), 31);
	assert_eq!(render_point(4, 33, 12, 38, 8), 35);
	assert_eq!(render_point(12, 38, 46, 31, 16), 38);
	assert_eq!(render_point(16, 30, 46, 31, 23), 30);
	assert_eq!(render_point(23, 40, 46, 31, 33), 37);
	assert_eq!(render_point(46, 31, 128, 67, 70), 41);
	assert_eq!(render_point(0, 28, 4, 33, 2), 30);
	assert_eq!(render_point(4, 33, 8, 43, 6), 38);
	assert_eq!(render_point(8, 43, 12, 38, 10), 41);
	assert_eq!(render_point(12, 38, 16, 30, 14), 34);
	assert_eq!(render_point(16, 30, 23, 40, 19), 34);
	assert_eq!(render_point(23, 40, 33, 26, 28), 33);
	assert_eq!(render_point(33, 26, 46, 31, 39), 28);
	assert_eq!(render_point(46, 31, 70, 20, 58), 26);
	assert_eq!(render_point(70, 20, 128, 67, 90), 36);
}

#[cfg(test)]
#[test]
fn test_imdct_slow() {
	use crate::imdct_test::*;
	let mut arr_1 = imdct_prepare(&IMDCT_INPUT_TEST_ARR_1);
	inverse_mdct_slow(&mut arr_1);
	let mismatches = fuzzy_compare_array(&arr_1, &IMDCT_OUTPUT_TEST_ARR_1, 0.00005, true);
	let mismatches_limit = 0;
	if mismatches > mismatches_limit {
		panic!("Numer of mismatches {} was larger than limit of {}", mismatches, mismatches_limit);
	}
}
