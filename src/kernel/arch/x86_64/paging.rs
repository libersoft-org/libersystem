// x86_64 4-level paging: map and unmap single 4 kB pages.
//
// Operates on the page-table hierarchy currently active in CR3 (the one Limine
// set up, which already maps the kernel image and the HHDM). Page tables are
// reached physically through the HHDM, and intermediate tables are allocated
// from the frame allocator on demand.
//
// Scope for M1: 4 kB pages only, no huge pages, and unmapping does not reclaim
// now-empty intermediate tables (a deliberate, documented simplification).

#![allow(dead_code)]

use core::arch::asm;

use crate::mem::frame;

// Page-table entry flags.
pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const NO_CACHE: u64 = 1 << 4; // PCD: disable caching (for MMIO)
pub const NO_EXECUTE: u64 = 1 << 63; // NX: data pages are never fetched from (needs EFER.NXE)

// Physical address bits within a page-table entry (bits 12..=51).
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
const ENTRY_COUNT: usize = 512;

// Whether the CPU supports the NX bit and EFER.NXE has been enabled. When it has
// not (an old CPU), map_page_in strips NO_EXECUTE so the bit is never a reserved-
// bit violation - the mapping is then executable, matching the hardware's best.
static NX_ENABLED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// Enable no-execute enforcement on the current core: check the NX capability
// (CPUID 0x8000_0001, EDX bit 20) and set EFER.NXE. Must run on every core (EFER
// is per-core) BEFORE any page carrying NO_EXECUTE is touched - the BSP calls it
// with the descriptor tables, each AP first thing in its bring-up.
pub fn enable_nx() {
	let nx_capable = core::arch::x86_64::__cpuid(0x8000_0001).edx & (1 << 20) != 0;
	if !nx_capable {
		return;
	}
	const IA32_EFER: u32 = 0xc000_0080;
	const EFER_NXE: u64 = 1 << 11;
	let efer = super::msr::read(IA32_EFER);
	super::msr::write(IA32_EFER, efer | EFER_NXE);
	NX_ENABLED.store(true, core::sync::atomic::Ordering::Release);
}

// Whether NX is enforced (tests assert the W^X behaviour only where it can hold).
pub fn nx_enabled() -> bool {
	NX_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

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
	unsafe {
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
}

// Map `virt` to physical frame `phys` with `flags` (PRESENT is always set) in the
// currently active address space.
pub fn map_page(virt: u64, phys: u64, flags: u64) {
	map_page_in(active_pml4_phys(), virt, phys, flags);
}

// Map `virt` to `phys` in the address space rooted at `pml4_phys`. The mapping
// takes effect immediately if that address space is the active one; otherwise it
// becomes visible when CR3 is loaded with it (which flushes the TLB anyway), so
// the invlpg here is harmless for a non-active space.
pub fn map_page_in(pml4_phys: u64, virt: u64, phys: u64, flags: u64) {
	// Without EFER.NXE the NX bit is a reserved bit; strip it so old CPUs still map.
	let flags = if nx_enabled() { flags } else { flags & !NO_EXECUTE };
	// Permission bits the whole walk must grant for the leaf to be reachable. NX
	// stays leaf-only: an intermediate NX would blanket the whole subtree.
	let parent_flags = flags & USER;
	unsafe {
		let pml4 = table_ptr(pml4_phys);
		let pdpt = table_ptr(next_table_create(pml4, table_index(virt, 39), parent_flags));
		let pd = table_ptr(next_table_create(pdpt, table_index(virt, 30), parent_flags));
		let pt = table_ptr(next_table_create(pd, table_index(virt, 21), parent_flags));
		let entry = pt.add(table_index(virt, 12));
		entry.write_volatile((phys & ADDR_MASK) | flags | PRESENT);
		invlpg(virt);
	}
}

// Unmap `virt` in the active address space, returning the frame it pointed at.
pub fn unmap_page(virt: u64) -> Option<u64> {
	unmap_page_in(active_pml4_phys(), virt)
}

// Unmap `count` consecutive pages starting at `base` in the active address space -
// the contiguous-virtual mapping a frame-backed object holds. The frames are not
// freed here; the caller owns them.
pub fn unmap_pages(base: u64, count: usize) {
	for i in 0..count {
		unmap_page(base + i as u64 * frame::PAGE_SIZE);
	}
}

// Unmap `virt` in the address space rooted at `pml4_phys` if it is mapped,
// returning the physical frame it pointed at. Intermediate tables are left in
// place (not reclaimed here); free_address_space reclaims them wholesale.
pub fn unmap_page_in(pml4_phys: u64, virt: u64) -> Option<u64> {
	unsafe {
		let pml4 = table_ptr(pml4_phys);
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

// Create a fresh top-level page table for a new address space. The user half
// (PML4 entries 0..256) starts empty; the kernel half (256..512) is copied from
// the active (kernel) PML4 so every address space shares the same kernel
// mappings - the kernel stays mapped after a CR3 switch, and because the copied
// entries point at the kernel's own intermediate tables, later kernel mappings
// under an already-present PML4 entry are visible everywhere. Returns the
// physical address of the new PML4, or None if no frame is available.
//
// Assumes it runs with the kernel address space active (processes are created
// from kernel threads / the boot context), so the active PML4 is the kernel one.
pub fn new_address_space() -> Option<u64> {
	let pml4_phys = frame::allocate()?;
	unsafe {
		let dst = table_ptr(pml4_phys);
		let src = table_ptr(active_pml4_phys());
		for i in 0..256 {
			dst.add(i).write_volatile(0);
		}
		for i in 256..ENTRY_COUNT {
			dst.add(i).write_volatile(src.add(i).read_volatile());
		}
	}
	Some(pml4_phys)
}

// Tear down an address space created by new_address_space: free the user-half
// page-table structure (PML4 entries 0..256 and the PDPT/PD/PT tables beneath
// them) and the PML4 frame itself. The kernel half (256..512) is shared and is
// never freed. Leaf data frames are owned by whoever mapped them (a MemoryObject
// or the caller) and are not freed here.
pub fn free_address_space(pml4_phys: u64) {
	unsafe {
		let pml4 = table_ptr(pml4_phys);
		for i in 0..256 {
			let entry = pml4.add(i).read_volatile();
			if entry & PRESENT != 0 {
				free_table_level(entry & ADDR_MASK, 3);
			}
		}
		frame::deallocate(pml4_phys);
	}
}

// Recursively free the intermediate page tables below `phys`. `level` is 3 for a
// PDPT, 2 for a PD, 1 for a PT. A PT's entries point at data frames, which are
// not freed; only the table frames themselves are reclaimed. 4 kB pages only,
// so there are no huge-page leaves to special-case.
//
// SAFETY: `phys` must be the physical address of a valid page table at `level`.
unsafe fn free_table_level(phys: u64, level: u32) {
	unsafe {
		if level > 1 {
			let table = table_ptr(phys);
			for i in 0..ENTRY_COUNT {
				let entry = table.add(i).read_volatile();
				if entry & PRESENT != 0 {
					free_table_level(entry & ADDR_MASK, level - 1);
				}
			}
		}
		frame::deallocate(phys);
	}
}

// Return the physical address of the next-level table, or None if not present.
//
// SAFETY: `table` must point at a valid 512-entry page table.
unsafe fn next_table_walk(table: *mut u64, index: usize) -> Option<u64> {
	unsafe {
		let entry = table.add(index).read_volatile();
		if entry & PRESENT == 0 { None } else { Some(entry & ADDR_MASK) }
	}
}

// Translate a virtual address to its physical address in the active address space,
// or None if unmapped. Walks the 4-level table and handles a huge-page leaf at the
// PDPT (1 GB) or PD (2 MB) level - Limine often maps the framebuffer with 2 MB
// pages, so a 4 kB-only walk would misread it. The returned phys carries the
// in-page offset.
pub fn translate(virt: u64) -> Option<u64> {
	const PS: u64 = 1 << 7;
	unsafe {
		let pml4 = table_ptr(active_pml4_phys());
		let pdpt = table_ptr(next_table_walk(pml4, table_index(virt, 39))?);
		let pdpt_e = pdpt.add(table_index(virt, 30)).read_volatile();
		if pdpt_e & PRESENT == 0 {
			return None;
		}
		if pdpt_e & PS != 0 {
			return Some((pdpt_e & 0x000f_ffff_c000_0000) | (virt & 0x3fff_ffff));
		}
		let pd = table_ptr(pdpt_e & ADDR_MASK);
		let pd_e = pd.add(table_index(virt, 21)).read_volatile();
		if pd_e & PRESENT == 0 {
			return None;
		}
		if pd_e & PS != 0 {
			return Some((pd_e & 0x000f_ffff_ffe0_0000) | (virt & 0x001f_ffff));
		}
		let pt = table_ptr(pd_e & ADDR_MASK);
		let pt_e = pt.add(table_index(virt, 12)).read_volatile();
		if pt_e & PRESENT == 0 {
			return None;
		}
		Some((pt_e & ADDR_MASK) | (virt & 0xfff))
	}
}

// Invalidate the TLB entry for a single page.
unsafe fn invlpg(virt: u64) {
	unsafe {
		asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
	}
}
