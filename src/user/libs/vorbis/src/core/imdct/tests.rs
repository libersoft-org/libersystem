use super::*;

#[cfg(test)]
#[test]
fn test_imdct_naive() {
	use crate::imdct_test::*;
	let mut arr_1 = imdct_prepare(&IMDCT_INPUT_TEST_ARR_1);
	let cbd = CachedBlocksizeDerived::from_blocksize(8);
	inverse_mdct_naive(&cbd, &mut arr_1);
	let mismatches = fuzzy_compare_array(&arr_1, &IMDCT_OUTPUT_TEST_ARR_1, 0.00005, true);
	let mismatches_limit = 0;
	if mismatches > mismatches_limit {
		panic!("Numer of mismatches {} was larger than limit of {}", mismatches, mismatches_limit);
	}
}

#[cfg(test)]
#[test]
fn test_imdct() {
	use crate::imdct_test::*;
	let mut arr_1 = imdct_prepare(&IMDCT_INPUT_TEST_ARR_1);
	let blocksize = 8;
	let cbd = CachedBlocksizeDerived::from_blocksize(blocksize);
	inverse_mdct(&cbd, &mut arr_1, blocksize);
	let mismatches = fuzzy_compare_array(&arr_1, &IMDCT_OUTPUT_TEST_ARR_1, 0.00005, true);
	let mismatches_limit = 0;
	if mismatches > mismatches_limit {
		panic!("Numer of mismatches {} was larger than limit of {}", mismatches, mismatches_limit);
	}
}
