/* Linker script for the riscv64 loader built as a static PIE on the ELF target
 * riscv64gc-unknown-none-elf. It lays the hand-written PE/COFF header (src/arch/
 * riscv64/head.rs, section .head) at image offset 0, keeps file offset == RVA (so
 * `llvm-objcopy -O binary` yields a valid PE), and defines the boundary symbols the
 * PE header's size fields and the self-relocation stub reference. See head.rs.
 *
 * Section alignment == file alignment == 4 KiB. The image is linked at base 0; the
 * only relocations are R_RISCV_RELATIVE (captured in .rela.dyn), which the entry stub
 * applies against the firmware's load base before calling efi_main.
 */

ENTRY(_pe_entry)

SECTIONS {
	. = 0;

	/* PE/COFF header: "MZ" + PE + COFF + PE32+ optional header + section table,
	 * padded to 4 KiB so the headers occupy exactly SizeOfHeaders. */
	.head : {
		KEEP(*(.head))
	}

	. = ALIGN(0x1000);
	_pe_text_start = .;
	.text : {
		KEEP(*(.text.head))          /* _pe_entry must be first in .text */
		*(.text .text.*)
		*(.rodata .rodata.*)
		*(.srodata .srodata.*)
	}
	. = ALIGN(0x1000);
	_pe_text_end = .;

	_pe_data_start = .;
	.data : {
		*(.data .data.*)
		*(.sdata .sdata.*)
		*(.got) *(.got.*)
		*(.sbss .sbss.*) *(.bss .bss.*) *(COMMON)

		/* Dummy PE base-relocation block: one no-op IMAGE_REL_BASED_ABSOLUTE entry
		 * (plus padding), so the firmware treats the image as relocatable but
		 * relocates nothing - the entry stub self-relocates via .rela.dyn instead. */
		. = ALIGN(4);
		_pe_reloc_start = .;
		LONG(0);                     /* PageRVA */
		LONG(12);                    /* BlockSize (8 header + 2 * 2-byte entries) */
		SHORT(0);                    /* IMAGE_REL_BASED_ABSOLUTE */
		SHORT(0);                    /* padding entry (also ABSOLUTE) */
		_pe_reloc_end = .;
	}

	/* Dynamic relocations (R_RISCV_RELATIVE only) the entry stub applies against the
	 * firmware's load base. Its own SHT_RELA output section (cannot be folded into
	 * the PROGBITS .data above), placed contiguously so it stays inside the image. */
	. = ALIGN(8);
	__rela_start = .;
	.rela.dyn : {
		*(.rela.dyn) *(.rela .rela.*) *(.rela.plt)
	}
	__rela_end = .;

	. = ALIGN(0x1000);
	_pe_data_end = .;
	_pe_image_end = .;

	__global_pointer$ = _pe_data_start + 0x800;

	/DISCARD/ : {
		/* Dynamic-linking metadata a PIE emits: the self-relocation stub reads
		 * .rela.dyn directly (RELATIVE only), so none of this is needed at runtime. */
		*(.dynamic) *(.dynsym) *(.dynstr) *(.hash) *(.gnu.hash) *(.gnu.version*)
		*(.interp)
		*(.eh_frame) *(.eh_frame_hdr)
		*(.comment)
		*(.riscv.attributes)
		*(.note) *(.note.*)
	}
}
