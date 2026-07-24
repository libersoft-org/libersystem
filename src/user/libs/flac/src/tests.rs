use super::*;

#[test]
fn validates_checksums_and_utf8_numbers() {
	assert_eq!(crc8(b"123456789"), 0xf4);
	assert_eq!(crc16(b"123456789"), 0xfee8);
	assert_eq!(read_utf8_number(&[0x7f]), Ok((127, 1)));
	assert_eq!(read_utf8_number(&[0xc2, 0x80]), Ok((128, 2)));
	assert_eq!(read_utf8_number(&[0xc0, 0x80]), Err(Error::Invalid));
}

#[test]
fn rejects_bad_stream_info_bounds() {
	assert!(matches!(Flac::parse(b"fLaC"), Err(Error::Truncated)));
	let mut bytes = alloc::vec![b'f', b'L', b'a', b'C', 0x80, 0, 0, 34];
	bytes.resize(42, 0);
	assert!(matches!(Flac::parse(&bytes), Err(Error::Invalid)));
}
