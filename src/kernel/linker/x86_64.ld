/* Linker script x86_64 (Limine, higher-half) */

OUTPUT_FORMAT(elf64-x86-64)
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
	/* higher-half: top 2 GB, matches the kernel code model */
	. = 0xffffffff80000000;

	.text : {
		*(.text .text.*)
	} :text

	. = ALIGN(CONSTANT(MAXPAGESIZE));

	.rodata : {
		*(.rodata .rodata.*)
	} :rodata

	/* Limine requests must be discoverable via the start/end markers */
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
