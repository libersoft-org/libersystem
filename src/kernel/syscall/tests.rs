use super::*;
use crate::arch;

crate::tagged_test!(syscall_roundtrip_stateless, [Syscall, Smoke]);
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

crate::tagged_test!(boot_profile_reports_nothing_when_the_boot_named_none, [Syscall]);
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

crate::tagged_test!(abi_check_accepts_the_matching_revision_and_refuses_a_mismatch, [Syscall]);
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

crate::tagged_test!(random_get_fills_distinct_bytes, [Syscall]);
fn random_get_fills_distinct_bytes() {
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let mut first = [0u8; 32];
			let mut second = [0u8; 32];
			let first_len = arch::syscall::invoke(SYS_RANDOM_GET, first.as_mut_ptr() as u64, first.len() as u64, 0, 0);
			let second_len = arch::syscall::invoke(SYS_RANDOM_GET, second.as_mut_ptr() as u64, second.len() as u64, 0, 0);
			assert_eq!(first_len as usize, first.len(), "random_get did not fill the whole buffer");
			assert_eq!(second_len as usize, second.len());
			// The buffer was actually written, and two draws differ (a false failure
			// is a 1-in-2^256 event).
			assert_ne!(first, [0u8; 32], "random_get left the buffer zeroed");
			assert_ne!(first, second, "two random draws were identical");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	crate::sched::spawn(body, 0);
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
}
