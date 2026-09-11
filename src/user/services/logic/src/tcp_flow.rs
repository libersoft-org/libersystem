//! Which operation an ICMP error is about, and what a Packet Too Big is allowed to change.
//!
//! AN ERROR NAMES A PACKET, NOT A CONNECTION. What comes back is a quotation of something this host
//! sent, and turning that into "which of my operations does this concern" is a lookup nobody else can
//! do: the layer below holds no flow state by design, and the layer above holds one flow each. This
//! module is the rule both of them are written against.
//!
//! THE FULL TUPLE, THE FAMILY AND THE INTERFACE GENERATION. Equal port numbers in two families are
//! two different flows, and the same tuple on a replaced NIC is a third; a match on anything less
//! lets one flow's error terminate, resize or invalidate another's. That is the failure this exists
//! to prevent, and it is a correctness failure rather than a diagnostic one.
//!
//! AND A MATCH IS NOT AN AUTHENTICATION. Nothing here proves the quotation came from a router on the
//! path - an off-path sender that can guess a tuple and a sequence can forge one. What the checks
//! buy is that a quotation which does NOT match cannot do anything at all, and that one which does
//! can only ever lower a limit this flow already had. The delayed-report and sequence-reuse
//! limitations are the ones P02M0174 states.

use crate::tcp_bind::Local;

/// The smallest MTU each family's path is required to carry. A report below it is not a path.
pub const MIN_PATH_MTU_V4: u32 = 68;
pub const MIN_PATH_MTU_V6: u32 = 1280;

/// One live operation's identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FlowKey {
	pub local: Local,
	pub local_port: u16,
	pub remote: Local,
	pub remote_port: u16,
	/// Which generation of the interface this flow runs on. A replaced NIC is a different interface,
	/// and a flow keyed without this would inherit the errors of one that is gone.
	pub interface_generation: u32,
}

/// What the quoted packet's upper layer was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuotedKind {
	/// A quoted TCP header, with the sequence the send-bound check needs.
	Tcp {
		sequence: u32,
	},
	Udp,
	/// A quoted Echo Request. BOTH fields are part of the match: an identifier alone cannot tell two
	/// probes of the same trace apart, which is exactly what a traceroute is made of.
	Echo {
		identifier: u16,
		sequence: u16,
	},
}

/// A validated error, as the layer below hands it up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Quotation {
	/// The hop that complained. It is NOT the quoted destination and need not be this host's first
	/// hop; reporting the quoted destination as the responder is how a traceroute comes to name the
	/// wrong router for every hop.
	pub responder: Local,
	pub local: Local,
	pub local_port: u16,
	pub remote: Local,
	pub remote_port: u16,
	pub interface_generation: u32,
	pub transport: QuotedKind,
}

impl FlowKey {
	/// Is this quotation about THIS operation?
	pub fn owns(&self, quote: &Quotation) -> bool {
		self.local == quote.local && self.local_port == quote.local_port && self.remote == quote.remote && self.remote_port == quote.remote_port && self.interface_generation == quote.interface_generation
	}

	/// The floor on any path-MTU report for this flow's family.
	pub fn min_path_mtu(&self) -> u32 {
		match self.local.is_v4() {
			true => MIN_PATH_MTU_V4,
			false => MIN_PATH_MTU_V6,
		}
	}
}

/// Why a Packet Too Big changed nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ignored {
	/// It quotes a different flow, a different family, or an interface generation that is gone.
	NotThisFlow,
	/// The quoted packet is not TCP, so this rule is not the one that applies to it.
	NotTcp,
	/// The route this flow was using is no longer there: there is nothing to lower.
	RouteGone,
	/// Nothing is outstanding. An empty flight rejects every quotation, which is what makes a forged
	/// one useless against an idle connection.
	EmptyFlight,
	/// The quoted sequence is not in the flight this connection actually has on the wire.
	OutOfWindow,
	/// The report is not smaller than what this flow is already using.
	NotSmaller,
}

/// What to do about a Packet Too Big.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathMtu {
	/// Lower this flow's transmit limit to this, and resegment what is outstanding.
	Apply(u32),
	Ignored(Ignored),
}

/// Decide a Packet Too Big for a TCP flow.
///
/// EVERY CHECK BEFORE ANY WRITE. The order matters: an out-of-window quotation must not have already
/// counted against a cache, changed a flow-local limit or triggered a resegmentation by the time it
/// is refused.
#[allow(clippy::too_many_arguments)]
pub fn path_mtu_for_tcp(flow: &FlowKey, quote: &Quotation, snd_una: u32, snd_nxt: u32, route_live: bool, reported: u32, current: u32) -> PathMtu {
	if !flow.owns(quote) {
		return PathMtu::Ignored(Ignored::NotThisFlow);
	}
	let QuotedKind::Tcp { sequence } = quote.transport else {
		return PathMtu::Ignored(Ignored::NotTcp);
	};
	if !route_live {
		return PathMtu::Ignored(Ignored::RouteGone);
	}
	if snd_nxt == snd_una {
		return PathMtu::Ignored(Ignored::EmptyFlight);
	}
	// P02M0174 M6's frozen rule. `SND.NXT` is the end of ACTUALLY TRANSMITTED sequence space, so
	// accepted-but-unsent bytes - including a packet still waiting on address resolution - do not
	// extend it. SYN and FIN occupy their normal sequence space and are eligible like any byte.
	if !crate::ipv6_quote::quotation_is_in_flight(sequence, snd_una, snd_nxt) {
		return PathMtu::Ignored(Ignored::OutOfWindow);
	}
	// A REPORT BELOW THE FAMILY'S FLOOR IS RAISED TO IT rather than refused: the hop is telling the
	// truth about itself and the floor is what the protocol guarantees, so the smaller of the two
	// that is still legal is the answer.
	let bounded: u32 = reported.max(flow.min_path_mtu());
	if bounded >= current {
		return PathMtu::Ignored(Ignored::NotSmaller);
	}
	PathMtu::Apply(bounded)
}

/// One flow's own transmit limit.
///
/// IT IS THE FLOW'S AND NOT THE CACHE'S. The bounded path-MTU cache may refuse the write - it has a
/// capacity, and a full table says so rather than pretending it recorded the lowering. When that
/// happens the flow keeps the smaller limit it validated and applies it immediately: cache
/// exhaustion must never restore a larger limit the path has already refused to carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FlowLimit {
	limit: u32,
}

impl FlowLimit {
	pub fn new(limit: u32) -> FlowLimit {
		FlowLimit { limit }
	}

	pub fn limit(&self) -> u32 {
		self.limit
	}

	/// Lower it. Returns whether anything moved - a report that is not smaller changes nothing, so
	/// successive strictly lower reports each take effect and a repeat does not.
	pub fn lower(&mut self, mtu: u32) -> bool {
		if mtu >= self.limit {
			return false;
		}
		self.limit = mtu;
		true
	}
}

/// Which of several live probes an error is about.
///
/// A TRACEROUTE IS A SEQUENCE OF PROBES TO ONE DESTINATION with one identifier, so the tuple alone
/// matches all of them and the SEQUENCE is what tells them apart. An implementation matching on the
/// tuple would attribute every hop's answer to whichever probe it looked at first.
pub fn probe_owns(flow: &FlowKey, identifier: u16, sequence: u16, quote: &Quotation) -> bool {
	if !flow.owns(quote) {
		return false;
	}
	matches!(quote.transport, QuotedKind::Echo { identifier: quoted_id, sequence: quoted_seq } if quoted_id == identifier && quoted_seq == sequence)
}

#[cfg(test)]
mod tests;
