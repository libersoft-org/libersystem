// AddressSpace kernel object.
//
// An address space wraps a page-table root (the CR3 value). The kernel address
// space wraps the tables the bootloader built and is shared by all kernel
// threads. A process address space, created with create(), owns a fresh page
// table whose kernel half is shared with the kernel space (so the kernel stays
// mapped) and whose user half is private - the basis for per-process isolation.
// Threads reach their address space through their Process and hold it alive.

#![allow(dead_code)]

use alloc::sync::Arc;
use core::any::Any;

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::arch;

pub struct AddressSpace {
	header: ObjectHeader,
	// Physical address of the top-level page table (CR3).
	cr3: u64,
	// Whether this object owns its page tables and must free them on drop. The
	// kernel space wraps the bootloader's tables and does not own them.
	owned: bool,
}

impl AddressSpace {
	// Capture the active address space (the kernel tables the bootloader built).
	pub fn kernel() -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), cr3: arch::context::read_cr3(), owned: false })
	}

	// Create a new process address space with its own page tables. The user half
	// is empty; the kernel half is shared with the kernel space. Returns None if
	// no frame is available for the top-level table.
	pub fn create() -> Option<Arc<Self>> {
		let cr3 = arch::paging::new_address_space()?;
		Some(Arc::new(Self { header: ObjectHeader::new(), cr3, owned: true }))
	}

	// The page-table root to load into CR3 when this address space is active.
	pub fn cr3(&self) -> u64 {
		self.cr3
	}

	// Map `virt` to physical frame `phys` with `flags` in this address space.
	pub fn map(&self, virt: u64, phys: u64, flags: u64) {
		arch::paging::map_page_in(self.cr3, virt, phys, flags);
	}

	// Unmap `virt` in this address space, returning the frame it pointed at.
	pub fn unmap(&self, virt: u64) -> Option<u64> {
		arch::paging::unmap_page_in(self.cr3, virt)
	}
}

impl Drop for AddressSpace {
	fn drop(&mut self) {
		// Reclaim the user-half page-table structure and the top-level table. The
		// kernel half is shared and is never freed.
		if self.owned {
			arch::paging::free_address_space(self.cr3);
		}
	}
}

impl_kernel_object!(AddressSpace, AddressSpace);
