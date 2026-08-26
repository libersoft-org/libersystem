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

use crate::{align_down, alloc_pages, alloc_scratch_pages};
use paging::{HHDM_OFFSET, PAGE_2MB, PAGE_SIZE, PageTables};
use uefi::{self, BootServices, Handle, SystemTable};

// The init/volume package filenames on the boot volume (the x86 loader reads them and
// hands the kernel their bytes as boot-protocol modules; the aarch64 kernel embeds them).
use crate::INIT_PKG_FILE;
use crate::VOLUME_PKG_FILE;
// The live medium's system volume: a LiberFS image the running system copies into memory, so a
// LiveCD needs no disk and never writes to the medium it booted from. Passed straight through as
// a module; the kernel hands it to the storage service, which is where it becomes a volume.

// Round `v` up to a multiple of `align` (a power of two), saturating rather than wrapping.
//
// It was `(v + align - 1) & !(align - 1)`, which WRAPS to a small number for a `v` near the top of
// the address space - and both callers feed it a firmware-reported address plus a firmware-reported
// size. A wrapped ceiling is a map built over the wrong interval, which is the worst answer of the
// three available; saturating gives the largest representable one, and every caller then compares
// it against the direct map's ceiling.
fn align_up(v: u64, align: u64) -> u64 {
	v.checked_add(align - 1).map_or(u64::MAX & !(align - 1), |sum| sum & !(align - 1))
}

// The kernel stack the loader hands over (in pages of 4 KiB): 128 KiB.
const STACK_PAGES: usize = 32;

// Upper bound on memory-map regions translated into the boot protocol.
// The boot protocol's region ceiling, which lives with the translation that enforces it.
use uefi::memory::MAX_REGIONS;

// Upper bound on kernel PT_LOAD segments.
const MAX_SEGMENTS: usize = 16;

// Halt the core (panic path): interrupts off, hlt forever. panic=abort, no unwind.
pub fn halt() -> ! {
	// Interrupts really off, which the comment above has always claimed and the code did not do: a
	// bare `hlt` wakes on the next interrupt and spins the loop forever.
	unsafe { asm!("cli", options(nomem, nostack, preserves_flags)) };
	loop {
		unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
	}
}

// Place the kernel and jump into it. Reads the init/volume packages off the boot
// volume (the kernel gets them as boot-protocol modules), loads the kernel ELF,
// gathers the framebuffer + RSDP, builds the page tables + BootInfo, snapshots the
// memory map, exits boot services, and switches to the kernel's page tables.
pub fn hand_off(bs: *mut BootServices, image_handle: Handle, system_table: *mut SystemTable, root: Option<*mut uefi::FileProtocol>, kernel: &[u8]) -> ! {
	// Already read in `main`, where it also supplied the bootstrap set: exactly one of the two is
	// present - a test medium carries the archive, a live medium the volume, and an installed
	// system neither, because its volume is on the disk.
	let live_volume = unsafe { crate::LIVE_VOLUME };
	// The archive assembled from the volume wins when there is one; the ESP copy is the fallback
	// for a machine whose system volume is missing or unreadable.
	let init_pkg: &[u8] = match unsafe { crate::BOOTSTRAP } {
		Some(archive) => archive,
		None => crate::read_verified_package(bs, root, INIT_PKG_FILE).expect("loader: cannot read init.pkg"),
	};
	// Optional. The system volume is a filesystem on the disk now, not an archive handed over at
	// boot, so a shipping image carries no `volume.pkg` at all - it survives only as the
	// kernel test suite's fixture. A machine without one boots exactly as before; the module is
	// simply absent, which is what the kernel's lookup already expects.
	// THROUGH THE SAME FALLBACK AS EVERYTHING ELSE. This read the factory archive ONLY through the
	// firmware's file protocol, while the kernel, the live volume and the bootstrap files all go
	// through `read_boot_file` - which falls back to reading the medium as FAT when the firmware
	// declines to mount it. On exactly that firmware, and only there, the module was silently
	// absent: a boot that looks identical and a test fixture that is not delivered, on the machines
	// this loader has a fallback for in the first place.
	let volume_pkg = crate::read_verified_package(bs, root, VOLUME_PKG_FILE);
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
	let stack_phys = unsafe { alloc_pages(bs, STACK_PAGES) }.expect("loader: cannot allocate kernel stack");
	let stack_top = HHDM_OFFSET + stack_phys + (STACK_PAGES as u64 * PAGE_SIZE);

	// Buffers the kernel keeps reading after hand-off: the BootInfo, the region
	// array, and the module array all live in LOADER_DATA (retained memory).
	let boot_info_phys = unsafe { alloc_pages(bs, 1) }.expect("loader: cannot allocate BootInfo");
	let regions_pages = (core::mem::size_of::<MemRegion>() * MAX_REGIONS).div_ceil(PAGE_SIZE as usize);
	let regions_phys = unsafe { alloc_pages(bs, regions_pages) }.expect("loader: cannot allocate region array");
	let modules_phys = unsafe { alloc_pages(bs, 1) }.expect("loader: cannot allocate module array");

	// Publish the loaded packages as modules. The volume archive is only there when the image
	// carries one.
	let modules = modules_phys as *mut Module;
	let mut module_count = 1usize;
	unsafe {
		*modules.add(0) = crate::make_module(init_pkg, INIT_PKG_FILE, HHDM_OFFSET);
		if let Some(volume) = volume_pkg {
			*modules.add(module_count) = crate::make_module(volume, VOLUME_PKG_FILE, HHDM_OFFSET);
			module_count += 1;
		}
		if let Some(volume) = live_volume {
			serial::write_str("loader: live system volume handed over\n");
			*modules.add(module_count) = crate::make_module(volume, crate::LIVE_VOLUME_FILE, HHDM_OFFSET);
			module_count += 1;
		}
	}

	// The highest physical address the HHDM and identity map must cover.
	//
	// ZERO IS A FAILURE, NOT A RESULT. `memory_top` answers 0 for every snapshot failure, and that
	// value was rounded and used: the HHDM and identity map were then built over an EMPTY interval,
	// so the loader handed the kernel a direct map covering nothing and carried on. That is the
	// fail-open shape this project has removed twice elsewhere - a machine with no RAM does not
	// exist, so 0 can only mean the map could not be taken.
	let top = unsafe { uefi::memory::memory_top(bs) };
	if top == 0 {
		panic!("loader: the firmware would not give a memory map, so there is no RAM ceiling to build the direct map over");
	}
	// AND IT HAS TO FIT UNDER THE KERNEL. `HHDM_MAX_PHYS` states where the fixed-offset direct map
	// runs into the kernel's own virtual range; a physical top above it is a machine this loader
	// cannot map with this offset, and saying so is the only honest answer. Checked BEFORE the
	// rounding, because the rounding is itself an addition that can wrap.
	if top > paging::HHDM_MAX_PHYS {
		panic!("loader: the firmware reports memory above the direct map's ceiling, which this loader cannot map at a fixed offset");
	}
	let Some(ram_top) = top.checked_add(PAGE_2MB - 1).map(|v| v & !(PAGE_2MB - 1)) else {
		panic!("loader: the memory ceiling cannot be rounded to a 2 MiB boundary without wrapping");
	};
	if ram_top > paging::HHDM_MAX_PHYS {
		panic!("loader: rounding the memory ceiling puts it above the direct map's ceiling");
	}

	// Build the page hierarchy: HHDM over all RAM, the framebuffer uncacheable,
	// a low identity map for the CR3 switch, and the kernel's segments.
	let mut tables = PageTables::new(bs).expect("loader: cannot allocate PML4");
	tables.map_hhdm(0, ram_top, false).expect("loader: HHDM map failed");
	tables.map_identity(ram_top).expect("loader: identity map failed");
	// ONE INTERVAL, AND THE ATTRIBUTES CORRECTED OVER IT.
	//
	// `map_hhdm(0, ram_top)` maps physical holes as if they were memory and takes no attributes
	// from the descriptors it covers, which is the finding this addresses in part. Mapping a hole
	// is inert - nothing reads an address the kernel's own memory map does not describe - but a
	// WRITE-BACK mapping laid over device memory is not: a speculative fetch or an evicted line
	// through it reaches a device, and this loader gives `PCD | PWT` only to the framebuffer.
	//
	// So the interval stays and everything in it that is NOT MEMORY is re-mapped uncacheable: the
	// MMIO descriptors, and the physical space no descriptor describes at all (UEFI-003, LDR-010).
	//
	// The interval rather than a per-descriptor map, because the contiguous map is load-bearing
	// twice: the kernel derives its `DIRECT_MAP_LIMIT` from the top of the same memory map and
	// bounds every firmware pointer against it. Leaving holes UNMAPPED would make that
	// bound describe addresses that fault; mapping them uncacheable keeps the bound exactly true
	// while removing the write-back mapping over things that are not memory, which is the part of
	// the finding that is an architecture violation rather than waste.
	map_non_memory_uncacheable(bs, &mut tables, ram_top);
	if fb.present {
		let fb_base = align_down(fb.phys, PAGE_2MB);
		let fb_end = align_up(fb.phys.saturating_add(fb.size), PAGE_2MB);
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
		(*boot_info).modules_len = module_count as u64;
		(*boot_info).framebuffer = fb.info;
		(*boot_info).fb_present = fb.present as u32;
		(*boot_info).psci_conduit = bootproto::PSCI_NONE;
		(*boot_info).rsdp = rsdp;
		(*boot_info).smp_trampoline = trampoline;
		(*boot_info).dtb = 0; // x86 uses ACPI, not a device tree.
	}

	// Snapshot the memory map and exit boot services. GetMemoryMap must be the
	// last firmware call before ExitBootServices, so the region translation (no
	// allocation) happens inline and the whole thing retries if the map changed.
	let region_count = finalize_and_exit(bs, image_handle, regions_phys as *mut MemRegion);
	// INTERRUPTS OFF THE MOMENT BOOT SERVICES ARE GONE, not later.
	//
	// UEFI runs with interrupts ENABLED, and the `cli` below sat inside the handoff asm - after this
	// store and after everything between. From `ExitBootServices` returning until then, the
	// firmware's handlers no longer exist and the kernel's IDT does not exist yet, so an interrupt
	// arriving in that window vectors through a table nobody owns. Nothing here needs interrupts;
	// closing the window costs one instruction.
	unsafe { asm!("cli", options(nomem, nostack, preserves_flags)) };
	unsafe { (*boot_info).memmap_len = region_count as u64 };

	// Boot services are gone. Switch to the kernel's page tables and jump to its
	// entry with a pointer to the BootInfo in RDI (SysV first argument).
	let boot_info_virt = HHDM_OFFSET + boot_info_phys;
	unsafe {
		asm!(
			// Already masked immediately after `ExitBootServices` - see above. Kept here because
			// this block must be reachable only with interrupts off and saying so twice is cheaper
			// than depending on the caller.
			"cli",
			"mov cr3, {cr3}",
			"mov rsp, {stack}",
			"mov rdi, {info}",
			// THE SysV STACK ALIGNMENT the compiler built `kmain` for. `stack_top` is page-aligned,
			// so `rsp % 16 == 0` here - but `kmain` is an ordinary `extern "C" fn` and the compiler
			// emits it expecting the state a CALL leaves: `rsp % 16 == 8` at the first instruction,
			// the return address accounting for the difference. Every aligned stack access it emits
			// was off by eight, and it works today only because nothing on that path spills to an
			// aligned slot.
			//
			// A pushed return address is what a `call` would have left, and it doubles as the
			// honest answer to "what happens if the kernel returns": it cannot, and the address
			// pushed is this `ud2`.
			"lea rax, [rip + 2f]",
			"push rax",
			"jmp {entry}",
			"2:",
			"ud2",
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
	// ONE PLAN, CHECKED BEFORE ANYTHING IS COPIED (LDR-011). This was a loop of two `assert!`s here,
	// a different pair of rules in the aarch64 backend and none in riscv64, and no backend asked the
	// two questions that need asking wherever an image is loaded: whether two segments claim the
	// same page, and whether the entry point lands inside a segment that was loaded and is
	// executable. `bootproto::elf::load_plan` is those rules in one place; `LoadRules` is which of
	// them this backend is entitled to demand.
	//
	// ET_EXEC: the shared parser admits `ET_DYN` too, because the kernel loads position-independent
	// userspace binaries with it and the compatibility checker reads shared libraries - but THIS
	// loader computes no load bias and processes no relocations, so a PIE kernel would be placed at
	// its link addresses and jumped to unrelocated.
	//
	// Alignment: this backend allocates from `p_memsz`, copies to the segment's own address and maps
	// from `align_down(..)`, all of which are wrong by the page offset if a LOAD segment does not
	// start on a page. ELF only requires `p_vaddr = p_offset (mod p_align)`, which the parser
	// checks for every image.
	//
	// W^X: `map_kernel_segment` derives `WRITABLE` and `NX` from the flags independently, so a
	// writable-and-executable segment would be mapped read-write-execute under a comment claiming
	// W^X. The x86_64 kernel has no such segment - unlike the aarch64 and riscv64 kernels, whose
	// boot stubs are one `RWE` segment by construction, which is why this backend asks for the rule
	// and those two do not.
	let plan = bootproto::elf::load_plan(&image, bootproto::elf::LoadRules::kernel(PAGE_SIZE));
	let plan = match plan {
		Ok(plan) => plan,
		Err(why) => panic!("loader: the kernel image is not loadable: {why:?}"),
	};
	assert!(plan.segments <= MAX_SEGMENTS, "loader: more kernel LOAD segments than this backend records");
	let mut count = 0usize;
	for i in 0..image.segment_count() {
		let Some(ph) = image.segment(i) else { continue };
		if ph.p_type != crate::elf::PT_LOAD || ph.p_memsz == 0 {
			continue;
		}
		let pages = ph.p_memsz.div_ceil(PAGE_SIZE);
		// ANYWHERE THE FIRMWARE LIKES, which is why this backend takes no `ReservedKernel`: it maps
		// `p_vaddr` to whatever physical pages it is given and never writes to `p_paddr`, so the
		// reservation is a claim it neither needs nor can be hurt by. The two backends that place
		// AT the link address are the ones that must refuse a span they do not own.
		let phys = unsafe { alloc_pages(bs, pages as usize) }.expect("loader: cannot allocate kernel segment");
		unsafe {
			core::ptr::write_bytes(phys as *mut u8, 0, (pages * PAGE_SIZE) as usize);
			if let Some(data) = image.segment_data(&ph) {
				core::ptr::copy_nonoverlapping(data.as_ptr(), phys as *mut u8, data.len());
			}
		}
		out[count] = KernelSegment { virt: align_down(ph.p_vaddr, PAGE_SIZE), phys, pages, writable: ph.p_flags & crate::elf::PF_W != 0, executable: ph.p_flags & crate::elf::PF_X != 0 };
		count += 1;
	}
	(plan.entry, count)
}

// Build a boot-protocol module for a loaded package: its HHDM address, size, and
// NUL-padded name.
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
	let g = unsafe { crate::locate_framebuffer(bs) };
	if !g.present {
		return FbResult { info: unsafe { core::mem::zeroed() }, phys: 0, size: 0, present: false };
	}
	let info = Framebuffer { addr: HHDM_OFFSET + g.phys, width: g.width, height: g.height, pitch: g.pitch, bpp: g.bpp, red_shift: g.red_shift, red_size: g.red_size, green_shift: g.green_shift, green_size: g.green_size, blue_shift: g.blue_shift, blue_size: g.blue_size, _pad: [0; 2] };
	FbResult { info, phys: g.phys, size: g.size, present: true }
}

// Scan the firmware configuration table for the ACPI 2.0 (then 1.0) RSDP,
// returning its physical address (0 if none).
fn find_rsdp(system_table: *mut SystemTable) -> u64 {
	let count = unsafe { (*system_table).number_of_table_entries };
	let entries = unsafe { (*system_table).configuration_table };
	// Firmware with no configuration tables may publish a null pointer with a zero count, and
	// `entries.add(i)` on null is undefined even when the loop never runs. Nothing to find either
	// way.
	if entries.is_null() || count == 0 {
		return 0;
	}
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
	if uefi::is_error(status) { 0 } else { addr }
}

// Re-map every MMIO descriptor below `ram_top` as uncacheable, over the contiguous HHDM.
//
// Best effort by design: a failure here leaves the write-back mapping the HHDM already made, which
// is what this boot had before, so it says so and carries on rather than refusing to boot over an
// attribute.
fn map_non_memory_uncacheable(bs: *mut BootServices, tables: &mut PageTables, ram_top: u64) {
	let Some((buf, pages, map_size, desc_size)) = (unsafe { uefi::memory::memory_map_snapshot(bs) }) else {
		// NO MAP AT ALL IS A REFUSAL. The loader knows it is about to hand the kernel a direct map
		// it could not check, and "no worse than the bug" is a reason not to panic over a cosmetic
		// attribute - MMIO mapped write-back is not cosmetic. It is a device seeing stale writes and
		// a CPU seeing stale reads, at a point where nothing can report it.
		panic!("loader: the firmware would not give a memory map, so the MMIO ranges inside the direct map cannot be checked");
	};
	let entries = map_size / desc_size;
	for i in 0..entries {
		let d = unsafe { &*((buf as *const u8).add(i * desc_size) as *const uefi::MemoryDescriptor) };
		if d.ty != uefi::MEMORY_MAPPED_IO && d.ty != uefi::MEMORY_MAPPED_IO_PORT_SPACE {
			continue;
		}
		let Some(bytes) = d.page_count.checked_mul(PAGE_SIZE) else { continue };
		let base = align_down(d.phys_start, PAGE_2MB);
		let end = align_up(d.phys_start.saturating_add(bytes), PAGE_2MB);
		// Only what the direct map actually covers. Anything above `ram_top` was never mapped.
		if base >= ram_top || end <= base {
			continue;
		}
		let end = end.min(ram_top);
		if tables.map_hhdm(base, end - base, true).is_none() {
			// AND NAME THE RANGE. A warning that does not say which physical range failed cannot be
			// acted on by the person reading the boot log, which is the only reader it has.
			//
			// A single range that cannot be re-mapped stays a warning rather than a refusal: the
			// loader knows exactly which range is wrong and can say so, and refusing the whole boot
			// over one device window is a worse trade than a machine that boots and reports it.
			serial::write_str("loader: WARNING - could not mark MMIO ");
			serial::write_hex(base);
			serial::write_str("..");
			serial::write_hex(end);
			serial::write_str(" uncacheable in the direct map; it stays write-back\n");
		}
	}

	// AND THE HOLES (UEFI-003). A physical range no descriptor describes is not memory and not a
	// device: it is nothing, and the direct map covered it write-back because the map is one
	// interval. A write-back mapping over absent space is the same class of wrongness as one over
	// device space - a speculative fetch or an evicted line through it is an access to nothing,
	// which on some machines aborts and on others silently reads garbage - and it is an
	// architecture violation outright on AArch64.
	//
	// Uncacheable rather than unmapped, for the reason the caller states: the kernel's direct-map
	// bound is one interval derived from the same map, and unmapping inside it would make that
	// bound describe addresses that fault.
	//
	// A 2 MiB frame that any descriptor touches at all is left alone: the map is 2 MiB granular and
	// a page that is part memory must stay memory.
	let mut base = 0u64;
	while base < ram_top {
		let frame_end = base.saturating_add(PAGE_2MB);
		let mut touched = false;
		for i in 0..entries {
			let d = unsafe { &*((buf as *const u8).add(i * desc_size) as *const uefi::MemoryDescriptor) };
			let Some(bytes) = d.page_count.checked_mul(PAGE_SIZE) else { continue };
			let start = d.phys_start;
			let end = d.phys_start.saturating_add(bytes);
			if start < frame_end && base < end {
				touched = true;
				break;
			}
		}
		if !touched && tables.map_hhdm(base, PAGE_2MB, true).is_none() {
			serial::write_str("loader: WARNING - could not mark the hole at ");
			serial::write_hex(base);
			serial::write_str(" uncacheable in the direct map; it stays write-back\n");
		}
		base = frame_end;
	}
	unsafe { ((*bs).free_pages)(buf as u64, pages) };
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
	let status = unsafe { ((*bs).get_memory_map)(&mut map_size, core::ptr::null_mut(), &mut key, &mut desc_size, &mut desc_ver) };
	if status != uefi::STATUS_BUFFER_TOO_SMALL || desc_size == 0 || desc_size < core::mem::size_of::<uefi::MemoryDescriptor>() {
		panic!("loader: the firmware did not describe its memory map");
	}
	// CHECKED, and kept as the buffer's real capacity. This was `map_size + desc_size * 16` with
	// unchecked multiplication and addition, on values the FIRMWARE reports - so a hostile or simply
	// broken `desc_size` wraps the capacity to a small number and the allocation that follows is
	// too small for the map that is about to be written into it. The margin exists because the map
	// can grow between the sizing call and the real one; it is sixteen descriptors and it is now
	// arithmetic that cannot wrap.
	let Some(cap) = desc_size.checked_mul(16).and_then(|margin| map_size.checked_add(margin)) else {
		panic!("loader: the firmware's memory-map dimensions do not fit an allocation");
	};
	// SCRATCH, not retained (LDR-012): this buffer is read AT `ExitBootServices` and owned by
	// nothing afterwards. Allocated in the loader's own reclaimable class so the map it produces
	// describes it as memory the kernel may have back, instead of `MEM_BOOTLOADER` - which the
	// kernel never seeds, so every boot lost it for the life of the system.
	let buf = unsafe { alloc_scratch_pages(bs, cap.div_ceil(PAGE_SIZE as usize)) }.expect("loader: cannot allocate memory map buffer") as *mut uefi::MemoryDescriptor;
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
		// BUFFER_TOO_SMALL IS A CAPACITY STATE, not a generic failure. The map can grow between the
		// sizing call and this one - that is why there is a margin at all - and on the retry after a
		// stale key it can grow again. Reported as "get_memory_map failed", the one thing the reader
		// could act on was the one thing the message did not say.
		if status == uefi::STATUS_BUFFER_TOO_SMALL {
			panic!("loader: the firmware's memory map outgrew the buffer sized for it; this loader cannot resize after the last allocation");
		}
		if uefi::is_error(status) {
			panic!("get_memory_map failed");
		}
		// AND A SUCCESSFUL CALL THAT REPORTS MORE THAN THE BUFFER HOLDS IS REFUSED BEFORE IT IS
		// WALKED. `translate_map` reads `size` bytes out of a buffer of `cap`; nothing compared the
		// two, so firmware answering with a size past its own buffer would have been read straight
		// past the end of the allocation.
		if size > cap {
			panic!("loader: the firmware reports a memory map larger than the buffer it was given");
		}
		// FATAL, not silent, and said HERE because this is where there is a serial port to say it
		// on: `translate_map` refuses a map with more regions than the boot protocol carries rather
		// than truncating it.
		let Some(count) = (unsafe { uefi::memory::translate_map(buf, size, desc_size, regions) }) else {
			serial::write_str("loader: FATAL - the firmware memory map has more regions than the boot protocol carries\n");
			panic!("memory map larger than MAX_REGIONS");
		};
		// The heap lives on firmware pages, so it stops being usable exactly here. Retiring it
		// makes a later allocation fail loudly instead of handing out memory the loader no
		// longer owns.
		crate::heap::retire();
		// And the firmware's console with it: after this call `ConOut` points at memory the loader
		// no longer owns, so every later diagnostic goes to the built-in UART.
		crate::console::release();
		let status = unsafe { ((*bs).exit_boot_services)(image_handle, key) };
		if !uefi::is_error(status) {
			return count;
		}
		// ONLY a stale map key is worth retrying - the specification's `EFI_INVALID_PARAMETER`.
		// This looped on every error forever, which is the least informative possible response to a
		// firmware saying something else is wrong.
		if !uefi::memory::exit_retryable(status) {
			panic!("loader: ExitBootServices refused");
		}
		// The map changed; retry without allocating.
	}
}
