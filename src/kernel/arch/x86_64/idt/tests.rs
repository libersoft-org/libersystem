crate::tagged_test!(breakpoint_exception_returns, [Idt, Kernel, ArchX86_64]);
fn breakpoint_exception_returns() {
	// Reaching the next line proves the IDT breakpoint handler returned cleanly.
	unsafe { core::arch::asm!("int3") };
}
