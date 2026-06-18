pub mod apic;
pub mod context;
pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod msr;
pub mod paging;
pub mod percpu;
pub mod serial;
pub mod syscall;
pub mod tsc;
pub mod usermode;

use core::arch::asm;

// install the CPU descriptor tables (GDT + TSS, then IDT)
pub fn init() {
	gdt::init();
	idt::init();
}

// Bring up interrupt delivery and the periodic timer. Requires the memory
// subsystem (HHDM) to be up. Leaves interrupts disabled; call enable_interrupts.
pub fn init_interrupts() {
	interrupts::init();
	apic::init();
}

// Enable the fast `syscall` instruction on the current core (per-core MSRs).
pub fn init_syscalls() {
	syscall::init();
}

// Calibrate the time-stamp counter for fine-grained timing. Run on the BSP with
// interrupts disabled so no timer ISR distorts the calibration window.
pub fn init_tsc() {
	tsc::init();
}

// Initialize per-CPU data for the bootstrap processor (CPU id 0). The BSP's
// GDT/TSS and IDT are already loaded by init(); this only sets up its GS base.
pub fn init_bsp_percpu(lapic_id: u32) {
	percpu::init(0, lapic_id);
}

// Full per-core bring-up for an application processor, run on that core: load
// the shared descriptor tables, set up per-CPU data, and enable its LAPIC.
pub fn init_ap(cpu_id: usize, lapic_id: u32) {
	gdt::load_ap(cpu_id);
	idt::load();
	percpu::init(cpu_id, lapic_id);
	apic::init_ap();
	syscall::init();
}

// enable / disable maskable interrupts on the current core
pub fn enable_interrupts() {
	unsafe {
		asm!("sti", options(nomem, nostack, preserves_flags));
	}
}

#[allow(dead_code)]
pub fn disable_interrupts() {
	unsafe {
		asm!("cli", options(nomem, nostack, preserves_flags));
	}
}

// True if maskable interrupts are currently enabled on the running core (RFLAGS.IF).
pub fn interrupts_enabled() -> bool {
	let flags: u64;
	unsafe {
		asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags));
	}
	flags & (1 << 9) != 0
}

// halt the kernel in an infinite loop, halting on each iteration
pub fn halt_loop() -> ! {
	loop {
		unsafe {
			asm!("hlt", options(nomem, nostack, preserves_flags));
		}
	}
}

// exit QEMU via the isa-debug-exit device (test harness only)
#[cfg(test)]
pub fn exit_qemu(success: bool) -> ! {
	let code: u32 = if success { 0x10 } else { 0x11 };
	unsafe {
		asm!("out dx, eax", in("dx") 0xf4u16, in("eax") code, options(nomem, nostack, preserves_flags));
	}
	halt_loop()
}
