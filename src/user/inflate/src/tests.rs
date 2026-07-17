use super::*;

fn hex(value: &str) -> Vec<u8> {
	value.as_bytes().chunks_exact(2).map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap()).collect()
}

#[test]
fn inflates_fixed_and_dynamic_huffman_streams() {
	let fixed = hex("78daf3c94c4a2d0aae2c2e49cd5508f07357c8cc4bcb492c4955284b4d2ec92fd253f019951f95a7a13c000138e5b1");
	let fixed_expected = b"LiberSystem PNG inflate vector. ".repeat(20);
	assert_eq!(zlib(&fixed, fixed_expected.len()).unwrap(), fixed_expected);

	let dynamic = hex("78daedc18501c0201004b0d9a850450aec3f0b33f479fc12210060740b4c67053edbb4f641c82e1c4d396bbacaba0b78b279f929469a874966937c648ec61384df2229afcf6c");
	let mut dynamic_expected = Vec::new();
	for index in 0usize..20 {
		dynamic_expected.extend(core::iter::repeat_n(b'A' + index as u8, 1000 / (index + 1)));
	}
	assert_eq!(dynamic_expected.len(), 3590);
	assert_eq!(zlib(&dynamic, dynamic_expected.len()).unwrap(), dynamic_expected);
}

#[test]
fn rejects_bad_adler_and_output_past_the_bound() {
	let mut stream = hex("78daf3c94c4a2d0aae2c2e49cd5508f07357c8cc4bcb492c4955284b4d2ec92fd253f019951f95a7a13c000138e5b1");
	assert_eq!(zlib(&stream, 2), Err(Error::Invalid));
	let last = stream.len() - 1;
	stream[last] ^= 1;
	assert_eq!(zlib(&stream, 640), Err(Error::Invalid));
}
