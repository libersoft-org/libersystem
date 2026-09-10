// What an IPv6 address IS, and what a value that carries no scope cannot be trusted to say.
//
// These are the rules every layer above depends on: a link-local address without an interface is
// not an address, a multicast group never sources a packet, a prefix that is not canonicalised is
// two records for one range, and the interface identifier must not be the NIC's hardware address.

use super::*;

fn address(text: [u16; 8]) -> Address {
	let mut bytes = [0u8; 16];
	for (index, group) in text.iter().enumerate() {
		bytes[index * 2..index * 2 + 2].copy_from_slice(&group.to_be_bytes());
	}
	Address(bytes)
}

#[test]
fn every_address_lands_in_exactly_one_kind() {
	assert_eq!(UNSPECIFIED.kind(), Kind::Unspecified);
	assert_eq!(LOOPBACK.kind(), Kind::Loopback);
	assert_eq!(address([0xfe80, 0, 0, 0, 0, 0, 0, 1]).kind(), Kind::LinkLocalUnicast);
	assert_eq!(address([0xfebf, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff]).kind(), Kind::LinkLocalUnicast, "the whole /10, not just fe80::/16");
	assert_eq!(ALL_NODES.kind(), Kind::Multicast);
	assert_eq!(address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]).kind(), Kind::GlobalUnicast);
	assert_eq!(address([0xfd00, 0, 0, 0, 0, 0, 0, 1]).kind(), Kind::GlobalUnicast, "unique-local is routable unicast");
	// The forms this host does not act on.
	assert_eq!(address([0, 0, 0, 0, 0, 0xffff, 0x0102, 0x0304]).kind(), Kind::Reserved, "IPv4-mapped");
	assert_eq!(address([0, 0, 0, 0, 0, 0, 0x0102, 0x0304]).kind(), Kind::Reserved, "IPv4-compatible");
	assert_eq!(address([0xfec0, 0, 0, 0, 0, 0, 0, 1]).kind(), Kind::Reserved, "the withdrawn site-local prefix");
}

#[test]
fn a_multicast_group_is_never_a_source_and_the_unspecified_is_never_a_destination() {
	assert!(!ALL_NODES.valid_source(), "a group does not send");
	assert!(ALL_NODES.valid_destination());
	assert!(!UNSPECIFIED.valid_source(), "only the DAD path may use it, and it says so separately");
	assert!(!UNSPECIFIED.valid_destination());
	assert!(!LOOPBACK.valid_destination(), "this stack has no loopback interface");
	let global = address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]);
	assert!(global.valid_source() && global.valid_destination());
}

#[test]
fn the_solicited_node_group_takes_the_low_twenty_four_bits() {
	let target = address([0x2001, 0xdb8, 0, 0, 0, 0, 0x1234, 0x5678]);
	let group = target.solicited_node();
	assert_eq!(group.octets()[..13], [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xff]);
	assert_eq!(&group.octets()[13..], &[0x34, 0x56, 0x78]);
	assert!(group.is_solicited_node_of(target));
	// Two addresses that differ ABOVE the low 24 bits share a group, which is the point: the group
	// narrows the wake-up, it does not identify the host.
	let other = address([0x2001, 0xdb8, 0, 0, 0x9999, 0x9999, 0x9934, 0x5678]);
	assert_eq!(other.solicited_node(), group);
	assert_eq!(group.multicast_ethernet(), Some([0x33, 0x33, 0xff, 0x34, 0x56, 0x78]));
	assert_eq!(address([0x2001, 0, 0, 0, 0, 0, 0, 1]).multicast_ethernet(), None);
}

#[test]
fn multicast_scope_reads_the_four_bits_and_names_only_what_this_host_acts_on() {
	assert_eq!(ALL_NODES.multicast_scope(), Some(MulticastScope::LinkLocal));
	assert_eq!(address([0xff01, 0, 0, 0, 0, 0, 0, 1]).multicast_scope(), Some(MulticastScope::InterfaceLocal));
	assert_eq!(address([0xff0e, 0, 0, 0, 0, 0, 0, 1]).multicast_scope(), Some(MulticastScope::Global));
	assert_eq!(address([0xff03, 0, 0, 0, 0, 0, 0, 1]).multicast_scope(), Some(MulticastScope::Other(3)));
	assert_eq!(LOOPBACK.multicast_scope(), None);
}

#[test]
fn a_link_local_address_without_an_interface_is_refused() {
	let link_local = address([0xfe80, 0, 0, 0, 0, 0, 0, 1]);
	assert!(Scoped::new(link_local, None).is_none(), "fe80::1 on two links is two hosts");
	assert!(Scoped::new(ALL_NODES, None).is_none(), "a link-scoped group needs the link too");
	let global = address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]);
	assert!(Scoped::new(global, None).is_some(), "a global address means the same everywhere");

	let first = Scoped::new(link_local, Some(Interface::new(0, 1))).expect("scoped");
	let same_link = Scoped::new(link_local, Some(Interface::new(0, 1))).expect("scoped");
	let replaced_nic = Scoped::new(link_local, Some(Interface::new(0, 2))).expect("scoped");
	assert!(first.same_target(&same_link));
	assert!(!first.same_target(&replaced_nic), "a new generation is a new link, whatever the index says");
}

#[test]
fn a_prefix_is_canonical_so_one_range_is_one_record() {
	let dirty = Prefix::new(address([0x2001, 0xdb8, 0, 0, 0xdead, 0xbeef, 0, 1]), 64).expect("prefix");
	let clean = Prefix::new(address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0]), 64).expect("prefix");
	assert_eq!(dirty, clean, "the bits below the length are cleared on the way in");
	assert!(dirty.contains(address([0x2001, 0xdb8, 0, 0, 1, 2, 3, 4])));
	assert!(!dirty.contains(address([0x2001, 0xdb8, 0, 1, 0, 0, 0, 0])));
	assert!(Prefix::new(UNSPECIFIED, 129).is_none(), "there is no bit 129");
	assert!(Prefix::new(UNSPECIFIED, 128).is_some());

	// A non-byte-aligned length masks inside the byte.
	let odd = Prefix::new(address([0x2001, 0xdbff, 0, 0, 0, 0, 0, 0]), 28).expect("prefix");
	assert_eq!(odd.base().groups()[1], 0xdbf0, "the low four bits of the fourth byte are cleared, not the whole group");

	assert_eq!(Prefix::link_local().len(), 10);
	assert!(Prefix::link_local().contains(address([0xfebf, 0, 0, 0, 0, 0, 0, 1])));
	assert!(!Prefix::link_local().contains(address([0xfec0, 0, 0, 0, 0, 0, 0, 1])));
}

#[test]
fn only_a_slash_64_carries_an_interface_identifier() {
	let sixty_four = Prefix::new(address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0]), 64).expect("prefix");
	let formed = sixty_four.with_interface_id([1, 2, 3, 4, 5, 6, 7, 8]).expect("a /64 can");
	assert_eq!(&formed.octets()[8..], &[1, 2, 3, 4, 5, 6, 7, 8]);
	assert_eq!(&formed.octets()[..8], &sixty_four.base().octets()[..8]);
	let fifty_six = Prefix::new(address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0]), 56).expect("prefix");
	assert!(fifty_six.with_interface_id([1, 2, 3, 4, 5, 6, 7, 8]).is_none(), "SLAAC's autonomous flag is defined for /64 alone");
}

#[test]
fn common_prefix_len_counts_bits_and_not_bytes() {
	let left = address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]);
	assert_eq!(left.common_prefix_len(left), 128);
	assert_eq!(left.common_prefix_len(address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 2])), 126);
	assert_eq!(left.common_prefix_len(address([0xa001, 0, 0, 0, 0, 0, 0, 0])), 0, "the very first bit differs");
	assert_eq!(left.common_prefix_len(address([0x3001, 0, 0, 0, 0, 0, 0, 0])), 3, "0x20 ^ 0x30 is 0x10, which has three leading zeros");
}

#[test]
fn the_interface_identifier_is_locally_administered_and_never_the_hardware_address() {
	let drawn = [0x9au8, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde];
	let id = interface_identifier(drawn).expect("an ordinary draw is accepted");
	assert_eq!(id[0] & 0x02, 0, "locally administered: nothing claims a globally unique origin");
	assert_eq!(&id[1..], &drawn[1..], "and nothing else is changed");

	// AND IT IS NOT THE MAC ADDRESS, which is the whole point. A modified-EUI-64 identifier from a
	// NIC with this hardware address would have these eight bytes; a drawn one must not.
	let mac = [0x52u8, 0x54, 0x00, 0x12, 0x34, 0x56];
	let eui64 = [mac[0] ^ 0x02, mac[1], mac[2], 0xff, 0xfe, mac[3], mac[4], mac[5]];
	assert_ne!(id, eui64);
	assert_ne!(&id[3..5], &[0xff, 0xfe], "no embedded EUI-48 marker");

	// The reserved forms are refused so the caller draws again rather than being handed a silent
	// substitution.
	assert_eq!(interface_identifier([0; 8]), None, "the subnet-router anycast address");
	assert_eq!(interface_identifier([0x02, 0x00, 0x5e, 0xfe, 0x00, 1, 2, 3]), None, "the IANA reserved range");
	assert_eq!(interface_identifier([0x00, 0x00, 0x5e, 0xfe, 0x01, 1, 2, 3]), None, "the range does not depend on the fifth byte");
	assert_eq!(interface_identifier([0x00, 0x00, 0x5e, 0xff, 0x01, 1, 2, 3]), Some([0x00, 0x00, 0x5e, 0xff, 0x01, 1, 2, 3]), "one byte outside it is fine");
	// A draw whose only problem is the universal bit becomes the all-zero identifier and is refused
	// on that ground, which is the case a check on the RAW draw would have missed.
	assert_eq!(interface_identifier([0x02, 0, 0, 0, 0, 0, 0, 0]), None);
}

#[test]
fn the_debug_form_is_the_canonical_text() {
	extern crate alloc;
	use alloc::format;
	assert_eq!(format!("{:?}", UNSPECIFIED), "::");
	assert_eq!(format!("{:?}", LOOPBACK), "::1");
	assert_eq!(format!("{:?}", ALL_NODES), "ff02::1");
	assert_eq!(format!("{:?}", address([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1])), "2001:db8::1");
	assert_eq!(format!("{:?}", address([0x2001, 0xdb8, 0, 1, 0, 0, 0, 1])), "2001:db8:0:1::1");
	assert_eq!(format!("{:?}", address([1, 2, 3, 4, 5, 6, 7, 8])), "1:2:3:4:5:6:7:8", "no run to shorten");
	assert_eq!(format!("{:?}", address([0x2001, 0, 0, 1, 0, 0, 0, 0])), "2001:0:0:1::", "the longer run wins, and a trailing run keeps its colons");
	assert_eq!(format!("{:?}", address([1, 0, 2, 0, 3, 0, 4, 0])), "1:0:2:0:3:0:4:0", "a single zero group is not shortened");
}
