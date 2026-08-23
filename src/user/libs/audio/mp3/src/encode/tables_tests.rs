// The tables are DATA, recovered mechanically, and these are the checks that make that trustworthy
// without a second reference to compare against.
//
// A Huffman table is valid exactly when it is a prefix code and its lengths satisfy Kraft's
// equality - `sum(2^-len) == 1` over the whole table. Together those catch a mistyped length, a
// duplicated codeword and a missing entry. What they cannot catch is a PERMUTATION of correct
// codewords, which is why the end-to-end test beside them encodes a signal and decodes it back:
// a permuted table produces a stream that decodes into noise.

use super::tables::{CODE_TABLES, COUNT1_A, COUNT1_B, SCALEFACTOR_BANDS, TABLE_SELECT};

// `sum(2^-len)` as an exact integer over a common denominator, so the comparison is not a float
// comparison. The longest codeword in this format is nineteen bits.
fn kraft_numerator(lengths: impl Iterator<Item = u8>) -> u64 {
	lengths.map(|len| 1u64 << (32 - len as u32)).sum()
}

const KRAFT_ONE: u64 = 1u64 << 32;

fn is_prefix_free(codes: &[(u8, u32)]) -> bool {
	for (i, &(a_len, a_code)) in codes.iter().enumerate() {
		for &(b_len, b_code) in codes.iter().skip(i + 1) {
			let (short, short_code, long_code) = if a_len <= b_len { (a_len, a_code, b_code >> (b_len - a_len)) } else { (b_len, b_code, a_code >> (a_len - b_len)) };
			let _ = short;
			if short_code == long_code {
				return false;
			}
		}
	}
	true
}

#[test]
fn every_code_table_is_a_prefix_code_that_uses_all_of_its_space() {
	// Table 0 is the empty one: a single entry of zero length, which codes nothing and is what the
	// two unused `table_select` values resolve to.
	assert_eq!(CODE_TABLES[0].codes, &[(0u8, 0u32)]);
	for (index, table) in CODE_TABLES.iter().enumerate().skip(1) {
		let xlen = table.xlen as usize;
		assert_eq!(table.codes.len(), xlen * xlen, "table {index} is not square");
		assert_eq!(kraft_numerator(table.codes.iter().map(|&(len, _)| len)), KRAFT_ONE, "table {index} does not satisfy Kraft's equality");
		assert!(is_prefix_free(table.codes), "table {index} has a codeword that prefixes another");
		for &(len, code) in table.codes {
			assert!((1..=19).contains(&len), "table {index} has a {len}-bit codeword");
			assert!(code < (1u32 << len), "table {index} has a codeword wider than its length");
		}
	}
}

#[test]
fn the_tables_have_the_dimensions_the_format_names() {
	// 2x2 for one table, 3x3 for two, 4x4 for two, 6x6 for three, 8x8 for three and 16x16 for four,
	// plus the empty one. Sixteen distinct code tables is what MPEG-1 Layer III has, and a
	// recovery that produced a seventeenth or lost one would fail here.
	let mut shape: alloc::vec::Vec<u8> = CODE_TABLES.iter().map(|table| table.xlen).collect();
	shape.sort_unstable();
	assert_eq!(shape, alloc::vec![1, 2, 3, 3, 4, 4, 6, 6, 6, 8, 8, 8, 16, 16, 16, 16]);
}

#[test]
fn table_select_resolves_the_way_the_format_numbers_it() {
	// Four and fourteen are the two values the standard leaves unused, and they resolve to the
	// empty table. Sixteen through twenty-three share one set of codes at rising escape widths, and
	// twenty-four through thirty-one share another - which is what lets a 16x16 table carry any
	// magnitude at all.
	assert_eq!(TABLE_SELECT[4], TABLE_SELECT[0]);
	assert_eq!(TABLE_SELECT[14], TABLE_SELECT[0]);
	assert_eq!(TABLE_SELECT[0].1, 0, "the empty table has no escape bits");
	let escape_a: alloc::vec::Vec<u8> = (16..24).map(|t| TABLE_SELECT[t].1).collect();
	assert_eq!(escape_a, alloc::vec![1, 2, 3, 4, 6, 8, 10, 13]);
	let escape_b: alloc::vec::Vec<u8> = (24..32).map(|t| TABLE_SELECT[t].1).collect();
	assert_eq!(escape_b, alloc::vec![4, 5, 6, 7, 8, 9, 11, 13]);
	for t in 16..24 {
		assert_eq!(TABLE_SELECT[t].0, TABLE_SELECT[16].0, "the escape tables share one set of codes");
		assert_eq!(CODE_TABLES[TABLE_SELECT[t].0 as usize].xlen, 16);
	}
	for t in 24..32 {
		assert_eq!(TABLE_SELECT[t].0, TABLE_SELECT[24].0);
		assert_eq!(CODE_TABLES[TABLE_SELECT[t].0 as usize].xlen, 16);
	}
}

#[test]
fn both_quadruple_tables_are_prefix_codes_and_table_b_is_four_bits_flat() {
	for (name, table) in [("A", &COUNT1_A), ("B", &COUNT1_B)] {
		assert_eq!(kraft_numerator(table.iter().map(|&(len, _)| len)), KRAFT_ONE, "count1 table {name} does not satisfy Kraft's equality");
		assert!(is_prefix_free(table), "count1 table {name} has a codeword that prefixes another");
	}
	// Table B is the one with no Huffman structure at all - four bits for every quadruple - which
	// is what makes it the cheaper choice where most quadruples are dense.
	assert!(COUNT1_B.iter().all(|&(len, _)| len == 4));
	assert!(COUNT1_A.iter().any(|&(len, _)| len != 4), "count1 table A is not a flat code");
}

#[test]
fn the_scalefactor_bands_cover_the_whole_granule_and_only_rise() {
	for (index, bands) in SCALEFACTOR_BANDS.iter().enumerate() {
		assert_eq!(bands[0], 0, "rate {index} does not start at line zero");
		assert_eq!(bands[22], 576, "rate {index} does not end at line 576");
		for pair in bands.windows(2) {
			assert!(pair[1] > pair[0], "rate {index} has a band that does not advance");
		}
	}
	// The three differ, and they differ where the format says they do: 44.1 and 32 kHz share their
	// first nine boundaries and part company after that, while 48 kHz is narrower from band nine on.
	assert_ne!(SCALEFACTOR_BANDS[0], SCALEFACTOR_BANDS[1]);
	assert_ne!(SCALEFACTOR_BANDS[0], SCALEFACTOR_BANDS[2]);
	assert_eq!(SCALEFACTOR_BANDS[0][..9], SCALEFACTOR_BANDS[2][..9]);
}
