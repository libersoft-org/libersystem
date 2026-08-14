// Memory subsystem: physical frames, paging helpers, and the kernel heap.
//
// `init` is called once early in boot with the loader's memory map and HHDM
// offset. After it returns, `alloc` collections (Box, Vec, ...) are usable.

pub mod frame;
pub mod heap;
pub mod tlb;
pub mod vapool;

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

// The end of the direct map: the highest physical address `virt = phys + HHDM_OFFSET` translates.
//
// The loader maps the HHDM over `align_up(memory_top(), 2 MiB)` and hands the kernel the SAME
// memory map it computed that from, reserved and ACPI regions included - so the top of the retained
// map, rounded the same way, is the extent rather than an estimate of it. Zero until `init` has run.
static DIRECT_MAP_LIMIT: AtomicU64 = AtomicU64::new(0);

// Is `[phys, phys + len)` inside the direct map?
//
// FOR FIRMWARE POINTERS. Everything the kernel allocates is inside by construction; what is not is
// an address ACPI or a device tree handed over, and those were dereferenced on the strength of a
// signature match. A pointer past the end of the HHDM is a wild read in early boot, before there is
// a fault handler worth the name - and refusing it costs one comparison.
pub fn within_direct_map(phys: u64, len: u64) -> bool {
	let limit = DIRECT_MAP_LIMIT.load(Ordering::Relaxed);
	if phys == 0 || limit == 0 {
		return false;
	}
	match phys.checked_add(len) {
		Some(end) => end <= limit,
		None => false,
	}
}

// Publish the direct map's extent from the retained memory map. Idempotent, and takes the highest
// answer it is ever given: the two boot paths retain their maps at different points.
fn publish_direct_map_limit(regions: &[MemRegion]) {
	let mut top = 0u64;
	for region in regions {
		top = top.max(region.base.saturating_add(region.length));
	}
	// Rounded up the way the loader rounds its map, so a table in the last partial 2 MiB of the
	// last region is inside the bound rather than one byte outside it.
	let top = top.next_multiple_of(2 * 1024 * 1024);
	DIRECT_MAP_LIMIT.fetch_max(top, Ordering::Relaxed);
}

// Publish the higher-half direct-map offset from the arch backend. aarch64 and
// riscv64 build their own boot page tables (a direct map at KERNEL_VA_OFFSET) rather
// than taking an HHDM from a bootloader, so they set the offset here before seeding
// the frame allocator, rather than through `init`.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn set_hhdm_offset(offset: u64) {
	HHDM_OFFSET.store(offset, Ordering::Relaxed);
}

// Retain the boot memory map for runtime inspection (SYS_MEMMAP_GET / lsmem). The x86
// path retains it inside `init`; aarch64 brings memory up in separate steps (frame /
// heap init directly), so it retains the map here once the heap is available.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn retain_memmap(regions: &[MemRegion]) {
	let mut retained = MEMMAP.lock();
	for region in regions {
		// ALLOC-OK: the firmware memory map, read once at boot.
		retained.push(abi::MemmapRegion { base: region.base, length: region.length, kind: region.kind, _pad: 0 });
	}
	publish_direct_map_limit(regions);
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
	// Reserve every top-level page-table entry the kernel's two growing windows can ever need,
	// while the kernel's is still the only address space in existence. After this point a new
	// address space copies a kernel half that is already complete, and nothing the heap or the
	// mmap pool does later can add an entry the copies would miss.
	heap::reserve_window();
	crate::syscall::reserve_kernel_vmap();
	let mut retained = MEMMAP.lock();
	for region in regions {
		// ALLOC-OK: the firmware memory map, read once at boot.
		retained.push(abi::MemmapRegion { base: region.base, length: region.length, kind: region.kind, _pad: 0 });
	}
	publish_direct_map_limit(regions);
}
