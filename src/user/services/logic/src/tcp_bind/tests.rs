//! The matrix, and the lookup written against the same rule.

use super::*;

const ANY4: Local = Local::V4([0, 0, 0, 0]);
const ANY6: Local = Local::V6([0; 16]);

fn v4(a: u8, b: u8, c: u8, d: u8) -> Local {
	Local::V4([a, b, c, d])
}

fn v6(low: u16) -> Local {
	let mut octets = [0u8; 16];
	octets[0] = 0x20;
	octets[1] = 0x01;
	octets[14..].copy_from_slice(&low.to_be_bytes());
	Local::V6(octets)
}

fn binding(mode: BindMode, address: Local, port: u16) -> Binding {
	Binding { mode, address, port }
}

#[test]
fn a_mode_and_an_address_must_agree_before_anything_else_is_considered() {
	assert_eq!(binding(BindMode::Ipv4Only, ANY4, 80).well_formed(), Ok(()));
	assert_eq!(binding(BindMode::Ipv4Only, v4(10, 0, 2, 15), 80).well_formed(), Ok(()));
	assert_eq!(binding(BindMode::Ipv6Only, ANY6, 80).well_formed(), Ok(()));
	assert_eq!(binding(BindMode::Ipv6Only, v6(1), 80).well_formed(), Ok(()));
	assert_eq!(binding(BindMode::DualStack, ANY6, 80).well_formed(), Ok(()));

	// A BIND COVERING TWO FAMILIES CANNOT NAME ONE ADDRESS IN ONE OF THEM.
	assert_eq!(binding(BindMode::DualStack, v6(1), 80).well_formed(), Err(BindRefusal::Mismatch));
	assert_eq!(binding(BindMode::DualStack, ANY4, 80).well_formed(), Err(BindRefusal::Mismatch));
	// And a mode whose family the address disagrees with is not a bind at all.
	assert_eq!(binding(BindMode::Ipv4Only, ANY6, 80).well_formed(), Err(BindRefusal::Mismatch));
	assert_eq!(binding(BindMode::Ipv6Only, ANY4, 80).well_formed(), Err(BindRefusal::Mismatch));
}

#[test]
fn an_ipv4_mapped_address_is_refused_in_every_mode() {
	// IT IS NOT A WAY TO EXPRESS AN IPv4 BIND. An IPv4 endpoint is expressible directly, so the
	// second spelling buys nothing and costs every consumer a check - and the one that forgets grants
	// IPv4 reach to a listener that asked for IPv6.
	let mut mapped = [0u8; 16];
	mapped[10] = 0xff;
	mapped[11] = 0xff;
	mapped[12..].copy_from_slice(&[10, 0, 2, 15]);
	let mapped = Local::V6(mapped);
	assert!(mapped.is_ipv4_mapped());
	for mode in [BindMode::Ipv4Only, BindMode::Ipv6Only, BindMode::DualStack] {
		assert_eq!(binding(mode, mapped, 80).well_formed(), Err(BindRefusal::Mapped), "{mode:?}");
	}
}

#[test]
fn the_two_single_family_wildcards_share_a_port_and_a_dual_stack_bind_shares_it_with_nothing() {
	let mut table = BindTable::new();
	assert_eq!(table.bind(binding(BindMode::Ipv4Only, ANY4, 80)), Ok(()));
	assert_eq!(table.bind(binding(BindMode::Ipv6Only, ANY6, 80)), Ok(()), "two families, two wildcards, no overlap");
	assert_eq!(table.len(), 2);

	// A DUAL-STACK BIND COVERS BOTH, so it conflicts with each of them.
	assert_eq!(table.bind(binding(BindMode::DualStack, ANY6, 80)), Err(BindRefusal::InUse));

	// And in the other order, which is the half a rule written once per direction gets wrong.
	let mut reverse = BindTable::new();
	assert_eq!(reverse.bind(binding(BindMode::DualStack, ANY6, 80)), Ok(()));
	assert_eq!(reverse.bind(binding(BindMode::Ipv4Only, ANY4, 80)), Err(BindRefusal::InUse));
	assert_eq!(reverse.bind(binding(BindMode::Ipv6Only, ANY6, 80)), Err(BindRefusal::InUse));
	assert_eq!(reverse.bind(binding(BindMode::DualStack, ANY6, 80)), Err(BindRefusal::InUse), "and with another of itself");
}

#[test]
fn the_same_mode_twice_and_a_wildcard_beside_a_specific_address_are_both_refused() {
	let mut table = BindTable::new();
	table.bind(binding(BindMode::Ipv4Only, ANY4, 80)).expect("the first");
	assert_eq!(table.bind(binding(BindMode::Ipv4Only, ANY4, 80)), Err(BindRefusal::InUse));
	// THE NARROWER BIND IS NOT A CARVE-OUT, because this milestone has no reuse rule to ask for one
	// with.
	assert_eq!(table.bind(binding(BindMode::Ipv4Only, v4(10, 0, 2, 15), 80)), Err(BindRefusal::InUse));

	// A different port is a different claim entirely.
	assert_eq!(table.bind(binding(BindMode::Ipv4Only, ANY4, 443)), Ok(()));
}

#[test]
fn a_segment_reaches_the_listener_whose_claim_covers_it_and_no_other() {
	let mut table = BindTable::new();
	table.bind(binding(BindMode::Ipv4Only, ANY4, 80)).expect("v4");
	table.bind(binding(BindMode::Ipv6Only, ANY6, 80)).expect("v6");

	assert_eq!(table.lookup(v4(10, 0, 2, 15), 80).map(|held| held.mode), Some(BindMode::Ipv4Only));
	assert_eq!(table.lookup(v6(1), 80).map(|held| held.mode), Some(BindMode::Ipv6Only));
	assert_eq!(table.lookup(v4(10, 0, 2, 15), 443), None, "a port nothing holds");

	// A DUAL-STACK LISTENER TAKES BOTH, which is the whole reason the mode exists.
	let mut dual = BindTable::new();
	dual.bind(binding(BindMode::DualStack, ANY6, 80)).expect("dual");
	assert!(dual.lookup(v4(10, 0, 2, 15), 80).is_some());
	assert!(dual.lookup(v6(1), 80).is_some());
}

#[test]
fn a_specific_bind_takes_only_its_own_address() {
	let mut table = BindTable::new();
	table.bind(binding(BindMode::Ipv6Only, v6(1), 80)).expect("specific");
	assert!(table.lookup(v6(1), 80).is_some());
	assert!(table.lookup(v6(2), 80).is_none(), "another address on the same port is nobody's");
	assert!(table.lookup(v4(10, 0, 2, 15), 80).is_none(), "and neither is the other family");
}

#[test]
fn equal_ports_in_two_families_cannot_alias_each_other() {
	// THE CASE THE FULL KEY EXISTS FOR. Port 80 in one family and port 80 in the other are different
	// claims, and a table keyed on the port alone would hand one family's segments to the other's
	// listener.
	let mut table = BindTable::new();
	table.bind(binding(BindMode::Ipv4Only, v4(10, 0, 2, 15), 80)).expect("v4");
	table.bind(binding(BindMode::Ipv6Only, v6(0x0f15), 80)).expect("v6");
	assert_eq!(table.lookup(v4(10, 0, 2, 15), 80).map(|held| held.address), Some(v4(10, 0, 2, 15)));
	assert_eq!(table.lookup(v6(0x0f15), 80).map(|held| held.address), Some(v6(0x0f15)));

	// And a low address word that happens to match the other family's is still the other family's.
	assert!(table.lookup(v6(1), 80).is_none());
}

#[test]
fn unbinding_releases_the_claim_and_the_port_can_be_taken_again() {
	let mut table = BindTable::new();
	let held = binding(BindMode::DualStack, ANY6, 80);
	table.bind(held).expect("the first");
	assert_eq!(table.bind(binding(BindMode::Ipv4Only, ANY4, 80)), Err(BindRefusal::InUse));
	assert!(table.unbind(&held));
	assert!(table.is_empty());
	assert_eq!(table.bind(binding(BindMode::Ipv4Only, ANY4, 80)), Ok(()));
	assert!(!table.unbind(&held), "and releasing something nothing holds changes nothing");
}
