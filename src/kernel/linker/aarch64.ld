/* Linker script aarch64 (Limine, higher-half)
   Stub prepared for a future port. Used only when building for aarch64. */

OUTPUT_FORMAT(elf64-littleaarch64)
ENTRY(kmain)

PHDRS
{
	text    PT_LOAD;
	rodata  PT_LOAD;
	data    PT_LOAD;
	dynamic PT_DYNAMIC;
}

SECTIONS
{
	. = 0xffffffff80000000;

	.text : {
		*(.text .text.*)
	} :text

	. = ALIGN(CONSTANT(MAXPAGESIZE));

	.rodata : {
		*(.rodata .rodata.*)
	} :rodata

	.limine_requests : {
		KEEP(*(.limine_requests_start))
		KEEP(*(.limine_requests))
		KEEP(*(.limine_requests_end))
	} :rodata

	. = ALIGN(CONSTANT(MAXPAGESIZE));

	.data : {
		*(.data .data.*)
	} :data

	.dynamic : {
		*(.dynamic)
	} :data :dynamic

	.bss : {
		*(.bss .bss.*)
		*(COMMON)
	} :data

	/DISCARD/ : {
		*(.eh_frame*)
		*(.note .note.*)
	}
}
