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
	// how far a bad length walks, not whether the walk stays somewhere readable.
	//
	// ASKED OF THE MAP RATHER THAN DERIVED FROM THE MEMORY MAP. This used to sum the memory map's
	// regions and round up, which is the same number only on the port whose direct map is sized from
	// that map: a device-tree port's boot stub maps a FIXED window past the top of RAM, so an
	// address one page below the last byte of memory is comfortably inside what `phys_to_virt`
	// translates - and the assertion below was false there, for a correct reason.
	let ceiling = mem::direct_map_ceiling_for_test();
	assert!(ceiling > 0, "the direct map has a published ceiling by the time tests run");
	assert!(mem::within_direct_map(ceiling - 4096, 4096), "the last page of the map is inside it");
	assert!(!mem::within_direct_map(ceiling - 4096, 8192), "a table that starts inside and ends outside is not");
}

crate::tagged_test!(the_clock_never_goes_backwards_across_cores, [Smp, Kernel], id = "kernel.smp.the_clock_never_goes_backwards_across_cores", covers = ["kernel", "tickclock"]);
fn the_clock_never_goes_backwards_across_cores() {
	// THE TICK IS COMPUTED FROM EACH CORE'S OWN COUNTER READING, and counters on different cores are not
	// promised to agree to the cycle: a core a little behind would read a tick below one another core had
	// already answered, which is time running backwards for anything that compares the two. The clock keeps
	// one atomic maximum over every tick it answers; this is that promise, on the live clock, on every core at
	// once. (`kernel.smp.the_global_clock_advances_once_per_period_however_many_cores_tick` asserted the
	// counter gate this replaced; the arithmetic itself - the anchor, the sleep offset, a reading behind the
	// anchor - is the host suite `host.tickclock`.)
	use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
	const READINGS: u64 = 200_000;
	static LATEST: AtomicU64 = AtomicU64::new(0);
	static BACKWARDS: AtomicU64 = AtomicU64::new(0);
	static DONE: AtomicUsize = AtomicUsize::new(0);
	extern "C" fn read(_argument: u64) {
		let mut mine = 0u64;
		for _ in 0..READINGS {
			// A VALUE ANOTHER CORE PUBLISHED BEFORE THIS READING BEGAN is one the reading may not be below: that
			// core's own reading raised the clock's maximum before it published.
			let before = LATEST.load(Ordering::SeqCst);
			let now = crate::arch::apic::ticks();
			if now < before || now < mine {
				BACKWARDS.fetch_add(1, Ordering::SeqCst);
			}
			mine = now;
			LATEST.fetch_max(now, Ordering::SeqCst);
		}
		DONE.fetch_add(1, Ordering::SeqCst);
	}
	LATEST.store(crate::arch::apic::ticks(), Ordering::SeqCst);
	BACKWARDS.store(0, Ordering::SeqCst);
	DONE.store(0, Ordering::SeqCst);
	let cores = crate::smp::cpu_count();
	let mut readers = alloc::vec::Vec::new();
	for cpu in 1..cores {
		let event = crate::object::event::Event::create().expect("a test event");
		let thread = crate::sched::prepare_with_object_for(read, event, crate::object::rights::Rights::ALL, Some(cpu));
		assert!(crate::sched::start_thread_on(cpu, &thread), "a reader was queued on cpu {cpu}");
		readers.push(thread);
	}
	// And the core running this test reads beside them.
	read(0);
	let mut spins = 0u64;
	while DONE.load(Ordering::SeqCst) < cores {
		core::hint::spin_loop();
		spins += 1;
		assert!(spins < 20_000_000_000, "the readers on the other cores did not finish");
	}
	// And every reader is gone - dropped by its core - before this returns, so the next test's Domain counts
	// are its own.
	for thread in readers {
		let mut spins = 0u64;
		while alloc::sync::Arc::strong_count(&thread) > 1 {
			core::hint::spin_loop();
			spins += 1;
			assert!(spins < 20_000_000_000, "a finished reader was never reaped by its core");
		}
	}
	assert_eq!(BACKWARDS.load(Ordering::SeqCst), 0, "a reading on some core was below one already answered");
	crate::serial_println!("clock: {cores} core(s) x {READINGS} readings, none below one already answered");
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
