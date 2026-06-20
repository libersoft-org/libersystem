// Physical frame allocator.
//
// Builds a free list of 4 KiB physical frames from the Limine memory map. The
// list is threaded *through the free frames themselves*: each free frame stores
// the physical address of the next free frame in its first 8 bytes, accessed via
// the higher-half direct map (HHDM). This is an O(1) alloc/free stack that needs
// no separate bookkeeping memory.
//
// The allocator is global and guarded by a SpinLock, so it is safe to call from
// any core.

#![allow(dead_code)]

use limine::memory_map::EntryType;
use limine::response::MemoryMapResponse;

use alloc::vec::Vec;

use crate::sync::SpinLock;

pub const PAGE_SIZE: u64 = 4096;

// 0 is used as the "empty list" sentinel. Physical frame 0 is never handed out
// (it is not part of any usable Limine region in practice, and we skip it
// defensively during init), so it is safe to overload as null here.
struct FrameAllocator {
	free_head: u64,
	free_count: usize,
	hhdm: u64,
}

impl FrameAllocator {
	const fn new() -> Self {
		Self { free_head: 0, free_count: 0, hhdm: 0 }
	}

	// SAFETY: `phys` must be a page-aligned physical frame that is currently
	// unused and mapped by the HHDM.
	unsafe fn push(&mut self, phys: u64) {
		unsafe {
			let link = (self.hhdm + phys) as *mut u64;
			link.write_volatile(self.free_head);
			self.free_head = phys;
			self.free_count += 1;
		}
	}

	fn pop(&mut self) -> Option<u64> {
		if self.free_head == 0 {
			return None;
		}
		let phys = self.free_head;
		let link = (self.hhdm + phys) as *const u64;
		self.free_head = unsafe { link.read_volatile() };
		self.free_count -= 1;
		Some(phys)
	}
}

static ALLOCATOR: SpinLock<FrameAllocator> = SpinLock::new(FrameAllocator::new());

// Populate the free list from the usable regions of the Limine memory map.
pub fn init(memory_map: &MemoryMapResponse, hhdm: u64) {
	let mut allocator = ALLOCATOR.lock();
	allocator.hhdm = hhdm;
	for entry in memory_map.entries() {
		if entry.entry_type != EntryType::USABLE {
			continue;
		}
		let mut base = align_up(entry.base, PAGE_SIZE);
		let end = entry.base + entry.length;
		while base + PAGE_SIZE <= end {
			if base != 0 {
				unsafe { allocator.push(base) };
			}
			base += PAGE_SIZE;
		}
	}
}

// Allocate one physical frame, returning its physical address.
pub fn allocate() -> Option<u64> {
	ALLOCATOR.lock().pop()
}

// Return a physical frame to the free list.
//
// SAFETY: `phys` must be a frame previously obtained from `allocate` that is no
// longer in use (and no longer mapped anywhere it could be written through).
pub fn deallocate(phys: u64) {
	unsafe { ALLOCATOR.lock().push(phys) };
}

// The number of whole pages needed to hold `bytes` (at least one).
pub fn pages_for(bytes: usize) -> usize {
	bytes.div_ceil(PAGE_SIZE as usize).max(1)
}

// Allocate `pages` physical frames, returning their addresses, or None if not
// enough are available (any frames already taken are returned on failure). The
// shared multi-frame allocation the frame-backed kernel objects use.
pub fn allocate_pages(pages: usize) -> Option<Vec<u64>> {
	let mut frames = Vec::with_capacity(pages);
	for _ in 0..pages {
		match allocate() {
			Some(phys) => frames.push(phys),
			None => {
				free_pages(&frames);
				return None;
			}
		}
	}
	Some(frames)
}

// Return a set of frames to the free list.
pub fn free_pages(frames: &[u64]) {
	for &phys in frames {
		deallocate(phys);
	}
}

// Number of frames currently free.
pub fn free_count() -> usize {
	ALLOCATOR.lock().free_count
}

const fn align_up(value: u64, align: u64) -> u64 {
	(value + align - 1) & !(align - 1)
}
