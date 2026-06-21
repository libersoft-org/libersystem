//! Hand-written helpers on the generated wire types.
//!
//! The canonical `Ipv4Addr` parses and renders itself, so every program - the
//! shell and each spawned net tool - shares one parse/format without a bespoke
//! networking library: the typed object is the shared thing, and it lives here in
//! `proto` alongside the generated bindings.

use crate::system::Ipv4Addr;

impl Ipv4Addr {
	// Parse a dotted-decimal address ("10.0.2.2") into its four octets, or None if
	// malformed: the wrong number of fields, a non-digit, an empty field, or an octet
	// greater than 255.
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

	// Render the address in dotted-decimal form ("10.0.2.15") into `out`, returning
	// the number of bytes written (at most 15). `out` must be at least 15 bytes.
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

// Render a MAC address (any byte length, typically 6) as colon-separated lowercase
// hex ("52:54:00:12:34:56") into `out`, returning the number of bytes written. `out`
// must hold at least 3 * mac.len() - 1 bytes. MAC has no typed wire object (it rides
// as list<u8>), so this is a free helper rather than a method.
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

// The decimal digits of `n` (0-255) into `out`, returning the count (1-3).
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

// A single lowercase hex digit for `n` (the low nibble; 0-15).
fn hex_digit(n: u8) -> u8 {
	if n < 10 { b'0' + n } else { b'a' + (n - 10) }
}
