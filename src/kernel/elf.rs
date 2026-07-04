// A minimal ELF64 loader: maps the loadable segments of a statically-linked
// userspace executable into a target address space and returns its entry point.
// It is deliberately small - it accepts only little-endian x86-64 ELF64 images,
// maps each PT_LOAD segment at its link-time virtual address, and applies no
// relocations (the userspace programs are linked non-PIE at a fixed base). Page
// tables are edited through the address space; segment contents are written
// through the HHDM, since the target address space is not the active one.

#![allow(dead_code)]

use alloc::vec::Vec;

use crate::arch;
use crate::mem::frame::{self, PAGE_SIZE};
use crate::mem::hhdm_offset;
use crate::object::address_space::AddressSpace;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElfError {
	Truncated,
	BadMagic,
	BadClass,
	BadType,
	BadMachine,
	OutOfMemory,
}

const ELF_HEADER_LEN: usize = 64;
const PHDR_LEN: usize = 56;
const PT_LOAD: u32 = 1;
const PF_W: u32 = 0x2;
const PF_X: u32 = 0x1;

// Little-endian fixed-width readers over a byte slice at a static offset. The
// callers slice the ELF header and program headers at offsets that always fit the
// validated length, so the fixed-size conversion never fails.
fn rd_u16(b: &[u8], at: usize) -> u16 {
	u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

fn rd_u32(b: &[u8], at: usize) -> u32 {
	u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn rd_u64(b: &[u8], at: usize) -> u64 {
	u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

// Load `elf` into `addr_space`, recording every physical frame it allocates into
// `frames` so the caller can free them on teardown (frames pushed before an error
// are left in `frames` for the caller's cleanup). Returns the entry-point virtual
// address.
pub fn load_into(elf: &[u8], addr_space: &AddressSpace, frames: &mut Vec<u64>) -> Result<u64, ElfError> {
	if elf.len() < ELF_HEADER_LEN {
		return Err(ElfError::Truncated);
	}
	if &elf[0..4] != b"\x7fELF" {
		return Err(ElfError::BadMagic);
	}
	// 64-bit (class 2), little-endian (data 1) only.
	if elf[4] != 2 || elf[5] != 1 {
		return Err(ElfError::BadClass);
	}
	// ET_EXEC (2) or ET_DYN (3); both are loaded at p_vaddr with a zero bias.
	let e_type = rd_u16(elf, 16);
	if e_type != 2 && e_type != 3 {
		return Err(ElfError::BadType);
	}
	// EM_X86_64.
	let e_machine = rd_u16(elf, 18);
	if e_machine != 0x3e {
		return Err(ElfError::BadMachine);
	}
	let e_entry = rd_u64(elf, 24);
	let e_phoff = rd_u64(elf, 32) as usize;
	let e_phentsize = rd_u16(elf, 54) as usize;
	let e_phnum = rd_u16(elf, 56) as usize;

	for index in 0..e_phnum {
		let base = e_phoff + index * e_phentsize;
		let phdr = elf.get(base..base + PHDR_LEN).ok_or(ElfError::Truncated)?;
		let p_type = rd_u32(phdr, 0);
		if p_type != PT_LOAD {
			continue;
		}
		let p_flags = rd_u32(phdr, 4);
		let p_offset = rd_u64(phdr, 8) as usize;
		let p_vaddr = rd_u64(phdr, 16);
		let p_filesz = rd_u64(phdr, 32) as usize;
		let p_memsz = rd_u64(phdr, 40);
		map_segment(elf, addr_space, frames, p_flags, p_offset, p_vaddr, p_filesz, p_memsz)?;
	}
	Ok(e_entry)
}

// Map one PT_LOAD segment page by page: allocate a zeroed frame for each page,
// copy the file-backed bytes that fall in it, and map it at the segment's virtual
// address. Bytes past p_filesz (the .bss tail) stay zero. Assumes page-aligned,
// non-overlapping segments (the userspace linker script enforces this). W^X: a
// segment is writable or executable per its flags, never both implicitly - only
// PF_X segments are fetchable, everything else maps no-execute.
fn map_segment(elf: &[u8], addr_space: &AddressSpace, frames: &mut Vec<u64>, p_flags: u32, p_offset: usize, p_vaddr: u64, p_filesz: usize, p_memsz: u64) -> Result<(), ElfError> {
	let mut flags = arch::paging::PRESENT | arch::paging::USER;
	if p_flags & PF_W != 0 {
		flags |= arch::paging::WRITABLE;
	}
	if p_flags & PF_X == 0 {
		flags |= arch::paging::NO_EXECUTE;
	}
	let hhdm = hhdm_offset();
	let pages = p_memsz.div_ceil(PAGE_SIZE);
	for page in 0..pages {
		let frame = frame::allocate().ok_or(ElfError::OutOfMemory)?;
		frames.push(frame);
		let dst = (hhdm + frame) as *mut u8;
		unsafe {
			core::ptr::write_bytes(dst, 0, PAGE_SIZE as usize);
		}
		let page_start = (page * PAGE_SIZE) as usize;
		if page_start < p_filesz {
			let copy = (p_filesz - page_start).min(PAGE_SIZE as usize);
			let src_start = p_offset + page_start;
			let src = elf.get(src_start..src_start + copy).ok_or(ElfError::Truncated)?;
			unsafe {
				core::ptr::copy_nonoverlapping(src.as_ptr(), dst, copy);
			}
		}
		addr_space.map(p_vaddr + page * PAGE_SIZE, frame, flags);
	}
	Ok(())
}
