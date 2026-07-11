// Loads the loadable segments of a statically-linked userspace executable into a
// target address space and returns its entry point. Parsing is the shared
// `bootproto::elf` reader (the same one the boot loader uses for the kernel); this
// module is only the kernel's MAPPING half - it maps each PT_LOAD segment at its
// link-time virtual address and applies no relocations (userspace programs are linked
// non-PIE at a fixed base). Page tables are edited through the address space; segment
// contents are written through the HHDM, since the target address space is not active.

#![allow(dead_code)]

use alloc::vec::Vec;

use crate::arch;
use crate::mem::frame::{self, PAGE_SIZE};
use crate::mem::hhdm_offset;
use crate::object::address_space::AddressSpace;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElfError {
	// The bytes are not a valid ELF64 image for this architecture (bad magic / class
	// / type / machine, or truncated). Every granular parse failure the shared reader
	// can report collapses to this - callers only distinguish it from out-of-memory.
	BadImage,
	OutOfMemory,
}

// Load `elf` into `addr_space`, recording every physical frame it allocates into
// `frames` so the caller can free them on teardown (frames pushed before an error are
// left in `frames` for the caller's cleanup). Returns the entry-point virtual address.
pub fn load_into(elf: &[u8], addr_space: &AddressSpace, frames: &mut Vec<u64>) -> Result<u64, ElfError> {
	let image = bootproto::elf::Elf::parse(elf).ok_or(ElfError::BadImage)?;
	for i in 0..image.segment_count() {
		let ph = image.segment(i).ok_or(ElfError::BadImage)?;
		if ph.p_type != bootproto::elf::PT_LOAD {
			continue;
		}
		let data = image.segment_data(&ph).ok_or(ElfError::BadImage)?;
		map_segment(data, addr_space, frames, ph.p_flags, ph.p_vaddr, ph.p_memsz)?;
	}
	Ok(image.entry)
}

// Map one PT_LOAD segment page by page: allocate a zeroed frame for each page, copy
// the file-backed bytes (`data`, the segment's p_filesz portion) that fall in it, and
// map it at the segment's virtual address. Bytes past `data.len()` (the .bss tail) stay
// zero. Assumes page-aligned, non-overlapping segments (the userspace linker script
// enforces this). W^X: a segment is writable or executable per its flags, never both
// implicitly - only PF_X segments are fetchable, everything else maps no-execute.
fn map_segment(data: &[u8], addr_space: &AddressSpace, frames: &mut Vec<u64>, p_flags: u32, p_vaddr: u64, p_memsz: u64) -> Result<(), ElfError> {
	let mut flags = arch::paging::PRESENT | arch::paging::USER;
	if p_flags & bootproto::elf::PF_W != 0 {
		flags |= arch::paging::WRITABLE;
	}
	if p_flags & bootproto::elf::PF_X == 0 {
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
		if page_start < data.len() {
			let copy = (data.len() - page_start).min(PAGE_SIZE as usize);
			unsafe {
				core::ptr::copy_nonoverlapping(data.as_ptr().add(page_start), dst, copy);
			}
		}
		addr_space.try_map(p_vaddr + page * PAGE_SIZE, frame, flags).map_err(|_| ElfError::OutOfMemory)?;
	}
	Ok(())
}
