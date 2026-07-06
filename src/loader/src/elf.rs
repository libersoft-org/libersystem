// Minimal ELF64 reader for the loader: just enough to walk an executable's
// program headers and load its PT_LOAD segments. The kernel is linked as a
// static, non-relocatable ET_EXEC (relocation-model=static), so the loader
// applies no relocations - it copies each segment to the physical memory it
// backs the segment's link-time virtual address with, honoring the segment's
// R/W/X flags when it maps them.

#![allow(dead_code)]

// ELF identification / header offsets we validate.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;

// Program-header type: a loadable segment.
pub const PT_LOAD: u32 = 1;

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

// A parsed, validated ELF64 image over its in-memory bytes.
pub struct Elf<'a> {
	bytes: &'a [u8],
	pub entry: u64,
	phoff: u64,
	phentsize: u16,
	phnum: u16,
}

impl<'a> Elf<'a> {
	// Validate the header and capture the entry point + program-header table
	// location. Returns None if the bytes are not a little-endian 64-bit x86-64
	// ET_EXEC image or are truncated.
	pub fn parse(bytes: &'a [u8]) -> Option<Self> {
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
		if header.e_type != ET_EXEC || header.e_machine != EM_X86_64 {
			return None;
		}
		Some(Self { bytes, entry: header.e_entry, phoff: header.e_phoff, phentsize: header.e_phentsize, phnum: header.e_phnum })
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
		let off = self.phoff as usize + i * self.phentsize as usize;
		if off + core::mem::size_of::<ProgramHeader>() > self.bytes.len() {
			return None;
		}
		// SAFETY: bounds-checked above; unaligned read.
		Some(unsafe { core::ptr::read_unaligned(self.bytes.as_ptr().add(off) as *const ProgramHeader) })
	}

	// The file bytes backing a segment's on-disk portion (p_offset .. p_offset +
	// p_filesz), or None if that range lies outside the file.
	pub fn segment_data(&self, ph: &ProgramHeader) -> Option<&'a [u8]> {
		let start = ph.p_offset as usize;
		let end = start.checked_add(ph.p_filesz as usize)?;
		self.bytes.get(start..end)
	}
}
