use super::*;

#[test]
fn rejects_compact_oversized_header_allocations() {
	let mut setup = b"\x05vorbis\x00BCV\x01\x00".to_vec();
	setup.extend_from_slice(&[1, 0, 4]);
	assert!(matches!(header::read_header_setup(&setup, 1, (6, 6)), Err(header::HeaderReadError::BufferNotAddressable)));

	let mut comments = b"\x03vorbis\x00\x00\x00\x00".to_vec();
	comments.extend_from_slice(&4_097u32.to_le_bytes());
	assert!(matches!(header::read_header_comment(&comments), Err(header::HeaderReadError::BufferNotAddressable)));
}
