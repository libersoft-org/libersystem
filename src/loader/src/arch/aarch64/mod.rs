// aarch64 loader backend: place the kernel and enter its own boot stub.
//
// Unlike x86 (where the loader also builds the kernel's page tables), the aarch64 kernel carries a
// position-independent boot stub that sets up the MMU + higher half itself - exactly the entry
// state QEMU's `-kernel` load produces. So this backend mirrors that state: it loads each PT_LOAD
// segment at its physical (link) address, finds the firmware's flattened device tree, exits boot
// services, turns the MMU off, and branches to the kernel entry (`_start`) with x0 pointing at a
// BootInfo whose `dtb` field carries the device tree. No PAGE TABLES are built here - the kernel's
// proven boot path does the rest - but a BootInfo IS, because the modules (the bootstrap package,
// the factory archive and a live medium's system volume) have nowhere else to be described.
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
pub fn hand_off(bs: *mut BootServices, image_handle: Handle, system_table: *mut SystemTable, root: Option<*mut uefi::FileProtocol>, kernel: &[u8]) -> ! {
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
	// The bootstrap set, published as modules exactly as on x86_64.
	//
	// This arch used to get its programs from an archive placed in RAM by the runner and found by
	// scanning for a magic number, because the loader handed over nothing. That works for a
	// direct `-kernel` boot and not at all under UEFI, where nobody lays the archive down - the
	// kernel came up and reported its init package malformed, having found no package at all.
	// Reading it here is what makes the loader path usable on this architecture (M0138c).
	// `main` has already reported WHERE the set came from, and there are three possible answers.
	// Saying "from the system volume" here claimed the first of them unconditionally, so a riscv64
	// boot that had actually assembled its set from the boot medium said both things, one after
	// the other, and the wrong one last.
	let init_pkg = match unsafe { crate::BOOTSTRAP } {
		Some(archive) => Some(archive),
		None => crate::read_boot_file(bs, root, crate::INIT_PKG_FILE),
	};
	// The factory archive too, when the medium carries one: the kernel test suite uses it as its
	// fixture, and a shipping medium simply has none.
	let volume_pkg = root.and_then(|root| crate::read_file(bs, root, crate::VOLUME_PKG_FILE));
	let boot_info = build_boot_info(bs, dtb, init_pkg, volume_pkg);

	// ExitBootServices is the last firmware call; after it no service may be used.
	exit_boot_services(bs, image_handle);

	// Mirror the QEMU `-kernel` entry state and enter the kernel's boot stub: turn
	// the MMU + caches off (the stub sets translation up from scratch), synchronise,
	// then branch to the entry with the BootInfo pointer in x0. The loader ran under
	// the firmware's identity map, so with translation off it keeps executing at the
	// same (physical) addresses through the branch.
	// ENTER THE KERNEL AT EL1 WITH THE MMU OFF. One stub, so there is one place that states the
	// entry contract - and it reads `CurrentEL` rather than assuming it.
	//
	// The UEFI specification has AArch64 firmware execute at the highest non-secure exception level
	// available, which is EL2 on most server-class parts. This sequence read and wrote `sctlr_el1`
	// only, so on such a machine it cleared the MMU and cache bits of a translation regime it was
	// not executing under, left SCTLR_EL2 untouched, and branched to the kernel's boot stub - which
	// sets up EL1 translation - with the CPU still at EL2. QEMU's virt machine starts the loader at
	// EL1, which is why this has never been seen here.
	unsafe {
		let current_el: u64;
		core::arch::asm!("mrs {0}, CurrentEL", out(reg) current_el, options(nomem, nostack));
		if (current_el >> 2) & 0b11 == 2 {
			core::arch::asm!(
				"dsb sy",
				// EL1 is AArch64 and its caches and MMU start off.
				"mov x9, #(1 << 31)",
				"msr hcr_el2, x9",
				"mrs x9, sctlr_el1",
				"bic x9, x9, #0x1",
				"bic x9, x9, #0x4",
				"bic x9, x9, #0x1000",
				"msr sctlr_el1, x9",
				// And EL2's own, since that is the regime executing right now.
				"mrs x9, sctlr_el2",
				"bic x9, x9, #0x1",
				"bic x9, x9, #0x4",
				"bic x9, x9, #0x1000",
				"msr sctlr_el2, x9",
				"isb",
				"tlbi alle2",
				"tlbi vmalle1",
				"ic iallu",
				"dsb sy",
				"isb",
				// Return to EL1h with interrupts masked, at the kernel's entry.
				"mov x9, #0x3c5",
				"msr spsr_el2, x9",
				"msr elr_el2, x1",
				"mov sp, x2",
				"eret",
				in("x0") boot_info,
				in("x1") entry,
				in("x2") stack_top(),
				options(noreturn),
			);
		}
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

// The stack the kernel starts on when the loader has to `eret` into EL1: the loader's own, which
// UEFI gave it and which nothing else will use after `ExitBootServices`. Read rather than invented,
// so the kernel begins on memory the firmware really allocated.
fn stack_top() -> u64 {
	let sp: u64;
	unsafe { core::arch::asm!("mov {0}, sp", out(reg) sp, options(nomem, nostack)) };
	sp & !0xf
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
	let framebuffer = if fb.present { Framebuffer { addr: fb.phys, width: fb.width, height: fb.height, pitch: fb.pitch, bpp: 32, red_shift: fb.red_shift, red_size: fb.red_size, green_shift: fb.green_shift, green_size: fb.green_size, blue_shift: fb.blue_shift, blue_size: fb.blue_size, _pad: [0; 2] } } else { unsafe { core::mem::zeroed() } };
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
		*(phys as *mut BootInfo) = BootInfo { magic: bootproto::MAGIC, version: bootproto::VERSION, _pad0: 0, hhdm_offset: 0, memmap: 0, memmap_len: 0, modules, modules_len, framebuffer, fb_present: fb.present as u32, _pad1: 0, rsdp: 0, smp_trampoline: 0, dtb };
	}
	phys
}

// Load each PT_LOAD segment at its physical (link) address - the placement QEMU's
// `-kernel` produces, which the kernel's higher-half boot stub relies on (its TTBR1
// maps a high VA to its link physical address). Returns the entry point (physical).
fn load_kernel(bs: *mut BootServices, kernel: &[u8]) -> u64 {
	let image = crate::elf::Elf::parse(kernel).expect("loader: kernel is not a valid aarch64 ELF64 executable");
	// ET_EXEC ONLY. The shared parser admits `ET_DYN` too, because the kernel loads position-
	// independent userspace binaries with it and the compatibility checker reads shared libraries -
	// but THIS loader computes no load bias and processes no relocations, so a PIE kernel would be
	// placed at its link addresses and jumped to unrelocated. Refused by name until a PIE kernel is
	// wanted, which is a change here rather than to the parser.
	assert!(image.image_type == crate::elf::ET_EXEC, "loader: the kernel image must be ET_EXEC");
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
		// An error here is expected on the second claim: `reserve_kernel_span` already staked this
		// span before the opportunistic allocations, precisely so nothing else could take it. What
		// would be fatal is the span belonging to something else, and that is what the reservation
		// prevents rather than what this call detects.
		let _ = status;
		unsafe {
			core::ptr::write_bytes(ph.p_paddr as *mut u8, 0, ph.p_memsz as usize);
			if let Some(data) = image.segment_data(&ph) {
				core::ptr::copy_nonoverlapping(data.as_ptr(), ph.p_paddr as *mut u8, data.len());
			}
			// CLEAN THIS SEGMENT TO THE POINT OF UNIFICATION, and invalidate the instruction cache
			// over it.
			//
			// The kernel arrives through ordinary stores, and the handoff below then does `dsb sy`
			// and clears C and I in SCTLR - but `dsb` ORDERS, it does not write dirty lines back.
			// So the kernel's text could still be resident only in the D-cache when the D-cache was
			// turned off, and the branch would land on whatever RAM held before. QEMU's TCG models
			// a coherent memory system and will never show it; real ARM parts will.
			clean_to_pou(ph.p_paddr, ph.p_memsz);
		}
	}
	image.entry
}

// Clean `len` bytes at `addr` out of the data cache to the point of unification and invalidate the
// instruction cache over the same range, so an instruction fetch with the caches off sees them.
//
// The line size is read from CTR_EL0 rather than assumed: DminLine is log2 of the number of WORDS
// in the smallest data cache line, so the stride is `4 << DminLine`.
unsafe fn clean_to_pou(addr: u64, len: u64) {
	if len == 0 {
		return;
	}
	unsafe {
		let ctr: u64;
		core::arch::asm!("mrs {0}, ctr_el0", out(reg) ctr, options(nomem, nostack));
		let dline: u64 = 4 << ((ctr >> 16) & 0xf);
		let iline: u64 = 4 << (ctr & 0xf);
		let end = addr + len;
		let mut at = addr & !(dline - 1);
		while at < end {
			core::arch::asm!("dc cvau, {0}", in(reg) at, options(nostack, preserves_flags));
			at += dline;
		}
		core::arch::asm!("dsb ish", options(nostack, preserves_flags));
		let mut at = addr & !(iline - 1);
		while at < end {
			core::arch::asm!("ic ivau, {0}", in(reg) at, options(nostack, preserves_flags));
			at += iline;
		}
		core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
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
// be called. aarch64 needs no translated map - the kernel reads RAM from the DTB.
fn exit_boot_services(bs: *mut BootServices, image_handle: Handle) {
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
	// GIVE THE HEAP BACK, here and not before the buffer above.
	//
	// The arenas are the loader's own working memory and left alone they reach the kernel as
	// `MEM_BOOTLOADER`, which its frame allocator never seeds - so they would be reserved for the
	// system's whole life. Freeing them BEFORE this allocation hung riscv64: the firmware satisfies
	// `AllocateAnyPages` out of whatever is free, sixteen megabytes had just become free, and the
	// buffer it handed back landed where the kernel is placed after `ExitBootServices`. Freed after
	// the buffer exists, the map still reports the arenas as usable - which is the point of
	// returning them - and nothing allocated afterwards can land in the kernel's destination,
	// because nothing is allocated afterwards.
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
		let status = unsafe { ((*bs).exit_boot_services)(image_handle, key) };
		if !uefi::is_error(status) {
			return;
		}
		// ONLY a stale map key is worth retrying. This looped on every error forever, which is the
		// least informative possible response to a firmware saying something else is wrong.
		if status != uefi::STATUS_INVALID_PARAMETER {
			panic!("loader: ExitBootServices refused");
		}
		// The map changed; retry without allocating.
	}
}
