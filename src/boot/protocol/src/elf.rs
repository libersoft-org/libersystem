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
// ELF identification / header fields validated on parse.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
// The only ELF version there is, in both places the format writes it: `e_ident[EI_VERSION]` and
// `e_version`. Neither was checked, so a file that declared no version - or a version whose layout
// this reader does not implement - was accepted and then read as though it had said version 1. That
// is fail-open on the field whose entire job is to say which layout follows.
const EV_CURRENT: u8 = 1;
const EI_VERSION: usize = 6;
#[cfg(test)]
const SHT_STRTAB: u32 = 3;
const SHT_NOTE: u32 = 7;
const SHF_ALLOC: u64 = 1 << 1;
pub const MAX_LIBER_IDENTITY_RECORD_BYTES: usize = 8 * 1024;
const LIBER_IDENTITY_SECTION: &[u8] = b".note.liber.identity";
const LIBER_IDENTITY_NOTE_NAME: &[u8] = b"LIBER\0";
const LIBER_IDENTITY_NOTE_TYPE: u32 = 1;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynamicRelocationKind {
	Relative,
	Symbol,
}

impl DynamicRelocationKind {
	pub const fn accepts_symbol(self, symbol: u32) -> bool {
		match self {
			Self::Relative => symbol == 0,
			Self::Symbol => true,
		}
	}
}

pub const fn expected_machine() -> u16 {
	EXPECTED_MACHINE
}

pub const fn dynamic_relocation_kind(machine: u16, relocation: u32) -> Option<DynamicRelocationKind> {
	match machine {
		EM_X86_64 => match relocation {
			8 => Some(DynamicRelocationKind::Relative),
			1 | 6 | 7 => Some(DynamicRelocationKind::Symbol),
			_ => None,
		},
		EM_AARCH64 => match relocation {
			1027 => Some(DynamicRelocationKind::Relative),
			257 | 1025 | 1026 => Some(DynamicRelocationKind::Symbol),
			_ => None,
		},
		EM_RISCV => match relocation {
			3 => Some(DynamicRelocationKind::Relative),
			2 | 5 => Some(DynamicRelocationKind::Symbol),
			_ => None,
		},
		_ => None,
	}
}

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
pub const MAX_DYNAMIC_MODULES: usize = 64;
pub const MAX_DYNAMIC_DEPENDENCY_DEPTH: usize = 16;

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
#[derive(Clone, Copy)]
struct SectionHeader {
	sh_name: u32,
	sh_type: u32,
	sh_flags: u64,
	sh_addr: u64,
	sh_offset: u64,
	sh_size: u64,
	sh_link: u32,
	sh_info: u32,
	sh_addralign: u64,
	sh_entsize: u64,
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
	shoff: u64,
	shentsize: u16,
	shnum: u16,
	shstrndx: u16,
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
		// BOTH VERSION FIELDS, because the format writes it twice and a reader that checks neither
		// is trusting a layout the file never claimed. `e_version` is a `u32` and the identification
		// byte is a `u8`; both must say 1.
		if header.e_ident[EI_VERSION] != EV_CURRENT || header.e_version != EV_CURRENT as u32 {
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
		let image = Self { bytes, image_type: header.e_type, entry: header.e_entry, phoff: header.e_phoff, phentsize: header.e_phentsize, phnum: header.e_phnum, shoff: header.e_shoff, shentsize: header.e_shentsize, shnum: header.e_shnum, shstrndx: header.e_shstrndx };
		// EVERY PT_LOAD VALIDATED HERE, once, rather than at each of the four places that load one.
		//
		// The parser bounded `p_offset .. p_offset + p_filesz` against the file and stopped there,
		// so each backend sized its allocation from `p_memsz` and copied `p_filesz` bytes into it -
		// and a header declaring `p_memsz = 4096, p_filesz = 65536` reserved one page and wrote
		// sixty-four kilobytes of firmware memory. The kernel's own loader is not exposed (it clamps
		// every per-page copy), so this was the loader alone, reading the boot medium; that is still
		// the wrong answer to a malformed image, and it is one comparison.
		for i in 0..image.segment_count() {
			let Some(ph) = image.segment(i) else {
				return None;
			};
			if ph.p_type != PT_LOAD {
				continue;
			}
			// More file bytes than memory to put them in.
			// THE PHYSICAL END, bounded once here rather than at each of the three places that
			// compute it.
			//
			// `p_filesz > p_memsz` was checked and every FILE offset was `checked_add`, and nothing
			// bounded `p_paddr + p_memsz` - which `reserve_kernel`, the aarch64 placement and the
			// riscv64 staging all compute, in release builds, silently. `p_paddr =
			// 0xffff_ffff_ffff_f000` with `p_memsz = 0x3000` wraps in all three.
			//
			// Here, before any backend sees the header, is the only version of this that a fourth
			// backend cannot forget.
			if ph.p_paddr.checked_add(ph.p_memsz).is_none() {
				return None;
			}
			// AND THE VIRTUAL END, for the same reason one line up. The x86 mapper walks
			// `virt + i * PAGE_SIZE` over this span unchecked, and a `PT_LOAD` whose virtual span
			// wraps is not a meaningful segment for any consumer - so the refusal belongs beside its
			// neighbour rather than in each backend, which is the argument that put the physical one
			// here.
			if ph.p_vaddr.checked_add(ph.p_memsz).is_none() {
				return None;
			}
			if ph.p_filesz > ph.p_memsz {
				return None;
			}
			// File bytes the file does not contain. Every backend treated this as an all-BSS
			// segment and booted it; a segment that declares contents it has not got is a
			// malformed executable, and the image is the thing to refuse.
			if ph.p_filesz > 0 && image.segment_data(&ph).is_none() {
				return None;
			}
			// NOT W^X HERE. It was, and it refused two of this project's own kernels: the aarch64
			// and riscv64 images link their boot stub - the code that runs with the MMU off and
			// then turns it on - as a single `RWE` segment, so the loader panicked at `parse` on
			// both architectures before it had loaded anything. The claim that "the kernel does not
			// have such a segment" was measured on x86_64 alone.
			//
			// W^X is a policy about what may be MAPPED, not a property of a well-formed ELF, so it
			// belongs where the mapping happens and where the image's provenance is known: the
			// kernel's userspace loader refuses it for every program it loads (`elf.rs`), and the
			// x86_64 loader asserts it for the kernel image, whose mapper derives `WRITABLE` and
			// `NX` from the flags independently.
			// THE ELF CONGRUENCE, which is what the format actually requires: a loadable segment
			// must satisfy `p_vaddr = p_offset (mod p_align)`, so a single file mapping places the
			// bytes at the right offset within their page.
			//
			// Absolute 4 KiB alignment was demanded here first, and it is not the rule. Every
			// shared object in the tree is linked with `p_align = 0x10000` and non-page-aligned
			// vaddrs (`0x3478c`), so this refused `lsrt.lslib` and the whole aarch64 image build
			// stopped at "no valid target ELF". What the x86 kernel loader needs - a page-aligned
			// LOAD address, because it copies to `phys + 0` and maps `align_down(p_vaddr)` - is a
			// requirement of THAT loader and is asserted there, next to the code that assumes it.
			if ph.p_align > 1 {
				if !ph.p_align.is_power_of_two() {
					return None;
				}
				if ph.p_vaddr % ph.p_align != ph.p_offset % ph.p_align {
					return None;
				}
			}
		}
		Some(image)
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

	fn section(&self, index: usize) -> Option<SectionHeader> {
		if self.shentsize as usize != core::mem::size_of::<SectionHeader>() || index >= self.shnum as usize {
			return None;
		}
		let start = usize::try_from(self.shoff).ok()?.checked_add(index.checked_mul(self.shentsize as usize)?)?;
		let end = start.checked_add(core::mem::size_of::<SectionHeader>())?;
		Some(unsafe { core::ptr::read_unaligned(self.bytes.get(start..end)?.as_ptr() as *const SectionHeader) })
	}

	fn section_data(&self, section: &SectionHeader) -> Option<&'a [u8]> {
		let start = usize::try_from(section.sh_offset).ok()?;
		let end = start.checked_add(usize::try_from(section.sh_size).ok()?)?;
		self.bytes.get(start..end)
	}

	pub fn liber_identity_note(&self) -> Option<&'a [u8]> {
		if self.shnum == 0 || self.shstrndx >= self.shnum {
			return None;
		}
		let strings = self.section_data(&self.section(self.shstrndx as usize)?)?;
		let mut record = None;
		for index in 0..self.shnum as usize {
			let section = self.section(index)?;
			if section.sh_type != SHT_NOTE || section.sh_flags & SHF_ALLOC == 0 || section_name(strings, section.sh_name)? != LIBER_IDENTITY_SECTION {
				continue;
			}
			let note = self.section_data(&section)?;
			let name_len = usize::try_from(u32::from_le_bytes(note.get(0..4)?.try_into().ok()?)).ok()?;
			let record_len = usize::try_from(u32::from_le_bytes(note.get(4..8)?.try_into().ok()?)).ok()?;
			let name_end = 12usize.checked_add(name_len)?;
			let record_start = name_end.checked_add(3)? & !3;
			let record_end = record_start.checked_add(record_len)?;
			let padded_record_end = record_end.checked_add(3)? & !3;
			if record_len == 0 || record_len > MAX_LIBER_IDENTITY_RECORD_BYTES || u32::from_le_bytes(note.get(8..12)?.try_into().ok()?) != LIBER_IDENTITY_NOTE_TYPE || note.get(12..name_end)? != LIBER_IDENTITY_NOTE_NAME || note.get(name_end..record_start)?.iter().any(|byte| *byte != 0) || note.get(record_end..padded_record_end)?.iter().any(|byte| *byte != 0) || note.len() != padded_record_end {
				return None;
			}
			let value = note.get(record_start..record_end)?;
			if record.replace(value).is_some() {
				return None;
			}
		}
		record
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

fn section_name(strings: &[u8], offset: u32) -> Option<&[u8]> {
	let tail = strings.get(offset as usize..)?;
	let end = tail.iter().position(|byte| *byte == 0)?;
	Some(&tail[..end])
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

// THE ONE PLACE THAT SAYS WHAT A LOADABLE IMAGE MUST SATISFY (LDR-011).
//
// Three backends load the kernel and each checked a different subset. x86_64 asserted page
// alignment and W^X in a loop of its own; aarch64 asserted `ET_EXEC` and nothing about the segments
// beyond what the parser had already refused; riscv64 walked the headers to find the destination
// span and checked neither. NOTHING anywhere asked whether two segments overlap, or whether the
// entry point lands inside a segment that was loaded and is executable - so an image whose entry is
// a number in the middle of `.bss`, or one whose two `PT_LOAD`s are written over each other, was
// loaded and jumped to.
//
// The checks are the same checks on all three; what differs is which of them each backend is
// entitled to demand, which is what `LoadRules` carries. A boot stub is one `RWE` segment by
// construction, so W^X is a rule about a KERNEL image and not about the format - that is why this
// takes rules instead of applying all of them always, and why the reason lives here rather than in a
// comment beside one backend's `assert!`.
//
// Bounded and allocation-free: `MAX_LOAD_SEGMENTS` is the array the loader already sizes its own
// segment table to, and the pairwise overlap check is quadratic in a number this caps. An image
// declaring more is refused rather than half-checked.
pub const MAX_LOAD_SEGMENTS: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoadPlanError {
	// The image is `ET_DYN` where the caller places at link addresses and computes no bias.
	NotExecutable,
	// Nothing to load: every header is non-`PT_LOAD` or has `p_memsz == 0`.
	NoLoadableSegment,
	// More `PT_LOAD` segments than this can reason about.
	TooManyLoadableSegments,
	// A segment's virtual or physical base is not on a page boundary.
	NotPageAligned(usize),
	// A segment is both writable and executable.
	WritableAndExecutable(usize),
	// Rounding a segment's end up to a page boundary does not fit in 64 bits. The raw end is the
	// PARSER's business and it refuses one that wraps; this is the rounding on top of it.
	SpanWraps(usize),
	// Two segments claim the same page, virtually or physically.
	Overlap(usize, usize),
	// The entry point is not inside a loaded, executable segment.
	EntryNotInAnExecutableSegment,
}

// What a caller is entitled to demand of the image it is about to load.
#[derive(Clone, Copy)]
pub struct LoadRules {
	pub page_size: u64,
	// `ET_EXEC` only. A caller that computes a load bias and applies relocations sets this false.
	pub require_executable_type: bool,
	// Page-aligned segment bases. A caller that places segments wherever it is given memory - the
	// x86_64 backend maps `p_vaddr` to firmware-chosen physical pages - still wants this, because
	// the mapping is by page.
	pub require_page_aligned: bool,
	// No segment both writable and executable.
	pub require_w_xor_x: bool,
	// The entry point lands inside a loaded segment that is executable.
	pub require_entry_in_executable_segment: bool,
}

impl LoadRules {
	// What a kernel image must satisfy. Every backend loads one of those, so this is the shape they
	// share; a caller wanting less says which rule it is dropping and why, at the call.
	pub const fn kernel(page_size: u64) -> LoadRules {
		LoadRules { page_size, require_executable_type: true, require_page_aligned: true, require_w_xor_x: true, require_entry_in_executable_segment: true }
	}
}

// The answer, and the numbers a backend would otherwise recompute from the same walk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LoadPlan {
	pub entry: u64,
	pub segments: usize,
	// Page-aligned bounds over every loadable segment, virtual and physical. The two backends that
	// place AT the link address size their staging from the physical pair.
	pub virt_low: u64,
	pub virt_high: u64,
	pub phys_low: u64,
	pub phys_high: u64,
}

const fn align_down_to(value: u64, page: u64) -> u64 {
	value & !(page - 1)
}

// `None` when the rounding wraps, which is a refusal and not a clamp.
const fn align_up_to(value: u64, page: u64) -> Option<u64> {
	match value.checked_add(page - 1) {
		Some(sum) => Some(sum & !(page - 1)),
		None => None,
	}
}

pub fn load_plan(image: &Elf<'_>, rules: LoadRules) -> Result<LoadPlan, LoadPlanError> {
	if rules.require_executable_type && image.image_type != ET_EXEC {
		return Err(LoadPlanError::NotExecutable);
	}
	// (index as the image numbers it, virtual span, physical span, flags) per loadable segment.
	let mut spans: [(usize, u64, u64, u64, u64, u32); MAX_LOAD_SEGMENTS] = [(0, 0, 0, 0, 0, 0); MAX_LOAD_SEGMENTS];
	let mut count = 0usize;
	let page = rules.page_size;
	for index in 0..image.segment_count() {
		let Some(ph) = image.segment(index) else { continue };
		if ph.p_type != PT_LOAD || ph.p_memsz == 0 {
			continue;
		}
		if count == MAX_LOAD_SEGMENTS {
			return Err(LoadPlanError::TooManyLoadableSegments);
		}
		// NOT RE-CHECKED HERE, and named so the division of labour is readable: `Elf::parse`
		// already refuses `p_filesz > p_memsz`, a `p_vaddr + p_memsz` or `p_paddr + p_memsz` that
		// wraps, a `p_offset .. p_offset + p_filesz` outside the file, and the `p_vaddr = p_offset
		// (mod p_align)` congruence. This function takes a PARSED image, so restating those would
		// be two places to change and one of them silently right.
		if rules.require_page_aligned && (ph.p_vaddr % page != 0 || ph.p_paddr % page != 0) {
			return Err(LoadPlanError::NotPageAligned(index));
		}
		if rules.require_w_xor_x && ph.p_flags & PF_W != 0 && ph.p_flags & PF_X != 0 {
			return Err(LoadPlanError::WritableAndExecutable(index));
		}
		let (Some(vend), Some(pend)) = (ph.p_vaddr.checked_add(ph.p_memsz), ph.p_paddr.checked_add(ph.p_memsz)) else {
			return Err(LoadPlanError::SpanWraps(index));
		};
		let (Some(vtop), Some(ptop)) = (align_up_to(vend, page), align_up_to(pend, page)) else {
			return Err(LoadPlanError::SpanWraps(index));
		};
		spans[count] = (index, align_down_to(ph.p_vaddr, page), vtop, align_down_to(ph.p_paddr, page), ptop, ph.p_flags);
		count += 1;
	}
	if count == 0 {
		return Err(LoadPlanError::NoLoadableSegment);
	}
	// PAGE SPANS, not byte spans. Two segments that do not overlap by a byte but share a page still
	// collide: the loader zeroes and copies by the page, so the second write erases the first's tail.
	let mut i = 0;
	while i < count {
		let mut j = i + 1;
		while j < count {
			let (a, alow, ahigh, aplow, aphigh, _) = spans[i];
			let (b, blow, bhigh, bplow, bphigh, _) = spans[j];
			if (alow < bhigh && blow < ahigh) || (aplow < bphigh && bplow < aphigh) {
				return Err(LoadPlanError::Overlap(a, b));
			}
			j += 1;
		}
		i += 1;
	}
	if rules.require_entry_in_executable_segment {
		let mut inside = false;
		let mut index = 0;
		while index < count {
			let (_, low, high, _, _, flags) = spans[index];
			if flags & PF_X != 0 && image.entry >= low && image.entry < high {
				inside = true;
				break;
			}
			index += 1;
		}
		if !inside {
			return Err(LoadPlanError::EntryNotInAnExecutableSegment);
		}
	}
	let mut plan = LoadPlan { entry: image.entry, segments: count, virt_low: u64::MAX, virt_high: 0, phys_low: u64::MAX, phys_high: 0 };
	let mut index = 0;
	while index < count {
		let (_, vlow, vhigh, plow, phigh, _) = spans[index];
		if vlow < plan.virt_low {
			plan.virt_low = vlow;
		}
		if vhigh > plan.virt_high {
			plan.virt_high = vhigh;
		}
		if plow < plan.phys_low {
			plan.phys_low = plow;
		}
		if phigh > plan.phys_high {
			plan.phys_high = phigh;
		}
		index += 1;
	}
	Ok(plan)
}

#[cfg(test)]
#[path = "elf/tests.rs"]
mod tests;
