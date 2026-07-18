// Minimal read-only ELF64 reader shared by the loader and the kernel.
//
// Both need the SAME thing from an ELF image: validate the header for this build's
// architecture and walk its program headers. Only what each does with the
// segments differs - the loader copies them to the physical memory backing their
// link-time addresses, the kernel maps them into a target address space's page tables
// - so the parsing lives here (in the dependency-free boot-protocol crate both share)
// and each caller keeps its own mapping. ET_DYN metadata is exposed through a bounded
// PT_DYNAMIC iterator; relocation policy remains the kernel loader's responsibility.
// The machine constants for the other architectures are unused on any single build.
#![allow(dead_code)]
// ELF identification / header fields validated on parse.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
pub const ET_EXEC: u16 = 2;
pub const ET_DYN: u16 = 3;
pub const EM_X86_64: u16 = 62;
pub const EM_AARCH64: u16 = 183;
pub const EM_RISCV: u16 = 243;

// The machine an image must target: the loader and the kernel each load images for
// their own build architecture, so the expected e_machine is the build arch's.
#[cfg(target_arch = "x86_64")]
const EXPECTED_MACHINE: u16 = EM_X86_64;
#[cfg(target_arch = "aarch64")]
const EXPECTED_MACHINE: u16 = EM_AARCH64;
#[cfg(target_arch = "riscv64")]
const EXPECTED_MACHINE: u16 = EM_RISCV;

// Program-header types used by the program and shared-library loaders.
pub const PT_LOAD: u32 = 1;
pub const PT_DYNAMIC: u32 = 2;

// Dynamic-table terminator. Further tags are interpreted by the kernel loader.
pub const DT_NULL: i64 = 0;
pub const DT_NEEDED: i64 = 1;
pub const DT_PLTRELSZ: i64 = 2;
pub const DT_PLTREL: i64 = 20;
pub const DT_JMPREL: i64 = 23;
pub const DT_HASH: i64 = 4;
pub const DT_STRTAB: i64 = 5;
pub const DT_SYMTAB: i64 = 6;
pub const DT_RELA: i64 = 7;
pub const DT_RELASZ: i64 = 8;
pub const DT_RELAENT: i64 = 9;
pub const DT_STRSZ: i64 = 10;
pub const DT_SYMENT: i64 = 11;
pub const DT_SONAME: i64 = 14;
pub const DT_RELACOUNT: i64 = 0x6fff_fff9;

// Program-header flags (p_flags).
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Header {
	e_ident: [u8; 16],
	e_type: u16,
	e_machine: u16,
	e_version: u32,
	e_entry: u64,
	e_phoff: u64,
	e_shoff: u64,
	e_flags: u32,
	e_ehsize: u16,
	e_phentsize: u16,
	e_phnum: u16,
	e_shentsize: u16,
	e_shnum: u16,
	e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProgramHeader {
	pub p_type: u32,
	pub p_flags: u32,
	pub p_offset: u64,
	pub p_vaddr: u64,
	pub p_paddr: u64,
	pub p_filesz: u64,
	pub p_memsz: u64,
	pub p_align: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DynamicEntry {
	pub tag: i64,
	pub value: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rela {
	pub offset: u64,
	pub info: u64,
	pub addend: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Symbol {
	pub name: u32,
	pub info: u8,
	pub other: u8,
	pub section: u16,
	pub value: u64,
	pub size: u64,
}

impl Symbol {
	pub const fn binding(self) -> u8 {
		self.info >> 4
	}

	pub const fn is_defined(self) -> bool {
		self.section != 0
	}

	pub const fn symbol_type(self) -> u8 {
		self.info & 0x0f
	}

	pub const fn visibility(self) -> u8 {
		self.other & 0x03
	}
}

impl Rela {
	pub const fn symbol(self) -> u32 {
		(self.info >> 32) as u32
	}

	pub const fn relocation_type(self) -> u32 {
		self.info as u32
	}
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DynamicInfo {
	pub hash: Option<u64>,
	pub strtab: Option<u64>,
	pub strsz: Option<u64>,
	pub symtab: Option<u64>,
	pub syment: Option<u64>,
	pub rela: Option<u64>,
	pub relasz: Option<u64>,
	pub relaent: Option<u64>,
	pub relacount: Option<u64>,
	pub jmprel: Option<u64>,
	pub pltrelsz: Option<u64>,
	pub pltrel: Option<u64>,
}

// A parsed, validated ELF64 image over its in-memory bytes.
pub struct Elf<'a> {
	bytes: &'a [u8],
	pub image_type: u16,
	pub entry: u64,
	phoff: u64,
	phentsize: u16,
	phnum: u16,
}

impl<'a> Elf<'a> {
	// Validate the header and capture the entry point + program-header table
	// location. Returns None if the bytes are not a little-endian 64-bit ET_EXEC /
	// ET_DYN image for this build's architecture, or are truncated.
	pub fn parse(bytes: &'a [u8]) -> Option<Self> {
		Self::parse_for_machine(bytes, EXPECTED_MACHINE)
	}

	// Host-side image builders audit artifacts for architectures other than their
	// own. Runtime callers use `parse`; builders pass the machine they are staging.
	pub fn parse_for_machine(bytes: &'a [u8], expected_machine: u16) -> Option<Self> {
		if bytes.len() < core::mem::size_of::<Elf64Header>() {
			return None;
		}
		// SAFETY: the length check above guarantees a full header is present; the
		// read is unaligned-safe.
		let header = unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const Elf64Header) };
		if header.e_ident[0..4] != ELF_MAGIC {
			return None;
		}
		if header.e_ident[4] != ELFCLASS64 || header.e_ident[5] != ELFDATA2LSB {
			return None;
		}
		if (header.e_type != ET_EXEC && header.e_type != ET_DYN) || header.e_machine != expected_machine || header.e_ehsize as usize != core::mem::size_of::<Elf64Header>() || header.e_phentsize as usize != core::mem::size_of::<ProgramHeader>() {
			return None;
		}
		let table_len = (header.e_phnum as usize).checked_mul(header.e_phentsize as usize)?;
		let table_start = usize::try_from(header.e_phoff).ok()?;
		let table_end = table_start.checked_add(table_len)?;
		if table_end > bytes.len() {
			return None;
		}
		Some(Self { bytes, image_type: header.e_type, entry: header.e_entry, phoff: header.e_phoff, phentsize: header.e_phentsize, phnum: header.e_phnum })
	}

	// The number of program headers.
	pub fn segment_count(&self) -> usize {
		self.phnum as usize
	}

	// The program header at index `i`, or None if it lies outside the file.
	pub fn segment(&self, i: usize) -> Option<ProgramHeader> {
		if i >= self.phnum as usize {
			return None;
		}
		let off = usize::try_from(self.phoff).ok()?.checked_add(i.checked_mul(self.phentsize as usize)?)?;
		let end = off.checked_add(core::mem::size_of::<ProgramHeader>())?;
		if end > self.bytes.len() {
			return None;
		}
		// SAFETY: bounds-checked above; unaligned read.
		Some(unsafe { core::ptr::read_unaligned(self.bytes.as_ptr().add(off) as *const ProgramHeader) })
	}

	// The file bytes backing a segment's on-disk portion (p_offset .. p_offset +
	// p_filesz), or None if that range lies outside the file.
	pub fn segment_data(&self, ph: &ProgramHeader) -> Option<&'a [u8]> {
		let start = usize::try_from(ph.p_offset).ok()?;
		let end = start.checked_add(usize::try_from(ph.p_filesz).ok()?)?;
		self.bytes.get(start..end)
	}

	// Translate an image virtual address range to its file-backed bytes. Dynamic
	// table pointers are virtual addresses, not file offsets; only a range wholly
	// contained in one PT_LOAD file span is valid at load time.
	pub fn virtual_data(&self, address: u64, len: u64) -> Option<&'a [u8]> {
		let requested_end = address.checked_add(len)?;
		for index in 0..self.segment_count() {
			let segment = self.segment(index)?;
			if segment.p_type != PT_LOAD {
				continue;
			}
			let segment_end = segment.p_vaddr.checked_add(segment.p_filesz)?;
			if address < segment.p_vaddr || requested_end > segment_end {
				continue;
			}
			let delta = address.checked_sub(segment.p_vaddr)?;
			let file_start = segment.p_offset.checked_add(delta)?;
			let start = usize::try_from(file_start).ok()?;
			let end = start.checked_add(usize::try_from(len).ok()?)?;
			return self.bytes.get(start..end);
		}
		None
	}

	// Locate the optional PT_DYNAMIC segment. Multiple dynamic tables are malformed:
	// dependency and relocation metadata must have one unambiguous source.
	pub fn dynamic_entries(&self) -> Option<Option<DynamicEntries<'a>>> {
		let mut dynamic = None;
		for index in 0..self.segment_count() {
			let segment = self.segment(index)?;
			if segment.p_type != PT_DYNAMIC {
				continue;
			}
			if dynamic.is_some() || segment.p_filesz == 0 || segment.p_filesz % core::mem::size_of::<DynamicEntry>() as u64 != 0 {
				return None;
			}
			let bytes = self.segment_data(&segment)?;
			let entry_len = core::mem::size_of::<DynamicEntry>();
			let terminator = bytes.chunks_exact(entry_len).position(|chunk| {
				let entry = unsafe { core::ptr::read_unaligned(chunk.as_ptr() as *const DynamicEntry) };
				entry.tag == DT_NULL
			})?;
			let used = (terminator + 1).checked_mul(entry_len)?;
			dynamic = Some(DynamicEntries { bytes: &bytes[..used], offset: 0, terminated: false });
		}
		Some(dynamic)
	}

	pub fn dynamic_info(&self) -> Option<Option<DynamicInfo>> {
		let Some(entries) = self.dynamic_entries()? else { return Some(None) };
		let mut info = DynamicInfo::default();
		for entry in entries {
			let slot = match entry.tag {
				DT_HASH => &mut info.hash,
				DT_STRTAB => &mut info.strtab,
				DT_STRSZ => &mut info.strsz,
				DT_SYMTAB => &mut info.symtab,
				DT_SYMENT => &mut info.syment,
				DT_RELA => &mut info.rela,
				DT_RELASZ => &mut info.relasz,
				DT_RELAENT => &mut info.relaent,
				DT_RELACOUNT => &mut info.relacount,
				DT_JMPREL => &mut info.jmprel,
				DT_PLTRELSZ => &mut info.pltrelsz,
				DT_PLTREL => &mut info.pltrel,
				DT_NULL => break,
				_ => continue,
			};
			if slot.replace(entry.value).is_some() {
				return None;
			}
		}
		if info.rela.is_some() || info.relasz.is_some() || info.relaent.is_some() {
			let (Some(rela), Some(relasz), Some(relaent)) = (info.rela, info.relasz, info.relaent) else { return None };
			if relaent != core::mem::size_of::<Rela>() as u64 || relasz % relaent != 0 || self.virtual_data(rela, relasz).is_none() {
				return None;
			}
			if info.relacount.is_some_and(|count| count > relasz / relaent) {
				return None;
			}
		}
		if info.strtab.is_some() || info.strsz.is_some() {
			let (Some(strtab), Some(strsz)) = (info.strtab, info.strsz) else { return None };
			if self.virtual_data(strtab, strsz).is_none() {
				return None;
			}
		}
		if info.jmprel.is_some() || info.pltrelsz.is_some() || info.pltrel.is_some() {
			let (Some(jmprel), Some(pltrelsz), Some(pltrel)) = (info.jmprel, info.pltrelsz, info.pltrel) else { return None };
			if pltrel != DT_RELA as u64 || pltrelsz % core::mem::size_of::<Rela>() as u64 != 0 || self.virtual_data(jmprel, pltrelsz).is_none() {
				return None;
			}
		}
		if info.symtab.is_some() != info.syment.is_some() || info.syment.is_some_and(|size| size != 24) {
			return None;
		}
		if info.hash.is_some() && (info.symtab.is_none() || info.strtab.is_none()) {
			return None;
		}
		Some(Some(info))
	}

	pub fn rela_entries(&self, info: &DynamicInfo) -> Option<RelaEntries<'a>> {
		let (Some(address), Some(len), Some(entry_len)) = (info.rela, info.relasz, info.relaent) else {
			return Some(RelaEntries { bytes: &[], offset: 0 });
		};
		if entry_len != core::mem::size_of::<Rela>() as u64 || len % entry_len != 0 {
			return None;
		}
		Some(RelaEntries { bytes: self.virtual_data(address, len)?, offset: 0 })
	}

	pub fn plt_rela_entries(&self, info: &DynamicInfo) -> Option<RelaEntries<'a>> {
		let (Some(address), Some(len), Some(kind)) = (info.jmprel, info.pltrelsz, info.pltrel) else {
			return Some(RelaEntries { bytes: &[], offset: 0 });
		};
		if kind != DT_RELA as u64 || len % core::mem::size_of::<Rela>() as u64 != 0 {
			return None;
		}
		Some(RelaEntries { bytes: self.virtual_data(address, len)?, offset: 0 })
	}

	pub fn needed_names(&self, info: &DynamicInfo) -> Option<NeededNames<'a>> {
		let mut offsets = [0u64; 64];
		let mut count = 0usize;
		for entry in self.dynamic_entries()?.into_iter().flatten() {
			if entry.tag == DT_NULL {
				break;
			}
			if entry.tag == DT_NEEDED {
				if count == offsets.len() {
					return None;
				}
				offsets[count] = entry.value;
				count += 1;
			}
		}
		if count == 0 {
			return Some(NeededNames { strings: &[], offsets, count: 0, index: 0 });
		}
		let strings = self.virtual_data(info.strtab?, info.strsz?)?;
		for offset in offsets.iter().take(count) {
			string_at(strings, *offset)?;
		}
		Some(NeededNames { strings, offsets, count, index: 0 })
	}

	pub fn symbols(&self, info: &DynamicInfo) -> Option<Symbols<'a>> {
		let hash_address = info.hash?;
		let header = self.virtual_data(hash_address, 8)?;
		let bucket_count = u32::from_le_bytes(header[..4].try_into().ok()?) as u64;
		let symbol_count = u32::from_le_bytes(header[4..8].try_into().ok()?) as u64;
		if bucket_count == 0 || symbol_count == 0 || symbol_count > 65_536 {
			return None;
		}
		let hash_words = bucket_count.checked_add(symbol_count)?;
		let hash_len = 8u64.checked_add(hash_words.checked_mul(4)?)?;
		self.virtual_data(hash_address, hash_len)?;
		let symbol_bytes = symbol_count.checked_mul(core::mem::size_of::<Symbol>() as u64)?;
		let symbols = self.virtual_data(info.symtab?, symbol_bytes)?;
		let strings = self.virtual_data(info.strtab?, info.strsz?)?;
		for index in 0..symbol_count as usize {
			let offset = index.checked_mul(core::mem::size_of::<Symbol>())?;
			let symbol = unsafe { core::ptr::read_unaligned(symbols.get(offset..offset + core::mem::size_of::<Symbol>())?.as_ptr() as *const Symbol) };
			string_at(strings, symbol.name as u64)?;
		}
		Some(Symbols { symbols, strings, index: 0 })
	}

	pub fn symbol(&self, info: &DynamicInfo, index: u32) -> Option<(Symbol, &'a str)> {
		self.symbols(info)?.nth(index as usize)
	}
}

pub struct DynamicEntries<'a> {
	bytes: &'a [u8],
	offset: usize,
	terminated: bool,
}

impl Iterator for DynamicEntries<'_> {
	type Item = DynamicEntry;

	fn next(&mut self) -> Option<DynamicEntry> {
		if self.terminated || self.offset == self.bytes.len() {
			return None;
		}
		let end = self.offset.checked_add(core::mem::size_of::<DynamicEntry>())?;
		let entry = unsafe { core::ptr::read_unaligned(self.bytes.get(self.offset..end)?.as_ptr() as *const DynamicEntry) };
		self.offset = end;
		if entry.tag == DT_NULL {
			self.terminated = true;
		}
		Some(entry)
	}
}

impl DynamicEntries<'_> {
	pub fn is_terminated(&self) -> bool {
		self.terminated
	}
}

pub struct RelaEntries<'a> {
	bytes: &'a [u8],
	offset: usize,
}

impl Iterator for RelaEntries<'_> {
	type Item = Rela;

	fn next(&mut self) -> Option<Rela> {
		if self.offset == self.bytes.len() {
			return None;
		}
		let end = self.offset.checked_add(core::mem::size_of::<Rela>())?;
		let entry = unsafe { core::ptr::read_unaligned(self.bytes.get(self.offset..end)?.as_ptr() as *const Rela) };
		self.offset = end;
		Some(entry)
	}
}

pub struct NeededNames<'a> {
	strings: &'a [u8],
	offsets: [u64; 64],
	count: usize,
	index: usize,
}

impl<'a> Iterator for NeededNames<'a> {
	type Item = &'a str;

	fn next(&mut self) -> Option<&'a str> {
		if self.index == self.count {
			return None;
		}
		let offset = self.offsets[self.index];
		self.index += 1;
		string_at(self.strings, offset)
	}
}

fn string_at(strings: &[u8], offset: u64) -> Option<&str> {
	let start = usize::try_from(offset).ok()?;
	let tail = strings.get(start..)?;
	let end = tail.iter().position(|byte| *byte == 0)?;
	core::str::from_utf8(&tail[..end]).ok()
}

pub struct Symbols<'a> {
	symbols: &'a [u8],
	strings: &'a [u8],
	index: usize,
}

impl<'a> Iterator for Symbols<'a> {
	type Item = (Symbol, &'a str);

	fn next(&mut self) -> Option<Self::Item> {
		let entry_len = core::mem::size_of::<Symbol>();
		let offset = self.index.checked_mul(entry_len)?;
		if offset == self.symbols.len() {
			return None;
		}
		let end = offset.checked_add(entry_len)?;
		let symbol = unsafe { core::ptr::read_unaligned(self.symbols.get(offset..end)?.as_ptr() as *const Symbol) };
		self.index += 1;
		let name_start = symbol.name as usize;
		let tail = self.strings.get(name_start..)?;
		let name_end = tail.iter().position(|byte| *byte == 0)?;
		let name = core::str::from_utf8(&tail[..name_end]).ok()?;
		Some((symbol, name))
	}
}

#[cfg(test)]
#[path = "elf/tests.rs"]
mod tests;
