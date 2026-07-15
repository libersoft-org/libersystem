#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub const MAX_INPUT: usize = 64 * 1024 * 1024;
pub const MAX_OUTPUT: usize = MAX_INPUT + MAX_INPUT / 8 + 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Invalid,
	TooLarge,
}

pub fn zlib(data: &[u8], compression: u8) -> Result<Vec<u8>, Error> {
	if compression > 100 {
		return Err(Error::Invalid);
	}
	if data.len() > MAX_INPUT {
		return Err(Error::TooLarge);
	}
	let level = ((compression as u16 * 10 + 50) / 100) as u8;
	let output = miniz_oxide::deflate::compress_to_vec_zlib(data, level);
	if output.len() > MAX_OUTPUT {
		return Err(Error::TooLarge);
	}
	Ok(output)
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
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
}
