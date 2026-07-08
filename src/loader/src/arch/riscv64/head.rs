// Hand-written PE/COFF header and self-relocation entry stub for the riscv64 loader.
//
// There is no built-in riscv64 UEFI rustc target, and rustc's object backend cannot
// emit a riscv64 PE/COFF image, so the loader is compiled for the ELF target
// `riscv64gc-unknown-none-elf` (as a static PIE) and this module prepends a minimal
// PE/COFF header - the same technique the Linux riscv64 EFI stub uses. The linker
// script (loader/riscv64-pe.ld) lays this `.head` section at file offset 0 and
// defines the symbols the header's size fields reference; `llvm-objcopy -O binary`
// then produces a flat image that is a valid EFI application U-Boot's boot manager
// loads as /EFI/BOOT/BOOTRISCV64.EFI.
//
// Because the image is a static PIE, the only relocations are R_RISCV_RELATIVE (in
// .rela.dyn). UEFI loads the image at an arbitrary base, so the entry stub relocates
// the image itself (adding the load base to each RELATIVE entry) before calling the
// architecture-neutral `efi_main`. The PE base-relocation directory points at a dummy
// no-op block, so the firmware performs no relocation of its own.
//
// The header field layout is the PE32+ format from the UEFI/PE specification. Size and
// offset fields are computed from linker-defined symbols (`_pe_*`, `__rela_*`) so they
// always match the final image layout.

use core::arch::global_asm;

global_asm!(
	r#"
// ---- PE/COFF header (file offset 0) --------------------------------------------
.section .head, "a"
.balign 8
.global _pe_image_base
_pe_image_base:
	.ascii "MZ"                       // IMAGE_DOS_SIGNATURE (also the first bytes UEFI checks)
	.org _pe_image_base + 0x3c        // e_lfanew lives at offset 0x3c in the DOS header
	.long _pe_pe_header - _pe_image_base

_pe_pe_header:
	.ascii "PE\0\0"                   // IMAGE_NT_SIGNATURE

// ---- COFF file header ----
	.short 0x5064                     // Machine = IMAGE_FILE_MACHINE_RISCV64
	.short 2                          // NumberOfSections (.text, .data)
	.long 0                           // TimeDateStamp
	.long 0                           // PointerToSymbolTable
	.long 0                           // NumberOfSymbols
	.short _pe_section_table - _pe_optional_header   // SizeOfOptionalHeader
	.short 0x020e                     // Characteristics: EXECUTABLE_IMAGE|LINE_NUMS_STRIPPED|LOCAL_SYMS_STRIPPED|DEBUG_STRIPPED

// ---- PE32+ optional header ----
_pe_optional_header:
	.short 0x020b                     // Magic = PE32+
	.byte 0                           // MajorLinkerVersion
	.byte 0                           // MinorLinkerVersion
	.long _pe_text_end - _pe_text_start          // SizeOfCode
	.long _pe_data_end - _pe_data_start          // SizeOfInitializedData
	.long 0                           // SizeOfUninitializedData
	.long _pe_entry - _pe_image_base             // AddressOfEntryPoint
	.long _pe_text_start - _pe_image_base        // BaseOfCode

	.quad 0                           // ImageBase
	.long 0x1000                      // SectionAlignment
	.long 0x1000                      // FileAlignment (== SectionAlignment: file offset == RVA)
	.short 0                          // MajorOperatingSystemVersion
	.short 0                          // MinorOperatingSystemVersion
	.short 0                          // MajorImageVersion
	.short 0                          // MinorImageVersion
	.short 0                          // MajorSubsystemVersion
	.short 0                          // MinorSubsystemVersion
	.long 0                           // Win32VersionValue
	.long _pe_image_end - _pe_image_base         // SizeOfImage
	.long _pe_header_end - _pe_image_base        // SizeOfHeaders
	.long 0                           // CheckSum
	.short 10                         // Subsystem = EFI_APPLICATION
	.short 0                          // DllCharacteristics
	.quad 0                           // SizeOfStackReserve
	.quad 0                           // SizeOfStackCommit
	.quad 0                           // SizeOfHeapReserve
	.quad 0                           // SizeOfHeapCommit
	.long 0                           // LoaderFlags
	.long 6                           // NumberOfRvaAndSizes

	.quad 0                           // [0] Export table
	.quad 0                           // [1] Import table
	.quad 0                           // [2] Resource table
	.quad 0                           // [3] Exception table
	.quad 0                           // [4] Certificate table
	.long _pe_reloc_start - _pe_image_base       // [5] Base relocation table RVA
	.long _pe_reloc_end - _pe_reloc_start        //     Base relocation table size

// ---- Section table ----
_pe_section_table:
	.ascii ".text\0\0\0"
	.long _pe_text_end - _pe_text_start          // VirtualSize
	.long _pe_text_start - _pe_image_base        // VirtualAddress
	.long _pe_text_end - _pe_text_start          // SizeOfRawData
	.long _pe_text_start - _pe_image_base        // PointerToRawData
	.long 0                           // PointerToRelocations
	.long 0                           // PointerToLinenumbers
	.short 0                          // NumberOfRelocations
	.short 0                          // NumberOfLinenumbers
	.long 0x60000020                  // CODE|MEM_EXECUTE|MEM_READ

	.ascii ".data\0\0\0"
	.long _pe_data_end - _pe_data_start          // VirtualSize
	.long _pe_data_start - _pe_image_base        // VirtualAddress
	.long _pe_data_end - _pe_data_start          // SizeOfRawData
	.long _pe_data_start - _pe_image_base        // PointerToRawData
	.long 0                           // PointerToRelocations
	.long 0                           // PointerToLinenumbers
	.short 0                          // NumberOfRelocations
	.short 0                          // NumberOfLinenumbers
	.long 0xc0000040                  // INITIALIZED_DATA|MEM_READ|MEM_WRITE

	.balign 0x1000
_pe_header_end:

// ---- Self-relocation entry stub (start of .text) -------------------------------
.section .text.head, "ax"
.balign 4
.global _pe_entry
_pe_entry:
	mv    s0, a0                      // preserve image handle across relocation
	mv    s1, a1                      // preserve system table
.option push
.option norelax
	lla   gp, __global_pointer$
.option pop
	lla   t0, _pe_image_base          // t0 = runtime load base (image linked at 0)
	lla   t1, __rela_start
	lla   t2, __rela_end
1:
	bgeu  t1, t2, 2f
	ld    t3, 0(t1)                   // r_offset
	ld    t4, 8(t1)                   // r_info
	li    t5, 3                       // R_RISCV_RELATIVE
	bne   t4, t5, 3f
	ld    t5, 16(t1)                  // r_addend
	add   t6, t0, t3                  // where = base + r_offset
	add   t5, t5, t0                  // value = base + r_addend
	sd    t5, 0(t6)
3:
	addi  t1, t1, 24                  // sizeof(Elf64_Rela)
	j     1b
2:
	mv    a0, s0
	mv    a1, s1
	call  efi_main
4:
	wfi
	j     4b
"#
);
