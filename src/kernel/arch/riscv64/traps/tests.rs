crate::tagged_test!(breakpoint_exception_returns, [Kernel, ArchRiscv64]);
fn breakpoint_exception_returns() {
	// Reaching the next line proves the trap handler resumed past ebreak: it decodes the
	// trapped instruction width (2 bytes for a compressed c.ebreak, else 4) and advances
	// sepc, the RISC-V analogue of the x86 int3 breakpoint round-trip.
	unsafe { core::arch::asm!("ebreak") };
}
