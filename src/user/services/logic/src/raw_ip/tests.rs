use super::*;

// A 28-byte IPv4 datagram: a 20-byte header and an 8-byte ICMP echo.
fn echo() -> [u8; 28] {
	let mut datagram = [0u8; 28];
	datagram[0] = 0x45;
	datagram[2..4].copy_from_slice(&28u16.to_be_bytes());
	datagram[9] = 1;
	datagram
}

#[test]
fn a_datagram_is_taken_for_what_its_header_says() {
	let bytes = echo();
	assert_eq!(datagram_of(&bytes), Ok(&bytes[..]));
	// Trailing bytes past the total length are not part of it.
	let mut padded = bytes.to_vec();
	padded.extend_from_slice(&[0xaa; 4]);
	assert_eq!(datagram_of(&padded), Ok(&bytes[..]));
}

fn datagram_of(bytes: &[u8]) -> Result<&[u8], Refused> {
	datagram(bytes, 1400)
}

#[test]
fn what_is_not_an_ipv4_datagram_is_refused() {
	assert_eq!(datagram_of(&[]), Err(Refused::Malformed));
	assert_eq!(datagram_of(&echo()[..19]), Err(Refused::Malformed), "shorter than a header");
	let mut six = echo();
	six[0] = 0x60;
	assert_eq!(datagram_of(&six), Err(Refused::Unsupported), "IPv6 on an IPv4-only link");
	let mut short_header = echo();
	short_header[0] = 0x44;
	assert_eq!(datagram_of(&short_header), Err(Refused::Malformed), "a header of sixteen bytes");
	let mut long_total = echo();
	long_total[2..4].copy_from_slice(&29u16.to_be_bytes());
	assert_eq!(datagram_of(&long_total), Err(Refused::Malformed), "a total length past what arrived");
	let mut total_inside_header = echo();
	total_inside_header[2..4].copy_from_slice(&16u16.to_be_bytes());
	assert_eq!(datagram_of(&total_inside_header), Err(Refused::Malformed));
	let mut ethernet = echo().to_vec();
	ethernet.insert(0, 0x00);
	assert_eq!(datagram_of(&ethernet), Err(Refused::Malformed), "an Ethernet frame is not a raw-IP datagram");
}

#[test]
fn a_datagram_past_the_mtu_is_refused() {
	let bytes = echo();
	assert_eq!(datagram(&bytes, 28), Ok(&bytes[..]));
	assert_eq!(datagram(&bytes, 27), Err(Refused::Oversized));
}
