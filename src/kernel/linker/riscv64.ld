/* Linker script riscv64 - higher-half kernel.
 *
 * QEMU `-machine virt` boots OpenSBI (M-mode) which jumps to the kernel ELF entry
 * (`_start`) in S-mode, MMU off, a0 = hartid, a1 = DTB pointer. QEMU `-kernel`
 * loads the ELF by its physical addresses into DRAM (base 0x8000_0000, OpenSBI
 * takes the first 2 MiB), so the boot stub lives in a low, identity-linked
 * `.boot` section at physical 0x8020_0000: it is position-independent, builds the
 * boot Sv39 table, turns on paging (SATP), then jumps into the higher half.
 *
 * Everything else is linked at KOFF + physical (KOFF = 0xFFFF_FFC0_0000_0000, the
 * base of the Sv39 high canonical half), so the kernel runs from the higher half
 * and the low half is free for userspace. Each high section's load address (AT) is
 * its virtual address minus KOFF, so QEMU still loads it physically just above the
 * boot stub. */

OUTPUT_FORMAT(elf64-littleriscv)
ENTRY(_start)

KOFF = 0xFFFFFFC000000000;

PHDRS
{
	boot	PT_LOAD FLAGS(7); /* RWX - the low boot stub + boot table */
	text	PT_LOAD FLAGS(5); /* RX  */
	rodata	PT_LOAD FLAGS(4); /* R   */
	data	PT_LOAD FLAGS(6); /* RW  */
}

SECTIONS
{
	/* Low, identity-linked boot section (physical == virtual). */
	. = 0x80200000;

	.boot : {
		KEEP(*(.text.boot.entry))
		KEEP(*(.text.boot))
		*(.data.boot)
		. = ALIGN(4096);
		__boot_tables = .;
		. += 4096; /* one Sv39 root table (512 * 8 B) */
		. = ALIGN(4096);
	} :boot
	__boot_phys_end = .;

	/* Higher-half kernel: virtual = KOFF + physical (load address). */
	. = __boot_phys_end + KOFF;

	.text : AT(ADDR(.text) - KOFF) {
		*(.text .text.*)
	} :text

	. = ALIGN(4096);

	.rodata : AT(ADDR(.rodata) - KOFF) {
		*(.rodata .rodata.*)
		*(.srodata .srodata.*)
	} :rodata

	. = ALIGN(4096);

	.data : AT(ADDR(.data) - KOFF) {
		*(.data .data.*)
		*(.sdata .sdata.*)
	} :data

	.bss (NOLOAD) : AT(ADDR(.bss) - KOFF) {
		__bss_start = .;
		*(.bss .bss.*)
		*(.sbss .sbss.*)
		*(COMMON)
		. = ALIGN(16);
		__bss_end = .;
		/* boot stack: reserved RAM (uninitialized), higher-half addresses. */
		. = ALIGN(16);
		__boot_stack_bottom = .;
		. += 0x10000; /* 64 KiB */
		__boot_stack_top = .;
	} :data

	/* End of the loaded image; a higher-half VA (mask off KOFF for the physical
	   address). The frame allocator seeds from free DRAM above it. */
	__kernel_end = .;

	/DISCARD/ : {
		*(.eh_frame)
		*(.eh_frame_hdr)
		*(.note .note.*)
		*(.comment)
		*(.riscv.attributes)
	}
}
