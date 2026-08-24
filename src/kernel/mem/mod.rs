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

// What the boot stub actually mapped, on the ports that build their own direct map. Zero means "no
// ceiling recorded", which is the x86_64 case: there the loader builds the HHDM from the same map
// the kernel is handed, so the map IS the extent.
static DIRECT_MAP_MAPPED: AtomicU64 = AtomicU64::new(0);

// Is `[phys, phys + len)` inside the direct map?
//
// FOR FIRMWARE POINTERS. Everything the kernel allocates is inside by construction; what is not is
// an address ACPI or a device tree handed over, and those were dereferenced on the strength of a
// signature match. A pointer past the end of the HHDM is a wild read in early boot, before there is
// a fault handler worth the name - and refusing it costs one comparison.
// Asked by the x86 SMP path of a firmware table pointer before it reads one, and by the
// device-tree ports of every controller range the machine description names - both before the first
// access through it, and both against the ceiling the boot prologue published.
// How far `phys_to_virt` translates, which is the larger of the two ceilings - see
// `within_direct_map`, whose whole answer is this number.
//
// IT IS NOT THE TOP OF RAM, and a caller that derives it from the memory map is right only on the
// port whose direct map is sized from that map. A device-tree port's boot stub maps a FIXED window -
// 4 GiB on aarch64, 8 GiB on riscv64 - which reaches past the last byte of memory the machine has,
// and every address inside that window is one `phys_to_virt` translates whether or not it is RAM.
fn direct_map_ceiling() -> u64 {
	DIRECT_MAP_LIMIT.load(Ordering::Relaxed).max(DIRECT_MAP_MAPPED.load(Ordering::Relaxed))
}

// The same, for the test that asserts on the ceiling itself.
#[cfg(test)]
pub fn direct_map_ceiling_for_test() -> u64 {
	direct_map_ceiling()
}

pub fn within_direct_map(phys: u64, len: u64) -> bool {
	// THE CEILING THE STUB PUBLISHED COUNTS ON ITS OWN, before any memory map has been retained.
	//
	// This read only the map-derived limit, which is published when the frame allocator is seeded -
	// so a device-tree port asking about its interrupt controller BEFORE that point got `false` for
	// every address, including the one the machine had just named. The controller then looked
	// unreachable and the boot refused a machine that was describing itself correctly.
	//
	// Both numbers answer the same question - how far `phys_to_virt` translates - and the larger of
	// the two is the honest answer at any moment: the stub's span is a fact from the instant the MMU
	// is on, and the map-derived one only ever raises it.
	let limit = direct_map_ceiling();
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
	// CLAMPED TO WHAT IS MAPPED. The map says what memory EXISTS; on aarch64 and riscv64 the boot
	// stub maps a fixed span in assembly before any map is read, so a machine with more RAM than
	// that would otherwise have `within_direct_map` answering true for addresses `phys_to_virt`
	// cannot translate.
	let mapped = DIRECT_MAP_MAPPED.load(Ordering::Relaxed);
	let top = if mapped == 0 { top } else { top.min(mapped) };
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

// Publish the direct map's extent from what the BOOT STUB ACTUALLY MAPPED, on the ports that build
// their own.
//
// `publish_direct_map_limit` derives the limit from the retained memory map, and its reasoning is
// the loader's: the HHDM covers the map because the loader built it from the map. That is true on
// x86_64 and false on the device-tree ports, where the stub maps a FIXED span in assembly before any
// map is read - 0..4 GiB on aarch64 (1 GB blocks at `aarch64/boot.rs:47`) and 0..8 GiB on riscv64
// (`riscv64/boot.rs:55-63`). On a machine whose device tree reports more RAM than that,
// `within_direct_map` answers true for physical addresses `phys_to_virt` does not translate, and the
// frame allocator can be seeded with banks the kernel cannot reach through the HHDM at all.
//
// A CEILING, not a maximum: `fetch_max` is what the map-derived version needs (two boot paths retain
// at different points) and is the wrong operation for a hard limit, so this clamps instead. Long
// term the direct map is built from the banks and this goes away; until then the limit says what is
// mapped rather than what exists.
// A CEILING RECORDED SEPARATELY, because it is published before the memory map exists. The stub sets
// this during early boot and `publish_direct_map_limit` applies it whenever the map is retained, so
// the two do not depend on each other's order - the first attempt stored straight into the limit and
// the map's later `fetch_max` simply raised it again.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn set_direct_map_extent(mapped_bytes: u64) {
	DIRECT_MAP_MAPPED.store(mapped_bytes, Ordering::Relaxed);
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

// THE LOADER'S HAND-OFF, so x86_64's. aarch64 and riscv64 build their region list from the device
// tree in their own prologue and call `frame::init` with it directly.
#[cfg(target_arch = "x86_64")]
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
