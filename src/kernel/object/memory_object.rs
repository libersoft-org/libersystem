// MemoryObject kernel object.
//
// A MemoryObject owns a set of physical frames - a chunk of RAM that can be
// mapped into an address space. The frames are allocated at creation and freed
// when the last reference to the object is dropped, so the object's lifetime
// (through Arc) governs the memory's lifetime. M6 supports at most one active
// mapping per object (tracked in `mapped_at`); richer sharing arrives with IPC.

#![allow(dead_code)]

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use super::domain::Domain;
use super::{KernelObject, ObjectHeader, ObjectType};
use crate::arch::paging;
use crate::mem::frame::{self, PAGE_SIZE};

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
	// Kernel virtual base where this object is currently mapped (0 = unmapped).
	mapped_at: AtomicU64,
	// Domain charged for this object's physical memory, if any. The charge is
	// refunded when the object is dropped.
	domain: Option<Arc<Domain>>,
}

impl MemoryObject {
	// Allocate `size` bytes (rounded up to whole pages, at least one) of physical
	// frames. Returns None if not enough frames are available. Unaccounted: used
	// for object-level construction that is not tied to a Domain quota.
	pub fn create(size: usize) -> Option<Arc<Self>> {
		let page = PAGE_SIZE as usize;
		let pages = ((size + page - 1) / page).max(1);
		let frames = match Self::alloc_frames(pages) {
			Some(f) => f,
			None => return None,
		};
		Some(Arc::new(Self { header: ObjectHeader::new(), frames, size: pages * page, mapped_at: AtomicU64::new(0), domain: None }))
	}

	// Allocate physical frames for an object charged to `domain`. The Domain's
	// memory quota is charged atomically before any frame is taken; on success
	// the charge is held until the object is dropped, on failure nothing is
	// charged or allocated.
	pub fn create_in(domain: &Arc<Domain>, size: usize) -> Result<Arc<Self>, MemoryError> {
		let page = PAGE_SIZE as usize;
		let pages = ((size + page - 1) / page).max(1);
		let bytes = (pages * page) as u64;
		if !domain.try_charge_memory(bytes) {
			return Err(MemoryError::QuotaExceeded);
		}
		let frames = match Self::alloc_frames(pages) {
			Some(f) => f,
			None => {
				domain.uncharge_memory(bytes);
				return Err(MemoryError::OutOfMemory);
			}
		};
		Ok(Arc::new(Self { header: ObjectHeader::new(), frames, size: pages * page, mapped_at: AtomicU64::new(0), domain: Some(domain.clone()) }))
	}

	// Take `pages` frames, rolling back on the first failure.
	fn alloc_frames(pages: usize) -> Option<Vec<u64>> {
		let mut frames = Vec::with_capacity(pages);
		for _ in 0..pages {
			match frame::allocate() {
				Some(phys) => frames.push(phys),
				None => {
					for phys in &frames {
						frame::deallocate(*phys);
					}
					return None;
				}
			}
		}
		Some(frames)
	}

	pub fn size(&self) -> usize {
		self.size
	}

	pub fn frames(&self) -> &[u64] {
		&self.frames
	}

	// Kernel virtual base this object is mapped at, or 0 if it is not mapped.
	pub fn mapped_at(&self) -> u64 {
		self.mapped_at.load(Ordering::Acquire)
	}

	pub fn set_mapped_at(&self, virt: u64) {
		self.mapped_at.store(virt, Ordering::Release);
	}
}

impl KernelObject for MemoryObject {
	fn header(&self) -> &ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> ObjectType {
		ObjectType::MemoryObject
	}

	fn as_any(&self) -> &dyn Any {
		self
	}
}

impl Drop for MemoryObject {
	fn drop(&mut self) {
		// Tear down any leftover mapping so freed frames are never left mapped.
		let base = self.mapped_at.load(Ordering::Acquire);
		if base != 0 {
			for i in 0..self.frames.len() {
				paging::unmap_page(base + i as u64 * PAGE_SIZE);
			}
		}
		for phys in &self.frames {
			frame::deallocate(*phys);
		}
		// Refund the physical memory to the owning Domain, if any.
		if let Some(domain) = &self.domain {
			domain.uncharge_memory(self.size as u64);
		}
	}
}
