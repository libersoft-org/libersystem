crate::tagged_test!(breakpoint_exception_returns, [Idt, Kernel, ArchX86_64], id = "kernel.arch.x86_64.idt.breakpoint_exception_returns", covers = ["kernel"]);
fn breakpoint_exception_returns() {
	// Reaching the next line proves the IDT breakpoint handler returned cleanly.
	unsafe { core::arch::asm!("int3") };
}
