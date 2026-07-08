// Memory subsystem: physical frames, paging helpers, and the kernel heap.
//
// `init` is called once early in boot with the loader's memory map and HHDM
// offset. After it returns, `alloc` collections (Box, Vec, ...) are usable.

pub mod frame;
pub mod heap;

use core::sync::atomic::{AtomicU64, Ordering};

use alloc::vec::Vec;

use bootproto::MemRegion;

use crate::sync::SpinLock;

// HHDM (higher-half direct map) offset: virt = phys + HHDM_OFFSET for all
// physical memory. Published once during init and read-only afterwards.
static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);

// The boot memory map, retained past init (the loader's hand-off is one-shot) so the
// physical layout stays inspectable at runtime - SYS_MEMMAP_GET reads it for `lsmem`.
static MEMMAP: SpinLock<Vec<abi::MemmapRegion>> = SpinLock::new(Vec::new());

pub fn hhdm_offset() -> u64 {
	HHDM_OFFSET.load(Ordering::Relaxed)
}

// Publish the higher-half direct-map offset from the arch backend. aarch64 builds
// its own boot page tables (a direct map at KERNEL_VA_OFFSET) rather than taking
// an HHDM from a bootloader, so it sets the offset here before seeding the frame
// allocator, rather than through `init`.
#[cfg(target_arch = "aarch64")]
pub fn set_hhdm_offset(offset: u64) {
	HHDM_OFFSET.store(offset, Ordering::Relaxed);
}

// Retain the boot memory map for runtime inspection (SYS_MEMMAP_GET / lsmem). The x86
// path retains it inside `init`; aarch64 brings memory up in separate steps (frame /
// heap init directly), so it retains the map here once the heap is available.
#[cfg(target_arch = "aarch64")]
pub fn retain_memmap(regions: &[MemRegion]) {
	let mut retained = MEMMAP.lock();
	for region in regions {
		retained.push(abi::MemmapRegion { base: region.base, length: region.length, kind: region.kind, _pad: 0 });
	}
}

// The number of retained boot memory-map regions.
pub fn memmap_len() -> usize {
	MEMMAP.lock().len()
}

// One retained boot memory-map region by index.
pub fn memmap_get(index: usize) -> Option<abi::MemmapRegion> {
	MEMMAP.lock().get(index).copied()
}

// Map a bootloader entry type onto the ABI's stable region-kind codes.
// The loader already hands the kernel these stable codes (bootproto MEM_* mirror
// abi MEMMAP_*), so the memory map is retained verbatim - no translation here.

pub fn init(regions: &[MemRegion], hhdm: u64) {
	HHDM_OFFSET.store(hhdm, Ordering::Relaxed);
	frame::init(regions);
	heap::init();
	// The heap is up now: the frame allocator's run table moves onto it (so
	// fragmentation is bounded by memory, not a fixed table), and the memory map
	// can be retained (Vec) for runtime inspection.
	frame::upgrade_to_heap();
	let mut retained = MEMMAP.lock();
	for region in regions {
		retained.push(abi::MemmapRegion { base: region.base, length: region.length, kind: region.kind, _pad: 0 });
	}
}
