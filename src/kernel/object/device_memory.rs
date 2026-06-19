// DeviceMemory kernel object.
//
// A DeviceMemory is a capability to a physical MMIO region (a device's registers
// or BARs). A driver maps it into its address space - uncacheable, since it is
// device registers and not RAM - to talk to its device. Unlike a MemoryObject the
// kernel does not own or free the physical range (it is hardware, not allocated
// RAM) and it is not charged to a memory quota; the capability simply gates which
// driver may reach which device. DeviceManager (later) mints these for the devices
// it discovers and hands each driver only its own.

#![allow(dead_code)]

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use super::{KernelObject, ObjectHeader, ObjectType};
use crate::arch::paging;
use crate::mem::frame::PAGE_SIZE;

pub struct DeviceMemory {
	header: ObjectHeader,
	// Physical base of the MMIO region.
	phys_base: u64,
	// Length of the region in bytes.
	len: usize,
	// Virtual base this region is currently mapped at (0 = unmapped).
	mapped_at: AtomicU64,
}

impl DeviceMemory {
	// A capability to the physical MMIO region [phys_base, phys_base + len).
	pub fn new(phys_base: u64, len: usize) -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), phys_base, len, mapped_at: AtomicU64::new(0) })
	}

	pub fn phys_base(&self) -> u64 {
		self.phys_base
	}

	pub fn len(&self) -> usize {
		self.len
	}

	pub fn is_empty(&self) -> bool {
		self.len == 0
	}

	// Number of pages the region spans (at least one).
	pub fn pages(&self) -> usize {
		self.len.div_ceil(PAGE_SIZE as usize).max(1)
	}

	pub fn mapped_at(&self) -> u64 {
		self.mapped_at.load(Ordering::Acquire)
	}

	pub fn set_mapped_at(&self, virt: u64) {
		self.mapped_at.store(virt, Ordering::Release);
	}
}

impl KernelObject for DeviceMemory {
	fn header(&self) -> &ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> ObjectType {
		ObjectType::DeviceMemory
	}

	fn as_any(&self) -> &dyn Any {
		self
	}

	fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
		self
	}
}

impl Drop for DeviceMemory {
	fn drop(&mut self) {
		// Tear down the mapping so the VA window is not left pointing at the device
		// after the capability is gone. The physical range is hardware, not owned
		// RAM, so nothing is freed.
		let base = self.mapped_at.load(Ordering::Acquire);
		if base != 0 {
			for i in 0..self.pages() {
				paging::unmap_page(base + i as u64 * PAGE_SIZE);
			}
		}
	}
}
