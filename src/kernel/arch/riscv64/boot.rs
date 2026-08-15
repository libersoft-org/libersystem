// riscv64 higher-half boot entry.
//
// QEMU `-machine virt` boots OpenSBI in M-mode; OpenSBI jumps to the kernel ELF
// entry `_start` in S-mode with the MMU off, a0 = hartid, a1 = DTB pointer. `_start`
// lives in the low, identity-linked `.text.boot` section: it is position-independent
// (PC-relative until paging is on), builds one Sv39 root table in the reserved
// `__boot_tables` page (a low identity megapage for the hand-off plus a high direct
// map of physical memory at KERNEL_VA_OFFSET), enables paging through SATP, then
// loads the higher-half address of `riscv64_main` (and the high boot stack) from
// `.data.boot` literals and jumps into the higher half. From there the kernel runs
// entirely from the high half, leaving the low half free for userspace.
//
// Sv39 layout: one root table (level 2), 512 entries, each a 1 GiB megapage leaf.
// Index 2 = the low identity (physical 0x8000_0000, covering the boot stub); index
// 256 = KERNEL_VA_OFFSET (physical 0), and 256 + N maps physical N GiB - so the
// kernel loaded at 0x8020_0000 (= 2 GiB) is reachable at its link VA via index 258.

use core::arch::global_asm;

global_asm!(
	r#"
.section .data.boot, "a"
.balign 8
.Lp_main:      .quad riscv64_main
.Lp_stack_top: .quad __boot_stack_top
.Lp_bss_start: .quad __bss_start
.Lp_bss_end:   .quad __bss_end

.section .text.boot.entry, "ax"
.global _start
_start:
	mv      s0, a0                  // save hartid
	mv      s1, a1                  // save DTB pointer

	// t0 = __boot_tables (its low identity address; SATP uses this physical page).
	la      t0, __boot_tables

	// Zero the root table (4096 bytes).
	mv      t1, t0
	li      t2, 4096
	add     t2, t1, t2
0:
	sd      zero, 0(t1)
	addi    t1, t1, 8
	bltu    t1, t2, 0b

	// Megapage leaf flags: V|R|W|X|G|A|D = 0xEF. A 1 GiB leaf for physical N GiB is
	// ((N GiB >> 12) << 10) | flags; each 1 GiB step adds 0x1000_0000 to the PTE.

	// Low identity: root[2] = leaf @ physical 0x8000_0000 (2 GiB) so the boot stub
	// (at 0x8020_0000) keeps executing after paging turns on. PPN part = 0x2000_0000.
	li      t1, 0x200000EF
	sd      t1, (2*8)(t0)

	// High direct map: root[256 + i] = leaf @ physical i GiB, i = 0..7 (0..8 GiB),
	// covering the low MMIO (i=0) and the DRAM at 0x8000_0000 (i=2). root[256] is
	// KERNEL_VA_OFFSET (physical 0).
	li      t3, 256
	slli    t3, t3, 3               // byte offset of root[256]
	add     t3, t0, t3
	li      t1, 0xEF                // leaf for physical 0
	li      t4, 0x10000000          // per-1-GiB PPN step
	li      t5, 8                   // map 8 GiB
	li      t6, 0
1:
	sd      t1, 0(t3)
	add     t1, t1, t4
	addi    t3, t3, 8
	addi    t6, t6, 1
	bltu    t6, t5, 1b

	// SATP = (8 << 60) | (root_phys >> 12): mode 8 = Sv39.
	srli    t1, t0, 12
	li      t2, 8
	slli    t2, t2, 60
	or      t1, t1, t2
	sfence.vma
	csrw    satp, t1
	sfence.vma

	// Switch to the higher-half boot stack (its VA is a .data.boot literal).
	la      t0, .Lp_stack_top
	ld      sp, 0(t0)

	// Zero the higher-half BSS.
	la      t0, .Lp_bss_start
	ld      t1, 0(t0)
	la      t0, .Lp_bss_end
	ld      t2, 0(t0)
2:
	bgeu    t1, t2, 3f
	sd      zero, 0(t1)
	addi    t1, t1, 8
	j       2b
3:
	// Branch into the higher half: riscv64_main(hartid, dtb).
	la      t0, .Lp_main
	ld      t0, 0(t0)
	mv      a0, s0
	mv      a1, s1
	jr      t0
4:
	wfi
	j       4b
"#
);

// The boot modules, handed over at RUN time rather than compiled in. riscv64 virt boots the
// kernel directly with no bootloader hand-off, so the archive arrives as an initrd and the
// device tree carries its range (`/chosen/linux,initrd-start` / `-end`).
//
// The kernel used to `include_bytes!` the packages from its own OUT_DIR, which made the kernel
// binary contain the userspace and made building the kernel require a built userspace. It does
// neither now: one kernel binary, whatever userspace is handed to it.
static BOOT_MODULES: crate::sync::SpinLock<Option<&'static [u8]>> = crate::sync::SpinLock::new(None);

// The boot argument, kept so a module can be looked up after the early boot has moved on.
static BOOT_ARG: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// A named module from the loader's hand-off, translated into the kernel's map.
//
// The loader hands over PHYSICAL addresses, because it runs before this kernel builds its map -
// so every read goes through `phys_to_virt`, exactly like the scanned archive.
fn loader_module_at(arg: u64, want: &[u8]) -> Option<&'static [u8]> {
	if arg == 0 {
		return None;
	}
	let magic = unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(arg) as *const u64) };
	if magic != bootproto::MAGIC {
		return None;
	}
	let bi = super::paging::phys_to_virt(arg) as *const bootproto::BootInfo;
	let (modules_phys, modules_len) = unsafe { (core::ptr::read_volatile(core::ptr::addr_of!((*bi).modules)), core::ptr::read_volatile(core::ptr::addr_of!((*bi).modules_len))) };
	if modules_phys == 0 || modules_len == 0 {
		return None;
	}
	let modules = unsafe { core::slice::from_raw_parts(super::paging::phys_to_virt(modules_phys) as *const bootproto::Module, modules_len as usize) };
	for module in modules {
		let end = module.name.iter().position(|&b| b == 0).unwrap_or(module.name.len());
		if &module.name[..end] == want {
			return Some(unsafe { core::slice::from_raw_parts(super::paging::phys_to_virt(module.addr) as *const u8, module.size as usize) });
		}
	}
	None
}

fn loader_archive(arg: u64) -> Option<&'static [u8]> {
	BOOT_ARG.store(arg, core::sync::atomic::Ordering::SeqCst);
	loader_module_at(arg, crate::product::INIT_PACKAGE.as_bytes())
}

fn loader_module(want: &[u8]) -> Option<&'static [u8]> {
	loader_module_at(BOOT_ARG.load(core::sync::atomic::Ordering::SeqCst), want)
}

fn boot_archive() -> &'static [u8] {
	(*BOOT_MODULES.lock()).unwrap_or(&[])
}

#[unsafe(no_mangle)]
extern "C" fn riscv64_main(hartid: u64, arg: u64) -> ! {
	super::serial::init();
	crate::serial_println!("riscv64: hello from the kernel (higher half)");
	// The entry argument is either a raw DTB pointer (OpenSBI `-kernel`) or a
	// `bootproto::BootInfo` pointer (the UEFI loader); tell them apart by the BootInfo
	// magic at the target. The UEFI path also carries a GOP framebuffer, so the kernel
	// draws its earliest boot log to the display instead of programming ramfb itself.
	let (dtb, uefi_fb) = decode_boot_arg(arg);
	crate::serial_println!("riscv64: hart {hartid}, DTB @ {dtb:#x}");

	// The loader's hand-off, taken BEFORE anything else looks at the boot argument - it must not
	// sit inside a device-tree branch, because under UEFI the firmware exposes no DTB and
	// everything conditioned on one is dead there.
	if let Some(archive) = loader_archive(arg) {
		*BOOT_MODULES.lock() = Some(archive);
		crate::serial_println!("riscv64: boot packages handed over by the loader ({} bytes)", archive.len());
	}

	// Prove the higher half is live: read back the VA this function is linked at.
	let here = riscv64_main as usize;
	crate::serial_println!("riscv64: _main VA = {here:#x} (KERNEL_VA_OFFSET high half)");

	// Confirm a high-half RAM read/write works through the Sv39 direct map.
	static mut PROBE: u64 = 0;
	let ok = unsafe {
		let p = &raw mut PROBE;
		core::ptr::write_volatile(p, 0xA5A5_1234_5678_C3C3);
		core::ptr::read_volatile(p) == 0xA5A5_1234_5678_C3C3
	};
	crate::serial_println!("riscv64: post-MMU RAM read/write = {}", if ok { "ok" } else { "FAIL" });

	// Increment 2: install the S-mode trap vector (STVEC) and exercise it.
	super::traps::init();
	// Enable the FPU (SSTATUS.FS = Initial, bit 13) so the context switch's fsd/fld and any
	// compiler-emitted FP do not trap.
	//
	// SSTATUS.SUM (bit 18) is NOT set here any more. It used to be, for the kernel's whole life, so
	// every stray kernel pointer was a potential user-memory access - the class of bug x86's SMAP
	// turns into a fault. `paging::user_access` opens the window around the copies that need it and
	// closes it again, and `paging::clac_on_entry` closes it on trap entry.
	unsafe { core::arch::asm!("csrs sstatus, {}", in(reg) 1u64 << 13, options(nostack, preserves_flags)) };
	// Let U-mode read the cycle / time / instret counters (SCOUNTEREN CY|TM|IR) so the
	// userspace runtime's rdcycle-based perf clock does not trap.
	unsafe { core::arch::asm!("csrw scounteren, {}", in(reg) 0x7u64, options(nostack, preserves_flags)) };
	crate::serial_println!("riscv64: STVEC trap vector installed");

	// Trap round-trip: `ebreak` traps to S-mode (OpenSBI delegates it via medeleg);
	// the handler advances SEPC past it and returns via sret, so execution resumes.
	unsafe { core::arch::asm!("ebreak") };
	crate::serial_println!("riscv64: trap round-trip (ebreak) OK");

	// Increment 4: parse the device tree (the shared FDT parser), seed the portable
	// frame allocator, and bring up the kernel heap in the higher half.
	use super::paging;
	let boot_info = super::dtb::parse(dtb);
	let ram_banks = boot_info.map(|bi| (bi.ram_regions, bi.ram_region_count));
	let (ram_top, cpu_count, pcie_ecam, _plic_base, fwcfg_base) = match boot_info {
		Some(bi) => {
			crate::serial_println!("riscv64: DTB parsed - RAM {:#x}..{:#x} ({} MB), {} CPU(s), ECAM {:#x}, PLIC {:#x}", bi.ram_base, bi.ram_base + bi.ram_size, bi.ram_size / (1024 * 1024), bi.cpu_count, bi.pcie_ecam, bi.plic_base);
			super::set_fwcfg_base(bi.fwcfg_base);
			(bi.ram_base + bi.ram_size, bi.cpu_count, bi.pcie_ecam, bi.plic_base, bi.fwcfg_base)
		}
		None => {
			crate::serial_println!("riscv64: no DTB found - using built-in defaults");
			(0, 1, 0, 0, 0)
		}
	};
	let cpu_count = cpu_count.max(1);
	// Svpbmt, before the first device is mapped. Without it `NO_CACHE` is dropped and MMIO gets
	// whatever the platform's physical memory attributes say - which is right on QEMU virt and
	// is not a general answer. With it, an uncacheable request becomes a PBMT=IO leaf and means
	// what it says.
	//
	// Detected rather than assumed, and the default is off: bits 62:61 are RESERVED on a hart
	// without the extension, so setting them where it is absent faults every mapping that uses
	// them. A device tree that does not mention it leaves this port exactly where it was.
	let svpbmt = super::dtb::has_isa_extension(dtb, b"svpbmt");
	paging::set_svpbmt(svpbmt);
	crate::serial_println!("riscv64: Svpbmt {} - uncacheable mappings {}", if svpbmt { "present" } else { "absent" }, if svpbmt { "use PBMT=IO" } else { "fall back to platform PMAs" });
	let _ = _plic_base;

	// Record the device-tree PCIe ECAM base (under 8 GiB, so the boot direct map already
	// reaches it). The AIA IMSIC uses its fixed QEMU virt S-file base (imsic.rs).
	super::pci::set_ecam_base(pcie_ecam);

	crate::mem::set_hhdm_offset(paging::KERNEL_VA_OFFSET);
	// AND WHAT THE STUB ACTUALLY MAPPED. The boot assembly maps eight 1 GiB leaves over 0..8 GiB before any memory
	// map is read, so the direct map's extent is that number and not the top of whatever the device
	// tree reports. Without this a machine with more RAM than the stub maps has `within_direct_map`
	// answering true for addresses `phys_to_virt` does not translate.
	crate::mem::set_direct_map_extent(8 * 1024 * 1024 * 1024);
	// The pool runs to the top of RAM, MINUS what the loader left in it. Same hand-off and same
	// hazard as aarch64 - no memory map, packages read into RAM above the kernel and read for the
	// life of the boot - so the same carve. See `arch::common::bootmem`.
	let mut holes = [crate::arch::common::bootmem::Hole { start: 0, end: 0 }; crate::arch::common::bootmem::MAX_HOLES];
	let hole_count = unsafe { crate::arch::common::bootmem::loader_reservations(BOOT_ARG.load(core::sync::atomic::Ordering::SeqCst), |phys| paging::phys_to_virt(phys), &mut holes) };
	// `banks + holes`, because each reservation splits at most one bank into one extra region.
	let mut regions = [bootproto::MemRegion { base: 0, length: 0, kind: bootproto::MEM_USABLE, _pad: 0 }; fdt::MAX_RAM_REGIONS + crate::arch::common::bootmem::MAX_HOLES];
	// EVERY BANK, not the contiguous run from the first one. `usable_region(0)` answers the floor -
	// the first page above the kernel image - and the fallback below is the machine with no device
	// tree at all, where one range is all there is to know.
	let region_count = match ram_banks {
		Some((banks, count)) if count > 0 => crate::arch::common::bootmem::carve_banks(&banks[..count], paging::usable_region(0).0, &holes[..hole_count], &mut regions),
		_ => {
			let (region_base, region_len) = paging::usable_region(ram_top);
			crate::arch::common::bootmem::carve(region_base, region_len, &mut holes[..hole_count], &mut regions)
		}
	};
	for hole in &holes[..hole_count] {
		crate::serial_println!("riscv64: reserved {:#x}..{:#x} ({} KiB) - handed over by the loader", hole.start, hole.end, (hole.end - hole.start) / 1024);
	}
	crate::mem::frame::init(&regions[..region_count]);
	crate::serial_println!("riscv64: frame allocator up - {} MB free DRAM", paging::frames_free() * 4 / 1024);
	crate::mem::heap::init();
	crate::mem::frame::upgrade_to_heap();
	// Reserve every top-level entry the kernel's two growing windows can ever need, while the
	// kernel's is still the only address space in existence. This is what `mem::init` does on
	// x86_64, and this port never did it.
	//
	// It matters here for the same reason it matters there and does NOT matter on aarch64: one
	// Sv39 root covers both halves, so a new address space COPIES the kernel-half entries at
	// creation and an entry created later is missing from every space made before it. aarch64
	// keeps the kernel half in TTBR1, a single tree every space shares, so a late entry is
	// visible everywhere by construction.
	//
	// Without this the first kernel-window mapping - a thread's kernel stack, which every
	// process creates - added a root entry to whatever address space happened to be active,
	// and the scheduler's guard then refused to switch into any other one:
	// "address space ... diverges from the kernel mapping at PML4[384]". 384 is the Sv39 root
	// index of KERNEL_MMAP_BASE.
	crate::mem::heap::reserve_window();
	crate::syscall::reserve_kernel_vmap();
	// Bring up the early framebuffer console so the kernel draws the boot log to the
	// display pixel-by-pixel like x86 - QEMU virt has no VGA, so without one the boot is
	// serial-only. The UEFI loader hands a GOP framebuffer in the BootInfo (drawn to
	// directly); the `-kernel` path has no loader, so the kernel programs QEMU ramfb over
	// fw-cfg itself. Runs after the heap + frame pool are up (the console grid, and
	// ramfb's framebuffer, are heap/frame allocations). A no-op if neither is present.
	match uefi_fb {
		Some(fb) => install_console(fb),
		None => init_ramfb_console(fwcfg_base),
	}
	// Retain the boot memory map so the `lsmem` inventory tool can render it (heap-backed,
	// so after heap::init).
	crate::mem::retain_memmap(&regions);
	{
		let mut v: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
		for i in 0..8 {
			// ALLOC-OK: boot bring-up self-test, before userspace exists.
			v.push(i * i);
		}
		let (mapped, free) = crate::mem::heap::stats();
		crate::serial_println!("riscv64: heap up - Vec sum={} | {} kB mapped, {} kB free", v.iter().sum::<u64>(), mapped / 1024, free / 1024);
	}

	// Prove 4 kB map_page / translate / unmap: map a fresh frame at a low test VA,
	// write + read it back through the mapping, confirm translate, then unmap.
	const TEST_VA: u64 = 0x1000_0000;
	let frame = paging::alloc_frame().expect("test frame");
	paging::map_page(TEST_VA, frame, paging::WRITABLE);
	unsafe { core::ptr::write_volatile(TEST_VA as *mut u64, 0xCAFE_F00D_1234_5678) };
	let readback = unsafe { core::ptr::read_volatile(paging::phys_to_virt(frame) as *const u64) };
	let xlate = paging::translate(TEST_VA);
	crate::serial_println!("riscv64: map_page test - readback {readback:#x}, translate {xlate:#x?} (frame {frame:#x})");
	paging::unmap_page(TEST_VA);
	// SAFETY: the frame this bring-up check allocated three lines up, just unmapped from
	// the only address that ever reached it.
	// NEVER-MAPPED: boot bring-up on the first core, before any other core is started, so no
	// other TLB exists to hold the translation this one just dropped.
	unsafe { paging::dealloc_frame(frame) };

	// Increment 5: monotonic clock, per-CPU block, context switch, scheduler, timer.
	super::tsc::init();
	// The timer rate comes from the device tree rather than from a constant naming QEMU. Done here
	// because this is where the DTB pointer is in hand and before anything converts a tick.
	super::tsc::init_from_dtb(dtb);
	super::apic::set_boot_hart(hartid);
	crate::serial_println!("riscv64: clock - timebase {} MHz, uptime {} ms", super::tsc::hz() / 1_000_000, super::tsc::cycles_to_ns(super::tsc::now()) / 1_000_000);

	// Per-CPU block for the boot hart, reachable through `tp`.
	super::percpu::allocate(cpu_count as usize);
	super::percpu::init(0, hartid as u32);
	// Bring up the AIA IMSIC on the boot hart (enable MSI delivery, accept any priority)
	// so per-device MSI EIDs can be enabled and delivered here.
	super::imsic::init_hart();
	// Size the per-CPU id tables and record the boot hart's real id (the SBI boot hart
	// is not necessarily hart 0), so the cross-hart wake IPI targets the right hart.
	crate::smp::set_cpu_count(cpu_count as usize);
	crate::smp::set_lapic_id(0, hartid as u32);
	{
		let cpu = super::percpu::this_cpu();
		crate::serial_println!("riscv64: per-CPU up (tp) - cpu_id={} hart={} of {} CPU(s)", cpu.cpu_id(), cpu.lapic_id(), cpu_count);
	}

	// Wake the secondary harts via SBI HSM hart_start (each brings up its own per-CPU
	// block, trap vector, and local timer, then idles until the scheduler is ready).
	super::smp::bring_up_secondaries(cpu_count, hartid);

	// The portable scheduler on top of the arch context/percpu contract.
	crate::sched::allocate(cpu_count as usize);
	crate::sched::init();

	// Under `cargo test`, the core subsystems (heap, paging, per-CPU, SMP, scheduler)
	// are up: arm the S-mode timer + enable interrupts (so the preemption tests can
	// interleave ring-0/ring-3 threads), wire the syscall path, populate the device
	// table + boot info, hand off to the kernel test harness, and exit QEMU
	// (SBI/semihosting). The scheduler demos and the userspace boot chain below are the
	// interactive (non-test) bring-up.
	#[cfg(test)]
	{
		super::apic::init();
		super::enable_interrupts();
		super::syscall::init();
		crate::device::init();
		publish_embedded_boot_info();
		crate::test_main();
		super::exit_qemu(true);
	}

	#[cfg(not(test))]
	{
		// Production boot (the interactive, non-test path): arm the S-mode timer, enable
		// interrupts, wire the syscall path, then bring up the real userspace boot chain
		// (SystemManager -> the service set -> the interactive shell) and idle on the
		// interrupt-driven console loop. The same clean sequence the x86_64 kmain runs -
		// no port demos.
		super::apic::init();
		super::enable_interrupts();
		super::syscall::init();
		run_system_manager();
		crate::serial_println!("riscv64: halting");
		super::halt_loop()
	}
}

// An early framebuffer the kernel draws its boot log to: its physical base (drawn
// through the direct map) plus geometry and pixel format. From QEMU ramfb (the
// `-kernel` path) or the UEFI loader's GOP (the BootInfo path).
#[derive(Clone, Copy)]
struct BootFb {
	phys: u64,
	width: u32,
	height: u32,
	stride: u32, // bytes per row
	red_shift: u8,
	red_size: u8,
	green_shift: u8,
	green_size: u8,
	blue_shift: u8,
	blue_size: u8,
}

// The early framebuffer set up at boot (None if the boot is serial-only), read by
// publish_embedded_boot_info to fill the BootInfo framebuffer for a userspace consumer.
static BOOT_FB: crate::sync::SpinLock<Option<BootFb>> = crate::sync::SpinLock::new(None);

// Decode the kernel entry argument: the DTB pointer, plus the GOP framebuffer when a
// UEFI loader handed a `bootproto::BootInfo` here rather than OpenSBI `-kernel`'s raw
// DTB pointer. Both are physical pointers reachable through the boot stub's direct map;
// a BootInfo is recognised by its magic, and on this arch carries the framebuffer's
// PHYSICAL base (the loader builds no page tables).
fn decode_boot_arg(arg: u64) -> (u64, Option<BootFb>) {
	if arg == 0 {
		return (0, None);
	}
	let magic = unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(arg) as *const u64) };
	if magic != bootproto::MAGIC {
		return (arg, None); // a raw DTB pointer (the OpenSBI `-kernel` entry state)
	}
	let bi = super::paging::phys_to_virt(arg) as *const bootproto::BootInfo;
	let dtb = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*bi).dtb)) };
	let present = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*bi).fb_present)) } != 0;
	let fb = present.then(|| {
		let f = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*bi).framebuffer)) };
		BootFb { phys: f.addr, width: f.width, height: f.height, stride: f.pitch, red_shift: f.red_shift, red_size: f.red_size, green_shift: f.green_shift, green_size: f.green_size, blue_shift: f.blue_shift, blue_size: f.blue_size }
	});
	(dtb, fb)
}

// Bring up the kernel framebuffer console on `fb` (its physical base drawn through the
// direct map), and record it for publish_embedded_boot_info to hand userspace.
fn install_console(fb: BootFb) {
	crate::console::init(crate::console::FbInfo { addr: super::paging::phys_to_virt(fb.phys) as *mut u8, width: fb.width as usize, height: fb.height as usize, pitch: fb.stride as usize, bytes_per_pixel: 4, red_shift: fb.red_shift, red_size: fb.red_size, green_shift: fb.green_shift, green_size: fb.green_size, blue_shift: fb.blue_shift, blue_size: fb.blue_size });
	*BOOT_FB.lock() = Some(fb);
}

// Program the QEMU ramfb early framebuffer over fw-cfg and bring up the console on it
// (the `-kernel` boot path, which has no loader to query GOP). ramfb is XRGB8888 - red
// at bit 16, green at 8, blue at 0. Serial-only if fw-cfg / ramfb is not present.
fn init_ramfb_console(fwcfg_base: u64) {
	let Some(fb) = crate::arch::common::fwcfg::setup_ramfb(fwcfg_base, 1280, 800, super::paging::phys_to_virt) else {
		return;
	};
	install_console(BootFb { phys: fb.phys, width: fb.width, height: fb.height, stride: fb.stride, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8 });
	crate::serial_println!("riscv64: ramfb framebuffer {}x{} at {:#x}", fb.width, fb.height, fb.phys);
}

// Publish a kernel-constructed BootInfo pointing at the embedded init.pkg / volume.pkg
// (riscv64 boots directly, with no bootloader hand-off, so the kernel builds its own).
// Both the userspace boot chain and the test harness read the packages through it.
fn publish_embedded_boot_info() {
	fn module(name: &[u8], bytes: &[u8]) -> bootproto::Module {
		let mut nm = [0u8; 32];
		nm[..name.len()].copy_from_slice(name);
		bootproto::Module { addr: bytes.as_ptr() as u64, size: bytes.len() as u64, name: nm }
	}
	// The loader hands over the bootstrap archive ITSELF - it read the programs off the volume and
	// packed them, so what arrives is already the init package, and `volume.pkg` arrives beside it
	// as its own named module.
	//
	// The `-kernel` runner used to lay down a WRAPPER holding both as entries, because a machine
	// with no loader can be handed exactly one blob. That shape is retired: unwrapping it looked
	// for an `init.pkg` inside an init.pkg, which was the last of four separate reasons this
	// kernel reported a malformed init package while holding a perfectly good one.
	let archive = boot_archive();
	let (init, volume): (&[u8], &[u8]) = (archive, loader_module(b"volume.pkg").unwrap_or(&[]));
	// ALLOC-OK: boot, building the BootInfo this kernel was not given; no userspace exists yet
	let modules: &'static mut [bootproto::Module; 2] = alloc::boxed::Box::leak(alloc::boxed::Box::new([module(b"init.pkg", init), module(b"volume.pkg", volume)]));
	// Hand the early framebuffer (if any) to a userspace consumer of the boot info.
	let (framebuffer, fb_present) = match *BOOT_FB.lock() {
		Some(f) => (bootproto::Framebuffer { addr: super::paging::phys_to_virt(f.phys), width: f.width, height: f.height, pitch: f.stride, bpp: 32, red_shift: f.red_shift, red_size: f.red_size, green_shift: f.green_shift, green_size: f.green_size, blue_shift: f.blue_shift, blue_size: f.blue_size, _pad: [0; 2] }, 1u32),
		None => (bootproto::Framebuffer { addr: 0, width: 0, height: 0, pitch: 0, bpp: 0, red_shift: 0, red_size: 0, green_shift: 0, green_size: 0, blue_shift: 0, blue_size: 0, _pad: [0; 2] }, 0u32),
	};
	// ALLOC-OK: boot, as above
	let bi: &'static bootproto::BootInfo = alloc::boxed::Box::leak(alloc::boxed::Box::new(bootproto::BootInfo { magic: bootproto::MAGIC, version: bootproto::VERSION, _pad0: 0, hhdm_offset: super::paging::KERNEL_VA_OFFSET, memmap: 0, memmap_len: 0, modules: modules.as_ptr() as u64, modules_len: modules.len() as u64, framebuffer, fb_present, psci_conduit: bootproto::PSCI_NONE, rsdp: 0, smp_trampoline: 0, dtb: 0 }));
	crate::publish_boot_info(bi);
}

// Spawn the real SystemManager from the embedded init package and drive the userspace
// boot chain as far as it runs, draining its reports, then hand off to the interactive
// shell over the serial console. The same portable mechanism the x86/aarch64 kernels
// use (pkg::Package + loader::spawn_elf_process + the PACKAGE/RAMDISK/MODE bootstrap).
#[cfg(not(test))]
fn run_system_manager() {
	if boot_archive().is_empty() {
		crate::serial_println!("riscv64: no boot packages were handed over - userspace is not started");
		return;
	}

	// Populate the kernel device table from the PCI scan so DeviceManager can enumerate
	// the virtio devices (the same one-time boot scan the other kmains do).
	crate::device::init();

	publish_embedded_boot_info();

	match crate::spawn_system_manager() {
		Ok((ep, koid)) => {
			crate::serial_println!("riscv64: system - SystemManager spawned (koid {koid}), bringing up userspace");
			// Drive the boot chain until the interactive shell attaches (the last component
			// to come up), draining its reports as they arrive. riscv under TCG settles the
			// interrupt-driven chain more slowly and variably than x86/aarch64, so drive to
			// the shell rather than a fixed budget; the cap is generous so the loop always
			// returns even if a component never settles.
			for _ in 0..4000 {
				crate::sched::run_until_idle();
				while let Ok(msg) = ep.recv() {
					crate::serial_println!("riscv64: userspace: {}", core::str::from_utf8(&msg.bytes).unwrap_or("<bad>"));
				}
				if crate::console_input::shell_listening() {
					break;
				}
				super::idle_halt();
			}
			crate::serial_println!("riscv64: system - userspace boot chain settled");
			crate::console_shell_loop();
		}
		Err(reason) => {
			crate::serial_println!("riscv64: system - SystemManager failed to start: {reason}");
		}
	}
}
