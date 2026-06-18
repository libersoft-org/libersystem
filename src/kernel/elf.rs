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
	let e_type = u16::from_le_bytes(elf[16..18].try_into().unwrap());
	if e_type != 2 && e_type != 3 {
		return Err(ElfError::BadType);
	}
	// EM_X86_64.
	let e_machine = u16::from_le_bytes(elf[18..20].try_into().unwrap());
	if e_machine != 0x3e {
		return Err(ElfError::BadMachine);
	}
	let e_entry = u64::from_le_bytes(elf[24..32].try_into().unwrap());
	let e_phoff = u64::from_le_bytes(elf[32..40].try_into().unwrap()) as usize;
	let e_phentsize = u16::from_le_bytes(elf[54..56].try_into().unwrap()) as usize;
	let e_phnum = u16::from_le_bytes(elf[56..58].try_into().unwrap()) as usize;

	for index in 0..e_phnum {
		let base = e_phoff + index * e_phentsize;
		let phdr = elf.get(base..base + PHDR_LEN).ok_or(ElfError::Truncated)?;
		let p_type = u32::from_le_bytes(phdr[0..4].try_into().unwrap());
		if p_type != PT_LOAD {
			continue;
		}
		let p_flags = u32::from_le_bytes(phdr[4..8].try_into().unwrap());
		let p_offset = u64::from_le_bytes(phdr[8..16].try_into().unwrap()) as usize;
		let p_vaddr = u64::from_le_bytes(phdr[16..24].try_into().unwrap());
		let p_filesz = u64::from_le_bytes(phdr[32..40].try_into().unwrap()) as usize;
		let p_memsz = u64::from_le_bytes(phdr[40..48].try_into().unwrap());
		map_segment(elf, addr_space, frames, p_flags, p_offset, p_vaddr, p_filesz, p_memsz)?;
	}
	Ok(e_entry)
}

// Map one PT_LOAD segment page by page: allocate a zeroed frame for each page,
// copy the file-backed bytes that fall in it, and map it at the segment's virtual
// address. Bytes past p_filesz (the .bss tail) stay zero. Assumes page-aligned,
// non-overlapping segments (the userspace linker script enforces this).
fn map_segment(elf: &[u8], addr_space: &AddressSpace, frames: &mut Vec<u64>, p_flags: u32, p_offset: usize, p_vaddr: u64, p_filesz: usize, p_memsz: u64) -> Result<(), ElfError> {
	let mut flags = arch::paging::PRESENT | arch::paging::USER;
	if p_flags & PF_W != 0 {
		flags |= arch::paging::WRITABLE;
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
