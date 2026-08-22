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

use core::ffi::c_void;

use uefi::{BootServices, Handle, Status, SystemTable};
// The firmware-facing work the loader drives - moved into the `uefi` crate, where a mock firmware
// can exercise it. The loader is a UEFI binary and cannot run a test; the algorithms it used to
// hold could not either, which is why the hostile-firmware cases in this milestone were argued in
// code and never run.
use uefi::file::read_file;
pub(crate) use uefi::gop::locate_framebuffer;
use uefi::memory::{alloc_pages, alloc_scratch_pages};

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
	// EVERY RETAINED ALLOCATION FROM HERE IS HANDED TO THE KERNEL, so it has to land where the
	// kernel can address it. aarch64 and riscv64 enter Rust on a boot stub with a FIXED early direct
	// map - 4 GB and 8 GB - and firmware placing a handoff allocation above that gives the kernel a
	// pointer it cannot read at the one moment it must. x86 declares no ceiling: it builds its own
	// direct map over all RAM first. Set before anything is allocated, which is why it is here.
	#[cfg(target_arch = "aarch64")]
	uefi::memory::set_alloc_ceiling(4 * 1024 * 1024 * 1024 - 1);
	#[cfg(target_arch = "riscv64")]
	uefi::memory::set_alloc_ceiling(8 * 1024 * 1024 * 1024 - 1);
	console::adopt(system_table);
	// And ask the machine where its console is, while the configuration table is still readable.
	// What comes out of this is what the loader prints to AFTER `ExitBootServices` - or nothing,
	// on a machine that names no console this loader can drive.
	#[cfg(not(target_arch = "x86_64"))]
	console::discover(system_table);
	arch::serial::init();
	arch::serial::write_str("\nLiberSystem UEFI loader\n");
	// AFTER the banner, because it is a line about this loader rather than a line from the
	// firmware, and a diagnostic printed before the program names itself reads as stray output.
	#[cfg(not(target_arch = "x86_64"))]
	console::report();

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
	// NOT ON x86_64, which never writes to `p_paddr` at all: it places every segment in frames the
	// firmware chose and ignores this value entirely (`hand_off` takes it as `_reserved`). The pass
	// still ran there, and every reservation it managed to take was a kernel-sized block of
	// `LOADER_DATA` nobody would ever use - which the kernel's map calls `MEM_BOOTLOADER` and never
	// seeds, so it is lost for the life of the system. Today's artifact has high `p_paddr` values so
	// the fixed requests fail and the leak is zero; a valid low-physical layout would strand the
	// whole image.
	#[cfg(not(target_arch = "x86_64"))]
	let reserved = reserve_kernel(bs, kernel);
	// And the source may not sit in the destination.
	let kernel = stage_kernel_clear_of_destination(bs, kernel);

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
			// AND THE LIMIT THE FIRMWARE READER ACTUALLY HAS. This accepted any path under 128 bytes
			// while `read_file` encodes into `MAX_PATH_UNITS` UTF-16 units, so the same bootstrap
			// list could work through Block I/O and fail through Simple File System - on a machine
			// with both, with the file simply absent and nothing said. Counted in UTF-16 units, not
			// bytes, because a non-ASCII path makes the two differ and it is units the firmware
			// counts. One unit is reserved for the terminator, as in the encoder.
			let units = core::str::from_utf8(path).ok()?.chars().map(char::len_utf16).sum::<usize>();
			if units + 1 > uefi::file::MAX_PATH_UNITS {
				return None;
			}
			for (i, &b) in path.iter().enumerate() {
				name[i] = if b == b'/' { b'\\' } else { b };
			}
			let name = core::str::from_utf8(&name[..path.len()]).ok()?;
			let bytes = unsafe { read_file(self.bs, self.root, name) }?;
			let mut owned = alloc::vec::Vec::new();
			let copied = owned.try_reserve_exact(bytes.len()).is_ok();
			if copied {
				owned.extend_from_slice(bytes);
			}
			// THE PAGES GO BACK. `read_file` hands out an unowned slice in fresh LOADER_DATA pages,
			// and this copies out of it - so leaving it behind permanently removed one file's worth
			// of RAM from the machine, once per bootstrap-list entry. Loader data becomes
			// `MEM_BOOTLOADER`, which the kernel never seeds as usable, so it is not reclaimed later
			// either. Freed on the failure path too, which is the one a `try_reserve` makes reachable.
			//
			// SAFETY: `bytes` is exactly what `read_file` returned and nothing refers to it now - the
			// copy above is complete.
			unsafe { uefi::file::free_file(self.bs, bytes) };
			if copied { Some(owned) } else { None }
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
	#[cfg(target_arch = "x86_64")]
	arch::hand_off(bs, image_handle, system_table, root, kernel);
	#[cfg(not(target_arch = "x86_64"))]
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
	// The parse lives with the rule it feeds - `uefi::disk::parse_pairing`, beside `choose_volume` -
	// so both halves of the pairing mechanism are in one place and both are tested. What stays here
	// is the reading.
	// NOT FREED HERE, deliberately. A UUID is 36 bytes and this read costs a whole page of
	// LOADER_DATA that the kernel never seeds as usable - but `read_boot_file` has TWO sources, the
	// firmware read (page-backed, freeable) and the FAT fallback (its own buffer, not), and the
	// return type does not say which. Handing the wrong one to `free_pages` is worse than the leak.
	// Closing this needs the ownership expressed in the type; see LDR-012.
	// A FILE THAT IS THERE AND UNREADABLE IS NOT A MEDIUM WITH NO PAIRING.
	//
	// Every outcome used to collapse through `Option`: no file, an unreadable file, and 36 bytes of
	// something that is not a UUID all became `None`, and the caller reads `None` as "this medium
	// names no system volume" and takes the FIRST LiberFS volume the firmware enumerates. That is
	// the exact behaviour the pairing exists to prevent - two LiberSystem disks in one machine,
	// enumeration order deciding which one boots - reinstated by a typo in the file that was meant
	// to stop it.
	//
	// So the two are distinguished. A medium with no pairing file is the rescue stick the fallback
	// is for. A medium whose pairing file cannot be parsed is a medium that says which volume it
	// wants and cannot be understood, and this refuses rather than guessing at it.
	let Some(bytes) = read_boot_file(bs, root, "etc/system-volume.uuid") else {
		return None;
	};
	match uefi::disk::parse_pairing(bytes) {
		Some(uuid) => Some(uuid),
		None => {
			arch::serial::write_str("loader: FATAL - the boot medium carries a pairing file that is not a uuid, so which volume it wants cannot be established\n");
			arch::halt();
		}
	}
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
	// THE PAIRING RULE HAS ONE HOME NOW - `uefi::disk::choose_volume` - and it is tested there,
	// against two volumes presented in either handle order. It used to be an `if` inside this
	// closure, in a UEFI binary nothing could run: the mechanism that decides which of two
	// LiberSystem disks in one machine is this one's was the only part of this milestone with no
	// way to be exercised.
	//
	// A LiberFS volume that is not the one this medium is paired with is somebody else's system.
	// Keep looking; on a machine with one volume there is nothing to skip.
	let want = unsafe { PAIRED_UUID };
	let Some(mut fs) = (unsafe {
		uefi::disk::choose_volume(bs, want, |disk| {
			let fs = liberfs::LiberFs::mount(disk).ok()?;
			let uuid = fs.uuid();
			Some((uuid, fs))
		})
	}) else {
		return VolumeRead::NoVolume;
	};
	// The bootstrap set, packed into the archive format the kernel already unpacks. This is
	// what retires `init.pkg` as an artifact: the same bytes reach the kernel, assembled from
	// files that exist on the volume rather than from a package built beside it.
	if let Some(archive) = blockio::assemble_bootstrap(&mut fs) {
		unsafe {
			BOOTSTRAP = retain(bs, &archive);
			BOOTSTRAP_SOURCE = SOURCE_SYSTEM_VOLUME;
		}
	}
	// A LiberFS volume without this file is still the system volume; a second one is not going to
	// be more right. Stop rather than read the same name off another disk, which is how a machine
	// boots half of one system and half of another.
	match fs.read_file(path) {
		Ok(bytes) => match retain(bs, &bytes) {
			Some(retained) => VolumeRead::Read(retained),
			None => VolumeRead::NotOnVolume,
		},
		Err(_) => VolumeRead::NotOnVolume,
	}
}

// Read a file from the boot medium: through the firmware when it mounted the volume, and through
// the FAT backend over the block protocol when it did not.
pub(crate) fn read_boot_file(bs: *mut BootServices, root: Option<*mut uefi::FileProtocol>, name: &str) -> Option<&'static [u8]> {
	if let Some(root) = root
		&& let Some(bytes) = unsafe { read_file(bs, root, name) }
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
	unsafe {
		blockio::each_disk(bs, |disk| {
			if visit(disk) {
				BOOT_MEDIUM = Some(disk);
				true
			} else {
				false
			}
		});
	}
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
#[cfg(target_arch = "riscv64")]
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
	let phys = unsafe { alloc_pages(bs, pages) }?;
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

pub(crate) fn align_down(v: u64, align: u64) -> u64 {
	v & !(align - 1)
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
#[cfg(not(target_arch = "x86_64"))]
struct ReservedKernel {
	// (page-aligned base, page count) per PT_LOAD segment this loader reserved.
	spans: [(u64, u64); MAX_KERNEL_SPANS],
	count: usize,
	// Whether every PT_LOAD was reserved. False when a claim failed OR when there were more
	// segments than this table holds - both mean "do not trust this to answer for the whole image".
	complete: bool,
}

// Sixteen, matching the x86 backend's `MAX_SEGMENTS`. A kernel with more LOAD segments than this is
// refused at placement rather than partially reserved.
#[cfg(not(target_arch = "x86_64"))]
const MAX_KERNEL_SPANS: usize = 16;

#[cfg(not(target_arch = "x86_64"))]
impl ReservedKernel {
	const EMPTY: Self = Self { spans: [(0, 0); MAX_KERNEL_SPANS], count: 0, complete: true };

	// Did this loader reserve exactly this span? Asked by the aarch64 backend at placement.
	#[cfg(target_arch = "aarch64")]
	pub fn owns(&self, base: u64, pages: u64) -> bool {
		self.spans[..self.count].iter().any(|&(reserved_base, reserved_pages)| reserved_base == base && reserved_pages == pages)
	}

	// Whether the whole image is accounted for. The riscv64 backend asks before it stages: an
	// incomplete reservation there is a note, not a refusal, because its overlap check is what
	// guards the copy. aarch64 does not ask - it refuses per span at placement instead.
	#[cfg(target_arch = "riscv64")]
	pub fn is_complete(&self) -> bool {
		self.complete
	}
}

#[cfg(not(target_arch = "x86_64"))]
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
fn stage_kernel_clear_of_destination(bs: *mut uefi::BootServices, kernel: &'static [u8]) -> &'static [u8] {
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
	arch::serial::write_str(
		"loader: the kernel file was read into its own destination; staging it clear
",
	);
	let pages = (kernel.len() as u64).div_ceil(PAGE_SIZE);
	let staged = unsafe { alloc_pages(bs, pages as usize) }.expect("loader: cannot stage the kernel clear of its destination");
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
