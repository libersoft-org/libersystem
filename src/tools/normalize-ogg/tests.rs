use super::*;

#[test]
fn normalizes_serial_and_checksum() {
	let mut page = b"OggS\0\x02\0\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\x01\x03abc".to_vec();
	normalize(&mut page).unwrap();
	assert_eq!(&page[14..18], &FIXED_SERIAL.to_le_bytes());
	let stored = u32::from_le_bytes(page[22..26].try_into().unwrap());
	page[22..26].fill(0);
	assert_eq!(stored, ogg_crc(&page));
}
