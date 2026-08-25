// Every completion, every failure and every order they can arrive in.
//
// The rules this crate exists for are all about the unhappy paths, so this suite is mostly unhappy
// paths: a map refused after its address was reserved, an unmap that confirms while the invalidation
// does not, a rebind that leaves old mappings naming a generation nobody answers to. What each one
// asserts is the same sentence from two sides - the kernel's ledger and the fake's idea of the
// hardware - because the two disagreeing is the failure that matters.

use super::fake::{Call, Fake, Injection, event};
use super::*;

fn requirements() -> Requirements {
	Requirements { address_bits: 64, alignment: 4096, max_segments: 8, coherent: true }
}

fn iommu() -> (Iommu<Fake>, DomainId) {
	let mut iommu = Iommu::new(Fake::new(), 8);
	let domain = iommu.create_domain(0x1_0000, 0x10_0000, Vec::new(), Generation(1)).expect("a domain");
	(iommu, domain)
}

#[test]
fn a_direction_is_a_permission_and_not_a_label() {
	assert!(Direction::ToDevice.permits(Access::Read));
	assert!(!Direction::ToDevice.permits(Access::Write), "a to-device mapping is one the device READS");
	assert!(!Direction::FromDevice.permits(Access::Read));
	assert!(Direction::FromDevice.permits(Access::Write));
	assert!(Direction::Bidirectional.permits(Access::Read) && Direction::Bidirectional.permits(Access::Write));
}

#[test]
fn an_address_space_never_hands_out_what_it_has_not_reclaimed() {
	let mut space = IovaSpace::new(0x1000, 0x4000, Vec::new());
	let first = space.allocate(0x1000, 0x1000).expect("room");
	let second = space.allocate(0x1000, 0x1000).expect("room");
	assert_ne!(first, second, "two live allocations are two addresses");
	assert_eq!(space.live_ranges(), 2);
	space.release(first).expect("released");
	let third = space.allocate(0x1000, 0x1000).expect("room");
	assert_eq!(third, first, "a released address comes back");
	// And what was never released does not.
	let fourth = space.allocate(0x1000, 0x1000).expect("room");
	assert_ne!(fourth, second);
}

#[test]
fn a_quarantined_address_is_never_offered_again() {
	let mut space = IovaSpace::new(0x1000, 0x3000, Vec::new());
	let first = space.allocate(0x1000, 0x1000).expect("room");
	space.quarantine(first).expect("quarantined");
	assert_eq!(space.quarantined_ranges(), 1);
	// The space still has room, and none of it is the quarantined range.
	for _ in 0..2 {
		let next = space.allocate(0x1000, 0x1000).expect("room");
		assert_ne!(next, first, "a quarantined address is out of circulation for good");
	}
	assert_eq!(space.allocate(0x1000, 0x1000), Err(Fault::NoSpace), "and the space it took is not given back either");
}

#[test]
fn a_reserved_region_is_stepped_over_rather_than_allocated_into() {
	let reserved = alloc::vec![Reserved { base: 0x2000, len: 0x1000 }];
	let mut space = IovaSpace::new(0x1000, 0x4000, reserved);
	let first = space.allocate(0x1000, 0x1000).expect("room");
	assert_eq!(first, DmaAddress(0x1000));
	let second = space.allocate(0x1000, 0x1000).expect("room");
	assert_eq!(second, DmaAddress(0x3000), "the reserved page is skipped, not handed out");
}

#[test]
fn a_malformed_request_is_refused_before_anything_is_reserved() {
	let mut space = IovaSpace::new(0x1000, 0x4000, Vec::new());
	assert_eq!(space.allocate(0, 0x1000), Err(Fault::Malformed), "a zero-length mapping maps nothing");
	assert_eq!(space.allocate(0x1000, 0), Err(Fault::Malformed));
	assert_eq!(space.allocate(0x1000, 3), Err(Fault::Malformed), "an alignment that is not a power of two is not an alignment");
	assert_eq!(space.live_ranges(), 0, "and none of them took an address");
}

#[test]
fn a_map_that_the_backend_refuses_leaves_no_address_behind() {
	let (mut iommu, domain) = iommu();
	iommu.backend_mut_for_test().inject(Injection::Map, Fault::NoSpace);
	assert_eq!(iommu.map(domain, 0x8000_0000, 0x1000, Direction::ToDevice, &requirements()), Err(Fault::NoSpace));
	assert_eq!(iommu.live_addresses(domain), 0, "the address was reserved and given back, not stranded");
	assert_eq!(iommu.live_mappings(), 0);
	// And the next map gets that same address, which is what says it really came back.
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::ToDevice, &requirements()).expect("mapped");
	assert_eq!(iommu.address_of(id), Some(DmaAddress(0x1_0000)));
}

#[test]
fn a_close_releases_the_frame_only_after_both_completions() {
	let (mut iommu, domain) = iommu();
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
	iommu.begin_close(id).expect("closing");
	assert_eq!(iommu.mapping(id).map(|m| m.state), Some(MappingState::Closing));
	assert_eq!(iommu.live_addresses(domain), 1, "the address is still held while the close is in flight");
	assert_eq!(iommu.finish_close(id), Ok(Release::FramesReusable));
	assert_eq!(iommu.mapping(id).map(|m| m.state), Some(MappingState::Released));
	assert_eq!(iommu.live_addresses(domain), 0);
	// UNMAP BEFORE INVALIDATE, and the fake records the order because the end state cannot show it.
	let calls = iommu.backend().calls();
	let unmap = calls.iter().position(|c| matches!(c, Call::Unmap(..))).expect("unmapped");
	let invalidate = calls.iter().position(|c| matches!(c, Call::Invalidate(..))).expect("invalidated");
	assert!(unmap < invalidate, "the translation is removed before the caches are told, not after");
}

#[test]
fn an_unconfirmed_invalidation_quarantines_the_frame_rather_than_recycling_it() {
	let (mut iommu, domain) = iommu();
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::FromDevice, &requirements()).expect("mapped");
	iommu.begin_close(id).expect("closing");
	iommu.backend_mut_for_test().inject(Injection::Invalidate, Fault::Unconfirmed);
	assert_eq!(iommu.finish_close(id), Ok(Release::Quarantined), "an invalidation nobody confirmed is not a release");
	assert_eq!(iommu.mapping(id).map(|m| m.state), Some(MappingState::Quarantined));
	assert_eq!(iommu.quarantined_addresses(domain), 1, "and the address is out of circulation with it");
	assert_eq!(iommu.live_addresses(domain), 0);
}

#[test]
fn an_unconfirmed_unmap_never_reaches_the_invalidation_and_never_releases() {
	let (mut iommu, domain) = iommu();
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::ToDevice, &requirements()).expect("mapped");
	iommu.begin_close(id).expect("closing");
	iommu.backend_mut_for_test().inject(Injection::Unmap, Fault::Unconfirmed);
	assert_eq!(iommu.finish_close(id), Ok(Release::Quarantined));
	// The invalidation is not attempted: there is nothing to invalidate if the unmap did not happen,
	// and a confirmed invalidation after an unconfirmed unmap would read as a clean close.
	assert!(!iommu.backend().calls().iter().any(|c| matches!(c, Call::Invalidate(..))), "no invalidation follows an unmap that did not complete");
	assert_eq!(iommu.quarantined_mappings(), 1);
}

#[test]
fn an_endpoint_reaches_its_own_mapping_and_nothing_else() {
	let (mut iommu, domain) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(domain, endpoint).expect("attached");
	let id = iommu.map(domain, 0x8000_0000, 0x2000, Direction::Bidirectional, &requirements()).expect("mapped");
	let base = iommu.address_of(id).expect("an address");
	assert_eq!(iommu.translate(endpoint, base, Access::Read), Ok(0x8000_0000));
	// THE LAST BYTE IS REACHABLE AND THE FIRST BYTE AFTER IT IS NOT. Off-by-one at a mapping's edge
	// is the difference between a bounded window and a device reading its neighbour.
	let last = base.checked_add(0x1FFF).expect("in range");
	assert_eq!(iommu.translate(endpoint, last, Access::Read), Ok(0x8000_1FFF));
	let past = base.checked_add(0x2000).expect("in range");
	assert_eq!(iommu.translate(endpoint, past, Access::Read), Err(Fault::NotMapped));
}

#[test]
fn a_device_write_to_a_read_only_mapping_faults_while_the_permitted_direction_works() {
	let (mut iommu, domain) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(domain, endpoint).expect("attached");
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::ToDevice, &requirements()).expect("mapped");
	let base = iommu.address_of(id).expect("an address");
	assert_eq!(iommu.translate(endpoint, base, Access::Read), Ok(0x8000_0000), "the direction it was made for works");
	assert_eq!(iommu.translate(endpoint, base, Access::Write), Err(Fault::Permission), "and the other one does not");
	assert_eq!(iommu.faults().total(), 1, "the refusal is a recorded security event, not a silent no");
	assert_eq!(iommu.faults().recent()[0].reason, Fault::Permission);
}

#[test]
fn an_unattached_endpoint_reaches_nothing_at_all() {
	let (mut iommu, domain) = iommu();
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
	let base = iommu.address_of(id).expect("an address");
	// DMA BEFORE ATTACH. The mapping exists; the endpoint has no domain, so there is no space in
	// which that address means anything.
	assert_eq!(iommu.translate(EndpointId(7), base, Access::Write), Err(Fault::UnknownEndpoint));
	assert!(!iommu.may_master(domain, EndpointId(7)), "and it may not be allowed to master the bus");
}

#[test]
fn the_same_numeric_address_in_two_domains_is_two_different_pages() {
	let mut iommu = Iommu::new(Fake::new(), 8);
	let a = iommu.create_domain(0x1_0000, 0x10_000, Vec::new(), Generation(1)).expect("domain a");
	let b = iommu.create_domain(0x1_0000, 0x10_000, Vec::new(), Generation(1)).expect("domain b");
	let (first, second) = (EndpointId(1), EndpointId(2));
	iommu.attach(a, first).expect("attached");
	iommu.attach(b, second).expect("attached");
	let in_a = iommu.map(a, 0xAAAA_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
	let in_b = iommu.map(b, 0xBBBB_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
	let address = iommu.address_of(in_a).expect("an address");
	assert_eq!(iommu.address_of(in_b), Some(address), "the two domains independently chose the same number");
	assert_eq!(iommu.translate(first, address, Access::Write), Ok(0xAAAA_0000));
	assert_eq!(iommu.translate(second, address, Access::Write), Ok(0xBBBB_0000), "and it means something different in each");

	// UNMAP IT FROM A ONLY. B's mapping is untouched, and A can no longer reach anything.
	iommu.begin_close(in_a).expect("closing");
	assert_eq!(iommu.finish_close(in_a), Ok(Release::FramesReusable));
	assert_eq!(iommu.translate(first, address, Access::Write), Err(Fault::NotMapped));
	assert_eq!(iommu.translate(second, address, Access::Write), Ok(0xBBBB_0000), "B's page is still B's");
}

#[test]
fn a_mapping_made_under_an_old_binding_is_nameless_after_a_rebind() {
	let (mut iommu, domain) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(domain, endpoint).expect("attached");
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
	let base = iommu.address_of(id).expect("an address");
	assert_eq!(iommu.translate(endpoint, base, Access::Write), Ok(0x8000_0000));
	// The device slot is reused: a new driver, a new generation.
	iommu.set_generation(domain, Generation(2)).expect("rebound");
	assert_eq!(iommu.translate(endpoint, base, Access::Write), Err(Fault::StaleGeneration), "the old binding's mapping does not answer to the new one");
	assert_eq!(iommu.faults().recent().last().map(|e| e.reason), Some(Fault::StaleGeneration));
}

#[test]
fn revoking_an_endpoint_releases_everything_it_could_reach() {
	let (mut iommu, domain) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(domain, endpoint).expect("attached");
	for _ in 0..3 {
		iommu.map(domain, 0x8000_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
	}
	assert_eq!(iommu.live_mappings(), 3);
	assert_eq!(iommu.revoke_endpoint(domain, endpoint), Ok(Release::FramesReusable));
	assert_eq!(iommu.live_mappings(), 0, "a revoked endpoint leaves no live mapping behind");
	assert_eq!(iommu.live_addresses(domain), 0);
	assert!(!iommu.may_master(domain, endpoint));
}

#[test]
fn a_crash_whose_detach_does_not_confirm_quarantines_every_page_it_could_reach() {
	let (mut iommu, domain) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(domain, endpoint).expect("attached");
	for _ in 0..3 {
		iommu.map(domain, 0x8000_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
	}
	iommu.backend_mut_for_test().inject(Injection::Detach, Fault::Unconfirmed);
	assert_eq!(iommu.revoke_endpoint(domain, endpoint), Ok(Release::Quarantined));
	assert_eq!(iommu.quarantined_mappings(), 3, "an unconfirmed detach releases nothing");
	assert_eq!(iommu.quarantined_addresses(domain), 3);
	assert_eq!(iommu.live_addresses(domain), 0);
}

#[test]
fn an_attach_that_fails_leaves_the_endpoint_unable_to_master_the_bus() {
	let (mut iommu, domain) = iommu();
	iommu.backend_mut_for_test().inject(Injection::Attach, Fault::Unconfirmed);
	assert_eq!(iommu.attach(domain, EndpointId(7)).err(), Some(Fault::Unconfirmed));
	assert!(!iommu.may_master(domain, EndpointId(7)), "a failed attach is not a quiet attach");
	assert_eq!(iommu.backend().attachments(), 0);
}

#[test]
fn a_fault_storm_does_bounded_work() {
	let (mut iommu, _) = iommu();
	for index in 0..1000 {
		iommu.backend_mut_for_test().queue_fault(event(7, 1, 0x1000 + index, Access::Write, Fault::NotMapped));
	}
	// The caller's buffer is what bounds each drain, and the log is what bounds the memory.
	let mut out = [event(0, 0, 0, Access::Read, Fault::NotMapped); 16];
	let mut drained = 0;
	while drained < 1000 {
		let taken = iommu.drain_faults(&mut out);
		if taken == 0 {
			break;
		}
		drained += taken;
	}
	assert_eq!(drained, 1000, "every event was taken, sixteen at a time");
	assert_eq!(iommu.faults().total(), 1000, "and every one was counted");
	assert_eq!(iommu.faults().recent().len(), 8, "while the ring holds only what it was sized for");
	assert_eq!(iommu.faults().dropped(), 992, "and says how many it dropped rather than pretending");
}

#[test]
fn an_address_limited_device_gets_a_bounce_rather_than_an_address_it_cannot_name() {
	let limited = Requirements { address_bits: 32, alignment: 4096, max_segments: 4, coherent: false };
	let low = alloc::vec![Segment { physical: 0x1_000, len: 0x1000 }];
	assert_eq!(plan(&low, &limited), Ok(Plan::Direct(low.clone())), "a page it can name is reached where it is");
	let high = alloc::vec![Segment { physical: 0x1_0000_0000, len: 0x1000 }];
	assert_eq!(plan(&high, &limited), Ok(Plan::Bounce { len: 0x1000 }), "and one it cannot is staged");
}

#[test]
fn too_many_segments_for_the_descriptor_format_is_also_a_bounce() {
	let narrow = Requirements { address_bits: 64, alignment: 4096, max_segments: 2, coherent: true };
	let many: Vec<Segment> = (0..4).map(|i| Segment { physical: 0x1000 * (i + 1), len: 0x1000 }).collect();
	assert_eq!(plan(&many, &narrow), Ok(Plan::Bounce { len: 0x4000 }));
	let few: Vec<Segment> = many[..2].to_vec();
	assert_eq!(plan(&few, &narrow), Ok(Plan::Direct(few)));
}

#[test]
fn an_iova_the_device_cannot_name_is_refused_with_its_address_given_back() {
	// The space is entirely above a 32-bit device's ceiling: every address it offers is unusable,
	// and the refusal must not consume one of them.
	let mut iommu = Iommu::new(Fake::new(), 8);
	let domain = iommu.create_domain(0x1_0000_0000, 0x10_0000, Vec::new(), Generation(1)).expect("a domain");
	let limited = Requirements { address_bits: 32, alignment: 4096, max_segments: 4, coherent: true };
	assert_eq!(iommu.map(domain, 0x1000, 0x1000, Direction::ToDevice, &limited), Err(Fault::OutOfRange));
	assert_eq!(iommu.live_addresses(domain), 0, "the address it could not use went back into the space");
}

#[test]
fn the_bind_decision_has_exactly_three_answers() {
	assert_eq!(decide_bind(Policy::IommuRequired, true), BindDecision::Translated);
	assert_eq!(decide_bind(Policy::IommuRequired, false), BindDecision::Refused, "an untrusted DMA driver does not start without enforcement");
	assert_eq!(decide_bind(Policy::TrustedUntranslated, false), BindDecision::DegradedUntranslated, "and a trusted one starts loudly");
	assert_eq!(decide_bind(Policy::TrustedUntranslated, true), BindDecision::Translated, "trust is permission to run without translation, not a preference for it");
}

#[test]
fn a_backend_that_cannot_enforce_directions_says_so() {
	// The enforcing profile asks this before it trusts anything: a backend that maps every request
	// read-write would satisfy every other assertion in this file and drop half the security claim.
	assert!(Fake::new().enforces_directions());
	assert!(!Fake::new().without_direction_support().enforces_directions());
}

#[test]
fn the_kernels_ledger_and_the_hardwares_agree_after_every_operation() {
	let (mut iommu, domain) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(domain, endpoint).expect("attached");
	let mut ids = Vec::new();
	for index in 0..4 {
		ids.push(iommu.map(domain, 0x8000_0000 + index * 0x1000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped"));
	}
	assert_eq!(iommu.backend().installed_ranges(), 4, "what the kernel thinks is installed is what the backend installed");
	for id in ids.iter().take(2) {
		iommu.begin_close(*id).expect("closing");
		assert_eq!(iommu.finish_close(*id), Ok(Release::FramesReusable));
	}
	assert_eq!(iommu.backend().installed_ranges(), 2);
	assert_eq!(iommu.live_mappings(), 2);
	// And the two that are gone really are gone from the hardware's view.
	for id in ids.iter().take(2) {
		let address = iommu.mapping(*id).map(|m| m.iova).expect("recorded");
		assert!(iommu.backend().translates(domain, address).is_none(), "a closed mapping translates nothing");
	}
}
