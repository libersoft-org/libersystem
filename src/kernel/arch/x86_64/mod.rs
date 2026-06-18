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

// Reboot the machine. Pulses the PCH reset-control register (0xCF9), then the
// 8042 keyboard-controller reset line; on real hardware one of these resets the
// CPU, and QEMU treats either as a machine reset (unless QEMU was started with
// -no-reboot, in which case it exits instead). Halts if both are ignored.
pub fn reset() -> ! {
	unsafe {
		// 0xCF9: set SYS_RST, then pulse RST_CPU|SYS_RST (the rising edge resets).
		outb(0xcf9, 0x02);
		outb(0xcf9, 0x06);
		// 8042: drain the input buffer (status bit 1), then pulse the reset line.
		let mut spins: u32 = 0;
		while inb(0x64) & 0x02 != 0 && spins < 1_000_000 {
			core::hint::spin_loop();
			spins += 1;
		}
		outb(0x64, 0xfe);
	}
	halt_loop()
}

// Power the machine off via ACPI S5 (soft-off): write SLP_EN to the PM1a control
// register. QEMU's q35 (ICH9) decodes it at 0x604, i440fx (PIIX4) at 0xB004; 0x600
// is written too as a harmless fallback. (Real-hardware ACPI comes later.)
pub fn poweroff() -> ! {
	unsafe {
		outw(0x604, 0x2000);
		outw(0xb004, 0x2000);
		outw(0x600, 0x2000);
	}
	halt_loop()
}

// single-byte / single-word port I/O for the reset and power-off paths
#[inline]
unsafe fn outb(port: u16, value: u8) {
	asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

#[inline]
unsafe fn outw(port: u16, value: u16) {
	asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
	let value: u8;
	asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
	value
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
