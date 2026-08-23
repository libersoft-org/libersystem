// aarch64 paging - VMSAv8-64 translation (higher half).
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

use crate::sync::SpinLock;

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
pub use crate::arch::common::paging::{NO_CACHE, NO_EXECUTE, PRESENT, USER, WRITABLE};

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

// Serializes every structural mutation of a page table (map / unmap / address-space
// teardown). A per-process TTBR0 tree is private, but map_page also targets the
// shared TTBR1 higher-half tree that is live on every core at once, so two cores
// mapping high VAs that share an intermediate level would otherwise race in
// map_page_root: both read an absent entry, both allocate a fresh next-level table,
// and one write wins - stranding the loser's leaf in an orphaned table (its thread
// then faults) and leaking a frame. This lock closes that race. It is a leaf lock
// over the frame allocator (map/unmap alloc/free intermediate tables under it, never
// the reverse), so the ordering is page-table -> frame and cannot deadlock. (The
// riscv64 backend carries the same lock for the same reason; on aarch64 the race
// never triggers under TCG's speed but is a real correctness bug.)
static PT_LOCK: SpinLock<()> = SpinLock::new(());

// Translate a virtual address to its physical address by walking the active
// tables (4 kB granule, 48-bit, levels L0..L3, honoring block descriptors). A
// top-bit-set VA walks TTBR1 (kernel/direct map), a low VA walks TTBR0.
// The portable permission flags governing `va`, or None if it is unmapped. See the
// x86_64 note: present-ness is not permission, and `user_buf_ok` asked only the former.
//
// The AP bits are per-leaf on this architecture (the table descriptors carry their own
// restrictions in APTable, which this kernel does not set), so the leaf's bits are the
// answer.
pub fn translate_flags(va: u64) -> Option<u64> {
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
		let shift = 39 - level * 9;
		let idx = ((va >> shift) & 0x1ff) as usize;
		let desc = unsafe { core::ptr::read_volatile(table.add(idx)) };
		if desc & VALID == 0 {
			return None;
		}
		if desc & TABLE == 0 || level == 3 {
			// AP[1] (bit 6) set means EL0 may reach it; AP[2] (bit 7) set means read-only.
			let mut flags = PRESENT;
			if desc & (1 << 6) != 0 {
				flags |= USER;
			}
			if desc & (1 << 7) == 0 {
				flags |= WRITABLE;
			}
			return Some(flags);
		}
		table = phys_to_virt(desc & ADDR_MASK) as *const u64;
	}
	None
}

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

// SPLIT THE DIRECT MAP AND GIVE EACH PART THE PERMISSIONS ITS CONTENTS WANT (KERN-ARCH-006).
//
// The boot stub maps 1-4 GiB with three 1 GiB Normal blocks, and a block descriptor with AP=00 and
// no PXN/UXN is READABLE, WRITABLE AND EXECUTABLE. The kernel runs out of that map - virtual =
// physical | KOFF - so two things follow that the tree advertises as impossible: every page of RAM
// is executable through the direct map, and the kernel's own text is writable through it.
//
// This replaces each of those blocks with a table of 2 MiB blocks and gives each block one of two
// shapes:
//
//   [__boot_phys_end, __kernel_rx_end)   read-only, executable   - `.text`, `.rodata`, `.extable`
//   everything else                      writable, execute-never - all other RAM, including the
//                                                                  boot section that holds these
//                                                                  very tables
//
// The linker script aligns both boundaries to 2 MiB for exactly this, which is what makes a block
// granularity enough: a permission boundary that fell inside a block would force the block to be
// the union of what its two halves need, which is where W^X goes to die.
//
// WHY NOT 4 kB: nothing here needs it. Splitting further would separate `.rodata` from `.text` -
// worth having, and a different property from the one the finding names. Executable read-only data
// is not a writable-executable alias.
//
// Called once, on the boot hart, after the frame allocator is up (it allocates the tables) and
// before any secondary hart is started or any userspace runs. The secondaries load the same TTBR1.
// The L1 the boot stub filled with 1 GiB blocks. TTBR1 holds the L0 - with T1SZ=16 the kernel's
// whole half sits under L0[0] - so a walk of the direct map starts one level DOWN from the root.
fn boot_l1() -> Option<u64> {
	let l0 = current_ttbr1();
	let root = unsafe { core::ptr::read_volatile(phys_to_virt(l0) as *const u64) };
	if root & VALID == 0 || root & TABLE == 0 {
		return None;
	}
	Some(root & ADDR_MASK)
}

pub fn harden_direct_map() {
	unsafe extern "C" {
		static __kernel_rx_start: u8;
		static __kernel_rx_end: u8;
	}
	const TWO_MB: u64 = 2 * 1024 * 1024;
	const GIB: u64 = 1024 * 1024 * 1024;
	// HIGHER-HALF SYMBOLS, masked down. The linker places them in the kernel's own half because a
	// low symbol is out of `adrp` range from code that runs there; `usable_region` does the same
	// with `__kernel_end`.
	let rx_start = (&raw const __kernel_rx_start as u64) & !KERNEL_VA_OFFSET;
	let rx_end = (&raw const __kernel_rx_end as u64) & !KERNEL_VA_OFFSET;

	let Some(l1) = boot_l1() else {
		crate::serial_println!("aarch64: TTBR1 has no L1 under L0[0] - the direct map is not the stub's, leaving it alone");
		return;
	};
	let _guard = PT_LOCK.lock();
	// L1 indices 1, 2 and 3 are the three DRAM gigabytes the stub mapped as blocks; index 0 is the
	// device gigabyte and 256 the high ECAM, both of which stay exactly as they are.
	for index in 1..4usize {
		let entry = (phys_to_virt(l1) as *mut u64).wrapping_add(index);
		let descriptor = unsafe { core::ptr::read_volatile(entry) };
		// Only a valid BLOCK is split. A table here means somebody has already been finer than the
		// stub, and replacing it would throw their mappings away.
		if descriptor & VALID == 0 || descriptor & TABLE != 0 {
			continue;
		}
		let Some(table) = alloc_frame() else {
			crate::serial_println!("aarch64: no frame to split the direct map at {} GiB - it stays writable-executable", index);
			return;
		};
		let base = index as u64 * GIB;
		for slot in 0..512usize {
			let block = base + slot as u64 * TWO_MB;
			// A block is text ONLY if it lies wholly inside the read-execute span. The bounds are
			// 2 MiB aligned, so "wholly inside" and "overlaps" are the same question here - stated
			// as containment because that is the property being relied on.
			let executable = block >= rx_start && block + TWO_MB <= rx_end;
			let mut bits = VALID | AF | ATTR_NORMAL | SH_INNER | block;
			if executable {
				bits |= 1 << 7; // AP[2] - read-only at EL1
			} else {
				bits |= PXN | UXN;
			}
			unsafe { core::ptr::write_volatile((phys_to_virt(table) as *mut u64).wrapping_add(slot), bits) };
		}
		unsafe { core::ptr::write_volatile(entry, table | VALID | TABLE) };
	}
	// The new tables must be visible before the old translations are dropped, and the old
	// translations must be gone before the next instruction is fetched through one.
	unsafe {
		asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack, preserves_flags));
	}
	crate::serial_println!("aarch64: direct map split at 2 MiB - text {rx_start:#x}..{rx_end:#x} is read-execute, the rest is write-no-execute");
}

// Whether the direct map still carries a writable-executable alias, for the test that asks. Walks
// the live tables and reports the first 2 MiB block that is both.
#[cfg(test)]
pub fn writable_executable_block() -> Option<u64> {
	const TWO_MB: u64 = 2 * 1024 * 1024;
	const GIB: u64 = 1024 * 1024 * 1024;
	let l1 = boot_l1()?;
	for index in 1..4usize {
		let descriptor = unsafe { core::ptr::read_volatile((phys_to_virt(l1) as *const u64).add(index)) };
		if descriptor & VALID == 0 {
			continue;
		}
		if descriptor & TABLE == 0 {
			// Still a 1 GiB block: writable (AP[2] clear) and executable (PXN clear) is the defect.
			if descriptor & (1 << 7) == 0 && descriptor & PXN == 0 {
				return Some(index as u64 * GIB);
			}
			continue;
		}
		let table = descriptor & ADDR_MASK;
		for slot in 0..512usize {
			let block = unsafe { core::ptr::read_volatile((phys_to_virt(table) as *const u64).add(slot)) };
			if block & VALID == 0 || block & TABLE != 0 {
				continue;
			}
			if block & (1 << 7) == 0 && block & PXN == 0 {
				return Some(index as u64 * GIB + slot as u64 * TWO_MB);
			}
		}
	}
	None
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
// Give back the intermediate tables THIS call created, innermost first (KERN-ARCH-019). Without
// it a walk that got one or two levels in and then could not allocate the next returned `Err` with
// fresh page-table frames attached to the address space, while the fallible mapper's own comment
// promised nothing was left behind - true of the leaf, and not of the metadata. The entry is
// cleared BEFORE the frame is retired, so nothing can reach it in between.
unsafe fn unwind_created(created: &[(*mut u64, u64); 3], len: usize) {
	for &(entry, phys) in created.iter().take(len).rev() {
		unsafe { core::ptr::write_volatile(entry, 0) };
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
	let mut created: [(*mut u64, u64); 3] = [(core::ptr::null_mut(), 0); 3];
	let mut created_len = 0usize;
	for level in 0..3u64 {
		let shift = 39 - level * 9; // L0=39, L1=30, L2=21
		let idx = ((va >> shift) & 0x1ff) as usize;
		let desc = unsafe { core::ptr::read_volatile(table.add(idx)) };
		let next = if desc & VALID == 0 {
			let Some(frame) = alloc_frame() else {
				unsafe { unwind_created(&created, created_len) };
				return Err(());
			};
			unsafe { core::ptr::write_volatile(table.add(idx), frame | VALID | TABLE) };
			created[created_len] = (unsafe { table.add(idx) }, frame);
			created_len += 1;
			frame
		} else if desc & TABLE == 0 {
			// A valid BLOCK, not a table (KERN-ARCH-021). Its output address names MEMORY, not a
			// page table, so descending into it writes a table entry into whatever lives there -
			// which after `harden_direct_map` is any 2 MiB of the direct map, and before it was
			// the whole gigabyte. REFUSING is the only safe answer: splitting the block here would
			// have to reproduce its attributes across 512 leaves under a caller that asked for one
			// page, and the callers that legitimately reach a block-mapped range do not exist.
			unsafe { unwind_created(&created, created_len) };
			return Err(());
		} else {
			desc & ADDR_MASK
		};
		table = phys_to_virt(next) as *mut u64;
	}
	let idx = ((va >> 12) & 0x1ff) as usize;
	unsafe {
		// see the note on the x86_64 port: replacing a live mapping loses the frame that
		// was there, with nothing to report it.
		if core::ptr::read_volatile(table.add(idx)) & VALID != 0 {
			unwind_created(&created, created_len);
			return Err(());
		}
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
	// The page-table frames this call created, so the caller can charge them to whoever asked for
	// the mapping - see `AddressSpace::try_map`.
	Ok(created_len)
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

// THE SANCTIONED WINDOW, which on this port is not a window at all: cortex-a72 is ARMv8.0 and has
// no PAN, so there is nothing to open and nothing to close. The name exists because the portable
// callers ask for it, and the passthrough is what says the protection is absent rather than off.
pub fn user_access<R>(f: impl FnOnce() -> R) -> R {
	f()
}

#[cfg(test)]
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
	unsafe {
		map_page_root(map_root_for(virt), virt, phys, flags).expect("aarch64 map_page: out of frames");
	}
}

// The most page-table frames one 4 kB mapping can create here: L1, L2 and L3 below a populated L0.
//
// The bound is what makes charging possible without a partial result: `AddressSpace::try_map`
// reserves this many against the Domain's memory limit BEFORE it walks, and gives back what the
// walk did not use. Charging afterwards would mean discovering the quota was exceeded with the
// frames already attached.
pub const MAX_NEW_TABLES: usize = 3;

// Fallible counterpart of `map_page` for userspace-triggered mappings: returns Err
// when an intermediate table cannot be allocated (out of frames), so the caller
// degrades to ERR_NO_MEMORY rather than panicking the kernel.
pub fn try_map_page(virt: u64, phys: u64, flags: u64) -> Result<usize, ()> {
	unsafe { map_page_root(map_root_for(virt), virt, phys, flags) }
}

#[cfg(test)]
pub fn map_page_in(ttbr: u64, virt: u64, phys: u64, flags: u64) {
	unsafe {
		map_page_root(root_in(ttbr, virt), virt, phys, flags).expect("aarch64 map_page: out of frames");
	}
}

// Fallible counterpart of `map_page_in`: returns Err when an intermediate table
// cannot be allocated, leaving nothing mapped so a userspace map can degrade to
// ERR_NO_MEMORY.
pub fn try_map_page_in(ttbr: u64, virt: u64, phys: u64, flags: u64) -> Result<usize, ()> {
	unsafe { map_page_root(root_in(ttbr, virt), virt, phys, flags) }
}

// The page-table root a virtual address maps through: the higher-half (top bit
// set) goes through TTBR1, a low address through the active TTBR0 tree.
fn map_root_for(virt: u64) -> u64 {
	if virt >> 63 == 1 { current_ttbr1() } else { current_ttbr0() }
}

// The root for an address in a NAMED address space. This port splits the two halves across
// two registers, so a "page-table root" here is always a TTBR0 value - and a higher-half
// address does not live in it, whoever passed it. The half decides the tree; the argument
// only decides WHICH low tree.
//
// The portable layer above cannot know that. `AddressSpace::kernel()` captures
// `read_cr3()`, which on this port reads TTBR0_EL1 - the active PROCESS's low-half root -
// and then maps kernel-half addresses "into" it. A thread's kernel stack went there: the
// range was reserved in the kernel window, the pages were written into a user tree where no
// higher-half address can ever resolve, and the first byte of the zeroing memset took a
// translation fault at the stack's own address. It did not show up on x86_64 or riscv64,
// where one root covers both halves and the distinction does not exist.
fn root_in(ttbr: u64, virt: u64) -> u64 {
	if virt >> 63 == 1 { current_ttbr1() } else { ttbr & ADDR_MASK }
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
	let _guard = PT_LOCK.lock();
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

// Flush this core's entire translation buffer - see the x86_64 note.
pub fn flush_local_tlb() {
	unsafe {
		core::arch::asm!("dsb ishst", "tlbi vmalle1", "dsb ish", "isb", options(nostack, preserves_flags));
	}
}

pub fn unmap_page(virt: u64) -> Option<u64> {
	// A top-bit-set virtual address lives in TTBR1 (higher half), a low address in the
	// active TTBR0 tree - mirror map_page's routing so a high mapping is actually found.
	let root = if virt >> 63 == 1 { current_ttbr1() } else { current_ttbr0() };
	unsafe { unmap_page_root(root, virt) }
}
pub fn unmap_page_in(ttbr: u64, virt: u64) -> Option<u64> {
	unsafe { unmap_page_root(root_in(ttbr, virt), virt) }
}

// Create a fresh address-space root (TTBR0 tree). The kernel runs in the higher
// half through TTBR1, so a per-process TTBR0 tree carries no kernel mappings: it
// starts as an empty L0. User pages (all below 128 TB) are mapped on demand.
// Returns the L0 physical address, or None if out of RAM. No PT_LOCK needed: this
// only allocates an empty root (a leaf frame alloc), it mutates no shared table.
// Always None on aarch64, and the reason is the architecture rather than an omission. The
// kernel lives in TTBR1 and userspace in TTBR0, and a context switch changes only TTBR0 - so a
// user address space holds no copy of the kernel mapping to drift from, and switching into one
// cannot lose the kernel the way a single-root architecture can. x86_64 and riscv64 both copy
// a kernel half into every new root and need the check; here there is nothing to compare.
pub fn kernel_half_divergence(_root: u64, _reference: u64) -> Option<(usize, u64, u64)> {
	None
}

// Nothing to reserve, for the same reason there is nothing to compare: the kernel's top-level
// entries live in TTBR1, which every address space shares rather than copies, so one created
// after a process exists is already visible to it. The callers reserve unconditionally - a
// window that must exist everywhere is a property of the layout, not of the architecture that
// happens to need help enforcing it.
pub fn reserve_kernel_top_level(_base: u64, _len: u64) {}

pub fn new_address_space() -> Option<u64> {
	// alloc_frame returns a zeroed frame, so the L0 is already empty.
	alloc_frame()
}

// Tear down an address space created by new_address_space: free every user-region
// page table and the L0 frame. Leaf data frames are owned by whoever mapped them
// and are not freed here.
pub fn free_address_space(root: u64) {
	let _guard = PT_LOCK.lock();
	unsafe {
		let l0 = phys_to_virt(root) as *const u64;
		for i in 0..512 {
			if let Some(l1) = next_table(l0, i) {
				// An L1 sits three table levels above the data pages (L1 -> L2 -> L3).
				free_table_level(l1, 3);
			}
		}
		// NEVER-MAPPED: a page-table frame of a DEAD address space, not a data frame. This runs
		// from `AddressSpace::drop`, so the last reference is gone and no thread can be in this
		// address space; and no port assigns ASIDs, so every switch away from it invalidated the
		// whole TLB of the core that left. Nothing anywhere can still translate through these.
		dealloc_frame(root);
	}
}

// Recursively free the intermediate tables below `phys`. `level` is 3 for an L1,
// 2 for an L2, 1 for an L3. An L3's entries point at data frames, which are not
// freed; only the table frames themselves are reclaimed. (Descending from an L1
// with level 2 - one short - used to skip the L3s entirely, leaking every leaf
// table of a torn-down address space; the concurrent-maps stress test counts the
// reclaimed frames and caught it.)
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
		// NEVER-MAPPED: a page-table frame of a DEAD address space, not a data frame. This runs
		// from `AddressSpace::drop`, so the last reference is gone and no thread can be in this
		// address space; and no port assigns ASIDs, so every switch away from it invalidated the
		// whole TLB of the core that left. Nothing anywhere can still translate through these.
		dealloc_frame(phys);
	}
}

// No bootstrap identity to remove on aarch64: the boot identity map IS the kernel
// address space (the kernel runs from the low half). This no-op keeps the portable
// contract until the kernel moves to the high half.
pub fn remove_bootstrap_identity() {}
