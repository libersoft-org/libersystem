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

	/* Where a kernel instruction is allowed to fault, and where it resumes.
	 *
	 * Emitted by the user-copy routines themselves (see kernel/extable.rs), so a routine that is
	 * compiled out takes its entries with it and nothing has to register anything at boot. Read-only
	 * data: the table is generated at link time and never written.
	 */
	. = ALIGN(8);
	.extable : {
		__extable_start = .;
		KEEP(*(.extable))
		__extable_end = .;
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
