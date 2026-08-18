// riscv64 loader backend: place the kernel and enter its own boot stub.
//
// Like aarch64 (and unlike x86, where the loader also builds the kernel's page tables), the
// riscv64 kernel carries a position-independent boot stub that builds its Sv39 tables + higher half
// itself and reads its device inventory from the device tree - exactly the entry state QEMU's `-kernel` load over OpenSBI
// produces (S-mode, paging off, a0 = boot hartid, a1 = DTB). So this backend mirrors
// that state: it loads each PT_LOAD segment at its physical (link) address, finds the
// firmware's flattened device tree and the boot hart id, exits boot services, turns
// paging off (SATP = 0), and jumps to the kernel entry (`_start`) with the hart id in
// a0 and the DTB pointer in a1. No PAGE TABLES are built here; a BootInfo is, because the modules
// handed over with the kernel have nowhere else to be described.
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

use bootproto::{BootInfo, Framebuffer};

use crate::{PAGE_SIZE, align_down};
use uefi::{self, BootServices, Guid, Handle, SystemTable};

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
pub fn hand_off(bs: *mut BootServices, image_handle: Handle, system_table: *mut SystemTable, root: Option<*mut uefi::FileProtocol>, kernel: &[u8], reserved: &crate::ReservedKernel) -> ! {
	let staged = stage_kernel(bs, kernel);
	serial::write_str("loader: kernel ELF staged clear of the firmware\n");

	// REPORTED, NOT REFUSED, and the difference is the point.
	//
	// The other two backends write the kernel to `p_paddr` while the firmware is still up, so a
	// span they do not own is somebody else's memory and the placement must stop. This backend
	// stages the image and copies it AFTER `ExitBootServices`, when the firmware's allocator no
	// longer has an opinion and `check_clear_of_destination` has established that nothing still
	// needed lies in the way - which is the check that matters here. What an incomplete reservation
	// tells this path is that the firmware handed some of the destination to something else
	// earlier, which is worth knowing when a boot goes wrong on an unfamiliar machine.
	if !reserved.is_complete() {
		serial::write_str("loader: NOTE - part of the kernel's physical destination could not be reserved before staging; the post-EBS copy proceeds and the overlap check below is what guards it\n");
	}

	// The boot hart id (a0) and the flattened device tree (a1) - the same pair OpenSBI
	// hands the kernel on a `-kernel` boot. The kernel scans memory for the DTB if the
	// firmware exposes none, and treats hart 0 as the boot hart if the protocol is
	// absent.
	let hartid = boot_hartid(bs);
	let dtb = find_dtb(system_table);
	serial::write_str(if dtb != 0 { "loader: device tree found\n" } else { "loader: no device-tree table (kernel will scan)\n" });

	// Build a BootInfo carrying the DTB pointer and the GOP framebuffer, so the kernel
	// draws its earliest boot log to the display pixel-by-pixel (QEMU virt has no VGA;
	// the `-kernel` path programs ramfb itself instead). The kernel entry tells a
	// BootInfo from a raw DTB pointer (the OpenSBI `-kernel` entry state) by the magic.
	// The bootstrap set, published as modules exactly as on x86_64 and aarch64. Without it this
	// architecture has no userspace under UEFI: its programs used to come from an archive the
	// runner laid in RAM and the kernel found by scanning for a magic number, which the loader
	// path neither lays down nor needs (M0138c).
	// `main` has already reported WHERE the set came from, and there are three possible answers.
	// Saying "from the system volume" here claimed the first of them unconditionally, so a riscv64
	// boot that had actually assembled its set from the boot medium said both things, one after
	// the other, and the wrong one last.
	let init_pkg = match unsafe { crate::BOOTSTRAP } {
		Some(archive) => Some(archive),
		None => crate::read_boot_file(bs, root, crate::INIT_PKG_FILE),
	};
	let volume_pkg = root.and_then(|root| crate::read_file(bs, root, crate::VOLUME_PKG_FILE));
	let boot_info = build_boot_info(bs, dtb, init_pkg, volume_pkg);

	// Everything the placement will overwrite has to be somewhere else FIRST - checked while
	// the firmware can still print a diagnosis, because after the copy there is no firmware and
	// a mistake is a machine that stops saying anything at all.
	// The loader's own extent comes from the firmware while there is still firmware to ask.
	let loader = crate::loader_image_extent(bs, image_handle);
	check_clear_of_destination(&staged, boot_info, dtb, loader);

	// ExitBootServices is the last firmware call; after it no service may be used.
	// GIVE THE HEAP BACK before the map is taken. Everything the kernel receives is in pages of
	// its own by now; the arenas underneath are the loader's own working memory, and left alone
	// they reach the kernel as `MEM_BOOTLOADER` - which its frame allocator never seeds, so they
	// would be reserved for the system's whole life. The number is printed because it is the one
	// this milestone asked to be measured.
	let region_count = exit_boot_services(bs, image_handle, unsafe { (*(boot_info as *const BootInfo)).memmap as *mut bootproto::MemRegion });
	unsafe { (*(boot_info as *mut BootInfo)).memmap_len = region_count as u64 };

	// MASK SUPERVISOR INTERRUPTS, FIRST THING AFTER THE LAST FIRMWARE CALL.
	//
	// UEFI runs boot services with interrupts enabled, and on RISC-V it additionally leaves a
	// SUPERVISOR TIMER configured for delivery - so this is not an inherited-state worry, it is a
	// timer that is going to fire. Nothing here ever cleared `sstatus.SIE`, and the kernel installs
	// `stvec` well into its Rust entry path: from this line until then, a delivered interrupt
	// vectors through the firmware's `stvec`, whose handler this code is about to overwrite with the
	// kernel image. `place_and_enter` copies over exactly that memory.
	//
	// `csrci sstatus, 2` clears SIE (bit 1) and nothing else.
	unsafe { core::arch::asm!("csrci sstatus, 2", options(nomem, nostack, preserves_flags)) };

	// Now, and only now, put the kernel at its link addresses and enter it. The loader ran
	// under the firmware's identity map, so with paging off it keeps executing at the same
	// (physical) addresses through the copy and the jump.
	//
	// SAFETY: boot services are gone, and `check_clear_of_destination` has established that
	// this code, its stack, the staging table, the hand-off record and the device tree all lie
	// outside the range about to be written.
	unsafe { place_and_enter(&staged, hartid, boot_info) }
}

// Allocate and fill a `bootproto::BootInfo` (in retained LOADER_DATA) carrying the DTB
// pointer and the GOP framebuffer, returning its physical address. The kernel reads it
// through its own direct map, so `framebuffer.addr` is the PHYSICAL base (this backend
// builds no page tables). Only the fields the device-tree kernel path reads are set;
// the memmap / modules / rsdp / trampoline are x86-only.
fn build_boot_info(bs: *mut BootServices, dtb: u64, init_pkg: Option<&'static [u8]>, volume_pkg: Option<&'static [u8]>) -> u64 {
	let live_volume = unsafe { crate::LIVE_VOLUME };
	let fb = crate::locate_framebuffer(bs);
	serial::write_str(if fb.present { "loader: GOP framebuffer found\n" } else { "loader: no GOP framebuffer (serial-only boot log)\n" });
	let phys = crate::alloc_pages(bs, 1).expect("loader: cannot allocate BootInfo");
	// THE FIRMWARE'S MEMORY MAP, WHICH THIS ARCHITECTURE HANDED OVER AS NOTHING.
	//
	// `memmap: 0, memmap_len: 0` was written here and the kernel fell back to the device tree's
	// `/memory`. The Devicetree Specification is explicit that under UEFI the system memory map
	// comes from `GetMemoryMap()` and `/memory` is to be ignored, and the reason is not formal: the
	// EFI map carries runtime services code and data, ACPI NVS and reclaimable regions, unusable
	// memory, firmware reservations, loader allocations and MMIO apertures, none of which a
	// `/memory` node expresses. So the kernel could mark as usable, zero, and hand to a page table
	// or a userspace process memory the firmware still owns.
	//
	// The array is allocated here and FILLED after the last `GetMemoryMap`, because that call must
	// be the one immediately before `ExitBootServices` - see `exit_boot_services`. The DTB stays
	// what it is for: CPU and device topology.
	let regions_pages = (uefi::memory::MAX_REGIONS * core::mem::size_of::<bootproto::MemRegion>()).div_ceil(PAGE_SIZE as usize);
	let regions_phys = crate::alloc_pages(bs, regions_pages).expect("loader: cannot allocate region array");
	let framebuffer = if fb.present { Framebuffer { addr: fb.phys, width: fb.width, height: fb.height, pitch: fb.pitch, bpp: fb.bpp, red_shift: fb.red_shift, red_size: fb.red_size, green_shift: fb.green_shift, green_size: fb.green_size, blue_shift: fb.blue_shift, blue_size: fb.blue_size, _pad: [0; 2] } } else { unsafe { core::mem::zeroed() } };
	unsafe {
		// The page is allocated ONLY when there is a package to describe. `match (init_pkg,
		// alloc_pages(..))` evaluated the allocation either way, so a boot with no package leaked a
		// page - and the `(Some, None)` arm produced a `BootInfo` with `modules_len = 0`, which is
		// a boot that will not work reported as one that will.
		let module_page = init_pkg.and_then(|_| crate::alloc_pages(bs, 1));
		if init_pkg.is_some() && module_page.is_none() {
			panic!("loader: cannot allocate the module array for the bootstrap package");
		}
		let (modules, modules_len) = match (init_pkg, module_page) {
			(Some(bytes), Some(array)) => {
				let entries = array as *mut bootproto::Module;
				*entries = crate::make_module(bytes, crate::INIT_PKG_FILE, 0);
				let mut count = 1u64;
				if let Some(volume) = volume_pkg {
					*entries.add(count as usize) = crate::make_module(volume, crate::VOLUME_PKG_FILE, 0);
					count += 1;
				}
				// AND THE LIVE VOLUME. `main` reads `system-volume.img` on every architecture and
				// the kernel looks for it, but only x86 published it - so a live medium booted here
				// with no volume at all, on the two architectures whose kernels then had nothing to
				// mount. A page holds 4096/40 = 102 modules, so three fit with room over.
				if let Some(volume) = live_volume {
					serial::write_str("loader: live system volume handed over\n");
					*entries.add(count as usize) = crate::make_module(volume, crate::LIVE_VOLUME_FILE, 0);
					count += 1;
				}
				(array, count)
			}
			_ => (0, 0),
		};
		*(phys as *mut BootInfo) = BootInfo { magic: bootproto::MAGIC, version: bootproto::VERSION, _pad0: 0, hhdm_offset: 0, memmap: regions_phys, memmap_len: 0, modules, modules_len, framebuffer, fb_present: fb.present as u32, psci_conduit: bootproto::PSCI_NONE, rsdp: 0, smp_trampoline: 0, dtb };
	}
	phys
}

// A kernel image copied into scratch memory, waiting to be put where it belongs.
//
// ONE contiguous block covering the image's whole physical span, not one per segment. Per
// segment was the first attempt and it had a hole: each scratch was checked against its own
// destination and against nothing else, so a segment's scratch could sit inside a DIFFERENT
// segment's destination and the copy would eat its own source. A bigger image makes that more
// likely, which is why the ordinary kernel booted and the test kernel did not.
struct Staged {
	entry: u64,
	scratch: u64,
	dest_low: u64,
	dest_high: u64,
}

// Copy the kernel into SCRATCH memory rather than to its link address, and record where it has
// to go.
//
// This used to write straight to the link address, and on QEMU virt that address is where
// U-Boot itself is running: U-Boot's image occupies 0x8020_0000..~0x802F_7818 and the kernel is
// linked from 0x8020_0000. The two overlap almost exactly. `AllocateAddress` succeeded because
// this U-Boot does not reserve its own image, so the loader cheerfully overwrote the firmware it
// was still calling into - and the next firmware call, inside ExitBootServices, ran into
// whatever the kernel had put there.
//
// The symptom was a boot that produced no kernel output at all, ever, on this path. The
// loader's own prints kept working because they write to the UART directly and never enter
// U-Boot, which is exactly what made it look like the kernel had failed to start rather than
// like the firmware had been demolished underneath it.
fn stage_kernel(bs: *mut BootServices, kernel: &[u8]) -> Staged {
	let image = crate::elf::Elf::parse(kernel).expect("loader: kernel is not a valid riscv64 ELF64 executable");
	// ET_EXEC ONLY. The shared parser admits `ET_DYN` too, because the kernel loads position-
	// independent userspace binaries with it and the compatibility checker reads shared libraries -
	// but THIS loader computes no load bias and processes no relocations, so a PIE kernel would be
	// placed at its link addresses and jumped to unrelocated. Refused by name until a PIE kernel is
	// wanted, which is a change here rather than to the parser.
	assert!(image.image_type == crate::elf::ET_EXEC, "loader: the kernel image must be ET_EXEC");

	// The destination span first, from the headers alone - nothing is allocated until it is
	// known what the allocation has to avoid.
	let mut dest_low = u64::MAX;
	let mut dest_high = 0u64;
	for i in 0..image.segment_count() {
		let Some(ph) = image.segment(i) else { continue };
		if ph.p_type != crate::elf::PT_LOAD || ph.p_memsz == 0 {
			continue;
		}
		dest_low = dest_low.min(align_down(ph.p_paddr, PAGE_SIZE));
		// Checked here too, even though the shared parser has already refused a header that would
		// wrap: a guarantee that is not visible at the arithmetic is one the next reader has to go
		// and find, and this file is the one that then rounds and multiplies it.
		// Checked here too, even though the shared parser has already refused a header that would
		// wrap: a guarantee that is not visible at the arithmetic is one the next reader has to go
		// and find, and this file is the one that then rounds and multiplies it. The parser is what
		// makes this branch unreachable; the panic is what says so if it ever is not.
		let end = ph.p_paddr.checked_add(ph.p_memsz).expect("the shared parser refuses a segment whose physical end wraps");
		dest_high = dest_high.max(end);
	}
	if dest_low == u64::MAX {
		panic!("loader: kernel image has no loadable segments");
	}
	let dest_high = dest_high.checked_add(PAGE_SIZE - 1).map(|v| v & !(PAGE_SIZE - 1)).expect("a page-rounded destination that wraps - the parser bounds the end that feeds this");
	let pages = ((dest_high - dest_low) / PAGE_SIZE) as usize;

	// Scratch that does not overlap the destination - `uefi::memory::staging_clear_of`, which is
	// where the rule and its reason now live, and where a mock firmware handing back addresses
	// INSIDE the destination can be made to test it. That case is this file's whole reason for
	// existing and no machine here produces it on demand.

	let scratch = uefi::memory::staging_clear_of(bs, pages, dest_low, dest_high).expect("loader: every staging allocation landed on the kernel's destination");
	// Zero the whole block first, so every BSS tail and inter-segment gap is already zero when
	// the single block copy places it.
	unsafe { core::ptr::write_bytes(scratch as *mut u8, 0, pages * PAGE_SIZE as usize) };
	for i in 0..image.segment_count() {
		let Some(ph) = image.segment(i) else { continue };
		if ph.p_type != crate::elf::PT_LOAD || ph.p_memsz == 0 {
			continue;
		}
		let Some(data) = image.segment_data(&ph) else { continue };
		unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), (scratch + (ph.p_paddr - dest_low)) as *mut u8, data.len()) };
	}
	Staged { entry: image.entry, scratch, dest_low, dest_high }
}

// Put the staged image where it belongs and enter the kernel. Nothing may call the firmware
// after this is entered - it does not return, and by its second instruction the firmware's own
// code may already be overwritten.
//
// # Safety
//
// ExitBootServices must have completed, and everything this needs - the stack it runs on, its
// own code, the staging block, and what the kernel is handed - must lie outside
// [dest_low, dest_high), which `check_clear_of_destination` establishes first.
unsafe fn place_and_enter(staged: &Staged, hartid: u64, boot_info: u64) -> ! {
	unsafe {
		core::ptr::copy_nonoverlapping(staged.scratch as *const u8, staged.dest_low as *mut u8, (staged.dest_high - staged.dest_low) as usize);
		// The kernel's text was just written as data; make the instruction fetch see it.
		core::arch::asm!("fence rw, rw", "fence.i", options(nostack, preserves_flags));
		// Mirror the OpenSBI `-kernel` entry state: paging off, hart id in a0, the hand-off
		// pointer in a1.
		core::arch::asm!(
			"csrw satp, zero",
			"sfence.vma",
			"jr {entry}",
			entry = in(reg) staged.entry,
			in("a0") hartid,
			in("a1") boot_info,
			options(noreturn),
		);
	}
}

// Six fixed objects plus the module array and its entries. A hand-off with more modules than this
// stops rather than silently checking a prefix of them.
const MAX_CHECKED_RANGES: usize = 16;

// Refuse to proceed if anything the final copy needs lives in the range that copy overwrites.
// A wrong answer here is a machine that stops with no output, which is the failure this whole
// change exists to remove - so it is said out loud instead.
fn check_clear_of_destination(staged: &Staged, boot_info: u64, dtb: u64, loader: Option<(u64, u64)>) {
	// RANGES, not addresses. Every one of these was checked by a single byte - the staged image by
	// its first, while it is the largest object in play - so an object whose START was outside the
	// destination and whose BODY was inside passed. U-Boot does not reserve its own image in the
	// firmware memory map, so the pages `retain()` hands back can be inside the future kernel
	// destination, and the post-EBS copy would then overwrite the bootstrap data before the kernel
	// reads it.
	let span = staged.dest_high - staged.dest_low;
	let overlaps = |start: u64, len: u64| -> bool {
		let end = start.saturating_add(len.max(1));
		start < staged.dest_high && staged.dest_low < end
	};
	let sp: u64;
	unsafe { core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
	// The loader's stack, from the current pointer up to the page it lives on: the whole ACTIVE
	// stack, not the one word `sp` names.
	let stack_top = (sp | (crate::PAGE_SIZE - 1)) + 1;
	// THE LOADER'S REAL EXTENT, from `EFI_LOADED_IMAGE_PROTOCOL`. One page around
	// `place_and_enter` stood in for this, and the loader is bigger than one page - so the check
	// most directly about "the loader is about to overwrite itself" was the loosest of them all.
	let (loader_base, loader_size) = loader.unwrap_or_else(|| {
		serial::write_str("loader: WARNING - EFI_LOADED_IMAGE_PROTOCOL gave no extent; checking one page around the copy routine instead\n");
		(place_and_enter as *const () as u64, crate::PAGE_SIZE)
	});
	let mut ranges: [(u64, u64, &str); MAX_CHECKED_RANGES] = [(0, 0, ""); MAX_CHECKED_RANGES];
	ranges[0] = (loader_base, loader_size, "the loader's own image");
	ranges[1] = (sp, stack_top - sp, "the loader's stack");
	ranges[2] = (staged as *const Staged as u64, core::mem::size_of::<Staged>() as u64, "the staging descriptor");
	// The staged image is `span` bytes - the same length the copy will move.
	ranges[3] = (staged.scratch, span, "the staged kernel image");
	ranges[4] = (boot_info, core::mem::size_of::<bootproto::BootInfo>() as u64, "the hand-off record");
	// The device tree's real length, off its own header: bytes 4..8 are `totalsize`, big-endian.
	ranges[5] = (dtb, dtb_total_size(dtb), "the device tree");
	let mut extra = 6;

	// AND EVERYTHING `BootInfo` POINTS AT, rather than a list of globals somebody remembered.
	//
	// The list used to be `BOOTSTRAP` and `LIVE_VOLUME`, which are two of the objects `retain()`
	// hands back through `AllocateAnyPages` - the allocations U-Boot may satisfy from inside the
	// kernel's destination. It missed the MODULE DESCRIPTOR ARRAY, a separate retained page that
	// `BootInfo.modules` points at and the kernel reads after the copy, and it would miss the next
	// thing retained for the same reason: a remembered list is right until something is added.
	//
	// Being in `BootInfo` is what makes an object one the kernel still has to read, so that is what
	// is walked: the record, the array over `modules_len` entries, and every module's own bytes -
	// which covers the bootstrap archive, both packages and the live image without naming any.
	let info = boot_info as *const bootproto::BootInfo;
	let (modules, modules_len) = unsafe { ((*info).modules, (*info).modules_len) };
	if modules != 0 && modules_len != 0 {
		ranges[extra] = (modules, modules_len * core::mem::size_of::<bootproto::Module>() as u64, "the module descriptor array");
		extra += 1;
		for index in 0..modules_len {
			if extra == MAX_CHECKED_RANGES {
				serial::write_str("loader: FATAL - more hand-off modules than the overlap check can hold\n");
				halt();
			}
			let module = unsafe { &*(modules as *const bootproto::Module).add(index as usize) };
			ranges[extra] = (module.addr, module.size, "a hand-off module the kernel has yet to read");
			extra += 1;
		}
	}
	for (start, len, what) in ranges.iter().take(extra) {
		if *start != 0 && overlaps(*start, *len) {
			serial::write_str("loader: FATAL - ");
			serial::write_str(what);
			serial::write_str(" lies where the kernel must be placed\n");
			halt();
		}
	}
}

// The device tree's own length, from its header (`totalsize`, big-endian at byte 4). One page when
// the magic is not there, which is the safe answer for something that may not be a DTB at all.
fn dtb_total_size(dtb: u64) -> u64 {
	if dtb == 0 {
		return 0;
	}
	let header = unsafe { core::slice::from_raw_parts(dtb as *const u8, 8) };
	if header[..4] != [0xd0, 0x0d, 0xfe, 0xed] {
		return crate::PAGE_SIZE;
	}
	u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as u64
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
	if uefi::is_error(status) { 0 } else { hartid as u64 }
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
// AND TRANSLATES THE MAP IT TAKES, which this threw away.
//
// The comment here said "aarch64 needs no translated map - the kernel reads RAM from the DTB", and
// that is the finding: under UEFI the device tree's `/memory` is not the system memory map. The
// final `GetMemoryMap` - the one whose key `ExitBootServices` consumes - is translated into
// `regions` in the same iteration, with no allocation between them, exactly as x86_64 does.
//
// Returns the region count, which the caller writes into the BootInfo after the exit: that is a
// store to plain memory and needs no firmware service.
fn exit_boot_services(bs: *mut BootServices, image_handle: Handle, regions: *mut bootproto::MemRegion) -> usize {
	let mut map_size = 0usize;
	let mut key = 0usize;
	let mut desc_size = 0usize;
	let mut desc_ver = 0u32;
	let status = unsafe { ((*bs).get_memory_map)(&mut map_size, core::ptr::null_mut(), &mut key, &mut desc_size, &mut desc_ver) };
	// THE SIZING CALL'S ANSWER MATTERS. It was ignored, so `desc_size` was whatever the firmware
	// left there - and a zero divides a few lines later. The expected status is
	// `EFI_BUFFER_TOO_SMALL`; anything else means the map was not described.
	if status != uefi::STATUS_BUFFER_TOO_SMALL || desc_size == 0 || desc_size < core::mem::size_of::<uefi::MemoryDescriptor>() {
		panic!("loader: the firmware did not describe its memory map");
	}
	let cap = map_size + desc_size * 16;
	let buf = crate::alloc_pages(bs, cap.div_ceil(PAGE_SIZE as usize)).expect("loader: cannot allocate memory map buffer") as *mut uefi::MemoryDescriptor;
	// GIVE THE HEAP BACK, and do it AFTER the buffer above rather than before it.
	//
	// The arenas are the loader's own working memory, and left alone they reach the kernel as
	// `MEM_BOOTLOADER` - which its frame allocator never seeds, so they would be reserved for the
	// system's whole life. The number is printed because it is the one this milestone asked to be
	// measured.
	//
	// The ORDER is a precaution rather than a fix for anything observed: freeing megabytes and then
	// asking the firmware for a buffer invites `AllocateAnyPages` to hand back part of what was just
	// freed, and on a port that copies the kernel over a fixed physical span after
	// `ExitBootServices` that placement would matter. Freed after the last allocation, nothing the
	// firmware places afterwards can be in freed memory, because it places nothing.
	{
		let freed = crate::heap::release(bs) / 1024;
		serial::write_str("loader: returned ");
		crate::serial_write_usize(freed);
		serial::write_str(" KiB of loader heap\n");
	}
	loop {
		let mut size = cap;
		let status = unsafe { ((*bs).get_memory_map)(&mut size, buf, &mut key, &mut desc_size, &mut desc_ver) };
		if uefi::is_error(status) {
			panic!("get_memory_map failed");
		}
		// The heap lives on firmware pages, so it stops being usable exactly here. Retiring it
		// makes a later allocation fail loudly instead of handing out memory the loader no
		// longer owns.
		crate::heap::retire();
		// And the firmware's console with it: after this call `ConOut` points at memory the loader
		// no longer owns, so every later diagnostic goes to the built-in UART.
		crate::console::release();
		// FATAL, not silent, and said HERE because this is where there is a serial port to say it
		// on: `translate_map` refuses a map with more regions than the boot protocol carries rather
		// than truncating it, and a map that looks complete and is missing its tail is the worst
		// available failure for the one structure that says which RAM exists.
		let Some(count) = uefi::memory::translate_map(buf, size, desc_size, regions) else {
			serial::write_str("loader: FATAL - the firmware memory map has more regions than the boot protocol carries\n");
			panic!("memory map larger than MAX_REGIONS");
		};
		let status = unsafe { ((*bs).exit_boot_services)(image_handle, key) };
		if !uefi::is_error(status) {
			return count;
		}
		// ONLY a stale map key is worth retrying. This looped on every error forever, which is the
		// least informative possible response to a firmware saying something else is wrong.
		if status != uefi::STATUS_INVALID_PARAMETER {
			panic!("loader: ExitBootServices refused");
		}
		// The map changed; retry without allocating.
	}
}
