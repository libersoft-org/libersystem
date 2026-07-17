use super::*;

#[test]
fn test_sign_extend() {
	assert_eq!(sign_extend!(0b00, i8, 8, 2), 0);
	assert_eq!(sign_extend!(0b01, i8, 8, 2), 1);
	assert_eq!(sign_extend!(0b11, i8, 8, 2), -1);
	assert_eq!(sign_extend!(0b111, i8, 8, 3), -1);
	assert_eq!(sign_extend!(0b101, i8, 8, 3), -3);
	assert_eq!(sign_extend!(0b01111110, i16, 16, 8), 126);
	assert_eq!(sign_extend!(0b10000010, i16, 16, 8), -126);
}

#[test]
fn test_mask_bits() {
	assert_eq!(mask_bits(0), 0b00000000);
	assert_eq!(mask_bits(1), 0b00000001);
	assert_eq!(mask_bits(2), 0b00000011);
	assert_eq!(mask_bits(3), 0b00000111);
	assert_eq!(mask_bits(4), 0b00001111);
	assert_eq!(mask_bits(5), 0b00011111);
	assert_eq!(mask_bits(6), 0b00111111);
	assert_eq!(mask_bits(7), 0b01111111);
	assert_eq!(mask_bits(8), 0b11111111);
}

#[test]
fn test_bmask_bits() {
	assert_eq!(bmask_bits(0), 0b11111111);
	assert_eq!(bmask_bits(1), 0b00000001);
	assert_eq!(bmask_bits(2), 0b00000011);
	assert_eq!(bmask_bits(3), 0b00000111);
	assert_eq!(bmask_bits(4), 0b00001111);
	assert_eq!(bmask_bits(5), 0b00011111);
	assert_eq!(bmask_bits(6), 0b00111111);
	assert_eq!(bmask_bits(7), 0b01111111);
	assert_eq!(bmask_bits(8), 0b11111111);
}

#[test]
fn test_float_32_unpack() {
	// Values were printed out from what stb_vorbis
	// calculated for this function from a test file.
	assert_eq!(float32_unpack(1611661312), 1.000000);
	assert_eq!(float32_unpack(1616117760), 5.000000);
	assert_eq!(float32_unpack(1618345984), 11.000000);
	assert_eq!(float32_unpack(1620115456), 17.000000);
	assert_eq!(float32_unpack(1627381760), 255.000000);
	assert_eq!(float32_unpack(3759144960), -1.000000);
	assert_eq!(float32_unpack(3761242112), -2.000000);
	assert_eq!(float32_unpack(3763339264), -4.000000);
	assert_eq!(float32_unpack(3763601408), -5.000000);
	assert_eq!(float32_unpack(3765436416), -8.000000);
	assert_eq!(float32_unpack(3765829632), -11.000000);
	assert_eq!(float32_unpack(3768451072), -30.000000);
	assert_eq!(float32_unpack(3772628992), -119.000000);
	assert_eq!(float32_unpack(3780634624), -1530.000000);
}

#[test]
fn test_float_32_unpack_issue_24() {
	// Regression test for issue #24, a
	// mismatch in decoded output for audio_simple_with_error.ogg
	// and singlemap-test.ogg.
	// The values are taken from the codebook_delta_value and
	// codebook_minimum_value values of the singlemap-test.ogg file.
	// The expected values come from stb_vorbis.
	assert_eq!(float32_unpack(1628434432), 255.0);
	assert_eq!(float32_unpack(1621655552), 17.0);
	assert_eq!(float32_unpack(1619722240), 11.0);
	assert_eq!(float32_unpack(1613234176), 1.0);
	assert_eq!(float32_unpack(3760717824), -1.0);
	assert_eq!(float32_unpack(3762814976), -2.0);
	assert_eq!(float32_unpack(3764912128), -4.0);
	assert_eq!(float32_unpack(3765043200), -5.0);
	assert_eq!(float32_unpack(3767009280), -8.0);
	assert_eq!(float32_unpack(3767205888), -11.0);
	assert_eq!(float32_unpack(3769565184), -30.0);
	assert_eq!(float32_unpack(3773751296), -119.0);
	assert_eq!(float32_unpack(3781948416), -1530.0);
}

#[test]
fn test_bitpacking_reader_static() {
	// Test vectors taken from Vorbis I spec, section 2.1.6
	let test_arr = &[0b11111100, 0b01001000, 0b11001110, 0b00000110];
	let mut cur = BitpackCursor::new(test_arr);
	assert_eq!(cur.read_u4().unwrap(), 12);
	assert_eq!(cur.read_u3().unwrap(), 7);
	assert_eq!(cur.read_u7().unwrap(), 17);
	assert_eq!(cur.read_u13().unwrap(), 6969);
}

#[test]
fn test_bitpacking_reader_dynamic() {
	// Test vectors taken from Vorbis I spec, section 2.1.6
	let test_arr = &[0b11111100, 0b01001000, 0b11001110, 0b00000110];
	let mut cur = BitpackCursor::new(test_arr);
	assert_eq!(cur.read_dyn_u8(4).unwrap(), 12);
	assert_eq!(cur.read_dyn_u8(3).unwrap(), 7);
	assert_eq!(cur.read_dyn_u16(7).unwrap(), 17);
	assert_eq!(cur.read_dyn_u16(13).unwrap(), 6969);

	// Regression test for bug
	let test_arr = &[93, 92];
	let mut cur = BitpackCursor::new(test_arr);
	assert_eq!(cur.read_dyn_u32(10).unwrap(), 93);
}

#[test]
fn test_bitpacking_reader_empty() {
	// Same as the normal bitpacking test
	// but with some additional empty reads.
	//
	// This is expected to happen by the vorbis spec.
	// For example, the mode_number read in the audio packet
	// decode at first position may be 0 bit long (if there
	// is only one mode, ilog([vorbis_mode_count] - 1) is zero).

	let test_arr = &[0b11111100, 0b01001000, 0b11001110, 0b00000110];
	let mut cur = BitpackCursor::new(test_arr);
	assert_eq!(cur.read_dyn_u8(4).unwrap(), 12);
	assert_eq!(cur.read_dyn_u8(0).unwrap(), 0);
	assert_eq!(cur.read_dyn_u8(0).unwrap(), 0);
	assert_eq!(cur.read_dyn_u8(3).unwrap(), 7);
	assert_eq!(cur.read_dyn_u8(0).unwrap(), 0);
	assert_eq!(cur.read_dyn_u16(7).unwrap(), 17);
	assert_eq!(cur.read_dyn_u16(0).unwrap(), 0);
	assert_eq!(cur.read_dyn_u16(0).unwrap(), 0);
	assert_eq!(cur.read_dyn_u16(13).unwrap(), 6969);
	assert_eq!(cur.read_dyn_u16(0).unwrap(), 0);
}

#[test]
fn test_bitpacking_reader_byte_aligned() {
	// Check that bitpacking readers work with "normal" byte aligned types:
	let test_arr = &[0x00, 0x00, 0x00, 0x00, 0x01];
	let mut cur = BitpackCursor::new(test_arr);
	assert_eq!(cur.read_dyn_u32(32).unwrap(), 0);
	assert_eq!(cur.read_dyn_u8(8).unwrap(), 1);

	// We not just check here whether it works for byte aligned
	// "normal" (non-dynamic) reader methods, we also check
	// whether, after reading first one, then seven bits,
	// it "gets back" to byte alignment (and increases the byte ctr)
	let test_arr = &[0x09, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
	let mut cur = BitpackCursor::new(test_arr);
	assert_eq!(cur.read_u1().unwrap(), 1);
	assert_eq!(cur.read_u7().unwrap(), 4);
	assert_eq!(cur.read_i8().unwrap(), 2);
	assert_eq!(cur.read_u32().unwrap(), 0);
	assert_eq!(cur.read_u8().unwrap(), 1);
}

#[test]
fn test_capture_pattern_nonaligned() {
	// Regression test from test OGG file
	// Tests for proper codebook capture
	// pattern reading.
	//
	// The OGG vorbis capture pattern
	// is a three octet (24 bits) value.
	//
	// The first block tests capture pattern
	// reading in a byte aligned scenario.
	// The actually problematic part was
	// the second block: it tests capture
	// pattern reading in a non-aligned
	// situation.

	let capture_pattern_arr = &[0x42, 0x43, 0x56];
	let mut cur = BitpackCursor::new(capture_pattern_arr);
	assert_eq!(cur.read_u24().unwrap(), 0x564342);

	let test_arr = &[0x28, 0x81, 0xd0, 0x90, 0x55, 0x00, 0x00];
	let mut cur = BitpackCursor::new(test_arr);
	cur.read_u5().unwrap(); // some value we are not interested in
	cur.read_u5().unwrap(); // some value we are not interested in
	assert_eq!(cur.read_u4().unwrap(), 0);
	assert_eq!(cur.read_u24().unwrap(), 0x564342);
	// Ensure that we incremented by only three bytes, not four
	assert_eq!(cur.read_u16().unwrap(), 1);
}
