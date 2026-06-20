// DmaBuffer kernel object.
//
// A DmaBuffer owns physical frames pinned for device DMA: a driver maps it to
// fill or drain it and hands its physical address to its device. Unlike a plain
// MemoryObject the memory is charged to the owning Domain's DMA quota - pinned DMA
// is a distinct, separately capped resource (the anti-DoS rule for drivers) - and
// the frames are freed and the quota refunded when the last reference drops.
//
// The MVP allocates frame by frame (not guaranteed physically contiguous). A
// single-page buffer is trivially contiguous, and virtio's scatter-gather rings
// consume a per-page physical list, so contiguous multi-page allocation is a
// later refinement.

#![allow(dead_code)]

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use super::domain::Domain;
use super::memory_object::MemoryError;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::arch::paging;
use crate::mem::frame::{self, PAGE_SIZE};

pub struct DmaBuffer {
	header: ObjectHeader,
	// Physical addresses of the pinned frames backing this buffer.
	frames: Vec<u64>,
	// Size in bytes (rounded up to whole pages).
	size: usize,
	// Virtual base where this buffer is currently mapped (0 = unmapped).
	mapped_at: AtomicU64,
	// Domain charged for this buffer's pinned DMA memory; refunded on drop.
	domain: Arc<Domain>,
}

impl DmaBuffer {
	// Allocate `size` bytes (rounded up to whole pages, at least one) of pinned DMA
	// memory charged to `domain`'s DMA quota. The quota is charged before any frame
	// is taken, so an over-cap request fails cleanly (QuotaExceeded) with nothing
	// allocated or charged, and an out-of-memory rolls the charge back.
	pub fn create_in(domain: &Arc<Domain>, size: usize) -> Result<Arc<Self>, MemoryError> {
		let pages = frame::pages_for(size);
		let bytes = pages as u64 * PAGE_SIZE;
		if !domain.try_charge_dma(bytes) {
			return Err(MemoryError::QuotaExceeded);
		}
		let frames = match frame::allocate_pages(pages) {
			Some(f) => f,
			None => {
				domain.uncharge_dma(bytes);
				return Err(MemoryError::OutOfMemory);
			}
		};
		Ok(Arc::new(Self { header: ObjectHeader::new(), frames, size: pages * PAGE_SIZE as usize, mapped_at: AtomicU64::new(0), domain: domain.clone() }))
	}

	pub fn size(&self) -> usize {
		self.size
	}

	pub fn frames(&self) -> &[u64] {
		&self.frames
	}

	// The physical address a driver hands its device for DMA (the first frame).
	pub fn phys_base(&self) -> u64 {
		self.frames.first().copied().unwrap_or(0)
	}

	pub fn mapped_at(&self) -> u64 {
		self.mapped_at.load(Ordering::Acquire)
	}

	pub fn set_mapped_at(&self, virt: u64) {
		self.mapped_at.store(virt, Ordering::Release);
	}
}

impl_kernel_object!(DmaBuffer, DmaBuffer);

impl Drop for DmaBuffer {
	fn drop(&mut self) {
		// Tear down any leftover mapping so freed frames are never left mapped.
		let base = self.mapped_at.load(Ordering::Acquire);
		if base != 0 {
			paging::unmap_pages(base, self.frames.len());
		}
		frame::free_pages(&self.frames);
		// Refund the pinned DMA memory to the owning Domain.
		self.domain.uncharge_dma(self.size as u64);
	}
}
