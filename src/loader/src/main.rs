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

extern crate alloc;

mod arch;
mod blockio;
mod console;
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
	// The firmware's console before the built-in UART, for as long as the firmware is there. See
	// `console`: the UART addresses are QEMU's, and nothing about them is promised on a machine
	// that is not `virt`.
	console::adopt(system_table);
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

	// WHICH LiberFS volume is this installation's. A superblock identifies LiberFS; it does not
	// identify the system that owns it, so two LiberSystem disks in one machine used to let the
	// firmware's block-handle order decide which one booted. The boot medium names its volume by
	// uuid in `etc/system-volume.uuid`, and when it does, nothing else may be the system volume.
	// A medium without the file keeps the old behaviour and says so, because a rescue stick is
	// exactly the medium that has no paired volume.
	unsafe { PAIRED_UUID = read_pairing(bs, root) };
	if unsafe { PAIRED_UUID }.is_none() {
		arch::serial::write_str("loader: the boot medium names no system volume; using the first LiberFS volume found\n");
	}

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
			// AND DROP WHAT THAT VOLUME ALREADY GAVE US. The bootstrap set is assembled during the
			// same mount, so a volume with a list but no kernel used to leave `BOOTSTRAP` set while
			// the kernel came from the boot medium - one boot made of two halves from two systems,
			// which is the outcome this code's own comment says it prevents. The halves are chosen
			// together or not at all.
			unsafe {
				if BOOTSTRAP_SOURCE == SOURCE_SYSTEM_VOLUME {
					BOOTSTRAP = None;
					BOOTSTRAP_SOURCE = "";
				}
			}
			read_boot_file(bs, root, KERNEL_FILE).expect("loader: cannot read kernel")
		}
	};
	arch::serial::write_str("loader: kernel loaded\n");

	// RESERVE THE KERNEL'S PHYSICAL SPAN NOW, before anything else allocates.
	//
	// Everything below - mounting filesystems, assembling the bootstrap set, retaining it - goes
	// through `AllocateAnyPages`, and the backends only asked for `ALLOCATE_ADDRESS` at the link
	// address afterwards. If any of those opportunistic allocations happened to land in the
	// kernel's span, the placement failed and the boot panicked - on a machine whose firmware
	// simply hands out pages in a different order.
	//
	// A failure here is not fatal AT THIS POINT, and the value says which spans were got: x86_64
	// places its segments wherever the firmware likes and never writes to `p_paddr`, so a refused
	// reservation costs it nothing, while the backends that DO write there refuse to place into a
	// span this loader does not own. What is no longer possible is proceeding on the strength of a
	// status nobody looked at.
	let reserved = reserve_kernel(bs, kernel);
	// And the source may not sit in the destination.
	let kernel = stage_kernel_clear_of_destination(bs, kernel, &reserved);

	// The BOOT medium as the last source, when no system volume answered. A machine whose volume is
	// missing or unreadable still boots, and it boots the same way as every other machine - the
	// same list, the same files, assembled by the same code. This is what replaced staging a
	// packaged `init.pkg` beside it: an archive is a second mechanism for one job, and the only one
	// of the two whose programs a user cannot see or replace one at a time.
	//
	// Read through the FAT backend rather than the firmware's file protocol on purpose. It takes
	// the same `/`-separated paths the volume list already uses, so no path is translated between
	// the two sources - and it works on firmware that declines to mount the ESP, which is exactly
	// the machine this fallback exists for.
	// A reader over the boot volume the FIRMWARE mounted, for the case the FAT scan cannot cover.
	//
	// Both are needed, and finding that out cost a red riscv64 suite. Under OVMF the ESP is
	// enumerated as a block device AND mounted, so scanning for FAT finds it. Under U-Boot it is
	// mounted but NOT exposed as a block device - the kernel read through the firmware succeeded
	// while the FAT scan saw nothing - so a fallback built only on the scan finds no bootstrap list
	// on exactly the machines that need one. The reverse case is real too: firmware that declines
	// to mount the medium is why the scan exists.
	//
	// Paths are translated here because UEFI separates with `\` while the list, the volume and the
	// FAT backend all use `/`. One translation at one boundary, rather than two path vocabularies.
	struct FirmwareVolume {
		bs: *mut BootServices,
		root: *mut uefi::FileProtocol,
	}

	impl blockio::ReadsFiles for FirmwareVolume {
		fn read(&mut self, path: &[u8]) -> Option<alloc::vec::Vec<u8>> {
			let mut name = [0u8; 128];
			if path.len() >= name.len() {
				return None;
			}
			for (i, &b) in path.iter().enumerate() {
				name[i] = if b == b'/' { b'\\' } else { b };
			}
			let name = core::str::from_utf8(&name[..path.len()]).ok()?;
			let bytes = read_file(self.bs, self.root, name)?;
			let mut owned = alloc::vec::Vec::new();
			owned.try_reserve_exact(bytes.len()).ok()?;
			owned.extend_from_slice(bytes);
			Some(owned)
		}
	}

	fn bootstrap_from_boot_medium(bs: *mut BootServices, root: Option<*mut uefi::FileProtocol>) {
		if unsafe { BOOTSTRAP }.is_some() {
			return;
		}
		if let Some(root) = root {
			let mut volume = FirmwareVolume { bs, root };
			if let Some(archive) = blockio::assemble_bootstrap(&mut volume)
				// RETURN ONLY WHEN THE RETAIN SUCCEEDED. This returned either way, so an
				// allocation failure here skipped the FAT scan that would have answered.
				&& let Some(retained) = retain(bs, &archive)
			{
				unsafe {
					BOOTSTRAP = Some(retained);
					BOOTSTRAP_SOURCE = SOURCE_BOOT_MEDIUM;
				}
				return;
			}
		}
		with_boot_medium(bs, |disk| {
			let Some(mut fs) = fat::FatFs::mount(disk) else { return false };
			match blockio::assemble_bootstrap(&mut fs) {
				Some(archive) => unsafe {
					// A retain that fails is not an answer, so the scan goes on rather than
					// stopping on a medium that gave nothing.
					match retain(bs, &archive) {
						Some(retained) => {
							BOOTSTRAP = Some(retained);
							BOOTSTRAP_SOURCE = SOURCE_BOOT_MEDIUM;
							true
						}
						None => false,
					}
				},
				// A FAT volume without a bootstrap list is not the boot medium; the next might be.
				None => false,
			}
		});
	}

	// A live medium's volume is a FILE on the boot filesystem, so the disk scan above never saw it.
	// Read it here, before the report below, and let it supply the bootstrap set the same way an
	// installed system's partition does.
	unsafe { LIVE_VOLUME = read_boot_file(bs, root, LIVE_VOLUME_FILE) };
	if let Some(image) = unsafe { LIVE_VOLUME } {
		bootstrap_from_image(bs, image);
	}

	bootstrap_from_boot_medium(bs, root);

	// Report which of the sources the bootstrap set came from, for the same reason the kernel read
	// is reported: a boot that silently used the fallback looks identical to one that did not.
	match unsafe { BOOTSTRAP } {
		Some(archive) => {
			arch::serial::write_str("loader: bootstrap set assembled from ");
			arch::serial::write_str(unsafe { BOOTSTRAP_SOURCE });
			arch::serial::write_str("\n");
			let _ = archive;
		}
		// Every source has been tried, so this names a machine that cannot boot rather than one
		// taking a slower path. The kernel is loaded and will say the same thing again.
		None => arch::serial::write_str("loader: NO bootstrap list on the system volume, the live image or the boot medium\n"),
	}
	arch::hand_off(bs, image_handle, system_table, root, kernel, &reserved);
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
// WHERE the bootstrap set came from. Three sources now answer in turn, and a boot that used the
// last one looks identical to a boot that used the first unless it says so - which is the same
// reason the kernel read is reported.
static mut BOOTSTRAP_SOURCE: &str = "";
pub(crate) const SOURCE_SYSTEM_VOLUME: &str = "the system volume";
const SOURCE_LIVE_IMAGE: &str = "the live medium's volume image";
const SOURCE_BOOT_MEDIUM: &str = "the boot medium (the system volume did not answer)";

// The uuid of the volume this boot medium is paired with, when it names one.
static mut PAIRED_UUID: Option<[u8; 16]> = None;

// Read `etc/system-volume.uuid` off the boot medium: 32 hex digits, dashes and surrounding
// whitespace ignored, so the file can be written in either of the two spellings people use.
fn read_pairing(bs: *mut BootServices, root: Option<*mut uefi::FileProtocol>) -> Option<[u8; 16]> {
	let bytes = read_boot_file(bs, root, "etc/system-volume.uuid")?;
	let mut out = [0u8; 16];
	let mut nibbles = 0usize;
	for &b in bytes {
		if b == b'-' || b.is_ascii_whitespace() {
			continue;
		}
		let v = match b {
			b'0'..=b'9' => b - b'0',
			b'a'..=b'f' => b - b'a' + 10,
			b'A'..=b'F' => b - b'A' + 10,
			_ => return None,
		};
		if nibbles >= 32 {
			return None;
		}
		out[nibbles / 2] |= if nibbles % 2 == 0 { v << 4 } else { v };
		nibbles += 1;
	}
	if nibbles == 32 { Some(out) } else { None }
}

// The live medium's system volume, read once. It is needed twice - to assemble the bootstrap set
// from and to hand to the kernel as a module - and it is the largest thing this loader reads, so
// reading it per use would cost another copy of it in firmware pages.
pub(crate) static mut LIVE_VOLUME: Option<&'static [u8]> = None;
pub(crate) const LIVE_VOLUME_FILE: &str = "system-volume.img";

// Assemble the bootstrap set from a system volume held in memory rather than on a disk.
//
// This is what lets a LIVE medium retire its `init.pkg`: the volume it carries names its own
// bootstrap programs in `etc/bootstrap.list`, exactly as an installed one does, but it is a file on
// FAT rather than a partition, so `read_from_system_volume` never sees it.
//
// Does nothing when a disk already answered - an installed system's own volume wins over an image
// that happens to be lying on the boot medium beside it.
pub(crate) fn bootstrap_from_image(bs: *mut BootServices, bytes: &'static [u8]) {
	if unsafe { BOOTSTRAP }.is_some() {
		return;
	}
	let Ok(mut fs) = liberfs::LiberFs::mount(blockio::ImageDisk { bytes }) else { return };
	if let Some(archive) = blockio::assemble_bootstrap(&mut fs) {
		unsafe {
			BOOTSTRAP = retain(bs, &archive);
			BOOTSTRAP_SOURCE = SOURCE_LIVE_IMAGE;
		}
	}
}

pub(crate) fn read_from_system_volume(bs: *mut BootServices, path: &[u8]) -> VolumeRead {
	let mut outcome = VolumeRead::NoVolume;
	blockio::each_disk(bs, |disk| {
		let Ok(mut fs) = liberfs::LiberFs::mount(disk) else { return false };
		// A LiberFS volume that is not the one this medium is paired with is somebody else's
		// system. Keep looking; on a machine with one volume there is nothing to skip.
		if let Some(want) = unsafe { PAIRED_UUID }
			&& fs.uuid() != want
		{
			return false;
		}
		// The bootstrap set, packed into the archive format the kernel already unpacks. This is
		// what retires `init.pkg` as an artifact: the same bytes reach the kernel, assembled from
		// files that exist on the volume rather than from a package built beside it.
		if let Some(archive) = blockio::assemble_bootstrap(&mut fs) {
			unsafe {
				BOOTSTRAP = retain(bs, &archive);
				BOOTSTRAP_SOURCE = SOURCE_SYSTEM_VOLUME;
			}
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

// The boot medium, chosen once.
static mut BOOT_MEDIUM: Option<blockio::FirmwareDisk> = None;

// Visit the boot medium: the one already chosen, or every device until one answers - and then that
// one is the boot medium for every later read.
//
// `read_from_fat` used to take the first FAT volume on which EACH NAME was found, one file at a
// time, so a machine without a firmware-mounted root could take its kernel from one stick and its
// bootstrap files from another. The medium is chosen once and then read from; if it does not carry
// a file, no other medium is asked for it.
fn with_boot_medium(bs: *mut BootServices, mut visit: impl FnMut(blockio::FirmwareDisk) -> bool) {
	if let Some(disk) = unsafe { BOOT_MEDIUM } {
		visit(disk);
		return;
	}
	blockio::each_disk(bs, |disk| {
		if visit(disk) {
			unsafe { BOOT_MEDIUM = Some(disk) };
			true
		} else {
			false
		}
	});
}

// Read a file from a FAT medium the firmware did not mount for us.
//
// The ESP is read through Simple File System, which the firmware provides, so this is not the
// ordinary path - it is the one for a medium the firmware declines to mount, which is what an
// installer or rescue stick looks like on firmware that only understands its own disk. Read-only,
// like everything else here.
pub(crate) fn read_from_fat(bs: *mut BootServices, path: &[u8]) -> Option<&'static [u8]> {
	let mut found: Option<&'static [u8]> = None;
	with_boot_medium(bs, |disk| {
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

// The archive name every architecture hands the kernel its bootstrap set under.
pub(crate) const INIT_PKG_FILE: &str = "init.pkg";
// The factory archive, carried by a test medium and by nothing else.
pub(crate) const VOLUME_PKG_FILE: &str = "volume.pkg";

// Describe a loaded blob for the kernel. `bias` is added to the physical address: x86_64 hands
// over higher-half addresses because it has already built the map, the device-tree architectures
// hand over physical ones because the kernel builds its own.
// The loader's OWN extent, from `EFI_LOADED_IMAGE_PROTOCOL`.
//
// The riscv64 overlap check stood one 4 KiB page around `place_and_enter` in for this. The loader
// is larger than one page, and its size is available exactly - so the approximation was a hole in
// the one check whose whole job is to notice that the loader is about to overwrite itself.
// Only the riscv64 backend's overlap check needs this today; the other two never move themselves.
#[cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
pub(crate) fn loader_image_extent(bs: *mut BootServices, image_handle: Handle) -> Option<(u64, u64)> {
	let mut li: *mut core::ffi::c_void = core::ptr::null_mut();
	let status = unsafe { ((*bs).handle_protocol)(image_handle, &uefi::LOADED_IMAGE_PROTOCOL_GUID, &mut li) };
	if status != uefi::STATUS_SUCCESS || li.is_null() {
		return None;
	}
	let image = li as *mut uefi::LoadedImage;
	let base = unsafe { (*image).image_base } as u64;
	let size = unsafe { (*image).image_size };
	if base == 0 || size == 0 {
		return None;
	}
	Some((base, size))
}

pub(crate) fn make_module(bytes: &[u8], name: &str, bias: u64) -> bootproto::Module {
	let mut module = bootproto::Module { addr: bias + bytes.as_ptr() as u64, size: bytes.len() as u64, name: [0; 32] };
	let n = name.len().min(module.name.len());
	module.name[..n].copy_from_slice(&name.as_bytes()[..n]);
	module
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
	// A name that does not fit is not this name: opening a truncated path would open a different
	// file, which is worse than not opening one.
	if !to_utf16(name, &mut wname) {
		return None;
	}

	let mut file: *mut uefi::FileProtocol = core::ptr::null_mut();
	let status = unsafe { ((*root).open)(root, &mut file, wname.as_ptr(), uefi::FILE_MODE_READ, 0) };
	if uefi::is_error(status) || file.is_null() {
		return None;
	}
	// EVERY PATH BELOW CLOSES THE FILE. Two of them used to return without doing so, and a handle
	// left open is a firmware object that can still move the memory map - the one thing that must
	// hold still between the sizing `GetMemoryMap` and `ExitBootServices`.
	let guard = OpenFile { file };
	let file = guard.file;

	// File size via GetInfo. The structure ends in a variable-length name, so the specified
	// pattern is: call, and when the firmware answers BUFFER_TOO_SMALL with a required size, call
	// again with that size. The 512-byte stack buffer holds every name this loader opens today;
	// the heap path is what makes the helper correct for a name that is longer.
	let mut info_buf = [0u8; 512];
	let mut info_size = info_buf.len();
	let mut heap_buf: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
	let mut info = info_buf.as_mut_ptr();
	let status = unsafe { ((*file).get_info)(file, &uefi::FILE_INFO_GUID, &mut info_size, info as *mut c_void) };
	let status = if status == uefi::STATUS_BUFFER_TOO_SMALL {
		if info_size > 64 * 1024 {
			return None;
		}
		heap_buf.try_reserve_exact(info_size).ok()?;
		heap_buf.resize(info_size, 0);
		info = heap_buf.as_mut_ptr();
		unsafe { ((*file).get_info)(file, &uefi::FILE_INFO_GUID, &mut info_size, info as *mut c_void) }
	} else {
		status
	};
	if uefi::is_error(status) || info_size < core::mem::size_of::<uefi::FileInfo>() {
		return None;
	}
	let file_size = unsafe { (*(info as *const uefi::FileInfo)).file_size } as usize;

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
	// A SHORT READ IS NOT A FILE. This returned `from_raw_parts(phys, file_size)` however much had
	// arrived, so a file whose `FileInfo` said one megabyte and whose second read failed became a
	// one-megabyte slice whose tail was whatever those freshly allocated pages held - handed on as
	// a kernel image, a bootstrap package or a volume. The pages go back and the answer is None.
	if read_total != file_size {
		unsafe { ((*bs).free_pages)(phys, pages) };
		return None;
	}
	Some(unsafe { core::slice::from_raw_parts(phys as *const u8, file_size) })
}

// Closes its handle however the scope ends.
struct OpenFile {
	file: *mut uefi::FileProtocol,
}

impl Drop for OpenFile {
	fn drop(&mut self) {
		unsafe { ((*self.file).close)(self.file) };
	}
}

// Encode `s` as NUL-terminated UTF-16 into `out`. False when it does not fit.
//
// This widened BYTES: `out[i] = b as u16` turned the two UTF-8 bytes of `č` into two separate
// UTF-16 units, so a name with any non-ASCII character named something else. And it `break`s when
// full, so a path longer than the buffer opened a DIFFERENT, SHORTER path rather than failing -
// with `FirmwareVolume` accepting up to 128 bytes before handing them here.
pub(crate) fn to_utf16(s: &str, out: &mut [u16]) -> bool {
	let mut i = 0;
	for unit in s.encode_utf16() {
		if i + 1 >= out.len() {
			return false;
		}
		out[i] = unit;
		i += 1;
	}
	out[i] = 0;
	true
}

// A decimal number on the boot console. There is no formatting machinery in this binary and there
// is no reason to link one for a diagnostic.
pub(crate) fn serial_write_usize(mut value: usize) {
	let mut digits = [0u8; 20];
	let mut n = 0;
	if value == 0 {
		digits[0] = b'0';
		n = 1;
	}
	while value > 0 {
		digits[n] = b'0' + (value % 10) as u8;
		value /= 10;
		n += 1;
	}
	for i in (0..n).rev() {
		arch::serial::write_byte(digits[i]);
	}
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
	// Bits per pixel, DERIVED from the mode's own bitmask rather than assumed.
	//
	// The helper below already computed this to get the pitch right for a non-32 bpp mode, and then
	// dropped it: `GopFb` had no field for it and all three backends published the constant `32`.
	// The kernel derives bytes-per-pixel from what they publish, so such a mode got a correct pitch
	// and a wrong pixel stride - the original finding surviving inside its own fix.
	pub bpp: u32,
	pub red_shift: u8,
	pub red_size: u8,
	pub green_shift: u8,
	pub green_size: u8,
	pub blue_shift: u8,
	pub blue_size: u8,
}

impl GopFb {
	pub(crate) const NONE: Self = Self { present: false, phys: 0, size: 0, width: 0, height: 0, pitch: 0, bpp: 0, red_shift: 0, red_size: 0, green_shift: 0, green_size: 0, blue_shift: 0, blue_size: 0 };
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
	// THE PIXEL SIZE COMES FROM THE MASKS in a bit-mask mode, which is what the masks are for. It
	// was the literal 32, and the pitch `pixels_per_scan_line * 4` with it - so a firmware
	// describing a 24- or 16-bit layout got a renderer writing four bytes per pixel into a
	// framebuffer with a different stride, which is a diagonal smear rather than a picture.
	//
	// And the masks are CHECKED rather than assumed contiguous and disjoint: `mask_size` counts
	// the run of ones above the lowest set bit, so a split mask reads as a short one, and two
	// channels claiming the same bits produce a colour neither of them asked for.
	let bpp = match format {
		uefi::PIXEL_BIT_MASK => {
			let all = mask.red | mask.green | mask.blue | mask.reserved;
			if all == 0 {
				return GopFb::NONE;
			}
			// Contiguous: each channel's mask must be exactly the run its shift and size describe.
			//
			// RESERVED IS ONE OF THEM. It contributes to `all` and therefore to the pixel size below,
			// and it was the one mask nothing checked - so a firmware whose reserved mask is split,
			// or overlaps a colour channel, produced an element size derived from a mask that had
			// been through no validation at all. The original defect here was a literal 32; this is
			// the last quarter of the check that replaced it.
			let reserved_shift = mask_shift(mask.reserved);
			let reserved_size = mask_size(mask.reserved);
			for (m, shift, size) in [(mask.red, rs, rz), (mask.green, gs, gz), (mask.blue, bs_shift, bz), (mask.reserved, reserved_shift, reserved_size)] {
				if m != 0 && m != (((1u64 << size) - 1) as u32) << shift {
					return GopFb::NONE;
				}
			}
			// Disjoint: no two channels may claim a bit, reserved included.
			if (mask.red & mask.green) | (mask.green & mask.blue) | (mask.red & mask.blue) != 0 {
				return GopFb::NONE;
			}
			if (mask.reserved & (mask.red | mask.green | mask.blue)) != 0 {
				return GopFb::NONE;
			}
			// The element size is the highest set bit across all four, rounded up to whole bytes.
			(32 - all.leading_zeros()).div_ceil(8) * 8
		}
		_ => 32u32,
	};
	GopFb { present: true, phys: unsafe { (*mode).frame_buffer_base }, size: unsafe { (*mode).frame_buffer_size as u64 }, width, height, pitch: pitch_px * (bpp / 8), bpp, red_shift: rs, red_size: rz, green_shift: gs, green_size: gz, blue_shift: bs_shift, blue_size: bz }
}

// Bit position of the lowest set bit of a channel mask.
fn mask_shift(mask: u32) -> u8 {
	if mask == 0 { 0 } else { mask.trailing_zeros() as u8 }
}

// Width in bits of a contiguous channel mask.
fn mask_size(mask: u32) -> u8 {
	(mask >> mask_shift(mask)).trailing_ones() as u8
}

// Claim every `PT_LOAD`'s page span at its physical link address, so a later opportunistic
// allocation cannot take it. Best effort by design - see the call site.
// The kernel's physical spans THIS LOADER actually got, and nothing else.
//
// The reservation used to be a `fn(..)` returning nothing, ending in
// `let _ = ((*bs).allocate_pages)(ALLOCATE_ADDRESS, ..)`, and the aarch64 backend then claimed the
// same spans a second time and discarded that status too - on the reasoning that the error was
// expected because the reservation already owned them. The reasoning is right about why the error
// is usually harmless and wrong about what it proves: `NOT_FOUND`/`NOT_AVAILABLE` is the same
// answer whether the owner is this loader or the firmware, a runtime service or a device, and the
// code proceeded to `write_bytes` either way. Before that change the second claim panicked. A boot
// that used to STOP could then write over whatever was there.
//
// So the reservation returns what it owns. A backend asks this value rather than re-asking the
// firmware a question whose answer it cannot interpret, and a span that is not in here is fatal at
// placement - the old panic, restored deliberately rather than by omission.
pub struct ReservedKernel {
	// (page-aligned base, page count) per PT_LOAD segment this loader reserved.
	spans: [(u64, u64); MAX_KERNEL_SPANS],
	count: usize,
	// Whether every PT_LOAD was reserved. False when a claim failed OR when there were more
	// segments than this table holds - both mean "do not trust this to answer for the whole image".
	complete: bool,
}

// Sixteen, matching the x86 backend's `MAX_SEGMENTS`. A kernel with more LOAD segments than this is
// refused at placement rather than partially reserved.
const MAX_KERNEL_SPANS: usize = 16;

impl ReservedKernel {
	const EMPTY: Self = Self { spans: [(0, 0); MAX_KERNEL_SPANS], count: 0, complete: true };

	// Did this loader reserve exactly this span?
	pub fn owns(&self, base: u64, pages: u64) -> bool {
		self.spans[..self.count].iter().any(|&(reserved_base, reserved_pages)| reserved_base == base && reserved_pages == pages)
	}

	// Does `[addr, addr+len)` fall inside any reserved span? For the source buffer, which must not
	// sit in the destination it is about to be copied out of.
	pub fn overlaps(&self, addr: u64, len: u64) -> bool {
		let end = addr.saturating_add(len);
		self.spans[..self.count].iter().any(|&(base, pages)| {
			let span_end = base.saturating_add(pages.saturating_mul(PAGE_SIZE));
			addr < span_end && base < end
		})
	}

	pub fn is_complete(&self) -> bool {
		self.complete
	}
}

fn reserve_kernel(bs: *mut uefi::BootServices, kernel: &[u8]) -> ReservedKernel {
	let mut reserved = ReservedKernel::EMPTY;
	let Some(image) = elf::Elf::parse(kernel) else {
		reserved.complete = false;
		return reserved;
	};
	for i in 0..image.segment_count() {
		let Some(ph) = image.segment(i) else { continue };
		if ph.p_type != elf::PT_LOAD || ph.p_memsz == 0 {
			continue;
		}
		let base = align_down(ph.p_paddr, PAGE_SIZE);
		// The shared parser refuses a header whose physical end wraps, so this cannot; the `expect`
		// is what says so rather than leaving the reader to find the guarantee two crates away.
		let pages = (ph.p_paddr - base).checked_add(ph.p_memsz).expect("a segment whose physical end wraps is refused by the parser").div_ceil(PAGE_SIZE);
		if reserved.count == MAX_KERNEL_SPANS {
			reserved.complete = false;
			break;
		}
		let mut addr = base;
		let status = unsafe { ((*bs).allocate_pages)(uefi::ALLOCATE_ADDRESS, uefi::LOADER_DATA, pages as usize, &mut addr) };
		if status != uefi::STATUS_SUCCESS {
			// Recorded, not fatal HERE: x86_64 places its segments wherever the firmware likes and
			// never touches `p_paddr`, so a failed reservation costs it nothing. The backend that
			// does write to `p_paddr` is the one that must refuse, and it asks this value.
			reserved.complete = false;
			continue;
		}
		reserved.spans[reserved.count] = (base, pages);
		reserved.count += 1;
	}
	reserved
}

// Move the kernel image out of its own destination, if the firmware put it there.
//
// The file is read with `AllocateAnyPages` BEFORE the reservation can be taken - the spans are in
// its ELF header, so there is nothing to reserve until it has been read - and the firmware may
// satisfy that buffer from inside the kernel's own destination. The placement then zeroes the
// segment's source before copying out of it, which riscv64's staging already solves for its own
// objects and the other two backends assumed away.
//
// A fresh allocation cannot land in the destination now, because the destination is reserved: this
// is called after `reserve_kernel`. If it somehow still overlaps, the boot stops rather than
// copying a buffer onto itself.
fn stage_kernel_clear_of_destination(bs: *mut uefi::BootServices, kernel: &'static [u8], reserved: &ReservedKernel) -> &'static [u8] {
	// THE DESTINATION SPANS, not the reserved ones.
	//
	// This asked `reserved.overlaps(...)`, and `ReservedKernel` holds only spans whose
	// `AllocateAddress` SUCCEEDED - so in the exact scenario this function documents, the source
	// buffer already owns those pages, the reservation of that span fails, the span is never
	// recorded, `overlaps` answers false, and the staging is skipped. The recovery path was
	// unreachable from its own premise.
	//
	// The destinations come from the ELF header and are known whether or not any allocation
	// succeeded, which is the question being asked: not "what did this loader get" but "where is
	// the kernel going".
	if !destination_overlaps(kernel) {
		return kernel;
	}
	let _ = reserved;
	arch::serial::write_str(
		"loader: the kernel file was read into its own destination; staging it clear
",
	);
	let pages = (kernel.len() as u64).div_ceil(PAGE_SIZE);
	let staged = alloc_pages(bs, pages as usize).expect("loader: cannot stage the kernel clear of its destination");
	unsafe {
		let moved = core::slice::from_raw_parts(staged as *const u8, kernel.len());
		core::ptr::copy_nonoverlapping(kernel.as_ptr(), staged as *mut u8, kernel.len());
		assert!(!destination_overlaps(moved), "loader: the staged kernel still overlaps its destination");
		moved
	}
}

// Does this buffer sit inside any PT_LOAD's physical span?
//
// Reads the spans out of the ELF header, which is what makes it answerable before - and regardless
// of - any allocation. `ReservedKernel` answers a different question, and asking it this one is why
// the recovery above could not trigger in the case it was written for.
fn destination_overlaps(buffer: &[u8]) -> bool {
	let Some(image) = elf::Elf::parse(buffer) else { return false };
	let start = buffer.as_ptr() as u64;
	let finish = start.saturating_add(buffer.len() as u64);
	for i in 0..image.segment_count() {
		let Some(ph) = image.segment(i) else { continue };
		if ph.p_type != elf::PT_LOAD || ph.p_memsz == 0 {
			continue;
		}
		let base = align_down(ph.p_paddr, PAGE_SIZE);
		let Some(end) = ph.p_paddr.checked_add(ph.p_memsz) else { return true };
		let end = align_up_page(end);
		if start < end && base < finish {
			return true;
		}
	}
	false
}

fn align_up_page(v: u64) -> u64 {
	v.checked_add(PAGE_SIZE - 1).map_or(u64::MAX, |x| x & !(PAGE_SIZE - 1))
}
