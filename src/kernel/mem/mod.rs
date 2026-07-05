// Memory subsystem: physical frames, paging helpers, and the kernel heap.
//
// `init` is called once early in boot with the Limine memory map and HHDM
// offset. After it returns, `alloc` collections (Box, Vec, ...) are usable.

pub mod frame;
pub mod heap;

use core::sync::atomic::{AtomicU64, Ordering};

use alloc::vec::Vec;

use limine::memory_map::EntryType;
use limine::response::MemoryMapResponse;

use crate::sync::SpinLock;

// HHDM (higher-half direct map) offset: virt = phys + HHDM_OFFSET for all
// physical memory. Published once during init and read-only afterwards.
static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);

// The boot memory map, retained past init (Limine's response is one-shot) so the
// physical layout stays inspectable at runtime - SYS_MEMMAP_GET reads it for `lsmem`.
static MEMMAP: SpinLock<Vec<abi::MemmapRegion>> = SpinLock::new(Vec::new());

pub fn hhdm_offset() -> u64 {
	HHDM_OFFSET.load(Ordering::Relaxed)
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
fn region_kind(entry_type: EntryType) -> u32 {
	match entry_type {
		EntryType::USABLE => abi::MEMMAP_USABLE,
		EntryType::RESERVED => abi::MEMMAP_RESERVED,
		EntryType::ACPI_RECLAIMABLE => abi::MEMMAP_ACPI_RECLAIMABLE,
		EntryType::ACPI_NVS => abi::MEMMAP_ACPI_NVS,
		EntryType::BAD_MEMORY => abi::MEMMAP_BAD,
		EntryType::BOOTLOADER_RECLAIMABLE => abi::MEMMAP_BOOTLOADER,
		EntryType::EXECUTABLE_AND_MODULES => abi::MEMMAP_KERNEL,
		EntryType::FRAMEBUFFER => abi::MEMMAP_FRAMEBUFFER,
		_ => abi::MEMMAP_RESERVED,
	}
}

pub fn init(memory_map: &MemoryMapResponse, hhdm: u64) {
	HHDM_OFFSET.store(hhdm, Ordering::Relaxed);
	frame::init(memory_map);
	heap::init();
	// The heap is up now, so the map can be retained (Vec) for runtime inspection.
	let mut retained = MEMMAP.lock();
	for entry in memory_map.entries() {
		retained.push(abi::MemmapRegion { base: entry.base, length: entry.length, kind: region_kind(entry.entry_type), _pad: 0 });
	}
}
