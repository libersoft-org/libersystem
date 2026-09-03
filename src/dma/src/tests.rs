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
	Requirements::new(64, 4096, 8, true).expect("a device this shape exists")
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
fn a_hardware_fault_is_stamped_with_the_generation_the_domain_carries() {
	// THE PROPERTY MOVED HERE, from the virtio backend, and this is the layer that can hold it: a
	// backend reports an endpoint and the domain it is attached to, and the domain is where the
	// generation the kernel minted lives. A backend with a generation of its own was a second answer
	// to the same question, and its `set_generation` moved it for every binding at once - so a fault
	// from a binding made before the rebind claimed to be from the one after it.
	//
	// The event goes in carrying `Generation(0)`, which is what the real backend now reports: the
	// absence of an answer rather than a binding being named. No binding ever carries 0.
	let (mut iommu, domain) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(domain, endpoint).expect("attached");
	iommu.set_generation(domain, Generation(9)).expect("rebound");
	iommu.backend_mut_for_test().queue_fault(FaultEvent { endpoint, domain, generation: Generation(0), address: Some(DmaAddress(0x2000)), access: Access::Write, reason: Fault::NotMapped });
	let mut out = [FaultEvent { endpoint: EndpointId(0), domain: DomainId(0), generation: Generation(0), address: None, access: Access::Read, reason: Fault::NotMapped }; 2];
	assert_eq!(iommu.drain_faults(&mut out), 1);
	assert_eq!(out[0].generation, Generation(9), "the generation the domain carries, not the zero the backend reported");
	assert_eq!(iommu.faults().recent().last().map(|e| e.generation), Some(Generation(9)), "and the log holds the stamped event, not the raw one");
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
	let limited = Requirements::new(32, 4096, 4, false).expect("a device this shape exists");
	let low = alloc::vec![Segment { physical: 0x1_000, len: 0x1000 }];
	assert_eq!(plan(&low, &limited), Ok(Plan::Direct(low.clone())), "a page it can name is reached where it is");
	let high = alloc::vec![Segment { physical: 0x1_0000_0000, len: 0x1000 }];
	assert_eq!(plan(&high, &limited), Ok(Plan::Bounce { len: 0x1000 }), "and one it cannot is staged");
}

#[test]
fn too_many_segments_for_the_descriptor_format_is_also_a_bounce() {
	let narrow = Requirements::new(64, 4096, 2, true).expect("a device this shape exists");
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
	let limited = Requirements::new(32, 4096, 4, true).expect("a device this shape exists");
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

// THE TEARDOWN ORDER, AND THE TWO OWNERS THAT REACH THE SAME MAPPING.
//
// A driver that exits drops a device capability and a set of DMA buffers, and the process teardown
// runs them in whatever order it runs them. Both reach the same translations - the device capability
// through the endpoint revoke, each buffer through its own close - so whichever arrives second finds
// work already done. These four say what each of those arrivals must answer.

#[test]
fn a_revoked_endpoint_has_every_translation_taken_down_before_it_leaves() {
	let (mut iommu, domain) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(domain, endpoint).expect("attached");
	for _ in 0..3 {
		iommu.map(domain, 0x8000_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
	}
	assert_eq!(iommu.backend().installed_ranges(), 3);
	assert_eq!(iommu.revoke_endpoint(domain, endpoint), Ok(Release::FramesReusable));
	// THE HARDWARE'S OWN IDEA OF ITS STATE, not the ledger's. The revoke used to mark every mapping
	// released without asking the device to drop one, so this count stayed at three while the kernel
	// declared the frames reusable.
	assert_eq!(iommu.backend().installed_ranges(), 0, "a revoke that releases frames has unmapped every one of them");
	// AND THE UNMAPS GO BEFORE THE DETACH. A virtio-iommu destroys a domain when its last endpoint
	// leaves, so an unmap sent afterwards names a domain the device no longer has.
	let calls = iommu.backend().calls();
	let detach = calls.iter().position(|c| matches!(c, Call::Detach(..))).expect("detached");
	let unmaps: Vec<usize> = calls.iter().enumerate().filter(|(_, c)| matches!(c, Call::Unmap(..))).map(|(i, _)| i).collect();
	assert_eq!(unmaps.len(), 3, "one unmap per mapping");
	assert!(unmaps.iter().all(|u| *u < detach), "every translation comes down while the endpoint is still attached");
}

#[test]
fn a_mapping_the_endpoint_revoke_already_released_closes_as_released() {
	let (mut iommu, domain) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(domain, endpoint).expect("attached");
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
	assert_eq!(iommu.revoke_endpoint(domain, endpoint), Ok(Release::FramesReusable));
	// The buffer that owns the frames arrives second. It used to be told `NotMapped`, because
	// `begin_close` refuses anything that is not `Live` - and a kernel reading that as "the unmap
	// did not complete" quarantined frames the revoke had already released cleanly.
	assert_eq!(iommu.close(id), Ok(Release::FramesReusable), "the verdict the first owner reached, not an error");
	assert_eq!(iommu.mapping(id).map(|m| m.state), Some(MappingState::Released));
	assert_eq!(iommu.backend().calls().iter().filter(|c| matches!(c, Call::Unmap(..))).count(), 1, "and the device is not asked twice");
}

#[test]
fn a_mapping_the_revoke_quarantined_stays_quarantined_however_often_it_is_closed() {
	let (mut iommu, domain) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(domain, endpoint).expect("attached");
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
	iommu.backend_mut_for_test().inject(Injection::Detach, Fault::Unconfirmed);
	assert_eq!(iommu.revoke_endpoint(domain, endpoint), Ok(Release::Quarantined));
	// A verdict of "nobody knows whether the device can still reach this" does not improve by being
	// asked again, and must never soften into a release.
	assert_eq!(iommu.close(id), Ok(Release::Quarantined));
	assert_eq!(iommu.close(id), Ok(Release::Quarantined));
	assert_eq!(iommu.quarantined_mappings(), 1);
	assert_eq!(iommu.live_addresses(domain), 0);
}

#[test]
fn one_translation_the_device_will_not_drop_does_not_condemn_the_frames_of_the_others() {
	let (mut iommu, domain) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(domain, endpoint).expect("attached");
	let ids: Vec<MappingId> = (0..3).map(|_| iommu.map(domain, 0x8000_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped")).collect();
	// One unmap refused - the injection fires once and clears - so the first mapping is condemned
	// and the two behind it are not.
	iommu.backend_mut_for_test().inject(Injection::Unmap, Fault::Unconfirmed);
	assert_eq!(iommu.revoke_endpoint(domain, endpoint), Ok(Release::Quarantined), "a revoke with anything quarantined is not a clean release");
	assert_eq!(iommu.quarantined_mappings(), 1, "only the one the device would not drop");
	assert_eq!(iommu.mapping(ids[0]).map(|m| m.state), Some(MappingState::Quarantined));
	assert_eq!(iommu.mapping(ids[1]).map(|m| m.state), Some(MappingState::Released));
	assert_eq!(iommu.mapping(ids[2]).map(|m| m.state), Some(MappingState::Released));
	assert_eq!(iommu.quarantined_addresses(domain), 1);
}

#[test]
fn requirements_refuse_the_numbers_no_device_has() {
	assert!(Requirements::new(64, 0, 8, true).is_err(), "an alignment of zero is a modulus by zero, not a device");
	assert!(Requirements::new(64, 4096 + 1, 8, true).is_err(), "a segment boundary is a power of two");
	assert!(Requirements::new(64, 4096, 0, true).is_err(), "a descriptor format holding no segments programs nothing");
	assert!(Requirements::new(0, 4096, 8, true).is_err(), "a device putting no bits on the bus does not master it");
	assert!(Requirements::new(65, 4096, 8, true).is_err(), "and 65 bits is an address space that does not exist");
	let ok = Requirements::new(32, 4096, 8, false).expect("a legacy engine");
	assert!(!ok.permits(0x1000, 0), "a zero-length span has no last byte to be in range");
	assert!(ok.permits(0x1000, 0x1000));
	assert!(!ok.permits(0x1001, 0x1000), "and the alignment still bites");
}

#[test]
fn a_fault_log_that_keeps_nothing_counts_what_it_dropped_rather_than_panicking() {
	let mut log = FaultLog::new(0);
	log.record(event(7, 1, 0x1000, Access::Write, Fault::NotMapped));
	log.record(event(7, 1, 0x2000, Access::Read, Fault::Permission));
	assert_eq!(log.total(), 2, "a log that keeps nothing still counts");
	assert_eq!(log.dropped(), 2);
	assert!(log.recent().is_empty());
}

#[test]
fn a_device_is_never_given_the_null_address() {
	// ZERO IS NOT AN ADDRESS TO A DEVICE ANY MORE THAN IT IS TO SOFTWARE, and a first-fit allocator
	// whose space starts at zero hands it out first. A `virtio` device reads a queue whose descriptor
	// table is at address zero as a queue that was never programmed - it builds no mapping for the
	// ring, never looks at it, never fills a buffer and never raises an interrupt, and reports
	// nothing, because from its side there is no queue there.
	//
	// Measured before it was fixed: with a controller in the machine, the first ring allocated in a
	// domain landed on IOVA 0, `virtio-net` transmitted, the host answered, and its receive queue was
	// never read once.
	let mut iommu = Iommu::new(Fake::new(), 8);
	let domain = iommu.create_domain(0, 0x10_0000, Vec::new(), Generation(1)).expect("a domain based at zero");
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
	let first = iommu.address_of(id).expect("an address");
	assert_ne!(first, DmaAddress(0), "the first mapping in a domain based at zero must not be given the null address");
	assert_eq!(first, DmaAddress(4096), "and it is the first aligned address above it rather than an arbitrary gap");
	// The rule holds for a whole domain's worth of allocations, not just the first.
	for _ in 0..8 {
		let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::Bidirectional, &requirements()).expect("mapped");
		assert_ne!(iommu.address_of(id), Some(DmaAddress(0)));
	}
}

#[test]
fn an_identity_mapping_lands_on_the_address_it_was_asked_for() {
	// The one mapping whose address the allocator does not choose: a doorbell is where the interrupt
	// controller says it is, so the only mapping that can carry an MSI is one to itself.
	let (mut iommu, domain) = iommu();
	let id = iommu.map_identity(domain, 0x2_0000, 0x1000, Direction::FromDevice).expect("identity mapped");
	assert_eq!(iommu.address_of(id), Some(DmaAddress(0x2_0000)));
	assert_eq!(iommu.mapping(id).map(|m| m.physical), Some(0x2_0000), "iova and physical are the same address, which is what identity means");
	// And the range is out of the space afterwards, so nothing else is handed it.
	let other = iommu.map(domain, 0x8000_0000, 0x1000, Direction::ToDevice, &requirements()).expect("mapped");
	assert_ne!(iommu.address_of(other), Some(DmaAddress(0x2_0000)));
	// Asking twice for the same range is a refusal rather than a second mapping over the first.
	assert_eq!(iommu.map_identity(domain, 0x2_0000, 0x1000, Direction::FromDevice).err(), Some(Fault::Overlaps));
	// And a range outside the negotiated input range is refused rather than mapped somewhere else.
	assert_eq!(iommu.map_identity(domain, 0xF000_0000, 0x1000, Direction::FromDevice).err(), Some(Fault::OutOfRange));
	// AND THE QUERY THE DOORBELL PATH ASKS INSTEAD OF MAPPING TWICE.
	//
	// `map_identity` refusing a duplicate is right for every caller but one: the MSI doorbell is
	// installed when a vector is acquired, so a driver that acquires, releases and acquires again
	// within one binding reaches it twice for the same range - and the refusal was read as "this
	// endpoint has no route for its interrupts", refusing the second acquire because the first
	// succeeded. So the doorbell asks first, and this is what it asks.
	assert!(iommu.identity_mapped(domain, 0x2_0000, 0x1000, Direction::FromDevice), "the range that was just identity-mapped is reported as mapped");
	assert!(!iommu.identity_mapped(domain, 0x2_0000, 0x2000, Direction::FromDevice), "a different LENGTH over the same base is a different request, not this mapping");
	assert!(!iommu.identity_mapped(domain, 0x2_0000, 0x1000, Direction::ToDevice), "a different DIRECTION is a different request: answering yes would skip a map that does not exist");
	assert!(!iommu.identity_mapped(domain, 0x3_0000, 0x1000, Direction::FromDevice), "a range nobody mapped is not mapped");
}

// THE BOUNCE DECISION NOW HAS A CONSUMER, and the sync points happen whether or not a copy does.
//
// `Plan::Bounce` existed and nothing built one, `Requirements::coherent` was recorded and never read,
// and there were no `sync_for_device`/`sync_for_cpu` operations - so a driver on an address-limited or
// non-coherent device was handed a decision it could not act on. M0's portable contract is these two
// moments and who performs the maintenance at them.
#[test]
fn a_staged_buffer_copies_at_the_sync_points_and_tells_the_caches() {
	use core::cell::RefCell;

	// A cache that records what it was asked to do, so the ORDER and the SPANS are checkable rather
	// than assumed.
	struct Recorder {
		events: RefCell<Vec<(&'static str, u64, u64)>>,
	}
	impl crate::CacheMaintenance for Recorder {
		fn clean_for_device(&self, physical: u64, len: u64) {
			self.events.borrow_mut().push(("clean", physical, len));
		}
		fn invalidate_for_cpu(&self, physical: u64, len: u64) {
			self.events.borrow_mut().push(("invalidate", physical, len));
		}
	}
	let cache = Recorder { events: RefCell::new(Vec::new()) };

	// A NON-COHERENT device that can address the low 32 bits.
	let requirements = crate::Requirements::new(32, 0x1000, 1, false).expect("a legal requirement");
	// THE BUFFER THE DEVICE WILL USE, and the CPU's mapping of it are ONE THING. This test used to
	// call both methods on a vector the `Bounce` had allocated for itself, with the physical address
	// stored beside it as a number nothing referred to - so it proved the two methods agreed with
	// each other and nothing about the address the device was given.
	// AND THE PAIRING IS MADE ONCE, IN `unsafe`, which is the only place it can be made at all: the
	// crate cannot check that a pointer and an address name one allocation, so the promise is stated
	// rather than assumed by a safe signature. This test IS the driver here - it owns the storage
	// and it chooses the address it stands for.
	let mut mapped = [0u8; 64];
	let staging = |bytes: &mut [u8], physical: u64| -> crate::Staging<'_> {
		let len = bytes.len();
		// SAFETY: `bytes` is this test's own storage and `physical` is the address it stands for -
		// one allocation seen two ways, which is what a driver gets from one DMA buffer. No other
		// reference to it is live while the `Staging` is.
		unsafe { crate::Staging::from_mapping(bytes.as_mut_ptr(), len, physical) }
	};
	{
		let mut bounce = crate::Bounce::new(staging(&mut mapped, 0x1000), &requirements).expect("a staging buffer the device can reach");
		assert_eq!(bounce.len(), 64);
		bounce.for_device(&[0xab; 16], &cache).expect("staged");
		assert_eq!(cache.events.borrow().last().copied(), Some(("clean", 0x1000, 16)), "the CPU wrote and the device is about to read, so the writes are pushed out first");
	}
	// THE BYTES ARE AT THE ADDRESS THE DEVICE WAS GIVEN. Asserted against the mapping itself rather
	// than by reading them back through the same object that wrote them.
	assert_eq!(&mapped[..16], &[0xab; 16], "what was staged is in the buffer the device will read");

	// AND THE DEVICE WRITES THERE. Simulated by writing through the mapping, which is what "the CPU
	// view of `physical()`" means - and is exactly what the old shape could not express, because the
	// device's buffer and the staging vector were different memory.
	mapped[..16].copy_from_slice(&[0x5c; 16]);
	let mut back = [0u8; 16];
	{
		let bounce = crate::Bounce::new(staging(&mut mapped, 0x1000), &requirements).expect("the same buffer");
		bounce.for_cpu(&mut back, &cache).expect("read back");
	}
	assert_eq!(back, [0x5c; 16], "the CPU reads what the DEVICE wrote, not what the CPU staged");
	assert_eq!(cache.events.borrow().last().copied(), Some(("invalidate", 0x1000, 16)), "the device wrote and the CPU is about to read, so stale lines go first");

	// A DIRECT plan on the same machine still has sync points - nothing is copied because the data is
	// already where the device looks, and the caches still have to be told. A bounce-shaped API that
	// made the COPY the trigger would silently skip this, which is the case worth having a name for.
	let segments = [crate::Segment { physical: 0x8000, len: 0x1000 }];
	let before = cache.events.borrow().len();
	crate::Bounce::sync_direct_for_device(&segments, &requirements, &cache);
	assert_eq!(cache.events.borrow().len(), before + 1, "a direct plan on a non-coherent machine still cleans before the device reads");

	// AND A COHERENT MACHINE PAYS NOTHING, which is what makes writing a driver against this contract
	// free on the ports that do not need it.
	let coherent = crate::Requirements::new(64, 0x1000, 1, true).expect("a legal requirement");
	let mut quiet_map = [0u8; 16];
	let quiet = cache.events.borrow().len();
	{
		let mut plain = crate::Bounce::new(staging(&mut quiet_map, 0x1000), &coherent).expect("a staging buffer");
		plain.for_device(&[1u8; 8], &cache).expect("staged");
	}
	crate::Bounce::sync_direct_for_device(&segments, &coherent, &cache);
	assert_eq!(cache.events.borrow().len(), quiet, "a coherent machine asks the caches for nothing at either point");
	assert_eq!(&quiet_map[..8], &[1u8; 8], "and the bytes are still where the device will look for them");

	// A staging buffer the device cannot address is not one.
	let mut unreachable = [0u8; 16];
	assert!(matches!(crate::Bounce::new(staging(&mut unreachable, 0x1_0000_0000), &requirements), Err(crate::Fault::OutOfRange)), "a 32-bit device cannot be staged above its ceiling");
	// And a zero-length mapping is not a buffer.
	let mut nothing: [u8; 0] = [];
	assert!(matches!(crate::Bounce::new(staging(&mut nothing, 0x1000), &requirements), Err(crate::Fault::Malformed)));
}

#[test]
fn a_tail_queued_by_an_ended_binding_is_not_charged_to_the_one_that_replaced_it() {
	// THE FLOOD TAIL, WHICH THE GENERATION CHECK ALONE DID NOT CATCH.
	//
	// Each poll is bounded, so an endpoint that faults harder than one drain can read leaves records
	// in the controller when its binding ends. Nothing in a record says which binding raised it: the
	// backend resolves the domain from the endpoint's attachment AT DRAIN TIME, and this layer then
	// stamps that domain's generation. So a tail read after the endpoint was rebound arrived wearing
	// the REPLACEMENT's domain and generation - which is exactly what the cross-generation
	// containment check compares against, so the check passed and the replacement had its bus
	// mastering taken away for a fault it never raised.
	//
	// Driven through the queue and the rebind rather than by handing an old generation to the
	// containment helper: the defect is in what the drain RECONSTRUCTS, and a test that supplies the
	// answer cannot see it.
	let (mut iommu, first) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(first, endpoint).expect("the first binding attaches");
	iommu.revoke_endpoint(first, endpoint).expect("and its binding ends");
	// The domain goes too, which is what a real teardown does - and is why the count below cannot
	// live in `DomainState`.
	iommu.destroy_domain(first).expect("the ended binding's domain is destroyed");

	// THE REPLACEMENT, on the same endpoint, with its own domain and its own generation.
	let second = iommu.create_domain(0x1_0000, 0x10_0000, Vec::new(), Generation(2)).expect("a second domain");
	iommu.attach(second, endpoint).expect("the replacement attaches");

	// AND THE OLD BINDING'S TAIL, READ NOW. The backend reports what it can see, which is the
	// endpoint attached to the REPLACEMENT's domain: this event is what the defect looked like.
	iommu.backend_mut_for_test().queue_fault(event(7, second.0, 0x2000, Access::Write, Fault::NotMapped));
	let mut out = [event(0, 0, 0, Access::Read, Fault::NotMapped); 4];
	assert_eq!(iommu.drain_faults(&mut out), 1);
	assert_eq!(out[0].domain, first, "the tail belongs to the binding that queued it");
	assert_eq!(out[0].generation, Generation(1), "and carries that binding's generation, not the replacement's");
	assert_eq!(iommu.faults_in(first), 1, "charged to the ended binding, whose domain no longer exists");
	assert_eq!(iommu.faults_in(second), 0, "and not to the one that replaced it");

	// AND THE TAIL IS OVER ONCE THE QUEUE HAS RUN DRY. The drain above took fewer events than the
	// buffer holds, so everything queued before it has been read: a fault from here on is the
	// replacement's own and is charged to it.
	iommu.backend_mut_for_test().queue_fault(event(7, second.0, 0x3000, Access::Write, Fault::NotMapped));
	assert_eq!(iommu.drain_faults(&mut out), 1);
	assert_eq!(out[0].domain, second, "a fault raised after the tail was proved gone is the live binding's");
	assert_eq!(out[0].generation, Generation(2));
	assert_eq!(iommu.faults_in(second), 1, "and is charged to it");
	assert_eq!(iommu.faults_in(first), 1, "while the ended binding's count stays where it was");
}

#[test]
fn a_faulting_replacement_is_not_hidden_behind_its_predecessors_tail_for_ever() {
	// A LIVE BINDING THAT FAULTS CONTINUOUSLY, ON AN ENDPOINT WHOSE PREDECESSOR'S TAIL WAS NEVER
	// PROVED DRAINED.
	//
	// The tail stopped attributing only when the transport was next observed EMPTY, and a
	// replacement faulting harder than the drain reads never lets that be observed: every one of its
	// own faults came out carrying the dead binding's domain and generation, failed the
	// cross-generation comparison the kernel contains on, and no live binding was ever taken off the
	// bus. The ended binding queued a FINITE number of records, so a stream that outlasts what the
	// transport can hold is not that binding's.
	//
	// Three records is a whole transport's worth for this fixture, so the fourth is the replacement
	// answering for itself.
	let mut iommu = Iommu::new(Fake::new().with_fault_queue_capacity(3), 8);
	let first = iommu.create_domain(0x1_0000, 0x10_0000, Vec::new(), Generation(1)).expect("a domain");
	let endpoint = EndpointId(9);
	iommu.attach(first, endpoint).expect("the first binding attaches");
	iommu.revoke_endpoint(first, endpoint).expect("and its binding ends");
	iommu.destroy_domain(first).expect("its domain is destroyed with it");
	let second = iommu.create_domain(0x1_0000, 0x10_0000, Vec::new(), Generation(2)).expect("a second domain");
	iommu.attach(second, endpoint).expect("the replacement attaches");

	// THE STORM. More queued than any drain reads, so the transport is never observed empty - which
	// is the state that used to make the misattribution permanent.
	for _ in 0..6 {
		iommu.backend_mut_for_test().queue_fault(event(9, second.0, 0x2000, Access::Write, Fault::NotMapped));
	}
	let mut out = [event(0, 0, 0, Access::Read, Fault::NotMapped); 1];
	for _ in 0..3 {
		assert_eq!(iommu.drain_faults(&mut out), 1);
		assert_eq!(out[0].domain, first, "while the tail can still explain it, the record is the ended binding's");
		assert_eq!(out[0].generation, Generation(1));
	}
	assert_eq!(iommu.faults_in(first), 3, "the ended binding is charged with a transport's worth and no more");

	// AND THE ONE PAST THE BOUND IS THE LIVE BINDING'S OWN.
	assert_eq!(iommu.drain_faults(&mut out), 1);
	assert_eq!(out[0].domain, second, "past what the predecessor could have queued, the endpoint is faulting now");
	assert_eq!(out[0].generation, Generation(2), "so the containment decision meets the generation that is actually live");
	assert_eq!(iommu.faults_in(second), 1, "and the accounting follows the attribution");
	assert_eq!(iommu.faults_in(first), 3, "while the ended binding's count stays where the bound left it");
}

#[test]
fn the_window_a_guess_is_bounded_by_is_not_spent_on_records_that_needed_no_guess() {
	// WHAT THE BOUND IS ABOUT IS AMBIGUITY, AND IT WAS BEING CHARGED FOR CERTAINTY (2026-09-03).
	//
	// `attributed` counted every record the tail rewrote. While the endpoint is still DETACHED the
	// backend can resolve it to no domain at all, so the record arrives as `DomainId(0)` and the
	// only binding it can belong to is the one that ended - there is no replacement yet to charge
	// it to. Spending the policy window on those meant a teardown whose queue outran
	// `MOST_TAIL_ATTRIBUTIONS` before it was read exhausted the whole bound on records that were
	// EXACT, and then handed the genuinely ambiguous ones that follow to a live binding: the error
	// direction the constant names, reached without any of the uncertainty it exists for.
	//
	// Everything is queued before anything is drained, deliberately: the transport is never observed
	// EMPTY until the last record, which is the other way a tail ends and would otherwise close this
	// one before the ambiguous half begins.
	let (mut iommu, first) = iommu();
	let endpoint = EndpointId(12);
	iommu.attach(first, endpoint).expect("the first binding attaches");
	iommu.revoke_endpoint(first, endpoint).expect("and its binding ends");

	// THE UNAMBIGUOUS HALF, raised while nothing was attached - more than the window, deliberately.
	let certain: u64 = crate::MOST_TAIL_ATTRIBUTIONS + 8;
	for _ in 0..certain {
		iommu.backend_mut_for_test().queue_fault(event(12, 0, 0x2000, Access::Write, Fault::NotMapped));
	}
	// AND THE AMBIGUOUS HALF, which is what the window is for: the replacement is attached and these
	// resolve to it, so each one may be the predecessor's tail or the replacement faulting now.
	iommu.destroy_domain(first).expect("the ended binding's domain is destroyed with it");
	let second = iommu.create_domain(0x1_0000, 0x10_0000, Vec::new(), Generation(2)).expect("a second domain");
	iommu.attach(second, endpoint).expect("the replacement attaches");
	for _ in 0..crate::MOST_TAIL_ATTRIBUTIONS + 1 {
		iommu.backend_mut_for_test().queue_fault(event(12, second.0, 0x2000, Access::Write, Fault::NotMapped));
	}

	let mut out = [event(0, 0, 0, Access::Read, Fault::NotMapped); 1];
	for at in 0..certain {
		assert_eq!(iommu.drain_faults(&mut out), 1);
		assert_eq!(out[0].domain, first, "record {at} was raised with the endpoint detached and can only be the ended binding's");
		assert_eq!(out[0].generation, Generation(1));
	}
	assert_eq!(iommu.faults_in(first), certain, "every one of them is charged to it, past the window and all");

	// AND THE WINDOW IS STILL WHOLE FOR THE ONES THAT ACTUALLY NEEDED IT.
	for at in 0..crate::MOST_TAIL_ATTRIBUTIONS {
		assert_eq!(iommu.drain_faults(&mut out), 1);
		assert_eq!(out[0].domain, first, "ambiguous record {at} is inside the stated window and is read as the tail's");
	}
	assert_eq!(iommu.drain_faults(&mut out), 1);
	assert_eq!(out[0].domain, second, "past it the endpoint answers for itself, on the generation that is live");
	assert_eq!(out[0].generation, Generation(2), "so the containment decision meets the generation that is actually live");
	assert_eq!(iommu.faults_in(second), 1, "and the accounting follows the attribution");
}

#[test]
fn a_backend_that_cannot_state_its_transports_bound_falls_back_to_the_stated_policy() {
	// THE DEFAULT IS THE POLICY, NOT "FOR EVER" (corrected 2026-09-03). `fault_queue_capacity`
	// answers `None` for every transport in this tree, because a driver that posts one event buffer
	// at a time cannot see what the device is holding behind it - so if `None` meant unbounded, the
	// bound would apply to nothing that ships. `MOST_TAIL_ATTRIBUTIONS` is what applies, and the
	// window it opens is wide: a healthy replacement is not charged for its predecessor's queue
	// until well past any ordinary teardown's tail.
	let (mut iommu, first) = iommu();
	let endpoint = EndpointId(11);
	iommu.attach(first, endpoint).expect("the first binding attaches");
	iommu.revoke_endpoint(first, endpoint).expect("and its binding ends");
	iommu.destroy_domain(first).expect("its domain is destroyed with it");
	let second = iommu.create_domain(0x1_0000, 0x10_0000, Vec::new(), Generation(2)).expect("a second domain");
	iommu.attach(second, endpoint).expect("the replacement attaches");
	let queued: u64 = crate::MOST_TAIL_ATTRIBUTIONS + 4;
	for _ in 0..queued {
		iommu.backend_mut_for_test().queue_fault(event(11, second.0, 0x2000, Access::Write, Fault::NotMapped));
	}
	let mut out = [event(0, 0, 0, Access::Read, Fault::NotMapped); 1];
	for _ in 0..crate::MOST_TAIL_ATTRIBUTIONS {
		assert_eq!(iommu.drain_faults(&mut out), 1);
		assert_eq!(out[0].domain, first, "inside the stated window the record is the ended binding's");
	}
	assert_eq!(iommu.faults_in(first), crate::MOST_TAIL_ATTRIBUTIONS, "the predecessor is charged with the whole window and no more");
	assert_eq!(iommu.faults_in(second), 0, "and nothing before it reached the replacement");
	assert_eq!(iommu.drain_faults(&mut out), 1);
	assert_eq!(out[0].domain, second, "past the window the endpoint answers for itself");
	assert_eq!(out[0].generation, Generation(2), "so the containment decision meets the generation that is actually live");
}

#[test]
fn a_tail_record_is_bounded_and_gives_up_the_ones_that_attribute_nothing_first() {
	// FIXED BOOKKEEPING. A machine that restarts one driver in a loop must not grow a record per
	// restart, and one that runs out of records must not silently give up one that is still
	// protecting a replacement - which is what the first version of this bound did.
	let (mut iommu, _) = iommu();
	// ONE RECORD PER ENDPOINT, so filling the bound needs that many DISTINCT endpoints - repeated
	// revocations of one endpoint merge and cannot fill it at all.
	for index in 0..(MOST_DETACHED_TAILS as u32) {
		let domain = iommu.create_domain(0x1_0000, 0x10_0000, Vec::new(), Generation(index as u64 + 1)).expect("a domain");
		let endpoint = EndpointId(100 + index);
		iommu.attach(domain, endpoint).expect("attached");
		iommu.revoke_endpoint(domain, endpoint).expect("and ended");
	}
	assert_eq!(iommu.detached_tails(), MOST_DETACHED_TAILS, "the bound holds exactly");
	assert!(iommu.attribution_is_complete(), "nothing has been given up yet");

	// ONE MORE, WITH NOTHING DRAINED. There is no record that attributes nothing, so rather than
	// evicting one that does - which hands its endpoint's queued tail back to the current-attachment
	// path, the defect this whole mechanism closes - the ledger says it can no longer tell an ended
	// binding's fault from a live one's, and stops taking containment decisions on generation.
	let extra = iommu.create_domain(0x1_0000, 0x10_0000, Vec::new(), Generation(99)).expect("a domain");
	let overflowing = EndpointId(200);
	iommu.attach(extra, overflowing).expect("attached");
	iommu.revoke_endpoint(extra, overflowing).expect("and ended");
	assert_eq!(iommu.detached_tails(), MOST_DETACHED_TAILS, "the list does not grow past its bound to hold it either");
	assert!(!iommu.attribution_is_complete(), "and it says so rather than dropping a live attribution quietly");
	assert_eq!(iommu.tails_overflowed(), 1, "counted, so a machine whose storm outlasted its bookkeeping is visible");

	// AND THE FIRST RECORD IS STILL THERE AND STILL ATTRIBUTING - which is the assertion the old
	// version of this test could not make, because the old rule had already evicted it.
	let mut out = [event(0, 0, 0, Access::Read, Fault::NotMapped); 4];
	iommu.backend_mut_for_test().queue_fault(event(100, 0, 0x4000, Access::Write, Fault::NotMapped));
	assert_eq!(iommu.drain_faults(&mut out), 1);
	assert_eq!(iommu.faults_in(DomainId(out[0].domain.0)), 1, "the oldest ended binding still owns its tail");
	assert_eq!(out[0].generation, Generation(1), "and names itself, rather than resolving through whatever is attached now");

	// The drain above ran the transport dry, which is the proof that clears both the tails and the
	// lost attribution: nothing queued before it can still be in flight.
	assert!(iommu.attribution_is_complete(), "a dry transport is what makes containment on generation trustworthy again");
}

#[test]
fn a_second_revocation_does_not_discard_the_tail_of_the_one_before_it() {
	// FIFO MEANS THE OLDER TAIL IS IN FRONT, WHICH THE FIRST VERSION HAD BACKWARDS.
	//
	// `remember_detached_tail` replaced an existing record for the same endpoint, on the reasoning
	// that "an older tail cannot still be queued behind a newer one - the queue is FIFO, so the
	// newer binding's tail is what a reader meets first". First in, first out: the OLDER binding's
	// records were queued first and are read first, so they are in front. Replacing the record threw
	// away the attribution for events that had not been read yet, and they were then charged to the
	// binding that came after them.
	let (mut iommu, first) = iommu();
	let endpoint = EndpointId(7);
	iommu.attach(first, endpoint).expect("the first binding attaches");
	iommu.revoke_endpoint(first, endpoint).expect("and ends");

	// A SECOND BINDING ON THE SAME ENDPOINT, ending before anything drained the first one's tail.
	let second = iommu.create_domain(0x1_0000, 0x10_0000, Vec::new(), Generation(2)).expect("a second domain");
	iommu.attach(second, endpoint).expect("the second binding attaches");
	iommu.revoke_endpoint(second, endpoint).expect("and ends too");
	assert_eq!(iommu.detached_tails(), 1, "one undrained record per endpoint: the second revocation MERGES rather than replacing the first or pushing beside it");
	assert!(iommu.attribution_is_complete(), "and nothing was given up to make room, so containment on generation is still trustworthy");

	// AND THE FRONT OF THE QUEUE IS THE FIRST BINDING'S. The backend reports whatever attachment it
	// can see, which is neither of them by now; the record is what says whose it is.
	iommu.backend_mut_for_test().queue_fault(event(7, 0, 0x2000, Access::Write, Fault::NotMapped));
	let mut out = [event(0, 0, 0, Access::Read, Fault::NotMapped); 4];
	assert_eq!(iommu.drain_faults(&mut out), 1);
	assert_eq!(out[0].domain, first, "the oldest undrained record is what a FIFO reader meets first");
	assert_eq!(out[0].generation, Generation(1));
	assert_eq!(iommu.faults_in(first), 1, "charged to the binding whose tail it is");
	assert_eq!(iommu.faults_in(second), 0, "and not to the one that ended after it");
}
