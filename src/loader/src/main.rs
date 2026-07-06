// LiberSystem UEFI boot loader: a PE32+ EFI application (target
// x86_64-unknown-uefi) that replaces Limine on x86_64.
//
// The firmware enters `efi_main`. From there the loader:
//   1. opens the FAT boot volume it was loaded from,
//   2. reads the kernel ELF and the init/volume packages into memory,
//   3. loads the kernel's PT_LOAD segments (W^X honored),
//   4. grabs the Graphics Output framebuffer and the ACPI RSDP,
//   5. reserves a low page for the AP bring-up trampoline,
//   6. builds fresh page tables (HHDM + identity + kernel higher-half),
//   7. snapshots the memory map and exits boot services,
//   8. switches CR3 and jumps into the kernel with a `bootproto::BootInfo`.
//
// Diagnostics go to COM1 so they land in the same serial log the kernel and the
// test harness use.

#![no_std]
#![no_main]

mod elf;
mod paging;
mod serial;
mod uefi;

use core::ffi::c_void;

use bootproto::{BootInfo, Framebuffer, MemRegion, Module};

use paging::{HHDM_OFFSET, PAGE_2MB, PAGE_SIZE, PageTables};
use uefi::{BootServices, Handle, Status, SystemTable};

// Kernel + package filenames on the boot volume's root (mkimage lays them there).
const KERNEL_FILE: &str = "kernel";
const INIT_PKG_FILE: &str = "init.pkg";
const VOLUME_PKG_FILE: &str = "volume.pkg";

// The kernel stack the loader hands over (in pages of 4 KiB): 128 KiB.
const STACK_PAGES: usize = 32;

// Upper bound on memory-map regions we translate into the boot protocol.
const MAX_REGIONS: usize = 512;

// Panic: report on serial and hang. panic=abort (Cargo profile) means no unwind.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
	serial::write_str("loader panic: ");
	if let Some(msg) = info.message().as_str() {
		serial::write_str(msg);
	}
	serial::write_str("\n");
	loop {
		unsafe { core::arch::asm!("hlt") };
	}
}

// The firmware entry point. The x86_64-unknown-uefi target links this symbol as
// the PE entry.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: Handle, system_table: *mut SystemTable) -> Status {
	serial::init();
	serial::write_str("\nLiberSystem UEFI loader\n");

	let bs = unsafe { (*system_table).boot_services };

	// Read the kernel and packages off the boot volume.
	let root = open_boot_volume(bs, image_handle).expect("loader: cannot open boot volume");
	let kernel = read_file(bs, root, KERNEL_FILE).expect("loader: cannot read kernel");
	let init_pkg = read_file(bs, root, INIT_PKG_FILE).expect("loader: cannot read init.pkg");
	let volume_pkg = read_file(bs, root, VOLUME_PKG_FILE).expect("loader: cannot read volume.pkg");
	serial::write_str("loader: kernel + packages loaded\n");

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
		core::arch::asm!(
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

const MAX_SEGMENTS: usize = 16;

// Load the kernel ELF: for each PT_LOAD segment, allocate its pages, copy the
// file bytes, zero the tail (BSS), and record the mapping. Returns (entry, count).
fn load_kernel(bs: *mut BootServices, kernel: &[u8], out: &mut [KernelSegment; MAX_SEGMENTS]) -> (u64, usize) {
	let image = elf::Elf::parse(kernel).expect("loader: kernel is not a valid ELF64 executable");
	let mut count = 0usize;
	for i in 0..image.segment_count() {
		let Some(ph) = image.segment(i) else { continue };
		if ph.p_type != elf::PT_LOAD || ph.p_memsz == 0 {
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
		out[count] = KernelSegment { virt: align_down(ph.p_vaddr, PAGE_SIZE), phys, pages, writable: ph.p_flags & elf::PF_W != 0, executable: ph.p_flags & elf::PF_X != 0 };
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

// Query the Graphics Output Protocol for the active mode's linear framebuffer.
fn locate_framebuffer(bs: *mut BootServices) -> FbResult {
	let none = FbResult { info: unsafe { core::mem::zeroed() }, phys: 0, size: 0, present: false };
	let mut gop: *mut c_void = core::ptr::null_mut();
	let status = unsafe { ((*bs).locate_protocol)(&uefi::GRAPHICS_OUTPUT_PROTOCOL_GUID, core::ptr::null_mut(), &mut gop) };
	if uefi::is_error(status) || gop.is_null() {
		return none;
	}
	let gop = gop as *mut uefi::GraphicsOutput;
	let mode = unsafe { (*gop).mode };
	if mode.is_null() {
		return none;
	}
	let info = unsafe { (*mode).info };
	if info.is_null() {
		return none;
	}
	let (width, height, pitch_px, format, mask) = unsafe { ((*info).horizontal_resolution, (*info).vertical_resolution, (*info).pixels_per_scan_line, (*info).pixel_format, &(*info).pixel_information) };
	// Channel shifts/sizes: the common 32-bpp RGB/BGR modes are fixed layouts; a
	// bit-mask mode is decoded from the reported channel masks.
	let (rs, gs, bs_shift) = match format {
		uefi::PIXEL_RGB => (0u8, 8u8, 16u8),
		uefi::PIXEL_BGR => (16u8, 8u8, 0u8),
		uefi::PIXEL_BIT_MASK => (mask_shift(mask.red), mask_shift(mask.green), mask_shift(mask.blue)),
		_ => return none,
	};
	let (rz, gz, bz) = match format {
		uefi::PIXEL_BIT_MASK => (mask_size(mask.red), mask_size(mask.green), mask_size(mask.blue)),
		_ => (8u8, 8u8, 8u8),
	};
	let bpp = 32u32;
	let pitch = pitch_px * (bpp / 8);
	let fb_info = Framebuffer { addr: HHDM_OFFSET + unsafe { (*mode).frame_buffer_base }, width, height, pitch, bpp, red_shift: rs, red_size: rz, green_shift: gs, green_size: gz, blue_shift: bs_shift, blue_size: bz, _pad: [0; 2] };
	FbResult { info: fb_info, phys: unsafe { (*mode).frame_buffer_base }, size: unsafe { (*mode).frame_buffer_size as u64 }, present: true }
}

// Bit position of the lowest set bit of a channel mask.
fn mask_shift(mask: u32) -> u8 {
	if mask == 0 { 0 } else { mask.trailing_zeros() as u8 }
}

// Width in bits of a contiguous channel mask.
fn mask_size(mask: u32) -> u8 {
	(mask >> mask_shift(mask)).trailing_ones() as u8
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

// Open the FAT volume the loader image was loaded from and return its root
// directory.
fn open_boot_volume(bs: *mut BootServices, image_handle: Handle) -> Option<*mut uefi::FileProtocol> {
	let mut li: *mut c_void = core::ptr::null_mut();
	let status = unsafe { ((*bs).handle_protocol)(image_handle, &uefi::LOADED_IMAGE_PROTOCOL_GUID, &mut li) };
	if uefi::is_error(status) || li.is_null() {
		return None;
	}
	let device = unsafe { (*(li as *mut uefi::LoadedImage)).device_handle };

	let mut sfs: *mut c_void = core::ptr::null_mut();
	let status = unsafe { ((*bs).handle_protocol)(device, &uefi::SIMPLE_FILE_SYSTEM_PROTOCOL_GUID, &mut sfs) };
	if uefi::is_error(status) || sfs.is_null() {
		return None;
	}
	let sfs = sfs as *mut uefi::SimpleFileSystem;

	let mut root: *mut uefi::FileProtocol = core::ptr::null_mut();
	let status = unsafe { ((*sfs).open_volume)(sfs, &mut root) };
	if uefi::is_error(status) || root.is_null() {
		return None;
	}
	Some(root)
}

// Read an entire file from the boot volume into fresh LOADER_DATA pages and
// return it as a 'static slice (the memory is retained across the hand-off).
fn read_file(bs: *mut BootServices, root: *mut uefi::FileProtocol, name: &str) -> Option<&'static [u8]> {
	let mut wname = [0u16; 64];
	to_utf16(name, &mut wname);

	let mut file: *mut uefi::FileProtocol = core::ptr::null_mut();
	let status = unsafe { ((*root).open)(root, &mut file, wname.as_ptr(), uefi::FILE_MODE_READ, 0) };
	if uefi::is_error(status) || file.is_null() {
		return None;
	}

	// File size via GetInfo.
	let mut info_buf = [0u8; 512];
	let mut info_size = info_buf.len();
	let status = unsafe { ((*file).get_info)(file, &uefi::FILE_INFO_GUID, &mut info_size, info_buf.as_mut_ptr() as *mut c_void) };
	if uefi::is_error(status) {
		return None;
	}
	let file_size = unsafe { (*(info_buf.as_ptr() as *const uefi::FileInfo)).file_size } as usize;

	let pages = file_size.div_ceil(PAGE_SIZE as usize).max(1);
	let phys = alloc_pages(bs, pages)?;

	// Read the whole file (loop until the firmware stops handing back bytes).
	let mut read_total = 0usize;
	while read_total < file_size {
		let mut chunk = file_size - read_total;
		let status = unsafe { ((*file).read)(file, &mut chunk, (phys as *mut u8).add(read_total) as *mut c_void) };
		if uefi::is_error(status) || chunk == 0 {
			break;
		}
		read_total += chunk;
	}
	unsafe { ((*file).close)(file) };
	Some(unsafe { core::slice::from_raw_parts(phys as *const u8, file_size) })
}

// Copy an ASCII string into a UTF-16 buffer, NUL-terminated.
fn to_utf16(s: &str, out: &mut [u16]) {
	let mut i = 0;
	for b in s.bytes() {
		if i + 1 >= out.len() {
			break;
		}
		out[i] = b as u16;
		i += 1;
	}
	out[i] = 0;
}

// Allocate `pages` 4 KiB pages of retained LOADER_DATA and return the physical
// base (0-checked None on failure).
fn alloc_pages(bs: *mut BootServices, pages: usize) -> Option<u64> {
	let mut addr: u64 = 0;
	let status = unsafe { ((*bs).allocate_pages)(uefi::ALLOCATE_ANY_PAGES, uefi::LOADER_DATA, pages, &mut addr) };
	if uefi::is_error(status) { None } else { Some(addr) }
}

// Reserve one page below 1 MiB for the AP bring-up trampoline; 0 if none is free.
fn alloc_low_page(bs: *mut BootServices) -> u64 {
	let mut addr: u64 = 0x0010_0000;
	let status = unsafe { ((*bs).allocate_pages)(uefi::ALLOCATE_MAX_ADDRESS, uefi::LOADER_DATA, 1, &mut addr) };
	if uefi::is_error(status) { 0 } else { addr }
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

fn align_up(v: u64, align: u64) -> u64 {
	(v + align - 1) & !(align - 1)
}

fn align_down(v: u64, align: u64) -> u64 {
	v & !(align - 1)
}
