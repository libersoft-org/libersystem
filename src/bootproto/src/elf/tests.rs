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
	let header = Elf64Header { e_ident: ident, e_type: image_type, e_machine: EXPECTED_MACHINE, e_version: 1, e_entry: 0x1000, e_phoff: header_len as u64, e_shoff: 0, e_flags: 0, e_ehsize: header_len as u16, e_phentsize: core::mem::size_of::<ProgramHeader>() as u16, e_phnum: segments.len() as u16, e_shentsize: 0, e_shnum: 0, e_shstrndx: 0 };
	unsafe {
		core::ptr::write_unaligned(bytes.as_mut_ptr() as *mut Elf64Header, header);
		core::ptr::copy_nonoverlapping(segments.as_ptr() as *const u8, bytes.as_mut_ptr().add(header_len), table_len);
	}
	bytes.extend_from_slice(payload);
	bytes
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
fn explicit_machine_parser_supports_cross_target_audits() {
	let mut bytes = image(ET_DYN, &[], &[]);
	let other_machine = if EXPECTED_MACHINE == EM_AARCH64 { EM_RISCV } else { EM_AARCH64 };
	let header = unsafe { &mut *(bytes.as_mut_ptr() as *mut Elf64Header) };
	header.e_machine = other_machine;
	assert!(Elf::parse(&bytes).is_none());
	assert!(Elf::parse_for_machine(&bytes, other_machine).is_some());
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

	let mut bad = bytes;
	let kind_value = payload_offset + dynamic_offset + 2 * core::mem::size_of::<DynamicEntry>() + 8;
	bad[kind_value..kind_value + 8].copy_from_slice(&17u64.to_le_bytes());
	assert!(Elf::parse(&bad).unwrap().dynamic_info().is_none());
}
