use super::*;
use std::vec;
use std::vec::Vec;

fn image(image_type: u16, segments: &[ProgramHeader], payload: &[u8]) -> Vec<u8> {
	let header_len = core::mem::size_of::<Elf64Header>();
	let table_len = core::mem::size_of_val(segments);
	let mut bytes = vec![0u8; header_len + table_len];
	let mut ident = [0u8; 16];
	ident[..4].copy_from_slice(&ELF_MAGIC);
	ident[4] = ELFCLASS64;
	ident[5] = ELFDATA2LSB;
	// THE IDENTIFICATION VERSION, which these fixtures left at zero. The parser did not read it,
	// so every fixture here declared no ELF version and was accepted anyway - which is precisely
	// the defect, visible from the test side: eleven fixtures had to be corrected for the check to
	// pass, and not one of them had ever been a legal ELF file.
	ident[EI_VERSION] = EV_CURRENT;
	let header = Elf64Header { e_ident: ident, e_type: image_type, e_machine: EXPECTED_MACHINE, e_version: 1, e_entry: 0x1000, e_phoff: header_len as u64, e_shoff: 0, e_flags: 0, e_ehsize: header_len as u16, e_phentsize: core::mem::size_of::<ProgramHeader>() as u16, e_phnum: segments.len() as u16, e_shentsize: 0, e_shnum: 0, e_shstrndx: 0 };
	unsafe {
		core::ptr::write_unaligned(bytes.as_mut_ptr() as *mut Elf64Header, header);
		core::ptr::copy_nonoverlapping(segments.as_ptr() as *const u8, bytes.as_mut_ptr().add(header_len), table_len);
	}
	bytes.extend_from_slice(payload);
	bytes
}

fn identity_note_image(record: &[u8]) -> (Vec<u8>, usize, usize) {
	let header_len = core::mem::size_of::<Elf64Header>();
	let strings = b"\0.shstrtab\0.note.liber.identity\0";
	let note_offset = header_len + strings.len();
	let note_len = 20 + ((record.len() + 3) & !3);
	let section_offset = note_offset + note_len;
	let sections = [
		SectionHeader { sh_name: 0, sh_type: 0, sh_flags: 0, sh_addr: 0, sh_offset: 0, sh_size: 0, sh_link: 0, sh_info: 0, sh_addralign: 0, sh_entsize: 0 },
		SectionHeader { sh_name: 1, sh_type: SHT_STRTAB, sh_flags: 0, sh_addr: 0, sh_offset: header_len as u64, sh_size: strings.len() as u64, sh_link: 0, sh_info: 0, sh_addralign: 1, sh_entsize: 0 },
		SectionHeader { sh_name: 11, sh_type: SHT_NOTE, sh_flags: SHF_ALLOC, sh_addr: 0, sh_offset: note_offset as u64, sh_size: note_len as u64, sh_link: 0, sh_info: 0, sh_addralign: 4, sh_entsize: 0 },
	];
	let mut bytes = vec![0u8; section_offset + core::mem::size_of_val(&sections)];
	let mut ident = [0u8; 16];
	ident[..4].copy_from_slice(&ELF_MAGIC);
	ident[4] = ELFCLASS64;
	ident[5] = ELFDATA2LSB;
	ident[EI_VERSION] = EV_CURRENT;
	let header = Elf64Header { e_ident: ident, e_type: ET_DYN, e_machine: EXPECTED_MACHINE, e_version: 1, e_entry: 0, e_phoff: header_len as u64, e_shoff: section_offset as u64, e_flags: 0, e_ehsize: header_len as u16, e_phentsize: core::mem::size_of::<ProgramHeader>() as u16, e_phnum: 0, e_shentsize: core::mem::size_of::<SectionHeader>() as u16, e_shnum: sections.len() as u16, e_shstrndx: 1 };
	unsafe {
		core::ptr::write_unaligned(bytes.as_mut_ptr() as *mut Elf64Header, header);
		core::ptr::copy_nonoverlapping(sections.as_ptr() as *const u8, bytes.as_mut_ptr().add(section_offset), core::mem::size_of_val(&sections));
	}
	bytes[header_len..note_offset].copy_from_slice(strings);
	bytes[note_offset..note_offset + 4].copy_from_slice(&6u32.to_le_bytes());
	bytes[note_offset + 4..note_offset + 8].copy_from_slice(&(record.len() as u32).to_le_bytes());
	bytes[note_offset + 8..note_offset + 12].copy_from_slice(&LIBER_IDENTITY_NOTE_TYPE.to_le_bytes());
	bytes[note_offset + 12..note_offset + 18].copy_from_slice(LIBER_IDENTITY_NOTE_NAME);
	bytes[note_offset + 20..note_offset + 20 + record.len()].copy_from_slice(record);
	(bytes, note_offset, section_offset)
}

#[test]
fn dynamic_entries_are_bounded_and_stop_at_dt_null() {
	let entries = [DynamicEntry { tag: 7, value: 0x1234 }, DynamicEntry { tag: DT_NULL, value: 0 }, DynamicEntry { tag: 99, value: 1 }];
	let payload = unsafe { core::slice::from_raw_parts(entries.as_ptr() as *const u8, core::mem::size_of_val(&entries)) };
	let offset = core::mem::size_of::<Elf64Header>() + core::mem::size_of::<ProgramHeader>();
	let segment = ProgramHeader { p_type: PT_DYNAMIC, p_flags: PF_R, p_offset: offset as u64, p_vaddr: 0x2000, p_paddr: 0, p_filesz: payload.len() as u64, p_memsz: payload.len() as u64, p_align: 8 };
	let bytes = image(ET_DYN, &[segment], payload);
	let elf = Elf::parse(&bytes).unwrap();
	assert_eq!(elf.image_type, ET_DYN);
	let mut dynamic = elf.dynamic_entries().unwrap().unwrap();
	assert_eq!(dynamic.next(), Some(entries[0]));
	assert_eq!(dynamic.next(), Some(entries[1]));
	assert!(dynamic.is_terminated());
	assert_eq!(dynamic.next(), None);
}

#[test]
fn malformed_header_and_dynamic_ranges_are_rejected() {
	let segment = ProgramHeader { p_type: PT_DYNAMIC, p_flags: PF_R, p_offset: u64::MAX, p_vaddr: 0, p_paddr: 0, p_filesz: 16, p_memsz: 16, p_align: 8 };
	let bytes = image(ET_DYN, &[segment], &[]);
	let elf = Elf::parse(&bytes).unwrap();
	assert!(elf.dynamic_entries().is_none());

	let mut truncated = image(ET_EXEC, &[], &[]);
	let header = unsafe { &mut *(truncated.as_mut_ptr() as *mut Elf64Header) };
	header.e_phnum = 1;
	header.e_phoff = u64::MAX;
	assert!(Elf::parse(&truncated).is_none());
}

#[test]
fn malformed_dynamic_tables_fail_closed() {
	let entry_len = core::mem::size_of::<DynamicEntry>();
	let header_len = core::mem::size_of::<Elf64Header>();
	let duplicate_offset = header_len + core::mem::size_of::<[ProgramHeader; 2]>();
	let terminator = [DynamicEntry { tag: DT_NULL, value: 0 }];
	let terminator_bytes = unsafe { core::slice::from_raw_parts(terminator.as_ptr() as *const u8, core::mem::size_of_val(&terminator)) };
	let duplicate_segments = [
		ProgramHeader { p_type: PT_DYNAMIC, p_flags: PF_R, p_offset: duplicate_offset as u64, p_vaddr: 0x2000, p_paddr: 0, p_filesz: entry_len as u64, p_memsz: entry_len as u64, p_align: 8 },
		ProgramHeader { p_type: PT_DYNAMIC, p_flags: PF_R, p_offset: duplicate_offset as u64, p_vaddr: 0x3000, p_paddr: 0, p_filesz: entry_len as u64, p_memsz: entry_len as u64, p_align: 8 },
	];
	let duplicate = image(ET_DYN, &duplicate_segments, terminator_bytes);
	assert!(Elf::parse(&duplicate).unwrap().dynamic_entries().is_none());

	let missing_offset = header_len + core::mem::size_of::<ProgramHeader>();
	let unterminated = [DynamicEntry { tag: DT_NEEDED, value: 0 }];
	let unterminated_bytes = unsafe { core::slice::from_raw_parts(unterminated.as_ptr() as *const u8, core::mem::size_of_val(&unterminated)) };
	let missing_segment = ProgramHeader { p_type: PT_DYNAMIC, p_flags: PF_R, p_offset: missing_offset as u64, p_vaddr: 0x4000, p_paddr: 0, p_filesz: entry_len as u64, p_memsz: entry_len as u64, p_align: 8 };
	let missing = image(ET_DYN, &[missing_segment], unterminated_bytes);
	assert!(Elf::parse(&missing).unwrap().dynamic_entries().is_none());

	let table_len = core::mem::size_of::<[ProgramHeader; 2]>();
	let payload_offset = header_len + table_len;
	let load_address = 0x5000u64;
	let strings = b"provider.lslib\0";
	let dynamic = [
		DynamicEntry { tag: DT_STRTAB, value: load_address },
		DynamicEntry { tag: DT_STRTAB, value: load_address },
		DynamicEntry { tag: DT_STRSZ, value: strings.len() as u64 },
		DynamicEntry { tag: DT_NULL, value: 0 },
	];
	let mut payload = strings.to_vec();
	let dynamic_offset = payload.len();
	payload.extend_from_slice(unsafe { core::slice::from_raw_parts(dynamic.as_ptr() as *const u8, core::mem::size_of_val(&dynamic)) });
	let singleton_segments = [
		ProgramHeader { p_type: PT_LOAD, p_flags: PF_R, p_offset: payload_offset as u64, p_vaddr: load_address, p_paddr: 0, p_filesz: payload.len() as u64, p_memsz: payload.len() as u64, p_align: 1 },
		ProgramHeader { p_type: PT_DYNAMIC, p_flags: PF_R, p_offset: (payload_offset + dynamic_offset) as u64, p_vaddr: load_address + dynamic_offset as u64, p_paddr: 0, p_filesz: core::mem::size_of_val(&dynamic) as u64, p_memsz: core::mem::size_of_val(&dynamic) as u64, p_align: 8 },
	];
	let duplicate_singleton = image(ET_DYN, &singleton_segments, &payload);
	assert!(Elf::parse(&duplicate_singleton).unwrap().dynamic_info().is_none());
}

#[test]
fn explicit_machine_parser_supports_cross_target_audits() {
	let mut bytes = image(ET_DYN, &[], &[]);
	let other_machine = if EXPECTED_MACHINE == EM_AARCH64 { EM_RISCV } else { EM_AARCH64 };
	let header = unsafe { &mut *(bytes.as_mut_ptr() as *mut Elf64Header) };
	header.e_machine = other_machine;
	assert!(Elf::parse(&bytes).is_none());
	assert!(Elf::parse_for_machine(&bytes, other_machine).is_some());
}

#[test]
fn liber_identity_note_is_exact_and_unique() {
	let record = b"format=liber-image-identity-v1\n";
	let (bytes, note_offset, section_offset) = identity_note_image(record);
	assert_eq!(Elf::parse(&bytes).unwrap().liber_identity_note(), Some(&record[..]));

	let (mut malformed, _, _) = identity_note_image(record);
	malformed[note_offset..note_offset + 4].copy_from_slice(&5u32.to_le_bytes());
	assert!(Elf::parse(&malformed).unwrap().liber_identity_note().is_none());

	let (mut duplicate, _, _) = identity_note_image(record);
	let note_header = duplicate[section_offset + 2 * core::mem::size_of::<SectionHeader>()..section_offset + 3 * core::mem::size_of::<SectionHeader>()].to_vec();
	duplicate[section_offset..section_offset + core::mem::size_of::<SectionHeader>()].copy_from_slice(&note_header);
	assert!(Elf::parse(&duplicate).unwrap().liber_identity_note().is_none());

	let oversized = vec![b'x'; MAX_LIBER_IDENTITY_RECORD_BYTES + 1];
	let (oversized, _, _) = identity_note_image(&oversized);
	assert!(Elf::parse(&oversized).unwrap().liber_identity_note().is_none());
}

#[test]
fn dynamic_relocation_policy_is_exact_for_every_supported_machine() {
	let cases: &[(u16, u32, &[u32])] = &[(EM_X86_64, 8, &[1, 6, 7]), (EM_AARCH64, 1027, &[257, 1025, 1026]), (EM_RISCV, 3, &[2, 5])];
	for &(machine, relative, symbols) in cases {
		assert_eq!(dynamic_relocation_kind(machine, relative), Some(DynamicRelocationKind::Relative));
		for &symbol in symbols {
			assert_eq!(dynamic_relocation_kind(machine, symbol), Some(DynamicRelocationKind::Symbol));
		}
		assert_eq!(dynamic_relocation_kind(machine, 0), None);
	}
	assert!(DynamicRelocationKind::Relative.accepts_symbol(0));
	assert!(!DynamicRelocationKind::Relative.accepts_symbol(1));
	assert!(DynamicRelocationKind::Symbol.accepts_symbol(0));
	assert_eq!(dynamic_relocation_kind(EM_X86_64, 1027), None);
	assert_eq!(dynamic_relocation_kind(EM_AARCH64, 3), None);
	assert_eq!(dynamic_relocation_kind(EM_RISCV, 8), None);
	assert_eq!(expected_machine(), EXPECTED_MACHINE);
}

#[test]
fn rela_metadata_uses_virtual_addresses_and_rejects_partial_tables() {
	let header_len = core::mem::size_of::<Elf64Header>();
	let table_len = core::mem::size_of::<[ProgramHeader; 2]>();
	let payload_offset = header_len + table_len;
	let load_address = 0x4000u64;
	let rela = Rela { offset: 0x5000, info: 8, addend: 0x1234 };
	let dynamic = [
		DynamicEntry { tag: DT_RELA, value: load_address },
		DynamicEntry { tag: DT_RELASZ, value: core::mem::size_of::<Rela>() as u64 },
		DynamicEntry { tag: DT_RELAENT, value: core::mem::size_of::<Rela>() as u64 },
		DynamicEntry { tag: DT_RELACOUNT, value: 1 },
		DynamicEntry { tag: DT_NULL, value: 0 },
	];
	let mut payload = Vec::new();
	payload.extend_from_slice(unsafe { core::slice::from_raw_parts(&rela as *const Rela as *const u8, core::mem::size_of::<Rela>()) });
	let dynamic_offset = payload.len();
	payload.extend_from_slice(unsafe { core::slice::from_raw_parts(dynamic.as_ptr() as *const u8, core::mem::size_of_val(&dynamic)) });
	let segments = [
		ProgramHeader { p_type: PT_LOAD, p_flags: PF_R, p_offset: payload_offset as u64, p_vaddr: load_address, p_paddr: 0, p_filesz: payload.len() as u64, p_memsz: payload.len() as u64, p_align: 8 },
		ProgramHeader { p_type: PT_DYNAMIC, p_flags: PF_R, p_offset: (payload_offset + dynamic_offset) as u64, p_vaddr: load_address + dynamic_offset as u64, p_paddr: 0, p_filesz: core::mem::size_of_val(&dynamic) as u64, p_memsz: core::mem::size_of_val(&dynamic) as u64, p_align: 8 },
	];
	let bytes = image(ET_DYN, &segments, &payload);
	let elf = Elf::parse(&bytes).unwrap();
	let info = elf.dynamic_info().unwrap().unwrap();
	assert_eq!(elf.rela_entries(&info).unwrap().collect::<Vec<_>>(), vec![rela]);

	let mut bad = bytes.clone();
	let rela_size_value = payload_offset + dynamic_offset + core::mem::size_of::<DynamicEntry>() + 8;
	bad[rela_size_value..rela_size_value + 8].copy_from_slice(&23u64.to_le_bytes());
	assert!(Elf::parse(&bad).unwrap().dynamic_info().is_none());
}

#[test]
fn needed_names_are_resolved_only_inside_the_bounded_string_table() {
	let header_len = core::mem::size_of::<Elf64Header>();
	let table_len = core::mem::size_of::<[ProgramHeader; 2]>();
	let payload_offset = header_len + table_len;
	let load_address = 0x6000u64;
	let strings = b"lsrt.lslib\0proto.lslib\0";
	let dynamic = [
		DynamicEntry { tag: DT_STRTAB, value: load_address },
		DynamicEntry { tag: DT_STRSZ, value: strings.len() as u64 },
		DynamicEntry { tag: DT_NEEDED, value: 0 },
		DynamicEntry { tag: DT_NEEDED, value: 11 },
		DynamicEntry { tag: DT_NULL, value: 0 },
	];
	let mut payload = strings.to_vec();
	let dynamic_offset = payload.len();
	payload.extend_from_slice(unsafe { core::slice::from_raw_parts(dynamic.as_ptr() as *const u8, core::mem::size_of_val(&dynamic)) });
	let segments = [
		ProgramHeader { p_type: PT_LOAD, p_flags: PF_R, p_offset: payload_offset as u64, p_vaddr: load_address, p_paddr: 0, p_filesz: payload.len() as u64, p_memsz: payload.len() as u64, p_align: 1 },
		ProgramHeader { p_type: PT_DYNAMIC, p_flags: PF_R, p_offset: (payload_offset + dynamic_offset) as u64, p_vaddr: load_address + dynamic_offset as u64, p_paddr: 0, p_filesz: core::mem::size_of_val(&dynamic) as u64, p_memsz: core::mem::size_of_val(&dynamic) as u64, p_align: 8 },
	];
	let bytes = image(ET_DYN, &segments, &payload);
	let elf = Elf::parse(&bytes).unwrap();
	let info = elf.dynamic_info().unwrap().unwrap();
	assert_eq!(elf.needed_names(&info).unwrap().collect::<Vec<_>>(), vec!["lsrt.lslib", "proto.lslib"]);

	let mut bad = bytes;
	bad[payload_offset + strings.len() - 1] = b'x';
	let bad_elf = Elf::parse(&bad).unwrap();
	let bad_info = bad_elf.dynamic_info().unwrap().unwrap();
	assert!(bad_elf.needed_names(&bad_info).is_none());
}

#[test]
fn sysv_hash_bounds_the_dynamic_symbol_table() {
	let header_len = core::mem::size_of::<Elf64Header>();
	let table_len = core::mem::size_of::<[ProgramHeader; 2]>();
	let payload_offset = header_len + table_len;
	let load_address = 0x8000u64;
	let strings = b"\0exported\0";
	let symbols = [Symbol { name: 0, info: 0, other: 0, section: 0, value: 0, size: 0 }, Symbol { name: 1, info: 0x12, other: 0, section: 1, value: 0x120, size: 8 }];
	let mut payload = strings.to_vec();
	let symbols_offset = payload.len();
	payload.extend_from_slice(unsafe { core::slice::from_raw_parts(symbols.as_ptr() as *const u8, core::mem::size_of_val(&symbols)) });
	let hash_offset = payload.len();
	for word in [1u32, 2, 1, 0, 0] {
		payload.extend_from_slice(&word.to_le_bytes());
	}
	let dynamic_offset = payload.len();
	let dynamic = [
		DynamicEntry { tag: DT_STRTAB, value: load_address },
		DynamicEntry { tag: DT_STRSZ, value: strings.len() as u64 },
		DynamicEntry { tag: DT_SYMTAB, value: load_address + symbols_offset as u64 },
		DynamicEntry { tag: DT_SYMENT, value: core::mem::size_of::<Symbol>() as u64 },
		DynamicEntry { tag: DT_HASH, value: load_address + hash_offset as u64 },
		DynamicEntry { tag: DT_NULL, value: 0 },
	];
	payload.extend_from_slice(unsafe { core::slice::from_raw_parts(dynamic.as_ptr() as *const u8, core::mem::size_of_val(&dynamic)) });
	let segments = [
		ProgramHeader { p_type: PT_LOAD, p_flags: PF_R, p_offset: payload_offset as u64, p_vaddr: load_address, p_paddr: 0, p_filesz: payload.len() as u64, p_memsz: payload.len() as u64, p_align: 1 },
		ProgramHeader { p_type: PT_DYNAMIC, p_flags: PF_R, p_offset: (payload_offset + dynamic_offset) as u64, p_vaddr: load_address + dynamic_offset as u64, p_paddr: 0, p_filesz: core::mem::size_of_val(&dynamic) as u64, p_memsz: core::mem::size_of_val(&dynamic) as u64, p_align: 8 },
	];
	let bytes = image(ET_DYN, &segments, &payload);
	let elf = Elf::parse(&bytes).unwrap();
	let info = elf.dynamic_info().unwrap().unwrap();
	assert_eq!(elf.symbols(&info).unwrap().collect::<Vec<_>>(), vec![(symbols[0], ""), (symbols[1], "exported")]);

	let mut bad = bytes.clone();
	bad[payload_offset + hash_offset + 4..payload_offset + hash_offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());
	let bad_elf = Elf::parse(&bad).unwrap();
	let bad_info = bad_elf.dynamic_info().unwrap().unwrap();
	assert!(bad_elf.symbols(&bad_info).is_none());

	let mut bad_syment = bytes.clone();
	let syment_value = payload_offset + dynamic_offset + 3 * core::mem::size_of::<DynamicEntry>() + 8;
	bad_syment[syment_value..syment_value + 8].copy_from_slice(&23u64.to_le_bytes());
	assert!(Elf::parse(&bad_syment).unwrap().dynamic_info().is_none());

	let mut bad_name = bytes;
	bad_name[payload_offset + symbols_offset + core::mem::size_of::<Symbol>()..payload_offset + symbols_offset + core::mem::size_of::<Symbol>() + 4].copy_from_slice(&u32::MAX.to_le_bytes());
	let bad_name_elf = Elf::parse(&bad_name).unwrap();
	let bad_name_info = bad_name_elf.dynamic_info().unwrap().unwrap();
	assert!(bad_name_elf.symbols(&bad_name_info).is_none());
}

#[test]
fn plt_rela_metadata_is_complete_and_bounded() {
	let header_len = core::mem::size_of::<Elf64Header>();
	let table_len = core::mem::size_of::<[ProgramHeader; 2]>();
	let payload_offset = header_len + table_len;
	let load_address = 0xa000u64;
	let relocation = Rela { offset: 0xb000, info: 7, addend: 0 };
	let mut payload = unsafe { core::slice::from_raw_parts(&relocation as *const Rela as *const u8, core::mem::size_of::<Rela>()) }.to_vec();
	let dynamic_offset = payload.len();
	let dynamic = [
		DynamicEntry { tag: DT_JMPREL, value: load_address },
		DynamicEntry { tag: DT_PLTRELSZ, value: core::mem::size_of::<Rela>() as u64 },
		DynamicEntry { tag: DT_PLTREL, value: DT_RELA as u64 },
		DynamicEntry { tag: DT_NULL, value: 0 },
	];
	payload.extend_from_slice(unsafe { core::slice::from_raw_parts(dynamic.as_ptr() as *const u8, core::mem::size_of_val(&dynamic)) });
	let segments = [
		ProgramHeader { p_type: PT_LOAD, p_flags: PF_R, p_offset: payload_offset as u64, p_vaddr: load_address, p_paddr: 0, p_filesz: payload.len() as u64, p_memsz: payload.len() as u64, p_align: 1 },
		ProgramHeader { p_type: PT_DYNAMIC, p_flags: PF_R, p_offset: (payload_offset + dynamic_offset) as u64, p_vaddr: load_address + dynamic_offset as u64, p_paddr: 0, p_filesz: core::mem::size_of_val(&dynamic) as u64, p_memsz: core::mem::size_of_val(&dynamic) as u64, p_align: 8 },
	];
	let bytes = image(ET_DYN, &segments, &payload);
	let elf = Elf::parse(&bytes).unwrap();
	let info = elf.dynamic_info().unwrap().unwrap();
	assert_eq!(elf.plt_rela_entries(&info).unwrap().collect::<Vec<_>>(), vec![relocation]);

	let mut bad_size = bytes.clone();
	let size_value = payload_offset + dynamic_offset + core::mem::size_of::<DynamicEntry>() + 8;
	bad_size[size_value..size_value + 8].copy_from_slice(&23u64.to_le_bytes());
	assert!(Elf::parse(&bad_size).unwrap().dynamic_info().is_none());

	let mut bad = bytes;
	let kind_value = payload_offset + dynamic_offset + 2 * core::mem::size_of::<DynamicEntry>() + 8;
	bad[kind_value..kind_value + 8].copy_from_slice(&17u64.to_le_bytes());
	assert!(Elf::parse(&bad).unwrap().dynamic_info().is_none());
}

#[test]
fn a_malformed_pt_load_is_refused_by_the_parser_rather_than_by_each_loader() {
	// The parser bounded `p_offset .. p_offset + p_filesz` against the file and stopped there, so
	// every backend sized its allocation from `p_memsz` and copied `p_filesz` bytes into it. A
	// header declaring `p_memsz = 4096, p_filesz = 65536` reserved one page and wrote sixty-four
	// kilobytes of firmware memory. The kernel's own loader clamps each per-page copy and was never
	// exposed; the loader, reading the boot medium, was.
	//
	// Validated once here, so all four readers get it and none of them has to remember.
	let payload = vec![0xAAu8; 8192];
	let ok = ProgramHeader { p_type: PT_LOAD, p_flags: PF_R | PF_X, p_offset: 0, p_vaddr: 0x1000, p_paddr: 0x1000, p_filesz: 4096, p_memsz: 8192, p_align: 4096 };
	assert!(Elf::parse(&image(ET_EXEC, &[ok], &payload)).is_some(), "the sane segment must parse, or nothing below means anything");

	let refused = |what: &str, edit: &dyn Fn(&mut ProgramHeader)| {
		let mut ph = ok;
		edit(&mut ph);
		assert!(Elf::parse(&image(ET_EXEC, &[ph], &payload)).is_none(), "{what}");
	};
	refused("more file bytes than memory to hold them", &|ph| {
		ph.p_filesz = 8192;
		ph.p_memsz = 4096;
	});
	refused("file bytes the file does not contain", &|ph| {
		ph.p_offset = 4096;
		ph.p_filesz = 1 << 20;
		ph.p_memsz = 1 << 20;
	});
	// NOT refused here, and the reason is worth keeping: this parser reads THIS PROJECT'S OWN
	// kernels, and the aarch64 and riscv64 images link their boot stub - MMU off, then on - as a
	// single `RWE` segment. Demanding W^X at parse refused both of them outright, before anything
	// was loaded, on a claim ("the kernel does not have such a segment") that had only been
	// measured on x86_64. W^X is a policy about what may be MAPPED: the kernel's userspace loader
	// enforces it for every program it loads, and the x86_64 loader asserts it for the kernel
	// image, whose mapper derives WRITABLE and NX independently.
	let mut wx = ok;
	wx.p_flags = PF_R | PF_W | PF_X;
	assert!(Elf::parse(&image(ET_EXEC, &[wx], &payload)).is_some(), "a writable-executable segment is a mapping policy question, not a malformed image");
	refused("a load address whose page offset does not match the file offset", &|ph| ph.p_vaddr = 0x1008);
	refused("an alignment that is not a power of two", &|ph| ph.p_align = 3000);
}

#[test]
fn a_shared_object_is_not_required_to_be_page_aligned() {
	// The congruence `p_vaddr = p_offset (mod p_align)` is what ELF requires, and absolute 4 KiB
	// alignment was demanded here first. Every shared object in this tree is linked with
	// `p_align = 0x10000` and addresses like `0x3478c`, so that rule refused `lsrt.lslib` and the
	// aarch64 image build stopped at "no valid target ELF" - a whole architecture, from one
	// comparison that reads plausibly.
	//
	// The stricter rule the x86 kernel loader needs is asserted in the loader, beside the code that
	// copies to `phys + 0` and maps from `align_down(p_vaddr)`.
	let payload = vec![0xAAu8; 0x2000];
	let congruent = ProgramHeader { p_type: PT_LOAD, p_flags: PF_R | PF_X, p_offset: 0x78c, p_vaddr: 0x1078c, p_paddr: 0x1078c, p_filesz: 0x100, p_memsz: 0x100, p_align: 0x10000 };
	assert!(Elf::parse(&image(ET_DYN, &[congruent], &payload)).is_some(), "a real shared object's segment must parse");

	let mut incongruent = congruent;
	incongruent.p_vaddr = 0x10000;
	incongruent.p_paddr = 0x10000;
	assert!(Elf::parse(&image(ET_DYN, &[incongruent], &payload)).is_none(), "a segment that cannot be mapped from the file as-is");
}

#[test]
fn a_segment_whose_physical_end_wraps_is_refused() {
	// `p_filesz > p_memsz` was checked and every FILE offset went through `checked_add`, and nothing
	// bounded `p_paddr + p_memsz` - which `reserve_kernel`, the aarch64 placement and the riscv64
	// staging all compute, in release builds, silently.
	//
	// It matters more than "an arithmetic slip". The aarch64 placement asserts `reserved.owns(base,
	// pages)` before it writes, and both sides compute `pages` with the same expression - so in the
	// overflow case both wrap identically, `owns` agrees, and the `write_bytes` below it uses the
	// FULL declared `p_memsz`. The check that looks like it stands in for this one cannot.
	let payload = vec![0xAAu8; 0x2000];
	let wrapping = ProgramHeader { p_type: PT_LOAD, p_flags: PF_R | PF_X, p_offset: 0x1000, p_vaddr: 0x1000, p_paddr: 0xffff_ffff_ffff_f000, p_filesz: 0x100, p_memsz: 0x3000, p_align: 0x1000 };
	assert!(Elf::parse(&image(ET_EXEC, &[wrapping], &payload)).is_none(), "a segment whose physical end wraps is not a loadable image");

	// And the same segment at an address that does not wrap still parses, or the assertion above
	// proves only that something else about it is wrong.
	let mut fits = wrapping;
	fits.p_paddr = 0x10_0000;
	assert!(Elf::parse(&image(ET_EXEC, &[fits], &payload)).is_some(), "an ordinary physical address is unaffected");
}

#[test]
fn an_image_that_declares_no_elf_version_is_refused() {
	// BOTH FIELDS, because the format writes the version twice and neither was read. An image with
	// either one set to zero - or to a version whose layout this reader does not implement - was
	// parsed as though it had said version 1, which is fail-open on the field whose only job is to
	// say which layout follows.
	//
	// The evidence that nothing checked it is in this file rather than in an argument: every
	// fixture above built `e_ident` from magic, class and data order and left the version byte at
	// zero, and all of them parsed. None had ever been a legal ELF file.
	let good = image(ET_EXEC, &[], &[]);
	assert!(Elf::parse_for_machine(&good, EXPECTED_MACHINE).is_some(), "the corrected fixture is accepted");

	for (offset, what) in [(6usize, "the identification version byte")] {
		let mut broken = good.clone();
		broken[offset] = 0;
		assert!(Elf::parse_for_machine(&broken, EXPECTED_MACHINE).is_none(), "a zero in {what} is refused");
		let mut future = good.clone();
		future[offset] = 2;
		assert!(Elf::parse_for_machine(&future, EXPECTED_MACHINE).is_none(), "and so is a version this reader does not implement in {what}");
	}

	// `e_version` is the `u32` at offset 20, after the 16-byte identification, `e_type` and
	// `e_machine`.
	for value in [0u32, 2, u32::MAX] {
		let mut broken = good.clone();
		broken[20..24].copy_from_slice(&value.to_le_bytes());
		assert!(Elf::parse_for_machine(&broken, EXPECTED_MACHINE).is_none(), "e_version = {value} is refused");
	}

	// And the preliminary classifier agrees about the identification byte, so a wrong-target answer
	// cannot be read out of a file that never declared this layout.
	let mut broken = good.clone();
	broken[6] = 0;
	assert_eq!(crate::compat::declared_machine(&broken), None, "the classifier refuses it too");
	assert_eq!(crate::compat::declared_machine(&good), Some(EXPECTED_MACHINE));
}
