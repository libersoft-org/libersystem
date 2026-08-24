// riscv64 paging - Sv39 translation (higher half).
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

use crate::sync::SpinLock;

pub use crate::arch::common::paging::{NO_CACHE, NO_EXECUTE, PRESENT, USER, WRITABLE};

// Serializes every structural mutation of a page table (map / unmap / address-space
// create + teardown). The Sv39 tables are shared - a per-process root shares the
// kernel high half, and the kernel root is live on every hart at once - so two harts
// mapping VAs that share an intermediate level would otherwise race: both read an
// absent intermediate entry, both allocate a fresh next-level table, and one write
// wins - stranding the loser's leaf in an orphaned table (its thread then faults) and
// leaking a frame. This lock closes that race. It is a leaf lock over the frame
// allocator (map/unmap alloc/free intermediate tables under it, never the reverse),
// so the ordering is page-table -> frame and cannot deadlock.
static PT_LOCK: SpinLock<()> = SpinLock::new(());

// Higher-half kernel offset: kernel VA = physical | KERNEL_VA_OFFSET, the base of the
// Sv39 high canonical half. The same offset is the direct-map (HHDM) base.
pub const KERNEL_VA_OFFSET: u64 = 0xFFFF_FFC0_0000_0000;

// Sv39 PTE bits.
const PTE_V: u64 = 1 << 0; // valid
const PTE_R: u64 = 1 << 1; // readable
const PTE_W: u64 = 1 << 2; // writable
const PTE_X: u64 = 1 << 3; // executable
const PTE_U: u64 = 1 << 4; // user-accessible
const PTE_G: u64 = 1 << 5; // global - the same translation in every address space, which the direct map is
const PTE_A: u64 = 1 << 6; // accessed
const PTE_D: u64 = 1 << 7; // dirty
// A leaf PTE has at least one of R/W/X set; a pointer (non-leaf) has R=W=X=0.
const PTE_RWX: u64 = PTE_R | PTE_W | PTE_X;

// Svpbmt page-based memory types, in bits 62:61 of a leaf PTE. 0 = PMA (whatever the platform
// says), 1 = NC (non-cacheable, idempotent), 2 = IO (non-cacheable, non-idempotent, strongly
// ordered) - which is what a device register file needs.
const PTE_PBMT_IO: u64 = 2 << 61;

// Whether this machine's harts implement Svpbmt, read from the device tree once at boot.
//
// Default FALSE, and it stays false unless a device tree says otherwise. That direction is not
// a preference, it is a requirement: bits 62:61 are RESERVED on a hart without the extension,
// and a PTE that sets them there faults. Guessing wrong costs every mapping that uses it.
static SVPBMT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// Record whether Svpbmt is available. Called once during boot, after the device tree is
// located and before the first device is mapped.
pub fn set_svpbmt(available: bool) {
	SVPBMT.store(available, core::sync::atomic::Ordering::Release);
}

fn svpbmt() -> bool {
	SVPBMT.load(core::sync::atomic::Ordering::Acquire)
}

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
// Whether a per-page uncacheable attribute can be EXPRESSED on this port. It cannot: Sv39
// has no memory-type field, and saying it per page needs the Svpbmt extension, which is
// not implemented here.
//
// So `NO_CACHE` is accepted and not translated, and the mapping's behaviour comes from
// the platform's physical memory attributes instead. On QEMU virt those mark the MMIO
// regions as device memory and everything works, which is why this has never shown. On a
// platform whose PMAs say otherwise, a driver's MMIO would be mapped cacheable and its
// writes reordered or held in a cache line, with nothing anywhere reporting that the
// request was not honoured.
//
// Refusing the mapping instead was tried and is worse: it takes every device on the port
// with it, turning a case that works into a boot that does not. The real answer - Svpbmt, or a
// platform PMA check - is work this port still owes.
//
// IT USED TO BE ASKABLE. A `no_cache_supported()` stood here "so a caller or a test can find out",
// and in the life of the port neither ever did: no caller, no test, on either build. The gap is
// written down here instead, where the mapper that has it is.

fn leaf_bits(flags: u64) -> u64 {
	// `NO_CACHE` becomes a PBMT=IO leaf where Svpbmt is available, and is dropped where it is
	// not - see `no_cache_supported`, which is how a caller finds out which of the two it got.
	let mut bits = PTE_V | PTE_A | PTE_D | PTE_R;
	if flags & NO_CACHE != 0 && svpbmt() {
		bits |= PTE_PBMT_IO;
	}
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
// The portable permission flags governing `va`, or None if it is unmapped. See the
// x86_64 note: present-ness is not permission.
//
// Sv39 leaf PTEs carry U and W directly, and intermediate entries have no permission bits
// of their own (an entry with any of R/W/X set IS a leaf), so the leaf answers.
pub fn translate_flags(va: u64) -> Option<u64> {
	let mut table = phys_to_virt(current_satp_root()) as *const u64;
	for level in (0..3).rev() {
		let idx = ((va >> (12 + 9 * level)) & 0x1ff) as usize;
		let desc = unsafe { read_volatile(table.add(idx)) };
		if desc & PTE_V == 0 {
			return None;
		}
		if desc & PTE_RWX != 0 {
			let mut flags = PRESENT;
			if desc & PTE_U != 0 {
				flags |= USER;
			}
			if desc & PTE_W != 0 {
				flags |= WRITABLE;
			}
			return Some(flags);
		}
		table = phys_to_virt(pte_pa(desc)) as *const u64;
	}
	None
}

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
// SPLIT THE DIRECT MAP AND GIVE EACH PART THE PERMISSIONS ITS CONTENTS WANT (KERN-ARCH-006).
//
// The boot stub maps the high direct map with 1 GiB leaves carrying `V|R|W|X|G|A|D` and the low
// identity window with one more. The kernel runs out of the direct map - virtual = physical | KOFF
// - so two things follow that the tree advertises as impossible: every page of RAM is executable
// through it, and the kernel's own text is writable through it.
//
// Each of those leaves becomes a table of 2 MiB leaves:
//
//   [__kernel_rx_start, __kernel_rx_end)   read + execute        - `.text`, `.rodata`, `.extable`
//   everything else                        read + write, no exec - all other RAM
//
// The low identity window loses execute entirely: it exists so the boot stub keeps running when
// paging turns on, and nothing re-enters it afterwards.
//
// The linker script aligns both boundaries to 2 MiB for exactly this. A permission boundary inside
// a leaf would force the leaf to be the union of what its two halves need, which is where W^X goes
// to die.
//
// Called once, on the boot hart, after the frame allocator is up and after every secondary has
// adopted these tables - a secondary keeps executing at its LOW physical PC once `satp` is set, so
// taking execute off the identity window before they are up would fault each of them.
pub fn harden_direct_map() {
	unsafe extern "C" {
		static __kernel_rx_start: u8;
		static __kernel_rx_end: u8;
	}
	const TWO_MB: u64 = 2 * 1024 * 1024;
	const GIB: u64 = 1024 * 1024 * 1024;
	let rx_start = (&raw const __kernel_rx_start as u64) & !KERNEL_VA_OFFSET;
	let rx_end = (&raw const __kernel_rx_end as u64) & !KERNEL_VA_OFFSET;

	let root = current_satp_root();
	let _guard = PT_LOCK.lock();
	// The high direct map is root[256..264] - 0..8 GiB at KERNEL_VA_OFFSET - and root[2] is the low
	// identity window the stub runs in. Every other root slot is a user mapping or absent.
	let split = |slot: usize, base: u64, allow_execute: bool| -> bool {
		let entry = (phys_to_virt(root) as *mut u64).wrapping_add(slot);
		let descriptor = unsafe { core::ptr::read_volatile(entry) };
		// Only a valid LEAF is split: a pointer here means somebody has already been finer than the
		// stub, and replacing it would throw their mappings away.
		if descriptor & PTE_V == 0 || descriptor & PTE_RWX == 0 {
			return true;
		}
		let Some(table) = alloc_frame() else {
			return false;
		};
		for index in 0..512usize {
			let leaf_pa = base + index as u64 * TWO_MB;
			let executable = allow_execute && leaf_pa >= rx_start && leaf_pa + TWO_MB <= rx_end;
			let permissions = if executable { PTE_R | PTE_X } else { PTE_R | PTE_W };
			let bits = ((leaf_pa >> 12) << 10) | permissions | PTE_V | PTE_G | PTE_A | PTE_D;
			unsafe { core::ptr::write_volatile((phys_to_virt(table) as *mut u64).wrapping_add(index), bits) };
		}
		// A pointer entry has R, W and X all clear; the permissions live in the leaves it names.
		unsafe { core::ptr::write_volatile(entry, ((table >> 12) << 10) | PTE_V) };
		true
	};
	let mut complete = true;
	for gib in 0..8usize {
		complete &= split(256 + gib, gib as u64 * GIB, true);
	}
	// The identity window: the same RAM, and nothing executes there once the harts are up.
	complete &= split(2, 2 * GIB, false);
	unsafe {
		asm!("sfence.vma", options(nostack, preserves_flags));
	}
	if !complete {
		crate::serial_println!("riscv64: not enough frames to split the whole direct map - part of it stays writable-executable");
		return;
	}
	crate::serial_println!("riscv64: direct map split at 2 MiB - text {rx_start:#x}..{rx_end:#x} is read-execute, the rest is write-no-execute");
}

// Whether the direct map still carries a writable-executable alias, for the test that asks.
#[cfg(test)]
pub fn writable_executable_block() -> Option<u64> {
	const TWO_MB: u64 = 2 * 1024 * 1024;
	const GIB: u64 = 1024 * 1024 * 1024;
	let root = current_satp_root();
	let check = |slot: usize, base: u64| -> Option<u64> {
		let descriptor = unsafe { core::ptr::read_volatile((phys_to_virt(root) as *const u64).add(slot)) };
		if descriptor & PTE_V == 0 {
			return None;
		}
		if descriptor & PTE_RWX != 0 {
			// Still a 1 GiB leaf: writable and executable together is the defect.
			if descriptor & PTE_W != 0 && descriptor & PTE_X != 0 {
				return Some(base);
			}
			return None;
		}
		let table = ((descriptor >> 10) & 0xFFF_FFFF_FFFF) << 12;
		for index in 0..512usize {
			let leaf = unsafe { core::ptr::read_volatile((phys_to_virt(table) as *const u64).add(index)) };
			if leaf & PTE_V == 0 || leaf & PTE_RWX == 0 {
				continue;
			}
			if leaf & PTE_W != 0 && leaf & PTE_X != 0 {
				return Some(base + index as u64 * TWO_MB);
			}
		}
		None
	};
	for gib in 0..8usize {
		if let Some(at) = check(256 + gib, gib as u64 * GIB) {
			return Some(at);
		}
	}
	check(2, 2 * GIB)
}

pub fn alloc_frame() -> Option<u64> {
	let pa = crate::mem::frame::allocate()?;
	unsafe {
		core::ptr::write_bytes(phys_to_virt(pa) as *mut u8, 0, 4096);
	}
	Some(pa)
}

// Return a frame to the portable pool.
//
// # Safety
//
// Same contract as `frame::deallocate`: `pa` must be a frame this caller owns, freed once,
// and no longer reachable through any page table on any core. Every CALLER of this is checked by
// `src/tools/check-frame-retirement.sh` for the statement of why that holds, which is where the
// obligation this signature defers is actually discharged.
pub unsafe fn dealloc_frame(pa: u64) {
	// NEVER-MAPPED: the contract above is the caller's, and each caller states its own reason.
	unsafe { crate::mem::frame::deallocate(pa) };
}

pub fn frames_free() -> u64 {
	crate::mem::frame::free_count() as u64
}

// ---- 4 kB page mapping -------------------------------------------------------

// Give every top-level entry covering [base, base + len) a table, in the kernel's own root,
// before any address space exists to be copied from it.
//
// `new_address_space` COPIES the high half. Mappings made UNDER an existing top-level entry are
// visible everywhere - the copy points at the kernel's own intermediate tables - but one that
// needs a NEW top-level entry writes it only into the kernel's root. Every address space made
// before that moment lacks it, and switching to one loads a root that cannot fetch the next
// instruction: fault, fault again in the handler needing the same mapping, and the machine
// resets with nothing logged. `kernel_half_divergence` catches it and names the entry; not
// reaching the state at all is better than a good report of it.
//
// Sv39's top level covers 1 GiB per entry rather than x86_64's 512 GiB, so a window costs
// proportionally more entries here - still one empty frame each, paid once at boot.
pub fn reserve_kernel_top_level(base: u64, len: u64) {
	const SLOT: u64 = 1 << 30; // one Sv39 level-2 entry
	let _guard = PT_LOCK.lock();
	let root = current_satp_root();
	let table = phys_to_virt(root) as *mut u64;
	let mut va = base & !(SLOT - 1);
	let end = base + len;
	while va < end {
		let idx = ((va >> 30) & 0x1ff) as usize;
		let desc = unsafe { read_volatile(table.add(idx)) };
		if desc & PTE_V == 0 {
			let frame = alloc_frame().expect("out of frames: kernel-half reservation");
			unsafe { write_volatile(table.add(idx), pte_ppn(frame) | PTE_V) };
		}
		va += SLOT;
	}
}

// Map one 4 kB page `va -> pa` in the tree rooted at `root` (a physical Sv39 root),
// allocating any missing intermediate tables, then flush the TLB.
// Detach and retire the intermediate levels a failed walk created, newest first.
//
// SAFETY: every entry named here was written by this walk and nothing else can have reached the
// frame it points at, because the leaf that would have made it reachable was never written.
unsafe fn unwind_created(created: &[(*mut u64, u64); 2], len: usize) {
	for &(entry, phys) in created.iter().take(len).rev() {
		unsafe { write_volatile(entry, 0) };
		unsafe { crate::mem::frame::retire(&[phys]) };
	}
	if len > 0 {
		// Drained here: this is the out-of-memory path, and leaving the frames in quarantine holds
		// the memory that would satisfy the caller's next attempt.
		unsafe { crate::mem::frame::drain_quarantine() };
	}
}

unsafe fn map_page_root(root: u64, va: u64, pa: u64, flags: u64) -> Result<usize, ()> {
	let _guard = PT_LOCK.lock();
	let mut table = phys_to_virt(root) as *mut u64;
	// The levels this call CREATES, so a failure further down can give them back - the same
	// rollback the x86_64 mapper carries, and for the same reason: a walk that got one level in and
	// could not allocate the next returned `Err` with a fresh page-table frame attached to the
	// address space, and nothing counted it. The entry is cleared BEFORE the frame is retired, so
	// nothing can reach it in between.
	let mut created: [(*mut u64, u64); 2] = [(core::ptr::null_mut(), 0); 2];
	let mut created_len = 0usize;
	for level in (1..3).rev() {
		let idx = ((va >> (12 + 9 * level)) & 0x1ff) as usize;
		let desc = unsafe { read_volatile(table.add(idx)) };
		let next = if desc & PTE_V == 0 {
			let Some(frame) = alloc_frame() else {
				unsafe { unwind_created(&created, created_len) };
				return Err(());
			};
			unsafe { write_volatile(table.add(idx), pte_ppn(frame) | PTE_V) };
			created[created_len] = (unsafe { table.add(idx) }, frame);
			created_len += 1;
			frame
		} else if desc & PTE_RWX != 0 {
			// A valid LEAF, not a pointer (KERN-ARCH-021). Sv39 puts 1 GiB and 2 MiB mappings at
			// exactly these levels, and its physical field names MEMORY - so descending into it
			// writes a page-table entry into whatever lives there, which after `harden_direct_map`
			// is any 2 MiB of the direct map. Refuse: a caller that asked for one 4 kB page has no
			// business re-cutting somebody else's large mapping.
			unsafe { unwind_created(&created, created_len) };
			return Err(());
		} else {
			pte_pa(desc)
		};
		table = phys_to_virt(next) as *mut u64;
	}
	let idx = ((va >> 12) & 0x1ff) as usize;
	// see the note on the x86_64 port: replacing a live mapping loses the frame that was
	// there, with nothing to report it.
	if unsafe { read_volatile(table.add(idx)) } & PTE_V != 0 {
		unsafe { unwind_created(&created, created_len) };
		return Err(());
	}
	unsafe { write_volatile(table.add(idx), pte_ppn(pa) | leaf_bits(flags)) };
	flush_tlb();
	// The page-table frames this call created, so the caller can charge them to whoever asked for
	// the mapping - see `AddressSpace::try_map`.
	Ok(created_len)
}

pub fn map_page(virt: u64, phys: u64, flags: u64) {
	unsafe {
		map_page_root(current_satp_root(), virt, phys, flags).expect("riscv64 map_page: out of frames");
	}
}

// The most page-table frames one 4 kB mapping can create here: the two levels below Sv39's root.
//
// The bound is what makes charging possible without a partial result: `AddressSpace::try_map`
// reserves this many against the Domain's memory limit BEFORE it walks, and gives back what the
// walk did not use. Charging afterwards would mean discovering the quota was exceeded with the
// frames already attached.
pub const MAX_NEW_TABLES: usize = 2;

// Fallible counterpart of `map_page` for userspace-triggered mappings: returns Err
// when an intermediate table cannot be allocated (out of frames), so the caller
// degrades to ERR_NO_MEMORY rather than panicking the kernel.
pub fn try_map_page(virt: u64, phys: u64, flags: u64) -> Result<usize, ()> {
	unsafe { map_page_root(current_satp_root(), virt, phys, flags) }
}

#[cfg(test)]
pub fn map_page_in(satp_root: u64, virt: u64, phys: u64, flags: u64) {
	unsafe {
		map_page_root(satp_root, virt, phys, flags).expect("riscv64 map_page: out of frames");
	}
}

// Fallible counterpart of `map_page_in`: returns Err when an intermediate table
// cannot be allocated, leaving nothing mapped so a userspace map can degrade to
// ERR_NO_MEMORY.
pub fn try_map_page_in(satp_root: u64, virt: u64, phys: u64, flags: u64) -> Result<usize, ()> {
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
	let _guard = PT_LOCK.lock();
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

// Flush this hart's entire translation buffer - see the x86_64 note.
pub fn flush_local_tlb() {
	unsafe {
		core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
	}
}

pub fn unmap_page(virt: u64) -> Option<u64> {
	unsafe { unmap_page_root(current_satp_root(), virt) }
}

pub fn unmap_page_in(satp_root: u64, virt: u64) -> Option<u64> {
	unsafe { unmap_page_root(satp_root, virt) }
}

// Create a fresh per-process address-space root that shares the kernel's high half
// (the direct-map megapages, so the kernel stays mapped when this space is active)
// and starts with an empty low (user) half. Returns the root physical address.
// The first kernel-half entry of `root` that differs from `reference`, as (index, ours,
// theirs), or None when they agree. The riscv64 twin of the x86_64 check, and for the same
// reason: `new_address_space` COPIES the high half rather than sharing it, so every address
// space carries a snapshot of the kernel mapping from when it was made. Switching into a root
// whose high half no longer matches loads a table that cannot fetch the next instruction,
// which faults, faults again in the handler, and resets the machine with nothing logged.
// Bits the hardware may write into a page-table entry on its own.
const HARDWARE_MAINTAINED: u64 = PTE_A | PTE_D;

pub fn kernel_half_divergence(root: u64, reference: u64) -> Option<(usize, u64, u64)> {
	let _guard = PT_LOCK.lock();
	unsafe {
		let this = phys_to_virt(root) as *const u64;
		let refr = phys_to_virt(reference) as *const u64;
		for i in 256..512 {
			let (a, b) = (read_volatile(this.add(i)), read_volatile(refr.add(i)));
			// Compare what SOFTWARE wrote. Sv39 lets an implementation set Accessed and Dirty
			// itself, so a copy taken before the kernel first walked an entry can differ from
			// the original in bits neither side chose - a false alarm about a mapping that is
			// identical in the frame it points at and the permissions it grants.
			if a & !HARDWARE_MAINTAINED != b & !HARDWARE_MAINTAINED {
				return Some((i, a, b));
			}
		}
	}
	None
}

pub fn new_address_space() -> Option<u64> {
	let root = alloc_frame()?; // zeroed
	let _guard = PT_LOCK.lock();
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
	let _guard = PT_LOCK.lock();
	unsafe {
		let r = phys_to_virt(root) as *const u64;
		for i in 0..256 {
			if let Some(l1) = next_table(r, i) {
				free_table_level(l1, 1);
			}
		}
		// NEVER-MAPPED: a page-table frame of a DEAD address space, not a data frame. This runs
		// from `AddressSpace::drop`, so the last reference is gone and no thread can be in this
		// address space; and no port assigns ASIDs, so every switch away from it invalidated the
		// whole TLB of the core that left. Nothing anywhere can still translate through these.
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
		// NEVER-MAPPED: a page-table frame of a DEAD address space, not a data frame. This runs
		// from `AddressSpace::drop`, so the last reference is gone and no thread can be in this
		// address space; and no port assigns ASIDs, so every switch away from it invalidated the
		// whole TLB of the core that left. Nothing anywhere can still translate through these.
		dealloc_frame(phys);
	}
}

// ---- the rest of the paging contract ----

// SSTATUS.SUM, the bit that lets S-mode touch a U-mapped page. x86 has SMAP and `stac`/`clac`;
// this is the same window, and until now it was open for the whole life of the kernel.
const SSTATUS_SUM: u64 = 1 << 18;

// Clear SUM on trap entry, so a trap taken INSIDE a window does not run the handler with user
// access - and a trap that context-switches does not leak the window into the next thread. The trap
// frame saves SSTATUS and the return path restores it with `csrw sstatus, t1`, so clearing it here
// is invisible to the interrupted code.
pub fn clac_on_entry() {
	unsafe { core::arch::asm!("csrc sstatus, {}", in(reg) SSTATUS_SUM, options(nostack, preserves_flags)) };
}

// Run `f` with user access permitted, and close the window again.
//
// The kernel used to run with SUM set permanently, which made every stray kernel pointer a
// potential user-memory access - the class of bug x86's SMAP exists to turn into a fault. Every
// kernel access to user memory already goes through this function, so the window is exactly as wide
// as the callers that need it.
//
// NESTING-SAFE: the previous value is read back from `csrrs`, and SUM is cleared afterwards only if
// it was clear before - an inner window must not close an outer one. Interrupts need no masking
// here because `clac_on_entry` closes the window on trap entry and the saved SSTATUS reopens it on
// return.
pub fn user_access<R>(f: impl FnOnce() -> R) -> R {
	let previous: u64;
	unsafe { core::arch::asm!("csrrs {0}, sstatus, {1}", out(reg) previous, in(reg) SSTATUS_SUM, options(nostack, preserves_flags)) };
	let result = f();
	if previous & SSTATUS_SUM == 0 {
		unsafe { core::arch::asm!("csrc sstatus, {}", in(reg) SSTATUS_SUM, options(nostack, preserves_flags)) };
	}
	result
}

// Copy `bytes` into a USER-mapped page at `dst` (a VA in the active address space).
// The copy runs inside a `user_access` window - SUM is no longer set for the kernel's whole life,
// so this is where it opens - and the page holds U-mode code, so `fence.i` makes the freshly
// written bytes coherent with the instruction fetch.
#[cfg(test)]
pub unsafe fn copy_to_user_page(dst: u64, bytes: &[u8]) {
	user_access(|| unsafe {
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len());
		asm!("fence.i", options(nostack, preserves_flags));
	});
}
