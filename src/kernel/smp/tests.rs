use super::{cpu_count, online_count};

crate::tagged_test!(smp_all_cores_online, [Smp, Kernel, Smoke], id = "kernel.smp.smp_all_cores_online", covers = ["kernel"]);
fn smp_all_cores_online() {
	// init_smp ran before the tests and waited for every core to report in, so
	// the online count must equal the managed core count (and exceed one when
	// QEMU is given more than a single CPU).
	assert_eq!(online_count(), cpu_count());
}

crate::tagged_test!(a_firmware_pointer_outside_the_direct_map_is_refused_before_it_is_dereferenced, [Smp, Kernel, Memory], id = "kernel.smp.a_firmware_pointer_outside_the_direct_map_is_refused_before_it_is_dereferenced", covers = ["kernel"]);
fn a_firmware_pointer_outside_the_direct_map_is_refused_before_it_is_dereferenced() {
	// Every ACPI address arrives from firmware and used to be dereferenced on the strength of a
	// signature match: `find_table` evaluated `table_signature` before `table_ok` - `&&` is left to
	// right - so the read the checksum was meant to gate happened first. Off the end of the HHDM
	// that is a wild read in early boot, before there is a fault handler worth the name.
	//
	// Asserted on the BOUND rather than by handing the walker a bad pointer, because the failure
	// this closes is a triple fault: a test that reproduces it does not report anything.
	use crate::mem;
	let hhdm = mem::hhdm_offset();
	assert!(mem::within_direct_map(0x1000, 36), "an ordinary low physical address is inside the map");
	assert!(!mem::within_direct_map(0, 36), "a null firmware pointer is not a table");
	assert!(!mem::within_direct_map(u64::MAX - 8, 36), "an address whose end overflows is refused rather than wrapped");
	assert!(!mem::within_direct_map(0x1_0000_0000_0000, 36), "an address far past any machine's RAM is outside the map");
	// And the readers refuse it rather than dereferencing it.
	assert_eq!(super::table_signature(hhdm, 0x1_0000_0000_0000), None, "the signature read is bounded");
	assert_eq!(super::table_length(hhdm, 0x1_0000_0000_0000), None, "so is the length read");
	assert!(!super::table_ok(hhdm, 0x1_0000_0000_0000), "and a table nothing can read does not pass its checksum");

	// A table whose DECLARED length runs off the end of the map is refused too - the ceiling bounds
	// how far a bad length walks, not whether the walk stays somewhere readable. Built at the very
	// top of the map so its header is inside and its body is not.
	let limit = {
		let mut top = 0u64;
		for index in 0..mem::memmap_len() {
			if let Some(region) = mem::memmap_get(index) {
				top = top.max(region.base + region.length);
			}
		}
		top.next_multiple_of(2 * 1024 * 1024)
	};
	assert!(limit > 0, "the boot memory map was retained, so the direct map has a known extent");
	assert!(mem::within_direct_map(limit - 4096, 36), "the last page of the map is inside it");
	assert!(!mem::within_direct_map(limit - 4096, 8192), "a table that starts inside and ends outside is not");
}
