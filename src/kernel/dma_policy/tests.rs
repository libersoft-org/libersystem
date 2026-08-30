// The decision, and the audit record it leaves.

use super::*;

crate::tagged_test!(an_untrusted_dma_driver_does_not_bind_without_enforcement, [Dma, Kernel], id = "kernel.dma_policy.an_untrusted_dma_driver_does_not_bind_without_enforcement", covers = ["kernel"]);
fn an_untrusted_dma_driver_does_not_bind_without_enforcement() {
	// The decision is the contract's, and this is the kernel side of it: the three answers, and no
	// fallback from a refusal to untranslated DMA. "It must never silently become untranslated DMA"
	// is the Goal's sentence, and a refusal that quietly degraded would be exactly that.
	assert_eq!(dma::decide_bind(dma::Policy::IommuRequired, false), BindDecision::Refused);
	assert_eq!(dma::decide_bind(dma::Policy::IommuRequired, true), BindDecision::Translated);
	assert_eq!(dma::decide_bind(dma::Policy::TrustedUntranslated, false), BindDecision::DegradedUntranslated);
	assert_eq!(dma::decide_bind(dma::Policy::TrustedUntranslated, true), BindDecision::Translated);
}

crate::tagged_test!(binding_without_translation_is_recorded_by_name_and_only_once, [Dma, Kernel], id = "kernel.dma_policy.binding_without_translation_is_recorded_by_name_and_only_once", covers = ["kernel"]);
fn binding_without_translation_is_recorded_by_name_and_only_once() {
	forget_degraded_for_test();
	// THE STATE IS SET RATHER THAN ASSUMED. This test's subject is what happens when there is no
	// enforcement, and it must say the same thing on the ordinary profile and on the enforcing one -
	// a test whose premise is the machine it happens to run on is a test that changes meaning when the
	// machine does. BOTH halves of that state are pinned: whether an IOMMU is translating, and whether
	// this machine has one at all - the second decides whether a protected type is refused outright,
	// which would never reach the degraded record this test is about.
	let was = enforcing();
	let was_expected = isolation_expected();
	set_enforcing(false);
	set_isolation_expected(false);

	assert_eq!(admit(1, 0, 4, 0), BindDecision::DegradedUntranslated);
	assert_eq!(admit(1, 0, 4, 0), BindDecision::DegradedUntranslated, "the same device again is the same answer");
	assert_eq!(admit(2, 0, 5, 0), BindDecision::DegradedUntranslated);

	let degraded = degraded_devices();
	assert_eq!(degraded.len(), 2, "one row per device, not one per bind - an audit record that floods stops being read");
	assert!(degraded.contains(&Degraded { device_type: 1, bus: 0, dev: 4, func: 0 }));
	assert!(degraded.contains(&Degraded { device_type: 2, bus: 0, dev: 5, func: 0 }));
	forget_degraded_for_test();
	set_isolation_expected(was_expected);
	set_enforcing(was);
}

crate::tagged_test!(enforcement_changes_the_answer_and_stops_the_degraded_record, [Dma, Kernel], id = "kernel.dma_policy.enforcement_changes_the_answer_and_stops_the_degraded_record", covers = ["kernel"]);
fn enforcement_changes_the_answer_and_stops_the_degraded_record() {
	forget_degraded_for_test();
	let was = enforcing();
	// The same pinning as above: this test's subject is a TRUSTED type's two answers, so the machine
	// must not be one where type 1 is a protected type refused outright.
	let was_expected = isolation_expected();
	set_isolation_expected(false);
	// TRUST IS PERMISSION TO RUN WITHOUT TRANSLATION, NOT A PREFERENCE FOR IT. A trusted driver on a
	// system that HAS an IOMMU is translated like everything else, and nothing goes in the degraded
	// list - which is what makes that list a description of the system rather than of the policy.
	set_enforcing(true);
	assert_eq!(admit(1, 0, 4, 0), BindDecision::Translated);
	assert!(degraded_devices().is_empty(), "nothing is degraded while translation is on");
	set_enforcing(false);
	assert_eq!(admit(1, 0, 4, 0), BindDecision::DegradedUntranslated);
	assert_eq!(degraded_devices().len(), 1);
	// The boot report reads the same state. Called here because a test build has no `boot_main` to
	// call it, and a reporting path nothing exercises is a reporting path that stops working.
	report();
	forget_degraded_for_test();
	set_isolation_expected(was_expected);
	set_enforcing(was);
}

crate::tagged_test!(a_device_type_that_requires_translation_is_refused_without_it, [Dma, Kernel], id = "kernel.dma_policy.a_device_type_that_requires_translation_is_refused_without_it", covers = ["kernel"]);
fn a_device_type_that_requires_translation_is_refused_without_it() {
	// THE MECHANISM, DRIVEN, INDEPENDENTLY OF WHICH MACHINE THIS IS. `policy_for` answers from the
	// machine - a controller on the bus holds the protected types to translation, a machine without
	// one does not - so the decision function is asked directly here, which is the same one `admit`
	// calls once the policy is known.
	let was = enforcing();
	set_enforcing(false);
	assert_eq!(dma::decide_bind(Policy::IommuRequired, false), BindDecision::Refused, "a device that declared it needs translation does not bind without it");
	// And it is a refusal rather than a quiet degrade: there is no fourth answer to fall back to.
	assert_ne!(dma::decide_bind(Policy::IommuRequired, false), BindDecision::DegradedUntranslated);
	set_enforcing(true);
	assert_eq!(dma::decide_bind(Policy::IommuRequired, true), BindDecision::Translated, "and it binds where translation exists");
	set_enforcing(was);
	// AND THE POLICY ITSELF DEPENDS ON THE MACHINE, which is the half a fixed assertion used to get
	// wrong: this asserted every type was untranslated, which was true only while the protected list
	// was empty, and stopped being true on a machine that carries a controller.
	let was_expected = isolation_expected();
	set_isolation_expected(false);
	assert_eq!(policy_for(abi::VIRTIO_TYPE_NET as u16), Policy::TrustedUntranslated, "a machine with no controller does not lose a driver over isolation it never had");
	set_isolation_expected(true);
	assert_eq!(policy_for(abi::VIRTIO_TYPE_NET as u16), Policy::IommuRequired, "and a machine that has one holds its protected types to it");
	assert_eq!(policy_for(abi::VIRTIO_TYPE_BLOCK as u16), Policy::TrustedUntranslated, "while a type that never declared it is unaffected either way");
	set_isolation_expected(was_expected);
}

crate::tagged_test!(a_device_that_gave_the_bus_back_leaves_the_degraded_list, [Dma, Kernel], id = "kernel.dma_policy.a_device_that_gave_the_bus_back_leaves_the_degraded_list", covers = ["kernel"]);
fn a_device_that_gave_the_bus_back_leaves_the_degraded_list() {
	forget_degraded_for_test();
	let was = enforcing();
	set_enforcing(false);

	// A driver asks for the bus, and is recorded. This is the moment `admit` writes the row.
	assert_eq!(admit(0x1050, 0, 11, 0), BindDecision::DegradedUntranslated);
	assert_eq!(admit(0x1041, 0, 2, 0), BindDecision::DegradedUntranslated);
	assert_eq!(degraded_devices().len(), 2);

	// AND THEN FAILS TO BIND AND RELEASES IT. The row used to stay forever, so a boot whose display
	// driver did not come up printed "11 device(s) master the bus untranslated" beside
	// "10 of 11 device(s) online" - two adjacent lines describing different machines.
	forget_degraded(0, 11, 0);
	let degraded = degraded_devices();
	assert_eq!(degraded.len(), 1, "the audit record lasts exactly as long as the ownership it describes");
	assert!(degraded.contains(&Degraded { device_type: 0x1041, bus: 0, dev: 2, func: 0 }), "and the device still holding the bus is still in it");
	// Forgetting a device that is not in the list is not an error - a driver may be released without
	// ever having been admitted.
	forget_degraded(0, 11, 0);
	assert_eq!(degraded_devices().len(), 1);

	forget_degraded_for_test();
	set_enforcing(was);
}

crate::tagged_test!(a_protected_driver_refuses_a_machine_whose_iommu_did_not_come_up, [Dma, Kernel], id = "kernel.dma_policy.a_protected_driver_refuses_a_machine_whose_iommu_did_not_come_up", covers = ["kernel"]);
fn a_protected_driver_refuses_a_machine_whose_iommu_did_not_come_up() {
	forget_degraded_for_test();
	let was_enforcing = enforcing();
	let was_expected = isolation_expected();
	const PROTECTED: u16 = abi::VIRTIO_TYPE_NET as u16;
	const ORDINARY: u16 = abi::VIRTIO_TYPE_BLOCK as u16;

	// A MACHINE WITH NO CONTROLLER ON ITS BUS. Untranslated DMA is the only DMA there is, so a
	// protected driver runs exactly as it did before - this is the ordinary harness, every
	// developer's run, and every machine whose firmware offers no IOMMU.
	set_isolation_expected(false);
	set_enforcing(false);
	assert_eq!(policy_for(PROTECTED), Policy::TrustedUntranslated);
	assert_eq!(admit(PROTECTED, 0, 2, 0), BindDecision::DegradedUntranslated, "a machine that never had isolation does not lose networking over it");

	// A MACHINE THAT HAS ONE AND DID NOT BRING IT UP. Isolation was available and something went
	// wrong with it, which is the one case a driver that declared it needs translation must refuse.
	forget_degraded_for_test();
	set_isolation_expected(true);
	assert_eq!(policy_for(PROTECTED), Policy::IommuRequired);
	assert_eq!(admit(PROTECTED, 0, 2, 0), BindDecision::Refused, "the controller is there and is not translating - this driver does not run");
	assert!(degraded_devices().is_empty(), "a refusal is not a degraded binding: nothing bound");
	// And a type that never claimed to need translation is unaffected either way.
	assert_eq!(policy_for(ORDINARY), Policy::TrustedUntranslated);
	assert_eq!(admit(ORDINARY, 0, 1, 0), BindDecision::DegradedUntranslated);

	// AND THE SAME MACHINE ONCE THE CONTROLLER IS TRANSLATING.
	forget_degraded_for_test();
	set_enforcing(true);
	assert_eq!(admit(PROTECTED, 0, 2, 0), BindDecision::Translated);
	assert!(degraded_devices().is_empty());

	forget_degraded_for_test();
	set_isolation_expected(was_expected);
	set_enforcing(was_enforcing);
}

crate::tagged_test!(a_published_isolation_claim_retracts_itself_when_a_later_device_falsifies_it, [Dma, Kernel], id = "kernel.dma_policy.a_published_isolation_claim_retracts_itself_when_a_later_device_falsifies_it", covers = ["kernel"]);
fn a_published_isolation_claim_retracts_itself_when_a_later_device_falsifies_it() {
	// THE SUMMARY IS TAKEN AT A MOMENT SOMEBODY ELSE CHOOSES. `report` is called when the kernel's
	// supervisor decides the system is up, and on a machine whose console never attaches that
	// decision is made on a deadline rather than on a device count - so a driver can still be on its
	// way to the bus when the strongest sentence this kernel prints about isolation goes out.
	//
	// Nothing else in the log would contradict it: admissions are recorded, not printed. So the
	// question this test asks is what happens to a claim that was true when it was made and is not
	// true a moment later.
	forget_degraded_for_test();
	let was = enforcing();
	let was_expected = isolation_expected();
	set_isolation_expected(false);

	// BEFORE ANYTHING IS PUBLISHED THERE IS NOTHING TO RETRACT, and a machine that binds every one of
	// its devices untranslated before the summary must not print a word of this - the summary itself
	// is where those devices are named.
	set_enforcing(false);
	assert_eq!(admit(0x1041, 0, 3, 0), BindDecision::DegradedUntranslated);
	assert_eq!(retractions_for_test(), 0, "an admission before the summary is what the summary is FOR");

	// NOW THE CLAIM IS PUBLISHED, in the shape that matters: translating, nothing degraded, which is
	// the line that says "dma: every bus-mastering device is translated".
	forget_degraded_for_test();
	set_enforcing(true);
	assert!(degraded_devices().is_empty());
	report();

	// AND A DEVICE BINDS AFTERWARDS WITHOUT TRANSLATION. This is the boot the summary cannot describe:
	// it was taken before this driver reached the bus.
	set_enforcing(false);
	assert_eq!(admit(0x1041, 0, 3, 0), BindDecision::DegradedUntranslated);
	assert_eq!(retractions_for_test(), 1, "a claim that stopped being true says so at the moment it stops");

	// ONCE PER DEVICE, LIKE THE RECORD ITSELF. A driver that opens its device repeatedly must not
	// turn one fact into a flood - the deduplication that keeps the audit list readable keeps this
	// line readable too.
	assert_eq!(admit(0x1041, 0, 3, 0), BindDecision::DegradedUntranslated);
	assert_eq!(retractions_for_test(), 1, "the same device again is the same fact");

	// A DIFFERENT DEVICE IS A DIFFERENT FACT.
	assert_eq!(admit(0x1050, 0, 4, 0), BindDecision::DegradedUntranslated);
	assert_eq!(retractions_for_test(), 2);

	forget_degraded_for_test();
	set_isolation_expected(was_expected);
	set_enforcing(was);
}
