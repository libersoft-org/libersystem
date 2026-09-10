//! Reading and writing IPv6 on Ethernet: the fixed header, the extension chain, and the bounds.
//!
//! WHAT MAKES THIS DIFFERENT FROM THE IPv4 PARSER BESIDE IT. IPv4 carries its whole length in the
//! header and the parser trims to it, because an Ethernet frame is padded to sixty bytes and the
//! padding is not payload. IPv6 has the same problem and a different field: Payload Length counts
//! what follows the forty-byte header, and everything after that is padding. A rule that refused
//! "trailing bytes" would refuse every short IPv6 frame on a padded link, so the packet is SLICED to
//! the declared length before anything walks it - once, here, rather than in each consumer.
//!
//! WHAT IS BOUNDED, AND WHY EACH BOUND EXISTS. An extension chain is a linked list a sender
//! controls: without a limit, a crafted packet makes this host walk as long as the sender likes
//! inside one frame. Eight headers and 256 bytes is more than any legitimate packet on this link
//! uses, and both are counted rather than assumed.
//!
//! THE CONFORMANCE GAP IS NAMED. This host processes an ATOMIC fragment - offset zero, more-fragments
//! clear - as the complete packet it is, and REFUSES every other fragment with a typed result that
//! the caller counts. It does not reassemble. That is a stated gap rather than a silent drop, and it
//! is what a DNS resolver on a path with a smaller MTU actually needs.

use crate::ipv6::{Address, Kind};
use alloc::vec::Vec;

/// The EtherType an IPv6 frame carries.
pub const ETHERTYPE_IPV6: u16 = 0x86dd;

/// The fixed IPv6 header, in bytes.
pub const HEADER_LEN: usize = 40;

/// The Ethernet II header, in bytes: destination, source, type.
pub const ETHERNET_HEADER_LEN: usize = 14;

/// The smallest MTU IPv6 permits on any link. A link below this cannot carry IPv6 at all.
pub const MIN_MTU: u16 = 1280;

/// How many extension headers this host will walk before refusing the packet.
pub const MAX_EXTENSION_HEADERS: usize = 8;

/// How many extension-header BYTES this host will walk before refusing the packet.
pub const MAX_EXTENSION_BYTES: usize = 256;

/// Next-header values this host acts on. Everything else is `Other`, which is delivered to nothing
/// and answered per the caller's policy.
pub const NEXT_HOP_BY_HOP: u8 = 0;
pub const NEXT_TCP: u8 = 6;
pub const NEXT_UDP: u8 = 17;
pub const NEXT_ROUTING: u8 = 43;
pub const NEXT_FRAGMENT: u8 = 44;
pub const NEXT_ICMPV6: u8 = 58;
pub const NEXT_NONE: u8 = 59;
pub const NEXT_DESTINATION: u8 = 60;

/// The Jumbo Payload option type, which this host refuses: its MTU and buffer model cannot carry a
/// jumbogram, and accepting the option while ignoring the length would be worse than refusing it.
const OPTION_JUMBO_PAYLOAD: u8 = 0xc2;

/// Pad1, which is one byte and has no length field.
const OPTION_PAD1: u8 = 0;

/// What the top two bits of an unknown option type tell a host to do.
///
/// The two bits are the whole reason a blanket "no ICMP error for a multicast destination" rule is
/// wrong: `DiscardAndReportAlways` and `DiscardAndReportUnicast` differ ONLY in the multicast case,
/// so a host that suppressed both could not implement the first and could not be told apart from a
/// host that ignored the bits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OptionAction {
	/// `00` - step over the option and keep going.
	Skip,
	/// `01` - drop the packet silently.
	Discard,
	/// `10` - drop it and report Parameter Problem, whatever the destination was.
	DiscardAndReportAlways,
	/// `11` - drop it and report Parameter Problem unless the destination was multicast.
	DiscardAndReportUnicast,
}

impl OptionAction {
	fn of(option_type: u8) -> OptionAction {
		match option_type >> 6 {
			0 => OptionAction::Skip,
			1 => OptionAction::Discard,
			2 => OptionAction::DiscardAndReportAlways,
			_ => OptionAction::DiscardAndReportUnicast,
		}
	}

	/// Does this action send Parameter Problem for a packet addressed to `destination`?
	pub fn reports(&self, destination: Address) -> bool {
		match self {
			OptionAction::Skip | OptionAction::Discard => false,
			OptionAction::DiscardAndReportAlways => true,
			OptionAction::DiscardAndReportUnicast => destination.kind() != Kind::Multicast,
		}
	}
}

/// Why a packet was not accepted. Every arm is a distinct decision a caller may need to count, and
/// none of them is "malformed" without saying what about it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// The frame is shorter than the Ethernet header, or the packet shorter than the fixed header.
	TooShort,
	/// The EtherType is not IPv6.
	NotIpv6,
	/// The version nibble is not 6.
	BadVersion,
	/// Payload Length declares more bytes than the frame carries.
	PayloadTruncated,
	/// The source address may not source a packet: multicast, or a form this host does not act on.
	BadSource,
	/// The destination may not be a destination.
	BadDestination,
	/// More than `MAX_EXTENSION_HEADERS` headers in the chain.
	ChainTooLong,
	/// More than `MAX_EXTENSION_BYTES` bytes of extension headers.
	ChainTooLarge,
	/// An extension header ran past the end of the packet, or declared a length of zero where the
	/// format forbids it.
	MalformedChain,
	/// An option's length ran past the end of its header.
	MalformedOption,
	/// An unknown option whose action bits say to drop, with the action so the caller can decide
	/// whether to report, and the offset of the option for the Parameter Problem pointer.
	UnknownOption { action: OptionAction, offset: u32 },
	/// A Routing header this host does not accept: type 0 is deprecated, and a non-zero Segments
	/// Left on any type this host does not implement cannot be forwarded.
	DeprecatedRouting,
	/// A Jumbo Payload option. Named separately from `UnknownOption` because refusing it is a
	/// STATED limit of this host rather than a property of the packet.
	Jumbogram,
	/// A fragment that is not atomic. The named conformance gap: counted, not silently dropped.
	NonAtomicFragment,
	/// A fragment header ahead of a Neighbour Discovery message, which RFC 6980 forbids.
	FragmentedNeighbourDiscovery,
}

/// The fixed header's fields, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
	pub traffic_class: u8,
	pub flow_label: u32,
	pub payload_len: u16,
	pub next_header: u8,
	pub hop_limit: u8,
	pub source: Address,
	pub destination: Address,
}

/// A packet this host accepted, and what the walk found on the way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Parsed<'a> {
	pub header: Header,
	/// The protocol above IPv6, after the extension chain was walked.
	pub upper: u8,
	/// The upper-layer payload, already sliced to Payload Length and past the extension chain.
	pub payload: &'a [u8],
	/// How many extension-header bytes preceded the payload. A caller building a reply needs it to
	/// quote the right offsets.
	pub extension_bytes: usize,
	/// Whether an atomic fragment header was stripped on the way. A Neighbour Discovery consumer
	/// refuses the message when this is set, which is RFC 6980.
	pub atomic_fragment: bool,
}

/// Read the fixed header out of an L3 packet, without walking the chain.
pub fn header(packet: &[u8]) -> Result<Header, Refusal> {
	if packet.len() < HEADER_LEN {
		return Err(Refusal::TooShort);
	}
	if packet[0] >> 4 != 6 {
		return Err(Refusal::BadVersion);
	}
	let mut source = [0u8; 16];
	let mut destination = [0u8; 16];
	source.copy_from_slice(&packet[8..24]);
	destination.copy_from_slice(&packet[24..40]);
	Ok(Header { traffic_class: ((packet[0] & 0x0f) << 4) | (packet[1] >> 4), flow_label: (u32::from(packet[1] & 0x0f) << 16) | (u32::from(packet[2]) << 8) | u32::from(packet[3]), payload_len: u16::from_be_bytes([packet[4], packet[5]]), next_header: packet[6], hop_limit: packet[7], source: Address::new(source), destination: Address::new(destination) })
}

/// Parse an Ethernet frame carrying IPv6.
///
/// The frame's own padding is discarded here rather than by the caller: `parse_packet` slices to
/// Payload Length, and this only has to find where the L3 packet starts.
pub fn parse_frame(frame: &[u8]) -> Result<Parsed<'_>, Refusal> {
	if frame.len() < ETHERNET_HEADER_LEN {
		return Err(Refusal::TooShort);
	}
	if u16::from_be_bytes([frame[12], frame[13]]) != ETHERTYPE_IPV6 {
		return Err(Refusal::NotIpv6);
	}
	parse_packet(&frame[ETHERNET_HEADER_LEN..])
}

/// Parse an L3 packet: validate the fixed header, slice to Payload Length, walk the chain.
pub fn parse_packet(packet: &[u8]) -> Result<Parsed<'_>, Refusal> {
	let header = header(packet)?;
	// A SOURCE THAT CANNOT SOURCE IS A PACKET THIS HOST DOES NOT ANSWER. The unspecified address is
	// permitted here and only here: a duplicate-address-detection solicitation is sourced from `::`,
	// and refusing it would break the mechanism that keeps two hosts off one address.
	if !header.source.valid_source() && header.source.kind() != Kind::Unspecified {
		return Err(Refusal::BadSource);
	}
	if !header.destination.valid_destination() {
		return Err(Refusal::BadDestination);
	}
	let declared = usize::from(header.payload_len);
	let available = packet.len() - HEADER_LEN;
	if declared > available {
		return Err(Refusal::PayloadTruncated);
	}
	// THE SLICE, and everything below walks inside it. Bytes beyond it are Ethernet padding.
	let body = &packet[HEADER_LEN..HEADER_LEN + declared];
	walk(header, body)
}

/// Walk the extension chain inside an already-sliced body.
fn walk(header: Header, body: &[u8]) -> Result<Parsed<'_>, Refusal> {
	let mut next = header.next_header;
	let mut offset = 0usize;
	let mut headers = 0usize;
	let mut atomic_fragment = false;
	loop {
		match next {
			NEXT_HOP_BY_HOP | NEXT_DESTINATION => {
				// A JUMBO PAYLOAD IS REFUSED BEFORE THE OPTION WALK CAN SKIP IT. Payload Length zero
				// with a Jumbo option means the real length is in the option, and a host that walked
				// past the option would then treat a jumbogram as an empty packet.
				let (length, options) = extension_slice(body, offset)?;
				step_options(options, HEADER_LEN + offset)?;
				// The first byte of an extension header is the NEXT one's number. Read it before
				// stepping over the header, not by arithmetic afterwards.
				next = options[0];
				offset += length;
			}
			NEXT_ROUTING => {
				let (length, routing) = extension_slice(body, offset)?;
				// Next Header, Hdr Ext Len, Routing Type, Segments Left.
				if routing.len() < 4 {
					return Err(Refusal::MalformedChain);
				}
				// Type 0 is deprecated (RFC 5095) and is a traffic-amplification tool. Any other
				// type with segments left is a packet this host would have to forward, and it is not
				// a router: both are refused rather than ignored.
				if routing[2] == 0 || routing[3] != 0 {
					return Err(Refusal::DeprecatedRouting);
				}
				next = routing[0];
				offset += length;
			}
			NEXT_FRAGMENT => {
				// The fragment header is a fixed eight bytes and carries its own next header.
				if offset + 8 > body.len() {
					return Err(Refusal::MalformedChain);
				}
				let fragment = &body[offset..offset + 8];
				let fragment_offset = (u16::from_be_bytes([fragment[2], fragment[3]]) >> 3) as usize;
				let more = fragment[3] & 1 != 0;
				if fragment_offset != 0 || more {
					return Err(Refusal::NonAtomicFragment);
				}
				atomic_fragment = true;
				next = fragment[0];
				offset += 8;
			}
			_ => break,
		}
		headers += 1;
		if headers > MAX_EXTENSION_HEADERS {
			return Err(Refusal::ChainTooLong);
		}
		if offset > MAX_EXTENSION_BYTES {
			return Err(Refusal::ChainTooLarge);
		}
	}
	// RFC 6980: a Neighbour Discovery message that arrived behind a fragment header is refused,
	// atomic or not. The rule exists because fragmentation is how an attacker gets ND options past
	// a host that only inspects the first fragment.
	if atomic_fragment && next == NEXT_ICMPV6 && is_neighbour_discovery(&body[offset..]) {
		return Err(Refusal::FragmentedNeighbourDiscovery);
	}
	Ok(Parsed { header, upper: next, payload: &body[offset..], extension_bytes: offset, atomic_fragment })
}

/// The ICMPv6 types Neighbour Discovery uses.
fn is_neighbour_discovery(payload: &[u8]) -> bool {
	matches!(payload.first(), Some(133..=137))
}

/// The bytes of the extension header at `offset`, and its total length.
///
/// Returns the whole header including its two leading bytes, because the option walk needs the
/// offsets to be the packet's own.
fn extension_slice(body: &[u8], offset: usize) -> Result<(usize, &[u8]), Refusal> {
	if offset + 2 > body.len() {
		return Err(Refusal::MalformedChain);
	}
	let length = (usize::from(body[offset + 1]) + 1) * 8;
	if offset + length > body.len() {
		return Err(Refusal::MalformedChain);
	}
	Ok((length, &body[offset..offset + length]))
}

/// Walk the options of a Hop-by-Hop or Destination Options header.
///
/// `header_offset` is where the header starts in the L3 packet, so an `UnknownOption` refusal can
/// carry the pointer a Parameter Problem message needs.
fn step_options(header_bytes: &[u8], header_offset: usize) -> Result<(), Refusal> {
	let mut index = 2usize;
	while index < header_bytes.len() {
		let option_type = header_bytes[index];
		if option_type == OPTION_PAD1 {
			index += 1;
			continue;
		}
		if index + 1 >= header_bytes.len() {
			return Err(Refusal::MalformedOption);
		}
		let length = usize::from(header_bytes[index + 1]);
		if index + 2 + length > header_bytes.len() {
			return Err(Refusal::MalformedOption);
		}
		if option_type == OPTION_JUMBO_PAYLOAD {
			return Err(Refusal::Jumbogram);
		}
		// PadN and the options this host understands are skipped; an unknown one obeys its bits.
		if option_type != 1 && !known_option(option_type) {
			let action = OptionAction::of(option_type);
			if action != OptionAction::Skip {
				return Err(Refusal::UnknownOption { action, offset: (header_offset + index) as u32 });
			}
		}
		index += 2 + length;
	}
	Ok(())
}

/// The options this host implements. Router Alert is the one a Multicast Listener Discovery message
/// carries, and refusing it would refuse this host's own group reports coming back.
fn known_option(option_type: u8) -> bool {
	option_type == 5
}

/// Is this packet addressed to us?
///
/// A host accepts its own unicast addresses, the groups it has joined, and all-nodes. Anything else
/// on the wire is somebody else's, and answering it is how a host becomes an amplifier.
pub fn destination_is_ours(destination: Address, unicast: &[Address], groups: &[Address]) -> bool {
	if destination == crate::ipv6::ALL_NODES {
		return true;
	}
	unicast.contains(&destination) || groups.contains(&destination)
}

/// Why an outgoing packet could not be built.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EgressRefusal {
	/// The payload exceeds what the path can carry. Carries the limit so the caller can report it
	/// rather than guess.
	TooLarge { limit: u16 },
	/// The source or destination is not one this host may put in the header.
	BadAddress,
	/// The payload is larger than Payload Length can express.
	PayloadOverflow,
}

/// Build an L3 packet: the fixed header and the payload, with no extension headers.
///
/// THIS HOST ORIGINATES NO FRAGMENTS, which is why the MTU check is a refusal rather than a split.
/// A caller told `TooLarge` knows the path is narrower than it thought, and the answer is a smaller
/// write or a Packet Too Big report to whoever asked - not a fragment this host would then have to
/// be able to reassemble.
pub fn build_packet(source: Address, destination: Address, next_header: u8, hop_limit: u8, payload: &[u8], mtu: u16) -> Result<Vec<u8>, EgressRefusal> {
	if !source.valid_source() || !destination.valid_destination() {
		return Err(EgressRefusal::BadAddress);
	}
	if payload.len() > usize::from(u16::MAX) {
		return Err(EgressRefusal::PayloadOverflow);
	}
	let limit = mtu.max(MIN_MTU);
	if HEADER_LEN + payload.len() > usize::from(limit) {
		return Err(EgressRefusal::TooLarge { limit });
	}
	let mut packet = Vec::with_capacity(HEADER_LEN + payload.len());
	packet.push(0x60);
	packet.extend_from_slice(&[0, 0, 0]);
	packet.extend_from_slice(&(payload.len() as u16).to_be_bytes());
	packet.push(next_header);
	packet.push(hop_limit);
	packet.extend_from_slice(&source.octets());
	packet.extend_from_slice(&destination.octets());
	packet.extend_from_slice(payload);
	Ok(packet)
}

/// Build the whole Ethernet frame around `build_packet`.
pub fn build_frame(destination_mac: [u8; 6], source_mac: [u8; 6], source: Address, destination: Address, next_header: u8, hop_limit: u8, payload: &[u8], mtu: u16) -> Result<Vec<u8>, EgressRefusal> {
	let packet = build_packet(source, destination, next_header, hop_limit, payload, mtu)?;
	let mut frame = Vec::with_capacity(ETHERNET_HEADER_LEN + packet.len());
	frame.extend_from_slice(&destination_mac);
	frame.extend_from_slice(&source_mac);
	frame.extend_from_slice(&ETHERTYPE_IPV6.to_be_bytes());
	frame.extend_from_slice(&packet);
	Ok(frame)
}

/// The checksum over the IPv6 pseudo-header and an upper-layer message.
///
/// IPv6 has no header checksum of its own, so the upper layer's is the only integrity check on the
/// addresses: a UDP or ICMPv6 message whose checksum was computed over different addresses does not
/// verify here, which is what stops a packet delivered to the wrong host from being processed.
pub fn pseudo_header_checksum(source: Address, destination: Address, next_header: u8, message: &[u8]) -> u16 {
	let mut sum: u32 = 0;
	for chunk in source.octets().chunks(2).chain(destination.octets().chunks(2)) {
		sum += u32::from(u16::from_be_bytes([chunk[0], chunk[1]]));
	}
	let length = message.len() as u32;
	sum += length >> 16;
	sum += length & 0xffff;
	sum += u32::from(next_header);
	let mut index = 0usize;
	while index + 1 < message.len() {
		sum += u32::from(u16::from_be_bytes([message[index], message[index + 1]]));
		index += 2;
	}
	if index < message.len() {
		sum += u32::from(u16::from_be_bytes([message[index], 0]));
	}
	while sum >> 16 != 0 {
		sum = (sum & 0xffff) + (sum >> 16);
	}
	let folded = !(sum as u16);
	// A zero checksum is transmitted as 0xffff: zero means "no checksum" in UDP over IPv4 and is
	// forbidden outright in IPv6, so the two representations of zero must not be confused.
	if folded == 0 { 0xffff } else { folded }
}

#[cfg(test)]
mod tests;
