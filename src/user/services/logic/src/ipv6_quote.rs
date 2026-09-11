//! What an ICMPv6 error's QUOTATION says, and the one check its consumer must make before acting.
//!
//! AN ERROR IS A CLAIM ABOUT A PACKET THIS HOST SENT, made by a third party. Two different questions
//! follow from that, and they belong to two different layers:
//!
//!   - is the quotation WELL FORMED, and does it name an address this interface holds? That is what
//!     this module answers, and it is all a layer holding no flow state can answer;
//!   - was the quoted packet actually one of MINE, still outstanding? That is a lookup in the
//!     consumer's own send state, and only the consumer can do it.
//!
//! Keeping the split explicit is what stops a forged Packet Too Big from lowering a path MTU: the
//! validation below is deliberately not enough to justify a durable write, and the only thing that
//! writes is the consumer, after `quotation_is_in_flight`.
//!
//! WHY A SHORT QUOTATION IS DROPPED RATHER THAN PARTLY BELIEVED. RFC 4443 asks a reporter to quote as
//! much of the invoking packet as fits; a quotation that stops inside the transport header still
//! names a source and destination, so it is tempting to expose it with the missing fields defaulted.
//! That is exactly the mistake: a zero sequence is a real sequence, and a consumer matching it
//! against its unacknowledged interval would attribute a stranger's truncated quotation to whichever
//! flow happens to start there. Seven quoted TCP bytes are not most of a header, they are a
//! quotation this host cannot attribute, so they are refused and counted.

use crate::ipv6::{Address, Interface};
use crate::ipv6_events::{ErrorClass, QuotedError, QuotedTransport};
use crate::ipv6_icmp;
use crate::ipv6_packet;

/// How many bytes of an ICMPv6 error precede the quotation: type, code, checksum and the four bytes
/// whose meaning the type decides.
pub const ERROR_HEADER_LEN: usize = 8;

/// The quoted TCP bytes needed to recover both ports AND the sequence the consumer checks.
pub const TCP_QUOTE_LEN: usize = 8;

/// The quoted UDP bytes needed to recover both ports.
pub const UDP_QUOTE_LEN: usize = 4;

/// The quoted ICMPv6 bytes needed to recover an echo identifier and sequence: the whole eight-byte
/// echo header, type and code included.
pub const ECHO_QUOTE_LEN: usize = 8;

/// Why a quotation produced no event.
///
/// Each one is a DROP, and the caller counts them together: they are all "an error arrived that this
/// host could not attribute", and splitting the counter by cause would be a dynamically keyed
/// counter a flood chooses the keys of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuoteRefusal {
	/// The message type is not one of the four errors this host demultiplexes.
	NotAnError,
	/// The message stops before the quotation, or the quoted IPv6 header is not one.
	Truncated,
	/// The quoted source is not an address this interface holds. Somebody else's complaint.
	NotOurs,
	/// The quotation stops before the transport identity this host demultiplexes on.
	Short,
}

/// Recover the transport identity from the quoted packet's body, as far as the quotation goes.
///
/// `body` is what follows the quoted IPv6 header. A protocol this host does not demultiplex is
/// `Other` at ANY length - there is nothing to truncate - while a protocol it does demultiplex is
/// refused when the quotation stops short of the fields that identify a flow.
pub fn transport(body: &[u8], next_header: u8) -> Result<QuotedTransport, QuoteRefusal> {
	match next_header {
		ipv6_packet::NEXT_TCP => {
			if body.len() < TCP_QUOTE_LEN {
				return Err(QuoteRefusal::Short);
			}
			Ok(QuotedTransport::Tcp { source_port: u16::from_be_bytes([body[0], body[1]]), destination_port: u16::from_be_bytes([body[2], body[3]]), sequence: u32::from_be_bytes([body[4], body[5], body[6], body[7]]) })
		}
		ipv6_packet::NEXT_UDP => {
			if body.len() < UDP_QUOTE_LEN {
				return Err(QuoteRefusal::Short);
			}
			Ok(QuotedTransport::Udp { source_port: u16::from_be_bytes([body[0], body[1]]), destination_port: u16::from_be_bytes([body[2], body[3]]) })
		}
		ipv6_packet::NEXT_ICMPV6 => {
			if body.len() < ECHO_QUOTE_LEN {
				return Err(QuoteRefusal::Short);
			}
			// AND IT MUST BE A REQUEST. The identifier and sequence sit at the same offsets in every
			// ICMPv6 message that has them, so reading them unconditionally would give a quoted
			// listener report an "echo identity" and hand it to a probe that never sent it. A quoted
			// message of another kind is still a real event; it just carries no echo identity.
			if body[0] != ipv6_icmp::ECHO_REQUEST || body[1] != 0 {
				return Ok(QuotedTransport::Other { next_header });
			}
			Ok(QuotedTransport::Icmpv6Echo { identifier: u16::from_be_bytes([body[4], body[5]]), sequence: u16::from_be_bytes([body[6], body[7]]) })
		}
		other => Ok(QuotedTransport::Other { next_header: other }),
	}
}

/// Turn a whole ICMPv6 error message into the event its consumer demultiplexes, or say why not.
///
/// `holds` is the set of addresses this interface currently has. The quoted source must be one of
/// them: that check drops a complaint about somebody else's packet, which is worth having and is not
/// the same as proving this host sent the quoted one.
pub fn validate(interface: Interface, reporter: Address, message: &[u8], holds: &[Address]) -> Result<QuotedError, QuoteRefusal> {
	if message.len() < ERROR_HEADER_LEN + ipv6_packet::HEADER_LEN {
		return Err(QuoteRefusal::Truncated);
	}
	let class = match message[0] {
		ipv6_icmp::DESTINATION_UNREACHABLE => ErrorClass::DestinationUnreachable { code: message[1] },
		ipv6_icmp::PACKET_TOO_BIG => ErrorClass::PacketTooBig { mtu: u32::from_be_bytes([message[4], message[5], message[6], message[7]]) },
		ipv6_icmp::TIME_EXCEEDED => ErrorClass::TimeExceeded { code: message[1] },
		ipv6_icmp::PARAMETER_PROBLEM => ErrorClass::ParameterProblem { code: message[1], pointer: u32::from_be_bytes([message[4], message[5], message[6], message[7]]) },
		_ => return Err(QuoteRefusal::NotAnError),
	};
	let quoted = &message[ERROR_HEADER_LEN..];
	let Ok(header) = ipv6_packet::header(quoted) else {
		return Err(QuoteRefusal::Truncated);
	};
	if !holds.contains(&header.source) {
		return Err(QuoteRefusal::NotOurs);
	}
	let transport = transport(&quoted[ipv6_packet::HEADER_LEN..], header.next_header)?;
	Ok(QuotedError { interface, reporter, class, quoted_source: header.source, quoted_destination: header.destination, transport })
}

/// The bound a TCP consumer must apply to a quoted sequence before acting on the error.
///
/// FROZEN HERE, IMPLEMENTED ABOVE. The rule is
/// [RFC 5927 section 4.1](https://www.rfc-editor.org/rfc/rfc5927.html#section-4.1)'s: the quoted
/// sequence must lie inside the interval that is actually on the wire, `[SND.UNA, SND.NXT)`, done in
/// sequence arithmetic so a wrapped interval is one interval and not two. The flight is less than
/// `2^31` by construction, which is what makes the subtraction unambiguous.
///
/// AN EMPTY FLIGHT REJECTS EVERYTHING, and falls out of the same expression rather than being a case:
/// `SND.NXT - SND.UNA` is zero and no unsigned value is below zero. That is the property that makes a
/// forged quotation useless against an idle connection.
///
/// Bytes accepted from the application but NOT YET SENT - including a packet still waiting for
/// address resolution - are not in the interval, because `SND.NXT` is the end of what was
/// transmitted. Retransmitted sequence space stays eligible: successive lower MTUs reported for the
/// same outstanding bytes are all valid, so there is deliberately no deduplication by sequence.
pub fn quotation_is_in_flight(quoted_sequence: u32, snd_una: u32, snd_nxt: u32) -> bool {
	quoted_sequence.wrapping_sub(snd_una) < snd_nxt.wrapping_sub(snd_una)
}

#[cfg(test)]
mod tests;
