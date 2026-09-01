// What can be checked about a controller on a machine that may not have one.
//
// THE HONEST SCOPE OF A UNIT TEST HERE. Bring-up is a conversation with a device: this file cannot
// invent one, and a test that faked the device would be testing the fake. What it CAN check is the
// part that is a decision rather than a conversation - that a machine without a controller reports
// no enforcement rather than assuming it, and that the two states the rest of the kernel reads agree
// with each other. The wire format and every failure order are host-tested in the `dma` crate, and
// the QEMU profile has a gate of its own.

use super::*;

crate::tagged_test!(a_machine_without_a_controller_does_not_claim_enforcement, [Dma, Kernel], id = "kernel.iommu.a_machine_without_a_controller_does_not_claim_enforcement", covers = ["kernel"]);
fn a_machine_without_a_controller_does_not_claim_enforcement() {
	// THREE STATES, NOT TWO, and the middle one is the whole point of `present`.
	//
	// `present()` is a fact about the BUS - a controller was found - and `enforcing()` is a fact
	// about the bring-up. `PRESENT` used to be stored in the success arm only, so the two were the
	// same question asked twice, and a controller that failed feature negotiation, queue creation or
	// the bypass read-back left `present()` answering false. `dma_policy` reads exactly that to
	// decide whether this machine was SUPPOSED to be isolated, so the one case a protected driver
	// must refuse became indistinguishable from a machine that never had a controller.
	match (present(), crate::dma_policy::enforcing()) {
		// A controller on the bus, translating. `with` has one to hand out.
		(true, true) => {
			assert!(with(|controller| controller.still_enforcing()).unwrap_or(false), "a controller that came up reports bypass off");
			assert!(translating(), "there is a controller to attach endpoints to");
			assert!(crate::dma_policy::isolation_expected(), "a machine with a controller expects isolation");
		}
		// A controller on the bus that did not come up. Isolation was available and is not here,
		// which is the case a protected driver refuses rather than degrades into.
		(true, false) => {
			assert!(with(|_| ()).is_none(), "a controller that failed bring-up is not handed out to callers");
			assert!(!translating(), "and there is nothing to attach endpoints to or map buffers through");
			assert!(crate::dma_policy::isolation_expected(), "the bus still has one, so this machine expected isolation and does not have it");
		}
		// No controller at all: the ordinary harness profile, and every machine whose firmware
		// offers none. Untranslated DMA is the only DMA there is and nothing pretends otherwise.
		(false, false) => {
			assert!(with(|_| ()).is_none(), "and there is no controller to ask");
			assert!(!translating(), "nor one to map through");
			assert!(!crate::dma_policy::isolation_expected(), "a machine that never had one does not expect isolation");
		}
		(false, true) => panic!("enforcement without a controller on the bus is not a state this kernel has"),
	}
}

crate::tagged_test!(a_controller_that_did_not_come_up_is_not_a_machine_without_one, [Dma, Kernel], id = "kernel.iommu.a_controller_that_did_not_come_up_is_not_a_machine_without_one", covers = ["kernel"]);
fn a_controller_that_did_not_come_up_is_not_a_machine_without_one() {
	// THE CONSEQUENCE OF THE MIDDLE STATE, driven through the policy that reads it.
	//
	// The two facts are set here rather than taken from the machine this happens to run on, because
	// the subject is what the policy DOES with them and that must be the same answer on every
	// profile. `isolation_expected` is the fact about the bus; `enforcing` is the fact about the
	// bring-up. Their four combinations are three real machines and one impossible one.
	let was_expected = crate::dma_policy::isolation_expected();
	let was_enforcing = crate::dma_policy::enforcing();
	const PROTECTED: u16 = abi::VIRTIO_TYPE_NET as u16;

	// A controller on the bus that did not come up: the driver that declared it needs translation
	// does not run. This is the state that used to be recorded as "no controller", and a protected
	// driver then bound DEGRADED - untranslated DMA on a machine that was supposed to be isolated.
	crate::dma_policy::set_isolation_expected(true);
	crate::dma_policy::set_enforcing(false);
	assert_eq!(crate::dma_policy::policy_for(PROTECTED), dma::Policy::IommuRequired);
	assert_eq!(crate::dma_policy::admit(PROTECTED, 0, 2, 0), dma::BindDecision::Refused, "isolation was available and is not here - this driver does not run");

	// The same machine once the controller is translating.
	crate::dma_policy::set_enforcing(true);
	assert_eq!(crate::dma_policy::admit(PROTECTED, 0, 2, 0), dma::BindDecision::Translated);

	// And a machine that never had one: untranslated is the only DMA there is, and networking is
	// not withdrawn over isolation this machine never offered.
	crate::dma_policy::set_isolation_expected(false);
	crate::dma_policy::set_enforcing(false);
	assert_eq!(crate::dma_policy::policy_for(PROTECTED), dma::Policy::TrustedUntranslated);
	assert_eq!(crate::dma_policy::admit(PROTECTED, 0, 2, 0), dma::BindDecision::DegradedUntranslated);

	crate::dma_policy::forget_degraded(0, 2, 0);
	crate::dma_policy::set_isolation_expected(was_expected);
	crate::dma_policy::set_enforcing(was_enforcing);
}

crate::tagged_test!(draining_faults_without_a_controller_is_bounded_and_empty, [Dma, Kernel], id = "kernel.iommu.draining_faults_without_a_controller_is_bounded_and_empty", covers = ["kernel"]);
fn draining_faults_without_a_controller_is_bounded_and_empty() {
	// The fault path is polled from the kernel's own loop, so it has to be safe to call on a machine
	// with no IOMMU at all - the alternative is a call site that has to know, and one that forgets.
	let mut out = [dma::FaultEvent { endpoint: dma::EndpointId(0), domain: dma::DomainId(0), generation: dma::Generation(0), address: None, access: dma::Access::Read, reason: dma::Fault::NotMapped }; 4];
	let taken = drain_faults(&mut out, None);
	if !present() {
		assert_eq!(taken, 0, "no controller reports no faults");
	}
	assert!(taken <= out.len(), "a drain never writes past the buffer it was given");
}

// THE HOSTILE ENDPOINT. Every case below is one of the required isolation cases, driven against
// QEMU's `edu` device - a PCI function whose DMA engine copies between an arbitrary physical address
// and its own buffer on command. It is the closest thing in reach to a malicious driver.
//
// SKIPPED WHERE THE FIXTURE IS ABSENT, AND SAID SO. Every boot but the enforcing profile's has no
// `edu` and no IOMMU, so these would otherwise be tests that pass by not running - which is the
// shape of evidence this tree refuses. Each prints what it did, and the QEMU gate requires the lines
// that say the cases actually ran.

fn fixture() -> Option<(super::edu::Edu, bool)> {
	let edu = super::edu::find()?;
	Some((edu, present()))
}

crate::tagged_test!(a_device_that_was_never_attached_cannot_reach_a_sentinel, [Dma, Kernel], id = "kernel.iommu.a_device_that_was_never_attached_cannot_reach_a_sentinel", covers = ["kernel"]);
fn a_device_that_was_never_attached_cannot_reach_a_sentinel() {
	let Some((edu, enforcing)) = fixture() else {
		crate::serial_println!("iommu-fixture: absent (no edu device on this machine)");
		return;
	};
	assert!(edu.alive(), "the fixture answers on its own register window");
	let sentinel = super::edu::Sentinel::new(0xA5).expect("a frame for the sentinel");
	// The device may master the bus and has NOT been attached to any domain. Under an enforcing
	// IOMMU there is no address space in which this physical address means anything.
	edu.set_bus_master(true);
	let finished = edu.transfer(sentinel.physical, 0x1000, true);
	edu.set_bus_master(false);
	if enforcing {
		assert!(sentinel.intact(), "an endpoint with no domain reached a page it was never given - the boundary this milestone claims does not hold");
		crate::serial_println!("iommu-fixture: case 1 PASSED - DMA before attach changed nothing (transfer completed={finished})");
	} else {
		// WITHOUT AN IOMMU THE SAME CALL SUCCEEDS, and the test says so rather than passing quietly:
		// this is the hole, demonstrated, and it is why the milestone exists.
		assert!(!sentinel.intact(), "with no IOMMU the device reaches any address it is told to - if this held, the fixture is not doing what it claims");
		crate::serial_println!("iommu-fixture: case 1 baseline - NO IOMMU, and the device overwrote the sentinel exactly as an unconstrained device does");
	}
	poll_faults();
}

crate::tagged_test!(a_mapping_is_reachable_to_its_last_byte_and_not_one_past_it, [Dma, Kernel], id = "kernel.iommu.a_mapping_is_reachable_to_its_last_byte_and_not_one_past_it", covers = ["kernel"]);
fn a_mapping_is_reachable_to_its_last_byte_and_not_one_past_it() {
	let Some((edu, enforcing)) = fixture() else {
		crate::serial_println!("iommu-fixture: absent (no edu device on this machine)");
		return;
	};
	if !enforcing {
		crate::serial_println!("iommu-fixture: case 3 skipped - no enforcing IOMMU on this machine");
		return;
	}
	// One page mapped for this endpoint, and the page after it deliberately not.
	//
	// THE PAGE AFTER IT IN THE DEVICE'S ADDRESS SPACE, which is the only space this case is about.
	// The two sentinels are separate frames wherever the allocator put them; what makes the second
	// one "one past the mapping" is that `address + 0x1000` is mapped in this domain to it and to
	// nothing else - so it is mapped explicitly rather than hoped for, and then taken away again,
	// leaving a number the device knows the shape of and may not resolve.
	let inside = super::edu::Sentinel::new(0x11).expect("a frame");
	let outside = super::edu::Sentinel::new(0x22).expect("a frame");
	let domain = attach_endpoint(edu.bus, edu.dev, edu.func, 1).expect("the fixture's endpoint attaches");
	let (mapping, address) = map_for_device(domain, inside.physical, 0x1000, dma::Direction::FromDevice).expect("one page mapped");
	// A second mapping, immediately after the first, only so this case knows which number is the
	// first byte past the mapping. It is closed before the transfers, so the address is one the
	// domain has no translation for.
	if let Ok((neighbour, next_address)) = map_for_device(domain, outside.physical, 0x1000, dma::Direction::FromDevice) {
		let adjacent = next_address.get() == address.get() + 0x1000;
		let _ = unmap_for_device(neighbour);
		assert!(adjacent, "the allocator did not place the second mapping immediately after the first, so this case cannot say which address is one past the end");
	}

	edu.set_bus_master(true);
	// The last byte of the mapping is reachable: a one-byte transfer at the far end must land.
	let _ = edu.transfer(address.get() + 0xFFF, 1, true);
	// And the first byte past it is not.
	let _ = edu.transfer(address.get() + 0x1000, 0x1000, true);
	edu.set_bus_master(false);

	assert!(!inside.intact(), "the mapped page is reachable - a device that could not write its own mapping would make every other case here meaningless");
	assert!(outside.intact(), "the page after the mapping was reached, which is the off-by-one an IOMMU exists to prevent");
	crate::serial_println!("iommu-fixture: case 3 PASSED - the mapping's last byte is reachable and the first byte after it is not");
	let _ = unmap_for_device(mapping);
	let _ = revoke_endpoint(domain, edu.bus, edu.dev, edu.func);
	poll_faults();
}

crate::tagged_test!(a_device_write_to_a_read_only_mapping_changes_nothing, [Dma, Kernel], id = "kernel.iommu.a_device_write_to_a_read_only_mapping_changes_nothing", covers = ["kernel"]);
fn a_device_write_to_a_read_only_mapping_changes_nothing() {
	let Some((edu, enforcing)) = fixture() else {
		crate::serial_println!("iommu-fixture: absent (no edu device on this machine)");
		return;
	};
	if !enforcing {
		crate::serial_println!("iommu-fixture: case 6 skipped - no enforcing IOMMU on this machine");
		return;
	}
	let sentinel = super::edu::Sentinel::new(0x33).expect("a frame");
	let domain = attach_endpoint(edu.bus, edu.dev, edu.func, 1).expect("attached");
	// TO-DEVICE: the device may READ this page and may not write it.
	let (mapping, address) = map_for_device(domain, sentinel.physical, 0x1000, dma::Direction::ToDevice).expect("mapped read-only");
	edu.set_bus_master(true);
	// The permitted direction: the device reads the page into its own buffer.
	let read = edu.transfer(address.get(), 0x1000, false);
	// And the forbidden one: the device tries to write it.
	let _ = edu.transfer(address.get(), 0x1000, true);
	edu.set_bus_master(false);
	assert!(sentinel.intact(), "a device wrote a mapping made for reading - the direction claim does not hold");
	crate::serial_println!("iommu-fixture: case 6 PASSED - the permitted direction worked (read={read}) and the forbidden one changed nothing");
	let _ = unmap_for_device(mapping);
	let _ = revoke_endpoint(domain, edu.bus, edu.dev, edu.func);
	poll_faults();
}

crate::tagged_test!(an_unmapped_address_is_unreachable_once_its_frame_belongs_to_somebody_else, [Dma, Kernel], id = "kernel.iommu.an_unmapped_address_is_unreachable_once_its_frame_belongs_to_somebody_else", covers = ["kernel"]);
fn an_unmapped_address_is_unreachable_once_its_frame_belongs_to_somebody_else() {
	let Some((edu, enforcing)) = fixture() else {
		crate::serial_println!("iommu-fixture: absent (no edu device on this machine)");
		return;
	};
	if !enforcing {
		crate::serial_println!("iommu-fixture: case 7 skipped - no enforcing IOMMU on this machine");
		return;
	}
	// A page the device is given, then taken away - and THE SAME FRAME handed to somebody else,
	// which is the state a stale descriptor actually threatens.
	//
	// THIS CASE USED TO BE UNFAILABLE. It allocated `first`, unmapped it, and then allocated `next`
	// while `first` was still alive and still owned its frame - so `next` was a DIFFERENT frame, and
	// the stale translation under test pointed at `first`'s. The assertion read `next.intact()`,
	// which was true with an IOMMU, without one, and against a backend that ignored every unmap.
	//
	// So the frame is released first and the claim is made only if the allocator really handed the
	// same one back. If it did not, this case cannot be made on this boot and says so instead of
	// passing: a case that reports success without having tested anything is worse than one that
	// reports it could not run.
	let first = super::edu::Sentinel::new(0x44).expect("a frame");
	let contested = first.physical;
	let domain = attach_endpoint(edu.bus, edu.dev, edu.func, 1).expect("attached");
	let (mapping, address) = map_for_device(domain, contested, 0x1000, dma::Direction::FromDevice).expect("mapped");
	assert_eq!(unmap_for_device(mapping), Ok(dma::Release::FramesReusable), "the unmap and its invalidation both completed");
	// The frame goes back to the allocator, and the next owner asks for one.
	drop(first);
	let next = super::edu::Sentinel::new(0x55).expect("the frame's next owner");
	if next.physical != contested {
		crate::serial_println!("iommu-fixture: case 7 INCONCLUSIVE - the allocator returned {:#x} rather than the released {:#x}, so no second owner holds the contested frame", next.physical, contested);
		let _ = revoke_endpoint(domain, edu.bus, edu.dev, edu.func);
		poll_faults();
		return;
	}
	edu.set_bus_master(true);
	let _ = edu.transfer(address.get(), 0x1000, true);
	edu.set_bus_master(false);
	assert!(next.intact(), "a device reached an address after its mapping was revoked, and changed the frame's next owner");
	crate::serial_println!("iommu-fixture: case 7 PASSED - DMA to a revoked address did not reach the frame's next owner, and the next owner holds the same frame");
	let _ = revoke_endpoint(domain, edu.bus, edu.dev, edu.func);
	poll_faults();
}

crate::tagged_test!(one_numeric_address_means_different_memory_to_two_endpoints, [Dma, Kernel], id = "kernel.iommu.one_numeric_address_means_different_memory_to_two_endpoints", covers = ["kernel"]);
fn one_numeric_address_means_different_memory_to_two_endpoints() {
	let (Some(first), Some(second)) = (super::edu::find_nth(0), super::edu::find_nth(1)) else {
		crate::serial_println!("iommu-fixture: case 5 skipped - this profile has fewer than two edu devices");
		return;
	};
	if !present() {
		crate::serial_println!("iommu-fixture: case 5 skipped - no enforcing IOMMU on this machine");
		return;
	}
	// IOVAS ARE DOMAIN-LOCAL, which is the property that makes "another endpoint's page" mean
	// anything at all. Two endpoints, two domains, and the same NUMBER mapped in each - to different
	// frames. Then one of them is unmapped, and the other must be untouched by anything the first
	// does with that number.
	let mine = super::edu::Sentinel::new(0x66).expect("a frame");
	let theirs = super::edu::Sentinel::new(0x77).expect("a frame");
	let domain_a = attach_endpoint(first.bus, first.dev, first.func, 1).expect("endpoint a attaches");
	let domain_b = attach_endpoint(second.bus, second.dev, second.func, 1).expect("endpoint b attaches");
	let (mapping_a, address_a) = map_for_device(domain_a, mine.physical, 0x1000, dma::Direction::FromDevice).expect("a is mapped");
	let (mapping_b, address_b) = map_for_device(domain_b, theirs.physical, 0x1000, dma::Direction::FromDevice).expect("b is mapped");
	assert_eq!(address_a, address_b, "two domains independently chose the same number, which is what makes this case a case");

	// Take A's mapping away and leave B's alone.
	assert_eq!(unmap_for_device(mapping_a), Ok(dma::Release::FramesReusable), "a's mapping is gone and its invalidation completed");
	first.set_bus_master(true);
	let _ = first.transfer(address_a.get(), 0x1000, true);
	first.set_bus_master(false);
	assert!(theirs.intact(), "endpoint A reached endpoint B's page through a number that means nothing in A's domain any more");

	// AND B STILL WORKS. Half of "the domains are independent" is that A cannot reach B; the other
	// half is that taking A's translation down did not take B's with it - an invalidation that flushed
	// the whole device rather than one domain would pass the assertion above and break this one.
	second.set_bus_master(true);
	let _ = second.transfer(address_b.get(), 0x1000, true);
	second.set_bus_master(false);
	assert!(!theirs.intact(), "endpoint B could no longer reach its own live mapping after A's was closed");
	crate::serial_println!("iommu-fixture: case 5 PASSED - the same numeric address is domain-local, A cannot reach B through it, and B still reaches its own");

	let _ = unmap_for_device(mapping_b);
	let _ = revoke_endpoint(domain_a, first.bus, first.dev, first.func);
	let _ = revoke_endpoint(domain_b, second.bus, second.dev, second.func);
	poll_faults();
}

crate::tagged_test!(a_buffer_from_a_previous_binding_is_not_mapped_into_the_next_one, [Dma, Kernel, Pci], id = "kernel.iommu.a_buffer_from_a_previous_binding_is_not_mapped_into_the_next_one", covers = ["kernel"]);
fn a_buffer_from_a_previous_binding_is_not_mapped_into_the_next_one() {
	// THE DOMAIN IS CHOSEN BY DEVICE INDEX, AND AN INDEX OUTLIVES THE BINDING IN IT.
	//
	// `sys_dma_buffer_create` reads the claim off the capability it was handed, allocates, and only
	// then asks which domain to map into. A release and a reclaim in between put the REPLACEMENT
	// binding's domain under that index, so the old caller's mapping landed in a domain it had no
	// authority over - and the derived-object registration that refuses the stale generation runs
	// AFTERWARDS, which rolls back the bookkeeping and not the translation. The generation now
	// travels with the request.
	//
	// Asked of the fixture rather than of a fake, for the reason this whole file states: a domain is
	// something a controller creates, and there is no controller to invent here.
	let Some((edu, enforcing)) = fixture() else {
		crate::serial_println!("iommu-fixture: absent (no edu device on this machine)");
		return;
	};
	if !enforcing {
		crate::serial_println!("iommu-fixture: generation case skipped - no enforcing IOMMU on this machine");
		return;
	}
	let Some(index) = (0..crate::device::count()).find(|&i| crate::device::with(i, |d| d.bus == edu.bus && d.dev == edu.dev && d.func == edu.func).unwrap_or(false)) else {
		panic!("the fixture's function is on the bus and has no device-table row - the binder could not name it either");
	};
	const BINDING: u64 = 0x5eed_0001;
	let sentinel = super::edu::Sentinel::new(0x33).expect("a frame for the sentinel");
	assert!(attach_for(index, edu.bus, edu.dev, edu.func, BINDING), "the fixture's endpoint attaches under a binding of its own");

	// THE NEXT BINDING'S NUMBER, AND THE ONE BEFORE IT. Neither is this domain's, and a mapping for
	// either would be one generation reaching into another's address space.
	assert_eq!(map_device_buffer(index as u32, BINDING + 1, sentinel.physical, 0x1000), Err(dma::Fault::StaleGeneration), "a later binding's number mapped into this binding's domain");
	assert_eq!(map_device_buffer(index as u32, BINDING - 1, sentinel.physical, 0x1000), Err(dma::Fault::StaleGeneration), "an earlier binding's number mapped into this binding's domain");

	// AND THE BINDING'S OWN NUMBER STILL WORKS, so the refusal above is arithmetic on the generation
	// rather than a mapping path that stopped working.
	let mapped = map_device_buffer(index as u32, BINDING, sentinel.physical, 0x1000).expect("the binding's own generation maps");
	let (mapping, _) = mapped.expect("the fixture is translated, so the map answers with one");
	let _ = unmap_for_device(mapping);
	assert!(detach_for(index, edu.bus, edu.dev, edu.func), "the fixture's endpoint detaches");
	crate::serial_println!("iommu-fixture: generation case PASSED - only the live binding's generation maps into its domain");
	poll_faults();
}

crate::tagged_test!(a_translated_address_stops_translating_when_its_claim_is_forced_to_end, [Dma, Kernel, Pci, Object], id = "kernel.iommu.a_translated_address_stops_translating_when_its_claim_is_forced_to_end", covers = ["kernel"]);
fn a_translated_address_stops_translating_when_its_claim_is_forced_to_end() {
	// THE HOSTILE HOLDER, WITH A LIVE TRANSLATION IN ITS HAND.
	//
	// P02M0098's M9 names three revocations and its definition of done names them again: the MMIO
	// window reached through the raw virtual address, the interrupt vector, and the DMA translation.
	// The first two had a test each. The third was proved by two things that are not it - that a
	// STALE-generation map is refused, and that the derived table's rows are revoked - and neither
	// asks the question M9 does, which is what happens to an address the holder ALREADY HAS when the
	// claim behind it is taken away without asking.
	//
	// So this holds a live translated IOVA across a forced release and asks the DEVICE. A ledger that
	// says the mapping is gone and a controller that still translates it would satisfy every earlier
	// test and none of this one.
	let Some((edu, enforcing)) = fixture() else {
		crate::serial_println!("iommu-fixture: absent (no edu device on this machine)");
		return;
	};
	if !enforcing {
		// WITHOUT A CONTROLLER THERE IS NO TRANSLATION TO REVOKE. Said rather than passed over: the
		// untranslated machine's baseline is case 1's, and repeating it here would be a second
		// answer to a question that already has one.
		crate::serial_println!("iommu-fixture: forced-release case skipped - no enforcing IOMMU on this machine");
		return;
	}
	let Some(index) = (0..crate::device::count()).find(|&i| crate::device::with(i, |d| d.bus == edu.bus && d.dev == edu.dev && d.func == edu.func).unwrap_or(false)) else {
		panic!("the fixture's function is on the bus and has no device-table row - the binder could not name it either");
	};
	let sentinel = super::edu::Sentinel::new(0x5a).expect("a frame for the sentinel");

	// THE BINDING, TAKEN THE WAY DEVICEMANAGER TAKES ONE, so the generation below is a real claim's
	// and the release is the production forced one rather than a detach called by hand.
	let key = crate::device::claim(index).expect("the fixture's function is claimable");
	assert!(attach_for(index, edu.bus, edu.dev, edu.func, key.generation), "the fixture's endpoint attaches under its claim");
	let (_mapping, iova) = map_device_buffer(index as u32, key.generation, sentinel.physical, 0x1000).expect("the binding's own generation maps").expect("the fixture is translated, so the map answers with an address");

	// AND IT WORKS FIRST, which is what makes everything after it a claim about a revocation rather
	// than about an address that never resolved. The device is told to write its buffer into RAM at
	// the address the controller gave this binding, and the sentinel must change.
	edu.set_bus_master(true);
	let reached = edu.transfer(iova.get(), 0x1000, true);
	assert!(!sentinel.intact(), "the device did not reach its own translated address (transfer completed={reached}) - there is no live translation here to revoke");

	// THE FORCED RELEASE. Nothing asks the holder for anything, and the holder here is still very
	// much alive: the device is mastering the bus with a translation it was given.
	assert_eq!(crate::device::release_claim(key), Ok(crate::device::ClaimState::Free), "the forced release did not confirm its teardown");

	// A SECOND BINDING ON THE SAME FUNCTION, and this is the part that makes the question honest.
	// The release turns bus mastering off, so a test that stopped at the release would be proving
	// the bus-master bit rather than the translation. The replacement puts the device back on the
	// bus - under its own claim, its own generation and its own domain - and the OLD address must
	// still reach nothing.
	let next = crate::device::claim(index).expect("the function is claimable again once its release confirmed");
	assert_ne!(next.generation, key.generation, "a replacement binding carries a generation of its own");
	assert!(attach_for(index, edu.bus, edu.dev, edu.func, next.generation), "the replacement attaches");
	sentinel.restore();
	edu.set_bus_master(true);
	let after = edu.transfer(iova.get(), 0x1000, true);
	assert!(sentinel.intact(), "the ended binding's translated address still reached its frame under the replacement - a forced release left a usable mapping behind");
	edu.set_bus_master(false);
	crate::serial_println!("iommu-fixture: forced-release case PASSED - a live translated address stopped reaching its frame when its claim was forced to end (transfer completed={after})");

	assert!(detach_for(index, edu.bus, edu.dev, edu.func), "the replacement's endpoint detaches");
	crate::device::release_claim(next).expect("the replacement releases");
	poll_faults();
}
