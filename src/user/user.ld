/* Linker script for ring-3 userspace programs.
 *
 * The kernel ELF loader maps each PT_LOAD segment at its link-time p_vaddr and
 * applies no relocations, so the program is linked at a fixed low-half base with
 * each segment page-aligned (one segment never shares a page with another). */

OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

PHDRS
{
	text   PT_LOAD;
	rodata PT_LOAD;
	data   PT_LOAD;
}

SECTIONS
{
	/* low half, well clear of the user stack the kernel maps near the 2 GB line */
	. = 0x200000;

	.text : {
		*(.text .text.*)
	} :text

	. = ALIGN(CONSTANT(MAXPAGESIZE));

	.rodata : {
		*(.rodata .rodata.*)
		/* THE DRIVER PROTOCOL NOTE, KEPT ON PURPOSE.
		 *
		 * It declares the bring-up protocol version a driver speaks, and it is read from the
		 * PACKAGED artifact before the driver is given a device - so it has to survive two things
		 * that would otherwise remove it: the `/DISCARD/` below, which throws away every `.note.*`
		 * section, and `llvm-strip --strip-all` in `mkpackages`. The section name is one the
		 * discard does not match, `KEEP` stops `--gc-sections` taking it, and living inside
		 * `.rodata` makes it SHF_ALLOC, which is what `--strip-all` leaves alone.
		 *
		 * A note that vanished would fail SILENTLY: every driver would read as having none, which
		 * is indistinguishable from a driver that declares no version. */
		KEEP(*(.liberdrv.note))
	} :rodata

	. = ALIGN(CONSTANT(MAXPAGESIZE));

	.data : {
		*(.data .data.*)
	} :data

	.bss : {
		*(.bss .bss.*)
		*(COMMON)
	} :data

	/DISCARD/ : {
		*(.eh_frame*)
		*(.note .note.*)
	}
}
