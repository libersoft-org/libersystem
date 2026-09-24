//! THE RAW-IP MEDIUM'S FRAMING, which is that there is none: one IPv4 datagram per message, in both
//! directions, and what arrives is taken for exactly what its own header says it is.
//!
//! There is no Ethernet header to strip, no ARP to answer and no DHCP to run - a raw-IP link's address,
//! route and DNS arrive with its attachment. What is checked here is what an Ethernet frame's length
//! and type field would otherwise have vouched for: that the bytes are an IPv4 datagram at all, that
//! its header and total lengths fit what arrived, and that it is no larger than the link's MTU.
//! Anything else is dropped; an IPv6 datagram on an IPv4-only link is dropped as unsupported.

/// Why a message on a raw-IP link was not a datagram to process.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
	/// Shorter than an IPv4 header, or its lengths do not fit.
	Malformed,
	/// Version 6 - a family this link does not carry.
	Unsupported,
	/// Past the link's MTU.
	Oversized,
}

/// The IPv4 datagram a raw-IP message carries, trimmed to its own total length.
pub fn datagram(message: &[u8], mtu: u16) -> Result<&[u8], Refused> {
	if message.len() > usize::from(mtu) {
		return Err(Refused::Oversized);
	}
	let Some(&first) = message.first() else { return Err(Refused::Malformed) };
	match first >> 4 {
		4 => {}
		6 => return Err(Refused::Unsupported),
		_ => return Err(Refused::Malformed),
	}
	if message.len() < 20 {
		return Err(Refused::Malformed);
	}
	let header = usize::from(first & 0x0f) * 4;
	let total = usize::from(u16::from_be_bytes([message[2], message[3]]));
	if header < 20 || total < header || total > message.len() {
		return Err(Refused::Malformed);
	}
	Ok(&message[..total])
}

#[cfg(test)]
mod tests;
