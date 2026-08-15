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

use crate::{PAGE_SIZE, align_down};
use uefi::{self, BootServices, Handle, SystemTable};

// Halt the core (panic path): wait for an event forever. panic=abort, no unwind.
pub fn halt() -> ! {
	loop {
		unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
	}
}

// Place the kernel at its physical link addresses, find the device tree, exit boot
// services, and enter the kernel's boot stub with the MMU off and the DTB in x0.
pub fn hand_off(bs: *mut BootServices, image_handle: Handle, system_table: *mut SystemTable, root: Option<*mut uefi::FileProtocol>, kernel: &[u8], reserved: &crate::ReservedKernel) -> ! {
	let entry = load_kernel(kernel, reserved);
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
	//
	// THE EL2 BRANCH BELOW HAS NOW BEEN RUN, on `-machine virt,virtualization=on`, and what it found
	// is the reason the PSCI conduit is read from the platform a hundred lines down. The drop itself
	// works: fifteen lines of kernel output came out of it. What broke was everything AFTER the
	// `eret` that assumed something was still at EL2 - the kernel's `hvc #0` at `cpu_on` landed in
	// the firmware's EL2 vectors, which outlive `ExitBootServices`, and the boot died there with no
	// explanation. `BootInfo::psci_conduit` exists because of that run.
	//
	// So a reader inherits the finding rather than the warning that preceded it: this branch is
	// exercised, and the thing to be careful about is not the drop but what the code below it
	// believes about the level it just left.
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
				// OPEN THE GENERIC TIMER TO EL1 BEFORE LEAVING EL2.
				//
				// `CNTHCTL_EL2.EL1PCTEN` (bit 0) and `EL1PCEN` (bit 1) are CLEAR out of reset, and
				// while they are, an EL1 read of the physical counter or timer traps to EL2 - where
				// this loader will have left no handler. This kernel's tick IS the EL1 generic
				// timer, so a boot that dropped from EL2 would have taken an unhandled trap at its
				// first tick. Nothing else configures these: once the loader has decided to change
				// exception level, opening what the level below needs is the loader's job.
				"mrs x9, cnthctl_el2",
				"orr x9, x9, #0x3",
				"msr cnthctl_el2, x9",
				// And zero the virtual offset, so the virtual counter EL1 reads is the physical one
				// rather than the physical one plus whatever CNTVOFF_EL2 held out of reset.
				"msr cntvoff_el2, xzr",
				// OPEN FP/SIMD AND DEBUG TO EL1. REASONED, NOT MEASURED - said plainly, because the
				// first version of this comment claimed the opposite.
				//
				// `CPTR_EL2.TFP` (bit 10) traps every FP/SIMD instruction at EL1 to EL2 and its reset
				// value is architecturally UNKNOWN (QEMU's is 0x33ff, with TFP SET); `MDCR_EL2` does
				// the same for debug and performance-monitor accesses. A loader that has just decided
				// to change exception level has to open what the level below needs, because nothing
				// else will and no handler is left up here - the same argument the generic timer
				// above is opened by.
				//
				// What the first EL2 boot proved is NOT this. It died at the kernel's `hvc #0`, and
				// so did the boot with these two writes in place - identical fault address. So they
				// are here on the architecture manual's word and Linux's `init_el2`, and the trap
				// they prevent has not been observed. 0x33ff is the canonical value: RES1 bits set,
				// TCPAC / TTA / TFP clear.
				"mov x9, #0x33ff",
				"msr cptr_el2, x9",
				"msr mdcr_el2, xzr",
				"isb",
				// Return to EL1h with interrupts masked, at the kernel's entry.
				"mov x9, #0x3c5",
				"msr spsr_el2, x9",
				"msr elr_el2, x1",
				// SP_EL1, NOT SP. `SPSR_EL2.M[3:0] = 0b0101` is EL1h, which selects SP_EL1 - and
				// `mov sp, x2` writes the stack pointer of the level EXECUTING, which is EL2. So
				// the value the comment calls "the stack the kernel starts on" went into a register
				// the kernel never reads, and EL1 would have started on whatever SP_EL1 held.
				//
				// It is dead either way in this tree - the kernel's `_start` sets SP from
				// `__boot_stack_top` before anything touches it - which is exactly why a wrong
				// register could sit here unnoticed, and why it is worth correcting while nothing
				// depends on the contract this code states.
				"msr sp_el1, x2",
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
		// WHAT WILL BE LEFT BELOW THE KERNEL, decided here because this is the code that decides it.
		//
		// `CurrentEL` is read here rather than remembered from `hand_off`, so the two answers cannot
		// drift apart.
		let current_el: u64;
		core::arch::asm!("mrs {0}, CurrentEL", out(reg) current_el, options(nomem, nostack));
		let psci_conduit = psci_conduit(current_el, dtb);
		*(phys as *mut BootInfo) = BootInfo { magic: bootproto::MAGIC, version: bootproto::VERSION, _pad0: 0, hhdm_offset: 0, memmap: 0, memmap_len: 0, modules, modules_len, framebuffer, fb_present: fb.present as u32, psci_conduit, rsdp: 0, smp_trampoline: 0, dtb };
	}
	phys
}

// Which PSCI conduit, if any, the kernel will have below it.
//
// THE EXCEPTION LEVEL DECIDES ONE THING AND THE PLATFORM DECIDES THE OTHER. This used to be a single
// expression: `PSCI_NONE` at EL2, `PSCI_HVC` otherwise. The first half was right about HVC and wrong
// about SMC, which is the correction below; the second half was a guess: a machine that hands firmware EL1 has something below it, and
// what that something answers is stated by the platform, not implied by where the firmware put us.
// Most server-class AArch64 keeps EL2 for a hypervisor and PSCI in EL3 firmware behind `smc`, so
// this loader was declaring `PSCI_HVC` and the kernel was executing `hvc #0` at `cpu_on` - the same
// class of assumption as the hard-coded UART address this milestone is named for.
//
// The two places a platform states it, in the order everything else in this milestone reads them:
// the device tree's `/psci/method`, then ACPI's FADT ARM Boot Architecture Flags. A machine that
// states neither gets `PSCI_NONE`, which the kernel reports and boots single-core on - the honest
// answer, and not the same as picking the more common instruction and hoping.
fn psci_conduit(current_el: u64, dtb: u64) -> u32 {
	let (discovered_dtb, rsdp) = crate::console::firmware_tables();
	let tree = if dtb != 0 { dtb } else { discovered_dtb };
	let discovered = if tree != 0 {
		fdt::Fdt::new(tree, crate::console::identity_map).psci_conduit().map(|conduit| match conduit {
			fdt::PsciConduit::Hvc => bootproto::PSCI_HVC,
			fdt::PsciConduit::Smc => bootproto::PSCI_SMC,
		})
	} else {
		None
	};
	let discovered = discovered.or_else(|| {
		if rsdp == 0 {
			return None;
		}
		uefi::acpi::Acpi::new(rsdp, crate::console::identity_map).psci_conduit().map(|conduit| match conduit {
			uefi::acpi::PsciConduit::Hvc => bootproto::PSCI_HVC,
			uefi::acpi::PsciConduit::Smc => bootproto::PSCI_SMC,
		})
	});
	// THE EXCEPTION LEVEL INVALIDATES ONE CONDUIT AND NOT THE OTHER.
	//
	// This returned `PSCI_NONE` at EL2 before it looked at anything, and the reasoning - "whatever
	// answered before is above the kernel now" - is true of an HVC service and false of an SMC one.
	// `hvc` traps to EL2, which is the level this loader itself leaves behind, so after the `eret`
	// nothing is there to answer. `smc` traps to EL3, which is BELOW both and is not affected by
	// where the firmware put us at all: a secure monitor implementing PSCI goes on implementing it.
	//
	// Most server-class AArch64 is exactly that shape - a hypervisor's EL2 and PSCI in EL3 firmware
	// behind `smc` - so the discarding branch threw away the right answer on the machines the
	// discovery was added for.
	match discovered {
		Some(bootproto::PSCI_HVC) if (current_el >> 2) & 0b11 == 2 => bootproto::PSCI_NONE,
		Some(conduit) => conduit,
		None => bootproto::PSCI_NONE,
	}
}

// Load each PT_LOAD segment at its physical (link) address - the placement QEMU's
// `-kernel` produces, which the kernel's higher-half boot stub relies on (its TTBR1
// maps a high VA to its link physical address). Returns the entry point (physical).
// `bs` is gone from the signature deliberately: this backend no longer allocates here. The spans
// were staked by `reserve_kernel` and the only question left is whether this loader owns them,
// which `ReservedKernel` answers without asking the firmware anything.
fn load_kernel(kernel: &[u8], reserved: &crate::ReservedKernel) -> u64 {
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
		// The segment's physical span (page-aligned), which `reserve_kernel` staked before any of
		// the opportunistic allocations so the firmware would still have it here.
		let base = align_down(ph.p_paddr, PAGE_SIZE);
		// The SAME expression `reserve_kernel` uses, and it has to be: `owns` below compares this
		// against what that recorded, so a difference between the two - including a difference in
		// how they overflow - would make the ownership check compare two wrong numbers and agree.
		let pages = (ph.p_paddr - base).checked_add(ph.p_memsz).expect("a segment whose physical end wraps is refused by the parser").div_ceil(PAGE_SIZE);
		// ASK THE RESERVATION, NOT THE FIRMWARE. A second `AllocateAddress` here returns the same
		// `NOT_FOUND`/`NOT_AVAILABLE` whether the owner is this loader or the firmware, a runtime
		// service or a device - so its status could not be interpreted, was discarded, and the
		// write below happened either way. `ReservedKernel` is the answer to the question that call
		// was trying to ask, and a span this loader does not own is fatal: the boot stops rather
		// than writing over whatever is there.
		assert!(reserved.owns(base, pages), "loader: the kernel's physical span at {base:#x} was not reserved by this loader - something else owns it, and placing the kernel there would overwrite it");
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
