use super::{MAX_KIND, MAX_SCOPE_KINDS, Refusal, Scope};

#[test]
// A SCOPE ADMITS WHAT IT WAS MINTED FOR AND NOTHING ELSE, which is the whole claim. The kinds here
// are the vocabulary's own numbers: block 1, net 2, display 3, audio 4, input 5, usb-bus 6,
// pointer 7, console-bytes 8, local-stream 9, touch 10.
fn a_scope_admits_its_own_kinds_and_refuses_every_other() {
	let input = Scope::of(&[5, 7, 10]).expect("three kinds are a subset");
	assert!(input.admits(5) && input.admits(7) && input.admits(10));
	assert!(!input.admits(1), "a service minted for input may not open a disk");
	assert!(!input.admits(6), "nor a USB bus");
	assert!(!input.admits(0), "zero is no kind at all");
	assert!(!input.admits(MAX_KIND + 1), "and a value past the mask is not in it");
}

#[test]
// AN EMPTY SET IS A DECLARATION ERROR AND IS REFUSED WHERE IT IS MADE. A connection that answers
// nothing is indistinguishable from a service that is up and broken, and the manifest row that
// forgot its kinds is what produced it.
fn an_empty_set_is_refused_and_the_inventory_scope_is_how_none_is_said_on_purpose() {
	assert_eq!(Scope::of(&[]), Err(Refusal::EmptySet));
	let inventory = Scope::inventory();
	assert!(inventory.is_inventory());
	for kind in 0..=MAX_KIND {
		assert!(!inventory.admits(kind), "the inventory scope admits no kind, including {kind}");
	}
	assert!(!Scope::of(&[1]).expect("one kind").is_inventory(), "a scope with a kind is not an inventory one");
}

#[test]
// A KIND THIS MASK CANNOT EXPRESS IS REFUSED RATHER THAN WRAPPED. A shift past the width is where a
// silent scope error comes from: kind 33 would set bit 1 and mint a connection to BLOCK devices.
fn a_kind_the_mask_cannot_express_is_refused_rather_than_wrapped() {
	assert_eq!(Scope::of(&[0]), Err(Refusal::UnknownKind(0)));
	assert_eq!(Scope::of(&[MAX_KIND + 1]), Err(Refusal::UnknownKind(MAX_KIND + 1)));
	assert_eq!(Scope::of(&[33]), Err(Refusal::UnknownKind(33)), "33 would be block, one shift around");
	assert_eq!(Scope::of(&[5, 33]), Err(Refusal::UnknownKind(33)), "and one bad entry refuses the set");
	assert!(Scope::of(&[MAX_KIND]).is_ok(), "the last expressible kind is expressible");
}

#[test]
// A REPEATED KIND IS THE SAME SUBSET. A manifest row naming a kind twice is untidy and not an error,
// and refusing it would make the declaration harder to write for no property gained.
fn a_repeated_kind_is_the_same_subset_and_the_count_is_bounded() {
	assert_eq!(Scope::of(&[3, 3, 3]), Scope::of(&[3]));
	let full: [u16; MAX_SCOPE_KINDS] = core::array::from_fn(|at| at as u16 + 1);
	assert!(Scope::of(&full).is_ok(), "the interface's own bound is admissible");
	let mut over = [1u16; MAX_SCOPE_KINDS + 1];
	over[0] = 2;
	assert_eq!(Scope::of(&over), Err(Refusal::TooManyKinds), "one past it is refused even as duplicates");
}

#[test]
// THE MASK ROUND-TRIPS, because a table stores one per slot and gives it back on every request.
fn a_stored_mask_gives_back_the_scope_it_came_from() {
	for kinds in [&[1u16][..], &[5, 7, 10][..], &[2, 4, 6, 8][..]] {
		let scope = Scope::of(kinds).expect("a subset");
		assert_eq!(Scope::from_bits(scope.bits()), scope);
	}
	assert_eq!(Scope::from_bits(Scope::inventory().bits()), Scope::inventory());
	assert!(Scope::unrestricted().admits(1) && Scope::unrestricted().admits(MAX_KIND));
}
