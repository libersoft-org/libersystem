// aarch64 direct-boot entry (M116 bring-up).
//
// QEMU `-machine virt -kernel <elf>` enters the ELF entry `_start` with the MMU
// off, at EL1 (or EL2), on an undefined stack, with x0 = the DTB physical
// address. `_start` (below, in `.text.boot` so it lands first) sets up the boot
// stack, zeroes the BSS, then calls `aarch64_main` with the DTB pointer.
//
// This is the M0-equivalent for the new architecture: it brings up the PL011
// serial console and reports in, then halts. The real port - the MMU + higher
// half, the VBAR_EL1 vectors, the GIC + generic timer, PSCI SMP, the SVC syscall
// path, and routing through the portable `kmain` - fills in from here in M116.

use core::arch::global_asm;

global_asm!(
	r#"
.section .text.boot, "ax"
.global _start
_start:
	// x0 holds the DTB pointer QEMU passed; preserve it across BSS zeroing.
	mov     x19, x0
	// Set up the boot stack (grows down from the linker-reserved top).
	adrp    x1, __boot_stack_top
	add     x1, x1, :lo12:__boot_stack_top
	mov     sp, x1
	// Zero the BSS: [__bss_start, __bss_end).
	adrp    x0, __bss_start
	add     x0, x0, :lo12:__bss_start
	adrp    x1, __bss_end
	add     x1, x1, :lo12:__bss_end
0:
	cmp     x0, x1
	b.hs    1f
	str     xzr, [x0], #8
	b       0b
1:
	// aarch64_main(dtb) - never returns.
	mov     x0, x19
	bl      aarch64_main
2:
	wfe
	b       2b
"#
);

#[unsafe(no_mangle)]
extern "C" fn aarch64_main(dtb: u64) -> ! {
	super::serial::init();

	// Current exception level (CurrentEL bits [3:2]).
	let current_el: u64;
	unsafe {
		core::arch::asm!("mrs {}, CurrentEL", out(reg) current_el, options(nomem, nostack, preserves_flags));
	}
	let el = (current_el >> 2) & 0b11;

	crate::serial_println!("{} kernel is starting ...", crate::product::NAME);
	crate::serial_println!("arch: aarch64 | EL{el} | DTB {dtb:#x}");

	// Turn on the MMU with a boot identity map. Serial keeps working across the
	// enable because the UART's device page is mapped and the map is identity.
	unsafe {
		super::paging::init_boot_mmu();
	}
	crate::serial_println!("aarch64: MMU on (identity map, 4 kB granule)");

	// Prove translation works: the UART (device) and this code (Normal RAM) walk
	// back to their own physical addresses, and a RAM read-back survives the MMU.
	let uart = super::paging::translate(0x0900_0000).unwrap_or(0);
	let code = super::paging::translate(aarch64_main as *const () as u64).unwrap_or(0);
	crate::serial_println!("aarch64: translate(uart 0x9000000) = {uart:#x}");
	crate::serial_println!("aarch64: translate(&aarch64_main)   = {code:#x}");

	static mut PROBE: u64 = 0;
	let ram_ok = unsafe {
		let p = &raw mut PROBE;
		core::ptr::write_volatile(p, 0xA5A5_1234_5678_C3C3);
		core::ptr::read_volatile(p) == 0xA5A5_1234_5678_C3C3
	};
	crate::serial_println!("aarch64: post-MMU RAM read/write = {}", if ram_ok { "ok" } else { "FAIL" });

	// Install the EL1 exception vectors (VBAR_EL1) and prove the synchronous
	// handler catches a fault: `brk #0` traps into the vector table, which
	// decodes ESR/FAR/ELR and reports before halting.
	super::exceptions::init_vectors();
	crate::serial_println!("aarch64: VBAR_EL1 exception vectors installed");
	crate::serial_println!("aarch64: triggering a test exception (brk #0) ...");
	unsafe {
		core::arch::asm!("brk #0");
	}

	// The exception handler halts, so this is unreachable.
	super::halt_loop()
}
