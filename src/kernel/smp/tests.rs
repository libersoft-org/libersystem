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
	assert!(mem::within_direct_map(0x1000, 36), "an ordinary low physical address is inside the map");
	assert!(!mem::within_direct_map(0, 36), "a null firmware pointer is not a table");
	assert!(!mem::within_direct_map(u64::MAX - 8, 36), "an address whose end overflows is refused rather than wrapped");
	assert!(!mem::within_direct_map(0x1_0000_0000_0000, 36), "an address far past any machine's RAM is outside the map");
	// And the readers refuse it rather than dereferencing it. THE READERS ARE THE ACPI WALK'S, so
	// they exist only where ACPI does: the device-tree ports reach their firmware description
	// through `fdt`, which is a host-tested parser rather than a pointer walked in early boot. The
	// bound above is portable and is asserted on all three.
	#[cfg(target_arch = "x86_64")]
	{
		let hhdm = mem::hhdm_offset();
		assert_eq!(super::table_signature(hhdm, 0x1_0000_0000_0000), None, "the signature read is bounded");
		assert_eq!(super::table_length(hhdm, 0x1_0000_0000_0000), None, "so is the length read");
		assert!(!super::table_ok(hhdm, 0x1_0000_0000_0000), "and a table nothing can read does not pass its checksum");
	}

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

crate::tagged_test!(the_global_clock_advances_once_per_period_however_many_cores_tick, [Smp, Kernel], id = "kernel.smp.the_global_clock_advances_once_per_period_however_many_cores_tick", covers = ["kernel"]);
fn the_global_clock_advances_once_per_period_however_many_cores_tick() {
	// KERN-ARCH-007. The scheduler tick is a PER-CORE timer on all three architectures, and aarch64
	// and riscv64 answered each core's interrupt with `TICKS.fetch_add(1)` - so the monotonic clock
	// advanced once per core per period. On the machine this suite runs on that is eight cores, so
	// time ran eight times fast and every deadline in the system - a sleep, a timeout, the caret
	// blink - was wrong by that factor.
	//
	// Asserted on the MECHANISM rather than by timing a window, because a rate measured against a
	// wall clock on an emulated guest under load is a flaky test and this is a fact about
	// arithmetic: the same instant, seen by any number of cores, is one tick.
	use crate::arch::common::time::{TICK_HZ, TickClock};

	// A thousand cycles per tick, so the numbers below are readable.
	let hz = 1_000 * TICK_HZ as u64;
	let clock = TickClock::new();

	// Eight cores take their first interrupt at the same instant. The first one anchors the clock
	// and counts; the other seven find nothing due.
	for _ in 0..8 {
		clock.advance(1_000_000, hz);
	}
	assert_eq!(clock.ticks(), 1, "the instant that starts the clock is one tick, not eight");

	// One full period later, all eight again.
	for _ in 0..8 {
		clock.advance(1_001_000, hz);
	}
	assert_eq!(clock.ticks(), 2, "one period is one tick, whatever the core count");

	// A stall: ten periods have passed by the time anyone looks. The backlog is claimed ONCE, in
	// one step - replaying it a tick per interrupt would run time fast in bursts, at the interrupt
	// rate times the core count, and fire every pending deadline in a rush.
	for _ in 0..8 {
		clock.advance(1_011_000, hz);
	}
	assert_eq!(clock.ticks(), 12, "a ten-period backlog is ten ticks, claimed by one core");

	// Less than a period is not a tick, however many cores ask.
	for _ in 0..8 {
		clock.advance(1_011_999, hz);
	}
	assert_eq!(clock.ticks(), 12, "part of a period is not a tick");

	// AND AN UNCALIBRATED COUNTER COUNTS NOTHING, rather than guessing a period: a frequency of
	// zero is a machine whose cycle clock is not up yet, and inventing a rate there is how a clock
	// ends up wrong in a way nobody can see.
	let cold = TickClock::new();
	cold.advance(1_000_000, 0);
	assert_eq!(cold.ticks(), 0, "no frequency, no ticks");
}

crate::tagged_test!(
	#[cfg(target_arch = "x86_64")]
	the_published_core_count_is_the_one_that_came_online,
	[Smp, Kernel],
	id = "kernel.smp.the_published_core_count_is_the_one_that_came_online",
	covers = ["kernel"]
);
#[cfg(target_arch = "x86_64")]
fn the_published_core_count_is_the_one_that_came_online() {
	// KERN-ARCH-009 and -010. The firmware's core count was published before anything had been
	// started, and narrowed to the confirmed count only INSIDE the branch that starts application
	// processors. Every way of skipping that branch - the loader reserving no trampoline page, or a
	// page-table root the trampoline's 32-bit CR3 load cannot express - therefore left the machine
	// claiming cores that were never woken, which the scheduler dispatches to and the shootdown
	// waits on.
	//
	// The three conditions are asked about directly here; the narrowing itself is now unconditional
	// (one `store` after the branch, on the online counter that only an AP report-in raises), which
	// is what the invariant below measures on the machine actually running.
	assert_eq!(super::ap_boot_refusal(1, 0x8000, 0x1000), Some("the firmware reports a single core"));
	assert!(super::ap_boot_refusal(4, 0, 0x1000).is_some(), "no trampoline page means no AP can be started");
	assert!(super::ap_boot_refusal(4, 0x8000, 0x1_0000_0000).is_some(), "a root above 4 GB does not survive a 32-bit CR3 load");
	assert_eq!(super::ap_boot_refusal(4, 0x8000, 0xFFFF_F000), None, "a root at the very top of the low 4 GB is still loadable");
	assert_eq!(super::ap_boot_refusal(4, 0x8000, 0x1000), None, "and an ordinary multi-core machine boots its APs");
	assert!(!crate::arch::apboot::cr3_is_reachable(u64::MAX), "the bound is on the whole 64-bit value, not its low half");
	assert_eq!(cpu_count(), online_count(), "the count the rest of the kernel reads is the confirmed one");
}
