use super::*;

#[test]
fn test_read_hdr_begin() {
	// Only tests flawed header begins, correct headers
	// are tested later by the test methods for the headers

	// Flawed ident header (see char before the /**/)
	let test_arr = &[
		0x01,
		0x76,
		0x6f,
		0x72,
		0x62,
		0x69,
		0x72,
		/**/ 0x00,
		0x00,
		0x00,
		0x00,
		0x02,
		0x44,
		0xac,
		0x00,
		0x00,
		0x00,
		0x00,
		0x00,
		0x00,
		0x80,
		0xb5,
		0x01,
		0x00,
		0x00,
		0x00,
		0x00,
		0x00,
		0xb8,
		0x01,
	];
	let mut rdr: BitpackCursor = BitpackCursor::new(test_arr);
	assert_eq!(read_header_begin(&mut rdr), Err(HeaderReadError::NotVorbisHeader));
}

#[test]
fn test_read_header_ident() {
	// Valid ident header
	let test_arr = &[
		0x01,
		0x76,
		0x6f,
		0x72,
		0x62,
		0x69,
		0x73,
		0x00,
		0x00,
		0x00,
		0x00,
		0x02,
		0x44,
		0xac,
		0x00,
		0x00,
		0x00,
		0x00,
		0x00,
		0x00,
		0x80,
		0xb5,
		0x01,
		0x00,
		0x00,
		0x00,
		0x00,
		0x00,
		0xb8,
		0x01,
	];
	let hdr = read_header_ident(test_arr).unwrap();
	assert_eq!(hdr.audio_channels, 2);
	assert_eq!(hdr.audio_sample_rate, 0x0000ac44);
	assert_eq!(hdr.bitrate_maximum, 0);
	assert_eq!(hdr.bitrate_nominal, 0x0001b580);
	assert_eq!(hdr.bitrate_minimum, 0);
	assert_eq!(hdr.blocksize_0, 8);
	assert_eq!(hdr.blocksize_1, 11);
}

#[test]
fn test_lookup1_values() {
	// First, with base two:
	// 2 ^ 10 = 1024
	assert_eq!(lookup1_values(1025, 10), 2);
	assert_eq!(lookup1_values(1024, 10), 2);
	assert_eq!(lookup1_values(1023, 10), 1);

	// Now, the searched base is five:
	// 5 ^ 5 = 3125
	assert_eq!(lookup1_values(3126, 5), 5);
	assert_eq!(lookup1_values(3125, 5), 5);
	assert_eq!(lookup1_values(3124, 5), 4);

	// Now some exotic tests (edge cases :p):
	assert_eq!(lookup1_values(1, 1), 1);
	assert_eq!(lookup1_values(0, 15), 0);
	assert_eq!(lookup1_values(0, 0), 0);
	assert_eq!(lookup1_values(1, 0), u32::MAX);
	assert_eq!(lookup1_values(400, 0), u32::MAX);
}
