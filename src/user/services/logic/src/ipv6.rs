//! IPv6 addresses, prefixes and the scope that gives a link-local one its meaning.
//!
//! WHY A TYPE AND NOT `[u8; 16]`. Half of what goes wrong with IPv6 in a small stack is an address
//! used out of the scope it belongs to: a link-local source sent to a global destination, a
//! neighbour cached under an address that means something different on another interface, a
//! multicast group treated as a unicast peer. A raw array carries none of that, so every call site
//! has to remember it. These types carry it instead, and the classification below is the one place
//! the rules are written.
//!
//! WHAT THIS MODULE REFUSES TO DECIDE. It does not choose an address for a boot, does not talk to a
//! router, and holds no state at all. Everything here is a function of its arguments, which is what
//! lets it be tested on a host.

use core::fmt;

/// A 128-bit IPv6 address, held in network byte order.
///
/// `Copy` and comparable so a cache can key on it; ordering is over the bytes as they appear on the
/// wire, which is also the order a longest-prefix walk wants.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Address(pub [u8; 16]);

/// The unspecified address `::`, which is a source only during duplicate-address detection.
pub const UNSPECIFIED: Address = Address([0; 16]);

/// The loopback address `::1`.
pub const LOOPBACK: Address = Address([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

/// `ff02::1`, every node on the link.
pub const ALL_NODES: Address = Address([0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

/// `ff02::2`, every router on the link.
pub const ALL_ROUTERS: Address = Address([0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);

/// `ff02::16`, the MLDv2 report destination.
pub const ALL_MLDV2_ROUTERS: Address = Address([0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16]);

/// The `fe80::/10` link-local unicast prefix length.
const LINK_LOCAL_PREFIX_LEN: u8 = 10;

/// The solicited-node prefix `ff02::1:ff00:0/104`.
const SOLICITED_NODE_PREFIX: [u8; 13] = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xff];

/// What an address IS, which decides what it may be used for.
///
/// The classification is exhaustive: every one of the 2^128 values lands in exactly one arm, so a
/// caller that matches on it has considered everything. `Reserved` is where the values this host has
/// no use for go - it is not "invalid", it is "not one of the kinds this stack acts on", and using
/// one as a source or destination is refused at the point of use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	/// `::` - a source during duplicate-address detection and nothing else.
	Unspecified,
	/// `::1` - this host, never seen on a link.
	Loopback,
	/// `fe80::/10` - meaningful only together with the interface it was learned on.
	LinkLocalUnicast,
	/// `ff00::/8` - a group, never a unicast peer.
	Multicast,
	/// Ordinary routable unicast, including unique-local `fc00::/7`.
	GlobalUnicast,
	/// The embedded-IPv4 forms, the withdrawn site-local prefix, and everything else this host does
	/// not act on.
	Reserved,
}

/// Where a multicast group reaches, out of the four scope bits the address carries.
///
/// Only the values this host acts on are named; anything else is `Other`, which is a refusal at the
/// point of use rather than a value to guess about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MulticastScope {
	InterfaceLocal,
	LinkLocal,
	AdminLocal,
	SiteLocal,
	Global,
	Other(u8),
}

impl Address {
	/// An address from its sixteen wire bytes.
	pub const fn new(bytes: [u8; 16]) -> Address {
		Address(bytes)
	}

	/// The wire bytes.
	pub const fn octets(&self) -> [u8; 16] {
		self.0
	}

	/// The address as eight 16-bit groups, which is the form the textual notation uses.
	pub fn groups(&self) -> [u16; 8] {
		let mut out = [0u16; 8];
		for (index, group) in out.iter_mut().enumerate() {
			*group = u16::from_be_bytes([self.0[index * 2], self.0[index * 2 + 1]]);
		}
		out
	}

	/// What this address is. See `Kind`.
	pub fn kind(&self) -> Kind {
		if *self == UNSPECIFIED {
			return Kind::Unspecified;
		}
		if *self == LOOPBACK {
			return Kind::Loopback;
		}
		if self.0[0] == 0xff {
			return Kind::Multicast;
		}
		if self.0[0] == 0xfe && (self.0[1] & 0xc0) == 0x80 {
			return Kind::LinkLocalUnicast;
		}
		// THE DEPRECATED FORMS ARE RESERVED, NOT GLOBAL. `::/96` carries IPv4-compatible addresses
		// and `::ffff:0:0/96` IPv4-mapped ones; neither is a v6 peer this host may put on the wire,
		// and both start with eighty zero bits, which is why they are checked before the catch-all.
		if self.0[..8] == [0; 8] {
			return Kind::Reserved;
		}
		// `fec0::/10` is the withdrawn site-local prefix. A host that treated it as global would be
		// routing by a rule that no longer exists.
		if self.0[0] == 0xfe && (self.0[1] & 0xc0) == 0xc0 {
			return Kind::Reserved;
		}
		Kind::GlobalUnicast
	}

	/// May a packet this host sends be addressed TO this?
	///
	/// The unspecified address never is, and neither is a form this host does not act on. Loopback
	/// is excluded because this stack has no loopback interface: a packet to `::1` would go on the
	/// wire, which is exactly the mistake worth refusing.
	pub fn valid_destination(&self) -> bool {
		matches!(self.kind(), Kind::LinkLocalUnicast | Kind::Multicast | Kind::GlobalUnicast)
	}

	/// May this host put this in the SOURCE field?
	///
	/// A multicast group never sources a packet. The unspecified address does, but only during
	/// duplicate-address detection, and that caller says so with `Scoped::unspecified_for_dad`
	/// rather than by passing this check.
	pub fn valid_source(&self) -> bool {
		matches!(self.kind(), Kind::LinkLocalUnicast | Kind::GlobalUnicast)
	}

	/// The scope of a multicast group, or `None` for an address that is not one.
	pub fn multicast_scope(&self) -> Option<MulticastScope> {
		if self.kind() != Kind::Multicast {
			return None;
		}
		Some(match self.0[1] & 0x0f {
			1 => MulticastScope::InterfaceLocal,
			2 => MulticastScope::LinkLocal,
			4 => MulticastScope::AdminLocal,
			5 => MulticastScope::SiteLocal,
			14 => MulticastScope::Global,
			other => MulticastScope::Other(other),
		})
	}

	/// The solicited-node multicast group for this address: `ff02::1:ff` and the low 24 bits.
	///
	/// Neighbour discovery listens on this group rather than on all-nodes, so a link with many hosts
	/// does not wake every one of them for every solicitation.
	pub fn solicited_node(&self) -> Address {
		let mut bytes = [0u8; 16];
		bytes[..13].copy_from_slice(&SOLICITED_NODE_PREFIX);
		bytes[13] = self.0[13];
		bytes[14] = self.0[14];
		bytes[15] = self.0[15];
		Address(bytes)
	}

	/// Is this the solicited-node group of `target`?
	pub fn is_solicited_node_of(&self, target: Address) -> bool {
		*self == target.solicited_node()
	}

	/// How many leading bits this address shares with `other`, at most 128.
	///
	/// Longest-prefix selection and the source-address rules both count matching bits, and counting
	/// them wrong is a routing decision made on the wrong evidence.
	pub fn common_prefix_len(&self, other: Address) -> u8 {
		let mut count = 0u8;
		for index in 0..16 {
			let difference = self.0[index] ^ other.0[index];
			if difference == 0 {
				count += 8;
				continue;
			}
			count += difference.leading_zeros() as u8;
			break;
		}
		count
	}

	/// The Ethernet destination a multicast address maps to: `33:33` and the low four bytes.
	pub fn multicast_ethernet(&self) -> Option<[u8; 6]> {
		if self.kind() != Kind::Multicast {
			return None;
		}
		Some([0x33, 0x33, self.0[12], self.0[13], self.0[14], self.0[15]])
	}
}

impl fmt::Debug for Address {
	/// The canonical text form (RFC 5952): lowercase hex, no leading zeros in a group, and the
	/// longest run of two or more zero groups replaced by `::`, the leftmost run winning a tie.
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		let groups = self.groups();
		let (mut best_start, mut best_len) = (0usize, 0usize);
		let (mut run_start, mut run_len) = (0usize, 0usize);
		for (index, group) in groups.iter().enumerate() {
			if *group == 0 {
				if run_len == 0 {
					run_start = index;
				}
				run_len += 1;
				if run_len > best_len {
					best_start = run_start;
					best_len = run_len;
				}
			} else {
				run_len = 0;
			}
		}
		if best_len < 2 {
			best_len = 0;
		}
		let mut index = 0usize;
		let mut wrote_any = false;
		while index < 8 {
			if best_len != 0 && index == best_start {
				formatter.write_str(if wrote_any { ":" } else { "::" })?;
				index += best_len;
				if index >= 8 && wrote_any {
					formatter.write_str(":")?;
				}
				continue;
			}
			if wrote_any {
				formatter.write_str(":")?;
			}
			write!(formatter, "{:x}", groups[index])?;
			wrote_any = true;
			index += 1;
		}
		Ok(())
	}
}

/// An interface identity: which link an address belongs to, and which incarnation of it.
///
/// GENERATION, NOT JUST INDEX. A NIC that is replaced keeps its index and loses every neighbour,
/// address and route learned on the previous one. Carrying the generation in the identity is what
/// makes a stale record impossible to mistake for a live one: an entry from generation 3 does not
/// answer a lookup on generation 4, and nothing has to remember to flush it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Interface {
	pub index: u16,
	pub generation: u32,
}

impl Interface {
	pub const fn new(index: u16, generation: u32) -> Interface {
		Interface { index, generation }
	}
}

/// An address together with the interface that gives it meaning.
///
/// A link-local address without one is not an address: `fe80::1` on two links is two different
/// hosts. Construction refuses that case, so a `Scoped` value can be used without re-checking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Scoped {
	address: Address,
	interface: Option<Interface>,
}

impl Scoped {
	/// A scoped address, or `None` when the pairing is meaningless: a link-local or link-scoped
	/// multicast address without an interface.
	pub fn new(address: Address, interface: Option<Interface>) -> Option<Scoped> {
		let needs_scope = matches!(address.kind(), Kind::LinkLocalUnicast) || matches!(address.multicast_scope(), Some(MulticastScope::InterfaceLocal | MulticastScope::LinkLocal));
		if needs_scope && interface.is_none() {
			return None;
		}
		Some(Scoped { address, interface })
	}

	/// The unspecified source a duplicate-address detection probe uses, on a named interface.
	///
	/// Spelled separately because `::` fails `valid_source` for every other caller, and a check with
	/// an exception nobody can see is a check that will be worked around.
	pub fn unspecified_for_dad(interface: Interface) -> Scoped {
		Scoped { address: UNSPECIFIED, interface: Some(interface) }
	}

	pub fn address(&self) -> Address {
		self.address
	}

	pub fn interface(&self) -> Option<Interface> {
		self.interface
	}

	/// Do these two name the same address on the same link?
	///
	/// Two link-local addresses equal as bytes are the SAME host only when they were learned on the
	/// same interface generation.
	pub fn same_target(&self, other: &Scoped) -> bool {
		self.address == other.address && self.interface == other.interface
	}
}

/// A prefix: an address and how many of its leading bits are significant.
///
/// Construction refuses a length above 128 and CLEARS the bits below it, so two prefixes naming the
/// same range are equal as values. A record keyed on an uncanonicalised prefix is a record with two
/// entries for one thing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Prefix {
	base: Address,
	len: u8,
}

impl Prefix {
	/// A prefix, or `None` when `len` exceeds 128.
	pub fn new(address: Address, len: u8) -> Option<Prefix> {
		if len > 128 {
			return None;
		}
		Some(Prefix { base: Address(mask_to(address.0, len)), len })
	}

	pub fn base(&self) -> Address {
		self.base
	}

	pub fn len(&self) -> u8 {
		self.len
	}

	/// Does `address` fall inside this prefix?
	pub fn contains(&self, address: Address) -> bool {
		mask_to(address.0, self.len) == self.base.0
	}

	/// The link-local prefix `fe80::/10`.
	pub fn link_local() -> Prefix {
		Prefix { base: Address([0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), len: LINK_LOCAL_PREFIX_LEN }
	}

	/// Form an address in this prefix from a 64-bit interface identifier.
	///
	/// Only a /64 can carry one: SLAAC's autonomous flag is defined for that length alone, and a
	/// prefix of any other length with an interface identifier is a configuration this host refuses
	/// rather than pads.
	pub fn with_interface_id(&self, id: [u8; 8]) -> Option<Address> {
		if self.len != 64 {
			return None;
		}
		let mut bytes = self.base.0;
		bytes[8..].copy_from_slice(&id);
		Some(Address(bytes))
	}
}

/// Clear every bit below `len`.
fn mask_to(mut bytes: [u8; 16], len: u8) -> [u8; 16] {
	let whole = (len / 8) as usize;
	let remainder = len % 8;
	if whole < 16 {
		if remainder != 0 {
			bytes[whole] &= 0xffu8 << (8 - remainder);
		} else {
			bytes[whole] = 0;
		}
		for byte in bytes.iter_mut().skip(whole + 1) {
			*byte = 0;
		}
	}
	bytes
}

/// The 64-bit interface identifier this host uses, and the ONE policy behind it.
///
/// NOT THE MAC ADDRESS, WHICH IS THE POINT. The modified-EUI-64 form embeds the NIC's hardware
/// address in every address the host forms, so the machine is recognisable on every network it
/// visits. That is the identifier RFC 7217 and RFC 8064 exist to stop exposing, and this host does
/// not form it.
///
/// THE POLICY, STATED ONCE: the identifier is sixty-four bits of kernel randomness, drawn ONCE PER
/// PREFIX and kept for as long as the host holds an address in that prefix. It follows that
///
///   - nothing about the NIC leaks, because no hardware value is an input;
///   - an observer on one network cannot recognise the host on another, because each prefix gets its
///     own draw;
///   - the address is stable while the host holds the prefix, so a peer sees one address rather than
///     a new one per packet.
///
/// AND THE LIMIT, WHICH IS REAL: it is NOT stable across reboots. RFC 7217 derives its identifier
/// from a secret that survives one, so a returning host keeps its address; this draws afresh, so it
/// does not. Keeping a secret across boots needs somewhere to keep it, which is a decision this
/// layer does not own - and the two privacy properties above, which are the ones the exposure
/// concern is about, hold either way.
///
/// This function takes the entropy rather than fetching it, so it stays a function of its arguments
/// and the caller's source of randomness is visible where the caller is.
pub fn interface_identifier(entropy: [u8; 8]) -> Option<[u8; 8]> {
	let mut id = entropy;
	// LOCALLY ADMINISTERED. This identifier is not derived from a globally unique hardware address
	// and must not claim to be: bit 1 of the first byte says which, and it is cleared.
	id[0] &= !0x02;
	// The reserved forms are refused rather than emitted. All-zero is the subnet-router anycast
	// address, and `0000:5efe` - which is `0200:5efe` with the bit above already cleared, and is why
	// this is checked AFTER clearing rather than on the raw draw - opens the IANA reserved range.
	// The caller draws again; refusing is what makes that the caller's decision rather than a silent
	// substitution here.
	if id == [0; 8] || id[..4] == [0x00, 0x00, 0x5e, 0xfe] {
		return None;
	}
	Some(id)
}

#[cfg(test)]
mod tests;
