use super::*;

crate::tagged_test!(the_exception_table_exists_and_is_small_enough_to_scan, [Kernel, Memory], id = "kernel.extable.the_exception_table_exists_and_is_small_enough_to_scan", covers = ["kernel"]);
fn the_exception_table_exists_and_is_small_enough_to_scan() {
	// Two claims this module makes about itself, both of which stop being true silently.
	//
	// The first is that the mechanism is IN the binary. `.extable` is emitted by inline assembly
	// inside the copy routines and gathered by the linker script; drop the `KEEP`, rename the
	// section, or let the routines be optimised out and the table is empty - and an empty table
	// turns every fixup into a kernel halt, which is exactly the behaviour this milestone replaced.
	// Nothing else would notice until a userspace race took the machine down.
	assert!(declared() > 0, "the exception table is empty: no instruction in this build declares that it may fault");

	// The second is that a LINEAR scan is the right shape. It is, at this size - the faulting
	// instructions are the copy loops themselves and there are a handful of them. The moment that
	// stops being true the lookup runs inside a fault handler on every kernel fault, and the answer
	// is to sort the table and binary-search it. This is the line that will say so.
	assert!(declared() <= 64, "the exception table has grown to {} entries; a linear scan in the fault handler is no longer the right shape - sort it and binary-search", declared());
}

crate::tagged_test!(a_copy_to_a_page_that_is_not_there_reports_rather_than_kills_the_kernel, [Kernel, Memory, Syscall], id = "kernel.extable.a_copy_to_a_page_that_is_not_there_reports_rather_than_kills_the_kernel", covers = ["kernel"]);
fn a_copy_to_a_page_that_is_not_there_reports_rather_than_kills_the_kernel() {
	// The whole milestone in one assertion: a kernel copy to a user address that is NOT mapped must
	// come back saying how far it got, and the machine must still be running to hear it.
	//
	// Without the fixup this does not fail, it HALTS - the ring-0 page fault falls through to the
	// halt loop and the suite stops dead at this line with no result. That is the shape of the
	// defect too, so an unfixed kernel and a broken fixup look identical from here, which is the
	// point: there is no way to write this test such that it merely goes red.
	//
	// A user-half address nothing has mapped. The kernel half would fault for a different reason
	// (and on x86_64 with SMAP the user half is exactly where the protection lives), so this has to
	// be an address userspace COULD have had and does not.
	//
	// Derived from `USER_VA_END` rather than written out, because the user half is not the same size
	// on every target: riscv64 runs Sv39 and its user half ends at 2^38, where the 2^46 address this
	// used to name is not merely unmapped but unmappable. That difference is invisible in a test
	// that only wants a fault - both answers fault - and it is exactly what broke the partial-copy
	// test below, which needs the address to be one the kernel can really map.
	const ABSENT: u64 = crate::memlayout::USER_VA_END / 2;
	let payload = [0xA5u8; 64];

	let caught_before = caught();
	let copied = crate::arch::paging::user_access(|| unsafe { crate::arch::usercopy::copy_to_user(ABSENT, payload.as_ptr(), payload.len()) });
	assert_eq!(copied, 0, "not one byte can have landed in a page that is not there");
	assert!(caught() > caught_before, "the fault was fixed up, which is the only way execution reached this line");

	// And the reverse direction, which is a different instruction and a different table entry: the
	// LOAD faults rather than the store.
	let mut into = [0u8; 64];
	let caught_before = caught();
	let read = crate::arch::paging::user_access(|| unsafe { crate::arch::usercopy::copy_from_user(into.as_mut_ptr(), ABSENT, into.len()) });
	assert_eq!(read, 0, "nothing can be read out of a page that is not there");
	assert!(caught() > caught_before, "and that fault was fixed up too");
	assert_eq!(into, [0u8; 64], "a refused read leaves the destination alone");

	// The kernel is still healthy: an ordinary copy between two kernel buffers still works, which
	// says the fixup returned to the right place rather than somewhere that happens not to crash.
	let mut ok = [0u8; 64];
	let n = unsafe { crate::arch::usercopy::copy_from_user(ok.as_mut_ptr(), payload.as_ptr() as u64, payload.len()) };
	assert_eq!(n, payload.len(), "a copy that faults nowhere copies everything");
	assert_eq!(ok, payload, "and copies the right bytes");
}

crate::tagged_test!(a_fault_on_a_kernel_address_is_never_rescued_however_the_pc_matches, [Kernel, Memory], id = "kernel.extable.a_fault_on_a_kernel_address_is_never_rescued_however_the_pc_matches", covers = ["kernel"]);
fn a_fault_on_a_kernel_address_is_never_rescued_however_the_pc_matches() {
	// The condition that keeps a real kernel bug loud, which the PC match alone does not.
	//
	// The table's entries cover instructions that touch a KERNEL operand as well as a user one: the
	// aarch64 and riscv64 byte loops declare both their load and their store, and x86_64's
	// `rep movsb` reads and writes in one instruction. So a kernel bug that hands `copy_to_user` a
	// bad kernel source pointer faults at a PC that is genuinely in the table. Recovering it would
	// convert that bug into a short copy - a wrong answer, delivered quietly, to a caller with no
	// way to tell.
	//
	// Asserted on the lookup rather than by faulting, because the failure mode of getting this
	// wrong is a kernel that keeps running with a corrupted assumption, and there is no way to
	// arrange the real fault without also arranging the bug.
	let declared = declared();
	assert!(declared > 0, "there is a table to ask about");

	// Every address the table names is a real fixup, so a user-half fault at one is rescued.
	let entry = first_entry().expect("the table has at least one entry");
	assert!(fixup_for(entry, crate::memlayout::USER_VA_END / 2).is_some(), "a declared instruction faulting on a user address must be rescued - that is the whole mechanism");

	// The SAME instruction faulting on a kernel address is not.
	assert!(fixup_for(entry, crate::memlayout::USER_VA_END).is_none(), "the first address of the kernel half is not userspace's");
	assert!(fixup_for(entry, u64::MAX & !0xFFF).is_none(), "nor is the top of the address space");
	assert!(fixup_for(entry, 0xFFFF_8000_0000_0000).is_none(), "nor is a higher-half kernel pointer, which is what a kernel bug passes");
}

crate::tagged_test!(a_copy_that_loses_its_page_partway_reports_exactly_how_far_it_got, [Kernel, Memory, Syscall], id = "kernel.extable.a_copy_that_loses_its_page_partway_reports_exactly_how_far_it_got", covers = ["kernel"]);
fn a_copy_that_loses_its_page_partway_reports_exactly_how_far_it_got() {
	// The state a mid-copy unmap actually produces, arranged deterministically.
	//
	// The requirement is a test that unmaps the page DURING the copy, on the grounds that the defect is
	// a race. The race is real and the interleaving is not reproducible on a cooperative single-core
	// harness - this suite has retired tests that tried. What IS reproducible is the state the race
	// leaves behind: a copy running into a page that is not there, partway through, with bytes
	// already delivered behind it. Two adjacent user pages with only the first mapped put the copy
	// in exactly that state at exactly the boundary, every time.
	//
	// And it tests more than the wholly-absent case does. A copy that faults on its first byte only
	// proves the fixup runs; this proves the ACCOUNTING - that the count coming back is the bytes
	// that landed, not zero and not the length asked for. A caller that believed either would
	// truncate a message or read uninitialised memory.
	use crate::arch::paging::{PRESENT, USER, WRITABLE};
	use crate::mem::frame::PAGE_SIZE;

	// Inside the user half on EVERY target - see `ABSENT` above for why that is not automatic. Two
	// pages are needed, so the address is a page-pair below the halfway mark rather than at it.
	const AT: u64 = crate::memlayout::USER_VA_END / 2 - 2 * PAGE_SIZE;
	let frame = crate::mem::frame::allocate().expect("a frame for the mapped half");
	crate::arch::paging::map_page(AT, frame, PRESENT | WRITABLE | USER);
	// The page AFTER it is deliberately absent. Nothing to undo at the end.

	let payload = alloc::vec![0x5Au8; (PAGE_SIZE * 2) as usize];
	let caught_before = caught();
	let copied = crate::arch::paging::user_access(|| unsafe { crate::arch::usercopy::copy_to_user(AT, payload.as_ptr(), payload.len()) });

	assert_eq!(copied as u64, PAGE_SIZE, "the copy stops at the page boundary and says so - not zero, not the whole length");
	assert!(caught() > caught_before, "and it stopped by being fixed up, not by checking anything");

	// The bytes that ARE claimed really arrived. A fixup that reported the right number while
	// delivering nothing would pass every assertion above.
	let landed = crate::arch::paging::user_access(|| {
		let mut all = true;
		for offset in [0u64, 1, PAGE_SIZE / 2, PAGE_SIZE - 1] {
			all &= unsafe { core::ptr::read_volatile((AT + offset) as *const u8) } == 0x5A;
		}
		all
	});
	assert!(landed, "every byte the copy counted must be on the page");

	crate::arch::paging::unmap_page(AT);
	unsafe { crate::mem::frame::deallocate(frame) };
}

crate::tagged_test!(a_process_load_of_a_fully_mapped_image_answers_without_faulting, [Kernel, Memory, Syscall, Process], id = "kernel.extable.a_process_load_of_a_fully_mapped_image_answers_without_faulting", covers = ["kernel"]);
fn a_process_load_of_a_fully_mapped_image_answers_without_faulting() {
	// The CONTROL for the test below, and the experiment that separates its two suspects.
	//
	// That test hangs on aarch64 - and prints nothing while doing it, which now means something
	// precise: a deliberate null dereference on this target prints its `NO ENTRY` line, its
	// `aarch64 EXCEPTION` line and `halting`, complete and in order. So the reporting path works, and
	// a hang that prints nothing is NOT an unhandled fault. Something inside `SYS_PROCESS_LOAD` stops
	// without ever faulting, and there are two candidates: the bounded kernel-side allocation the
	// image is copied into, and the faultable copy itself.
	//
	// This one maps BOTH pages, so the copy cannot fault. Everything else is identical - the same
	// spawned thread, the same handle, the same length, the same allocation. An inline variant was
	// tried first and is not possible: `SYS_PROCESS_CREATE` refuses from the test thread, which has
	// none of the process context `spawn_with_object` gives.
	//
	// If this hangs, the fault is irrelevant and the allocation path is the bug.
	// If it answers, the hang needs the fault, and the copy is where to look.
	use crate::arch::paging::{PRESENT, USER, WRITABLE};
	use crate::mem::frame::PAGE_SIZE;
	use core::sync::atomic::{AtomicI64, Ordering};

	const AT: u64 = crate::memlayout::USER_VA_END / 2 - 8 * PAGE_SIZE;
	static ANSWER: AtomicI64 = AtomicI64::new(0);

	// What the SPAWNED thread can see of the mapping the TEST thread made. Every component of this
	// path has now been cleared in isolation - scheduler, registers, exception table, fault report,
	// the kernel-side buffer, process creation, and the loader called directly - so what is left is
	// the combination, and the first thing a combination can get wrong is whose address space the
	// pages are in. `map_page` maps into the ACTIVE address space; if the spawned thread's is not the
	// one the test mapped into, the image the loader is handed is not the image that was written.
	static SEEN_FIRST: AtomicI64 = AtomicI64::new(-1);
	static SEEN_SECOND: AtomicI64 = AtomicI64::new(-1);

	extern "C" fn spawner(_bootstrap: u64) {
		unsafe {
			// STAGED MARKERS, because the thread does not come back and the atomics above can only be
			// read by a test body that never resumes. Each marker is a short constant so that garbled
			// output is unmistakable rather than merely suspicious - the earlier probe run showed
			// clean lines up to a point and mangled ones after, and the LAST CLEAN MARKER is the
			// measurement this run exists to take.
			crate::serial_println!("MARK-1-entered");
			// Read before loading, so a mapping the loader cannot see is reported rather than
			// inferred. Both pages are mapped here, so both reads must succeed.
			// INSIDE `user_access`, because these read a USER address from ring 0 and x86_64 has
			// SMAP. The raw reads passed on aarch64 - where `user_access` is a passthrough, there
			// being no PAN on cortex-a72 - and faulted on x86_64 with error code 0x1, a supervisor
			// read of a present user page, which is exactly what SMAP is for. A probe that only
			// works on the target being debugged is a probe that hides the target's differences.
			crate::arch::paging::user_access(|| {
				SEEN_FIRST.store(core::ptr::read_volatile(AT as *const u8) as i64, Ordering::SeqCst);
				SEEN_SECOND.store(core::ptr::read_volatile((AT + PAGE_SIZE) as *const u8) as i64, Ordering::SeqCst);
			});
			crate::serial_println!("MARK-2-read first={} second={}", SEEN_FIRST.load(Ordering::SeqCst), SEEN_SECOND.load(Ordering::SeqCst));
			// A canary either side of the create call. `a_spawned_thread_can_create_a_child_process_
			// and_let_it_go` proves the call RETURNS a handle; it does not prove it leaves memory
			// alone, and the probe run garbled its very next line - so this checks rather than
			// assumes.
			let mut canary = alloc::vec![0xA5u8; 4096];
			crate::serial_println!("MARK-3-canary-ready");
			let child = crate::arch::syscall::invoke(crate::syscall::SYS_PROCESS_CREATE, 0, 0, 0, 0);
			crate::serial_println!("MARK-4-created");
			let intact = canary.iter().all(|byte| *byte == 0xA5);
			crate::serial_println!("MARK-5-canary intact={intact}");
			canary[0] = 0;
			assert!((child as i64) > 0, "the child process is created");
			crate::serial_println!("MARK-6-calling-load");
			let answer = crate::arch::syscall::invoke(crate::syscall::SYS_PROCESS_LOAD, child, AT, 2 * PAGE_SIZE, 0) as i64;
			crate::serial_println!("MARK-7-load-returned");
			ANSWER.store(answer, Ordering::SeqCst);
		}
	}

	let first = crate::mem::frame::allocate().expect("a frame for the first page");
	let second = crate::mem::frame::allocate().expect("a frame for the second page");
	crate::arch::paging::map_page(AT, first, PRESENT | WRITABLE | USER);
	crate::arch::paging::map_page(AT + PAGE_SIZE, second, PRESENT | WRITABLE | USER);
	crate::arch::paging::user_access(|| unsafe {
		core::ptr::write_bytes(AT as *mut u8, 0, 2 * PAGE_SIZE as usize);
		core::ptr::copy_nonoverlapping(b"\x7fELF".as_ptr(), AT as *mut u8, 4);
	});

	let (_kernel_ep, user_ep) = crate::object::channel::Channel::create();
	crate::sched::spawn_with_object(spawner, user_ep, crate::object::rights::Rights::ALL);
	crate::sched::run_until_idle();

	// Four bytes of magic and eight kilobytes of zeroes is not a loadable image, so the loader
	// refuses it. What matters is that it ANSWERS: the syscall returned, from the same thread shape
	// and the same allocation that hangs when the second page is absent.
	// The magic byte is at AT and the second page is all zeroes, so this says whether the spawned
	// thread saw the SAME memory the test wrote - checked before the answer, because a wrong image
	// makes every later assertion meaningless.
	assert_eq!(SEEN_FIRST.load(Ordering::SeqCst), 0x7f, "the spawned thread must see the image the test mapped - its first byte is the ELF magic");
	assert_eq!(SEEN_SECOND.load(Ordering::SeqCst), 0, "and its second page, which the test zeroed");
	assert!(ANSWER.load(Ordering::SeqCst) < 0, "a load of a malformed but fully mapped image must answer with an error rather than hang");

	crate::arch::paging::unmap_page(AT);
	crate::arch::paging::unmap_page(AT + PAGE_SIZE);
	unsafe {
		crate::mem::frame::deallocate(first);
		crate::mem::frame::deallocate(second);
	}
}

crate::tagged_test!(a_process_load_whose_image_goes_away_is_an_error_rather_than_a_dead_kernel, [Kernel, Memory, Syscall, Process], id = "kernel.extable.a_process_load_whose_image_goes_away_is_an_error_rather_than_a_dead_kernel", covers = ["kernel"]);
fn a_process_load_whose_image_goes_away_is_an_error_rather_than_a_dead_kernel() {
	// The path the exception table was written for and did not cover.
	//
	// `SYS_PROCESS_LOAD` used to hand the ELF loader a raw slice over the caller's memory and run
	// the whole load inside a `user_access` window. Every read of that slice is ordinary code in the
	// ELF parser - a bounds check, a header field, a segment copy - and none of it is in `.extable`,
	// nor can it be, because it is not a copy routine. A page that went away partway through
	// faulted in ring 0 at an unrescuable PC and the machine halted. It needed no privilege: create
	// a child in your own Domain, call load on a large image, unmap the buffer from another thread.
	//
	// The image is copied into the kernel first now, through the faultable copy, so the loader reads
	// memory userspace cannot take away and the refusal happens at a declared instruction.
	//
	// Driven from a kernel thread through the real syscalls, because the load has to get past a
	// process handle before it reaches the image at all - the shape `userspace_spawn_syscalls_...`
	// uses. Like the other tests in this file, an unfixed kernel does not fail here, it HALTS.
	use crate::arch::paging::{PRESENT, USER, WRITABLE};
	use crate::mem::frame::PAGE_SIZE;
	use core::sync::atomic::{AtomicI64, AtomicU64, Ordering};

	// Two user pages with only the first mapped. An ELF magic at the start, so the length and the
	// header are plausible and the refusal has to come from the absent second page.
	const AT: u64 = crate::memlayout::USER_VA_END / 2 - 4 * PAGE_SIZE;
	static ANSWER: AtomicI64 = AtomicI64::new(0);
	static CAUGHT_BEFORE: AtomicU64 = AtomicU64::new(0);

	extern "C" fn spawner(_bootstrap: u64) {
		unsafe {
			let child = crate::arch::syscall::invoke(crate::syscall::SYS_PROCESS_CREATE, 0, 0, 0, 0);
			assert!((child as i64) > 0, "the child process is created");
			CAUGHT_BEFORE.store(super::caught(), Ordering::SeqCst);
			let answer = crate::arch::syscall::invoke(crate::syscall::SYS_PROCESS_LOAD, child, AT, 2 * PAGE_SIZE, 0) as i64;
			ANSWER.store(answer, Ordering::SeqCst);
		}
	}

	let frame = crate::mem::frame::allocate().expect("a frame for the mapped half");
	crate::arch::paging::map_page(AT, frame, PRESENT | WRITABLE | USER);
	crate::arch::paging::user_access(|| unsafe {
		core::ptr::write_bytes(AT as *mut u8, 0, PAGE_SIZE as usize);
		core::ptr::copy_nonoverlapping(b"\x7fELF".as_ptr(), AT as *mut u8, 4);
	});

	let (_kernel_ep, user_ep) = crate::object::channel::Channel::create();
	crate::sched::spawn_with_object(spawner, user_ep, crate::object::rights::Rights::ALL);
	crate::sched::run_until_idle();

	assert!(ANSWER.load(Ordering::SeqCst) < 0, "a load whose image is half absent must answer with an error - and the kernel must still be running to answer");
	assert!(caught() > CAUGHT_BEFORE.load(Ordering::SeqCst), "the absent page was reached through the faultable copy; if it were not, this line would not have been reached at all");

	crate::arch::paging::unmap_page(AT);
	unsafe { crate::mem::frame::deallocate(frame) };
}
