//! Hand-written helpers on the generated network wire types.
//!
//! TEXT IS A RENDERING, NEVER THE VALUE. Every address on the wire is octets; these functions are
//! the only place a human form is produced or consumed, so there is exactly one answer to "how does
//! this host write an address" and exactly one to "what does it accept".

use crate::generated::liber::network::v1::{IpAddress, Ipv4Addr, Ipv6Addr, MacAddr, ScopedAddress};

impl Ipv4Addr {
	pub fn parse(s: &[u8]) -> Option<Ipv4Addr> {
		let octets = parse_dotted_quad(s)?;
		Some(Ipv4Addr { a: octets[0], b: octets[1], c: octets[2], d: octets[3] })
	}

	pub fn octets(&self) -> [u8; 4] {
		[self.a, self.b, self.c, self.d]
	}

	pub fn from_octets(octets: [u8; 4]) -> Ipv4Addr {
		Ipv4Addr { a: octets[0], b: octets[1], c: octets[2], d: octets[3] }
	}

	pub fn is_unspecified(&self) -> bool {
		self.octets() == [0, 0, 0, 0]
	}

	pub fn render(&self, out: &mut [u8]) -> usize {
		let octets: [u8; 4] = self.octets();
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

// Four decimal octets separated by dots, and nothing else. Leading zeros are REFUSED rather than
// accepted: `010.1.1.1` is octal in some resolvers and decimal in others, and an address that means
// two things is worse than one that is rejected.
fn parse_dotted_quad(s: &[u8]) -> Option<[u8; 4]> {
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
			if digits > 0 && val == 0 {
				return None;
			}
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
	Some(octets)
}

impl Ipv6Addr {
	pub fn octets(&self) -> [u8; 16] {
		[self.o0, self.o1, self.o2, self.o3, self.o4, self.o5, self.o6, self.o7, self.o8, self.o9, self.o10, self.o11, self.o12, self.o13, self.o14, self.o15]
	}

	pub fn from_octets(o: [u8; 16]) -> Ipv6Addr {
		Ipv6Addr { o0: o[0], o1: o[1], o2: o[2], o3: o[3], o4: o[4], o5: o[5], o6: o[6], o7: o[7], o8: o[8], o9: o[9], o10: o[10], o11: o[11], o12: o[12], o13: o[13], o14: o[14], o15: o[15] }
	}

	pub fn groups(&self) -> [u16; 8] {
		let o = self.octets();
		let mut groups = [0u16; 8];
		let mut i = 0;
		while i < 8 {
			groups[i] = u16::from_be_bytes([o[2 * i], o[2 * i + 1]]);
			i += 1;
		}
		groups
	}

	pub fn is_unspecified(&self) -> bool {
		self.octets() == [0u8; 16]
	}

	pub fn is_link_local(&self) -> bool {
		let o = self.octets();
		o[0] == 0xfe && o[1] & 0xc0 == 0x80
	}

	pub fn is_multicast(&self) -> bool {
		self.octets()[0] == 0xff
	}

	/// An IPv4-mapped address, `::ffff:a.b.c.d`.
	///
	/// NAMED SO IT CAN BE REFUSED. These are how an IPv4 address is smuggled into an IPv6 field, and
	/// this system's contracts refuse them as source, destination and listen address: an IPv4
	/// endpoint is expressible directly, so a second spelling for it buys nothing and costs every
	/// consumer a check it will sometimes forget.
	pub fn is_ipv4_mapped(&self) -> bool {
		let o = self.octets();
		o[..10] == [0u8; 10] && o[10] == 0xff && o[11] == 0xff
	}

	/// Parse the textual form, with `::` compression and an optional trailing dotted quad.
	pub fn parse(s: &[u8]) -> Option<Ipv6Addr> {
		// The address is at most eight groups; `::` splits it into a head and a tail, and the zeros
		// go between them. Parsing the two halves separately is what makes the "at most one `::`"
		// rule fall out rather than being a separate check.
		let mut head: [u16; 8] = [0; 8];
		let mut tail: [u16; 8] = [0; 8];
		let mut head_len: usize = 0;
		let mut tail_len: usize = 0;
		let mut compressed: bool = false;
		let mut at: usize = 0;

		if s.len() >= 2 && s[0] == b':' {
			if s[1] != b':' {
				return None;
			}
			compressed = true;
			at = 2;
		}
		while at < s.len() {
			if s[at] == b':' {
				if compressed {
					return None;
				}
				compressed = true;
				at += 1;
				if at == s.len() {
					break;
				}
				continue;
			}
			// A dotted quad may only appear last, and it occupies two groups.
			let start = at;
			let mut end = at;
			let mut dotted = false;
			while end < s.len() && s[end] != b':' {
				if s[end] == b'.' {
					dotted = true;
				}
				end += 1;
			}
			if dotted {
				if end != s.len() {
					return None;
				}
				let quad = parse_dotted_quad(&s[start..end])?;
				let pair = [u16::from_be_bytes([quad[0], quad[1]]), u16::from_be_bytes([quad[2], quad[3]])];
				for group in pair {
					if !push_group(&mut head, &mut head_len, &mut tail, &mut tail_len, compressed, group) {
						return None;
					}
				}
				break;
			}
			let group = parse_hex_group(&s[start..end])?;
			if !push_group(&mut head, &mut head_len, &mut tail, &mut tail_len, compressed, group) {
				return None;
			}
			at = end;
			if at < s.len() {
				// A trailing single colon is not an address.
				if at + 1 == s.len() {
					return None;
				}
				at += 1;
			}
		}

		let total = head_len + tail_len;
		if compressed {
			// `::` must stand for at least one group, otherwise it is a longer spelling of a full
			// address and RFC 5952 forbids it.
			if total >= 8 {
				return None;
			}
		} else if total != 8 {
			return None;
		}

		let mut groups = [0u16; 8];
		groups[..head_len].copy_from_slice(&head[..head_len]);
		groups[8 - tail_len..].copy_from_slice(&tail[..tail_len]);
		let mut octets = [0u8; 16];
		let mut i = 0;
		while i < 8 {
			let bytes = groups[i].to_be_bytes();
			octets[2 * i] = bytes[0];
			octets[2 * i + 1] = bytes[1];
			i += 1;
		}
		Some(Ipv6Addr::from_octets(octets))
	}

	/// Render in RFC 5952's canonical form: lowercase, no leading zeros, and `::` over the LONGEST
	/// run of zero groups - leftmost when two runs tie, and never over a run of one.
	///
	/// ONE FORM, BECAUSE TWO WOULD BE COMPARED. `2001:db8::1` and `2001:0db8:0:0:0:0:0:1` are the
	/// same address and different strings; anything that logs, diffs or keys on the text would treat
	/// them as different, so the rendering fixes the choice once.
	pub fn render(&self, out: &mut [u8]) -> usize {
		if self.is_ipv4_mapped() {
			let o = self.octets();
			let mut pos = 0;
			pos += push(out, pos, b"::ffff:");
			pos += Ipv4Addr { a: o[12], b: o[13], c: o[14], d: o[15] }.render(&mut out[pos..]);
			return pos;
		}
		let groups = self.groups();
		// The longest run of zero groups, and where it starts. A run of ONE is not compressed: `::`
		// would be two characters where `0` is one, and the standard says so.
		let (mut best_start, mut best_len) = (usize::MAX, 0usize);
		let mut index = 0;
		while index < 8 {
			if groups[index] != 0 {
				index += 1;
				continue;
			}
			let start = index;
			while index < 8 && groups[index] == 0 {
				index += 1;
			}
			if index - start > best_len {
				best_start = start;
				best_len = index - start;
			}
		}
		if best_len < 2 {
			best_start = usize::MAX;
		}

		let mut pos = 0;
		let mut i = 0;
		while i < 8 {
			if i == best_start {
				pos += push(out, pos, b"::");
				i += best_len;
				continue;
			}
			if i > 0 && !(best_start != usize::MAX && i == best_start + best_len) {
				pos += push(out, pos, b":");
			}
			pos += write_hex_group(groups[i], &mut out[pos..]);
			i += 1;
		}
		if pos == 0 {
			pos += push(out, pos, b"::");
		}
		pos
	}
}

fn push_group(head: &mut [u16; 8], head_len: &mut usize, tail: &mut [u16; 8], tail_len: &mut usize, compressed: bool, group: u16) -> bool {
	let target = if compressed { tail } else { head };
	let len = if compressed { tail_len } else { head_len };
	if *len >= 8 {
		return false;
	}
	target[*len] = group;
	*len += 1;
	true
}

fn parse_hex_group(s: &[u8]) -> Option<u16> {
	if s.is_empty() || s.len() > 4 {
		return None;
	}
	let mut value: u32 = 0;
	for &byte in s {
		let digit = match byte {
			b'0'..=b'9' => byte - b'0',
			b'a'..=b'f' => byte - b'a' + 10,
			b'A'..=b'F' => byte - b'A' + 10,
			_ => return None,
		};
		value = value * 16 + u32::from(digit);
	}
	Some(value as u16)
}

fn write_hex_group(group: u16, out: &mut [u8]) -> usize {
	if group == 0 {
		out[0] = b'0';
		return 1;
	}
	let mut digits = [0u8; 4];
	let mut count = 0;
	let mut value = group;
	while value != 0 {
		digits[count] = hex_digit((value & 0xf) as u8);
		count += 1;
		value >>= 4;
	}
	let mut pos = 0;
	while pos < count {
		out[pos] = digits[count - 1 - pos];
		pos += 1;
	}
	count
}

impl IpAddress {
	/// Parse either family from text. A value with a colon in it is IPv6 and nothing else, which is
	/// what makes the two forms unambiguous without a caller-supplied family.
	pub fn parse(s: &[u8]) -> Option<IpAddress> {
		if s.contains(&b':') {
			return Ipv6Addr::parse(s).map(IpAddress::V6);
		}
		Ipv4Addr::parse(s).map(IpAddress::V4)
	}

	pub fn render(&self, out: &mut [u8]) -> usize {
		match self {
			IpAddress::V4(addr) => addr.render(out),
			IpAddress::V6(addr) => addr.render(out),
		}
	}

	pub fn is_unspecified(&self) -> bool {
		match self {
			IpAddress::V4(addr) => addr.is_unspecified(),
			IpAddress::V6(addr) => addr.is_unspecified(),
		}
	}

	pub fn is_v4(&self) -> bool {
		matches!(self, IpAddress::V4(_))
	}

	pub fn is_v6(&self) -> bool {
		matches!(self, IpAddress::V6(_))
	}

	/// The unspecified address of a family: `0.0.0.0` or `::`, which is what every one of these
	/// protocols means by "any".
	pub fn unspecified_v4() -> IpAddress {
		IpAddress::V4(Ipv4Addr::from_octets([0; 4]))
	}

	pub fn unspecified_v6() -> IpAddress {
		IpAddress::V6(Ipv6Addr::from_octets([0; 16]))
	}

	/// Does this address need an interface to mean anything?
	///
	/// Link-local unicast and every multicast scope narrower than global do; a global unicast
	/// address does not. This is the predicate `scoped-address` validation is written against.
	pub fn needs_scope(&self) -> bool {
		match self {
			IpAddress::V4(_) => false,
			IpAddress::V6(addr) => addr.is_link_local() || addr.is_multicast(),
		}
	}
}

impl ScopedAddress {
	/// An address that needs no interface to be meaningful.
	pub fn global(addr: IpAddress) -> ScopedAddress {
		ScopedAddress { addr, scope: None }
	}

	pub fn render(&self, out: &mut [u8]) -> usize {
		let mut pos = self.addr.render(out);
		if let Some(scope) = &self.scope {
			pos += push(out, pos, b"%if");
			pos += write_dec_u32(scope.index, &mut out[pos..]);
			pos += push(out, pos, b".");
			pos += write_dec_u64(scope.generation, &mut out[pos..]);
		}
		pos
	}
}

impl MacAddr {
	pub fn octets(&self) -> [u8; 6] {
		[self.a, self.b, self.c, self.d, self.e, self.f]
	}

	pub fn from_octets(o: [u8; 6]) -> MacAddr {
		MacAddr { a: o[0], b: o[1], c: o[2], d: o[3], e: o[4], f: o[5] }
	}

	pub fn render(&self, out: &mut [u8]) -> usize {
		write_mac(&self.octets(), out)
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

fn push(out: &mut [u8], at: usize, text: &[u8]) -> usize {
	out[at..at + text.len()].copy_from_slice(text);
	text.len()
}

fn write_dec(n: u8, out: &mut [u8]) -> usize {
	write_dec_u64(u64::from(n), out)
}

fn write_dec_u32(n: u32, out: &mut [u8]) -> usize {
	write_dec_u64(u64::from(n), out)
}

fn write_dec_u64(mut n: u64, out: &mut [u8]) -> usize {
	let mut digits = [0u8; 20];
	let mut count = 0;
	loop {
		digits[count] = b'0' + (n % 10) as u8;
		count += 1;
		n /= 10;
		if n == 0 {
			break;
		}
	}
	let mut pos = 0;
	while pos < count {
		out[pos] = digits[count - 1 - pos];
		pos += 1;
	}
	count
}

fn hex_digit(n: u8) -> u8 {
	if n < 10 { b'0' + n } else { b'a' + (n - 10) }
}

#[cfg(test)]
mod tests;
