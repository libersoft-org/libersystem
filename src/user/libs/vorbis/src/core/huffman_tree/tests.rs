use super::*;

#[cfg(test)]
impl VorbisHuffmanTree {
	fn iter_test(&self, path: u32, path_len: u8, expected_val: u32) {
		let mut itr = self.iter();
		for i in 1..path_len {
			assert_eq!(itr.next((path & (1 << (path_len - i))) != 0), None);
		}
		assert_eq!(itr.next((path & 1) != 0), Some(expected_val));
	}
}

#[test]
fn test_huffman_tree() {
	// Official example from the vorbis spec section 3.2.1
	let tree = VorbisHuffmanTree::load_from_array(&[2, 4, 4, 4, 4, 2, 3, 3]).unwrap();

	tree.iter_test(0b00, 2, 0);
	tree.iter_test(0b0100, 4, 1);
	tree.iter_test(0b0101, 4, 2);
	tree.iter_test(0b0110, 4, 3);
	tree.iter_test(0b0111, 4, 4);
	tree.iter_test(0b10, 2, 5);
	tree.iter_test(0b110, 3, 6);
	tree.iter_test(0b111, 3, 7);

	// Some other example
	// we mostly test the length (max 32) here
	VorbisHuffmanTree::load_from_array(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 32]).unwrap();
}

#[test]
fn test_issue_8() {
	// regression test for issue 8
	// make sure that it doesn't panic.
	let _ = VorbisHuffmanTree::load_from_array(&[0; 625]);
}

#[test]
fn test_under_over_spec() {
	// All trees base on the official example from the vorbis spec section 3.2.1
	// but with modifications to under- or overspecify them

	// underspecified
	let tree = VorbisHuffmanTree::load_from_array(&[2, 4, 4, 4, 4, 2, 3 /*, 3*/]);
	assert!(tree.is_err());

	// underspecified
	let tree = VorbisHuffmanTree::load_from_array(&[2, 4, 4, 4, /*4,*/ 2, 3, 3]);
	assert!(tree.is_err());

	// overspecified
	let tree = VorbisHuffmanTree::load_from_array(&[2, 4, 4, 4, 4, 2, 3, 3 /*]*/, 3]);
	assert!(tree.is_err());
}

#[test]
fn test_single_entry_huffman_tree() {
	// Special testing for single entry codebooks, as required by the vorbis spec
	let tree = VorbisHuffmanTree::load_from_array(&[1]).unwrap();
	tree.iter_test(0b0, 1, 0);
	tree.iter_test(0b1, 1, 0);

	let tree = VorbisHuffmanTree::load_from_array(&[0, 0, 1, 0]).unwrap();
	tree.iter_test(0b0, 1, 2);
	tree.iter_test(0b1, 1, 2);

	let tree = VorbisHuffmanTree::load_from_array(&[2]);
	assert!(tree.is_err());
}

#[test]
fn test_unordered_huffman_tree() {
	// Reordered the official example from the vorbis spec section 3.2.1
	//
	// Ensuring that unordered huffman trees work as well is important
	// because the spec does not disallow them, and unordered
	// huffman trees appear in "the wild".
	let tree = VorbisHuffmanTree::load_from_array(&[2, 4, 4, 2, 4, 4, 3, 3]).unwrap();

	tree.iter_test(0b00, 2, 0);
	tree.iter_test(0b0100, 4, 1);
	tree.iter_test(0b0101, 4, 2);
	tree.iter_test(0b10, 2, 3);
	tree.iter_test(0b0110, 4, 4);
	tree.iter_test(0b0111, 4, 5);
	tree.iter_test(0b110, 3, 6);
	tree.iter_test(0b111, 3, 7);
}

#[test]
fn test_extracted_huffman_tree() {
	// Extracted from a real-life vorbis file.
	VorbisHuffmanTree::load_from_array(&[
		5,
		6,
		11,
		11,
		11,
		11,
		10,
		10,
		12,
		11,
		5,
		2,
		11,
		5,
		6,
		6,
		7,
		9,
		11,
		13,
		13,
		10,
		7,
		11,
		6,
		7,
		8,
		9,
		10,
		12,
		11,
		5,
		11,
		6,
		8,
		7,
		9,
		11,
		14,
		15,
		11,
		6,
		6,
		8,
		4,
		5,
		7,
		8,
		10,
		13,
		10,
		5,
		7,
		7,
		5,
		5,
		6,
		8,
		10,
		11,
		10,
		7,
		7,
		8,
		6,
		5,
		5,
		7,
		9,
		9,
		11,
		8,
		8,
		11,
		8,
		7,
		6,
		6,
		7,
		9,
		12,
		11,
		10,
		13,
		9,
		9,
		7,
		7,
		7,
		9,
		11,
		13,
		12,
		15,
		12,
		11,
		9,
		8,
		8,
		8,
	])
	.unwrap();
}
