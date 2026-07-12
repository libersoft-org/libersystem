// Page-table construction for the hand-off. The loader builds a fresh 4-level
// x86-64 page hierarchy that the kernel runs on from the moment it takes over:
//
//   * the HHDM - all physical RAM (and the framebuffer) mapped at a fixed
//     higher-half offset with 2 MiB pages, so the kernel can reach any physical
//     address as `phys + hhdm_offset` (the framebuffer sub-range uncacheable);
//   * a low identity map over the same physical range, so the loader keeps
//     executing across the `mov cr3` before it jumps into the kernel;
//   * the kernel image mapped per PT_LOAD segment at its link-time higher-half
//     address, honoring W^X (writable xor executable) from the segment flags.
//
// Page-table pages are firmware AllocatePages allocations (LOADER_DATA); during
// boot services physical == virtual, so each freshly allocated table is written
// straight at its physical address.

use crate::uefi::{self, BootServices, PhysicalAddress};

pub const PAGE_SIZE: u64 = 4096;
pub const PAGE_2MB: u64 = 2 * 1024 * 1024;

// Page-table entry flags.
const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const PWT: u64 = 1 << 3;
const PCD: u64 = 1 << 4;
const HUGE: u64 = 1 << 7;
const NX: u64 = 1 << 63;

// Physical-address field of a page-table entry (bits 12..=51).
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

// The HHDM base: virt = phys + HHDM_OFFSET. Matches the offset the kernel expects
// (the conventional higher-half direct map base), so no kernel constant changes.
pub const HHDM_OFFSET: u64 = 0xffff_8000_0000_0000;

// Builds and owns the page hierarchy under construction.
pub struct PageTables {
	bs: *mut BootServices,
	pub pml4: PhysicalAddress,
}

impl PageTables {
	// Allocate a zeroed PML4 to root the new hierarchy.
	pub fn new(bs: *mut BootServices) -> Option<Self> {
		let pml4 = alloc_table(bs)?;
		Some(Self { bs, pml4 })
	}

	// Map `size` bytes of RAM starting at physical `phys` into the HHDM using
	// 2 MiB pages, writable and (optionally) uncacheable. `phys` and `size` must
	// be 2 MiB aligned.
	pub fn map_hhdm(&mut self, phys: u64, size: u64, uncacheable: bool) -> Option<()> {
		let mut off = 0u64;
		while off < size {
			let p = phys + off;
			let flags = PRESENT | WRITABLE | NX | if uncacheable { PCD | PWT } else { 0 };
			self.map_2mb(HHDM_OFFSET + p, p, flags)?;
			off += PAGE_2MB;
		}
		Some(())
	}

	// Identity-map `size` bytes from physical 0 with 2 MiB pages, so the loader's
	// own code (firmware-identity-mapped) stays valid across the CR3 switch until
	// it jumps to the kernel's higher-half entry. `size` must be 2 MiB aligned.
	pub fn map_identity(&mut self, size: u64) -> Option<()> {
		let mut p = 0u64;
		while p < size {
			self.map_2mb(p, p, PRESENT | WRITABLE)?;
			p += PAGE_2MB;
		}
		Some(())
	}

	// Map one kernel segment: `pages` 4 KiB pages from link-time virtual `virt` to
	// physical `phys`, with write/execute per the segment (W^X: writable clears
	// execute, read-only code stays executable). `virt` and `phys` are page
	// aligned.
	pub fn map_kernel_segment(&mut self, virt: u64, phys: u64, pages: u64, writable: bool, executable: bool) -> Option<()> {
		let mut flags = PRESENT;
		if writable {
			flags |= WRITABLE;
		}
		if !executable {
			flags |= NX;
		}
		for i in 0..pages {
			self.map_4kb(virt + i * PAGE_SIZE, phys + i * PAGE_SIZE, flags)?;
		}
		Some(())
	}

	// Install a 2 MiB leaf at PD level for `virt` -> `phys`.
	fn map_2mb(&mut self, virt: u64, phys: u64, flags: u64) -> Option<()> {
		let pdpt = self.next_table(self.pml4, pml4_index(virt))?;
		let pd = self.next_table(pdpt, pdpt_index(virt))?;
		let entry = (phys & ADDR_MASK) | flags | HUGE;
		unsafe { table_ptr(pd).add(pd_index(virt)).write_volatile(entry) };
		Some(())
	}

	// Install a 4 KiB leaf at PT level for `virt` -> `phys`.
	fn map_4kb(&mut self, virt: u64, phys: u64, flags: u64) -> Option<()> {
		let pdpt = self.next_table(self.pml4, pml4_index(virt))?;
		let pd = self.next_table(pdpt, pdpt_index(virt))?;
		let pt = self.next_table(pd, pd_index(virt))?;
		let entry = (phys & ADDR_MASK) | flags;
		unsafe { table_ptr(pt).add(pt_index(virt)).write_volatile(entry) };
		Some(())
	}

	// The physical address of the next-level table under `table[index]`,
	// allocating and linking a fresh one if the entry is empty. Intermediate
	// entries are present+writable and never NX, so a leaf's own flags govern the
	// mapping.
	fn next_table(&mut self, table: PhysicalAddress, index: usize) -> Option<PhysicalAddress> {
		let slot = unsafe { table_ptr(table).add(index) };
		let entry = unsafe { slot.read_volatile() };
		if entry & PRESENT != 0 {
			return Some(entry & ADDR_MASK);
		}
		let new = alloc_table(self.bs)?;
		unsafe { slot.write_volatile((new & ADDR_MASK) | PRESENT | WRITABLE) };
		Some(new)
	}
}

// A page-table page as a 512-entry u64 array pointer (physical == virtual during
// boot services).
fn table_ptr(phys: PhysicalAddress) -> *mut u64 {
	phys as *mut u64
}

// Paging index bit fields.
fn pml4_index(v: u64) -> usize {
	((v >> 39) & 0x1ff) as usize
}
fn pdpt_index(v: u64) -> usize {
	((v >> 30) & 0x1ff) as usize
}
fn pd_index(v: u64) -> usize {
	((v >> 21) & 0x1ff) as usize
}
fn pt_index(v: u64) -> usize {
	((v >> 12) & 0x1ff) as usize
}

// Allocate one zeroed 4 KiB page for a page table.
fn alloc_table(bs: *mut BootServices) -> Option<PhysicalAddress> {
	let mut addr: PhysicalAddress = 0;
	let status = unsafe { ((*bs).allocate_pages)(uefi::ALLOCATE_ANY_PAGES, uefi::LOADER_DATA, 1, &mut addr) };
	if uefi::is_error(status) {
		return None;
	}
	unsafe { core::ptr::write_bytes(addr as *mut u8, 0, PAGE_SIZE as usize) };
	Some(addr)
}
