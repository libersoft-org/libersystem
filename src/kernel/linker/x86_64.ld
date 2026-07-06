/* Linker script x86_64 (own UEFI loader, higher-half, static/non-PIE) */

OUTPUT_FORMAT(elf64-x86-64)
ENTRY(kmain)

PHDRS
{
	text    PT_LOAD;
	rodata  PT_LOAD;
	data    PT_LOAD;
}

SECTIONS
{
	/* higher-half: top 2 GB, matches the kernel code model */
	. = 0xffffffff80000000;

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
