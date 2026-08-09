/* Linker script aarch64 - higher-half kernel.
 *
 * QEMU `-machine virt -kernel <elf>` loads the ELF by its physical addresses into
 * DRAM (base 0x4000_0000), MMU off, and enters the ELF entry (`_start`) at that
 * physical address. So the boot stub lives in a low, identity-linked section
 * (`.boot` at physical 0x4008_0000): it is position-independent, builds the boot
 * page tables, turns on the MMU (TTBR0 = low identity for the hand-off, TTBR1 =
 * the high kernel / direct map), then branches into the higher half.
 *
 * Everything else is linked at KOFF + physical (KOFF = 0xFFFF_0000_0000_0000), so
 * the kernel runs from the higher half; TTBR0 is then free for userspace. Each
 * high section's load address (AT) is its virtual address minus KOFF, so QEMU
 * still loads it physically just above the boot stub. */

OUTPUT_FORMAT(elf64-littleaarch64)
ENTRY(_start)

KOFF = 0xFFFF000000000000;

PHDRS
{
	boot	PT_LOAD FLAGS(7); /* RWX - the low boot stub + boot tables */
	text	PT_LOAD FLAGS(5); /* RX  */
	rodata	PT_LOAD FLAGS(4); /* R   */
	data	PT_LOAD FLAGS(6); /* RW  */
}

SECTIONS
{
	/* Low, identity-linked boot section (physical == virtual). */
	. = 0x40080000;

	.boot : {
		KEEP(*(.text.boot))
		*(.data.boot)
		. = ALIGN(4096);
		__boot_tables = .;
		. += 3 * 4096; /* BOOT_L1, BOOT_L0_LOW, BOOT_L0_HIGH */
		. = ALIGN(4096);
	} :boot
	__boot_phys_end = .;

	/* Higher-half kernel: virtual = KOFF + physical (load address). */
	. = __boot_phys_end + KOFF;

	.text : AT(ADDR(.text) - KOFF) {
		*(.text .text.*)
	} :text

	. = ALIGN(CONSTANT(MAXPAGESIZE));

	.rodata : AT(ADDR(.rodata) - KOFF) {
		*(.rodata .rodata.*)
	} :rodata

	/* Where a kernel instruction is allowed to fault, and where it resumes.
	 *
	 * Emitted by the user-copy routines themselves (see kernel/extable.rs), so a routine that is
	 * compiled out takes its entries with it and nothing has to register anything at boot. Read-only
	 * data: the table is generated at link time and never written.
	 */
	. = ALIGN(8);
	.extable : AT(ADDR(.extable) - KOFF) {
		__extable_start = .;
		KEEP(*(.extable))
		__extable_end = .;
	} :rodata

	. = ALIGN(CONSTANT(MAXPAGESIZE));

	.data : AT(ADDR(.data) - KOFF) {
		*(.data .data.*)
	} :data

	.bss (NOLOAD) : AT(ADDR(.bss) - KOFF) {
		__bss_start = .;
		*(.bss .bss.*)
		*(COMMON)
		. = ALIGN(16);
		__bss_end = .;
		/* boot stack: reserved RAM (uninitialized), higher-half addresses. */
		. = ALIGN(16);
		__boot_stack_bottom = .;
		/* 256 KiB. Was 64 KiB, which the boot path fitted in comfortably until the test suite
		   began formatting a LiberFS volume in the kernel to build its storage fixture: that
		   walks a B-tree and a transaction log, and it overflowed into .bss. The symptom was
		   memory corruption rather than a fault - a `serial_println!` printing megabytes of
		   memory, and data aborts at addresses that made no sense - so the cost of getting this
		   wrong is high and the 192 KiB of reserved image is cheap. */
		. += 0x40000;
		__boot_stack_top = .;
	} :data

	/* End of the loaded image; free DRAM (physical) for the frame allocator. */
	. = ALIGN(4096);
	__kernel_end = .;

	/DISCARD/ : {
		*(.eh_frame*)
		*(.note .note.*)
		*(.dynamic)
		*(.dynsym .dynstr .hash .gnu.hash)
		*(.got .got.plt)
		*(.rela .rela.*)
		*(.interp)
	}
}
