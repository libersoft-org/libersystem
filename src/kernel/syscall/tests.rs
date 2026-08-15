use super::*;
use crate::arch;

crate::tagged_test!(syscall_roundtrip_stateless, [Syscall, Smoke], id = "kernel.syscall.syscall_roundtrip_stateless", covers = ["kernel"]);
fn syscall_roundtrip_stateless() {
	// Stateless syscalls round-trip from the test (idle) context: there is no
	// current thread, but these calls do not need one.
	unsafe {
		// A call returns there and back, carrying a value across the boundary.
		assert_eq!(arch::syscall::invoke(SYS_DEBUG_NOOP, 0x1234, 0, 0, 0), 0x1234);
		// An unknown syscall number is rejected with the error sentinel.
		let bad = arch::syscall::invoke(9999, 0, 0, 0, 0);
		assert_eq!(bad as i64, ERR_BAD_SYSCALL);
		assert!(sys_is_err(bad));
		// The kernel clock is monotonic across two reads.
		let first = arch::syscall::invoke(SYS_CLOCK_GET, 0, 0, 0, 0);
		let second = arch::syscall::invoke(SYS_CLOCK_GET, 0, 0, 0, 0);
		assert!(second >= first);
		assert!(!sys_is_err(first));
	}
}

crate::tagged_test!(boot_profile_reports_nothing_when_the_boot_named_none, [Syscall], id = "kernel.syscall.boot_profile_reports_nothing_when_the_boot_named_none", covers = ["kernel"]);
fn boot_profile_reports_nothing_when_the_boot_named_none() {
	// The development-only artifact registry gates itself on this answer, so the answer for
	// an ordinary boot has to be dependable. A test boot never carries a profile - the
	// persistent development instance and the test configuration are mutually exclusive - so
	// this asserts the negative side of that gate, the side no test guest can reach through
	// the facility itself. It also proves the syscall is safe to call with no buffer at all,
	// which is how a caller asks whether a profile exists before sizing one.
	unsafe {
		assert_eq!(arch::syscall::invoke(SYS_BOOT_PROFILE, 0, 0, 0, 0), 0, "a boot that named no profile reports none");
		let mut name = [0xffu8; 32];
		assert_eq!(arch::syscall::invoke(SYS_BOOT_PROFILE, name.as_mut_ptr() as u64, name.len() as u64, 0, 0), 0, "and writes nothing into the caller's buffer");
		assert!(name.iter().all(|byte| *byte == 0xff), "the buffer is left untouched");
	}
}

crate::tagged_test!(abi_check_accepts_the_matching_revision_and_refuses_a_mismatch, [Syscall], id = "kernel.syscall.abi_check_accepts_the_matching_revision_and_refuses_a_mismatch", covers = ["kernel"]);
fn abi_check_accepts_the_matching_revision_and_refuses_a_mismatch() {
	// SYS_ABI_CHECK is the runtime's first syscall: a starting binary reports the ABI
	// revision it was built against, and the kernel refuses a mismatch so it never runs
	// against a renumbered call or a grown struct. Stateless, so it round-trips from the
	// idle context.
	unsafe {
		let accepted = arch::syscall::invoke(SYS_ABI_CHECK, ABI_VERSION as u64, 0, 0, 0);
		assert_eq!(accepted, 0, "the kernel's own ABI revision is accepted");
		assert!(!sys_is_err(accepted));
		let mismatch = arch::syscall::invoke(SYS_ABI_CHECK, ABI_VERSION as u64 + 1, 0, 0, 0);
		assert_eq!(mismatch as i64, ERR_ABI_MISMATCH, "a different ABI revision is refused");
		assert!(sys_is_err(mismatch));
	}
}

crate::tagged_test!(random_fills_distinct_bytes_from_whichever_source_is_honest, [Syscall], id = "kernel.syscall.random_fills_distinct_bytes_from_whichever_source_is_honest", covers = ["kernel"]);
fn random_fills_distinct_bytes_from_whichever_source_is_honest() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			// WHICHEVER syscall this machine can honestly answer.
			//
			// The secure one refuses where there is no hardware source, which is two of the three
			// architectures - so a test that only ever called it would be asserting the old
			// contract, where one syscall answered from the formula under a name that promised a
			// key. What is common to both is what this case is about: the buffer is filled, and two
			// draws differ.
			let call = if arch::random::secure_available() { SYS_RANDOM_GET } else { SYS_RANDOM_INSECURE };
			let mut first = [0u8; 32];
			let mut second = [0u8; 32];
			let first_len = arch::syscall::invoke(call, first.as_mut_ptr() as u64, first.len() as u64, 0, 0);
			let second_len = arch::syscall::invoke(call, second.as_mut_ptr() as u64, second.len() as u64, 0, 0);
			assert_eq!(first_len as usize, first.len(), "the source did not fill the whole buffer");
			assert_eq!(second_len as usize, second.len());
			// The buffer was actually written, and two draws differ (a false failure
			// is a 1-in-2^256 event).
			assert_ne!(first, [0u8; 32], "the source left the buffer zeroed");
			assert_ne!(first, second, "two random draws were identical");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	crate::sched::spawn(body, 0);
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}

crate::tagged_test!(the_syscall_image_buffer_allocates_and_survives_on_a_spawned_thread, [Kernel, Syscall, Memory], id = "kernel.syscall.the_syscall_image_buffer_allocates_and_survives_on_a_spawned_thread", covers = ["kernel"]);
fn the_syscall_image_buffer_allocates_and_survives_on_a_spawned_thread() {
	// `SYS_PROCESS_LOAD` buffers the caller's image into the kernel before parsing it, and that
	// buffer is the last untested thing on a path that breaks: driven from a spawned kernel thread on
	// aarch64, the load neither answers nor faults, and a control with the image FULLY MAPPED - so no
	// fault is possible - died with QEMU reporting a kernel address written into a virtio queue
	// register. Memory is being corrupted somewhere on that path whether or not a page is absent.
	//
	// This is the allocation on its own, in the context that breaks: the same `try_zeroed_bytes`, the
	// same 8 KiB, the same spawned thread, and nothing else - no handle, no user buffer, no loader.
	// It writes a pattern through the whole buffer and reads it back, so a buffer that is short,
	// overlapping something live, or not really there fails here rather than three layers later.
	//
	// If this passes, the allocation is cleared and the ELF loader is what remains. If it fails, this
	// is the bug, and it is one this milestone has already met once in another form - an allocation
	// reachable from a context that cannot safely make one.
	use core::sync::atomic::{AtomicU64, Ordering};

	const LEN: usize = 8192;
	// 0 = never ran, 1 = allocation refused, 2 = pattern did not survive, 3 = fine.
	static OUTCOME: AtomicU64 = AtomicU64::new(0);

	extern "C" fn body(_bootstrap: u64) {
		let Some(mut image) = super::try_zeroed_bytes(LEN) else {
			OUTCOME.store(1, Ordering::SeqCst);
			return;
		};
		assert_eq!(image.len(), LEN, "the buffer is the length that was asked for");
		for (index, slot) in image.iter_mut().enumerate() {
			*slot = (index % 251) as u8;
		}
		let intact = image.iter().enumerate().all(|(index, byte)| *byte == (index % 251) as u8);
		OUTCOME.store(if intact { 3 } else { 2 }, Ordering::SeqCst);
	}

	crate::sched::spawn(body, 0);
	crate::sched::run_until_idle();

	match OUTCOME.load(Ordering::SeqCst) {
		0 => panic!("the spawned thread never ran"),
		1 => panic!("an 8 KiB kernel buffer could not be allocated from a spawned thread"),
		2 => panic!("an 8 KiB kernel buffer did not hold what was written into it - the allocation overlaps something live"),
		_ => {}
	}
}

crate::tagged_test!(a_spawned_thread_can_create_a_child_process_and_let_it_go, [Kernel, Syscall, Process], id = "kernel.syscall.a_spawned_thread_can_create_a_child_process_and_let_it_go", covers = ["kernel"]);
fn a_spawned_thread_can_create_a_child_process_and_let_it_go() {
	// The first half of the path that breaks, on its own.
	//
	// `SYS_PROCESS_LOAD` from a spawned kernel thread neither answers nor faults on aarch64, and the
	// two things it does have now been split: the kernel-side image buffer is cleared by the test
	// above - it allocates and holds its contents - so what is left is creating the child process and
	// the loader that fills it. This test does the FIRST of those and stops: create the child, check
	// the handle, and let the thread end without loading anything.
	//
	// If this breaks, process creation or teardown from a spawned thread is the bug and the loader is
	// innocent. If it passes, both halves are individually sound and the loader is the last thing on
	// the path that has not been isolated.
	use core::sync::atomic::{AtomicI64, Ordering};

	static CHILD: AtomicI64 = AtomicI64::new(0);

	extern "C" fn body(_bootstrap: u64) {
		let child = unsafe { arch::syscall::invoke(SYS_PROCESS_CREATE, 0, 0, 0, 0) } as i64;
		CHILD.store(child, Ordering::SeqCst);
	}

	let (_kernel_ep, user_ep) = crate::object::channel::Channel::create();
	crate::sched::spawn_with_object(body, user_ep, crate::object::rights::Rights::ALL, 0);
	crate::sched::run_until_idle();

	let child = CHILD.load(Ordering::SeqCst);
	assert!(child != 0, "the spawned thread never ran");
	assert!(child > 0, "a spawned kernel thread with a process context must be able to create a child process, and got {child}");
}

crate::tagged_test!(the_loader_refuses_a_malformed_image_without_disturbing_anything, [Kernel, Process, Memory], id = "kernel.syscall.the_loader_refuses_a_malformed_image_without_disturbing_anything", covers = ["kernel"]);
fn the_loader_refuses_a_malformed_image_without_disturbing_anything() {
	// The last piece of the path that breaks, with everything else stripped off.
	//
	// `SYS_PROCESS_LOAD` from a spawned kernel thread breaks on aarch64 - and the same test passes on
	// x86_64, so the reproducer is sound and the fault is this target's. Every other stage has been
	// cleared by its own test: the scheduler runs the thread, the syscall preserves registers, the
	// exception table is intact, the fault report prints in full, the 8 KiB image buffer allocates and
	// holds its contents, and `SYS_PROCESS_CREATE` works from that thread.
	//
	// This calls the loader DIRECTLY - no spawned thread, no syscall dispatch, no handle table, no
	// user mapping, no faultable copy - with the same malformed image the control used: four bytes of
	// ELF magic and nothing else. A parser that reads `EI_CLASS` refuses it immediately.
	//
	// If this breaks, the loader owns the bug and none of the machinery around it matters.
	// If it passes, the loader is innocent in isolation and what breaks is the COMBINATION - which
	// points at the address space the load runs against rather than at the parsing.
	use crate::object::address_space::AddressSpace;
	use crate::object::process::Process;

	let mut image = alloc::vec![0u8; 8192];
	image[..4].copy_from_slice(b"\x7fELF");

	let process = Process::new(AddressSpace::create().expect("an address space"), crate::sched::root_domain()).expect("a test process");
	let outcome = crate::loader::load_image_into(&process, &image);
	assert!(outcome.is_err(), "four bytes of magic and eight kilobytes of zeroes is not a loadable image");
}
