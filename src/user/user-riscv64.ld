/* Linker script for ring-3 (U-mode) userspace programs on riscv64.
 *
 * The kernel ELF loader maps each PT_LOAD segment at its link-time p_vaddr and
 * applies no relocations, so the program is linked at a fixed low-half base with
 * each segment page-aligned (one segment never shares a page with another). This
 * mirrors user.ld (x86_64) with the RISC-V object format and the small-data
 * (.sdata/.sbss/.srodata) sections the RV toolchain emits. */

OUTPUT_FORMAT(elf64-littleriscv)
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
		*(.srodata .srodata.*)
	} :rodata

	. = ALIGN(CONSTANT(MAXPAGESIZE));

	.data : {
		*(.data .data.*)
		*(.sdata .sdata.*)
	} :data

	.bss : {
		*(.bss .bss.*)
		*(.sbss .sbss.*)
		*(COMMON)
	} :data

	/DISCARD/ : {
		*(.eh_frame*)
		*(.note .note.*)
		*(.riscv.attributes)
	}
}
