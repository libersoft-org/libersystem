/* Linker script for ring-3 (EL0) userspace programs on aarch64.
 *
 * The kernel ELF loader maps each PT_LOAD segment at its link-time p_vaddr and
 * applies no relocations, so the program is linked at a fixed low-half base with
 * each segment page-aligned (one segment never shares a page with another). This
 * mirrors user.ld (x86_64) with the AArch64 object format. */

OUTPUT_FORMAT(elf64-littleaarch64)
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
