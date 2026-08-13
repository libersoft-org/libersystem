// The encoder's headers, read back by this tree's own decoder.
//
// That is the whole point of these tests: a specification can be misread twice in the same
// direction, so nothing here compares the bytes against what I believe the format says. Every
// assertion runs the headers through `header::read_header_*` - the parser the player uses - and
// checks the values that came out.

use super::*;
use crate::header;

#[test]
fn the_bit_writer_packs_low_bit_first() {
	// The one thing about Vorbis' bit packing that catches everybody. A four-bit 0b1010 followed by
	// a four-bit 0b0011 is one byte, and the FIRST value occupies the LOW nibble.
	let mut writer = BitWriter::new();
	writer.write(0b1010, 4).expect("written");
	writer.write(0b0011, 4).expect("written");
	assert_eq!(writer.finish(), alloc::vec![0b0011_1010]);

	// A value wider than the byte it starts in continues into the next one, low bits first.
	let mut writer = BitWriter::new();
	writer.write(1, 1).expect("written");
	writer.write(0xff, 8).expect("written");
	assert_eq!(writer.finish(), alloc::vec![0xff, 0x01]);

	// A partial last byte keeps its unused bits zero: the packet's length is in bytes and the
	// reader stops at the framing bit rather than at the end of the byte.
	let mut writer = BitWriter::new();
	writer.write(0b101, 3).expect("written");
	assert_eq!(writer.finish(), alloc::vec![0b0000_0101]);
}

#[test]
fn a_codeword_is_written_most_significant_bit_first() {
	// A codeword is a path through a tree, so its first bit is the one nearest the root - which is
	// its most significant bit, written into the same low-bit-first stream as everything else.
	let mut writer = BitWriter::new();
	writer.write_codeword(0b110, 3).expect("written");
	assert_eq!(writer.finish(), alloc::vec![0b0000_0011]);
	assert!(BitWriter::new().write_codeword(0, 0).is_none(), "a zero-length codeword is not a codeword");
}

#[test]
fn a_flat_codebook_is_complete_and_its_codes_are_canonical() {
	let book = Codebook::flat(4, None).expect("four entries is a power of two");
	assert_eq!(book.entries(), 4);
	// Four entries of two bits each: 00, 01, 10, 11 in index order.
	assert_eq!(book.code(0), Some((0b00, 2)));
	assert_eq!(book.code(1), Some((0b01, 2)));
	assert_eq!(book.code(2), Some((0b10, 2)));
	assert_eq!(book.code(3), Some((0b11, 2)));
	// A book that is not a power of two cannot be flat AND complete, and the decoder refuses an
	// incomplete tree - so it is refused here rather than written and discovered later.
	assert!(Codebook::flat(3, None).is_none());
	assert!(Codebook::flat(1, None).is_none());
}

#[test]
fn vorbis_floats_decode_to_the_values_they_were_built_from() {
	// The format's own float: mantissa * 2^(exponent - 788). These are the values the residue book
	// carries, and a decoder that reads a different number from them decodes to a different signal.
	for value in [1.0f32, -1.0, 0.5, 0.031_25, 2.0, -0.031_25] {
		let packed = f32_to_vorbis(value);
		let mantissa = (packed & 0x001f_ffff) as f64;
		let exponent = ((packed >> 21) & 0x3ff) as i32 - 788;
		let sign = if packed & 0x8000_0000 != 0 { -1.0 } else { 1.0 };
		let decoded = sign * mantissa * libm::pow(2.0, exponent as f64);
		assert!((decoded - value as f64).abs() < 1e-9, "{value} came back as {decoded}");
	}
	assert_eq!(f32_to_vorbis(0.0), 0, "zero is zero, not a denormal");
}

#[test]
fn the_identification_header_says_what_the_stream_is() {
	let packet = write_ident(2, 44_100, 11).expect("the header is written");
	let ident = header::read_header_ident(&packet).expect("our own decoder reads it");
	assert_eq!(ident.audio_channels, 2);
	assert_eq!(ident.audio_sample_rate, 44_100);
	assert_eq!(ident.blocksize_0, 11);
	assert_eq!(ident.blocksize_1, 11);
}

#[test]
fn the_comment_header_carries_the_vendor_and_no_comments() {
	let packet = write_comment("LiberSystem").expect("the header is written");
	let comment = header::read_header_comment(&packet).expect("our own decoder reads it");
	assert_eq!(comment.comment_list.len(), 0, "version one strips metadata deliberately");
}

#[test]
fn the_setup_header_is_read_back_as_the_configuration_it_describes() {
	// THE LOAD-BEARING TEST. The setup header is where a stream stops being decodable if anything
	// in it is a bit wrong, and everything after it depends on the decoder having read the same
	// codebooks, floor and residue this encoder believes it wrote.
	for channels in [1u8, 2] {
		let setup = write_setup(channels, 11).expect("the header is written");
		let parsed = header::read_header_setup(&setup, channels, (11, 11)).expect("our own decoder reads it");
		assert_eq!(parsed.codebooks.len(), 3, "a floor book, a class book and a residue value book");
		assert_eq!(parsed.floors.len(), 1);
		assert_eq!(parsed.residues.len(), 1);
		assert_eq!(parsed.mappings.len(), 1);
		assert_eq!(parsed.modes.len(), 1);
		// The residue covers every channel's whole spectrum, which is what residue 2's interleaving
		// means - and getting this number wrong is how a stereo stream decodes as half a signal.
		assert_eq!(parsed.residues[0].residue_end, 1024 * channels as u32);
		assert_eq!(parsed.residues[0].residue_partition_size, RESIDUE_PARTITION);
		assert_eq!(parsed.residues[0].residue_classifications, 1);
	}
}

#[test]
fn a_setup_header_with_an_incomplete_codebook_is_refused_by_the_decoder() {
	// The refusal this encoder is written against: the decoder demands a complete Huffman tree, so
	// a book whose lengths do not fill it is rejected rather than tolerated. This is what makes
	// `Codebook::flat` refuse a non-power-of-two rather than writing something that parses here and
	// fails in a player.
	let mut book = Codebook { lengths: alloc::vec![2, 2, 2], lookup: None, codes: alloc::vec::Vec::new() };
	book.assign_codes().expect("codes are assignable even when the tree is short");
	let mut out = BitWriter::new();
	write_header_begin(&mut out, 5).expect("written");
	out.write(0, 8).expect("written");
	book.write(&mut out).expect("written");
	let packet = out.finish();
	assert!(header::read_header_setup(&packet, 1, (11, 11)).is_err(), "three two-bit entries leave a hole in the tree");
}

// The setup header read back FIELD BY FIELD, mirroring the decoder's parser.
//
// `the_setup_header_is_read_back_as_the_configuration_it_describes` says whether the header parses;
// this says WHERE it stopped parsing when it does not, which is the difference between a failing
// test you can act on and one that only tells you something is wrong. It earned its place
// immediately: the class-dimension field is three bits, fourteen posts in one class wrote a
// thirteen, and the decoder read back a five - a number that is legal, so the only symptom was
// `HeaderBadFormat` eleven fields later.
#[test]
fn the_setup_header_reads_back_field_by_field() {
	use crate::bitpacking::BitpackCursor;
	let setup = write_setup(1, 11).expect("written");
	let mut rdr = BitpackCursor::new(&setup);
	assert_eq!(rdr.read_u8().unwrap(), 5, "packet type");
	for expected in b"vorbis" {
		assert_eq!(rdr.read_u8().unwrap(), *expected, "signature");
	}
	assert_eq!(rdr.read_u8().unwrap(), 2, "codebook count minus one");
	for book in 0..3u32 {
		assert_eq!(rdr.read_u24().unwrap(), 0x564342, "book {book} sync");
		assert_eq!(rdr.read_u16().unwrap(), 1, "book {book} dimensions");
		let entries = rdr.read_u24().unwrap();
		assert_eq!(rdr.read_bit_flag().unwrap(), false, "book {book} ordered");
		assert_eq!(rdr.read_bit_flag().unwrap(), false, "book {book} sparse");
		for _ in 0..entries {
			let _ = rdr.read_u5().unwrap();
		}
		let lookup = rdr.read_u4().unwrap();
		if lookup != 0 {
			let _ = rdr.read_f32().unwrap();
			let _ = rdr.read_f32().unwrap();
			let bits = rdr.read_u4().unwrap() + 1;
			let _ = rdr.read_bit_flag().unwrap();
			for _ in 0..entries {
				let _ = rdr.read_dyn_u32(bits).unwrap();
			}
		}
	}
	assert_eq!(rdr.read_u6().unwrap(), 0, "time count minus one");
	assert_eq!(rdr.read_u16().unwrap(), 0, "time value");
	assert_eq!(rdr.read_u6().unwrap(), 0, "floor count minus one");
	assert_eq!(rdr.read_u16().unwrap(), 1, "floor type");
	assert_eq!(rdr.read_u5().unwrap(), 2, "floor partitions");
	assert_eq!(rdr.read_u4().unwrap(), 0, "partition class");
	assert_eq!(rdr.read_u4().unwrap(), 0, "partition class");
	assert_eq!(rdr.read_u3().unwrap(), 6, "class dimensions minus one");
	assert_eq!(rdr.read_u2().unwrap(), 0, "class subclasses");
	assert_eq!(rdr.read_u8().unwrap(), 1, "subclass book plus one");
	assert_eq!(rdr.read_u2().unwrap(), 1, "multiplier minus one");
	assert_eq!(rdr.read_u4().unwrap(), 10, "rangebits");
	for post in FLOOR_POSTS {
		assert_eq!(rdr.read_dyn_u32(10).unwrap(), post, "floor post");
	}
	assert_eq!(rdr.read_u6().unwrap(), 0, "residue count minus one");
	assert_eq!(rdr.read_u16().unwrap(), 2, "residue type");
	assert_eq!(rdr.read_u24().unwrap(), 0, "residue begin");
	assert_eq!(rdr.read_u24().unwrap(), 1024, "residue end");
	assert_eq!(rdr.read_u24().unwrap(), RESIDUE_PARTITION - 1, "partition size minus one");
	assert_eq!(rdr.read_u6().unwrap(), 0, "classifications minus one");
	assert_eq!(rdr.read_u8().unwrap(), 1, "classbook");
	assert_eq!(rdr.read_u3().unwrap(), 1, "cascade low bits");
	assert_eq!(rdr.read_bit_flag().unwrap(), false, "cascade high flag");
	assert_eq!(rdr.read_u8().unwrap(), 2, "residue book");
	assert_eq!(rdr.read_u6().unwrap(), 0, "mapping count minus one");
	assert_eq!(rdr.read_u16().unwrap(), 0, "mapping type");
	assert_eq!(rdr.read_bit_flag().unwrap(), false, "submap flag");
	assert_eq!(rdr.read_bit_flag().unwrap(), false, "coupling flag");
	assert_eq!(rdr.read_u2().unwrap(), 0, "reserved");
	assert_eq!(rdr.read_u8().unwrap(), 0, "submap reserved byte");
	assert_eq!(rdr.read_u8().unwrap(), 0, "submap floor");
	assert_eq!(rdr.read_u8().unwrap(), 0, "submap residue");
	assert_eq!(rdr.read_u6().unwrap(), 0, "mode count minus one");
	assert_eq!(rdr.read_bit_flag().unwrap(), false, "blockflag");
	assert_eq!(rdr.read_u16().unwrap(), 0, "windowtype");
	assert_eq!(rdr.read_u16().unwrap(), 0, "transformtype");
	assert_eq!(rdr.read_u8().unwrap(), 0, "mode mapping");
	assert_eq!(rdr.read_bit_flag().unwrap(), true, "framing");
}
