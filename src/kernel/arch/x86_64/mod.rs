pub mod apboot;
pub mod apic;
pub mod context;
pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod ioapic;
pub mod msr;
pub mod paging;
pub mod pci;
pub mod percpu;
mod pit;
mod port;
pub mod random;
pub mod rtc;
pub mod serial;
pub mod syscall;
pub mod tsc;
pub mod usermode;

use core::arch::asm;

// Shared programmed-I/O port helpers (the reset and power-off paths use them).
use self::port::{inb, outb, outw};

// install the CPU descriptor tables (GDT + TSS, then IDT), and turn on no-execute
// enforcement before any NX-flagged mapping exists (the heap window is the first),
// plus supervisor-mode access/execution prevention (SMAP + SMEP) before any
// USER-flagged mapping exists
pub fn init() {
	context::enable_fpu();
	gdt::init();
	idt::init();
	paging::enable_nx();
	paging::enable_smap_smep();
}

// Bring up interrupt delivery and the periodic timer. Requires the memory
// subsystem (HHDM) to be up. Leaves interrupts disabled; call enable_interrupts.
pub fn init_interrupts() {
	interrupts::init();
	apic::init();
	ioapic::init();
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
// GDT/TSS and IDT are already loaded by init(); this only sets up its GS base
// and records where its TSS.RSP0 slot lives for per-thread ring-3 stacks.
pub fn init_bsp_percpu(lapic_id: u32) {
	percpu::init(0, lapic_id);
	percpu::set_tss_rsp0_slot(gdt::rsp0_slot_addr());
}

// Full per-core bring-up for an application processor, run on that core: enable
// no-execute and SMAP/SMEP first (the shared page tables already carry NX-flagged
// and USER-flagged leaves), load the shared descriptor tables, set up per-CPU
// data, and enable its LAPIC.
pub fn init_ap(cpu_id: usize, lapic_id: u32) {
	context::enable_fpu();
	paging::enable_nx();
	paging::enable_smap_smep();
	gdt::load_ap(cpu_id);
	idt::load();
	percpu::init(cpu_id, lapic_id);
	percpu::set_tss_rsp0_slot(gdt::rsp0_slot_addr());
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

// Halt the current core until the next interrupt, for the idle loop. `sti` enables
// interrupts and, by the one-instruction delay before they take effect, lets the
// following `hlt` execute first - so a wakeup interrupt cannot slip in between the two
// and be lost. This yields the physical CPU instead of busy-spinning, which under
// virtualization is essential: a spinning vCPU steals host time from the cores doing
// real work and from the host's own device emulation.
pub fn idle_halt() {
	unsafe {
		asm!("sti; hlt", options(nomem, nostack, preserves_flags));
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

// Write the CPU's model / brand string into `out`, returning the byte count. The
// CPUID brand string (leaves 0x8000_0002..0x8000_0004, 48 bytes) when the CPU
// advertises it - under KVM this is the host CPU's real model - trimmed of the
// padding CPUID leaves; otherwise the 12-byte vendor string (CPUID leaf 0). Feeds
// the `lscpu` model field.
pub fn cpu_brand(out: &mut [u8]) -> usize {
	use core::arch::x86_64::__cpuid;
	{
		if __cpuid(0x8000_0000).eax >= 0x8000_0004 {
			let mut raw: [u8; 48] = [0u8; 48];
			for (i, &leaf) in [0x8000_0002u32, 0x8000_0003, 0x8000_0004].iter().enumerate() {
				let r = __cpuid(leaf);
				for (j, &word) in [r.eax, r.ebx, r.ecx, r.edx].iter().enumerate() {
					raw[i * 16 + j * 4..i * 16 + j * 4 + 4].copy_from_slice(&word.to_le_bytes());
				}
			}
			let end: usize = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
			return copy_trimmed(&raw[..end], out);
		}
		// No brand string: the 12-byte vendor id (CPUID 0: EBX, EDX, ECX).
		let r = __cpuid(0);
		let mut vendor: [u8; 12] = [0u8; 12];
		vendor[0..4].copy_from_slice(&r.ebx.to_le_bytes());
		vendor[4..8].copy_from_slice(&r.edx.to_le_bytes());
		vendor[8..12].copy_from_slice(&r.ecx.to_le_bytes());
		copy_trimmed(&vendor, out)
	}
}

// Copy `src` into `out` (up to its length) with leading and trailing ASCII spaces
// trimmed, returning the copied length.
fn copy_trimmed(src: &[u8], out: &mut [u8]) -> usize {
	let start: usize = src.iter().position(|&b| b != b' ').unwrap_or(src.len());
	let mut end: usize = src.len();
	while end > start && src[end - 1] == b' ' {
		end -= 1;
	}
	let trimmed: &[u8] = &src[start..end];
	let n: usize = trimmed.len().min(out.len());
	out[..n].copy_from_slice(&trimmed[..n]);
	n
}

// exit QEMU via the isa-debug-exit device (test harness only)
#[cfg(test)]
pub fn exit_qemu(success: bool) -> ! {
	// Flush any queued serial output (the test report) before QEMU exits.
	serial::flush_sync();
	let code: u32 = if success { 0x10 } else { 0x11 };
	unsafe {
		asm!("out dx, eax", in("dx") 0xf4u16, in("eax") code, options(nomem, nostack, preserves_flags));
	}
	halt_loop()
}
