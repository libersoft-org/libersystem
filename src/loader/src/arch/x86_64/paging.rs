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

use uefi::{self, BootServices, PhysicalAddress};

pub const PAGE_SIZE: u64 = 4096;
pub const PAGE_2MB: u64 = 2 * 1024 * 1024;

// Page-table entry flags.
const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const PWT: u64 = 1 << 3;
const PCD: u64 = 1 << 4;
const HUGE: u64 = 1 << 7;
const NX: u64 = 1 << 63;

const IA32_EFER: u32 = 0xC000_0080;
const EFER_NXE: u64 = 1 << 11;
const CR4_LA57: u64 = 1 << 12;

#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
	let (low, high): (u32, u32);
	unsafe { core::arch::asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags)) };
	((high as u64) << 32) | low as u64
}

#[inline]
unsafe fn wrmsr(msr: u32, value: u64) {
	unsafe { core::arch::asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nomem, nostack, preserves_flags)) };
}

#[inline]
unsafe fn cr4() -> u64 {
	let value: u64;
	unsafe { core::arch::asm!("mov {}, cr4", out(reg) value, options(nomem, nostack, preserves_flags)) };
	value
}

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
		// ESTABLISH WHAT THE TABLES ASSUME, before building any.
		//
		// Every entry below carries `NX`, and bit 63 is RESERVED - a fault on use - until `EFER.NXE`
		// is set. Firmware usually sets it, and "usually" is the whole problem: on firmware that does
		// not, the loader builds tables that fault the moment they are switched to, and it does so
		// after the point where anything could report it. Setting it here costs one MSR write and
		// removes the assumption.
		//
		// And the DEPTH: these are four-level tables. With `CR4.LA57` set the CPU walks five, so the
		// PML4 built here would be read as a PML5 and every translation would come from the wrong
		// level. Nothing confirmed it; refuse instead of producing a hierarchy the CPU will read
		// differently than it was written.
		unsafe {
			if cr4() & CR4_LA57 != 0 {
				return None;
			}
			let efer = rdmsr(IA32_EFER);
			if efer & EFER_NXE == 0 {
				wrmsr(IA32_EFER, efer | EFER_NXE);
			}
		}
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
			// A HUGE LEAF IS NOT A TABLE. This returned `entry & ADDR_MASK` for any present entry,
			// so a 4 KiB mapping whose address fell under one of the 2 MiB pages the HHDM and the
			// identity map are built from took that leaf's FRAME address as a page table - and
			// wrote page-table entries into ordinary physical memory, wherever the leaf pointed.
			//
			// Not reachable with the current linker script, because the kernel's virtual addresses
			// do not collide with those maps. It is reachable from a malformed image, because the
			// loader maps whatever `p_vaddr` the image declares.
			if entry & HUGE != 0 {
				return None;
			}
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
// BELOW 4 GB, all of them.
//
// The application-processor trampoline starts in 16-bit real mode and loads CR3 with a 32-BIT
// register, so a page table above 4 GB is an address it cannot express - and it is the PML4 that
// matters most, because that is the one the trampoline loads. `ALLOCATE_ANY_PAGES` let firmware put
// it anywhere, so on a machine with plenty of high memory the APs would fail to come up, or come up
// on a truncated CR3, depending on what the low bits happened to be.
//
// Every level is capped rather than only the root: the intermediate tables are reached through
// physical addresses stored in entries, and keeping the whole hierarchy in the low 4 GB costs
// nothing and removes the question of which levels the constraint applies to.
const TABLE_CEILING: PhysicalAddress = 0xFFFF_FFFF;

fn alloc_table(bs: *mut BootServices) -> Option<PhysicalAddress> {
	let mut addr: PhysicalAddress = TABLE_CEILING;
	let status = unsafe { ((*bs).allocate_pages)(uefi::ALLOCATE_MAX_ADDRESS, uefi::LOADER_DATA, 1, &mut addr) };
	if uefi::is_error(status) {
		return None;
	}
	unsafe { core::ptr::write_bytes(addr as *mut u8, 0, PAGE_SIZE as usize) };
	Some(addr)
}
