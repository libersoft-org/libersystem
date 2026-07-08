// riscv64 loader backend: place the kernel and enter its own boot stub.
//
// Like aarch64 (and unlike x86, where the loader builds the kernel's page tables and
// hands it a BootInfo), the riscv64 kernel carries a position-independent boot stub
// that builds its Sv39 tables + higher half itself and reads its device inventory
// from the device tree - exactly the entry state QEMU's `-kernel` load over OpenSBI
// produces (S-mode, paging off, a0 = boot hartid, a1 = DTB). So this backend mirrors
// that state: it loads each PT_LOAD segment at its physical (link) address, finds the
// firmware's flattened device tree and the boot hart id, exits boot services, turns
// paging off (SATP = 0), and jumps to the kernel entry (`_start`) with the hart id in
// a0 and the DTB pointer in a1. No page tables or BootInfo are built here.
//
// The kernel is linked higher-half with each segment's load address (LMA) equal to
// its virtual address minus KERNEL_VA_OFFSET, so loading by physical address places
// it exactly where the boot stub's low-identity megapage (physical 0x8000_0000)
// expects to keep executing after it turns paging on.
//
// This backend is compiled for `riscv64gc-unknown-none-elf` (there is no built-in
// riscv64 UEFI target, and rustc's object backend cannot emit a riscv64 PE/COFF).
// A hand-written PE/COFF header (head.rs) is prepended by the linker script so the
// flat image objcopy produces is a valid EFI application U-Boot's boot manager loads.

pub mod head;
pub mod serial;

use crate::uefi::{self, BootServices, Guid, Handle, SystemTable};
use crate::{align_down, PAGE_SIZE};

// RISCV_EFI_BOOT_PROTOCOL: U-Boot (and EDK2) expose the id of the hart that entered
// the firmware through this protocol, so the loader can hand it to the kernel in a0 -
// the same value OpenSBI's `-kernel` boot passes.
const RISCV_EFI_BOOT_PROTOCOL_GUID: Guid = Guid::new(0xccd15fec, 0x6f73, 0x4eec, [0x83, 0x95, 0x3e, 0x69, 0xe4, 0xb9, 0x40, 0xbf]);

#[repr(C)]
struct RiscvEfiBootProtocol {
	revision: u64,
	get_boot_hartid: unsafe extern "efiapi" fn(*mut RiscvEfiBootProtocol, *mut usize) -> uefi::Status,
}

// Halt the hart (panic path): wait for an interrupt forever. panic=abort, no unwind.
pub fn halt() -> ! {
	loop {
		unsafe { core::arch::asm!("wfi", options(nomem, nostack, preserves_flags)) };
	}
}

// Place the kernel at its physical link addresses, find the device tree and boot hart
// id, exit boot services, turn paging off and enter the kernel's boot stub with the
// hart id in a0 and the DTB in a1.
pub fn hand_off(bs: *mut BootServices, image_handle: Handle, system_table: *mut SystemTable, _root: *mut uefi::FileProtocol, kernel: &[u8]) -> ! {
	let entry = load_kernel(bs, kernel);
	serial::write_str("loader: kernel ELF loaded at its physical link addresses\n");

	// The boot hart id (a0) and the flattened device tree (a1) - the same pair OpenSBI
	// hands the kernel on a `-kernel` boot. The kernel scans memory for the DTB if the
	// firmware exposes none, and treats hart 0 as the boot hart if the protocol is
	// absent.
	let hartid = boot_hartid(bs);
	let dtb = find_dtb(system_table);
	serial::write_str(if dtb != 0 { "loader: device tree found\n" } else { "loader: no device-tree table (kernel will scan)\n" });

	// ExitBootServices is the last firmware call; after it no service may be used.
	exit_boot_services(bs, image_handle);

	// Mirror the OpenSBI `-kernel` entry state and enter the kernel's boot stub: turn
	// paging off (the stub builds Sv39 from scratch), fence, then jump to the entry
	// with the hart id in a0 and the DTB in a1. The loader ran under the firmware's
	// identity map, so with paging off it keeps executing at the same (physical)
	// addresses through the jump.
	unsafe {
		core::arch::asm!(
			"csrw satp, zero",
			"sfence.vma",
			"jr {entry}",
			entry = in(reg) entry,
			in("a0") hartid,
			in("a1") dtb,
			options(noreturn),
		);
	}
}

// Load each PT_LOAD segment at its physical (link) address - the placement OpenSBI's
// `-kernel` produces, which the kernel's higher-half boot stub relies on (its low
// identity megapage maps physical 0x8000_0000). Returns the entry point (physical).
fn load_kernel(bs: *mut BootServices, kernel: &[u8]) -> u64 {
	let image = crate::elf::Elf::parse(kernel).expect("loader: kernel is not a valid riscv64 ELF64 executable");
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

// Ask the RISCV_EFI_BOOT_PROTOCOL for the boot hart id; fall back to 0 if the firmware
// does not expose it (the kernel then treats hart 0 as the boot hart).
fn boot_hartid(bs: *mut BootServices) -> u64 {
	let mut iface: *mut core::ffi::c_void = core::ptr::null_mut();
	let status = unsafe { ((*bs).locate_protocol)(&RISCV_EFI_BOOT_PROTOCOL_GUID, core::ptr::null_mut(), &mut iface) };
	if uefi::is_error(status) || iface.is_null() {
		return 0;
	}
	let proto = iface as *mut RiscvEfiBootProtocol;
	let mut hartid: usize = 0;
	let status = unsafe { ((*proto).get_boot_hartid)(proto, &mut hartid) };
	if uefi::is_error(status) {
		0
	} else {
		hartid as u64
	}
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
// be called. riscv64 needs no translated map - the kernel reads RAM from the DTB.
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
