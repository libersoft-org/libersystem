//! The firmware's memory map: taking a snapshot of it, measuring it, translating it into the boot
//! protocol's region array, and the one status that is worth retrying `ExitBootServices` for.
//!
//! Moved out of the x86 backend so it can be driven by a mock firmware. Everything here is a
//! decision about numbers the firmware hands over - how many descriptors, in what stride, of what
//! type, and what to do when the key goes stale between two calls - and none of it was reachable
//! from a test while it lived inside a UEFI binary.

use crate::{self as uefi, BootServices};
use bootproto::MemRegion;

// The boot protocol carries this many regions. A firmware with more is fatal rather than truncated:
// see `translate_map`.
pub const MAX_REGIONS: usize = 512;

// The page size the loader allocates and aligns in (4 KiB, every architecture).
pub const PAGE_SIZE: u64 = 4096;

// Allocate `pages` 4 KiB pages of retained LOADER_DATA and return the physical base (0-checked None
// on failure).
//
// # Safety
// `bs` must be the live `BootServices` table this firmware handed the loader, before
// `ExitBootServices`. Nothing in this signature says so - a raw pointer is a number, and a safe
// function taking one promises any number will do (UEFI-005).
pub unsafe fn alloc_pages(bs: *mut BootServices, pages: usize) -> Option<u64> {
	// UNDER THE CEILING, when one has been declared.
	//
	// `ALLOCATE_ANY_PAGES` lets firmware place a retained allocation anywhere in physical memory,
	// and everything the loader HANDS TO THE KERNEL goes through here: BootInfo, module arrays,
	// region arrays, file reads, RISC-V scratch. The non-x86 kernels enter Rust on a boot stub with
	// a FIXED early direct map - 4 GB on aarch64, 8 GB on riscv64 - so an allocation above that is
	// memory the kernel cannot address at the moment it is asked to read it. On a machine with RAM
	// high in the physical space, firmware is entitled to put it there.
	//
	// x86 declares no ceiling and keeps the old behaviour: it builds its own direct map over all RAM
	// before entering the kernel, so it has no such limit to respect.
	let ceiling = ALLOC_CEILING.load(core::sync::atomic::Ordering::Relaxed);
	let mut addr: u64 = if ceiling == 0 { 0 } else { ceiling };
	let policy = if ceiling == 0 { uefi::ALLOCATE_ANY_PAGES } else { uefi::ALLOCATE_MAX_ADDRESS };
	let status = unsafe { ((*bs).allocate_pages)(policy, uefi::LOADER_DATA, pages, &mut addr) };
	if uefi::is_error(status) { None } else { Some(addr) }
}

// The same, in the loader's own SCRATCH class: memory the kernel may reclaim the moment it runs
// (LDR-012).
//
// For allocations whose life ends at the handoff and which cannot be freed before it - the final
// memory-map buffer is read AT `ExitBootServices`, and there is no firmware afterwards to free it.
// `alloc_pages` above is for what the kernel must KEEP; this is for what it must not have to keep.
//
// # Safety
//
// Same contract as `alloc_pages`: `bs` must be the live `BootServices` table, before
// `ExitBootServices`.
pub unsafe fn alloc_scratch_pages(bs: *mut BootServices, pages: usize) -> Option<u64> {
	let ceiling = ALLOC_CEILING.load(core::sync::atomic::Ordering::Relaxed);
	let mut addr: u64 = if ceiling == 0 { 0 } else { ceiling };
	let policy = if ceiling == 0 { uefi::ALLOCATE_ANY_PAGES } else { uefi::ALLOCATE_MAX_ADDRESS };
	let status = unsafe { ((*bs).allocate_pages)(policy, uefi::OS_LOADER_SCRATCH, pages, &mut addr) };
	if uefi::is_error(status) { None } else { Some(addr) }
}

// The highest physical address a retained loader allocation may occupy, or 0 for no limit.
//
// Set once by an architecture whose kernel enters on a fixed early direct map, BEFORE any handoff
// allocation is made. It is a `static` rather than a parameter because every caller of `alloc_pages`
// hands its result to the kernel and none of them should be able to forget.
static ALLOC_CEILING: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn set_alloc_ceiling(max_physical_address: u64) {
	ALLOC_CEILING.store(max_physical_address, core::sync::atomic::Ordering::Relaxed);
}

// Whether `ExitBootServices` refusing with `status` is worth retrying with a fresh memory map.
//
// ONLY a stale map key is - the specification's `EFI_INVALID_PARAMETER`. The loader's loop used to
// retry on EVERY error forever, which is the least informative possible response to a firmware
// saying something else is wrong; a machine that hangs silently at the hand-off tells nobody which
// call refused or why.
pub fn exit_retryable(status: uefi::Status) -> bool {
	status == uefi::STATUS_INVALID_PARAMETER
}

// Take the firmware's memory map into fresh pages. Returns the buffer, its page count (for the
// caller's `free_pages`), the map's byte size and the descriptor stride.
//
// Factored out because two callers need the same six-step dance - size, pad, allocate, fetch,
// check, stride - and the first of them had a divide-by-zero in it until the sizing call's status
// was checked. One copy, one set of checks.
//
// # Safety
// `bs` must be the live `BootServices` table, before `ExitBootServices`. The returned buffer is a
// raw pointer into pages the caller must give back with `free_pages` using the returned count.
pub unsafe fn memory_map_snapshot(bs: *mut BootServices) -> Option<(*mut uefi::MemoryDescriptor, usize, usize, usize)> {
	let mut map_size = 0usize;
	let mut key = 0usize;
	let mut desc_size = 0usize;
	let mut desc_ver = 0u32;
	let status = unsafe { ((*bs).get_memory_map)(&mut map_size, core::ptr::null_mut(), &mut key, &mut desc_size, &mut desc_ver) };
	if status != uefi::STATUS_BUFFER_TOO_SMALL || desc_size == 0 || desc_size < core::mem::size_of::<uefi::MemoryDescriptor>() {
		return None;
	}
	// CHECKED, like `memory_top` in this file already is. Every number here came from the firmware,
	// and the memory map is the one input that describes the whole address space - so a `desc_size`
	// that is absurd should refuse rather than wrap into a small allocation the second call then
	// overruns.
	let Some(headroom) = desc_size.checked_mul(8) else { return None };
	let Some(sized) = map_size.checked_add(headroom) else { return None };
	map_size = sized;
	let pages = map_size.div_ceil(PAGE_SIZE as usize);
	let buf = unsafe { alloc_pages(bs, pages) }? as *mut uefi::MemoryDescriptor;
	let status = unsafe { ((*bs).get_memory_map)(&mut map_size, buf, &mut key, &mut desc_size, &mut desc_ver) };
	if uefi::is_error(status) {
		unsafe { ((*bs).free_pages)(buf as u64, pages) };
		return None;
	}
	// AND `desc_size` IS RE-CHECKED, because this call reports it again.
	//
	// The SIZING call's answer was validated above and nothing looked at the second one - and the
	// second is the one that describes the buffer now in hand. Firmware that reported one stride for
	// the sizing and another for the fill would have had every descriptor after the first read at
	// the wrong offset, out of a buffer whose contents are the one structure that says which RAM
	// exists. Ordinary firmware reports the same number twice; the helper's comments claimed more
	// independence than it had.
	// AND NOT MORE THAN THE BUFFER IT WAS GIVEN.
	//
	// `map_size` is an input capacity AND a firmware output through the same variable, and only the
	// stride and divisibility were checked afterwards. A defective firmware can return success while
	// enlarging it to a stride-aligned value bigger than the buffer; `translate_map` then derives its
	// entry count from that number and walks past the allocation, reading unrelated firmware memory
	// as descriptors - which can publish invented usable regions straight into the frame allocator.
	// Correct firmware would answer BUFFER_TOO_SMALL; the caller still owns not trusting a count
	// beyond the capacity it supplied.
	if map_size > pages * PAGE_SIZE as usize {
		unsafe { ((*bs).free_pages)(buf as u64, pages) };
		return None;
	}
	if desc_size == 0 || desc_size < core::mem::size_of::<uefi::MemoryDescriptor>() || map_size % desc_size != 0 {
		unsafe { ((*bs).free_pages)(buf as u64, pages) };
		return None;
	}
	Some((buf, pages, map_size, desc_size))
}

// Translate the EFI memory map into the boot protocol's region array (sorted
// ascending by base and coalesced). Returns the region count.
//
// # Safety
// `buf` must point at `map_size` readable bytes of `MemoryDescriptor`s laid out at `desc_size`
// stride - the shape `memory_map_snapshot` returns - and `regions` at `MAX_REGIONS` writable
// entries. None of that is checkable from the arguments, which are three numbers and two addresses.
pub unsafe fn translate_map(buf: *const uefi::MemoryDescriptor, map_size: usize, desc_size: usize, regions: *mut MemRegion) -> Option<usize> {
	// ITS OWN INVARIANT, not its caller's. The only caller today validates `desc_size` before
	// getting here, so the division was safe - by a fact about somewhere else. A shared helper that
	// divides by an argument holds the argument's precondition itself, or the day a second caller
	// appears it divides by zero.
	if desc_size == 0 {
		return None;
	}
	// AND IT HAS TO BE A DESCRIPTOR. `desc_size` is the firmware's stride, which may legitimately be
	// LARGER than this structure - the specification says so, precisely to allow future fields - and
	// may not be smaller: reading a `MemoryDescriptor` out of fewer bytes than one takes reads past
	// the entry into the next, or past the buffer at the last.
	if desc_size < core::mem::size_of::<uefi::MemoryDescriptor>() {
		return None;
	}
	// AND THE MAP HAS TO BE A WHOLE NUMBER OF THEM. `map_size / desc_size` silently discarded a
	// partial tail, which for the one structure that says which RAM exists is the same
	// looks-complete-and-is-not failure the `MAX_REGIONS` refusal above was written for.
	if map_size % desc_size != 0 {
		return None;
	}
	let entries = map_size / desc_size;
	let mut n = 0usize;
	for i in 0..entries {
		// FATAL, not silent. Breaking here handed the kernel a map that looked complete and was
		// missing its tail - the worst available failure mode for the one structure that says which
		// RAM exists, and one that would surface as memory corruption long afterwards.
		if n >= MAX_REGIONS {
			// REFUSED, NOT TRUNCATED, and reported rather than panicked here: the caller is the
			// loader, which has a serial port to say so on and a machine to stop. Breaking out
			// instead - which is what this did before it panicked - handed the kernel a map that
			// looked complete and was missing its tail, the worst available failure mode for the
			// one structure that says which RAM exists.
			return None;
		}
		let d = unsafe { &*((buf as *const u8).add(i * desc_size) as *const uefi::MemoryDescriptor) };
		let kind = region_kind(d.ty);
		unsafe {
			// A page count from the firmware, times the page size. Unchecked this wraps a descriptor
			// claiming 2^52 pages into a small region - a memory map the kernel then believes.
			let Some(length) = d.page_count.checked_mul(PAGE_SIZE) else { return None };
			// AND THE REGION'S END. The base comes off the firmware too, and a descriptor whose
			// start plus length leaves the address space describes a region no machine has - which
			// the kernel would then carve, seed or map.
			if d.phys_start.checked_add(length).is_none() {
				return None;
			}
			*regions.add(n) = MemRegion { base: d.phys_start, length, kind, _pad: 0 };
		}
		n += 1;
	}
	// Insertion sort ascending by base (region counts are small).
	for i in 1..n {
		let mut j = i;
		while j > 0 {
			let a = unsafe { *regions.add(j - 1) };
			let b = unsafe { *regions.add(j) };
			if a.base <= b.base {
				break;
			}
			unsafe {
				*regions.add(j - 1) = b;
				*regions.add(j) = a;
			}
			j -= 1;
		}
	}
	// Coalesce adjacent same-kind runs in place.
	if n == 0 {
		return Some(0);
	}
	// COALESCE ADJACENT RUNS AND CANONICALIZE OVERLAPS INTO A DISJOINT PARTITION.
	//
	// Only exactly-adjacent same-kind runs were merged, and OVERLAPPING descriptors were passed
	// through as independent regions - so the same physical page could be described twice, once as
	// usable and once as reserved, and handed to two owners. Firmware does produce these.
	//
	// The precedence is restrictive and it is the whole point: where two descriptors disagree about
	// a byte, the one that is NOT usable owns it. An ambiguity that resolved to usable would put
	// MMIO or firmware-owned memory into the frame allocator, which is the direction that corrupts.
	// Where both are non-usable the first keeps the overlap: neither answer is usable, so either is
	// safe, and keeping the earlier one makes the pass deterministic.
	let restrictive = |kind: u32| kind != bootproto::MEM_USABLE;
	let mut w = 0usize;
	for r in 1..n {
		let cur = unsafe { *regions.add(r) };
		let last = unsafe { &mut *regions.add(w) };
		// Both sums checked: this pass adds firmware-provided lengths, and the comparison that
		// decides whether to coalesce is itself a sum.
		let Some(last_end) = last.base.checked_add(last.length) else { return None };
		let Some(cur_end) = cur.base.checked_add(cur.length) else { return None };

		// Disjoint, in sorted order: the ordinary case.
		if cur.base >= last_end {
			if cur.kind == last.kind && cur.base == last_end {
				last.length = cur_end - last.base;
			} else {
				w += 1;
				unsafe { *regions.add(w) = cur };
			}
			continue;
		}

		// Overlapping. Same kind is not a conflict - it is one region described twice.
		if cur.kind == last.kind {
			if cur_end > last_end {
				last.length = cur_end - last.base;
			}
			continue;
		}

		if restrictive(cur.kind) && !restrictive(last.kind) {
			// The restrictive descriptor takes the contested bytes: the usable one gives up its
			// tail, and disappears entirely if that was all of it.
			last.base = last.base;
			last.length = cur.base - last.base;
			if last.length == 0 {
				// It contributed nothing; overwrite it rather than keeping an empty region.
				unsafe { *regions.add(w) = cur };
			} else {
				w += 1;
				unsafe { *regions.add(w) = cur };
			}
			continue;
		}

		// `last` is restrictive (or both are): it keeps the overlap and `cur` starts after it.
		if cur_end <= last_end {
			// Fully contained - nothing of `cur` survives.
			continue;
		}
		w += 1;
		unsafe { *regions.add(w) = MemRegion { base: last_end, length: cur_end - last_end, kind: cur.kind, _pad: 0 } };
	}
	Some(w + 1)
}

// Map an EFI memory type onto a boot-protocol region kind. Conventional and
// boot-services memory become usable (free after exit); loader memory is retained
// (it holds the kernel image, packages, page tables, BootInfo, stack, and
// trampoline); everything else is reserved / ACPI / bad as reported.
fn region_kind(ty: u32) -> u32 {
	match ty {
		uefi::CONVENTIONAL_MEMORY | uefi::BOOT_SERVICES_CODE | uefi::BOOT_SERVICES_DATA => bootproto::MEM_USABLE,
		uefi::LOADER_CODE | uefi::LOADER_DATA => bootproto::MEM_BOOTLOADER,
		// The loader's own scratch class: retained through the exit and free the moment the kernel
		// is running (LDR-012).
		uefi::OS_LOADER_SCRATCH => bootproto::MEM_BOOTLOADER_RECLAIMABLE,
		uefi::ACPI_RECLAIM_MEMORY => bootproto::MEM_ACPI_RECLAIMABLE,
		uefi::ACPI_MEMORY_NVS => bootproto::MEM_ACPI_NVS,
		uefi::UNUSABLE_MEMORY => bootproto::MEM_BAD,
		_ => bootproto::MEM_RESERVED,
	}
}

// The highest physical address any memory-map descriptor reaches.
//
// # Safety
// `bs` must be the live `BootServices` table, before `ExitBootServices`.
pub unsafe fn memory_top(bs: *mut BootServices) -> u64 {
	let Some((buf, pages, map_size, desc_size)) = (unsafe { memory_map_snapshot(bs) }) else {
		return 0;
	};
	// RAM ONLY. This took the maximum over EVERY descriptor, so a firmware that describes a PCI
	// window or an MMIO aperture high in the address space made the HHDM and the identity map span
	// the whole interval in 2 MiB pages - megabytes of page tables, seconds of boot time, a direct
	// map laid over physical holes, and device memory mapped write-back, when only the framebuffer
	// is given `PCD | PWT`. The kernel translates RAM through the HHDM; it reaches devices through
	// its own mappings.
	let mut top = 0u64;
	let entries = map_size / desc_size;
	for i in 0..entries {
		let d = unsafe { &*((buf as *const u8).add(i * desc_size) as *const uefi::MemoryDescriptor) };
		if d.ty == uefi::MEMORY_MAPPED_IO || d.ty == uefi::MEMORY_MAPPED_IO_PORT_SPACE {
			continue;
		}
		let Some(end) = d.page_count.checked_mul(PAGE_SIZE).and_then(|bytes| d.phys_start.checked_add(bytes)) else {
			continue;
		};
		if end > top {
			top = end;
		}
	}
	unsafe { ((*bs).free_pages)(buf as u64, pages) };
	top
}

// Staging memory that does not overlap the span the kernel has to end up in.
//
// THE FIRMWARE CHOOSES WHERE, and on the platform this was written for the two collide: on QEMU
// `virt` the riscv64 kernel is linked at 0x8020_0000, which is where U-Boot itself is running, and
// `AllocateAddress` there SUCCEEDED because that U-Boot does not reserve its own image. The loader
// cheerfully overwrote the firmware it was still calling into, and the next firmware call - inside
// `ExitBootServices` - ran into whatever the kernel had put there. The symptom was a boot with no
// kernel output at all, ever, on that path; the loader's own prints kept working because they go
// straight to the UART and never enter the firmware.
//
// So the kernel is staged somewhere else and copied into place after the last firmware call, and
// this is the part that has to find "somewhere else": ask, check, and ask again while HOLDING ON TO
// THE REJECTS, because freeing one invites the next request to return the same block.
//
// AND THEN GIVES THEM BACK, on every path out. The comment used to say `ExitBootServices` reclaims
// them, and that is not true of the map this loader hands over: `LOADER_DATA` translates to
// `MEM_BOOTLOADER`, which the kernel's frame allocator never seeds - so every rejected candidate was
// a kernel-sized block of memory lost for the life of the system, and the exhausted path also
// allocated a seventeenth candidate it then dropped on the floor without even recording it.
//
// `None` when every attempt landed on the destination - a machine whose free memory is exactly where
// the kernel goes, which is a refusal rather than a placement to force.
//
// # Safety
// `bs` must be the live `BootServices` table, before `ExitBootServices`.
pub unsafe fn staging_clear_of(bs: *mut BootServices, pages: usize, dest_low: u64, dest_high: u64) -> Option<u64> {
	let mut rejects: [u64; 16] = [0; 16];
	let mut reject_count = 0usize;
	// Give every rejected candidate back once a decision has been made, whatever the decision is.
	// Until then they stay allocated on purpose, so the next request cannot return the same block.
	let release = |rejects: &[u64], count: usize| {
		for &address in rejects.iter().take(count) {
			unsafe { ((*bs).free_pages)(address, pages) };
		}
	};
	loop {
		let Some(candidate) = (unsafe { alloc_pages(bs, pages) }) else {
			release(&rejects, reject_count);
			return None;
		};
		let span = (pages as u64).checked_mul(PAGE_SIZE).and_then(|bytes| candidate.checked_add(bytes));
		let Some(span) = span else {
			// Arithmetic that cannot be represented is a candidate this cannot reason about, and it
			// was previously returned to nobody: the `?` left the function with the candidate still
			// allocated and every reject with it.
			unsafe { ((*bs).free_pages)(candidate, pages) };
			release(&rejects, reject_count);
			return None;
		};
		if span <= dest_low || candidate >= dest_high {
			release(&rejects, reject_count);
			return Some(candidate);
		}
		if reject_count == rejects.len() {
			// This candidate too - it is the one the old code allocated after the array was full
			// and then lost without recording.
			unsafe { ((*bs).free_pages)(candidate, pages) };
			release(&rejects, reject_count);
			return None;
		}
		rejects[reject_count] = candidate;
		reject_count += 1;
	}
}
