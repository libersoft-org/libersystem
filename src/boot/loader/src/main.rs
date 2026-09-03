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
// The public keys this loader accepts a signed boot manifest from, and the profile it was built
// for. Public halves only - see the module.
mod trust;

use core::ffi::c_void;

use uefi::{BootServices, Handle, Status, SystemTable};
// The firmware-facing work the loader drives - moved into the `uefi` crate, where a mock firmware
// can exercise it. The loader is a UEFI binary and cannot run a test; the algorithms it used to
// hold could not either, which is why the hostile-firmware cases in this milestone were argued in
// code and never run.
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
	// THE MESSAGE, INCLUDING THE ONES THAT SAY ANYTHING.
	//
	// This was `info.message().as_str()`, which answers `Some` only for a panic whose message is a
	// bare literal - so every `panic!` in this file carrying a value said NOTHING. The refusals that
	// most need explaining are exactly the ones that name what they refused: "a boot source was
	// selected and failed its manifest ({reason:?})" reached the serial port as an empty string
	// after the colon, and a fail-closed loader whose last word is a line number is one nobody can
	// diagnose from a serial log.
	let mut out = SerialWriter;
	let _ = core::fmt::Write::write_fmt(&mut out, format_args!("{}", info.message()));
	arch::serial::write_str("\n");
	arch::halt()
}

// `core::fmt` over the serial port, which is all the panic handler needs and is why it is here
// rather than anywhere a caller might reach for it. No buffer: a panic that ran out of one would be
// a panic reporting its own reporting.
struct SerialWriter;

impl core::fmt::Write for SerialWriter {
	fn write_str(&mut self, s: &str) -> core::fmt::Result {
		arch::serial::write_str(s);
		Ok(())
	}
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
	// WHICH KEYS THIS BUILD TRUSTS, BEFORE IT LOADS ANYTHING. A boot that accepts a manifest signed
	// by a key whose private half is published, and does not say so, is exactly the thing a trust
	// chain is supposed to make impossible to do by accident.
	trust::announce();
	// AND WHAT THE FIRMWARE SAYS ABOUT ITSELF, in the same breath. A loader that verifies a manifest
	// under firmware that verified nothing is one link of a chain reported as the whole of it - so
	// the state is printed whether or not it is enforcing, and the gate reads this line.
	{
		let state = unsafe { uefi::variables::secure_boot_state(system_table) };
		arch::serial::write_str("loader: firmware SecureBoot=");
		write_variable(state.secure_boot);
		arch::serial::write_str(" SetupMode=");
		write_variable(state.setup_mode);
		arch::serial::write_str(if state.enforcing() { " (enforcing)\n" } else { " (NOT enforcing)\n" });
	}
	arch::serial::write_str("\nLiberSystem UEFI loader\n");
	// AFTER the banner, because it is a line about this loader rather than a line from the
	// firmware, and a diagnostic printed before the program names itself reads as stray output.
	#[cfg(not(target_arch = "x86_64"))]
	console::report();

	let bs = unsafe { (*system_table).boot_services };
	// The heap has to exist before the filesystem crates are used, and cannot outlive boot
	// services.
	heap::init(bs);
	// WHICH DEVICE THIS IMAGE CAME OFF, asked of the firmware before anything is read. Both readers
	// of the boot medium use it: the Simple File System path opens the volume on it, and the block
	// path matches it against the enumeration rather than guessing at the medium's contents.
	unsafe { BOOT_DEVICE = uefi::disk::loaded_image_device(bs, image_handle) };
	// The firmware normally mounts the boot volume for us. When it does not - a medium its Simple
	// File System driver declines - the FAT backend below reads it through the block protocol
	// instead, which is the whole reason that crate is linked.
	let root = open_boot_volume(bs, image_handle);

	// WHICH LiberFS volume is this installation's. A superblock identifies LiberFS; it does not
	// identify the system that owns it, so two LiberSystem disks in one machine used to let the
	// firmware's block-handle order decide which one booted. The boot medium names its volume by
	// uuid in its own SIGNED manifest, and when it does, nothing else may be the system volume.
	// A medium without the file keeps the old behaviour and says so, because a rescue stick is
	// exactly the medium that has no paired volume.
	unsafe { PAIRED_UUID = read_pairing(bs, root) };
	if unsafe { PAIRED_UUID }.is_none() {
		arch::serial::write_str("loader: the boot medium names no system volume; using the first LiberFS volume found\n");
	}

	// Prefer the system volume: a program that runs should have a file on the volume the user can
	// see, which is the whole point of this milestone. The ESP copy is the fallback, so a machine
	// whose system volume is missing or unreadable still boots far enough to say so.
	// EACH SOURCE CARRIES ITS OWN MANIFEST, and the kernel is checked against the one belonging to
	// the source it came from - not against whichever manifest happened to be readable. A boot made
	// of two halves from two systems is the failure this code already refuses for the bootstrap set;
	// the digests would be no use if they could be crossed the same way.
	let kernel = match read_from_system_volume(bs, KERNEL_FILE.as_bytes()) {
		VolumeRead::Read(bytes) => {
			arch::serial::write_str("loader: kernel read from the system volume\n");
			// THE SIGNED MANIFEST WHEREVER THE SOURCE HAS ONE, and the text one otherwise. Both are
			// checked BEFORE these bytes are parsed as an ELF, before a destination is derived from
			// them and before anything is copied anywhere: a header read out of an unverified image
			// is a decision made on an attacker's numbers.
			match read_from_system_volume(bs, b"etc/boot.manifest2") {
				VolumeRead::Read(signed) => {
					let mut scratch = alloc::vec::Vec::new();
					if scratch.try_reserve_exact(bootproto::manifest::DOMAIN.len() + signed.len()).is_err() {
						panic!("loader: no room to verify the system volume's signed manifest");
					}
					scratch.resize(bootproto::manifest::DOMAIN.len() + signed.len(), 0);
					let expected = trust::Expected::volume(unsafe { PAIRED_UUID });
					let Some(manifest) = trust::verify_for(signed, &expected, &mut scratch) else {
						panic!("loader: the system volume's signed manifest was refused - see the line above");
					};
					if !blockio::covered_by(&manifest, bootproto::manifest::KIND_KERNEL, KERNEL_FILE.as_bytes(), &bytes) {
						panic!("loader: the kernel is not what the system volume's SIGNED manifest records");
					}
					announce_release(&manifest, "the system volume's kernel");
				}
				// PRESENT AND UNREADABLE IS BETRAYAL, NOT ABSENCE - which `assemble_bootstrap` has
				// said for the bootstrap set all along, and this path could not say because every
				// failure to read arrived here as one value.
				//
				// Damaging one file on the volume dropped its KERNEL from "signed" to "checksummed":
				// the read failed, this fell to the arm below, and a `test-trust` build accepted the
				// text manifest - a checksum list an attacker recomputes along with the payload. That
				// is a downgrade performed without forging anything, and it is the case the
				// selected-source rule exists for.
				VolumeRead::Unreadable => {
					panic!("loader: the system volume's signed manifest is there and could not be read - refusing to fall back to the checksum one");
				}
				_ => {
					// See `assemble_bootstrap`: a signed manifest that is ABSENT used to drop this
					// source to the text one, which is a checksum list an attacker recomputes along
					// with the payload. Whether this build takes that is a profile, and it says so.
					if !trust::IS_TEST_TRUST {
						panic!("loader: the system volume carries no SIGNED manifest, and this build authenticates what it boots - refusing rather than falling back to the text one");
					}
					let VolumeRead::Read(manifest) = read_from_system_volume(bs, b"etc/boot.manifest") else {
						panic!("loader: the system volume has a kernel and no manifest of either kind - refusing to boot from it");
					};
					if !blockio::digests_ok(&manifest, KERNEL_FILE.as_bytes(), &bytes) {
						panic!("loader: the kernel does not match etc/boot.manifest on the system volume");
					}
					arch::serial::write_str("loader: THIS KERNEL IS NOT AUTHENTICATED - the system volume carries no signed manifest, and this build accepts the checksum one\n");
				}
			}
			bytes
		}
		// The two failures are reported apart on purpose. "No LiberFS volume" points at the
		// disk, its cabling or the wrong machine; "volume found, file missing" points at the
		// build that staged it. A single message covering both would send whoever reads this
		// log looking in the wrong place, and this milestone's own risk note says the failure
		// message matters more than usual here.
		VolumeRead::NoVolume => {
			arch::serial::write_str("loader: no LiberFS volume on any block device; using the boot volume\n");
			read_verified_kernel_from_boot_medium(bs, root)
		}
		// A SELECTED VOLUME THAT FAILED IS THE END OF THE BOOT.
		//
		// Not a fallback: the pairing named this volume, or this machine has exactly one, and the
		// read of its kernel failed. Continuing to the boot medium here is how a machine whose disk
		// is going bad silently boots an older kernel off its ESP.
		VolumeRead::Unreadable => {
			panic!("loader: the system volume was selected and did not answer - it would not mount, or its kernel is missing or unreadable - refusing to boot something else instead");
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
			read_verified_kernel_from_boot_medium(bs, root)
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
		// AND NOW IT CAN TELL YOU. This answered `Absent` for every failure, because the firmware's
		// own status was dropped by the reader under it - so the downgrade `FileRead::Unreadable`
		// exists to catch was catchable on the two block backends and not on this one, which is the
		// backend every machine whose firmware mounts the ESP actually uses. A signed manifest
		// DAMAGED rather than removed read as absent, and `assemble_bootstrap` fell back to the
		// text manifest beside it: an attacker performs a downgrade by corrupting one file instead
		// of forging anything.
		//
		// `read_file_reported` carries the status back, so present-and-unreadable is now what it is.
		fn read_file(&mut self, path: &[u8]) -> blockio::FileRead {
			let mut name = [0u8; 128];
			if path.len() >= name.len() {
				return blockio::FileRead::Unreadable;
			}
			// AND THE LIMIT THE FIRMWARE READER ACTUALLY HAS. This accepted any path under 128 bytes
			// while the encoder takes `MAX_PATH_UNITS` UTF-16 units, so the same bootstrap
			// list could work through Block I/O and fail through Simple File System - on a machine
			// with both, with the file simply absent and nothing said. Counted in UTF-16 units, not
			// bytes, because a non-ASCII path makes the two differ and it is units the firmware
			// counts. One unit is reserved for the terminator, as in the encoder.
			//
			// A name this reader cannot express is `Unreadable` rather than `Absent`: it is a
			// statement about the reader, and answering "the volume does not have it" would be the
			// same silent downgrade one level down.
			let Ok(text) = core::str::from_utf8(path) else {
				return blockio::FileRead::Unreadable;
			};
			if text.chars().map(char::len_utf16).sum::<usize>() + 1 > uefi::file::MAX_PATH_UNITS {
				return blockio::FileRead::Unreadable;
			}
			for (i, &b) in path.iter().enumerate() {
				name[i] = if b == b'/' { b'\\' } else { b };
			}
			// `/` and `\` are both ASCII, so the translation above cannot land inside a multi-byte
			// sequence and this cannot fail - but it is checked rather than asserted.
			let Ok(name) = core::str::from_utf8(&name[..path.len()]) else {
				return blockio::FileRead::Unreadable;
			};
			let bytes = match unsafe { uefi::file::read_file_reported(self.bs, self.root, name) } {
				uefi::file::FirmwareRead::Bytes(bytes) => bytes,
				uefi::file::FirmwareRead::Absent => return blockio::FileRead::Absent,
				uefi::file::FirmwareRead::Failed => return blockio::FileRead::Unreadable,
			};
			let mut owned = alloc::vec::Vec::new();
			let copied = owned.try_reserve_exact(bytes.len()).is_ok();
			if copied {
				owned.extend_from_slice(bytes);
			}
			// THE PAGES GO BACK. The reader hands out an unowned slice in fresh LOADER_DATA pages,
			// and this copies out of it - so leaving it behind permanently removed one file's worth
			// of RAM from the machine, once per bootstrap-list entry. Loader data becomes
			// `MEM_BOOTLOADER`, which the kernel never seeds as usable, so it is not reclaimed later
			// either. Freed on the failure path too, which is the one a `try_reserve` makes reachable.
			//
			// SAFETY: `bytes` is exactly what the reader returned and nothing refers to it now - the
			// copy above is complete.
			unsafe { uefi::file::free_file(self.bs, bytes) };
			if copied { blockio::FileRead::Bytes(owned) } else { blockio::FileRead::Unreadable }
		}
	}

	fn bootstrap_from_boot_medium(bs: *mut BootServices, root: Option<*mut uefi::FileProtocol>) {
		if unsafe { BOOTSTRAP }.is_some() || unsafe { BOOTSTRAP_REFUSED }.is_some() {
			return;
		}
		if let Some(root) = root {
			let mut volume = FirmwareVolume { bs, root };
			match blockio::assemble_bootstrap(&mut volume, &trust::Expected::medium()) {
				// RETURN ONLY WHEN THE RETAIN SUCCEEDED. This returned either way, so an
				// allocation failure here skipped the FAT scan that would have answered.
				abi::bootstrap::Selection::Verified(archive) => {
					if let Some(retained) = retain(bs, &archive) {
						unsafe {
							BOOTSTRAP = Some(retained);
							BOOTSTRAP_SOURCE = SOURCE_BOOT_MEDIUM;
						}
						return;
					}
				}
				// SELECTED AND FAILED. There is no path from here to another source.
				abi::bootstrap::Selection::Invalid(reason) => {
					unsafe { BOOTSTRAP_REFUSED = Some(reason) };
					return;
				}
				// Not a boot source at all; the scan below may find one.
				abi::bootstrap::Selection::Unavailable => {}
			}
		}
		with_boot_medium(bs, |disk| {
			let Some(mut fs) = fat::FatFs::mount_read_only(disk) else { return Visit::NotAMedium };
			match blockio::assemble_bootstrap(&mut fs, &trust::Expected::medium()) {
				abi::bootstrap::Selection::Verified(archive) => unsafe {
					match retain(bs, &archive) {
						Some(retained) => {
							BOOTSTRAP = Some(retained);
							BOOTSTRAP_SOURCE = SOURCE_BOOT_MEDIUM;
						}
						// A retain that fails leaves this boot without a bootstrap set, and it does
						// not send the search to another disk to find one: the medium is this one.
						None => {}
					}
					Visit::Mounted { done: true }
				},
				// SELECTED AND FAILED. The scan stops: a medium that names programs and cannot be
				// checked is not one to look past.
				abi::bootstrap::Selection::Invalid(reason) => {
					unsafe { BOOTSTRAP_REFUSED = Some(reason) };
					Visit::Mounted { done: true }
				}
				// A FAT medium with no bootstrap list is still the boot medium - it mounted - and
				// this boot simply has no set on it. It used to send the scan to the next disk,
				// which is how a kernel from one stick met a bootstrap set from another.
				abi::bootstrap::Selection::Unavailable => Visit::Mounted { done: true },
			}
		});
	}

	// A live medium's volume is a FILE on the boot filesystem, so the disk scan above never saw it.
	// Read it here, before the report below, and let it supply the bootstrap set the same way an
	// installed system's partition does.
	// THE SAME THREE ANSWERS AS A PACKAGE, AND FOR THE SAME REASON (corrected 2026-08-31). This read
	// collapsed "the medium carries no live volume" into "the live volume could not be read", and the
	// manifest coverage check lived inside the `Some` arm - so deleting or corrupting the image the
	// signed manifest selected removed it silently and the boot went on to the next bootstrap source.
	// See `read_verified_package`, which this now matches.
	let volume_read = read_boot_file_reported(bs, root, LIVE_VOLUME_FILE);
	if let MediumRead::Unreadable = volume_read {
		panic!("loader: the boot medium's live system volume could not be READ - that is a failing medium, not a medium without one");
	}
	unsafe {
		LIVE_VOLUME = match volume_read {
			MediumRead::Bytes(bytes) => Some(bytes),
			_ => None,
		}
	};
	// AND AN ABSENT ONE IS CHECKED AGAINST THE MANIFEST BEFORE IT IS CALLED ABSENT. A signed row for
	// `system-volume.img` is the medium saying it carries one.
	if unsafe { LIVE_VOLUME }.is_none()
		&& let Some(manifest) = boot_medium_manifest(bs, root)
		&& manifest.find(bootproto::manifest::KIND_SYSTEM_VOLUME, LIVE_VOLUME_FILE.as_bytes()).is_some()
	{
		panic!("loader: the boot medium's SIGNED manifest names a system volume the medium does not carry - refusing to boot as though it were never there");
	}
	if let Some(image) = unsafe { LIVE_VOLUME } {
		// COVERED BEFORE IT IS MOUNTED OR PUBLISHED. This image is handed to the kernel as a module
		// and read as a filesystem here; both are decisions taken on its contents, so the medium's
		// manifest has to have vouched for the whole of it first. A medium with only the text
		// manifest keeps what it had - integrity, and nothing about origin.
		if let Some(manifest) = boot_medium_manifest(bs, root)
			&& !blockio::covered_by(&manifest, bootproto::manifest::KIND_SYSTEM_VOLUME, LIVE_VOLUME_FILE.as_bytes(), image)
		{
			panic!("loader: the live system volume is not what the boot medium's signed manifest records");
		}
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
	// A SELECTED SOURCE THAT FAILED ENDS THE BOOT, and it ends it HERE rather than by handing a
	// kernel a set nobody checked. The same shape as the kernel's own manifest failure two hundred
	// lines up: the panic handler writes to serial and hangs, which is what fail-closed looks like
	// on a machine with no operator.
	if let Some(reason) = unsafe { BOOTSTRAP_REFUSED } {
		panic!("loader: a boot source was selected and failed its manifest ({reason:?}) - refusing to hand off");
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
	// No block device carried a LiberFS superblock. The one answer policy may consider another
	// source for: the volume this medium wants is not in the machine.
	NoVolume,
	// A LiberFS volume was found and this file is not on it.
	NotOnVolume,
	// A LiberFS volume was found and the read FAILED, which is not the same thing.
	//
	// M4's rule is that a source, once selected, either answers or stops the boot: a missing named
	// file, an I/O failure or malformed metadata is `Invalid` and none of them falls through to
	// another disk or to the ESP. All three used to arrive here as `NotOnVolume` - the file is not
	// there, `fs.read_file` returned an error, and `retain` had no memory for the bytes - and the
	// caller printed "the system volume has no kernel" and read the boot medium's copy instead. A
	// volume the pairing NAMED, whose kernel could not be read off a failing disk, quietly booted
	// somebody else's kernel and said nothing that would let anyone tell.
	Unreadable,
}

// The bootstrap archive assembled from the volume, if there was one. Read in the SAME mount as
// the kernel: mounting twice would walk every block device twice and, worse, could pick a
// different volume for the two halves of one boot on a machine with more than one.
pub(crate) static mut BOOTSTRAP: Option<&'static [u8]> = None;
// WHERE the bootstrap set came from. Three sources now answer in turn, and a boot that used the
// last one looks identical to a boot that used the first unless it says so - which is the same
// reason the kernel read is reported.
static mut BOOTSTRAP_SOURCE: &str = "";
// A SOURCE THAT REFUSED, WHICH IS NOT A SOURCE THAT WAS ABSENT.
//
// Once one is seen the search stops and the boot does not continue: there is no path from "this
// medium named programs and its manifest refused them" to "try the next one". A fallback is another
// source a signed policy permits, and "try everything until something boots" is not a policy.
static mut BOOTSTRAP_REFUSED: Option<abi::bootstrap::Refusal> = None;
pub(crate) const SOURCE_SYSTEM_VOLUME: &str = "the system volume";
const SOURCE_LIVE_IMAGE: &str = "the live medium's volume image";
const SOURCE_BOOT_MEDIUM: &str = "the boot medium (the system volume did not answer)";

// The uuid of the volume this boot medium is paired with, when it names one.
static mut PAIRED_UUID: Option<[u8; 16]> = None;

// WHAT THIS LOADER CHOSE AS THE SYSTEM VOLUME, recorded where the choice is made.
//
// Set once, by whichever branch actually used a volume, and read at the hand-off. It is not
// re-derived from what is lying on the medium, because two of the three cases can be true of one
// medium at once - a shipping ISO carries the image AND a signed pairing - and a reader deciding
// afterwards can decide differently from the loader that already decided.
static mut ROOT_SELECTION: bootproto::RootSelection = bootproto::RootSelection { kind: bootproto::ROOT_NONE, module: 0, uuid: [0; 16] };

// WHETHER THIS LOADER WITHHOLDS THE DEVICE TREE FROM THE KERNEL.
//
// The named no-device-tree regression profile needs a machine the kernel sees as having no tree, and
// this harness cannot produce one: QEMU's `virt` publishes a DTB to the firmware and the firmware
// publishes it in its configuration table, on both device-tree ports. There are exactly two ways to
// get one - a QEMU machine that publishes none, or a LOADER THAT DECLINES TO PASS ONE ON - and this
// is the second.
//
// COMPILE-TIME, FOR THE REASON THE KERNEL'S OWN AUTHORISATION IS. A boot with no tree has no way to
// name itself, because the machine description IS the tree; so the profile is compiled in on both
// sides and the harness that boots it is what sets the variable. The two halves are deliberately the
// SAME name: a loader that withholds the tree and a kernel that has not authorised its static
// descriptor is a machine that panics, and the panic says so - which is the correct outcome for a
// mismatched pair and is what the kernel's refusal exists for.
//
// The ordinary loader is untouched: nothing in the shipping build sets this, `find_dtb` answers what
// the firmware published, and the kernel is handed it.
//
// THE DEVICE-TREE PORTS ONLY, because x86_64 has no tree to withhold - it reads ACPI - so the option
// would be a knob with nothing behind it there, and an unused one at that.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn withholds_device_tree() -> bool {
	option_env!("LIBER_NO_DT_PROFILE").is_some_and(|value| value == "1")
}

// What the loader chose, for the hand-off.
pub(crate) fn root_selection() -> bootproto::RootSelection {
	unsafe { ROOT_SELECTION }
}

// Which system volume this boot medium is paired with: the uuid in its own signed manifest, and
// `None` when it names none.
fn read_pairing(bs: *mut BootServices, root: Option<*mut uefi::FileProtocol>) -> Option<[u8; 16]> {
	// OUT OF THE SIGNED MANIFEST, NOT OUT OF A FILE BESIDE IT.
	//
	// The pairing used to be `etc/system-volume.uuid`, plain text, read straight off the medium and
	// used to decide which volume this machine boots. Nothing signed it. Anyone who could write the
	// ESP could repoint it at a different signed volume - or simply DELETE it, which is easier and
	// worse: the loader read the absence as "this medium names no volume" and took the first LiberFS
	// volume the firmware enumerated, which is exactly the behaviour the pairing exists to remove.
	// One unsigned file could turn the whole mechanism off.
	//
	// The value now travels in the boot medium's own signed manifest, in the header field made for
	// it, and it is read only after that manifest has been verified for THIS product, THIS
	// architecture and this source kind. A medium whose manifest names no volume - a rescue stick,
	// the test medium - answers zero and keeps the fallback, and it says so.
	let bytes = match read_boot_file_reported(bs, root, "etc/boot.manifest2") {
		MediumRead::Bytes(bytes) => bytes,
		// A MEDIUM WITH NO SIGNED MANIFEST NAMES NO VOLUME, and on an authenticated build that is
		// not a medium to boot at all - which the kernel and bootstrap reads below refuse for
		// themselves. Here it means only that there is no pairing to be had.
		MediumRead::Absent => {
			if !trust::IS_TEST_TRUST {
				arch::serial::write_str("loader: the boot medium carries no SIGNED manifest, so it names no system volume and this build cannot take one on trust\n");
			}
			return None;
		}
		// AND A MANIFEST THAT IS THERE AND CANNOT BE READ IS NOT ONE THAT IS ABSENT. It is the same
		// case as one that does not verify, arrived at one step earlier: the medium says which
		// volume it wants and this loader cannot find out which. Falling through to "names no
		// volume" would take the first LiberFS volume in the machine - the behaviour the pairing
		// exists to remove - because a sector went bad.
		MediumRead::Unreadable => {
			arch::serial::write_str("loader: FATAL - the boot medium's signed manifest is present and could not be read, so which volume it names cannot be established\n");
			arch::halt();
		}
	};
	let mut scratch = alloc::vec::Vec::new();
	if scratch.try_reserve_exact(bootproto::manifest::DOMAIN.len() + bytes.len()).is_err() {
		arch::serial::write_str("loader: FATAL - no room to verify the boot medium's signed manifest\n");
		arch::halt();
	}
	scratch.resize(bootproto::manifest::DOMAIN.len() + bytes.len(), 0);
	let Some(manifest) = trust::verify_for(bytes, &trust::Expected::medium(), &mut scratch) else {
		// A MEDIUM WHOSE MANIFEST IS THERE AND DOES NOT VERIFY IS NOT A MEDIUM WITH NO PAIRING. It
		// is a medium that says which volume it wants and cannot be believed, and every later read
		// from it is about to be refused for the same reason. Stopping here says which.
		arch::serial::write_str("loader: FATAL - the boot medium's signed manifest was refused, so which volume it names cannot be established\n");
		arch::halt();
	};
	// The release this boot is, latched by the first thing verified - and on a paired machine that
	// is this manifest, before any volume has been chosen.
	announce_release(&manifest, "the boot medium's pairing record");
	if manifest.volume_uuid == [0u8; 16] { None } else { Some(manifest.volume_uuid) }
}

// Read a PACKAGE off the boot medium and check it against the medium's signed manifest.
//
// THE BUILD SIGNED THESE ROWS AND THE LOADER HAD NEVER READ ONE. `sign-manifest` writes a
// `package:` row for whichever payload a medium carries, and `KIND_PACKAGE` appeared in no loader
// source file at all - so `init.pkg` and `volume.pkg` were read off the ESP and published to the
// kernel as boot modules with nothing compared. On a machine whose system volume is missing or
// unreadable, `init.pkg` IS the userspace, arriving unverified down the fallback the signed path
// exists to make unnecessary.
//
// A package that is absent is absent; one that is there and is not what the manifest records, or is
// not in the manifest at all, stops the boot. There is no third answer: a module the kernel is about
// to run is not something to hand over with a warning.
pub(crate) fn read_verified_package(bs: *mut BootServices, root: Option<*mut uefi::FileProtocol>, name: &str) -> Option<&'static [u8]> {
	// ABSENT, UNREADABLE AND "THE MANIFEST NAMED IT" ARE THREE ANSWERS, AND THIS READ GAVE ONE
	// (corrected 2026-08-31).
	//
	// The payload was read through the `Option`-returning reader, which collapses "the medium does
	// not carry this file" into "the medium could not be read" - and the caller treats `None` as the
	// legitimate absence of an OPTIONAL artifact and boots on. `volume.pkg` is optional on all three
	// ports and `init.pkg` is optional on two of them, so a signed payload that was DELETED, or whose
	// sectors have gone bad, silently became "this medium does not carry one" and the boot proceeded
	// to hand-off or to another bootstrap source. That is the terminal case M3/M4 name - a named file
	// missing, or an I/O failure - taken as a normal one.
	//
	// The manifest is therefore read FIRST, and the three cases are separated against it: unreadable
	// is terminal whatever the manifest says, a payload the manifest NAMES and the medium does not
	// carry is terminal, and only a payload that is absent AND unnamed is the optional artifact this
	// returns `None` for.
	// The bytes, if the medium carries them. An ABSENT payload is still checked against the manifest
	// below before it is called optional; an unreadable one never gets that far.
	let payload: Option<&'static [u8]> = match read_boot_file_reported(bs, root, name) {
		MediumRead::Bytes(bytes) => Some(bytes),
		MediumRead::Absent => None,
		MediumRead::Unreadable => panic!("loader: a package the boot medium should carry could not be READ - that is a failing medium, not a medium without one"),
	};
	let signed = match read_boot_file_reported(bs, root, "etc/boot.manifest2") {
		MediumRead::Bytes(signed) => signed,
		// The same profile split every other read on this medium makes: an authenticated build does
		// not publish an unverified module, and a test-trust one says what it is doing.
		MediumRead::Absent => {
			if !trust::IS_TEST_TRUST {
				panic!("loader: a package is on the boot medium and the medium carries no SIGNED manifest - refusing to hand it to the kernel");
			}
			arch::serial::write_str(
				"loader: THIS PACKAGE IS NOT AUTHENTICATED - the boot medium carries no signed manifest, and this build accepts it
",
			);
			// Whatever the medium had, which on this profile is the whole of the check. `None` here
			// is a medium with neither a manifest nor the payload, which is a legitimate absence.
			return payload;
		}
		// AND AN UNREADABLE ONE IS NOT AN ABSENT ONE, on EITHER profile. The test-trust arm above is
		// for a medium that carries no manifest by design; a medium that carries one and cannot be
		// read is a medium whose module nothing can be checked against, and handing that to the
		// kernel is the failure the signed path exists to remove. See `MediumRead`.
		MediumRead::Unreadable => panic!("loader: a package is on the boot medium and the medium's signed manifest is present and unreadable - refusing to hand it to the kernel"),
	};
	let mut scratch = alloc::vec::Vec::new();
	if scratch.try_reserve_exact(bootproto::manifest::DOMAIN.len() + signed.len()).is_err() {
		panic!("loader: no room to verify the boot medium's signed manifest");
	}
	scratch.resize(bootproto::manifest::DOMAIN.len() + signed.len(), 0);
	let Some(manifest) = trust::verify_for(signed, &trust::Expected::medium(), &mut scratch) else {
		panic!("loader: the boot medium's signed manifest was refused - see the line above");
	};
	let Some(bytes) = payload else {
		// NAMED AND NOT THERE IS NOT OPTIONAL. The manifest is the medium's statement about what it
		// carries; a row for this artifact means the build put one here and it is gone. Returning
		// `None` would let the boot continue down the unsigned fallback the row exists to make
		// unnecessary, which is the substitution this whole path prevents.
		if manifest.find(bootproto::manifest::KIND_PACKAGE, name.as_bytes()).is_some() {
			panic!("loader: the boot medium's SIGNED manifest names a package the medium does not carry - refusing to boot as though it were never there");
		}
		return None;
	};
	if !blockio::covered_by(&manifest, bootproto::manifest::KIND_PACKAGE, name.as_bytes(), bytes) {
		panic!("loader: a package on the boot medium is not what its SIGNED manifest records - refusing to hand it to the kernel");
	}
	Some(bytes)
}

// The live medium's system volume, read once. It is needed twice - to assemble the bootstrap set
// from and to hand to the kernel as a module - and it is the largest thing this loader reads, so
// reading it per use would cost another copy of it in firmware pages.
// WHICH MODULE THE EMBEDDED SELECTION NAMES, recorded by the architecture that builds the module
// array. A no-op unless the live image is this boot's system volume: an `Embedded` selection is what
// gives the index a meaning, and writing one over a `Block` selection would rename a disk.
pub(crate) fn record_embedded_root(module: u32) {
	unsafe {
		if ROOT_SELECTION.kind == bootproto::ROOT_EMBEDDED {
			ROOT_SELECTION.module = module;
		}
	}
}

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
	// AND A SOURCE THAT REFUSED IS ALSO AN ANSWER. The same pair `bootstrap_from_boot_medium`
	// guards on: "this volume named programs and its manifest refused them" ends the boot, and
	// reading another source's list afterwards is the fallback that a refusal exists to prevent.
	if unsafe { BOOTSTRAP }.is_some() || unsafe { BOOTSTRAP_REFUSED }.is_some() {
		return;
	}
	let Ok(mut fs) = liberfs::LiberFs::mount(blockio::ImageDisk { bytes }) else { return };
	match blockio::assemble_bootstrap(&mut fs, &trust::Expected::volume(unsafe { PAIRED_UUID })) {
		abi::bootstrap::Selection::Verified(archive) => unsafe {
			BOOTSTRAP = retain(bs, &archive);
			BOOTSTRAP_SOURCE = SOURCE_LIVE_IMAGE;
			// THE IMAGE IS THIS BOOT'S SYSTEM VOLUME, and only because no disk answered first: the
			// guard at the top of this function is what makes an installed system win over an image
			// lying on the medium beside it.
			//
			// `module` AND `uuid` ARE WHAT THE PROTOCOL SAYS THEY ARE. This wrote `module: 0` and the
			// image's filesystem uuid; `RootSelection` defines this case as the INDEX of
			// `system-volume.img` with a ZERO uuid, and on the shipping x86_64 path module zero is
			// `init.pkg` - so the one field that names which module was chosen named a different
			// one, and every reader sidestepped it by looking the module up by filename instead.
			// The index is not knowable here: it is decided where the module array is built, and
			// `record_embedded_root` is called from there.
			ROOT_SELECTION = bootproto::RootSelection { kind: bootproto::ROOT_EMBEDDED, module: 0, uuid: [0u8; 16] };
		},
		abi::bootstrap::Selection::Invalid(reason) => unsafe { BOOTSTRAP_REFUSED = Some(reason) },
		abi::bootstrap::Selection::Unavailable => {}
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
	// EVERY MOUNT ERROR USED TO BE `.ok()?` - one character that turned a corrupt superblock, a
	// device that did not answer, and a volume written by a newer build into "not a LiberFS disk",
	// which is the one answer that lets this boot go on to the medium's own kernel. Corrupting one
	// superblock reached the signed fallback as though the disk had been pulled out of the machine.
	//
	// `Unformatted` is the only error that means what the old code said: no superblock, so a blank
	// disk or somebody else's. The other five say the disk is OURS AND BROKEN, and a broken system
	// volume stops the boot rather than handing it to whatever else is bootable.
	let mut fs = match unsafe {
		uefi::disk::choose_volume(bs, want, |disk| match liberfs::LiberFs::mount(disk) {
			Ok(fs) => {
				let uuid = fs.uuid();
				Ok(Some((uuid, fs)))
			}
			Err(liberfs::MountError::Unformatted) => Ok(None),
			Err(_) => Err(()),
		})
	} {
		uefi::disk::VolumeChoice::Chosen(fs) => fs,
		uefi::disk::VolumeChoice::NotHere => return VolumeRead::NoVolume,
		uefi::disk::VolumeChoice::Failed => return VolumeRead::Unreadable,
	};
	// A VOLUME WAS FOUND AND NOTHING NAMED IT.
	//
	// `choose_volume` with no pairing takes the first LiberFS volume the firmware enumerates, which
	// is the block-handle order this whole mechanism exists to stop deciding things. That is the
	// right answer for a rescue stick - a medium deliberately paired with nothing, booting whatever
	// system is in the machine - and it is not an answer an authenticated release may give, because
	// the release cannot say which system it just booted. The build refuses to produce such a medium
	// when it carries a volume; this refuses to boot one.
	if want.is_none() && !trust::IS_TEST_TRUST {
		panic!("loader: a LiberFS volume is in this machine and the boot medium's signed manifest names none - refusing to pick one by firmware enumeration order");
	}
	// A DISK VOLUME ANSWERED, so this boot's system volume is that one and nothing downstream has
	// to work it out again. Recorded HERE, at the branch that used it - `choose_volume` has already
	// applied the pairing rule, so `fs.uuid()` is the volume this medium names, not the first one
	// the firmware enumerated.
	unsafe {
		if ROOT_SELECTION.kind == bootproto::ROOT_NONE {
			ROOT_SELECTION = bootproto::RootSelection { kind: bootproto::ROOT_BLOCK, module: 0, uuid: fs.uuid() };
		}
	}
	// The bootstrap set, packed into the archive format the kernel already unpacks. This is
	// what retires `init.pkg` as an artifact: the same bytes reach the kernel, assembled from
	// files that exist on the volume rather than from a package built beside it.
	//
	// ONCE PER BOOT, NOT ONCE PER READ. `main` reads this volume up to three times - the kernel, the
	// signed manifest, the text manifest - and every one of them re-assembled the whole set:
	// re-reading `etc/bootstrap.list` and then every program it names, off the same volume, to build
	// an archive that was thrown away because `BOOTSTRAP` already held one. The boot log carried the
	// receipt as `bootstrap set verified against a SIGNED etc/boot.manifest2`, printed twice.
	//
	// The guard is the same pair `bootstrap_from_boot_medium` already uses: a set that was assembled,
	// or a source that refused, are both answers and neither is improved by asking again.
	if unsafe { BOOTSTRAP }.is_none() && unsafe { BOOTSTRAP_REFUSED }.is_none() {
		match blockio::assemble_bootstrap(&mut fs, &trust::Expected::volume(unsafe { PAIRED_UUID })) {
			abi::bootstrap::Selection::Verified(archive) => unsafe {
				BOOTSTRAP = retain(bs, &archive);
				BOOTSTRAP_SOURCE = SOURCE_SYSTEM_VOLUME;
			},
			abi::bootstrap::Selection::Invalid(reason) => unsafe { BOOTSTRAP_REFUSED = Some(reason) },
			abi::bootstrap::Selection::Unavailable => {}
		}
	}
	// A LiberFS volume without this file is still the system volume; a second one is not going to
	// be more right. Stop rather than read the same name off another disk, which is how a machine
	// boots half of one system and half of another.
	match fs.read_file(path) {
		Ok(bytes) => match retain(bs, &bytes) {
			Some(retained) => VolumeRead::Read(retained),
			// Out of firmware pages. The file is on the volume and this boot cannot hold it, which
			// is a failure of the read rather than an absence.
			None => VolumeRead::Unreadable,
		},
		// A NAME THAT IS NOT ON THE VOLUME IS THE ONE ABSENCE - ON A VOLUME NOTHING SELECTED.
		// Everything else the filesystem answers with - a bad extent, a short read, a disk returning
		// an error - is the selected source failing, and the difference is what decides whether this
		// boot may look elsewhere.
		//
		// AND A PAIRING SELECTS. When the medium's signed manifest NAMES this volume, the volume is
		// the chosen source and M4's rule applies to it: a missing named file is `Invalid`, not
		// absence. Without this, an attacker who could not forge a signature did not need to - leave
		// the volume's signed manifest exactly where it is, delete the kernel from the mutable
		// filesystem, and the loader announced "the system volume has no kernel" and booted the
		// medium's own. The unpaired case is unchanged: a rescue stick paired with nothing, or a
		// machine with a single volume and no pairing, may still fall back, because nothing named
		// that volume as the source.
		Err(liberfs::FsError::NotFound) if want.is_none() => VolumeRead::NotOnVolume,
		Err(liberfs::FsError::NotFound) => VolumeRead::Unreadable,
		Err(_) => VolumeRead::Unreadable,
	}
}

// Read a file from the boot medium: through the firmware when it mounted the volume, and through
// the FAT backend over the block protocol when it did not.
pub(crate) fn read_boot_file(bs: *mut BootServices, root: Option<*mut uefi::FileProtocol>, name: &str) -> Option<&'static [u8]> {
	// ONE PATH SPELLING FOR TWO READERS. The FAT backend takes `/`-separated paths, as the bootstrap
	// list writes them; the firmware's file protocol takes `\`. This took the caller's string
	// verbatim for both, which worked for exactly as long as every name here was a single component
	// - and then `etc/boot.manifest` reached the firmware reader as a name no volume has.
	let mut separated = [0u8; 128];
	let firmware_name = if name.len() < separated.len() {
		for (i, &b) in name.as_bytes().iter().enumerate() {
			separated[i] = if b == b'/' { b'\\' } else { b };
		}
		core::str::from_utf8(&separated[..name.len()]).ok()
	} else {
		None
	};
	// A FILE THE MEDIUM DOES NOT CARRY IS NOT A FIRMWARE THAT DID NOT MOUNT IT.
	//
	// This fell through to the block-level scan on ANY unsuccessful firmware read, and the scan is
	// not cheap: it walks every Block I/O handle in the machine and mounts FAT on each, and a FAT
	// mount audits the volume's whole ownership map. On a shipping medium - which carries a system
	// volume and no factory archive, by design - the archive read reached firmware that answered
	// `EFI_NOT_FOUND`, and the loader then spent about half a minute searching every disk for a
	// file no medium in the machine has, discarded the `None` it started with, and printed a line
	// claiming the firmware had not mounted a volume it had been reading from since before the
	// banner.
	//
	// `Absent` ends the read here. The fallback is for a medium this firmware cannot read, and
	// nothing else.
	if let Some(root) = root
		&& let Some(firmware_name) = firmware_name
	{
		match unsafe { uefi::file::read_file_reported(bs, root, firmware_name) } {
			uefi::file::FirmwareRead::Bytes(bytes) => return Some(bytes),
			uefi::file::FirmwareRead::Absent => return None,
			// Something is there and this reader could not get it, or the name is one it cannot
			// encode. The block backend takes both.
			uefi::file::FirmwareRead::Failed => {
				arch::serial::write_str("loader: the firmware could not read ");
				arch::serial::write_str(name);
				arch::serial::write_str(" off the boot volume; reading the medium as FAT\n");
			}
		}
	} else {
		arch::serial::write_str("loader: the firmware did not mount the boot volume; reading it as FAT\n");
	}
	read_from_fat(bs, name.as_bytes())
}

// The same read, keeping the absent-versus-unreadable distinction the manifest paths turn on.
//
// ORDINARY FILES KEEP THE `Option`. A kernel or a package that is not there is refused by name a
// line later, and the callers that decide a TRUST question on an absence are the three that read a
// manifest - so those are the ones that ask this.
//
// A firmware `Failed` is "something is there and this reader could not get it", which is already the
// unreadable answer: the FAT backend is given its chance and, if it cannot produce the bytes either,
// the file is unreadable rather than absent - even where FAT reports `NotFound`, because a firmware
// that opened it and a FAT reader that cannot find it disagree about the medium, and disagreement is
// not evidence of absence.
pub(crate) fn read_boot_file_reported(bs: *mut BootServices, root: Option<*mut uefi::FileProtocol>, name: &str) -> MediumRead {
	let mut separated = [0u8; 128];
	let firmware_name = if name.len() < separated.len() {
		for (i, &b) in name.as_bytes().iter().enumerate() {
			separated[i] = if b == b'/' { b'\\' } else { b };
		}
		core::str::from_utf8(&separated[..name.len()]).ok()
	} else {
		None
	};
	let mut firmware_had_it = false;
	if let Some(root) = root
		&& let Some(firmware_name) = firmware_name
	{
		match unsafe { uefi::file::read_file_reported(bs, root, firmware_name) } {
			uefi::file::FirmwareRead::Bytes(bytes) => return MediumRead::Bytes(bytes),
			uefi::file::FirmwareRead::Absent => return MediumRead::Absent,
			uefi::file::FirmwareRead::Failed => {
				firmware_had_it = true;
				arch::serial::write_str("loader: the firmware could not read ");
				arch::serial::write_str(name);
				arch::serial::write_str(" off the boot volume; reading the medium as FAT\n");
			}
		}
	} else {
		arch::serial::write_str("loader: the firmware did not mount the boot volume; reading it as FAT\n");
	}
	match read_from_fat_reported(bs, name.as_bytes()) {
		MediumRead::Bytes(bytes) => MediumRead::Bytes(bytes),
		MediumRead::Absent if firmware_had_it => MediumRead::Unreadable,
		other => other,
	}
}

// The kernel from the BOOT MEDIUM, checked against the manifest that medium carries.
//
// The system-volume branch checks its kernel against the volume's manifest; this is the other
// source, and it read its kernel unchecked - which is not a corner: it is the branch every machine
// with no system volume takes, and the one the test media take on all three architectures. A
// verified path nothing walks and an unverified one everything walks is worse than neither, because
// the milestone can be read as done.
//
// The medium's manifest is not the volume's copy. Its kernel is staged independently, so its digest
// is computed where it is staged - see `stage_boot_manifest`.
// WHAT WAS VERIFIED, SAID OUT LOUD AND NOT PUT IN `BootInfo`.
//
// The kernel does not need trust metadata: this is a loader-owned decision, taken before the
// kernel exists, and a field it could read would be a field something could later be tempted to
// re-derive a decision from. What the harness asserts is this line - the release the manifest
// names, the key that signed it, and the digest of the record itself, which is what makes two
// boots comparable.
//
// AND WHAT IT COVERED. One boot verifies up to three manifests and two of them are the same record
// read for different questions - the medium's, once to learn which volume it is paired with and
// again as cover for the kernel read off it - so the receipt was printed twice with matching
// digests and nothing saying the two lines were different answers.
fn announce_release(manifest: &bootproto::manifest::Manifest<'_>, subject: &str) {
	arch::serial::write_str("loader: signed manifest verified - release ");
	for byte in manifest.release {
		arch::serial::write_byte(*byte);
	}
	arch::serial::write_str(", key ");
	write_hex32(manifest.key_id);
	arch::serial::write_str(", manifest ");
	let digest = bootproto::sha256::digest(manifest.payload());
	for byte in &digest[..8] {
		write_hex8(*byte);
	}
	arch::serial::write_str(" (");
	arch::serial::write_str(subject);
	arch::serial::write_str(")\n");
}

fn write_hex8(byte: u8) {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	arch::serial::write_byte(HEX[(byte >> 4) as usize]);
	arch::serial::write_byte(HEX[(byte & 0xf) as usize]);
}

fn write_hex32(value: u32) {
	for shift in [24, 16, 8, 0] {
		write_hex8((value >> shift) as u8);
	}
}

// The boot medium's SIGNED manifest, verified, or None when it carries none.
//
// ONE PLACE, because two call sites read it: the kernel, and the system volume image the medium
// hands the kernel as a module. Reading it twice would be two chances to check different things
// against different manifests.
// A variable firmware defines, or the fact that it does not. `absent` is not a zero: firmware
// without these variables has no Secure Boot at all, which is a different machine from one that has
// it switched off.
fn write_variable(value: Option<u8>) {
	match value {
		Some(byte) => arch::serial::write_byte(b'0' + (byte % 10)),
		None => arch::serial::write_str("absent"),
	}
}

fn boot_medium_manifest(bs: *mut BootServices, root: Option<*mut uefi::FileProtocol>) -> Option<bootproto::manifest::Manifest<'static>> {
	// ABSENT AND UNREADABLE ARE DIFFERENT ANSWERS HERE, and the caller's fallback is why: `None`
	// sends it to the v1 checksum manifest, which is the arm a medium with no signed manifest at all
	// is entitled to. A medium whose signed manifest is THERE and could not be read is not entitled
	// to it: the strongest statement about this boot exists on the medium and this loader could not
	// establish it. See `MediumRead`.
	let signed = match read_boot_file_reported(bs, root, "etc/boot.manifest2") {
		MediumRead::Bytes(bytes) => bytes,
		MediumRead::Absent => return None,
		MediumRead::Unreadable => panic!("loader: the boot medium's signed manifest is present and could not be read - refusing rather than falling back to the checksum one"),
	};
	let mut scratch = alloc::vec::Vec::new();
	if scratch.try_reserve_exact(bootproto::manifest::DOMAIN.len() + signed.len()).is_err() {
		panic!("loader: no room to verify the boot medium's signed manifest");
	}
	scratch.resize(bootproto::manifest::DOMAIN.len() + signed.len(), 0);
	let expected = trust::Expected::medium();
	let Some(manifest) = trust::verify_for(signed, &expected, &mut scratch) else {
		panic!("loader: the boot medium's signed manifest was refused - see the line above");
	};
	Some(manifest)
}

fn read_verified_kernel_from_boot_medium(bs: *mut BootServices, root: Option<*mut uefi::FileProtocol>) -> &'static [u8] {
	let bytes = read_boot_file(bs, root, KERNEL_FILE).expect("loader: cannot read kernel");
	// THE SIGNED MANIFEST WHEREVER THE MEDIUM HAS ONE, as on the system volume, and for the same
	// reason: the check happens before these bytes are parsed as an ELF or copied anywhere.
	if let Some(manifest) = boot_medium_manifest(bs, root) {
		if !blockio::covered_by(&manifest, bootproto::manifest::KIND_KERNEL, KERNEL_FILE.as_bytes(), bytes) {
			panic!("loader: the kernel is not what the boot medium's SIGNED manifest records");
		}
		announce_release(&manifest, "the boot medium's kernel");
		return bytes;
	}
	// The third of the same fallback: a signed manifest that is ABSENT, on the medium this loader
	// itself came off. Whether this build takes it is the same profile decision, said the same way.
	if !trust::IS_TEST_TRUST {
		panic!("loader: the boot medium carries no SIGNED manifest, and this build authenticates what it boots - refusing rather than falling back to the text one");
	}
	let Some(manifest) = read_boot_file(bs, root, "etc/boot.manifest") else {
		panic!("loader: the boot medium has a kernel and no manifest of either kind - refusing to boot from it");
	};
	if !blockio::digests_ok(manifest, KERNEL_FILE.as_bytes(), bytes) {
		panic!("loader: the kernel does not match etc/boot.manifest on the boot medium");
	}
	arch::serial::write_str("loader: THIS KERNEL IS NOT AUTHENTICATED - the boot medium carries no signed manifest, and this build accepts the checksum one\n");
	bytes
}

// The boot medium, chosen once.
static mut BOOT_MEDIUM: Option<blockio::FirmwareDisk> = None;
// The handle the firmware said this image was loaded from, when it named one this loader can also
// reach as a block device. Set once, before anything reads a medium.
static mut BOOT_DEVICE: Option<Handle> = None;

// Visit the boot medium: the one already chosen, or every device until one answers - and then that
// one is the boot medium for every later read.
//
// `read_from_fat` used to take the first FAT volume on which EACH NAME was found, one file at a
// time, so a machine without a firmware-mounted root could take its kernel from one stick and its
// bootstrap files from another. The medium is chosen once and then read from; if it does not carry
// a file, no other medium is asked for it.
//
// AND IT IS CHOSEN BY IDENTITY WHERE THE FIRMWARE STATES ONE. The latch used to happen only when
// the visitor said "found it", so a search that MISSED chose nothing and the next read paid the
// whole enumeration again - and the choice, when it did happen, was "the first FAT volume that
// happened to carry the file being asked for", which on a machine with a stick in it is a guess
// about which medium this system booted from. `EFI_LOADED_IMAGE_PROTOCOL` names the device; a
// handle that matches it IS the boot medium whatever it does or does not hold.
//
// The content scan below stays for the firmware that mounts the ESP without exposing it as a block
// device - U-Boot does exactly that, and a `LoadedImage` handle with no Block I/O on it matches
// nothing here.
fn with_boot_medium(bs: *mut BootServices, mut visit: impl FnMut(blockio::FirmwareDisk) -> Visit) {
	if let (None, Some(want)) = (unsafe { BOOT_MEDIUM }, unsafe { BOOT_DEVICE }) {
		unsafe {
			blockio::each_disk(bs, |disk| {
				if disk.handle() == want {
					BOOT_MEDIUM = Some(disk);
					true
				} else {
					false
				}
			});
		}
	}
	if let Some(disk) = unsafe { BOOT_MEDIUM } {
		visit(disk);
		return;
	}
	// THE LATCH IS THE MOUNT, NOT THE FIND, and the two are different answers so the visitor gives
	// two.
	//
	// It latched on `true`, which every visitor returned only when the file it wanted was read - so
	// a medium that mounted cleanly and did not carry THAT file was forgotten, and the next read
	// walked and re-mounted every disk in the machine again. Worse than the cost: with two FAT media
	// present, each file was independently taken from whichever disk happened to have it, which is a
	// boot assembled from two systems - the thing this function's own comment says it exists to
	// stop. A medium that mounts is the medium; what it does or does not hold comes after.
	unsafe {
		blockio::each_disk(bs, |disk| match visit(disk) {
			Visit::NotAMedium => false,
			Visit::Mounted { done } => {
				BOOT_MEDIUM = Some(disk);
				done
			}
		});
	}
}

// What a `with_boot_medium` visitor found on one disk.
#[derive(Clone, Copy)]
pub(crate) enum Visit {
	// Not a FAT volume, or not one this loader can mount. Keep looking, and remember nothing.
	NotAMedium,
	// It mounted. This IS the boot medium from here on, whether or not the file being looked for was
	// on it - `done` only says whether the scan has anything left to do.
	Mounted { done: bool },
}

// Read a file from a FAT medium the firmware did not mount for us.
//
// The ESP is read through Simple File System, which the firmware provides, so this is not the
// ordinary path - it is the one for a medium the firmware declines to mount, which is what an
// installer or rescue stick looks like on firmware that only understands its own disk. Read-only,
// like everything else here.
pub(crate) fn read_from_fat(bs: *mut BootServices, path: &[u8]) -> Option<&'static [u8]> {
	match read_from_fat_reported(bs, path) {
		MediumRead::Bytes(bytes) => Some(bytes),
		_ => None,
	}
}

// WHAT A READ OF THE BOOT MEDIUM FOUND. THREE ANSWERS, because two of them decide opposite things.
//
// A manifest that is ABSENT is a medium that carries none - a rescue stick, the test medium - and
// the profile decides what to do about that. A manifest that is THERE AND UNREADABLE is a medium
// that says which product, which volume and which digests it wants, and cannot be believed: a
// corrupt FAT, a failing disk, a firmware that opens the file and cannot read it. Collapsing the
// second into the first is what let a damaged medium take the v1 checksum fallback, hand a package
// over unauthenticated, or name no system volume and fall back to the first LiberFS one it found -
// which is the pairing turned off by a broken sector rather than by a decision.
pub(crate) enum MediumRead {
	Bytes(&'static [u8]),
	// The medium was read and this file is not on it.
	Absent,
	// The medium is there and could not be read, or was read and could not be retained.
	Unreadable,
}

// The same read, with that distinction kept. See `MediumRead`.
pub(crate) fn read_from_fat_reported(bs: *mut BootServices, path: &[u8]) -> MediumRead {
	let mut found = MediumRead::Unreadable;
	with_boot_medium(bs, |disk| {
		let Some(mut fs) = fat::FatFs::mount_read_only(disk) else { return Visit::NotAMedium };
		// MOUNTED IS THE ANSWER, whether or not this file is on it. A FAT medium missing one file is
		// still this boot's medium, and looking for that file on a second one is how a system gets
		// assembled out of two.
		found = match fs.read_file(path) {
			// AND A READ THAT COULD NOT BE RETAINED IS NOT A FILE THAT IS NOT THERE. The bytes exist
			// and the loader has nowhere to put them, which is the same answer a failing disk gives:
			// this medium's manifest could not be established.
			Ok(bytes) => retain(bs, &bytes).map_or(MediumRead::Unreadable, MediumRead::Bytes),
			Err(fat::FsError::NotFound) => MediumRead::Absent,
			Err(_) => MediumRead::Unreadable,
		};
		Visit::Mounted { done: true }
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
	// The same handle `with_boot_medium` matches against: one question asked of the firmware, one
	// answer, used by both readers of this medium.
	let device = unsafe { uefi::disk::loaded_image_device(bs, image_handle) }?;

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
