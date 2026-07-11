// aarch64 loader backend: place the kernel and enter its own boot stub.
//
// Unlike x86 (where the loader builds the kernel's page tables and hands it a
// BootInfo), the aarch64 kernel carries a position-independent boot stub that sets
// up the MMU + higher half itself and builds its BootInfo from the device tree -
// exactly the entry state QEMU's `-kernel` load produces. So this backend mirrors
// that state: it loads each PT_LOAD segment at its physical (link) address, finds the
// firmware's flattened device tree, exits boot services, turns the MMU off, and
// branches to the kernel entry (`_start`) with the DTB pointer in x0. No page tables
// or BootInfo are built here - the kernel's proven boot path does the rest.
//
// The kernel is linked higher-half with each segment's load address (LMA) equal to
// its virtual address minus KERNEL_VA_OFFSET, so loading by physical address places
// it exactly where the boot stub's TTBR1 direct map (high VA -> VA & !KOFF) expects.

pub mod serial;

use bootproto::{BootInfo, Framebuffer};

use crate::uefi::{self, BootServices, Handle, SystemTable};
use crate::{PAGE_SIZE, align_down};

// Halt the core (panic path): wait for an event forever. panic=abort, no unwind.
pub fn halt() -> ! {
	loop {
		unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
	}
}

// Place the kernel at its physical link addresses, find the device tree, exit boot
// services, and enter the kernel's boot stub with the MMU off and the DTB in x0.
pub fn hand_off(bs: *mut BootServices, image_handle: Handle, system_table: *mut SystemTable, _root: *mut uefi::FileProtocol, kernel: &[u8]) -> ! {
	let entry = load_kernel(bs, kernel);
	serial::write_str("loader: kernel ELF loaded at its physical link addresses\n");

	// The flattened device tree (the kernel's device + memory inventory, replacing
	// x86's ACPI). 0 if the firmware exposes none - the kernel then scans memory.
	let dtb = find_dtb(system_table);
	serial::write_str(if dtb != 0 { "loader: device tree found\n" } else { "loader: no device-tree table (kernel will scan)\n" });

	// Build a BootInfo carrying the DTB pointer and the GOP framebuffer, so the kernel
	// draws its earliest boot log to the display pixel-by-pixel (QEMU virt has no VGA;
	// the `-kernel` path programs ramfb itself instead). The kernel enters through its
	// own boot stub, which forwards x0 to the kernel entry; there it tells a BootInfo
	// from a raw DTB pointer (the `-kernel` entry state) by the BootInfo magic.
	let boot_info = build_boot_info(bs, dtb);

	// ExitBootServices is the last firmware call; after it no service may be used.
	exit_boot_services(bs, image_handle);

	// Mirror the QEMU `-kernel` entry state and enter the kernel's boot stub: turn
	// the MMU + caches off (the stub sets translation up from scratch), synchronise,
	// then branch to the entry with the BootInfo pointer in x0. The loader ran under
	// the firmware's identity map, so with translation off it keeps executing at the
	// same (physical) addresses through the branch.
	unsafe {
		core::arch::asm!(
			"dsb sy",
			"mrs x9, sctlr_el1",
			"bic x9, x9, #0x1",    // M = 0 (MMU off)
			"bic x9, x9, #0x4",    // C = 0 (data cache off)
			"bic x9, x9, #0x1000", // I = 0 (instruction cache off)
			"msr sctlr_el1, x9",
			"isb",
			"tlbi vmalle1",
			"ic iallu",
			"dsb sy",
			"isb",
			"br x1",
			in("x0") boot_info, // the kernel boot stub forwards x0 to the entry
			in("x1") entry,     // scratch x9 is clobbered freely (noreturn - never comes back)
			options(noreturn),
		);
	}
}

// Allocate and fill a `bootproto::BootInfo` (in retained LOADER_DATA) carrying the DTB
// pointer and the GOP framebuffer, returning its physical address. The kernel reads it
// through its own direct map, so `framebuffer.addr` is the PHYSICAL base (this backend
// builds no page tables). Only the fields the device-tree kernel path reads are set;
// the memmap / modules / rsdp / trampoline are x86-only.
fn build_boot_info(bs: *mut BootServices, dtb: u64) -> u64 {
	let fb = crate::locate_framebuffer(bs);
	serial::write_str(if fb.present { "loader: GOP framebuffer found\n" } else { "loader: no GOP framebuffer (serial-only boot log)\n" });
	let phys = crate::alloc_pages(bs, 1).expect("loader: cannot allocate BootInfo");
	let framebuffer = if fb.present { Framebuffer { addr: fb.phys, width: fb.width, height: fb.height, pitch: fb.pitch, bpp: 32, red_shift: fb.red_shift, red_size: fb.red_size, green_shift: fb.green_shift, green_size: fb.green_size, blue_shift: fb.blue_shift, blue_size: fb.blue_size, _pad: [0; 2] } } else { unsafe { core::mem::zeroed() } };
	unsafe {
		*(phys as *mut BootInfo) = BootInfo { magic: bootproto::MAGIC, version: bootproto::VERSION, _pad0: 0, hhdm_offset: 0, memmap: 0, memmap_len: 0, modules: 0, modules_len: 0, framebuffer, fb_present: fb.present as u32, _pad1: 0, rsdp: 0, smp_trampoline: 0, dtb };
	}
	phys
}

// Load each PT_LOAD segment at its physical (link) address - the placement QEMU's
// `-kernel` produces, which the kernel's higher-half boot stub relies on (its TTBR1
// maps a high VA to its link physical address). Returns the entry point (physical).
fn load_kernel(bs: *mut BootServices, kernel: &[u8]) -> u64 {
	let image = crate::elf::Elf::parse(kernel).expect("loader: kernel is not a valid aarch64 ELF64 executable");
	for i in 0..image.segment_count() {
		let Some(ph) = image.segment(i) else { continue };
		if ph.p_type != crate::elf::PT_LOAD || ph.p_memsz == 0 {
			continue;
		}
		// Reserve exactly the segment's physical span (page-aligned) so the firmware
		// hands it back at its link address, then copy the file bytes and zero the
		// tail (BSS).
		let base = align_down(ph.p_paddr, PAGE_SIZE);
		let pages = (ph.p_paddr - base + ph.p_memsz).div_ceil(PAGE_SIZE);
		let mut addr = base;
		let status = unsafe { ((*bs).allocate_pages)(uefi::ALLOCATE_ADDRESS, uefi::LOADER_DATA, pages as usize, &mut addr) };
		if uefi::is_error(status) {
			panic!("cannot place a kernel segment at its physical link address");
		}
		unsafe {
			core::ptr::write_bytes(ph.p_paddr as *mut u8, 0, ph.p_memsz as usize);
			if let Some(data) = image.segment_data(&ph) {
				core::ptr::copy_nonoverlapping(data.as_ptr(), ph.p_paddr as *mut u8, data.len());
			}
		}
	}
	image.entry
}

// Scan the firmware configuration table for the flattened device tree, returning its
// physical address (0 if none - the kernel then scans memory for the DTB magic).
fn find_dtb(system_table: *mut SystemTable) -> u64 {
	let count = unsafe { (*system_table).number_of_table_entries };
	let entries = unsafe { (*system_table).configuration_table };
	for i in 0..count {
		let e = unsafe { &*entries.add(i) };
		if e.vendor_guid == uefi::DTB_TABLE_GUID {
			return e.vendor_table as u64;
		}
	}
	0
}

// Get the current memory map (only for its key) and exit boot services, retrying if
// the map changed between the two calls. After this returns no firmware service may
// be called. aarch64 needs no translated map - the kernel reads RAM from the DTB.
fn exit_boot_services(bs: *mut BootServices, image_handle: Handle) {
	let mut map_size = 0usize;
	let mut key = 0usize;
	let mut desc_size = 0usize;
	let mut desc_ver = 0u32;
	unsafe { ((*bs).get_memory_map)(&mut map_size, core::ptr::null_mut(), &mut key, &mut desc_size, &mut desc_ver) };
	let cap = map_size + desc_size * 16;
	let buf = crate::alloc_pages(bs, cap.div_ceil(PAGE_SIZE as usize)).expect("loader: cannot allocate memory map buffer") as *mut uefi::MemoryDescriptor;
	loop {
		let mut size = cap;
		let status = unsafe { ((*bs).get_memory_map)(&mut size, buf, &mut key, &mut desc_size, &mut desc_ver) };
		if uefi::is_error(status) {
			panic!("get_memory_map failed");
		}
		let status = unsafe { ((*bs).exit_boot_services)(image_handle, key) };
		if !uefi::is_error(status) {
			return;
		}
		// The map changed; retry without allocating.
	}
}
