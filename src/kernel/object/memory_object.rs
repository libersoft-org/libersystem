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
		let pages = frame::pages_for(size);
		let bytes = pages as u64 * PAGE_SIZE;
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

	pub fn add_mapping(&self, cr3: u64, base: u64) {
		self.mappings.lock().push((cr3, base));
	}

	pub fn remove_mapping(&self, cr3: u64) -> bool {
		let base = {
			let mut mappings = self.mappings.lock();
			let Some(index) = mappings.iter().position(|(mapped_cr3, _)| *mapped_cr3 == cr3) else { return false };
			mappings.swap_remove(index).1
		};
		for page in 0..self.frames.len() {
			paging::unmap_page_in(cr3, base + page as u64 * PAGE_SIZE);
		}
		crate::syscall::free_vrange(base, self.size as u64);
		true
	}
}

impl_kernel_object!(MemoryObject, MemoryObject);

impl Drop for MemoryObject {
	fn drop(&mut self) {
		debug_assert!(self.mappings.lock().is_empty(), "process cleanup must remove every MemoryObject mapping");
		frame::free_pages(&self.frames);
		// Refund the physical memory to the owning Domain, if any.
		if let Some(domain) = &self.domain {
			domain.uncharge_memory(self.size as u64);
		}
	}
}
