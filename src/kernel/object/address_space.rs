// AddressSpace kernel object.
//
// An address space wraps a page-table root (the CR3 value). For now every kernel
// thread shares the single kernel address space the bootloader set up, so this is
// a thin wrapper; per-process address spaces with their own page tables arrive
// with userspace. Threads hold an Arc to their address space so it outlives them.

#![allow(dead_code)]

use alloc::sync::Arc;
use core::any::Any;

use super::{KernelObject, ObjectHeader, ObjectType};
use crate::arch;

pub struct AddressSpace {
	header: ObjectHeader,
	// Physical address of the top-level page table (CR3).
	cr3: u64,
}

impl AddressSpace {
	// Capture the active address space (the kernel tables the bootloader built).
	pub fn kernel() -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), cr3: arch::context::read_cr3() })
	}

	// The page-table root to load into CR3 when this address space is active.
	pub fn cr3(&self) -> u64 {
		self.cr3
	}
}

impl KernelObject for AddressSpace {
	fn header(&self) -> &ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> ObjectType {
		ObjectType::AddressSpace
	}

	fn as_any(&self) -> &dyn Any {
		self
	}
}
