/* Linker script aarch64 - direct-boot bring-up (M116).
 *
 * QEMU `-machine virt -kernel <elf>` loads the ELF by its physical addresses
 * into DRAM (which starts at 0x4000_0000) with the MMU off and enters at the ELF
 * entry (`_start`) with x0 = the DTB physical address. The image is therefore
 * linked static (non-PIE) at a low identity base (vaddr == paddr) just above the
 * DRAM base. The higher-half virtual map + the own UEFI loader hand-off come once
 * the MMU is up later in M116. */

OUTPUT_FORMAT(elf64-littleaarch64)
ENTRY(_start)

PHDRS
{
	text	PT_LOAD FLAGS(5); /* RX */
	rodata	PT_LOAD FLAGS(4); /* R  */
	data	PT_LOAD FLAGS(6); /* RW */
}

SECTIONS
{
	. = 0x40080000;

	.text : {
		KEEP(*(.text.boot))
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

	.bss (NOLOAD) : {
		__bss_start = .;
		*(.bss .bss.*)
		*(COMMON)
		. = ALIGN(16);
		__bss_end = .;
		/* boot stack: reserved RAM (uninitialized) right above the BSS. */
		. = ALIGN(16);
		__boot_stack_bottom = .;
		. += 0x10000; /* 64 KiB */
		__boot_stack_top = .;
	} :data

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
