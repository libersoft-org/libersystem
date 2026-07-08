// aarch64 paging - VMSAv8-64 translation (M116, higher half).
//
// The low `.text.boot` stub (see boot.rs) builds the boot page tables and turns
// on the MMU: TTBR0 holds a low identity map (for the hand-off), TTBR1 holds the
// higher-half kernel map plus a direct map of physical memory at
// `KERNEL_VA_OFFSET` (VA = PA | KOFF). The kernel then runs entirely from the
// high half, so TTBR0 is free for userspace. Every physical/table access here
// goes through `phys_to_virt` (the TTBR1 direct map), never a raw physical
// pointer.
//
// This module walks those tables (`translate`), runs the frame allocator (free
// DRAM above the kernel image), maps 4 kB pages (`map_page`, allocating the
// intermediate L1/L2/L3 tables), and builds/tears down per-process TTBR0 trees
// (`new_address_space` / `free_address_space`).

use core::arch::asm;

// Higher-half kernel offset: kernel VA = physical | KERNEL_VA_OFFSET. The same
// offset is the direct-map (HHDM) base, so any physical address is reachable as
// `phys_to_virt(pa)` through TTBR1.
pub const KERNEL_VA_OFFSET: u64 = 0xFFFF_0000_0000_0000;

// Map a physical address to its kernel virtual address in the TTBR1 direct map.
#[inline(always)]
pub fn phys_to_virt(pa: u64) -> u64 {
	pa | KERNEL_VA_OFFSET
}

// Portable page-table permission bits (the flag set the portable callers OR
// together). The real per-PTE VMSAv8 encoding is applied by `map_page`; these
// keep the contract's constant names meaningful.
pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const NO_CACHE: u64 = 1 << 4;
pub const NO_EXECUTE: u64 = 1 << 63;

// Descriptor bits (VMSAv8-64, stage 1).
const VALID: u64 = 1 << 0; // entry is valid
const TABLE: u64 = 1 << 1; // at L0/L1/L2: a table descriptor (VALID|TABLE = 0b11); a block clears this bit
const AF: u64 = 1 << 10; // access flag (a 0 here faults on first access)
const SH_INNER: u64 = 3 << 8; // inner shareable (for Normal memory)
const ATTR_DEVICE: u64 = 0 << 2; // MAIR index 0 = Device-nGnRnE
const ATTR_NORMAL: u64 = 1 << 2; // MAIR index 1 = Normal write-back
const PXN: u64 = 1 << 53; // privileged execute-never
const UXN: u64 = 1 << 54; // unprivileged execute-never

// The physical-address mask for a table/page pointer (bits [47:12]).
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

// Translate a virtual address to its physical address by walking the active
// tables (4 kB granule, 48-bit, levels L0..L3, honoring block descriptors). A
// top-bit-set VA walks TTBR1 (kernel/direct map), a low VA walks TTBR0.
pub fn translate(va: u64) -> Option<u64> {
	let ttbr: u64;
	unsafe {
		if va >> 63 == 1 {
			asm!("mrs {}, ttbr1_el1", out(reg) ttbr, options(nomem, nostack, preserves_flags));
		} else {
			asm!("mrs {}, ttbr0_el1", out(reg) ttbr, options(nomem, nostack, preserves_flags));
		}
	}
	let mut table = phys_to_virt(ttbr & ADDR_MASK) as *const u64;
	for level in 0..4u64 {
		let shift = 39 - level * 9; // L0=39, L1=30, L2=21, L3=12
		let idx = ((va >> shift) & 0x1ff) as usize;
		let desc = unsafe { core::ptr::read_volatile(table.add(idx)) };
		if desc & VALID == 0 {
			return None;
		}
		// A block (bit 1 clear) at L1/L2, or a page at L3, is a leaf; a table
		// descriptor (bit 1 set) at L0..L2 points at the next level.
		if desc & TABLE == 0 || level == 3 {
			let region = 1u64 << shift; // the leaf's coverage
			let base = desc & ADDR_MASK & !(region - 1);
			return Some(base | (va & (region - 1)));
		}
		table = phys_to_virt(desc & ADDR_MASK) as *const u64;
	}
	None
}

// ---- frame allocator (bump) -------------------------------------------------
//
// Physical frames come from the portable frame allocator (`crate::mem::frame`),
// seeded at boot from the device-tree memory map - the same allocator the x86
// port uses. Page tables and freshly allocated frames are reached through the
// TTBR1 direct map (`phys_to_virt`), never a raw physical pointer.

unsafe extern "C" {
	static __kernel_end: u8;
}

// Base of DRAM on QEMU virt, and a fallback top for `-m 512M` when no device tree
// is available.
const DRAM_BASE: u64 = 0x4000_0000;
const DRAM_TOP_FALLBACK: u64 = DRAM_BASE + 512 * 1024 * 1024;

// The usable physical range to seed the frame allocator with: free DRAM above the
// loaded kernel image (`__kernel_end`, a higher-half VA - converted to physical -
// page aligned) up to `ram_top` (0 = use the built-in fallback). Returns (base,
// length) in bytes.
pub fn usable_region(ram_top: u64) -> (u64, u64) {
	let kend_phys = (&raw const __kernel_end as u64) & !KERNEL_VA_OFFSET;
	let base = (kend_phys + 0xFFF) & !0xFFF;
	let top = if ram_top > base { ram_top } else { DRAM_TOP_FALLBACK };
	(base, top.saturating_sub(base))
}

// Allocate one zeroed 4 kB physical frame from the portable pool, or None when
// memory is exhausted.
pub fn alloc_frame() -> Option<u64> {
	let pa = crate::mem::frame::allocate()?;
	unsafe {
		core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, 4096);
	}
	Some(pa)
}

// Return a frame to the portable pool.
pub fn dealloc_frame(pa: u64) {
	crate::mem::frame::deallocate(pa);
}

// How many 4 kB frames the pool still has (for bring-up reporting).
pub fn frames_free() -> u64 {
	crate::mem::frame::free_count() as u64
}

// ---- TTBR1 higher-half root -------------------------------------------------

// The active TTBR1 (higher-half / direct-map) root physical address. The boot
// stub set it up; the kernel keeps a single TTBR1 tree for the life of the run.
fn current_ttbr1() -> u64 {
	let ttbr1: u64;
	unsafe {
		asm!("mrs {}, ttbr1_el1", out(reg) ttbr1, options(nomem, nostack, preserves_flags));
	}
	ttbr1 & ADDR_MASK
}

// ---- 4 kB page mapping ------------------------------------------------------

// Translate the portable permission flags to a VMSAv8-64 stage-1 L3 page leaf.
fn leaf_bits(flags: u64) -> u64 {
	// A valid L3 page descriptor: bits[1:0] = 0b11 (VALID | "page"), AF set.
	let mut bits = VALID | TABLE | AF;
	if flags & NO_CACHE != 0 {
		bits |= ATTR_DEVICE;
	} else {
		bits |= ATTR_NORMAL | SH_INNER;
	}
	// AP[2:1]: bit6 = accessible at EL0, bit7 = read-only.
	if flags & USER != 0 {
		bits |= 1 << 6;
	}
	if flags & WRITABLE == 0 {
		bits |= 1 << 7;
	}
	// Execute permissions: honour NO_EXECUTE; a user page is never privileged-
	// executable (PXN) even when it stays user-executable (UXN clear).
	if flags & NO_EXECUTE != 0 {
		bits |= PXN | UXN;
	} else if flags & USER != 0 {
		bits |= PXN;
	}
	bits
}

// Map one 4 kB page `va -> pa` in the table tree rooted at `root` (a physical L0
// table address), allocating any missing intermediate tables from the frame
// allocator, then invalidate the TLB for that VA.
unsafe fn map_page_root(root: u64, va: u64, pa: u64, flags: u64) {
	let mut table = phys_to_virt(root) as *mut u64;
	for level in 0..3u64 {
		let shift = 39 - level * 9; // L0=39, L1=30, L2=21
		let idx = ((va >> shift) & 0x1ff) as usize;
		let desc = unsafe { core::ptr::read_volatile(table.add(idx)) };
		let next = if desc & VALID == 0 {
			let frame = alloc_frame().expect("aarch64 map_page: out of frames");
			unsafe { core::ptr::write_volatile(table.add(idx), frame | VALID | TABLE) };
			frame
		} else {
			desc & ADDR_MASK
		};
		table = phys_to_virt(next) as *mut u64;
	}
	let idx = ((va >> 12) & 0x1ff) as usize;
	unsafe {
		core::ptr::write_volatile(table.add(idx), (pa & ADDR_MASK) | leaf_bits(flags));
		asm!(
			"dsb ishst",
			"tlbi vae1, {page}",
			"dsb ish",
			"isb",
			page = in(reg) va >> 12,
			options(nostack, preserves_flags),
		);
	}
}

// The active TTBR0 (low-half) root physical address.
fn current_ttbr0() -> u64 {
	let ttbr0: u64;
	unsafe {
		asm!("mrs {}, ttbr0_el1", out(reg) ttbr0, options(nomem, nostack, preserves_flags));
	}
	ttbr0 & ADDR_MASK
}

// ---- the rest of the paging contract (fills in as the port matures) ----

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

pub unsafe fn copy_to_user_page(dst: u64, bytes: &[u8]) {
	// cortex-a72 (ARMv8.0) has no PAN, so the kernel writes the USER-mapped page
	// directly - no sanctioned window is needed (user_access is a passthrough). The
	// page holds ring-3 code, so make the freshly written bytes coherent with the
	// instruction fetch: complete the stores, invalidate the I-cache to the point of
	// unification, and synchronise.
	unsafe {
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len());
		core::arch::asm!("dsb ish", "ic iallu", "dsb ish", "isb", options(nostack, preserves_flags));
	}
}

pub fn map_page(virt: u64, phys: u64, flags: u64) {
	// A top-bit-set virtual address translates through TTBR1 (higher half); a
	// low address through the active TTBR0 tree.
	let root = if virt >> 63 == 1 { current_ttbr1() } else { current_ttbr0() };
	unsafe {
		map_page_root(root, virt, phys, flags);
	}
}
pub fn map_page_in(ttbr: u64, virt: u64, phys: u64, flags: u64) {
	unsafe {
		map_page_root(ttbr & ADDR_MASK, virt, phys, flags);
	}
}

// Return the next-level table's physical address, or None if the entry is absent
// or a block (not a table descriptor).
unsafe fn next_table(table: *const u64, idx: usize) -> Option<u64> {
	let desc = unsafe { core::ptr::read_volatile(table.add(idx)) };
	if desc & VALID == 0 || desc & TABLE == 0 { None } else { Some(desc & ADDR_MASK) }
}

// Unmap `virt` in the tree rooted at `root`, returning the frame it pointed at (if
// mapped). Intermediate tables are left in place; free_address_space reclaims them.
unsafe fn unmap_page_root(root: u64, virt: u64) -> Option<u64> {
	let l1 = unsafe { next_table(phys_to_virt(root) as *const u64, ((virt >> 39) & 0x1ff) as usize)? };
	let l2 = unsafe { next_table(phys_to_virt(l1) as *const u64, ((virt >> 30) & 0x1ff) as usize)? };
	let l3 = unsafe { next_table(phys_to_virt(l2) as *const u64, ((virt >> 21) & 0x1ff) as usize)? };
	let leaf = (phys_to_virt(l3) as *mut u64).wrapping_add(((virt >> 12) & 0x1ff) as usize);
	let desc = unsafe { core::ptr::read_volatile(leaf) };
	if desc & VALID == 0 {
		return None;
	}
	unsafe {
		core::ptr::write_volatile(leaf, 0);
		asm!(
			"dsb ishst",
			"tlbi vae1, {page}",
			"dsb ish",
			"isb",
			page = in(reg) virt >> 12,
			options(nostack, preserves_flags),
		);
	}
	Some(desc & ADDR_MASK)
}

pub fn unmap_page(virt: u64) -> Option<u64> {
	// A top-bit-set virtual address lives in TTBR1 (higher half), a low address in the
	// active TTBR0 tree - mirror map_page's routing so a high mapping is actually found.
	let root = if virt >> 63 == 1 { current_ttbr1() } else { current_ttbr0() };
	unsafe { unmap_page_root(root, virt) }
}
pub fn unmap_pages(base: u64, count: usize) {
	for i in 0..count {
		unmap_page(base + i as u64 * 4096);
	}
}
pub fn unmap_page_in(ttbr: u64, virt: u64) -> Option<u64> {
	unsafe { unmap_page_root(ttbr & ADDR_MASK, virt) }
}

// Create a fresh address-space root (TTBR0 tree). The kernel runs in the higher
// half through TTBR1, so a per-process TTBR0 tree carries no kernel mappings: it
// starts as an empty L0. User pages (all below 128 TB) are mapped on demand.
// Returns the L0 physical address, or None if out of RAM.
pub fn new_address_space() -> Option<u64> {
	// alloc_frame returns a zeroed frame, so the L0 is already empty.
	alloc_frame()
}

// Tear down an address space created by new_address_space: free every user-region
// page table and the L0 frame. Leaf data frames are owned by whoever mapped them
// and are not freed here.
pub fn free_address_space(root: u64) {
	unsafe {
		let l0 = phys_to_virt(root) as *const u64;
		for i in 0..512 {
			if let Some(l1) = next_table(l0, i) {
				free_table_level(l1, 2);
			}
		}
		dealloc_frame(root);
	}
}

// Recursively free the intermediate tables below `phys`. `level` is 2 for an L2,
// 1 for an L3. An L3's entries point at data frames, which are not freed; only the
// table frames themselves are reclaimed.
//
// SAFETY: `phys` must be the physical address of a valid page table at `level`.
unsafe fn free_table_level(phys: u64, level: u32) {
	unsafe {
		if level > 1 {
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

// No bootstrap identity to remove on aarch64: the boot identity map IS the kernel
// address space (the kernel runs from the low half). This no-op keeps the portable
// contract until the kernel moves to the high half.
pub fn remove_bootstrap_identity() {}
