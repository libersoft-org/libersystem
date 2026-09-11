//! Selection slots, and what each malformation is refused for.

use super::*;
use alloc::vec;

/// The library-name rule the service uses, restated here so the fixtures do not depend on it.
fn valid_name(name: &str) -> bool {
	name.strip_suffix(".lslib").is_some_and(|stem| !stem.is_empty() && !stem.starts_with("lib") && stem.len() <= 58 && stem.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
}

fn digest(seed: u8) -> [u8; 32] {
	[seed; 32]
}

fn hex(seed: u8) -> alloc::string::String {
	(0..32).map(|_| alloc::format!("{seed:02x}")).collect()
}

fn line(kind: &str, candidates: &[(&str, u8)]) -> alloc::vec::Vec<u8> {
	let body: alloc::vec::Vec<alloc::string::String> = candidates.iter().map(|(name, seed)| alloc::format!("{name}={}", hex(*seed))).collect();
	alloc::format!("{kind}:{}", body.join(",")).into_bytes()
}

#[test]
fn a_well_formed_slot_keeps_the_consumers_own_order() {
	// THE ORDER IS PART OF THE SIGNED RECORD, so "which one runs" is a decision the consumer made at
	// build time. A parser that sorted the set would silently overrule it.
	let slot = parse_slot(&line("vulkan-icd", &[("second.lslib", 2), ("first.lslib", 1)]), valid_name).expect("well formed");
	assert_eq!(slot.kind, "vulkan-icd");
	assert_eq!(slot.admitted, vec![(alloc::string::String::from("second.lslib"), digest(2)), (alloc::string::String::from("first.lslib"), digest(1))]);
}

#[test]
fn each_malformation_is_refused_for_its_own_reason() {
	assert_eq!(parse_slot(b"novulkanicd", valid_name), Err(Refusal::NoKind));
	assert_eq!(parse_slot(&line("VULKAN", &[("a.lslib", 1)]), valid_name), Err(Refusal::BadKind), "a kind is lower case");
	assert_eq!(parse_slot(&line("", &[("a.lslib", 1)]), valid_name), Err(Refusal::BadKind), "and is not empty");
	assert_eq!(parse_slot(b"vulkan-icd:a.lslib", valid_name), Err(Refusal::NoDigest));
	assert_eq!(parse_slot(&line("vulkan-icd", &[("libbad.lslib", 1)]), valid_name), Err(Refusal::BadName), "a provider name may not start with lib");
	assert_eq!(parse_slot(b"vulkan-icd:a.lslib=short", valid_name), Err(Refusal::BadDigest));
	assert_eq!(parse_slot(&line("vulkan-icd", &[("a.lslib", 1), ("a.lslib", 2)]), valid_name), Err(Refusal::DuplicateName));
	assert_eq!(parse_slot(b"vulkan-icd:", valid_name), Err(Refusal::NoDigest), "an empty candidate is not a candidate");
}

#[test]
fn a_slot_admitting_nothing_is_refused_when_the_record_is_read() {
	// A SLOT THAT CAN NEVER BE FILLED is a consumer that can never start, and saying so when the
	// record is read names the record as the fault rather than the launch.
	//
	// The only spelling that reaches `EmptySet` is one with no candidate section at all, because a
	// present-but-malformed one is refused earlier and more precisely.
	let slot = Slot { kind: alloc::string::String::from("vulkan-icd"), admitted: alloc::vec::Vec::new() };
	assert_eq!(bind(&slot, &[], |_| None), Binding::Unfilled);
}

#[test]
fn more_candidates_than_a_slot_may_hold_are_refused() {
	let many: alloc::vec::Vec<(alloc::string::String, u8)> = (0..=MAX_CANDIDATES as u8).map(|index| (alloc::format!("p{index}.lslib"), index)).collect();
	let pairs: alloc::vec::Vec<(&str, u8)> = many.iter().map(|(name, seed)| (name.as_str(), *seed)).collect();
	assert_eq!(parse_slot(&line("vulkan-icd", &pairs), valid_name), Err(Refusal::TooManyCandidates));
}

#[test]
fn the_first_staged_candidate_with_the_admitted_bytes_wins() {
	let slot = parse_slot(&line("vulkan-icd", &[("first.lslib", 1), ("second.lslib", 2)]), valid_name).expect("well formed");
	// The preferred one is not staged, so the next in the consumer's order is taken.
	assert_eq!(bind(&slot, &[], |name| (name == "second.lslib").then(|| digest(2))), Binding::Bind(alloc::string::String::from("second.lslib")));
	// And when both are, the consumer's first choice wins.
	assert_eq!(bind(&slot, &[], |_| Some(digest(1))), Binding::Bind(alloc::string::String::from("first.lslib")));
}

#[test]
fn a_replaced_provider_is_refused_rather_than_skipped() {
	// MOVING ON TO THE NEXT NAME would let a replaced provider hide behind it, which is precisely the
	// substitution the digest is in the record to prevent.
	let slot = parse_slot(&line("vulkan-icd", &[("first.lslib", 1), ("second.lslib", 2)]), valid_name).expect("well formed");
	let staged = |name: &str| match name {
		"first.lslib" => Some(digest(0xee)),
		"second.lslib" => Some(digest(2)),
		_ => None,
	};
	assert_eq!(bind(&slot, &[], staged), Binding::Replaced { name: alloc::string::String::from("first.lslib") });
}

#[test]
fn a_candidate_that_is_already_a_dependency_is_named_as_the_fault() {
	// ONE PROVIDER ACCOUNTED FOR TWICE would fail the arithmetic below; saying so here names the
	// record rather than the count.
	let slot = parse_slot(&line("vulkan-icd", &[("first.lslib", 1)]), valid_name).expect("well formed");
	let dependencies = vec![alloc::string::String::from("first.lslib")];
	assert_eq!(bind(&slot, &dependencies, |_| Some(digest(1))), Binding::AlreadyADependency { name: alloc::string::String::from("first.lslib") });
}

#[test]
fn no_admitted_candidate_staged_refuses_the_launch() {
	let slot = parse_slot(&line("vulkan-icd", &[("first.lslib", 1), ("second.lslib", 2)]), valid_name).expect("well formed");
	assert_eq!(bind(&slot, &[], |_| None), Binding::Unfilled);
	// AND A STAGED PROVIDER THE SET DOES NOT NAME FILLS NOTHING, which is the whole of "cannot
	// widen": it is not asked about, so its bytes never matter.
	assert_eq!(bind(&slot, &[], |name| (name == "elsewhere.lslib").then(|| digest(9))), Binding::Unfilled);
}

#[test]
fn the_record_accounts_for_a_bound_slot_exactly_once() {
	let providers = vec![(alloc::string::String::from("base"), digest(7))];
	let bound = vec![(alloc::string::String::from("icd.lslib"), 0usize)];
	let dependencies = vec![(alloc::string::String::from("base.lslib"), digest(7)), (alloc::string::String::from("icd.lslib"), digest(1))];
	assert!(accounts_for(&providers, &bound, &dependencies));

	// TWO DEPENDENCIES CANNOT BOTH CLAIM ONE DECLARED POSITION.
	let twice = vec![(alloc::string::String::from("icd.lslib"), 0usize), (alloc::string::String::from("icd.lslib"), 0usize)];
	let three = vec![
		(alloc::string::String::from("base.lslib"), digest(7)),
		(alloc::string::String::from("icd.lslib"), digest(1)),
		(alloc::string::String::from("icd.lslib"), digest(1)),
	];
	assert!(!accounts_for(&providers, &twice, &three));
}

#[test]
fn a_dependency_the_record_does_not_account_for_fails_the_arithmetic() {
	let providers = vec![(alloc::string::String::from("base"), digest(7))];
	// An extra dependency with no provider line and no slot: the count differs.
	let dependencies = vec![(alloc::string::String::from("base.lslib"), digest(7)), (alloc::string::String::from("extra.lslib"), digest(8))];
	assert!(!accounts_for(&providers, &[], &dependencies));

	// AND A PROVIDER WHOSE STAGED BYTES DIFFER FROM THE RECORD'S fails even when the count agrees.
	let wrong = vec![(alloc::string::String::from("base.lslib"), digest(0xee))];
	assert!(!accounts_for(&providers, &[], &wrong));
}

#[test]
fn a_record_with_no_slots_behaves_exactly_as_it_did() {
	// THE CHANGE IS ADDITIVE FOR EVERY CONSUMER THAT HAS NONE, which is every consumer in the tree
	// today; if this were not true the mechanism would have altered every launch to serve one.
	let providers = vec![(alloc::string::String::from("a"), digest(1)), (alloc::string::String::from("b"), digest(2))];
	let dependencies = vec![(alloc::string::String::from("a.lslib"), digest(1)), (alloc::string::String::from("b.lslib"), digest(2))];
	assert!(accounts_for(&providers, &[], &dependencies));
	assert!(accounts_for(&[], &[], &[]), "and a consumer with nothing at all still passes");
}
