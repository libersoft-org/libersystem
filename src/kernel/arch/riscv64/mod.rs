// riscv64 (RISC-V) architecture backend.
//
// STATUS: STUB. This module satisfies the same arch contract as `arch::x86_64`
// (the set of `arch::*` symbols the portable kernel calls - see the contract
// listed in `arch/mod.rs`) so that a cross-build for `riscv64gc-unknown-none-elf`
// links, but nothing here is implemented yet: the RISC-V mechanics (Sv39 page
// tables via SATP, the STVEC trap vector, PLIC + CLINT / SBI timer, SBI HSM
// hart_start SMP wake, ECALL syscall, the `tp` per-CPU register, the 16550 / SBI
// console, DTB parsing) land in M117. Runtime entry points `todo!()`; a boot on
// this arch is not possible until then.

pub mod serial;

// halt the kernel forever (wait-for-interrupt)
pub fn halt_loop() -> ! {
	loop {
		unsafe {
			core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
		}
	}
}

// install the trap vector and enable memory-protection features
pub fn init() {
	todo!("riscv64: STVEC + page-protection bits (M117)")
}

pub fn init_interrupts() {
	todo!("riscv64: PLIC + CLINT / SBI timer (M117)")
}

pub fn init_syscalls() {
	todo!("riscv64: ECALL vector wiring (M117)")
}

pub fn init_tsc() {
	todo!("riscv64: timebase-frequency (M117)")
}

pub fn init_bsp_percpu(_hartid: u32) {
	todo!("riscv64: tp register for the boot hart (M117)")
}

pub fn init_ap(_cpu_id: usize, _hartid: u32) {
	todo!("riscv64: secondary-hart bring-up (M117)")
}

// enable maskable interrupts on the current hart (set SSTATUS.SIE, bit 1)
pub fn enable_interrupts() {
	unsafe {
		core::arch::asm!("csrsi sstatus, 2", options(nomem, nostack, preserves_flags));
	}
}

pub fn disable_interrupts() {
	unsafe {
		core::arch::asm!("csrci sstatus, 2", options(nomem, nostack, preserves_flags));
	}
}

// True if supervisor interrupts are currently enabled (SSTATUS.SIE, bit 1).
pub fn interrupts_enabled() -> bool {
	let sstatus: u64;
	unsafe {
		core::arch::asm!("csrr {}, sstatus", out(reg) sstatus, options(nomem, nostack, preserves_flags));
	}
	sstatus & (1 << 1) != 0
}

// idle the hart until an interrupt (enable interrupts, then wait-for-interrupt)
pub fn idle_halt() {
	unsafe {
		core::arch::asm!("csrsi sstatus, 2", "wfi", options(nomem, nostack, preserves_flags));
	}
}

// reboot / power off via the SBI SRST extension - stubbed to a halt.
pub fn reset() -> ! {
	halt_loop()
}

pub fn poweroff() -> ! {
	halt_loop()
}

#[cfg(test)]
pub fn exit_qemu(_success: bool) -> ! {
	halt_loop()
}

// ------------------------------------------------------------------ paging
pub mod paging {
	// Placeholder page-table permission bits (mirrors the x86 flag set the
	// portable callers OR together; the real Sv39 PTE encoding lands in M117).
	pub const PRESENT: u64 = 1 << 0;
	pub const WRITABLE: u64 = 1 << 1;
	pub const USER: u64 = 1 << 2;
	pub const NO_CACHE: u64 = 1 << 4;
	pub const NO_EXECUTE: u64 = 1 << 63;

	pub fn enable_nx() {}
	pub fn enable_smap_smep() {}
	pub fn smap_enabled() -> bool {
		false
	}
	pub fn smep_enabled() -> bool {
		false
	}
	pub fn nx_enabled() -> bool {
		false
	}
	pub fn clac_on_entry() {}

	pub fn user_access<R>(f: impl FnOnce() -> R) -> R {
		f()
	}

	pub unsafe fn copy_to_user_page(_dst: u64, _bytes: &[u8]) {
		todo!("riscv64 paging (M117)")
	}

	pub fn map_page(_virt: u64, _phys: u64, _flags: u64) {
		todo!("riscv64 paging (M117)")
	}
	pub fn map_page_in(_satp: u64, _virt: u64, _phys: u64, _flags: u64) {
		todo!("riscv64 paging (M117)")
	}
	pub fn unmap_page(_virt: u64) -> Option<u64> {
		todo!("riscv64 paging (M117)")
	}
	pub fn unmap_pages(_base: u64, _count: usize) {
		todo!("riscv64 paging (M117)")
	}
	pub fn unmap_page_in(_satp: u64, _virt: u64) -> Option<u64> {
		todo!("riscv64 paging (M117)")
	}
	pub fn new_address_space() -> Option<u64> {
		todo!("riscv64 paging (M117)")
	}
	pub fn free_address_space(_satp: u64) {
		todo!("riscv64 paging (M117)")
	}
	pub fn translate(_virt: u64) -> Option<u64> {
		todo!("riscv64 paging (M117)")
	}
	pub fn remove_bootstrap_identity() {}
}

// ----------------------------------------------------------------- context
pub mod context {
	pub unsafe fn switch_context(_old_sp: *mut u64, _new_sp: u64) {
		todo!("riscv64 context switch (M117)")
	}
	pub fn init_thread_stack(_stack: &mut [u8], _entry: extern "C" fn(u64), _arg: u64) -> u64 {
		todo!("riscv64 context switch (M117)")
	}
	// The active address-space token (SATP on riscv64; kept named `cr3` for the
	// portable contract).
	pub fn read_cr3() -> u64 {
		todo!("riscv64 SATP (M117)")
	}
	pub unsafe fn write_cr3(_satp: u64) {
		todo!("riscv64 SATP (M117)")
	}
}

// ------------------------------------------------------------------ percpu
pub mod percpu {
	pub struct PerCpu;

	impl PerCpu {
		pub fn cpu_id(&self) -> u32 {
			todo!("riscv64 per-CPU (M117)")
		}
		pub fn lapic_id(&self) -> u32 {
			todo!("riscv64 per-CPU (M117)")
		}
	}

	pub fn allocate(_count: usize) {
		todo!("riscv64 per-CPU (M117)")
	}
	pub fn init(_cpu_id: usize, _hartid: u32) {
		todo!("riscv64 per-CPU (M117)")
	}
	pub fn this_cpu() -> &'static PerCpu {
		todo!("riscv64 per-CPU (M117)")
	}
	pub fn set_kernel_rsp(_value: u64) {
		todo!("riscv64 per-CPU (M117)")
	}
	pub fn set_tss_rsp0_slot(_addr: u64) {
		todo!("riscv64 per-CPU (M117)")
	}
	pub fn set_rsp0(_value: u64) {
		todo!("riscv64 per-CPU (M117)")
	}
	pub fn in_user_syscall() -> bool {
		todo!("riscv64 per-CPU (M117)")
	}
}

// -------------------------------------------------------------- interrupts
pub mod interrupts {
	use crate::object::interrupt::Interrupt;
	use alloc::sync::Arc;

	pub const IRQ_BASE: u8 = 32;

	pub type HandlerFn = fn(u8);

	pub fn is_bindable(_vector: u8) -> bool {
		false
	}
	pub fn bind(_vector: u8, _intr: &Arc<Interrupt>) -> bool {
		false
	}
	pub fn unbind(_vector: u8) {}
	pub fn acquire_msi(_table_phys: u64, _dest: u8, _owner: u32) -> Option<u8> {
		None
	}
	pub fn irq_info(_index: usize) -> Option<abi::IrqInfo> {
		None
	}
	pub fn irq_info_len() -> usize {
		0
	}
	pub fn bind_msi(_vector: u8, _intr: &Arc<Interrupt>) -> bool {
		false
	}
	pub fn is_bound(_vector: u8) -> bool {
		false
	}
	pub fn register(_vector: u8, _handler: HandlerFn) {}
	pub fn init() {
		todo!("riscv64 PLIC (M117)")
	}
}

// -------------------------------------------------------------------- apic
// (the riscv64 interrupt controller is the PLIC/CLINT; the module keeps the
// portable `apic` name for the contract until the ports rename it.)
pub mod apic {
	pub fn local_id() -> u32 {
		todo!("riscv64 hartid (M117)")
	}
	pub fn eoi() {
		todo!("riscv64 PLIC claim/complete (M117)")
	}
	pub fn send_wake_ipi(_dest: u32) {
		todo!("riscv64 SBI IPI (M117)")
	}
	pub fn send_init(_dest: u32) {
		todo!("riscv64 SBI HSM (M117)")
	}
	pub fn send_startup(_dest: u32, _vector: u8) {
		todo!("riscv64 SBI HSM (M117)")
	}
	pub fn ticks() -> u64 {
		todo!("riscv64 rdtime (M117)")
	}
	pub fn init() {
		todo!("riscv64 PLIC + timer (M117)")
	}
	pub fn init_ap() {
		todo!("riscv64 per-hart timer (M117)")
	}
}

// --------------------------------------------------------------------- tsc
pub mod tsc {
	pub fn now() -> u64 {
		todo!("riscv64 rdtime (M117)")
	}
	pub fn init() {
		todo!("riscv64 timebase-frequency (M117)")
	}
	pub fn hz() -> u64 {
		todo!("riscv64 timebase-frequency (M117)")
	}
	pub fn cycles_to_ns(_cycles: u64) -> u64 {
		todo!("riscv64 timer scaling (M117)")
	}
}

// ------------------------------------------------------------------ ioapic
pub mod ioapic {
	pub fn route(_gsi: u32, _vector: u8, _dest: u32) {
		todo!("riscv64 PLIC routing (M117)")
	}
	pub fn init() {
		todo!("riscv64 PLIC (M117)")
	}
	pub fn mask(_gsi: u32) {
		todo!("riscv64 PLIC mask (M117)")
	}
}

// --------------------------------------------------------------------- rtc
pub mod rtc {
	pub fn read_unix() -> u64 {
		todo!("riscv64 Goldfish RTC (M117)")
	}
}

// ------------------------------------------------------------------ random
pub mod random {
	pub fn fill(_buf: &mut [u8]) {
		todo!("riscv64 entropy source (M117)")
	}
}

// ------------------------------------------------------------------ apboot
// (riscv64 wakes secondary harts via the SBI HSM `hart_start` call, not a
// real-mode trampoline; these keep the portable names so smp.rs links until
// M117 replaces the wake path.)
pub mod apboot {
	pub fn trampoline_len() -> usize {
		0
	}
	pub unsafe fn install(_dst: *mut u8, _satp: u64, _entry: u64) {
		todo!("riscv64 SBI HSM wake (M117)")
	}
	pub unsafe fn set_stack(_dst: *mut u8, _stack_top: u64) {
		todo!("riscv64 SBI HSM wake (M117)")
	}
}

// ----------------------------------------------------------------- syscall
pub mod syscall {
	pub fn init() {
		todo!("riscv64 ECALL wiring (M117)")
	}
	pub unsafe fn invoke(_num: u64, _a0: u64, _a1: u64, _a2: u64, _a3: u64) -> u64 {
		todo!("riscv64 ECALL (M117)")
	}
}

// ---------------------------------------------------------------- usermode
pub mod usermode {
	pub const FAULT_PROBE_ADDR: u64 = 0x0dea_d000;

	pub unsafe fn enter(_entry: u64, _user_stack: u64, _arg: u64) {
		todo!("riscv64 U-mode entry (M117)")
	}
	pub fn exit_to_kernel() -> ! {
		todo!("riscv64 U-mode return (M117)")
	}
	pub fn program_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
	pub fn program_fault_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
	pub fn program_yield_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
	pub fn program_nx_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
	pub fn program_stack_probe_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
	pub fn program_spin_bytes() -> &'static [u8] {
		todo!("riscv64 test program bytes (M117)")
	}
}

// --------------------------------------------------------------------- pci
// PCI config space is a bus standard; only the config-space ACCESS mechanism is
// arch-specific (x86 ports vs riscv64 ECAM MMIO). The device types + scan logic
// become portable in M117; for now the scans return empty so the tree links.
pub mod pci {
	use alloc::vec::Vec;

	#[derive(Clone, Copy)]
	pub struct PciDevice {
		pub bus: u8,
		pub dev: u8,
		pub func: u8,
		pub vendor: u16,
		pub device_id: u16,
		pub class: u8,
		pub subclass: u8,
		pub prog_if: u8,
		pub header_type: u8,
		pub bars: [u32; 6],
	}

	#[derive(Clone, Copy, Default)]
	pub struct VirtioCap {
		pub bar: u8,
		pub offset: u32,
		pub length: u32,
		pub notify_multiplier: u32,
	}

	#[derive(Clone, Copy)]
	pub struct VirtioDevice {
		pub pci: PciDevice,
		pub virtio_type: u16,
		pub bar: u8,
		pub bar_phys: u64,
		pub region_len: u64,
		pub common: VirtioCap,
		pub notify: VirtioCap,
		pub isr: VirtioCap,
		pub device: VirtioCap,
		pub msix_cap: u16,
		pub msix_count: u16,
		pub msix_table_phys: u64,
	}

	#[derive(Clone, Copy)]
	pub struct XhciDevice {
		pub pci: PciDevice,
		pub bar_phys: u64,
		pub bar_len: u64,
		pub msix_cap: u16,
		pub msix_count: u16,
		pub msix_table_phys: u64,
	}

	pub fn scan() -> Vec<PciDevice> {
		Vec::new()
	}
	pub fn scan_virtio() -> Vec<VirtioDevice> {
		Vec::new()
	}
	pub fn scan_xhci() -> Vec<XhciDevice> {
		Vec::new()
	}
	pub fn set_intx_disabled(_bus: u8, _dev: u8, _func: u8, _disabled: bool) {}
	pub fn msix_enable(_bus: u8, _dev: u8, _func: u8, _cap: u16) {}
}
