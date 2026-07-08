// riscv64 paging - Sv39 translation (M117, higher half).
//
// The boot stub (boot.rs) built one Sv39 root table and turned paging on: a low
// identity megapage for the hand-off plus a high direct map of physical memory at
// KERNEL_VA_OFFSET (VA = PA | KOFF). The kernel runs entirely from the high half, so
// the low half is free for userspace. RISC-V has ONE SATP root (no TTBR0/TTBR1
// split), so a per-process address space is a fresh root that SHARES the kernel's
// high-half megapages (copied from the live root) and carries the user's low-half
// 4 kB pages. This module walks those tables (`translate`), maps 4 kB pages
// (`map_page`, allocating the intermediate levels), and builds / tears down the
// per-process roots. Every physical / table access goes through `phys_to_virt`.

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};

pub use crate::arch::common::paging::{NO_CACHE, NO_EXECUTE, PRESENT, USER, WRITABLE};

// Higher-half kernel offset: kernel VA = physical | KERNEL_VA_OFFSET, the base of the
// Sv39 high canonical half. The same offset is the direct-map (HHDM) base.
pub const KERNEL_VA_OFFSET: u64 = 0xFFFF_FFC0_0000_0000;

// Sv39 PTE bits.
const PTE_V: u64 = 1 << 0; // valid
const PTE_R: u64 = 1 << 1; // readable
const PTE_W: u64 = 1 << 2; // writable
const PTE_X: u64 = 1 << 3; // executable
const PTE_U: u64 = 1 << 4; // user-accessible
const PTE_A: u64 = 1 << 6; // accessed
const PTE_D: u64 = 1 << 7; // dirty
// A leaf PTE has at least one of R/W/X set; a pointer (non-leaf) has R=W=X=0.
const PTE_RWX: u64 = PTE_R | PTE_W | PTE_X;

// Map a physical address to its kernel virtual address in the direct map.
#[inline(always)]
pub fn phys_to_virt(pa: u64) -> u64 {
	pa | KERNEL_VA_OFFSET
}

// The PPN field of a PTE built from a physical address (bits [53:10] = PA[55:12]).
#[inline(always)]
fn pte_ppn(pa: u64) -> u64 {
	(pa >> 12) << 10
}

// The physical address a PTE's PPN field points at.
#[inline(always)]
fn pte_pa(pte: u64) -> u64 {
	(pte >> 10) << 12
}

// The active SATP root's physical address (SATP.PPN << 12).
fn current_satp_root() -> u64 {
	let satp: u64;
	unsafe {
		asm!("csrr {}, satp", out(reg) satp, options(nomem, nostack, preserves_flags));
	}
	(satp & 0xFFF_FFFF_FFFF) << 12 // PPN is bits [43:0]
}

// Flush the whole TLB (a per-VA sfence is a later optimisation).
fn flush_tlb() {
	unsafe {
		asm!("sfence.vma", options(nostack, preserves_flags));
	}
}

// Translate the portable permission flags to an Sv39 leaf PTE (accessed + dirty are
// pre-set so the hardware never faults to update them). Every mapped page is at least
// readable; NO_CACHE has no base-Sv39 PTE encoding (QEMU virt's device regions carry
// their attributes in the physical memory map), so it is ignored.
fn leaf_bits(flags: u64) -> u64 {
	let mut bits = PTE_V | PTE_A | PTE_D | PTE_R;
	if flags & WRITABLE != 0 {
		bits |= PTE_W;
	}
	if flags & NO_EXECUTE == 0 {
		bits |= PTE_X;
	}
	if flags & USER != 0 {
		bits |= PTE_U;
	}
	bits
}

// Walk the active root and translate a virtual address to physical, honouring leaves
// at any level (1 GiB / 2 MiB / 4 kB). Returns None if unmapped.
pub fn translate(va: u64) -> Option<u64> {
	let mut table = phys_to_virt(current_satp_root()) as *const u64;
	for level in (0..3).rev() {
		let idx = ((va >> (12 + 9 * level)) & 0x1ff) as usize;
		let desc = unsafe { read_volatile(table.add(idx)) };
		if desc & PTE_V == 0 {
			return None;
		}
		if desc & PTE_RWX != 0 {
			let size = 1u64 << (12 + 9 * level);
			return Some((pte_pa(desc) & !(size - 1)) | (va & (size - 1)));
		}
		table = phys_to_virt(pte_pa(desc)) as *const u64;
	}
	None
}

// ---- frame allocator ---------------------------------------------------------

unsafe extern "C" {
	static __kernel_end: u8;
}

// DRAM base on QEMU virt riscv, and a fallback top for `-m 512M` with no device tree.
const DRAM_BASE: u64 = 0x8000_0000;
const DRAM_TOP_FALLBACK: u64 = DRAM_BASE + 512 * 1024 * 1024;

// The usable physical range to seed the frame allocator with: free DRAM above the
// loaded kernel image (`__kernel_end`, a higher-half VA - masked to physical, page
// aligned) up to `ram_top` (0 = the built-in fallback).
pub fn usable_region(ram_top: u64) -> (u64, u64) {
	let kend_phys = (&raw const __kernel_end as u64) & !KERNEL_VA_OFFSET;
	let base = (kend_phys + 0xFFF) & !0xFFF;
	let top = if ram_top > base { ram_top } else { DRAM_TOP_FALLBACK };
	(base, top.saturating_sub(base))
}

// Allocate one zeroed 4 kB physical frame from the portable pool, or None if exhausted.
pub fn alloc_frame() -> Option<u64> {
	let pa = crate::mem::frame::allocate()?;
	unsafe {
		core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, 4096);
	}
	Some(pa)
}

pub fn dealloc_frame(pa: u64) {
	crate::mem::frame::deallocate(pa);
}

pub fn frames_free() -> u64 {
	crate::mem::frame::free_count() as u64
}

// ---- 4 kB page mapping -------------------------------------------------------

// Map one 4 kB page `va -> pa` in the tree rooted at `root` (a physical Sv39 root),
// allocating any missing intermediate tables, then flush the TLB.
unsafe fn map_page_root(root: u64, va: u64, pa: u64, flags: u64) {
	let mut table = phys_to_virt(root) as *mut u64;
	for level in (1..3).rev() {
		let idx = ((va >> (12 + 9 * level)) & 0x1ff) as usize;
		let desc = unsafe { read_volatile(table.add(idx)) };
		let next = if desc & PTE_V == 0 {
			let frame = alloc_frame().expect("riscv64 map_page: out of frames");
			unsafe { write_volatile(table.add(idx), pte_ppn(frame) | PTE_V) };
			frame
		} else {
			pte_pa(desc)
		};
		table = phys_to_virt(next) as *mut u64;
	}
	let idx = ((va >> 12) & 0x1ff) as usize;
	unsafe { write_volatile(table.add(idx), pte_ppn(pa) | leaf_bits(flags)) };
	flush_tlb();
}

pub fn map_page(virt: u64, phys: u64, flags: u64) {
	unsafe { map_page_root(current_satp_root(), virt, phys, flags) }
}

pub fn map_page_in(satp_root: u64, virt: u64, phys: u64, flags: u64) {
	unsafe { map_page_root(satp_root, virt, phys, flags) }
}

// Return the next-level table's physical address, or None if the entry is absent or
// a leaf (not a pointer).
unsafe fn next_table(table: *const u64, idx: usize) -> Option<u64> {
	let desc = unsafe { read_volatile(table.add(idx)) };
	if desc & PTE_V == 0 || desc & PTE_RWX != 0 { None } else { Some(pte_pa(desc)) }
}

// Unmap `virt` in the tree rooted at `root`, returning the frame it pointed at (if
// mapped). Intermediate tables are left in place; free_address_space reclaims them.
unsafe fn unmap_page_root(root: u64, virt: u64) -> Option<u64> {
	let l1 = unsafe { next_table(phys_to_virt(root) as *const u64, ((virt >> 30) & 0x1ff) as usize)? };
	let l0 = unsafe { next_table(phys_to_virt(l1) as *const u64, ((virt >> 21) & 0x1ff) as usize)? };
	let leaf = (phys_to_virt(l0) as *mut u64).wrapping_add(((virt >> 12) & 0x1ff) as usize);
	let desc = unsafe { read_volatile(leaf) };
	if desc & PTE_V == 0 {
		return None;
	}
	unsafe { write_volatile(leaf, 0) };
	flush_tlb();
	Some(pte_pa(desc))
}

pub fn unmap_page(virt: u64) -> Option<u64> {
	unsafe { unmap_page_root(current_satp_root(), virt) }
}

pub fn unmap_pages(base: u64, count: usize) {
	for i in 0..count {
		unmap_page(base + i as u64 * 4096);
	}
}

pub fn unmap_page_in(satp_root: u64, virt: u64) -> Option<u64> {
	unsafe { unmap_page_root(satp_root, virt) }
}

// Create a fresh per-process address-space root that shares the kernel's high half
// (the direct-map megapages, so the kernel stays mapped when this space is active)
// and starts with an empty low (user) half. Returns the root physical address.
pub fn new_address_space() -> Option<u64> {
	let root = alloc_frame()?; // zeroed
	let kernel = current_satp_root(); // any live root's high half is the kernel's
	unsafe {
		let dst = phys_to_virt(root) as *mut u64;
		let src = phys_to_virt(kernel) as *const u64;
		for i in 256..512 {
			write_volatile(dst.add(i), read_volatile(src.add(i)));
		}
	}
	Some(root)
}

// Tear down an address space from new_address_space: free every user-half (low)
// intermediate table and the root frame. The high half is the shared kernel
// megapages (leaf PTEs, not owned tables) and leaf data frames are owned by whoever
// mapped them - neither is freed here.
pub fn free_address_space(root: u64) {
	unsafe {
		let r = phys_to_virt(root) as *const u64;
		for i in 0..256 {
			if let Some(l1) = next_table(r, i) {
				free_table_level(l1, 1);
			}
		}
		dealloc_frame(root);
	}
}

// Recursively free the intermediate tables below `phys`. `level` 1 = a level-1 table
// (its entries point at level-0 tables), 0 = a level-0 table (its entries are leaf
// pages, not freed). Only the table frames are reclaimed.
//
// SAFETY: `phys` must be a valid page table at `level`.
unsafe fn free_table_level(phys: u64, level: u32) {
	unsafe {
		if level > 0 {
			let table = phys_to_virt(phys) as *const u64;
			for i in 0..512 {
				if let Some(next) = next_table(table, i) {
					free_table_level(next, level - 1);
				}
			}
		}
		dealloc_frame(phys);
	}
}

// ---- the rest of the paging contract ----

pub fn enable_nx() {}
pub fn enable_smap_smep() {}
pub fn smap_enabled() -> bool {
	false
}
pub fn smep_enabled() -> bool {
	false
}
pub fn nx_enabled() -> bool {
	false
}
pub fn clac_on_entry() {}

pub fn user_access<R>(f: impl FnOnce() -> R) -> R {
	f()
}

// Copy `bytes` into a USER-mapped page at `dst` (a VA in the active address space).
// SSTATUS.SUM (set at boot) lets the S-mode kernel touch the U-mapped page; the page
// holds U-mode code, so `fence.i` makes the freshly written bytes coherent with the
// instruction fetch.
pub unsafe fn copy_to_user_page(dst: u64, bytes: &[u8]) {
	unsafe {
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len());
		asm!("fence.i", options(nostack, preserves_flags));
	}
}

// The boot identity map (root[2]) is only used during the hand-off; the kernel runs
// from the high half, so it can be dropped once the port matures. No-op for now (it
// does not collide with any user or kernel VA the kernel later touches).
pub fn remove_bootstrap_identity() {}
