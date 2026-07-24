use super::*;
use alloc::vec;

#[test]
fn compression_endpoints_round_trip_identically() {
	let mut input = vec![0u8; 32_768];
	for (index, byte) in input.iter_mut().enumerate() {
		*byte = ((index / 19) ^ (index * 31)) as u8;
	}
	let fast = zlib(&input, 0).unwrap();
	let compact = zlib(&input, 100).unwrap();
	assert_eq!(inflate::zlib(&fast, input.len()).unwrap(), input);
	assert_eq!(inflate::zlib(&compact, input.len()).unwrap(), input);
	assert!(compact.len() <= fast.len());
}

#[test]
fn rejects_invalid_effort_and_oversized_input() {
	assert_eq!(zlib(&[], 101), Err(Error::Invalid));
	assert_eq!(zlib(&vec![0; MAX_INPUT + 1], 50), Err(Error::TooLarge));
}
