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
pub fn alloc_pages(bs: *mut BootServices, pages: usize) -> Option<u64> {
	let mut addr: u64 = 0;
	let status = unsafe { ((*bs).allocate_pages)(uefi::ALLOCATE_ANY_PAGES, uefi::LOADER_DATA, pages, &mut addr) };
	if uefi::is_error(status) { None } else { Some(addr) }
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
pub fn memory_map_snapshot(bs: *mut BootServices) -> Option<(*mut uefi::MemoryDescriptor, usize, usize, usize)> {
	let mut map_size = 0usize;
	let mut key = 0usize;
	let mut desc_size = 0usize;
	let mut desc_ver = 0u32;
	let status = unsafe { ((*bs).get_memory_map)(&mut map_size, core::ptr::null_mut(), &mut key, &mut desc_size, &mut desc_ver) };
	if status != uefi::STATUS_BUFFER_TOO_SMALL || desc_size == 0 || desc_size < core::mem::size_of::<uefi::MemoryDescriptor>() {
		return None;
	}
	map_size += desc_size * 8;
	let pages = map_size.div_ceil(PAGE_SIZE as usize);
	let buf = alloc_pages(bs, pages)? as *mut uefi::MemoryDescriptor;
	let status = unsafe { ((*bs).get_memory_map)(&mut map_size, buf, &mut key, &mut desc_size, &mut desc_ver) };
	if uefi::is_error(status) {
		unsafe { ((*bs).free_pages)(buf as u64, pages) };
		return None;
	}
	Some((buf, pages, map_size, desc_size))
}

// Translate the EFI memory map into the boot protocol's region array (sorted
// ascending by base and coalesced). Returns the region count.
pub fn translate_map(buf: *const uefi::MemoryDescriptor, map_size: usize, desc_size: usize, regions: *mut MemRegion) -> Option<usize> {
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
			*regions.add(n) = MemRegion { base: d.phys_start, length: d.page_count * PAGE_SIZE, kind, _pad: 0 };
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
	let mut w = 0usize;
	for r in 1..n {
		let cur = unsafe { *regions.add(r) };
		let last = unsafe { &mut *regions.add(w) };
		if cur.kind == last.kind && last.base + last.length == cur.base {
			last.length += cur.length;
		} else {
			w += 1;
			unsafe { *regions.add(w) = cur };
		}
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
		uefi::ACPI_RECLAIM_MEMORY => bootproto::MEM_ACPI_RECLAIMABLE,
		uefi::ACPI_MEMORY_NVS => bootproto::MEM_ACPI_NVS,
		uefi::UNUSABLE_MEMORY => bootproto::MEM_BAD,
		_ => bootproto::MEM_RESERVED,
	}
}

// The highest physical address any memory-map descriptor reaches.
pub fn memory_top(bs: *mut BootServices) -> u64 {
	let Some((buf, pages, map_size, desc_size)) = memory_map_snapshot(bs) else {
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
// THE REJECTS, because freeing one invites the next request to return the same block. The rejects
// are firmware pages and `ExitBootServices` reclaims the lot.
//
// `None` when every attempt landed on the destination - a machine whose free memory is exactly where
// the kernel goes, which is a refusal rather than a placement to force.
pub fn staging_clear_of(bs: *mut BootServices, pages: usize, dest_low: u64, dest_high: u64) -> Option<u64> {
	let mut rejects: [u64; 16] = [0; 16];
	let mut reject_count = 0usize;
	loop {
		let candidate = alloc_pages(bs, pages)?;
		let span = (pages as u64).checked_mul(PAGE_SIZE).and_then(|bytes| candidate.checked_add(bytes))?;
		if span <= dest_low || candidate >= dest_high {
			return Some(candidate);
		}
		if reject_count == rejects.len() {
			return None;
		}
		rejects[reject_count] = candidate;
		reject_count += 1;
	}
}
