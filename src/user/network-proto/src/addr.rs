//! Hand-written helpers on the generated network wire types.

use crate::generated::liber::network::v1::Ipv4Addr;

impl Ipv4Addr {
	pub fn parse(s: &[u8]) -> Option<Ipv4Addr> {
		let mut octets: [u8; 4] = [0u8; 4];
		let mut idx: usize = 0;
		let mut val: u32 = 0;
		let mut digits: usize = 0;
		for &byte in s {
			if byte == b'.' {
				if digits == 0 || idx >= 3 {
					return None;
				}
				octets[idx] = val as u8;
				idx += 1;
				val = 0;
				digits = 0;
			} else if byte.is_ascii_digit() {
				val = val * 10 + u32::from(byte - b'0');
				if val > 255 {
					return None;
				}
				digits += 1;
			} else {
				return None;
			}
		}
		if digits == 0 || idx != 3 {
			return None;
		}
		octets[3] = val as u8;
		Some(Ipv4Addr { a: octets[0], b: octets[1], c: octets[2], d: octets[3] })
	}

	pub fn render(&self, out: &mut [u8]) -> usize {
		let octets: [u8; 4] = [self.a, self.b, self.c, self.d];
		let mut pos: usize = 0;
		let mut i: usize = 0;
		while i < 4 {
			if i > 0 {
				out[pos] = b'.';
				pos += 1;
			}
			pos += write_dec(octets[i], &mut out[pos..]);
			i += 1;
		}
		pos
	}
}

pub fn write_mac(mac: &[u8], out: &mut [u8]) -> usize {
	let mut pos: usize = 0;
	let mut i: usize = 0;
	while i < mac.len() {
		if i > 0 {
			out[pos] = b':';
			pos += 1;
		}
		out[pos] = hex_digit(mac[i] >> 4);
		out[pos + 1] = hex_digit(mac[i] & 0x0f);
		pos += 2;
		i += 1;
	}
	pos
}

fn write_dec(n: u8, out: &mut [u8]) -> usize {
	if n >= 100 {
		out[0] = b'0' + n / 100;
		out[1] = b'0' + (n / 10) % 10;
		out[2] = b'0' + n % 10;
		3
	} else if n >= 10 {
		out[0] = b'0' + n / 10;
		out[1] = b'0' + n % 10;
		2
	} else {
		out[0] = b'0' + n;
		1
	}
}

fn hex_digit(n: u8) -> u8 {
	if n < 10 {
		b'0' + n
	} else {
		b'a' + (n - 10)
	}
}
