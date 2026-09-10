//! The neighbour-discovery messages on the wire, and the options they carry.
//!
//! WHAT IS AND IS NOT CHECKED HERE. This is a codec: it reads bytes into typed values and writes
//! typed values back into bytes. The decisions - whether to believe a router, whether to form an
//! address, what to do with a lifetime - belong to the modules beside it, and keeping them apart is
//! what lets each be tested against hostile input without standing a whole stack up.
//!
//! WHAT A CODEC MUST STILL REFUSE. An option length of zero, which RFC 4861 forbids precisely
//! because a walker that accepted it would loop forever; an option that runs past the end of the
//! message; and a message shorter than the fixed part of its own type. Those are not policy, they
//! are the difference between a parser and a hang.
//!
//! THE HOP LIMIT IS THE ONLY AUTHENTICATION THIS PROTOCOL HAS. Every neighbour-discovery message
//! must arrive with a hop limit of 255, because a router decrements it: a message that still has 255
//! cannot have been forwarded, so it came from the link. It is a weak check and it is the one the
//! protocol is built on, so it is applied to every message rather than to some of them.

use crate::ipv6::{Address, Prefix};
use crate::ipv6_slaac::PrefixInformation;
use alloc::vec::Vec;

/// The hop limit every neighbour-discovery message must arrive with.
pub const ND_HOP_LIMIT: u8 = 255;

/// Option types this host reads.
pub const OPTION_SOURCE_LINK_LAYER: u8 = 1;
pub const OPTION_TARGET_LINK_LAYER: u8 = 2;
pub const OPTION_PREFIX_INFORMATION: u8 = 3;
pub const OPTION_MTU: u8 = 5;
pub const OPTION_RDNSS: u8 = 25;

/// How many options this host will walk in one message.
///
/// A message is bounded by the frame, but the walk should be bounded by something it can state:
/// thirty-two is far more than any legitimate advertisement carries and small enough that a crafted
/// message full of one-unit options costs nothing.
pub const MAX_OPTIONS: usize = 32;

/// One decoded option. Unknown types are SKIPPED rather than refused - RFC 4861 requires it, so that
/// a future option does not make this host drop the advertisement carrying it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NdOption {
	SourceLinkLayer([u8; 6]),
	TargetLinkLayer([u8; 6]),
	Prefix(PrefixInformation),
	/// The link MTU a router advertises.
	Mtu(u32),
	/// Recursive DNS servers with their shared lifetime, in seconds.
	Rdnss {
		lifetime_seconds: u32,
		servers: Vec<Address>,
	},
}

/// Why a message or option was not decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NdRefusal {
	/// Shorter than the fixed part of its type.
	TooShort,
	/// The hop limit was not 255, so the message may have been forwarded.
	NotFromTheLink,
	/// An option declared a length of zero, which would make a walker loop.
	ZeroLengthOption,
	/// An option ran past the end of the message.
	OptionOverruns,
	/// More options than this host will walk.
	TooManyOptions,
	/// A field inside a well-formed option is not one this host accepts.
	MalformedOption,
	/// The message type is not a neighbour-discovery one.
	WrongType,
}

/// A router advertisement, decoded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RouterAdvertisement {
	pub current_hop_limit: u8,
	pub managed: bool,
	pub other_configuration: bool,
	/// The two-bit preference field, already read.
	pub preference_bits: u8,
	pub router_lifetime_seconds: u16,
	pub reachable_time_ms: u32,
	pub retrans_timer_ms: u32,
	pub options: Vec<NdOption>,
}

/// A neighbour solicitation, decoded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NeighbourSolicitation {
	pub target: Address,
	pub options: Vec<NdOption>,
}

/// A neighbour advertisement, decoded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NeighbourAdvertisement {
	pub router: bool,
	pub solicited: bool,
	pub override_flag: bool,
	pub target: Address,
	pub options: Vec<NdOption>,
}

/// Walk the options at `body`, which is the message past its fixed part.
pub fn decode_options(body: &[u8]) -> Result<Vec<NdOption>, NdRefusal> {
	let mut options = Vec::new();
	let mut index = 0usize;
	let mut walked = 0usize;
	while index < body.len() {
		if index + 2 > body.len() {
			return Err(NdRefusal::OptionOverruns);
		}
		let option_type = body[index];
		let units = body[index + 1];
		if units == 0 {
			// THE LOOP THE STANDARD FORBIDS. An option of zero length would leave the cursor where
			// it was, and a walker that trusted the field would never come back.
			return Err(NdRefusal::ZeroLengthOption);
		}
		let length = usize::from(units) * 8;
		if index + length > body.len() {
			return Err(NdRefusal::OptionOverruns);
		}
		walked += 1;
		if walked > MAX_OPTIONS {
			return Err(NdRefusal::TooManyOptions);
		}
		let option = &body[index..index + length];
		match option_type {
			OPTION_SOURCE_LINK_LAYER | OPTION_TARGET_LINK_LAYER => {
				// One unit holds the two-byte header and a six-byte Ethernet address exactly.
				if length != 8 {
					return Err(NdRefusal::MalformedOption);
				}
				let mut mac = [0u8; 6];
				mac.copy_from_slice(&option[2..8]);
				options.push(if option_type == OPTION_SOURCE_LINK_LAYER { NdOption::SourceLinkLayer(mac) } else { NdOption::TargetLinkLayer(mac) });
			}
			OPTION_PREFIX_INFORMATION => {
				if length != 32 {
					return Err(NdRefusal::MalformedOption);
				}
				let prefix_len = option[2];
				let flags = option[3];
				let valid_seconds = u32::from_be_bytes([option[4], option[5], option[6], option[7]]);
				let preferred_seconds = u32::from_be_bytes([option[8], option[9], option[10], option[11]]);
				let mut bytes = [0u8; 16];
				bytes.copy_from_slice(&option[16..32]);
				let Some(prefix) = Prefix::new(Address::new(bytes), prefix_len) else {
					return Err(NdRefusal::MalformedOption);
				};
				options.push(NdOption::Prefix(PrefixInformation { prefix, on_link: flags & 0x80 != 0, autonomous: flags & 0x40 != 0, valid_seconds, preferred_seconds }));
			}
			OPTION_MTU => {
				if length != 8 {
					return Err(NdRefusal::MalformedOption);
				}
				options.push(NdOption::Mtu(u32::from_be_bytes([option[4], option[5], option[6], option[7]])));
			}
			OPTION_RDNSS => {
				// Two bytes of header, two reserved, four of lifetime, then whole addresses.
				if length < 24 || (length - 8) % 16 != 0 {
					return Err(NdRefusal::MalformedOption);
				}
				let lifetime_seconds = u32::from_be_bytes([option[4], option[5], option[6], option[7]]);
				let mut servers = Vec::new();
				for chunk in option[8..].chunks_exact(16) {
					let mut bytes = [0u8; 16];
					bytes.copy_from_slice(chunk);
					servers.push(Address::new(bytes));
				}
				options.push(NdOption::Rdnss { lifetime_seconds, servers });
			}
			// UNKNOWN OPTIONS ARE SKIPPED, which is what keeps a future extension from making this
			// host drop the advertisement that carries it.
			_ => {}
		}
		index += length;
	}
	Ok(options)
}

/// Decode a router advertisement out of a verified ICMPv6 message.
pub fn decode_router_advertisement(message: &[u8], hop_limit: u8) -> Result<RouterAdvertisement, NdRefusal> {
	if hop_limit != ND_HOP_LIMIT {
		return Err(NdRefusal::NotFromTheLink);
	}
	if message.len() < 16 {
		return Err(NdRefusal::TooShort);
	}
	if message[0] != crate::ipv6_icmp::ROUTER_ADVERTISEMENT {
		return Err(NdRefusal::WrongType);
	}
	let flags = message[5];
	Ok(RouterAdvertisement { current_hop_limit: message[4], managed: flags & 0x80 != 0, other_configuration: flags & 0x40 != 0, preference_bits: (flags >> 3) & 0b11, router_lifetime_seconds: u16::from_be_bytes([message[6], message[7]]), reachable_time_ms: u32::from_be_bytes([message[8], message[9], message[10], message[11]]), retrans_timer_ms: u32::from_be_bytes([message[12], message[13], message[14], message[15]]), options: decode_options(&message[16..])? })
}

/// Decode a neighbour solicitation.
pub fn decode_neighbour_solicitation(message: &[u8], hop_limit: u8) -> Result<NeighbourSolicitation, NdRefusal> {
	if hop_limit != ND_HOP_LIMIT {
		return Err(NdRefusal::NotFromTheLink);
	}
	if message.len() < 24 {
		return Err(NdRefusal::TooShort);
	}
	if message[0] != crate::ipv6_icmp::NEIGHBOUR_SOLICITATION {
		return Err(NdRefusal::WrongType);
	}
	let mut target = [0u8; 16];
	target.copy_from_slice(&message[8..24]);
	Ok(NeighbourSolicitation { target: Address::new(target), options: decode_options(&message[24..])? })
}

/// Decode a neighbour advertisement.
pub fn decode_neighbour_advertisement(message: &[u8], hop_limit: u8) -> Result<NeighbourAdvertisement, NdRefusal> {
	if hop_limit != ND_HOP_LIMIT {
		return Err(NdRefusal::NotFromTheLink);
	}
	if message.len() < 24 {
		return Err(NdRefusal::TooShort);
	}
	if message[0] != crate::ipv6_icmp::NEIGHBOUR_ADVERTISEMENT {
		return Err(NdRefusal::WrongType);
	}
	let flags = message[4];
	let mut target = [0u8; 16];
	target.copy_from_slice(&message[8..24]);
	Ok(NeighbourAdvertisement { router: flags & 0x80 != 0, solicited: flags & 0x40 != 0, override_flag: flags & 0x20 != 0, target: Address::new(target), options: decode_options(&message[24..])? })
}

/// Build a neighbour solicitation for `target`.
///
/// `source_link_layer` is omitted when the source address is unspecified, because a duplicate-address
/// detection probe must not tell the link where to send an answer it does not want.
pub fn build_neighbour_solicitation(target: Address, source_link_layer: Option<[u8; 6]>) -> Vec<u8> {
	let mut message = Vec::with_capacity(32);
	message.push(crate::ipv6_icmp::NEIGHBOUR_SOLICITATION);
	message.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0]);
	message.extend_from_slice(&target.octets());
	if let Some(mac) = source_link_layer {
		message.push(OPTION_SOURCE_LINK_LAYER);
		message.push(1);
		message.extend_from_slice(&mac);
	}
	message
}

/// Build a neighbour advertisement.
pub fn build_neighbour_advertisement(target: Address, link_layer: [u8; 6], router: bool, solicited: bool, override_flag: bool) -> Vec<u8> {
	let mut flags = 0u8;
	if router {
		flags |= 0x80;
	}
	if solicited {
		flags |= 0x40;
	}
	if override_flag {
		flags |= 0x20;
	}
	let mut message = Vec::with_capacity(32);
	message.push(crate::ipv6_icmp::NEIGHBOUR_ADVERTISEMENT);
	message.extend_from_slice(&[0, 0, 0]);
	message.push(flags);
	message.extend_from_slice(&[0, 0, 0]);
	message.extend_from_slice(&target.octets());
	message.push(OPTION_TARGET_LINK_LAYER);
	message.push(1);
	message.extend_from_slice(&link_layer);
	message
}

/// Build a router solicitation.
pub fn build_router_solicitation(source_link_layer: Option<[u8; 6]>) -> Vec<u8> {
	let mut message = Vec::with_capacity(16);
	message.push(crate::ipv6_icmp::ROUTER_SOLICITATION);
	message.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0]);
	if let Some(mac) = source_link_layer {
		message.push(OPTION_SOURCE_LINK_LAYER);
		message.push(1);
		message.extend_from_slice(&mac);
	}
	message
}

#[cfg(test)]
mod tests;
