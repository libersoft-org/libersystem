pub mod serial;

// halt the kernel (wait-for-event)
pub fn halt_loop() -> ! {
	loop {
		unsafe {
			core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
		}
	}
}
