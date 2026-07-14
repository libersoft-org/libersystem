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

use super::domain::Domain;
use super::memory_object::MemoryError;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::arch::paging;
use crate::mem::frame::{self, PAGE_SIZE};
use crate::sync::SpinLock;

pub struct DmaBuffer {
	header: ObjectHeader,
	// Physical addresses of the pinned frames backing this buffer.
	frames: Vec<u64>,
	// Size in bytes (rounded up to whole pages).
	size: usize,
	// The driver and display server map the same backing in different address spaces.
	mappings: SpinLock<Vec<(u64, u64)>>,
	// Domain charged for this buffer's pinned DMA memory; refunded on drop.
	domain: Arc<Domain>,
}

impl DmaBuffer {
	// Allocate `size` bytes (rounded up to whole pages, at least one) of pinned,
	// physically CONTIGUOUS DMA memory charged to `domain`'s DMA quota - one run,
	// so a device sees a single span (a virtqueue ring, a block data stage, a
	// jumbo frame all ride it whole). The quota is charged before any frame is
	// taken, so an over-cap request fails cleanly (QuotaExceeded) with nothing
	// allocated or charged, and an out-of-memory rolls the charge back.
	pub fn create_in(domain: &Arc<Domain>, size: usize) -> Result<Arc<Self>, MemoryError> {
		let pages = frame::pages_for(size);
		let bytes = pages as u64 * PAGE_SIZE;
		if !domain.try_charge_dma(bytes) {
			return Err(MemoryError::QuotaExceeded);
		}
		let base = match frame::allocate_contiguous(pages) {
			Some(b) => b,
			None => {
				domain.uncharge_dma(bytes);
				return Err(MemoryError::OutOfMemory);
			}
		};
		let frames: Vec<u64> = (0..pages as u64).map(|i| base + i * PAGE_SIZE).collect();
		Ok(Arc::new(Self { header: ObjectHeader::new(), frames, size: pages * PAGE_SIZE as usize, mappings: SpinLock::new(Vec::new()), domain: domain.clone() }))
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

impl_kernel_object!(DmaBuffer, DmaBuffer);

impl Drop for DmaBuffer {
	fn drop(&mut self) {
		debug_assert!(self.mappings.lock().is_empty(), "process cleanup must remove every DmaBuffer mapping");
		frame::free_pages(&self.frames);
		// Refund the pinned DMA memory to the owning Domain.
		self.domain.uncharge_dma(self.size as u64);
	}
}
