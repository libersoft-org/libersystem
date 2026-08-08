// MemoryObject kernel object.
//
// A MemoryObject owns a set of physical frames - a chunk of RAM that can be
// mapped into an address space. The frames are allocated at creation and freed
// when the last reference to the object is dropped, so the object's lifetime
// (through Arc) governs the memory's lifetime. This supports at most one active
// mapping per object (tracked in `mapped_at`); richer sharing arrives with IPC.

#![allow(dead_code)]

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

use super::domain::Domain;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::arch::paging;
use crate::mem::frame::{self, PAGE_SIZE};
use crate::sync::SpinLock;

// Why a MemoryObject could not be created.
pub enum MemoryError {
	// The owning Domain is at its memory quota.
	QuotaExceeded,
	// No physical frames were available.
	OutOfMemory,
}

pub struct MemoryObject {
	header: ObjectHeader,
	// Physical addresses of the frames backing this object.
	frames: Vec<u64>,
	// Size in bytes (rounded up to whole pages).
	size: usize,
	// One mapping per address space. Shared buffers are routinely mapped by both a
	// service and its client, so no single global `mapped_at` value can represent
	// their lifetime correctly.
	mappings: SpinLock<Vec<(u64, u64)>>,
	// Domain charged for this object's physical memory, if any. The charge is
	// refunded when the object is dropped.
	domain: Option<Arc<Domain>>,
}

impl MemoryObject {
	// Allocate `size` bytes (rounded up to whole pages, at least one) of physical
	// frames. Returns None if not enough frames are available. Unaccounted: used
	// for object-level construction that is not tied to a Domain quota.
	pub fn create(size: usize) -> Option<Arc<Self>> {
		let pages = frame::pages_for(size);
		let frames = frame::allocate_pages(pages)?;
		Some(Arc::new(Self { header: ObjectHeader::new(), frames, size: pages * PAGE_SIZE as usize, mappings: SpinLock::new(Vec::new()), domain: None }))
	}

	// Allocate physical frames for an object charged to `domain`. The Domain's
	// memory quota is charged atomically before any frame is taken; on success
	// the charge is held until the object is dropped, on failure nothing is
	// charged or allocated.
	pub fn create_in(domain: &Arc<Domain>, size: usize) -> Result<Arc<Self>, MemoryError> {
		// The size is a caller's number, and everything after this line multiplies it. A ceiling
		// first, then checked arithmetic: `pages as u64 * PAGE_SIZE` can wrap to a small value that
		// the Domain quota then happily approves, after which `allocate_pages` is asked for an
		// absurd count.
		if size as u64 > abi::MAX_OBJECT_BYTES {
			return Err(MemoryError::OutOfMemory);
		}
		let pages = frame::pages_for(size);
		let Some(bytes) = (pages as u64).checked_mul(PAGE_SIZE) else {
			return Err(MemoryError::OutOfMemory);
		};
		if !domain.try_charge_memory(bytes) {
			return Err(MemoryError::QuotaExceeded);
		}
		let frames = match frame::allocate_pages(pages) {
			Some(f) => f,
			None => {
				domain.uncharge_memory(bytes);
				return Err(MemoryError::OutOfMemory);
			}
		};
		Ok(Arc::new(Self { header: ObjectHeader::new(), frames, size: pages * PAGE_SIZE as usize, mappings: SpinLock::new(Vec::new()), domain: Some(domain.clone()) }))
	}

	pub fn size(&self) -> usize {
		self.size
	}

	pub fn frames(&self) -> &[u64] {
		&self.frames
	}

	pub fn is_mapped_in(&self, cr3: u64) -> bool {
		self.mappings.lock().iter().any(|(mapped_cr3, _)| *mapped_cr3 == cr3)
	}

	// Claim the right to map this object into `cr3`, under one lock. Returns false if
	// another caller already holds the claim or the mapping already exists.
	//
	// Asking `is_mapped_in` and then calling `add_mapping` is a check followed by an act,
	// with the lock dropped in between: two threads of one process could both find the
	// object unmapped and both map it, and the second mapping then vanished from the
	// process's cleanup list while staying in the page tables. The claim and the answer
	// have to be the same operation.
	//
	// A base of 0 marks the reservation - "being mapped, by someone" - and
	// `commit_mapping` replaces it with the real address.
	pub fn reserve_mapping(&self, cr3: u64) -> bool {
		let mut mappings = self.mappings.lock();
		if mappings.iter().any(|(mapped_cr3, _)| *mapped_cr3 == cr3) {
			return false;
		}
		mappings.push((cr3, 0));
		true
	}

	// Record where a reserved mapping ended up.
	pub fn commit_mapping(&self, cr3: u64, base: u64) {
		let mut mappings = self.mappings.lock();
		if let Some(entry) = mappings.iter_mut().find(|(mapped_cr3, _)| *mapped_cr3 == cr3) {
			entry.1 = base;
		}
	}

	// Drop a reservation whose mapping never happened.
	pub fn abandon_reservation(&self, cr3: u64) {
		self.mappings.lock().retain(|(mapped_cr3, base)| !(*mapped_cr3 == cr3 && *base == 0));
	}

	pub fn add_mapping(&self, cr3: u64, base: u64) {
		self.mappings.lock().push((cr3, base));
	}

	// Take this object's mapping out of `space`. The address space, not a bare cr3, because
	// the virtual range goes back to a pool that lives inside it.
	pub fn remove_mapping(&self, space: &crate::object::address_space::AddressSpace) -> bool {
		let cr3 = space.cr3();
		let base = {
			let mut mappings = self.mappings.lock();
			let Some(index) = mappings.iter().position(|(mapped_cr3, _)| *mapped_cr3 == cr3) else { return false };
			mappings.swap_remove(index).1
		};
		for page in 0..self.frames.len() {
			paging::unmap_page_in(cr3, base + page as u64 * PAGE_SIZE);
		}
		crate::syscall::free_vrange(Some(space), base, self.size as u64);
		true
	}
}

impl_kernel_object!(MemoryObject, MemoryObject);

impl Drop for MemoryObject {
	fn drop(&mut self) {
		debug_assert!(self.mappings.lock().is_empty(), "process cleanup must remove every MemoryObject mapping");
		// SAFETY: this object owns its frames, and the debug assert above has established
		// that nothing is mapping them any more.
		// RETIRED, not freed: these frames were mapped, and a frame handed out again while another
		// core still holds a stale translation is a physical use-after-free. `retire` is the one
		// door back to the allocator for anything a page table ever pointed at.
		unsafe { frame::retire(&self.frames) };
		// Refund the physical memory to the owning Domain, if any.
		if let Some(domain) = &self.domain {
			domain.uncharge_memory(self.size as u64);
		}
	}
}
