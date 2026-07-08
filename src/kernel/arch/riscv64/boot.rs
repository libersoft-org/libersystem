// riscv64 higher-half boot entry (M117).
//
// QEMU `-machine virt` boots OpenSBI in M-mode; OpenSBI jumps to the kernel ELF
// entry `_start` in S-mode with the MMU off, a0 = hartid, a1 = DTB pointer. `_start`
// lives in the low, identity-linked `.text.boot` section: it is position-independent
// (PC-relative until paging is on), builds one Sv39 root table in the reserved
// `__boot_tables` page (a low identity megapage for the hand-off plus a high direct
// map of physical memory at KERNEL_VA_OFFSET), enables paging through SATP, then
// loads the higher-half address of `riscv64_main` (and the high boot stack) from
// `.data.boot` literals and jumps into the higher half. From there the kernel runs
// entirely from the high half, leaving the low half free for userspace.
//
// Sv39 layout: one root table (level 2), 512 entries, each a 1 GiB megapage leaf.
// Index 2 = the low identity (physical 0x8000_0000, covering the boot stub); index
// 256 = KERNEL_VA_OFFSET (physical 0), and 256 + N maps physical N GiB - so the
// kernel loaded at 0x8020_0000 (= 2 GiB) is reachable at its link VA via index 258.

use core::arch::global_asm;

global_asm!(
	r#"
.section .data.boot, "a"
.balign 8
.Lp_main:      .quad riscv64_main
.Lp_stack_top: .quad __boot_stack_top
.Lp_bss_start: .quad __bss_start
.Lp_bss_end:   .quad __bss_end

.section .text.boot, "ax"
.global _start
_start:
	mv      s0, a0                  // save hartid
	mv      s1, a1                  // save DTB pointer

	// t0 = __boot_tables (its low identity address; SATP uses this physical page).
	la      t0, __boot_tables

	// Zero the root table (4096 bytes).
	mv      t1, t0
	li      t2, 4096
	add     t2, t1, t2
0:
	sd      zero, 0(t1)
	addi    t1, t1, 8
	bltu    t1, t2, 0b

	// Megapage leaf flags: V|R|W|X|G|A|D = 0xEF. A 1 GiB leaf for physical N GiB is
	// ((N GiB >> 12) << 10) | flags; each 1 GiB step adds 0x1000_0000 to the PTE.

	// Low identity: root[2] = leaf @ physical 0x8000_0000 (2 GiB) so the boot stub
	// (at 0x8020_0000) keeps executing after paging turns on. PPN part = 0x2000_0000.
	li      t1, 0x200000EF
	sd      t1, (2*8)(t0)

	// High direct map: root[256 + i] = leaf @ physical i GiB, i = 0..7 (0..8 GiB),
	// covering the low MMIO (i=0) and the DRAM at 0x8000_0000 (i=2). root[256] is
	// KERNEL_VA_OFFSET (physical 0).
	li      t3, 256
	slli    t3, t3, 3               // byte offset of root[256]
	add     t3, t0, t3
	li      t1, 0xEF                // leaf for physical 0
	li      t4, 0x10000000          // per-1-GiB PPN step
	li      t5, 8                   // map 8 GiB
	li      t6, 0
1:
	sd      t1, 0(t3)
	add     t1, t1, t4
	addi    t3, t3, 8
	addi    t6, t6, 1
	bltu    t6, t5, 1b

	// SATP = (8 << 60) | (root_phys >> 12): mode 8 = Sv39.
	srli    t1, t0, 12
	li      t2, 8
	slli    t2, t2, 60
	or      t1, t1, t2
	sfence.vma
	csrw    satp, t1
	sfence.vma

	// Switch to the higher-half boot stack (its VA is a .data.boot literal).
	la      t0, .Lp_stack_top
	ld      sp, 0(t0)

	// Zero the higher-half BSS.
	la      t0, .Lp_bss_start
	ld      t1, 0(t0)
	la      t0, .Lp_bss_end
	ld      t2, 0(t0)
2:
	bgeu    t1, t2, 3f
	sd      zero, 0(t1)
	addi    t1, t1, 8
	j       2b
3:
	// Branch into the higher half: riscv64_main(hartid, dtb).
	la      t0, .Lp_main
	ld      t0, 0(t0)
	mv      a0, s0
	mv      a1, s1
	jr      t0
4:
	wfi
	j       4b
"#
);

#[unsafe(no_mangle)]
extern "C" fn riscv64_main(hartid: u64, dtb: u64) -> ! {
	super::serial::init();
	crate::serial_println!("riscv64: hello from the kernel (higher half)");
	crate::serial_println!("riscv64: hart {hartid}, DTB @ {dtb:#x}");

	// Prove the higher half is live: read back the VA this function is linked at.
	let here = riscv64_main as usize;
	crate::serial_println!("riscv64: _main VA = {here:#x} (KERNEL_VA_OFFSET high half)");

	// Confirm a high-half RAM read/write works through the Sv39 direct map.
	static mut PROBE: u64 = 0;
	let ok = unsafe {
		let p = &raw mut PROBE;
		core::ptr::write_volatile(p, 0xA5A5_1234_5678_C3C3);
		core::ptr::read_volatile(p) == 0xA5A5_1234_5678_C3C3
	};
	crate::serial_println!("riscv64: post-MMU RAM read/write = {}", if ok { "ok" } else { "FAIL" });

	crate::serial_println!("riscv64: M117 bring-up increment 1 (boot + Sv39 + SBI serial) - OK, halting");
	super::halt_loop()
}
