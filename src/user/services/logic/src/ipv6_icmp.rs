//! ICMPv6: the echo pair, the four errors this host originates, and the two things that stop it.
//!
//! ICMPv6 IS NOT AN OPTIONAL PING CODEC. Path-MTU discovery, neighbour discovery and every "why did
//! that fail" answer ride on it, so a host that only replies to echo is a host whose TCP stalls on a
//! narrow path with no way to find out. This module owns the message layer; neighbour discovery
//! builds on it and lives next door.
//!
//! TWO THINGS STOP AN ERROR FROM BEING SENT, and they exist for different reasons. The RULES say an
//! error is never sent about another error, never about a packet from a source that is not a single
//! host, and - with two named exceptions - never about a packet sent to a group: without them, one
//! malformed multicast frame makes every host on the link answer at once. The RATE LIMIT says that
//! even when the rules permit an error, this host will not emit an unbounded stream of them: a flood
//! of unreachable destinations must not turn this guest into somebody else's amplifier.
//!
//! THE TWO EXEMPTIONS ARE STATED HERE BECAUSE A BLANKET RULE CONTRADICTS THE OPTION BITS. An unknown
//! option with action `10` says "report REGARDLESS of the destination"; action `11` says "report
//! unless it was multicast". A host that suppressed both could not implement `10` and could not be
//! told apart from one that ignored the bits. Packet Too Big is the second: a PMTU report is the
//! whole mechanism by which a sender learns a path is narrower, and a multicast destination does not
//! change that.

use crate::ipv6::{Address, Interface, Kind};
use crate::ipv6_events::ErrorClass;
use crate::ipv6_packet::{self, HEADER_LEN, MIN_MTU, NEXT_ICMPV6, OptionAction};
use alloc::vec::Vec;

/// ICMPv6 message types this host acts on.
pub const DESTINATION_UNREACHABLE: u8 = 1;
pub const PACKET_TOO_BIG: u8 = 2;
pub const TIME_EXCEEDED: u8 = 3;
pub const PARAMETER_PROBLEM: u8 = 4;
pub const ECHO_REQUEST: u8 = 128;
pub const ECHO_REPLY: u8 = 129;

/// The neighbour-discovery types, which are informational and never an error.
pub const ROUTER_SOLICITATION: u8 = 133;
pub const ROUTER_ADVERTISEMENT: u8 = 134;
pub const NEIGHBOUR_SOLICITATION: u8 = 135;
pub const NEIGHBOUR_ADVERTISEMENT: u8 = 136;
pub const REDIRECT: u8 = 137;

/// The multicast listener types.
pub const MLD_QUERY: u8 = 130;
pub const MLD_REPORT_V2: u8 = 143;

/// The fixed part of every ICMPv6 message: type, code, checksum.
pub const MESSAGE_HEADER_LEN: usize = 4;

/// How much of the invoking packet an error quotes.
///
/// BOUNDED, AND BOUNDED BY THE MINIMUM MTU RATHER THAN THE PATH'S. An error must itself fit in the
/// smallest thing any path can carry, or the report about a narrow path cannot get through it.
pub const MAX_QUOTE: usize = MIN_MTU as usize - HEADER_LEN - MESSAGE_HEADER_LEN - 4;

/// The default rate this host originates errors at, in errors per second.
pub const DEFAULT_ERROR_RATE: u32 = 10;

/// The burst the bucket holds.
pub const ERROR_BURST: u32 = 20;

/// The smallest and largest configured rate this host will accept.
pub const MIN_ERROR_RATE: u32 = 1;
pub const MAX_ERROR_RATE: u32 = 1000;

/// The floor no path-MTU record may go below. A path that cannot carry 1280 cannot carry IPv6.
pub const PATH_MTU_FLOOR: u32 = MIN_MTU as u32;

/// How long a path-MTU record lives after the last ACCEPTED lowering, in milliseconds.
pub const PATH_MTU_LIFETIME_MS: u64 = 600_000;

/// Is this message type an error rather than an informational message?
///
/// The type's top bit is the answer, which is the definition rather than a list: 0..127 are errors,
/// 128..255 are informational. A host that kept a list would eventually meet a type it had not
/// heard of and guess wrong about whether to answer it.
pub fn is_error(message_type: u8) -> bool {
	message_type < 128
}

/// A token bucket over originated errors.
///
/// SATURATING REFILL, CAPPED AT THE BURST. A host that was idle for an hour does not get 36000
/// errors to send; it gets the burst. That is the property that makes the limit a limit rather than
/// a delay.
#[derive(Clone, Copy, Debug)]
pub struct RateLimiter {
	rate: u32,
	tokens: u32,
	last_refill_ms: u64,
	limited: u32,
}

impl RateLimiter {
	/// A limiter at `rate` errors per second, starting full.
	pub fn new(rate: u32) -> RateLimiter {
		RateLimiter { rate: rate.clamp(MIN_ERROR_RATE, MAX_ERROR_RATE), tokens: ERROR_BURST, last_refill_ms: 0, limited: 0 }
	}

	/// Read `net.icmpv6-error-rate`, ONCE, at startup.
	///
	/// An absent value, a value that is not an integer, and a value outside 1..1000 all give the
	/// default. A configuration this host cannot honour is not a reason to have no limit.
	pub fn from_config(value: Option<&str>) -> RateLimiter {
		let parsed = value.and_then(|text| text.trim().parse::<u32>().ok()).filter(|rate| (MIN_ERROR_RATE..=MAX_ERROR_RATE).contains(rate));
		RateLimiter::new(parsed.unwrap_or(DEFAULT_ERROR_RATE))
	}

	/// The configured rate, after clamping.
	pub fn rate(&self) -> u32 {
		self.rate
	}

	/// May one error be originated at `now_ms`? Spends a token when it may.
	pub fn allow(&mut self, now_ms: u64) -> bool {
		self.refill(now_ms);
		if self.tokens == 0 {
			self.limited = self.limited.saturating_add(1);
			return false;
		}
		self.tokens -= 1;
		true
	}

	fn refill(&mut self, now_ms: u64) {
		let elapsed = now_ms.saturating_sub(self.last_refill_ms);
		if elapsed == 0 {
			return;
		}
		let earned = elapsed.saturating_mul(u64::from(self.rate)) / 1000;
		if earned == 0 {
			// Not a whole token yet: leave the clock where it is so the fraction is not lost to
			// integer division on every call.
			return;
		}
		self.tokens = (u64::from(self.tokens).saturating_add(earned).min(u64::from(ERROR_BURST))) as u32;
		self.last_refill_ms = now_ms;
	}

	/// How many errors were suppressed by the limit. Aggregate, with no per-packet log.
	pub fn limited(&self) -> u32 {
		self.limited
	}
}

/// Why an error was not originated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Suppressed {
	/// The invoking packet was itself an ICMPv6 error. Answering one would make two hosts talk to
	/// each other forever.
	TriggerWasError,
	/// The invoking packet's source is not a single host: a multicast group, the unspecified
	/// address, or a form this host does not act on. There is nobody to tell.
	SourceNotUnique,
	/// The invoking packet was addressed to a group, and this error class is not one of the two the
	/// standard exempts.
	MulticastDestination,
	/// The rules permit it and the bucket is empty.
	RateLimited,
}

/// What the caller wants to say, and about what.
///
/// `option_action` is carried for Parameter Problem because the unknown-option bits, not the
/// destination, decide the multicast question for that one case.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Origination {
	pub class: ErrorClass,
	/// The source of the packet that provoked it - who would be told.
	pub trigger_source: Address,
	/// The destination of the packet that provoked it.
	pub trigger_destination: Address,
	/// The invoking packet's upper-layer type, when it was ICMPv6, so an error about an error is
	/// refused.
	pub trigger_icmp_type: Option<u8>,
	/// For a Parameter Problem raised by an unknown option, the action bits that raised it.
	pub option_action: Option<OptionAction>,
}

/// May this host originate `origination` at `now_ms`, and spend a token if so?
///
/// The order is the order of cost: the free structural rules first, the bucket last, so a packet
/// that was never going to be answered does not spend a token.
pub fn may_originate(origination: &Origination, limiter: &mut RateLimiter, now_ms: u64) -> Result<(), Suppressed> {
	if origination.trigger_icmp_type.is_some_and(is_error) {
		return Err(Suppressed::TriggerWasError);
	}
	if !origination.trigger_source.valid_source() {
		return Err(Suppressed::SourceNotUnique);
	}
	if origination.trigger_destination.kind() == Kind::Multicast && !exempt_from_multicast_rule(origination) {
		return Err(Suppressed::MulticastDestination);
	}
	if !limiter.allow(now_ms) {
		return Err(Suppressed::RateLimited);
	}
	Ok(())
}

/// The two cases the standard exempts from the multicast suppression rule.
fn exempt_from_multicast_rule(origination: &Origination) -> bool {
	match origination.class {
		// A PMTU report is the whole mechanism by which a sender learns the path is narrower.
		ErrorClass::PacketTooBig { .. } => true,
		// An unknown option whose action bits say to report REGARDLESS of the destination.
		ErrorClass::ParameterProblem { .. } => origination.option_action == Some(OptionAction::DiscardAndReportAlways),
		_ => false,
	}
}

/// Build an ICMPv6 error message, quoting a bounded portion of the invoking packet.
///
/// The checksum is computed over the pseudo-header, so the message verifies only against the pair of
/// addresses it is actually sent between.
pub fn build_error(class: ErrorClass, source: Address, destination: Address, invoking: &[u8]) -> Vec<u8> {
	let (message_type, code, word) = match class {
		ErrorClass::DestinationUnreachable { code } => (DESTINATION_UNREACHABLE, code, 0),
		ErrorClass::PacketTooBig { mtu } => (PACKET_TOO_BIG, 0, mtu),
		ErrorClass::TimeExceeded { code } => (TIME_EXCEEDED, code, 0),
		ErrorClass::ParameterProblem { code, pointer } => (PARAMETER_PROBLEM, code, pointer),
	};
	let quote = &invoking[..invoking.len().min(MAX_QUOTE)];
	let mut message = Vec::with_capacity(MESSAGE_HEADER_LEN + 4 + quote.len());
	message.push(message_type);
	message.push(code);
	message.extend_from_slice(&[0, 0]);
	message.extend_from_slice(&word.to_be_bytes());
	message.extend_from_slice(quote);
	let checksum = ipv6_packet::pseudo_header_checksum(source, destination, NEXT_ICMPV6, &message);
	message[2..4].copy_from_slice(&checksum.to_be_bytes());
	message
}

/// An echo request or reply, decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Echo {
	pub identifier: u16,
	pub sequence: u16,
	/// Where the payload starts inside the message, so the caller can copy it without re-parsing.
	pub payload_offset: usize,
}

/// A validated echo reply, as the central dispatcher receives it.
///
/// EVERYTHING THE CONSUMER NEEDS AND NOTHING IT DOES NOT. The layer keeps no probe table: request
/// matching and round-trip timing belong to whoever sent the request, and it needs the identifier,
/// the sequence, both addresses, the interface identity and the hop limit the reply arrived with.
/// The last of those is how a caller notices a path that changed length underneath it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EchoReply {
	pub interface: Interface,
	pub source: Address,
	pub destination: Address,
	pub identifier: u16,
	pub sequence: u16,
	pub hop_limit: u8,
}

/// Why a message was not accepted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IcmpRefusal {
	/// Shorter than the fixed message header.
	TooShort,
	/// The checksum over the pseudo-header does not verify: the message was not sent between these
	/// two addresses, or it was damaged.
	BadChecksum,
	/// The type is not one this host acts on.
	UnknownType,
}

/// Verify an ICMPv6 message's checksum against the addresses it arrived between.
pub fn verify(source: Address, destination: Address, message: &[u8]) -> Result<(), IcmpRefusal> {
	if message.len() < MESSAGE_HEADER_LEN {
		return Err(IcmpRefusal::TooShort);
	}
	// The all-ones form is what a correct message sums to: the codec writes 0xffff rather than 0
	// for a computed zero, and a receiver folding the whole message gets the same value back.
	if ipv6_packet::pseudo_header_checksum(source, destination, NEXT_ICMPV6, message) != 0xffff {
		return Err(IcmpRefusal::BadChecksum);
	}
	Ok(())
}

/// Read an echo request or reply out of a verified message.
pub fn parse_echo(message: &[u8]) -> Result<Echo, IcmpRefusal> {
	if message.len() < MESSAGE_HEADER_LEN + 4 {
		return Err(IcmpRefusal::TooShort);
	}
	if message[0] != ECHO_REQUEST && message[0] != ECHO_REPLY {
		return Err(IcmpRefusal::UnknownType);
	}
	Ok(Echo { identifier: u16::from_be_bytes([message[4], message[5]]), sequence: u16::from_be_bytes([message[6], message[7]]), payload_offset: MESSAGE_HEADER_LEN + 4 })
}

/// Build the reply to an echo request: the same identifier, sequence and payload, type 129.
pub fn build_echo_reply(source: Address, destination: Address, request: &[u8]) -> Result<Vec<u8>, IcmpRefusal> {
	let echo = parse_echo(request)?;
	let mut message = Vec::with_capacity(request.len());
	message.push(ECHO_REPLY);
	message.push(0);
	message.extend_from_slice(&[0, 0]);
	message.extend_from_slice(&echo.identifier.to_be_bytes());
	message.extend_from_slice(&echo.sequence.to_be_bytes());
	message.extend_from_slice(&request[echo.payload_offset..]);
	let checksum = ipv6_packet::pseudo_header_checksum(source, destination, NEXT_ICMPV6, &message);
	message[2..4].copy_from_slice(&checksum.to_be_bytes());
	Ok(message)
}

/// Build an echo REQUEST: type 128, with the identity the caller will match its reply against.
///
/// THE IDENTIFIER AND THE SEQUENCE ARE THE CORRELATION, and both of them are needed. A traceroute is
/// a run of probes to one destination under one identifier, so the identifier alone matches every
/// hop's answer to whichever probe was looked at first - which is how a trace comes to name the
/// wrong router for every row.
pub fn build_echo_request(source: Address, destination: Address, identifier: u16, sequence: u16, payload: &[u8]) -> Vec<u8> {
	let mut message = Vec::with_capacity(MESSAGE_HEADER_LEN + 4 + payload.len());
	message.push(ECHO_REQUEST);
	message.push(0);
	message.extend_from_slice(&[0, 0]);
	message.extend_from_slice(&identifier.to_be_bytes());
	message.extend_from_slice(&sequence.to_be_bytes());
	message.extend_from_slice(payload);
	let checksum = ipv6_packet::pseudo_header_checksum(source, destination, NEXT_ICMPV6, &message);
	message[2..4].copy_from_slice(&checksum.to_be_bytes());
	message
}

/// One path-MTU record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PathMtu {
	pub destination: Address,
	pub interface: Interface,
	pub mtu: u32,
	/// When this record stops being believed, in milliseconds.
	pub expires_at_ms: u64,
}

/// What a `record` attempt did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MtuOutcome {
	/// The value lowered the current one and was written, with the new expiry.
	Lowered { mtu: u32 },
	/// The report did not lower anything. Nothing was written and the expiry was NOT refreshed - a
	/// router that keeps repeating the same MTU must not be able to keep a record alive forever.
	NotLower,
	/// The table is full of live records. The caller keeps its own smaller transmit limit and may
	/// retry once a record expires; it is NOT told the lowering was recorded.
	Capacity,
}

/// The bounded path-MTU cache.
///
/// THE WRITE HAPPENS HERE AND ONLY FOR A CONSUMER THAT ASKED. The layer below validates a Packet Too
/// Big message as far as it can - the type, the code, and that the quoted source is an address this
/// interface holds - but it cannot prove this host sent the quoted packet, because that is a lookup
/// in flow state it does not keep. The consumer that owns the flow makes that check and then asks
/// for the write. Anything else lets a stranger's quotation lower this host's path MTU.
#[derive(Debug, Default)]
pub struct PathMtuCache {
	entries: Vec<PathMtu>,
}

impl PathMtuCache {
	pub fn new() -> PathMtuCache {
		PathMtuCache::default()
	}

	/// Drop records whose expiry has passed. Called before an admission so a full table of dead
	/// records does not refuse a live one.
	pub fn expire(&mut self, now_ms: u64) -> usize {
		let before = self.entries.len();
		self.entries.retain(|entry| entry.expires_at_ms > now_ms);
		before - self.entries.len()
	}

	/// Record a lowering asked for by the consumer that owns the flow.
	pub fn record(&mut self, interface: Interface, destination: Address, reported: u32, now_ms: u64) -> MtuOutcome {
		let mtu = reported.max(PATH_MTU_FLOOR);
		self.expire(now_ms);
		if let Some(entry) = self.entries.iter_mut().find(|held| held.destination == destination && held.interface == interface) {
			if mtu >= entry.mtu {
				return MtuOutcome::NotLower;
			}
			entry.mtu = mtu;
			entry.expires_at_ms = now_ms + PATH_MTU_LIFETIME_MS;
			return MtuOutcome::Lowered { mtu };
		}
		// A first report about a destination lowers only if it is below the link's own maximum,
		// which the caller expresses by not asking otherwise. The floor still applies.
		if self.entries.len() as u32 >= crate::ipv6_budget::Resource::PathMtu.limit() {
			return MtuOutcome::Capacity;
		}
		self.entries.push(PathMtu { destination, interface, mtu, expires_at_ms: now_ms + PATH_MTU_LIFETIME_MS });
		MtuOutcome::Lowered { mtu }
	}

	/// The MTU to use for `destination`, or `None` when nothing has lowered it.
	pub fn get(&self, interface: Interface, destination: Address, now_ms: u64) -> Option<u32> {
		self.entries.iter().find(|entry| entry.destination == destination && entry.interface == interface && entry.expires_at_ms > now_ms).map(|entry| entry.mtu)
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}
}

#[cfg(test)]
mod tests;
