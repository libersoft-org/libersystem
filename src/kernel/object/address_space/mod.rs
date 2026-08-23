// AddressSpace kernel object.
//
// An address space wraps a page-table root (the CR3 value). The kernel address
// space wraps the tables the bootloader built and is shared by all kernel
// threads. A process address space, created with create(), owns a fresh page
// table whose kernel half is shared with the kernel space (so the kernel stays
// mapped) and whose user half is private - the basis for per-process isolation.
// Threads reach their address space through their Process and hold it alive.

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use super::domain::Domain;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::arch;
use crate::mem::frame::PAGE_SIZE;
use crate::mem::vapool::VaPool;
use crate::memlayout::{USER_MMAP_BASE, USER_VA_END};
use crate::sync::SpinLock;

pub struct AddressSpace {
	header: ObjectHeader,
	// Physical address of the top-level page table (CR3).
	cr3: u64,
	// Whether this object owns its page tables and must free them on drop. The
	// kernel space wraps the bootloader's tables and does not own them.
	owned: bool,
	// This space's own user mmap window. Per address space rather than global: two
	// address spaces may hand out the same user virtual address without sharing
	// anything, so a global pool made unrelated processes contend for one lock, let
	// any one of them exhaust the window for all of them, and gave a range a lifetime
	// longer than the tables it was an address in. This one dies with the space.
	//
	// The kernel space's copy is never used - kernel ranges come from KERNEL_VMAP,
	// selected by address - and it is empty rather than absent because `kernel()`
	// hands out a fresh wrapper each call, so there is nothing durable to key on.
	vmap: SpinLock<VaPool>,
	// WHO PAYS FOR THE PAGE TABLES. None for the kernel space, which wraps tables the bootloader
	// built and owns nothing.
	//
	// A process that maps pages makes the kernel allocate frames for its OWN page tables, and until
	// this nothing charged them: a mapping loop was a way to consume kernel memory without meeting
	// any limit. The frames are charged against the memory limit that already exists, so a Domain
	// that is out of memory cannot be made to build tables for one more page.
	domain: Option<Arc<Domain>>,
	// Page-table frames charged so far, in pages. Kept so `Drop` gives back exactly what was taken
	// rather than recomputing it from tables that are being torn down.
	table_pages: AtomicU64,
}

impl AddressSpace {
	// Capture the active address space (the kernel tables the bootloader built).
	pub fn kernel() -> Arc<Self> {
		// ALLOC-OK: the kernel's own address space, captured once at boot before userspace exists.
		Arc::new(Self { header: ObjectHeader::new(), cr3: arch::context::read_cr3(), owned: false, vmap: SpinLock::new(VaPool::new(USER_MMAP_BASE, USER_VA_END)), domain: None, table_pages: AtomicU64::new(0) })
	}

	// Create a new process address space with its own page tables, charged to `domain`. The user
	// half is empty; the kernel half is shared with the kernel space. Returns None if the Domain's
	// memory limit has no room for the root table, or if no frame is available for it.
	pub fn create_in(domain: &Arc<Domain>) -> Option<Arc<Self>> {
		// THE ROOT TABLE IS THE FIRST PAGE THIS SPACE COSTS, and it is charged before it is
		// allocated: a Domain at its limit does not get one more frame because the frame happened
		// to be free.
		if !domain.try_charge_memory(PAGE_SIZE) {
			return None;
		}
		let Some(cr3) = arch::paging::new_address_space() else {
			domain.uncharge_memory(PAGE_SIZE);
			return None;
		};
		let space = crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), cr3, owned: true, vmap: SpinLock::new(VaPool::new(USER_MMAP_BASE, USER_VA_END)), domain: Some(domain.clone()), table_pages: AtomicU64::new(1) });
		if space.is_none() {
			arch::paging::free_address_space(cr3);
			domain.uncharge_memory(PAGE_SIZE);
		}
		space
	}

	// The same, charged to nobody. TEST-ONLY: a space with no Domain behind it is one whose page
	// tables no limit covers, which is the state this milestone exists to remove from production.
	#[cfg(test)]
	pub fn create() -> Option<Arc<Self>> {
		let cr3 = arch::paging::new_address_space()?;
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), cr3, owned: true, vmap: SpinLock::new(VaPool::new(USER_MMAP_BASE, USER_VA_END)), domain: None, table_pages: AtomicU64::new(0) })
	}

	// Hand out a range of this space's user mmap window, or 0 when it is exhausted.
	pub fn alloc_vrange(&self, len: u64) -> u64 {
		self.vmap.lock().alloc(len)
	}

	// Give a range of this space's user mmap window back.
	pub fn free_vrange(&self, base: u64, len: u64) {
		self.vmap.lock().free(base, len);
	}

	// The page-table root to load into CR3 when this address space is active.
	pub fn cr3(&self) -> u64 {
		self.cr3
	}

	// Map `virt` to physical frame `phys` with `flags` in this address space.
	//
	// Panics on a USER mapping outside the user half rather than making one. This is the
	// infallible sibling of `try_map` and it carries the same bound for the same reason -
	// a bound that only one of two doors enforces is a bound with a door beside it. Every
	// caller of this one is kernel-internal with an address it computed itself, so
	// reaching this is a kernel bug and says so.
	#[cfg(test)]
	pub fn map(&self, virt: u64, phys: u64, flags: u64) {
		assert!(flags & arch::paging::USER == 0 || user_range_ok(virt), "USER mapping outside the user half: {virt:#x}");
		arch::paging::map_page_in(self.cr3, virt, phys, flags);
	}

	// Fallible map for userspace-triggered mappings: returns Err when an
	// intermediate page table cannot be allocated, so a program load / stack
	// growth under memory pressure degrades to a clean error instead of panicking
	// the kernel. Nothing is left mapped on failure.
	pub fn try_map(&self, virt: u64, phys: u64, flags: u64) -> Result<(), ()> {
		// Anything carrying USER is a user mapping, and a user mapping outside the user
		// half is not a thing this kernel makes. It made them: nothing here bounded the
		// address, and an `ET_EXEC` image naming a higher-half `p_vaddr` was mapped there
		// with the USER bit set - the first link of a full escalation on x86_64.
		//
		// The bound lives here rather than only in the ELF validator because this is the
		// narrowest place every user mapping passes through: the loader, the stack, the
		// fault handler's demand-grown pages and the mmap paths all arrive at this one
		// call. A caller that means to map kernel memory says so by not passing USER.
		if flags & arch::paging::USER != 0 && !user_range_ok(virt) {
			return Err(());
		}
		// THE PAGE TABLES THIS MAPPING MAY BUILD ARE CHARGED BEFORE THE WALK, and the walk gives
		// back what it did not use. Reserving the worst case first is what makes the refusal clean:
		// a Domain over its limit is told so with nothing mapped and no table attached, where
		// charging afterwards would discover it with the frames already in the tree.
		//
		// The kernel space has no Domain and is not charged: it wraps tables the bootloader built.
		let reserved = self.domain.as_ref().map(|domain| {
			let bytes = arch::paging::MAX_NEW_TABLES as u64 * PAGE_SIZE;
			if domain.try_charge_memory(bytes) { Some(bytes) } else { None }
		});
		if let Some(None) = reserved {
			return Err(());
		}
		let created = arch::paging::try_map_page_in(self.cr3, virt, phys, flags);
		if let (Some(domain), Some(Some(bytes))) = (self.domain.as_ref(), reserved) {
			// What the walk actually built stays charged; the rest goes straight back. A failed
			// walk built nothing - it unwinds its own levels - so the whole reservation returns.
			let used = created.unwrap_or(0) as u64 * PAGE_SIZE;
			self.table_pages.fetch_add(created.unwrap_or(0) as u64, Ordering::Relaxed);
			domain.uncharge_memory(bytes - used);
		}
		created.map(|_| ())
	}

	// Unmap `virt` in this address space, returning the frame it pointed at.
	pub fn unmap(&self, virt: u64) -> Option<u64> {
		arch::paging::unmap_page_in(self.cr3, virt)
	}
}

// Does one page starting at `virt` lie wholly inside the user half?
fn user_range_ok(virt: u64) -> bool {
	virt < crate::memlayout::USER_VA_END && virt.checked_add(crate::mem::frame::PAGE_SIZE).is_some_and(|end| end <= crate::memlayout::USER_VA_END)
}

impl Drop for AddressSpace {
	fn drop(&mut self) {
		// Reclaim the user-half page-table structure and the top-level table. The
		// kernel half is shared and is never freed.
		if self.owned {
			arch::paging::free_address_space(self.cr3);
		}
		// AND THE DOMAIN GETS ITS MEMORY BACK - the root table plus every intermediate this space
		// built, in one uncharge, from the count kept as they were charged rather than recomputed
		// from tables that have just been torn down.
		if let Some(domain) = self.domain.as_ref() {
			let pages = self.table_pages.swap(0, Ordering::Relaxed);
			domain.uncharge_memory(pages * PAGE_SIZE);
		}
	}
}

impl_kernel_object!(AddressSpace, AddressSpace);

#[cfg(test)]
mod tests;
