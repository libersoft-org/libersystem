use super::*;

#[test]
fn test_compute_bitreverse() {
	let br = compute_bitreverse(8);
	// The output was generated from the output of the
	// original stb_vorbis function.
	let cmp_arr = &[0, 64, 32, 96, 16, 80, 48, 112, 8, 72, 40, 104, 24, 88, 56, 120, 4, 68, 36, 100, 20, 84, 52, 116, 12, 76, 44, 108, 28, 92, 60, 124];
	assert_eq!(br, cmp_arr);
}
