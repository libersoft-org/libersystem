// LiberSystem UEFI boot loader: a UEFI application (PE32+ on x86_64, PE/AArch64 on
// aarch64) that replaces the third-party bootloader.
//
// The firmware enters `efi_main`. The architecture-neutral driver here:
//   1. opens the FAT boot volume it was loaded from,
//   2. reads the kernel ELF into retained memory,
// then hands off to the architecture backend (`arch::hand_off`), which places the
// kernel in memory and jumps into it the way that architecture's kernel expects:
//   * x86_64 builds fresh page tables (HHDM + identity + kernel higher-half) and a
//     `bootproto::BootInfo` (memory map, framebuffer, ACPI RSDP, packages), snapshots
//     the memory map, exits boot services, switches CR3 and jumps to the kernel entry;
//   * aarch64 loads each segment at its physical link address, finds the firmware DTB,
//     exits boot services, turns the MMU off and branches to the kernel's PIC boot
//     stub with the DTB in x0 - the same entry state QEMU's `-kernel` load produces,
//     so the kernel's own boot path sets up the MMU + BootInfo from there.
//
// The UEFI bindings (uefi.rs), the ELF reader (elf.rs) and the boot-volume file I/O
// here are architecture-neutral and shared; only `arch` differs per architecture.
//
// Diagnostics go to the platform's debug UART so they land in the same serial log the
// kernel and the test harness use.

#![no_std]
#![no_main]

mod arch;
mod elf;
mod uefi;

use core::ffi::c_void;

use uefi::{BootServices, Handle, Status, SystemTable};

// The kernel filename on the boot volume's root (mkimage lays it there). Both
// architectures read the kernel; the init/volume package filenames are x86-only (the
// aarch64 kernel embeds its packages) and live in the x86 backend.
pub(crate) const KERNEL_FILE: &str = "kernel";

// The page size the loader allocates and aligns in (4 KiB, both architectures).
pub(crate) const PAGE_SIZE: u64 = 4096;

// Panic: report on serial and hang. panic=abort (Cargo profile) means no unwind.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
	arch::serial::write_str("loader panic");
	if let Some(loc) = info.location() {
		arch::serial::write_str(" at ");
		arch::serial::write_str(loc.file());
		arch::serial::write_str(":");
		let mut line = loc.line();
		// Print the line number (small, base 10) without alloc.
		let mut digits = [0u8; 10];
		let mut n = 0;
		if line == 0 {
			digits[0] = b'0';
			n = 1;
		}
		while line > 0 {
			digits[n] = b'0' + (line % 10) as u8;
			line /= 10;
			n += 1;
		}
		for i in (0..n).rev() {
			arch::serial::write_byte(digits[i]);
		}
	}
	arch::serial::write_str(": ");
	if let Some(msg) = info.message().as_str() {
		arch::serial::write_str(msg);
	}
	arch::serial::write_str("\n");
	arch::halt()
}

// The firmware entry point. The `*-unknown-uefi` targets link this symbol as the PE
// entry. Shared across architectures: open the boot volume, read the kernel, then let
// the architecture backend place it and jump.
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image_handle: Handle, system_table: *mut SystemTable) -> Status {
	arch::serial::init();
	arch::serial::write_str("\nLiberSystem UEFI loader\n");

	let bs = unsafe { (*system_table).boot_services };
	let root = open_boot_volume(bs, image_handle).expect("loader: cannot open boot volume");
	let kernel = read_file(bs, root, KERNEL_FILE).expect("loader: cannot read kernel");
	arch::serial::write_str("loader: kernel loaded\n");

	arch::hand_off(bs, image_handle, system_table, root, kernel);
}

// Open the FAT volume the loader image was loaded from and return its root directory.
pub(crate) fn open_boot_volume(bs: *mut BootServices, image_handle: Handle) -> Option<*mut uefi::FileProtocol> {
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

// Read an entire file from the boot volume into fresh LOADER_DATA pages and return it
// as a 'static slice (the memory is retained across the hand-off).
pub(crate) fn read_file(bs: *mut BootServices, root: *mut uefi::FileProtocol, name: &str) -> Option<&'static [u8]> {
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
pub(crate) fn to_utf16(s: &str, out: &mut [u16]) {
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

// Allocate `pages` 4 KiB pages of retained LOADER_DATA and return the physical base
// (0-checked None on failure).
pub(crate) fn alloc_pages(bs: *mut BootServices, pages: usize) -> Option<u64> {
	let mut addr: u64 = 0;
	let status = unsafe { ((*bs).allocate_pages)(uefi::ALLOCATE_ANY_PAGES, uefi::LOADER_DATA, pages, &mut addr) };
	if uefi::is_error(status) { None } else { Some(addr) }
}

pub(crate) fn align_down(v: u64, align: u64) -> u64 {
	v & !(align - 1)
}
