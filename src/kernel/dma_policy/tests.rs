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
	// enforcement, and it must say the same thing on the ordinary profile and on P02M0153's
	// enforcing one - a test whose premise is the machine it happens to run on is a test that
	// changes meaning when the machine does.
	let was = enforcing();
	set_enforcing(false);

	assert_eq!(admit(1, 0, 4, 0), BindDecision::DegradedUntranslated);
	assert_eq!(admit(1, 0, 4, 0), BindDecision::DegradedUntranslated, "the same device again is the same answer");
	assert_eq!(admit(2, 0, 5, 0), BindDecision::DegradedUntranslated);

	let degraded = degraded_devices();
	assert_eq!(degraded.len(), 2, "one row per device, not one per bind - an audit record that floods stops being read");
	assert!(degraded.contains(&Degraded { device_type: 1, bus: 0, dev: 4, func: 0 }));
	assert!(degraded.contains(&Degraded { device_type: 2, bus: 0, dev: 5, func: 0 }));
	forget_degraded_for_test();
	set_enforcing(was);
}

crate::tagged_test!(enforcement_changes_the_answer_and_stops_the_degraded_record, [Dma, Kernel], id = "kernel.dma_policy.enforcement_changes_the_answer_and_stops_the_degraded_record", covers = ["kernel"]);
fn enforcement_changes_the_answer_and_stops_the_degraded_record() {
	forget_degraded_for_test();
	let was = enforcing();
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
	set_enforcing(was);
}

crate::tagged_test!(a_device_type_that_requires_translation_is_refused_without_it, [Dma, Kernel], id = "kernel.dma_policy.a_device_type_that_requires_translation_is_refused_without_it", covers = ["kernel"]);
fn a_device_type_that_requires_translation_is_refused_without_it() {
	// THE MECHANISM, DRIVEN. `IOMMU_REQUIRED_TYPES` is empty in this tree and the comment above it
	// says why, so nothing else here would ever reach the refusing arm - and an arm nothing reaches
	// is an arm nobody has tested. This asks the decision directly, which is the same function
	// `admit` calls.
	let was = enforcing();
	set_enforcing(false);
	assert_eq!(dma::decide_bind(Policy::IommuRequired, false), BindDecision::Refused, "a device that declared it needs translation does not bind without it");
	// And it is a refusal rather than a quiet degrade: there is no fourth answer to fall back to.
	assert_ne!(dma::decide_bind(Policy::IommuRequired, false), BindDecision::DegradedUntranslated);
	set_enforcing(true);
	assert_eq!(dma::decide_bind(Policy::IommuRequired, true), BindDecision::Translated, "and it binds where translation exists");
	set_enforcing(was);
	// Every type this tree declares is under the untranslated policy, which is the state the boot
	// report names on every boot.
	assert_eq!(policy_for(1), Policy::TrustedUntranslated);
}
