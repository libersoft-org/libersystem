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
}
