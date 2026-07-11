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
use crate::sync::SpinLock;

// Page-table entry flags: the portable permission set (the x86-64 PTE bit positions,
// used here as hardware bits verbatim). NO_CACHE is PCD (disable caching, for MMIO);
// NO_EXECUTE is NX (needs EFER.NXE).
pub use crate::arch::common::paging::{NO_CACHE, NO_EXECUTE, PRESENT, USER, WRITABLE};

// Physical address bits within a page-table entry (bits 12..=51).
const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
const ENTRY_COUNT: usize = 512;

// Serializes every structural mutation of a page table (map / unmap / address-space
// create + teardown). The kernel half of every address space shares the SAME
// intermediate tables (new_address_space copies the kernel PML4 entries), and the
// kernel PML4 is live on every core at once, so two cores mapping VAs that share an
// intermediate level would otherwise race in next_table_create: both read an absent
// entry, both allocate a fresh next-level table, and one write wins - stranding the
// loser's leaf in an orphaned table (its thread then faults) and leaking a frame.
// This lock closes that race. It is a leaf lock over the frame allocator (map/unmap
// alloc/free intermediate tables under it, never the reverse), so the ordering is
// page-table -> frame and cannot deadlock. (The riscv64 backend carries the same
// lock for the same reason; on x86 the race never triggers under KVM's speed but is
// a real correctness bug - M118.)
static PT_LOCK: SpinLock<()> = SpinLock::new(());

// Whether the CPU supports the NX bit and EFER.NXE has been enabled. When it has
// not (an old CPU), map_page_in strips NO_EXECUTE so the bit is never a reserved-
// bit violation - the mapping is then executable, matching the hardware's best.
static NX_ENABLED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// Whether CR4.SMAP / CR4.SMEP are enforced (CPUID-gated, like NX). SMAP makes a
// plain kernel access to a USER-mapped page fault; the sanctioned copy paths open
// an explicit window with `user_access`. SMEP makes a ring-0 instruction fetch
// from a USER-mapped page fault unconditionally.
static SMAP_ENABLED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static SMEP_ENABLED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

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

// Enable supervisor-mode access/execution prevention on the current core: check
// the capabilities (CPUID leaf 7, EBX bit 7 = SMEP, bit 20 = SMAP) and set the
// CR4 bits. CR4 is per-core, so - like enable_nx - the BSP calls it at init and
// every AP in its own bring-up. From here on, a kernel dereference of a user
// pointer outside a `user_access` window page-faults instead of silently reading
// or writing user memory, and a ring-0 jump into user memory always faults.
pub fn enable_smap_smep() {
	const CR4_SMEP: u64 = 1 << 20;
	const CR4_SMAP: u64 = 1 << 21;
	let features = core::arch::x86_64::__cpuid_count(7, 0).ebx;
	let smep = features & (1 << 7) != 0;
	let smap = features & (1 << 20) != 0;
	if !smep && !smap {
		return;
	}
	let mut cr4: u64;
	unsafe { asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags)) };
	if smep {
		cr4 |= CR4_SMEP;
	}
	if smap {
		cr4 |= CR4_SMAP;
	}
	unsafe { asm!("mov cr4, {}", in(reg) cr4, options(nomem, nostack, preserves_flags)) };
	if smep {
		SMEP_ENABLED.store(true, core::sync::atomic::Ordering::Release);
	}
	if smap {
		SMAP_ENABLED.store(true, core::sync::atomic::Ordering::Release);
	}
}

// Whether SMAP / SMEP are enforced (tests assert the refusal only where it can hold).
pub fn smap_enabled() -> bool {
	SMAP_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

pub fn smep_enabled() -> bool {
	SMEP_ENABLED.load(core::sync::atomic::Ordering::Acquire)
}

// Run `f` inside the sanctioned user-memory window: EFLAGS.AC set (stac) so SMAP
// permits the access, cleared again (clac) after. Every kernel access to user
// memory - the syscall copy-in/copy-out paths, the test scaffolds staging their
// ring-3 programs - goes through here; anything else faults under SMAP. The
// window must stay a leaf operation (no yields, no blocking) so AC never travels
// into unrelated kernel execution.
#[inline]
pub fn user_access<R>(f: impl FnOnce() -> R) -> R {
	let guarded = smap_enabled();
	if guarded {
		unsafe { asm!("stac", options(nomem, nostack)) };
	}
	let result = f();
	if guarded {
		unsafe { asm!("clac", options(nomem, nostack)) };
	}
	result
}

// Clear EFLAGS.AC at an interrupt entry. A gate does not clear AC, so an interrupt
// taken from ring 3 (where user code may set AC freely) - or one landing inside a
// user_access window - would otherwise run kernel code with SMAP suspended; worse,
// an entry that context-switches (the timer) would leak AC=1 into the next
// thread's kernel execution. The resuming iretq restores the interrupted context's
// own AC, so clearing it here is invisible to the interrupted code.
#[inline]
pub fn clac_on_entry() {
	if smap_enabled() {
		unsafe { asm!("clac", options(nomem, nostack)) };
	}
}

// Copy bytes into a USER-mapped page from ring 0 through the sanctioned window -
// the test scaffolds stage their embedded ring-3 programs this way.
//
// SAFETY: `dst` must be a mapped, writable destination for `bytes.len()` bytes.
pub unsafe fn copy_to_user_page(dst: u64, bytes: &[u8]) {
	user_access(|| unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len()) });
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
// and zeroing a fresh table if the entry is not yet present. Returns None when no
// frame is available for a fresh table, so the caller can degrade a userspace map
// to ERR_NO_MEMORY instead of panicking the kernel. `parent_flags` carries
// permission bits (notably USER) that every level of the walk must also grant; a
// user leaf is only reachable if each table along the way is USER too, so the bit
// is OR-ed into both new and pre-existing intermediate entries.
//
// SAFETY: `table` must point at a valid 512-entry page table.
unsafe fn next_table_create(table: *mut u64, index: usize, parent_flags: u64) -> Option<u64> {
	unsafe {
		let entry = table.add(index);
		let value = entry.read_volatile();
		if value & PRESENT == 0 {
			let new = frame::allocate()?;
			let new_table = table_ptr(new);
			for i in 0..ENTRY_COUNT {
				new_table.add(i).write_volatile(0);
			}
			entry.write_volatile((new & ADDR_MASK) | PRESENT | WRITABLE | parent_flags);
			Some(new)
		} else {
			// Widen an existing intermediate to also grant the requested bits.
			if value & parent_flags != parent_flags {
				entry.write_volatile(value | parent_flags);
			}
			Some(value & ADDR_MASK)
		}
	}
}

// Map `virt` to physical frame `phys` with `flags` (PRESENT is always set) in the
// currently active address space.
pub fn map_page(virt: u64, phys: u64, flags: u64) {
	map_page_in(active_pml4_phys(), virt, phys, flags);
}

// Fallible counterpart of `map_page` for userspace-triggered mappings: returns Err
// when an intermediate page table cannot be allocated (out of frames), so the
// caller can propagate ERR_NO_MEMORY and roll back rather than panicking the kernel.
pub fn try_map_page(virt: u64, phys: u64, flags: u64) -> Result<(), ()> {
	try_map_page_in(active_pml4_phys(), virt, phys, flags)
}

// Drop the loader's low-half identity map from the active (kernel) page tables.
// The loader identity-maps all physical memory so its own code keeps executing
// across the `mov cr3` and the application-processor trampoline can run below
// 1 MiB after loading these tables; once the APs are up the kernel no longer needs
// it, and it MUST go before any kernel-context user mapping - a 2 MiB identity page
// would otherwise shadow a 4 KiB user page mapped at the same low virtual address
// (the kernel address space wraps these tables, so ring-3 test excursions map user
// pages right here). Clears every lower-half PML4 entry (the whole user half) and
// flushes the TLB by reloading CR3. The abandoned lower-level tables are
// bootloader-reserved memory, never in the usable pool, so nothing leaks.
pub fn remove_bootstrap_identity() {
	let cr3 = active_pml4_phys();
	unsafe {
		let pml4 = table_ptr(cr3);
		for i in 0..256 {
			pml4.add(i).write_volatile(0);
		}
		asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
	}
}

// Map `virt` to `phys` in the address space rooted at `pml4_phys`. The mapping
// takes effect immediately if that address space is the active one; otherwise it
// becomes visible when CR3 is loaded with it (which flushes the TLB anyway), so
// the invlpg here is harmless for a non-active space.
pub fn map_page_in(pml4_phys: u64, virt: u64, phys: u64, flags: u64) {
	try_map_page_in(pml4_phys, virt, phys, flags).expect("out of frames: page table");
}

// Fallible counterpart of `map_page_in`: returns Err when an intermediate table
// cannot be allocated, so a userspace-triggered map degrades to ERR_NO_MEMORY
// instead of panicking. Nothing is left mapped on failure - the walk stops at the
// level that could not be extended, and the leaf is only written once every
// intermediate exists.
pub fn try_map_page_in(pml4_phys: u64, virt: u64, phys: u64, flags: u64) -> Result<(), ()> {
	let _guard = PT_LOCK.lock();
	// Without EFER.NXE the NX bit is a reserved bit; strip it so old CPUs still map.
	let flags = if nx_enabled() { flags } else { flags & !NO_EXECUTE };
	// Permission bits the whole walk must grant for the leaf to be reachable. NX
	// stays leaf-only: an intermediate NX would blanket the whole subtree.
	let parent_flags = flags & USER;
	unsafe {
		let pml4 = table_ptr(pml4_phys);
		let pdpt = table_ptr(next_table_create(pml4, table_index(virt, 39), parent_flags).ok_or(())?);
		let pd = table_ptr(next_table_create(pdpt, table_index(virt, 30), parent_flags).ok_or(())?);
		let pt = table_ptr(next_table_create(pd, table_index(virt, 21), parent_flags).ok_or(())?);
		let entry = pt.add(table_index(virt, 12));
		entry.write_volatile((phys & ADDR_MASK) | flags | PRESENT);
		invlpg(virt);
	}
	Ok(())
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
	let _guard = PT_LOCK.lock();
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
	let _guard = PT_LOCK.lock();
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
	let _guard = PT_LOCK.lock();
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
