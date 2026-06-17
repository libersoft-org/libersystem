// x86_64 4-level paging: map and unmap single 4 KiB pages.
//
// Operates on the page-table hierarchy currently active in CR3 (the one Limine
// set up, which already maps the kernel image and the HHDM). Page tables are
// reached physically through the HHDM, and intermediate tables are allocated
// from the frame allocator on demand.
//
// Scope for M1: 4 KiB pages only, no huge pages, and unmapping does not reclaim
// now-empty intermediate tables (a deliberate, documented simplification).

#![allow(dead_code)]

use core::arch::asm;

use crate::mem::frame;

// Page-table entry flags.
pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const NO_CACHE: u64 = 1 << 4; // PCD: disable caching (for MMIO)

// Physical address bits within a page-table entry (bits 12..=51).
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
const ENTRY_COUNT: usize = 512;

fn hhdm() -> u64 {
	crate::mem::hhdm_offset()
}

fn active_pml4_phys() -> u64 {
	let cr3: u64;
	unsafe { asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) };
	cr3 & ADDR_MASK
}

// Index into the table at the given level for a virtual address.
fn table_index(virt: u64, shift: u32) -> usize {
	((virt >> shift) & 0x1ff) as usize
}

// Pointer to the first entry of the table at physical address `phys`.
fn table_ptr(phys: u64) -> *mut u64 {
	(hhdm() + phys) as *mut u64
}

// Return the physical address of the next-level table for `index`, allocating
// and zeroing a fresh table if the entry is not yet present. `parent_flags`
// carries permission bits (notably USER) that every level of the walk must also
// grant; a user leaf is only reachable if each table along the way is USER too,
// so the bit is OR-ed into both new and pre-existing intermediate entries.
//
// SAFETY: `table` must point at a valid 512-entry page table.
unsafe fn next_table_create(table: *mut u64, index: usize, parent_flags: u64) -> u64 {
	let entry = table.add(index);
	let value = entry.read_volatile();
	if value & PRESENT == 0 {
		let new = frame::allocate().expect("out of frames: page table");
		let new_table = table_ptr(new);
		for i in 0..ENTRY_COUNT {
			new_table.add(i).write_volatile(0);
		}
		entry.write_volatile((new & ADDR_MASK) | PRESENT | WRITABLE | parent_flags);
		new
	} else {
		// Widen an existing intermediate to also grant the requested bits.
		if value & parent_flags != parent_flags {
			entry.write_volatile(value | parent_flags);
		}
		value & ADDR_MASK
	}
}

// Map `virt` to physical frame `phys` with `flags` (PRESENT is always set).
pub fn map_page(virt: u64, phys: u64, flags: u64) {
	// Permission bits the whole walk must grant for the leaf to be reachable.
	let parent_flags = flags & USER;
	unsafe {
		let pml4 = table_ptr(active_pml4_phys());
		let pdpt = table_ptr(next_table_create(pml4, table_index(virt, 39), parent_flags));
		let pd = table_ptr(next_table_create(pdpt, table_index(virt, 30), parent_flags));
		let pt = table_ptr(next_table_create(pd, table_index(virt, 21), parent_flags));
		let entry = pt.add(table_index(virt, 12));
		entry.write_volatile((phys & ADDR_MASK) | flags | PRESENT);
		invlpg(virt);
	}
}

// Unmap `virt` if it is mapped, returning the physical frame it pointed at.
// Intermediate tables are left in place (not reclaimed) in M1.
pub fn unmap_page(virt: u64) -> Option<u64> {
	unsafe {
		let pml4 = table_ptr(active_pml4_phys());
		let pdpt = table_ptr(next_table_walk(pml4, table_index(virt, 39))?);
		let pd = table_ptr(next_table_walk(pdpt, table_index(virt, 30))?);
		let pt = table_ptr(next_table_walk(pd, table_index(virt, 21))?);
		let entry = pt.add(table_index(virt, 12));
		let value = entry.read_volatile();
		if value & PRESENT == 0 {
			return None;
		}
		entry.write_volatile(0);
		invlpg(virt);
		Some(value & ADDR_MASK)
	}
}

// Return the physical address of the next-level table, or None if not present.
//
// SAFETY: `table` must point at a valid 512-entry page table.
unsafe fn next_table_walk(table: *mut u64, index: usize) -> Option<u64> {
	let entry = table.add(index).read_volatile();
	if entry & PRESENT == 0 {
		None
	} else {
		Some(entry & ADDR_MASK)
	}
}

// Invalidate the TLB entry for a single page.
unsafe fn invlpg(virt: u64) {
	asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
}
