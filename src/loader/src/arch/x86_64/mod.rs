// x86_64 loader backend: build the kernel's page tables + BootInfo and jump to it.
//
// The x86 kernel entry (`kmain`) runs on the page hierarchy the loader hands it - the
// HHDM over all RAM, a low identity map that keeps the loader executing across the
// `mov cr3`, and the kernel image at its higher-half link addresses (W^X). It reads
// everything it needs (memory map, framebuffer, ACPI RSDP, the init/volume packages)
// from the `bootproto::BootInfo` this backend fills in. The architecture-neutral file
// I/O + entry live in main.rs; only this placement/hand-off is x86-specific.

pub mod paging;
pub mod serial;

use core::arch::asm;

use bootproto::{BootInfo, Framebuffer, MemRegion, Module};

use crate::uefi::{self, BootServices, Handle, SystemTable};
use crate::{align_down, alloc_pages, read_file};
use paging::{PageTables, HHDM_OFFSET, PAGE_2MB, PAGE_SIZE};

// The init/volume package filenames on the boot volume (the x86 loader reads them and
// hands the kernel their bytes as boot-protocol modules; the aarch64 kernel embeds them).
const INIT_PKG_FILE: &str = "init.pkg";
const VOLUME_PKG_FILE: &str = "volume.pkg";

// Round `v` up to a multiple of `align` (a power of two).
fn align_up(v: u64, align: u64) -> u64 {
	(v + align - 1) & !(align - 1)
}

// The kernel stack the loader hands over (in pages of 4 KiB): 128 KiB.
const STACK_PAGES: usize = 32;

// Upper bound on memory-map regions translated into the boot protocol.
const MAX_REGIONS: usize = 512;

// Upper bound on kernel PT_LOAD segments.
const MAX_SEGMENTS: usize = 16;

// Halt the core (panic path): interrupts off, hlt forever. panic=abort, no unwind.
pub fn halt() -> ! {
	loop {
		unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
	}
}

// Place the kernel and jump into it. Reads the init/volume packages off the boot
// volume (the kernel gets them as boot-protocol modules), loads the kernel ELF,
// gathers the framebuffer + RSDP, builds the page tables + BootInfo, snapshots the
// memory map, exits boot services, and switches to the kernel's page tables.
pub fn hand_off(bs: *mut BootServices, image_handle: Handle, system_table: *mut SystemTable, root: *mut uefi::FileProtocol, kernel: &[u8]) -> ! {
	let init_pkg = read_file(bs, root, INIT_PKG_FILE).expect("loader: cannot read init.pkg");
	let volume_pkg = read_file(bs, root, VOLUME_PKG_FILE).expect("loader: cannot read volume.pkg");
	serial::write_str("loader: packages loaded\n");

	// Load the kernel ELF: allocate + copy each PT_LOAD segment, record its
	// link-time virtual base, physical base, page count, and W/X flags.
	let mut segments = [KernelSegment::EMPTY; MAX_SEGMENTS];
	let (entry, seg_count) = load_kernel(bs, kernel, &mut segments);
	serial::write_str("loader: kernel ELF loaded\n");

	// Graphics Output framebuffer (optional - headless boots have none).
	let fb = locate_framebuffer(bs);

	// ACPI RSDP from the firmware configuration table.
	let rsdp = find_rsdp(system_table);

	// A page below 1 MiB for the AP real-mode trampoline (best effort).
	let trampoline = alloc_low_page(bs);

	// The kernel stack.
	let stack_phys = alloc_pages(bs, STACK_PAGES).expect("loader: cannot allocate kernel stack");
	let stack_top = HHDM_OFFSET + stack_phys + (STACK_PAGES as u64 * PAGE_SIZE);

	// Buffers the kernel keeps reading after hand-off: the BootInfo, the region
	// array, and the module array all live in LOADER_DATA (retained memory).
	let boot_info_phys = alloc_pages(bs, 1).expect("loader: cannot allocate BootInfo");
	let regions_pages = (core::mem::size_of::<MemRegion>() * MAX_REGIONS).div_ceil(PAGE_SIZE as usize);
	let regions_phys = alloc_pages(bs, regions_pages).expect("loader: cannot allocate region array");
	let modules_phys = alloc_pages(bs, 1).expect("loader: cannot allocate module array");

	// Publish the two loaded packages as modules.
	let modules = modules_phys as *mut Module;
	unsafe {
		*modules.add(0) = make_module(init_pkg, INIT_PKG_FILE);
		*modules.add(1) = make_module(volume_pkg, VOLUME_PKG_FILE);
	}

	// The highest physical address the HHDM and identity map must cover.
	let ram_top = align_up(memory_top(bs), PAGE_2MB);

	// Build the page hierarchy: HHDM over all RAM, the framebuffer uncacheable,
	// a low identity map for the CR3 switch, and the kernel's segments.
	let mut tables = PageTables::new(bs).expect("loader: cannot allocate PML4");
	tables.map_hhdm(0, ram_top, false).expect("loader: HHDM map failed");
	tables.map_identity(ram_top).expect("loader: identity map failed");
	if fb.present {
		let fb_base = align_down(fb.phys, PAGE_2MB);
		let fb_end = align_up(fb.phys + fb.size, PAGE_2MB);
		tables.map_hhdm(fb_base, fb_end - fb_base, true).expect("loader: framebuffer map failed");
	}
	for seg in &segments[..seg_count] {
		tables.map_kernel_segment(seg.virt, seg.phys, seg.pages, seg.writable, seg.executable).expect("loader: kernel map failed");
	}
	serial::write_str("loader: page tables built\n");

	// Fill in the boot protocol (all pointers are HHDM virtual addresses).
	let boot_info = boot_info_phys as *mut BootInfo;
	unsafe {
		(*boot_info).magic = bootproto::MAGIC;
		(*boot_info).version = bootproto::VERSION;
		(*boot_info)._pad0 = 0;
		(*boot_info).hhdm_offset = HHDM_OFFSET;
		(*boot_info).memmap = HHDM_OFFSET + regions_phys;
		(*boot_info).modules = HHDM_OFFSET + modules_phys;
		(*boot_info).modules_len = 2;
		(*boot_info).framebuffer = fb.info;
		(*boot_info).fb_present = fb.present as u32;
		(*boot_info)._pad1 = 0;
		(*boot_info).rsdp = rsdp;
		(*boot_info).smp_trampoline = trampoline;
		(*boot_info).dtb = 0; // x86 uses ACPI, not a device tree.
	}

	// Snapshot the memory map and exit boot services. GetMemoryMap must be the
	// last firmware call before ExitBootServices, so the region translation (no
	// allocation) happens inline and the whole thing retries if the map changed.
	let region_count = finalize_and_exit(bs, image_handle, regions_phys as *mut MemRegion);
	unsafe { (*boot_info).memmap_len = region_count as u64 };

	// Boot services are gone. Switch to the kernel's page tables and jump to its
	// entry with a pointer to the BootInfo in RDI (SysV first argument).
	let boot_info_virt = HHDM_OFFSET + boot_info_phys;
	unsafe {
		asm!(
			"mov cr3, {cr3}",
			"mov rsp, {stack}",
			"mov rdi, {info}",
			"jmp {entry}",
			cr3 = in(reg) tables.pml4,
			stack = in(reg) stack_top,
			info = in(reg) boot_info_virt,
			entry = in(reg) entry,
			options(noreturn),
		);
	}
}

// A loaded kernel segment: its link-time virtual base, backing physical base,
// page count, and W/X permissions.
#[derive(Clone, Copy)]
struct KernelSegment {
	virt: u64,
	phys: u64,
	pages: u64,
	writable: bool,
	executable: bool,
}

impl KernelSegment {
	const EMPTY: Self = Self { virt: 0, phys: 0, pages: 0, writable: false, executable: false };
}

// Load the kernel ELF: for each PT_LOAD segment, allocate its pages, copy the
// file bytes, zero the tail (BSS), and record the mapping. Returns (entry, count).
fn load_kernel(bs: *mut BootServices, kernel: &[u8], out: &mut [KernelSegment; MAX_SEGMENTS]) -> (u64, usize) {
	let image = crate::elf::Elf::parse(kernel).expect("loader: kernel is not a valid ELF64 executable");
	let mut count = 0usize;
	for i in 0..image.segment_count() {
		let Some(ph) = image.segment(i) else { continue };
		if ph.p_type != crate::elf::PT_LOAD || ph.p_memsz == 0 {
			continue;
		}
		let pages = ph.p_memsz.div_ceil(PAGE_SIZE);
		let phys = alloc_pages(bs, pages as usize).expect("loader: cannot allocate kernel segment");
		unsafe {
			core::ptr::write_bytes(phys as *mut u8, 0, (pages * PAGE_SIZE) as usize);
			if let Some(data) = image.segment_data(&ph) {
				core::ptr::copy_nonoverlapping(data.as_ptr(), phys as *mut u8, data.len());
			}
		}
		out[count] = KernelSegment { virt: align_down(ph.p_vaddr, PAGE_SIZE), phys, pages, writable: ph.p_flags & crate::elf::PF_W != 0, executable: ph.p_flags & crate::elf::PF_X != 0 };
		count += 1;
	}
	(image.entry, count)
}

// Build a boot-protocol module for a loaded package: its HHDM address, size, and
// NUL-padded name.
fn make_module(bytes: &[u8], name: &str) -> Module {
	let mut m = Module { addr: HHDM_OFFSET + bytes.as_ptr() as u64, size: bytes.len() as u64, name: [0; 32] };
	let n = name.len().min(m.name.len());
	m.name[..n].copy_from_slice(&name.as_bytes()[..n]);
	m
}

// The framebuffer the loader found, plus its physical base + byte size (for the
// HHDM mapping) and whether one is present at all.
struct FbResult {
	info: Framebuffer,
	phys: u64,
	size: u64,
	present: bool,
}

// Query the Graphics Output Protocol (the shared, architecture-neutral helper) and,
// when a framebuffer is present, build the x86 boot-protocol Framebuffer with an HHDM
// virtual `addr` (the loader maps the framebuffer into the HHDM below).
fn locate_framebuffer(bs: *mut BootServices) -> FbResult {
	let g = crate::locate_framebuffer(bs);
	if !g.present {
		return FbResult { info: unsafe { core::mem::zeroed() }, phys: 0, size: 0, present: false };
	}
	let info = Framebuffer { addr: HHDM_OFFSET + g.phys, width: g.width, height: g.height, pitch: g.pitch, bpp: 32, red_shift: g.red_shift, red_size: g.red_size, green_shift: g.green_shift, green_size: g.green_size, blue_shift: g.blue_shift, blue_size: g.blue_size, _pad: [0; 2] };
	FbResult { info, phys: g.phys, size: g.size, present: true }
}

// Scan the firmware configuration table for the ACPI 2.0 (then 1.0) RSDP,
// returning its physical address (0 if none).
fn find_rsdp(system_table: *mut SystemTable) -> u64 {
	let count = unsafe { (*system_table).number_of_table_entries };
	let entries = unsafe { (*system_table).configuration_table };
	let mut fallback = 0u64;
	for i in 0..count {
		let e = unsafe { &*entries.add(i) };
		if e.vendor_guid == uefi::ACPI_20_TABLE_GUID {
			return e.vendor_table as u64;
		}
		if e.vendor_guid == uefi::ACPI_10_TABLE_GUID {
			fallback = e.vendor_table as u64;
		}
	}
	fallback
}

// Reserve one page below 1 MiB for the AP bring-up trampoline; 0 if none is free.
fn alloc_low_page(bs: *mut BootServices) -> u64 {
	let mut addr: u64 = 0x0010_0000;
	let status = unsafe { ((*bs).allocate_pages)(uefi::ALLOCATE_MAX_ADDRESS, uefi::LOADER_DATA, 1, &mut addr) };
	if uefi::is_error(status) {
		0
	} else {
		addr
	}
}

// The highest physical address any memory-map descriptor reaches.
fn memory_top(bs: *mut BootServices) -> u64 {
	let mut map_size = 0usize;
	let mut key = 0usize;
	let mut desc_size = 0usize;
	let mut desc_ver = 0u32;
	// First call sizes the buffer.
	unsafe { ((*bs).get_memory_map)(&mut map_size, core::ptr::null_mut(), &mut key, &mut desc_size, &mut desc_ver) };
	map_size += desc_size * 8;
	let buf = match alloc_pages(bs, map_size.div_ceil(PAGE_SIZE as usize)) {
		Some(p) => p as *mut uefi::MemoryDescriptor,
		None => return 0,
	};
	let status = unsafe { ((*bs).get_memory_map)(&mut map_size, buf, &mut key, &mut desc_size, &mut desc_ver) };
	if uefi::is_error(status) {
		return 0;
	}
	let mut top = 0u64;
	let entries = map_size / desc_size;
	for i in 0..entries {
		let d = unsafe { &*((buf as *const u8).add(i * desc_size) as *const uefi::MemoryDescriptor) };
		let end = d.phys_start + d.page_count * PAGE_SIZE;
		if end > top {
			top = end;
		}
	}
	unsafe { ((*bs).free_pages)(buf as u64, map_size.div_ceil(PAGE_SIZE as usize)) };
	top
}

// Get the final memory map, translate it into the region array, then exit boot
// services (retrying if the map changed between the two calls). Returns the
// translated region count. After this returns no firmware service may be called.
fn finalize_and_exit(bs: *mut BootServices, image_handle: Handle, regions: *mut MemRegion) -> usize {
	// Pre-size and allocate the raw EFI map buffer once (allocation must not
	// happen inside the get/exit loop).
	let mut map_size = 0usize;
	let mut key = 0usize;
	let mut desc_size = 0usize;
	let mut desc_ver = 0u32;
	unsafe { ((*bs).get_memory_map)(&mut map_size, core::ptr::null_mut(), &mut key, &mut desc_size, &mut desc_ver) };
	let cap = map_size + desc_size * 16;
	let buf = alloc_pages(bs, cap.div_ceil(PAGE_SIZE as usize)).expect("loader: cannot allocate memory map buffer") as *mut uefi::MemoryDescriptor;

	loop {
		let mut size = cap;
		let status = unsafe { ((*bs).get_memory_map)(&mut size, buf, &mut key, &mut desc_size, &mut desc_ver) };
		if uefi::is_error(status) {
			panic!("get_memory_map failed");
		}
		let count = translate_map(buf, size, desc_size, regions);
		let status = unsafe { ((*bs).exit_boot_services)(image_handle, key) };
		if !uefi::is_error(status) {
			return count;
		}
		// The map changed; retry without allocating.
	}
}

// Translate the EFI memory map into the boot protocol's region array (sorted
// ascending by base and coalesced). Returns the region count.
fn translate_map(buf: *const uefi::MemoryDescriptor, map_size: usize, desc_size: usize, regions: *mut MemRegion) -> usize {
	let entries = map_size / desc_size;
	let mut n = 0usize;
	for i in 0..entries {
		if n >= MAX_REGIONS {
			break;
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
		return 0;
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
	w + 1
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
