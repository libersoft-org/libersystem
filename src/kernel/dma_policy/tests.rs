// The decision, driven through every combination of mode, machine and policy - and the audit record
// it leaves.
//
// EVERY TEST PINS THE MACHINE. The subject is what the policy DOES with the boot's mode, the bus and
// the bring-up, and that must be the same answer on the degraded test row, on the enforcing gate and
// on either device-tree port - a test whose premise is whichever machine it happens to run on is a
// test that changes meaning when the machine does. The three facts are set, asserted against, and
// put back.

use super::*;
use bootproto::dma_mode::{Handoff, Mode};

// The device the synthetic table rows are declared for - see `device::add_synthetic_device`.
fn synthetic_device(dev: u8) -> driver_binding::Discovered {
	driver_binding::Discovered { transport: abi::TRANSPORT_PLAIN_PCI, virtio_type: u16::MAX as u32, class: 0xff, subclass: 0xff, prog_if: 0xff, vendor: 0xffff, product: 0xffff, bus: 0xff, dev, func: 0 }
}

// What the machine was before a test stated its own, so it can be put back.
struct Pinned {
	handoff: Option<Handoff>,
	enforcing: bool,
}

fn pin() -> Pinned {
	Pinned { handoff: handoff(), enforcing: enforcing() }
}

fn restore(pinned: Pinned) {
	set_handoff_for_test(pinned.handoff);
	set_enforcing(pinned.enforcing);
	set_controller_present_for_test(None);
	forget_degraded_for_test();
}

// State the machine: the mode, whether a controller is on the bus, and whether it is translating.
fn machine(handoff: Option<Handoff>, controller: bool, enforcing: bool) {
	set_handoff_for_test(handoff);
	set_controller_present_for_test(Some(controller));
	set_enforcing(enforcing);
}

const TRUSTED: &[u8] = b"synthetic-trusted";
const NONE: &[u8] = b"synthetic-none";
const PROTECTED: &[u8] = b"synthetic-protected";

fn verdict(name: &[u8], device: &driver_binding::Discovered) -> Result<Admission, Refusal> {
	decide(&entry_field(name), device).map(|(admission, _)| admission)
}

crate::tagged_test!(every_mode_and_policy_combination_answers_as_the_matrix_says, [Dma, Kernel], id = "kernel.dma_policy.every_mode_and_policy_combination_answers_as_the_matrix_says", covers = ["kernel"]);
fn every_mode_and_policy_combination_answers_as_the_matrix_says() {
	let pinned = pin();
	let device = synthetic_device(1);

	// NO MODE: every policy, every machine, refused. The producer failed, and there is no row that
	// reads absence as a default - not the test row, not the development one.
	for (controller, enforcing) in [(false, false), (true, false), (true, true)] {
		machine(None, controller, enforcing);
		for name in [TRUSTED, NONE, PROTECTED] {
			assert_eq!(verdict(name, &device), Err(Refusal::NoMode), "no mode refuses {} on a machine with controller={controller} enforcing={enforcing}", core::str::from_utf8(name).unwrap());
		}
	}

	// ENFORCING-REQUIRED WITH A TRANSLATING CONTROLLER: both DMA-capable policies are translated,
	// and `none` masters nothing.
	machine(Some(Handoff::Signed(Mode::EnforcingRequired)), true, true);
	assert_eq!(verdict(PROTECTED, &device), Ok(Admission::Translated));
	assert_eq!(verdict(TRUSTED, &device), Ok(Admission::Translated), "trust is permission to run without translation, not a preference for it");
	assert_eq!(verdict(NONE, &device), Ok(Admission::NonMastering));

	// ENFORCING-REQUIRED WITHOUT ENFORCEMENT - the controller did not come up, or there is none:
	// both DMA-capable policies REFUSE. There is no degraded fallback on this mode, because the
	// mode is the statement that this machine translates, and a machine that does not is not the
	// machine the boot was built for.
	for controller in [true, false] {
		machine(Some(Handoff::Harness(Mode::EnforcingRequired)), controller, false);
		assert_eq!(verdict(PROTECTED, &device), Err(Refusal::EnforcementAbsent), "controller={controller}");
		assert_eq!(verdict(TRUSTED, &device), Err(Refusal::EnforcementAbsent), "a trusted driver is not admitted untranslated on an enforcing mode (controller={controller})");
		assert_eq!(verdict(NONE, &device), Ok(Admission::NonMastering), "a driver that never masters the bus needs no translation to be admitted");
	}

	// NO-IOMMU, THE EXPLICIT DEGRADED PROFILE: `iommu-required` refuses - that is the field
	// working - `trusted-untranslated` enters the audited degraded inventory, `none` masters
	// nothing.
	machine(Some(Handoff::Harness(Mode::NoIommu)), false, false);
	assert_eq!(verdict(PROTECTED, &device), Err(Refusal::PolicyRefused), "the one row that declares it needs translation does not run without it");
	assert_eq!(verdict(TRUSTED, &device), Ok(Admission::DegradedUntranslated));
	assert_eq!(verdict(NONE, &device), Ok(Admission::NonMastering));
	// The signed value and the harness value are the same mode; provenance changes nothing here.
	machine(Some(Handoff::Signed(Mode::NoIommu)), false, false);
	assert_eq!(verdict(TRUSTED, &device), Ok(Admission::DegradedUntranslated));

	// NO-IOMMU WITH A CONTROLLER ON THE BUS: a controller/mode mismatch. Refused rather than
	// reclassified as an enforcing boot nobody signed for - whether or not the controller came up.
	for enforcing in [false, true] {
		machine(Some(Handoff::Signed(Mode::NoIommu)), true, enforcing);
		for name in [TRUSTED, NONE, PROTECTED] {
			assert_eq!(verdict(name, &device), Err(Refusal::ControllerOnDegradedMode), "{} on a no-iommu boot with a controller (enforcing={enforcing})", core::str::from_utf8(name).unwrap());
		}
	}
	restore(pinned);
}

crate::tagged_test!(an_unknown_or_mismatched_entry_is_refused_by_name_whatever_the_mode, [Dma, Kernel], id = "kernel.dma_policy.an_unknown_or_mismatched_entry_is_refused_by_name_whatever_the_mode", covers = ["kernel"]);
fn an_unknown_or_mismatched_entry_is_refused_by_name_whatever_the_mode() {
	let pinned = pin();
	let device = synthetic_device(2);
	machine(Some(Handoff::Signed(Mode::EnforcingRequired)), true, true);
	// A name the image does not declare, in any of the ways a name can fail to be one.
	assert_eq!(verdict(b"not-a-driver", &device), Err(Refusal::UnknownEntry));
	assert_eq!(decide(&[0u8; abi::ENTRY_NAME_LEN], &device).map(|(a, _)| a), Err(Refusal::UnknownEntry), "an empty field names nothing");
	assert_eq!(verdict(b"Synthetic-Trusted", &device), Err(Refusal::UnknownEntry), "a name is canonical or it is not a name - there is no case folding");
	let mut trailing = entry_field(TRUSTED);
	trailing[abi::ENTRY_NAME_LEN - 1] = b'x';
	assert_eq!(decide(&trailing, &device).map(|(a, _)| a), Err(Refusal::UnknownEntry), "bytes after the terminator are not a field the ABI defines");
	// AN ENTRY THAT EXISTS AND IS NOT DECLARED FOR THIS DEVICE. `virtio_blk` is a real row; its rule
	// says virtio-pci type 2, and the synthetic device is neither. The kernel refuses the selection
	// by name rather than trusting that the manager matched.
	assert_eq!(verdict(b"virtio_blk", &device), Err(Refusal::EntryDoesNotMatchDevice));
	let blk = driver_binding::Discovered { transport: abi::TRANSPORT_VIRTIO_PCI, virtio_type: abi::VIRTIO_TYPE_BLOCK, class: 1, subclass: 0, prog_if: 0, vendor: 0x1af4, product: 0x1042, bus: 0, dev: 3, func: 0 };
	assert_eq!(verdict(b"virtio_blk", &blk), Ok(Admission::Translated), "and the same entry admits the device it is declared for");
	assert_eq!(verdict(TRUSTED, &blk), Err(Refusal::EntryDoesNotMatchDevice), "while the synthetic entry is not declared for a real disk");
	// THE IDENTITY CHECK COMES FIRST. A mode that would refuse everything still answers "unknown
	// entry" for an unknown entry, so the manager learns which of its inputs was wrong.
	machine(None, false, false);
	assert_eq!(verdict(b"not-a-driver", &device), Err(Refusal::UnknownEntry));
	assert_eq!(verdict(b"virtio_blk", &device), Err(Refusal::EntryDoesNotMatchDevice));
	restore(pinned);
}

crate::tagged_test!(a_selected_entry_is_admitted_because_it_is_declared_for_the_device_and_not_because_it_ranks_first, [Dma, Kernel], id = "kernel.dma_policy.a_selected_entry_is_admitted_because_it_is_declared_for_the_device_and_not_because_it_ranks_first", covers = ["kernel"]);
fn a_selected_entry_is_admitted_because_it_is_declared_for_the_device_and_not_because_it_ranks_first() {
	// THE AUTHORITY BOUNDARY. Two entries are declared for the synthetic device, with different
	// policies. Whichever the manager names is admitted - with ITS OWN policy - and the kernel does
	// not substitute the one it would have ranked first. An operator's stored `select=` reaches the
	// kernel as exactly this: a name that is a candidate, honoured, never overruled.
	let pinned = pin();
	let device = synthetic_device(3);
	machine(Some(Handoff::Signed(Mode::NoIommu)), false, false);
	let trusted = decide(&entry_field(TRUSTED), &device).expect("declared for the device");
	let none = decide(&entry_field(NONE), &device).expect("also declared for the device");
	assert_eq!(trusted, (Admission::DegradedUntranslated, abi::DMA_POLICY_TRUSTED_UNTRANSLATED as u8), "each attempted entry reaches admission with its own policy");
	assert_eq!(none, (Admission::NonMastering, abi::DMA_POLICY_NONE as u8));
	// And an entry that is NOT declared for the device is refused, whatever the operator stored.
	assert_eq!(verdict(b"virtio_net", &device), Err(Refusal::EntryDoesNotMatchDevice));
	restore(pinned);
}

crate::tagged_test!(the_kernel_registry_is_the_manifest_migration_table, [Dma, Kernel], id = "kernel.dma_policy.the_kernel_registry_is_the_manifest_migration_table", covers = ["kernel", "manifest"]);
fn the_kernel_registry_is_the_manifest_migration_table() {
	// The generated table carries every driver the manifest stages, with the migration table's
	// values: the network driver requires translation and every other row is the explicit trusted
	// exception. A row that arrived unclassified cannot exist - the manifest refuses it - and this
	// is the kernel side of the same fact.
	let names = registry_names();
	assert!(names.contains(&&b"virtio_net"[..]), "the network driver is in the table");
	assert!(names.contains(&&b"virtio_blk"[..]));
	assert!(names.contains(&&b"xhci"[..]));
	assert_eq!(registry_policy(b"virtio_net"), Some(abi::DMA_POLICY_IOMMU_REQUIRED as u8));
	for name in names {
		let expected = if name == b"virtio_net" { abi::DMA_POLICY_IOMMU_REQUIRED } else { abi::DMA_POLICY_TRUSTED_UNTRANSLATED };
		assert_eq!(registry_policy(name), Some(expected as u8), "{} carries the migration table's value", core::str::from_utf8(name).unwrap());
	}
	// The synthetic entries are NOT in the manifest's table.
	assert!(!registry_names().contains(&TRUSTED));
}

crate::tagged_test!(binding_without_translation_is_recorded_by_name_and_only_once, [Dma, Kernel], id = "kernel.dma_policy.binding_without_translation_is_recorded_by_name_and_only_once", covers = ["kernel"]);
fn binding_without_translation_is_recorded_by_name_and_only_once() {
	let pinned = pin();
	forget_degraded_for_test();
	machine(Some(Handoff::Harness(Mode::NoIommu)), false, false);
	let first = synthetic_device(4);
	let second = synthetic_device(5);
	assert_eq!(admit(&entry_field(TRUSTED), &first, 1).map(|(a, _)| a), Ok(Admission::DegradedUntranslated));
	assert_eq!(admit(&entry_field(TRUSTED), &first, 1).map(|(a, _)| a), Ok(Admission::DegradedUntranslated), "the same device again is the same answer");
	assert_eq!(admit(&entry_field(TRUSTED), &second, 2).map(|(a, _)| a), Ok(Admission::DegradedUntranslated));
	// AND A NON-MASTERING ADMISSION IS NOT A DEGRADED ONE: nothing reaches memory, so nothing is
	// listed.
	assert_eq!(admit(&entry_field(NONE), &synthetic_device(6), 3).map(|(a, _)| a), Ok(Admission::NonMastering));
	let degraded = degraded_devices();
	assert_eq!(degraded.len(), 2, "one row per device, not one per bind - an audit record that floods stops being read");
	assert!(degraded.contains(&Degraded { device_type: 1, bus: 0xff, dev: 4, func: 0 }));
	assert!(degraded.contains(&Degraded { device_type: 2, bus: 0xff, dev: 5, func: 0 }));
	restore(pinned);
}

crate::tagged_test!(enforcement_changes_the_answer_and_stops_the_degraded_record, [Dma, Kernel], id = "kernel.dma_policy.enforcement_changes_the_answer_and_stops_the_degraded_record", covers = ["kernel"]);
fn enforcement_changes_the_answer_and_stops_the_degraded_record() {
	let pinned = pin();
	forget_degraded_for_test();
	let device = synthetic_device(7);
	// TRUST IS PERMISSION TO RUN WITHOUT TRANSLATION, NOT A PREFERENCE FOR IT. A trusted driver on
	// an enforcing machine is translated like everything else, and nothing goes in the degraded
	// list - which is what makes that list a description of the system rather than of the policy.
	machine(Some(Handoff::Signed(Mode::EnforcingRequired)), true, true);
	assert_eq!(admit(&entry_field(TRUSTED), &device, 1).map(|(a, _)| a), Ok(Admission::Translated));
	assert!(degraded_devices().is_empty(), "nothing is degraded while translation is on");
	machine(Some(Handoff::Signed(Mode::NoIommu)), false, false);
	assert_eq!(admit(&entry_field(TRUSTED), &device, 1).map(|(a, _)| a), Ok(Admission::DegradedUntranslated));
	assert_eq!(degraded_devices().len(), 1);
	// The boot report reads the same state. Called here because a test build has no `boot_main` to
	// call it, and a reporting path nothing exercises is a reporting path that stops working.
	report();
	restore(pinned);
}

crate::tagged_test!(a_device_that_gave_the_bus_back_leaves_the_degraded_list, [Dma, Kernel], id = "kernel.dma_policy.a_device_that_gave_the_bus_back_leaves_the_degraded_list", covers = ["kernel"]);
fn a_device_that_gave_the_bus_back_leaves_the_degraded_list() {
	let pinned = pin();
	forget_degraded_for_test();
	machine(Some(Handoff::Harness(Mode::NoIommu)), false, false);
	// A driver asks for the bus, and is recorded. This is the moment `admit` writes the row.
	assert_eq!(admit(&entry_field(TRUSTED), &synthetic_device(11), 0x1050).map(|(a, _)| a), Ok(Admission::DegradedUntranslated));
	assert_eq!(admit(&entry_field(TRUSTED), &synthetic_device(2), 0x1041).map(|(a, _)| a), Ok(Admission::DegradedUntranslated));
	assert_eq!(degraded_devices().len(), 2);
	// AND THEN FAILS TO BIND AND RELEASES IT. The row used to stay forever, so a boot whose display
	// driver did not come up printed "11 device(s) master the bus untranslated" beside
	// "10 of 11 device(s) online" - two adjacent lines describing different machines.
	forget_degraded(0xff, 11, 0);
	let degraded = degraded_devices();
	assert_eq!(degraded.len(), 1, "the audit record lasts exactly as long as the ownership it describes");
	assert!(degraded.contains(&Degraded { device_type: 0x1041, bus: 0xff, dev: 2, func: 0 }), "and the device still holding the bus is still in it");
	// Forgetting a device that is not in the list is not an error - a driver may be released without
	// ever having been admitted.
	forget_degraded(0xff, 11, 0);
	assert_eq!(degraded_devices().len(), 1);
	restore(pinned);
}

crate::tagged_test!(a_published_isolation_claim_retracts_itself_when_a_later_device_falsifies_it, [Dma, Kernel], id = "kernel.dma_policy.a_published_isolation_claim_retracts_itself_when_a_later_device_falsifies_it", covers = ["kernel"]);
fn a_published_isolation_claim_retracts_itself_when_a_later_device_falsifies_it() {
	// THE SUMMARY IS TAKEN AT A MOMENT SOMEBODY ELSE CHOOSES. `report` is called when the kernel's
	// supervisor decides the system is up, and on a machine whose console never attaches that
	// decision is made on a deadline rather than on a device count - so a driver can still be on its
	// way to the bus when the strongest sentence this kernel prints about isolation goes out.
	let pinned = pin();
	forget_degraded_for_test();
	let device = synthetic_device(3);
	// BEFORE ANYTHING IS PUBLISHED THERE IS NOTHING TO RETRACT.
	machine(Some(Handoff::Harness(Mode::NoIommu)), false, false);
	assert_eq!(admit(&entry_field(TRUSTED), &device, 0x1041).map(|(a, _)| a), Ok(Admission::DegradedUntranslated));
	assert_eq!(retractions_for_test(), 0, "an admission before the summary is what the summary is FOR");
	// NOW THE CLAIM IS PUBLISHED, in the shape that matters: translating, nothing degraded, which is
	// the line that says "dma: every bus-mastering device is translated".
	forget_degraded_for_test();
	machine(Some(Handoff::Signed(Mode::EnforcingRequired)), true, true);
	assert!(degraded_devices().is_empty());
	report();
	// AND A DEVICE BINDS AFTERWARDS WITHOUT TRANSLATION. Only a `no-iommu` boot admits one; the
	// machine is restated as that boot, which is the shape of a claim taken before the mode's
	// consequences had all landed.
	machine(Some(Handoff::Harness(Mode::NoIommu)), false, false);
	assert_eq!(admit(&entry_field(TRUSTED), &device, 0x1041).map(|(a, _)| a), Ok(Admission::DegradedUntranslated));
	assert_eq!(retractions_for_test(), 1, "a claim that stopped being true says so at the moment it stops");
	// ONCE PER DEVICE, LIKE THE RECORD ITSELF.
	assert_eq!(admit(&entry_field(TRUSTED), &device, 0x1041).map(|(a, _)| a), Ok(Admission::DegradedUntranslated));
	assert_eq!(retractions_for_test(), 1, "the same device again is the same fact");
	// A DIFFERENT DEVICE IS A DIFFERENT FACT.
	assert_eq!(admit(&entry_field(TRUSTED), &synthetic_device(4), 0x1050).map(|(a, _)| a), Ok(Admission::DegradedUntranslated));
	assert_eq!(retractions_for_test(), 2);
	restore(pinned);
}
