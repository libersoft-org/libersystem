// Renamed from `breakpoint_exception_returns` 2026-08-10: the x86_64 IDT suite has a test of the
// same name, and the verification model joins the built binary's symbols to this declaration by the
// Rust function name - so two tests with one name were one test to it, sharing architectures and
// covers. The `id` below is UNCHANGED, which is the whole point of having one: the rename cost the
// test nothing, not its identity and not its history.
crate::tagged_test!(riscv64_breakpoint_exception_returns, [Kernel, ArchRiscv64], id = "kernel.arch.riscv64.traps.breakpoint_exception_returns", covers = ["kernel"]);
fn riscv64_breakpoint_exception_returns() {
	// Reaching the next line proves the trap handler resumed past ebreak: it decodes the
	// trapped instruction width (2 bytes for a compressed c.ebreak, else 4) and advances
	// sepc, the RISC-V analogue of the x86 int3 breakpoint round-trip.
	unsafe { core::arch::asm!("ebreak") };
}
