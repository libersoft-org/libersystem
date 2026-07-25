use super::register;
use core::sync::atomic::{AtomicBool, Ordering};

crate::tagged_test!(handler_registration_dispatch, [Interrupt, Kernel, ArchX86_64]);
fn handler_registration_dispatch() {
	static FIRED: AtomicBool = AtomicBool::new(false);
	fn handler(_vector: u8) {
		FIRED.store(true, Ordering::SeqCst);
	}
	// Register on an unused device vector and trigger it with a software
	// interrupt: proves registration and dispatch wiring without a device.
	register(47, handler);
	unsafe { core::arch::asm!("int 0x2f", options(nomem, nostack)) };
	assert!(FIRED.load(Ordering::SeqCst));
}
