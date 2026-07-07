// aarch64 (ARM64) architecture backend.
//
// STATUS: STUB. This module satisfies the same arch contract as `arch::x86_64`
// (the set of `arch::*` symbols the portable kernel calls - see the contract
// listed in `arch/mod.rs`) so that a cross-build for `aarch64-unknown-none`
// links, but nothing here is implemented yet: the ARMv8-A mechanics (VMSAv8
// page tables, the VBAR_EL1 vector table, the GIC + generic timer, PSCI SMP
// wake, SVC syscall, TPIDR_EL1 per-CPU, PL011 UART, DTB parsing) land in M116.
// Runtime entry points `todo!()`; a boot on this arch is not possible until then.

mod boot;
mod exceptions;
mod gic;
pub mod serial;

// halt the kernel forever (wait-for-event)
pub fn halt_loop() -> ! {
	loop {
		unsafe {
			core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
		}
	}
}

// install the CPU exception vectors and enable memory-protection features
pub fn init() {
	todo!("aarch64: VBAR_EL1 + MMU protection bits (M116)")
}

pub fn init_interrupts() {
	todo!("aarch64: GIC + generic timer (M116)")
}

pub fn init_syscalls() {
	todo!("aarch64: SVC vector wiring (M116)")
}

pub fn init_tsc() {
	todo!("aarch64: generic-timer frequency (M116)")
}

pub fn init_bsp_percpu(_mpidr: u32) {
	todo!("aarch64: TPIDR_EL1 for the boot core (M116)")
}

pub fn init_ap(_cpu_id: usize, _mpidr: u32) {
	todo!("aarch64: secondary-core bring-up (M116)")
}

// enable maskable interrupts on the current core (clear DAIF.I)
pub fn enable_interrupts() {
	unsafe {
		core::arch::asm!("msr daifclr, #2", options(nomem, nostack, preserves_flags));
	}
}

pub fn disable_interrupts() {
	unsafe {
		core::arch::asm!("msr daifset, #2", options(nomem, nostack, preserves_flags));
	}
}

// True if IRQs are currently unmasked (DAIF.I clear, bit 7).
pub fn interrupts_enabled() -> bool {
	let daif: u64;
	unsafe {
		core::arch::asm!("mrs {}, daif", out(reg) daif, options(nomem, nostack, preserves_flags));
	}
	daif & (1 << 7) == 0
}

// idle the core until an interrupt (enable IRQs, then wait-for-interrupt)
pub fn idle_halt() {
	unsafe {
		core::arch::asm!("msr daifclr, #2", "wfi", options(nomem, nostack, preserves_flags));
	}
}

// reboot / power off via PSCI (SYSTEM_RESET / SYSTEM_OFF) - stubbed to a halt.
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
pub mod paging;

// ----------------------------------------------------------------- context
pub mod context {
	pub unsafe fn switch_context(_old_sp: *mut u64, _new_sp: u64) {
		todo!("aarch64 context switch (M116)")
	}
	pub fn init_thread_stack(_stack: &mut [u8], _entry: extern "C" fn(u64), _arg: u64) -> u64 {
		todo!("aarch64 context switch (M116)")
	}
	// The active address-space token (TTBR0 on aarch64; kept named `cr3` for the
	// portable contract).
	pub fn read_cr3() -> u64 {
		todo!("aarch64 TTBR0 (M116)")
	}
	pub unsafe fn write_cr3(_ttbr: u64) {
		todo!("aarch64 TTBR0 (M116)")
	}
}

// ------------------------------------------------------------------ percpu
pub mod percpu {
	pub struct PerCpu;

	impl PerCpu {
		pub fn cpu_id(&self) -> u32 {
			todo!("aarch64 per-CPU (M116)")
		}
		pub fn lapic_id(&self) -> u32 {
			todo!("aarch64 per-CPU (M116)")
		}
	}

	pub fn allocate(_count: usize) {
		todo!("aarch64 per-CPU (M116)")
	}
	pub fn init(_cpu_id: usize, _mpidr: u32) {
		todo!("aarch64 per-CPU (M116)")
	}
	pub fn this_cpu() -> &'static PerCpu {
		todo!("aarch64 per-CPU (M116)")
	}
	pub fn set_kernel_rsp(_value: u64) {
		todo!("aarch64 per-CPU (M116)")
	}
	pub fn set_tss_rsp0_slot(_addr: u64) {
		todo!("aarch64 per-CPU (M116)")
	}
	pub fn set_rsp0(_value: u64) {
		todo!("aarch64 per-CPU (M116)")
	}
	pub fn in_user_syscall() -> bool {
		todo!("aarch64 per-CPU (M116)")
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
		todo!("aarch64 GIC (M116)")
	}
}

// -------------------------------------------------------------------- apic
// (the aarch64 interrupt controller is the GIC; the module keeps the portable
// `apic` name for the contract until the ports rename it.)
pub mod apic {
	pub fn local_id() -> u32 {
		todo!("aarch64 GIC/MPIDR (M116)")
	}
	pub fn eoi() {
		todo!("aarch64 GIC EOI (M116)")
	}
	pub fn send_wake_ipi(_dest: u32) {
		todo!("aarch64 GIC SGI (M116)")
	}
	pub fn send_init(_dest: u32) {
		todo!("aarch64 PSCI wake (M116)")
	}
	pub fn send_startup(_dest: u32, _vector: u8) {
		todo!("aarch64 PSCI wake (M116)")
	}
	pub fn ticks() -> u64 {
		todo!("aarch64 generic timer (M116)")
	}
	pub fn init() {
		todo!("aarch64 GIC + timer (M116)")
	}
	pub fn init_ap() {
		todo!("aarch64 GIC per-core (M116)")
	}
}

// --------------------------------------------------------------------- tsc
pub mod tsc {
	pub fn now() -> u64 {
		todo!("aarch64 CNTVCT (M116)")
	}
	pub fn init() {
		todo!("aarch64 CNTFRQ (M116)")
	}
	pub fn hz() -> u64 {
		todo!("aarch64 CNTFRQ (M116)")
	}
	pub fn cycles_to_ns(_cycles: u64) -> u64 {
		todo!("aarch64 timer scaling (M116)")
	}
}

// ------------------------------------------------------------------ ioapic
pub mod ioapic {
	pub fn route(_gsi: u32, _vector: u8, _dest: u32) {
		todo!("aarch64 GIC routing (M116)")
	}
	pub fn init() {
		todo!("aarch64 GIC distributor (M116)")
	}
	pub fn mask(_gsi: u32) {
		todo!("aarch64 GIC mask (M116)")
	}
}

// --------------------------------------------------------------------- rtc
pub mod rtc {
	pub fn read_unix() -> u64 {
		todo!("aarch64 PL031 RTC (M116)")
	}
}

// ------------------------------------------------------------------ random
pub mod random {
	pub fn fill(_buf: &mut [u8]) {
		todo!("aarch64 RNDR / DTB entropy (M116)")
	}
}

// ------------------------------------------------------------------ apboot
// (aarch64 wakes secondaries via PSCI CPU_ON, not a real-mode trampoline; these
// keep the portable names so smp.rs links until M116 replaces the wake path.)
pub mod apboot {
	pub fn trampoline_len() -> usize {
		0
	}
	pub unsafe fn install(_dst: *mut u8, _ttbr: u64, _entry: u64) {
		todo!("aarch64 PSCI wake (M116)")
	}
	pub unsafe fn set_stack(_dst: *mut u8, _stack_top: u64) {
		todo!("aarch64 PSCI wake (M116)")
	}
}

// ----------------------------------------------------------------- syscall
pub mod syscall {
	pub fn init() {
		todo!("aarch64 SVC wiring (M116)")
	}
	pub unsafe fn invoke(_num: u64, _a0: u64, _a1: u64, _a2: u64, _a3: u64) -> u64 {
		todo!("aarch64 SVC (M116)")
	}
}

// ---------------------------------------------------------------- usermode
pub mod usermode {
	pub const FAULT_PROBE_ADDR: u64 = 0x0dea_d000;

	pub unsafe fn enter(_entry: u64, _user_stack: u64, _arg: u64) {
		todo!("aarch64 EL0 entry (M116)")
	}
	pub fn exit_to_kernel() -> ! {
		todo!("aarch64 EL0 return (M116)")
	}
	pub fn program_bytes() -> &'static [u8] {
		todo!("aarch64 test program bytes (M116)")
	}
	pub fn program_fault_bytes() -> &'static [u8] {
		todo!("aarch64 test program bytes (M116)")
	}
	pub fn program_yield_bytes() -> &'static [u8] {
		todo!("aarch64 test program bytes (M116)")
	}
	pub fn program_nx_bytes() -> &'static [u8] {
		todo!("aarch64 test program bytes (M116)")
	}
	pub fn program_stack_probe_bytes() -> &'static [u8] {
		todo!("aarch64 test program bytes (M116)")
	}
	pub fn program_spin_bytes() -> &'static [u8] {
		todo!("aarch64 test program bytes (M116)")
	}
}

// --------------------------------------------------------------------- pci
// PCI config space is a bus standard; only the config-space ACCESS mechanism is
// arch-specific (x86 ports vs aarch64 ECAM MMIO). The device types + scan logic
// become portable in M116; for now the scans return empty so the tree links.
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
