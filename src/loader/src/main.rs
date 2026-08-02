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
mod blockio;
mod elf;
mod heap;
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
	// The heap has to exist before the filesystem crates are used, and cannot outlive boot
	// services.
	heap::init(bs);
	// The firmware normally mounts the boot volume for us. When it does not - a medium its Simple
	// File System driver declines - the FAT backend below reads it through the block protocol
	// instead, which is the whole reason that crate is linked.
	let root = open_boot_volume(bs, image_handle);

	// Prefer the system volume: a program that runs should have a file on the volume the user can
	// see, which is the whole point of this milestone. The ESP copy is the fallback, so a machine
	// whose system volume is missing or unreadable still boots far enough to say so.
	let kernel = match read_from_system_volume(bs, KERNEL_FILE.as_bytes()) {
		VolumeRead::Read(bytes) => {
			arch::serial::write_str("loader: kernel read from the system volume\n");
			bytes
		}
		// The two failures are reported apart on purpose. "No LiberFS volume" points at the
		// disk, its cabling or the wrong machine; "volume found, file missing" points at the
		// build that staged it. A single message covering both would send whoever reads this
		// log looking in the wrong place, and this milestone's own risk note says the failure
		// message matters more than usual here.
		VolumeRead::NoVolume => {
			arch::serial::write_str("loader: no LiberFS volume on any block device; using the boot volume\n");
			read_boot_file(bs, root, KERNEL_FILE).expect("loader: cannot read kernel")
		}
		VolumeRead::NotOnVolume => {
			arch::serial::write_str("loader: system volume found but it has no kernel; using the boot volume\n");
			read_boot_file(bs, root, KERNEL_FILE).expect("loader: cannot read kernel")
		}
	};
	arch::serial::write_str("loader: kernel loaded\n");

	// Report which of the two sources the bootstrap set came from, for the same reason the kernel
	// read is reported: a boot that silently used the fallback looks identical to one that did not.
	match unsafe { BOOTSTRAP } {
		Some(archive) => {
			arch::serial::write_str("loader: bootstrap set assembled from the system volume\n");
			let _ = archive;
		}
		None => arch::serial::write_str("loader: no bootstrap list on the volume; using the boot volume's archive\n"),
	}
	arch::hand_off(bs, image_handle, system_table, root, kernel);
}

// Find the LiberFS system volume among the firmware's block devices and read one file from it.
//
// Discovery is by SUPERBLOCK, not by device order: `LiberFs::mount` returns None for anything that
// is not a LiberFS volume, so trying each device in turn identifies the right one even on a
// machine with several disks. Trusting the order would boot the wrong system on exactly the
// machines where getting it wrong matters most.
//
// The bytes are copied out of the heap into retained firmware pages, because the heap does not
// survive `ExitBootServices` and the kernel image must.
pub(crate) enum VolumeRead {
	Read(&'static [u8]),
	// No block device carried a LiberFS superblock.
	NoVolume,
	// A LiberFS volume was found, but not this file - or its bytes could not be retained.
	NotOnVolume,
}

// The bootstrap archive assembled from the volume, if there was one. Read in the SAME mount as
// the kernel: mounting twice would walk every block device twice and, worse, could pick a
// different volume for the two halves of one boot on a machine with more than one.
pub(crate) static mut BOOTSTRAP: Option<&'static [u8]> = None;

pub(crate) fn read_from_system_volume(bs: *mut BootServices, path: &[u8]) -> VolumeRead {
	let mut outcome = VolumeRead::NoVolume;
	blockio::each_disk(bs, |disk| {
		let Some(mut fs) = liberfs::LiberFs::mount(disk) else { return false };
		// The bootstrap set, packed into the archive format the kernel already unpacks. This is
		// what retires `init.pkg` as an artifact: the same bytes reach the kernel, assembled from
		// files that exist on the volume rather than from a package built beside it.
		if let Some(archive) = blockio::assemble_bootstrap(&mut fs) {
			unsafe { BOOTSTRAP = retain(bs, &archive) };
		}
		// A LiberFS volume without this file is still the system volume; a second one is not
		// going to be more right. Stop rather than read the same name off another disk, which
		// is how a machine boots half of one system and half of another.
		outcome = match fs.read_file(path) {
			Ok(bytes) => match retain(bs, &bytes) {
				Some(retained) => VolumeRead::Read(retained),
				None => VolumeRead::NotOnVolume,
			},
			Err(_) => VolumeRead::NotOnVolume,
		};
		true
	});
	outcome
}

// Read a file from the boot medium: through the firmware when it mounted the volume, and through
// the FAT backend over the block protocol when it did not.
pub(crate) fn read_boot_file(bs: *mut BootServices, root: Option<*mut uefi::FileProtocol>, name: &str) -> Option<&'static [u8]> {
	if let Some(root) = root
		&& let Some(bytes) = read_file(bs, root, name)
	{
		return Some(bytes);
	}
	arch::serial::write_str("loader: the firmware did not mount the boot volume; reading it as FAT\n");
	read_from_fat(bs, name.as_bytes())
}

// Read a file from a FAT medium the firmware did not mount for us.
//
// The ESP is read through Simple File System, which the firmware provides, so this is not the
// ordinary path - it is the one for a medium the firmware declines to mount, which is what an
// installer or rescue stick looks like on firmware that only understands its own disk. Read-only,
// like everything else here.
pub(crate) fn read_from_fat(bs: *mut BootServices, path: &[u8]) -> Option<&'static [u8]> {
	let mut found: Option<&'static [u8]> = None;
	blockio::each_disk(bs, |disk| {
		let Some(mut fs) = fat::FatFs::mount(disk) else { return false };
		match fs.read_file(path) {
			Ok(bytes) => {
				found = retain(bs, &bytes);
				true
			}
			// A FAT volume without this file is not the one we want; the next medium might be.
			Err(_) => false,
		}
	});
	found
}

// Copy `bytes` into fresh LOADER_DATA pages, which the firmware retains across the hand-off.
fn retain(bs: *mut BootServices, bytes: &[u8]) -> Option<&'static [u8]> {
	let pages = bytes.len().div_ceil(PAGE_SIZE as usize).max(1);
	let phys = alloc_pages(bs, pages)?;
	unsafe {
		core::ptr::copy_nonoverlapping(bytes.as_ptr(), phys as *mut u8, bytes.len());
		Some(core::slice::from_raw_parts(phys as *const u8, bytes.len()))
	}
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

// The active linear framebuffer the firmware's Graphics Output Protocol reports: its
// physical base + byte size and the pixel geometry/format. Architecture-neutral - each
// backend turns it into a `bootproto::Framebuffer` (x86 stores an HHDM virtual `addr`
// it mapped; the device-tree arches store the physical base and let the kernel map it).
pub(crate) struct GopFb {
	pub present: bool,
	pub phys: u64,
	// Read only by the x86 backend (to map the framebuffer into the HHDM); the
	// device-tree arches pass the physical base straight through and never map it.
	#[allow(dead_code)]
	pub size: u64,
	pub width: u32,
	pub height: u32,
	pub pitch: u32, // bytes per row
	pub red_shift: u8,
	pub red_size: u8,
	pub green_shift: u8,
	pub green_size: u8,
	pub blue_shift: u8,
	pub blue_size: u8,
}

impl GopFb {
	pub(crate) const NONE: Self = Self { present: false, phys: 0, size: 0, width: 0, height: 0, pitch: 0, red_shift: 0, red_size: 0, green_shift: 0, green_size: 0, blue_shift: 0, blue_size: 0 };
}

// Query the Graphics Output Protocol for the active mode's linear framebuffer. Returns
// `GopFb::NONE` on a headless boot (no GOP / no active mode / an unsupported format).
pub(crate) fn locate_framebuffer(bs: *mut BootServices) -> GopFb {
	let mut gop: *mut c_void = core::ptr::null_mut();
	let status = unsafe { ((*bs).locate_protocol)(&uefi::GRAPHICS_OUTPUT_PROTOCOL_GUID, core::ptr::null_mut(), &mut gop) };
	if uefi::is_error(status) || gop.is_null() {
		return GopFb::NONE;
	}
	let gop = gop as *mut uefi::GraphicsOutput;
	let mode = unsafe { (*gop).mode };
	if mode.is_null() {
		return GopFb::NONE;
	}
	let info = unsafe { (*mode).info };
	if info.is_null() {
		return GopFb::NONE;
	}
	let (width, height, pitch_px, format, mask) = unsafe { ((*info).horizontal_resolution, (*info).vertical_resolution, (*info).pixels_per_scan_line, (*info).pixel_format, &(*info).pixel_information) };
	// Channel shifts/sizes: the common 32-bpp RGB/BGR modes are fixed layouts; a
	// bit-mask mode is decoded from the reported channel masks.
	let (rs, gs, bs_shift) = match format {
		uefi::PIXEL_RGB => (0u8, 8u8, 16u8),
		uefi::PIXEL_BGR => (16u8, 8u8, 0u8),
		uefi::PIXEL_BIT_MASK => (mask_shift(mask.red), mask_shift(mask.green), mask_shift(mask.blue)),
		_ => return GopFb::NONE,
	};
	let (rz, gz, bz) = match format {
		uefi::PIXEL_BIT_MASK => (mask_size(mask.red), mask_size(mask.green), mask_size(mask.blue)),
		_ => (8u8, 8u8, 8u8),
	};
	let bpp = 32u32;
	GopFb { present: true, phys: unsafe { (*mode).frame_buffer_base }, size: unsafe { (*mode).frame_buffer_size as u64 }, width, height, pitch: pitch_px * (bpp / 8), red_shift: rs, red_size: rz, green_shift: gs, green_size: gz, blue_shift: bs_shift, blue_size: bz }
}

// Bit position of the lowest set bit of a channel mask.
fn mask_shift(mask: u32) -> u8 {
	if mask == 0 { 0 } else { mask.trailing_zeros() as u8 }
}

// Width in bits of a contiguous channel mask.
fn mask_size(mask: u32) -> u8 {
	(mask >> mask_shift(mask)).trailing_ones() as u8
}
