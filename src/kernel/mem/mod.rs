// Memory subsystem: physical frames, paging helpers, and the kernel heap.
//
// `init` is called once early in boot with the Limine memory map and HHDM
// offset. After it returns, `alloc` collections (Box, Vec, ...) are usable.

pub mod frame;
pub mod heap;

use core::sync::atomic::{AtomicU64, Ordering};

use limine::response::MemoryMapResponse;

// HHDM (higher-half direct map) offset: virt = phys + HHDM_OFFSET for all
// physical memory. Published once during init and read-only afterwards.
static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);

pub fn hhdm_offset() -> u64 {
	HHDM_OFFSET.load(Ordering::Relaxed)
}

pub fn init(memory_map: &MemoryMapResponse, hhdm: u64) {
	HHDM_OFFSET.store(hhdm, Ordering::Relaxed);
	frame::init(memory_map, hhdm);
	heap::init();
}
