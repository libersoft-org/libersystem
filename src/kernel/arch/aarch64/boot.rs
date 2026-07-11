// aarch64 higher-half boot entry (M116).
//
// QEMU `-machine virt -kernel <elf>` enters the ELF entry `_start` at its physical
// address with the MMU off, at EL1, x0 = DTB. `_start` lives in the low, identity-
// linked `.text.boot` section: it is position-independent (no absolute references
// to the high half until the MMU is on), builds the boot page tables in the
// reserved `__boot_tables` region (an identity/direct-map L1 shared by a low L0
// for TTBR0 and a high L0 for TTBR1), turns on the MMU, then loads the higher-half
// address of `aarch64_main` (and the high boot stack) from `.data.boot` literals
// and branches into the higher half. From there the kernel runs entirely from
// TTBR1, leaving TTBR0 free for userspace.

use core::arch::global_asm;

global_asm!(
	r#"
.section .data.boot, "a"
.balign 8
.Lp_main:      .quad aarch64_main
.Lp_stack_top: .quad __boot_stack_top
.Lp_bss_start: .quad __bss_start
.Lp_bss_end:   .quad __bss_end

.section .text.boot, "ax"
.global _start
_start:
	mov     x19, x0                 // save DTB

	// Boot tables: x20 = L1, x21 = L0_LOW (TTBR0), x22 = L0_HIGH (TTBR1).
	adrp    x20, __boot_tables
	add     x20, x20, :lo12:__boot_tables
	add     x21, x20, #4096
	add     x22, x20, #8192

	// Zero the three tables (12 kB).
	mov     x0, x20
	add     x1, x20, #12288
0:
	str     xzr, [x0], #8
	cmp     x0, x1
	b.lo    0b

	// L1[0] = 1 GB Device block @ 0 (UART/GIC/low ECAM).
	mov     x0, #0x0401
	movk    x0, #0x0060, lsl #48
	str     x0, [x20]
	// L1[1..3] = 1 GB Normal blocks @ 1/2/3 GB (DRAM).
	movz    x2, #0x4000, lsl #16    // x2 = 0x4000_0000 (1 GB)
	mov     x0, #0x0705             // Normal block flags
	orr     x0, x0, x2
	str     x0, [x20, #8]
	add     x0, x0, x2
	str     x0, [x20, #16]
	add     x0, x0, x2
	str     x0, [x20, #24]
	// L1[256] = 1 GB Device block @ 256 GB (the high-mem PCIe ECAM).
	mov     x0, #0x0401
	movk    x0, #0x0040, lsl #32
	movk    x0, #0x0060, lsl #48
	str     x0, [x20, #2048]
	// L0_LOW[0] and L0_HIGH[0] -> L1 (table descriptor).
	orr     x0, x20, #3
	str     x0, [x21]
	str     x0, [x22]

	// MAIR: attr0 = Device-nGnRnE, attr1 = Normal write-back.
	mov     x0, #0xFF00
	msr     mair_el1, x0
	// TCR: T0SZ=T1SZ=16 (48-bit), 4 kB granules, WB inner-shareable, IPS = PARange.
	mrs     x0, id_aa64mmfr0_el1
	and     x0, x0, #0x7
	lsl     x0, x0, #32
	movz    x1, #0x3510
	movk    x1, #0xB510, lsl #16
	orr     x0, x0, x1
	msr     tcr_el1, x0
	msr     ttbr0_el1, x21
	msr     ttbr1_el1, x22
	dsb     sy
	tlbi    vmalle1
	dsb     sy
	isb
	// Enable the MMU (SCTLR_EL1.M).
	mrs     x0, sctlr_el1
	orr     x0, x0, #1
	msr     sctlr_el1, x0
	isb

	// Switch to the higher-half boot stack.
	adrp    x0, .Lp_stack_top
	ldr     x1, [x0, :lo12:.Lp_stack_top]
	mov     sp, x1
	// Zero the higher-half BSS.
	adrp    x0, .Lp_bss_start
	ldr     x2, [x0, :lo12:.Lp_bss_start]
	adrp    x0, .Lp_bss_end
	ldr     x3, [x0, :lo12:.Lp_bss_end]
1:
	cmp     x2, x3
	b.hs    2f
	str     xzr, [x2], #8
	b       1b
2:
	// Branch into the higher half: aarch64_main(dtb).
	adrp    x0, .Lp_main
	ldr     x4, [x0, :lo12:.Lp_main]
	mov     x0, x19
	br      x4
3:
	wfe
	b       3b
"#
);

// The init and volume packages, assembled from the aarch64 userspace by build.rs
// and embedded here (aarch64 has no bootloader module hand-off). Empty when the
// userspace was not staged first (a bare `cargo build`).
const INIT_PKG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/init.pkg"));
const VOLUME_PKG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/volume.pkg"));

#[unsafe(no_mangle)]
extern "C" fn aarch64_main(dtb: u64) -> ! {
	super::serial::init();

	// Enable Advanced SIMD / floating-point at EL0 and EL1 (CPACR_EL1.FPEN = 0b11)
	// so the kernel and userspace may use FP/vector instructions - the compiler
	// emits them for bulk memory operations - without trapping (EC 0x7).
	super::enable_fp();

	// Current exception level (CurrentEL bits [3:2]).
	let current_el: u64;
	unsafe {
		core::arch::asm!("mrs {}, CurrentEL", out(reg) current_el, options(nomem, nostack, preserves_flags));
	}
	let el = (current_el >> 2) & 0b11;

	crate::serial_println!("{} kernel is starting ...", crate::product::NAME);
	crate::serial_println!("arch: aarch64 | EL{el} | DTB {dtb:#x}");

	// The low boot stub already enabled the MMU: TTBR0 = a low identity map (for
	// the hand-off), TTBR1 = the higher-half kernel plus a physical direct map. The
	// kernel runs from the high half; device MMIO is reached through phys_to_virt.
	crate::serial_println!("aarch64: MMU on (higher half, 4 kB granule)");

	// Prove translation works: the UART (device) and this code (Normal RAM) walk
	// back to their own physical addresses, and a RAM read-back survives the MMU.
	let uart = super::paging::translate(0x0900_0000).unwrap_or(0);
	let code = super::paging::translate(aarch64_main as *const () as u64).unwrap_or(0);
	crate::serial_println!("aarch64: translate(uart 0x9000000) = {uart:#x}");
	crate::serial_println!("aarch64: translate(&aarch64_main)   = {code:#x}");

	static mut PROBE: u64 = 0;
	let ram_ok = unsafe {
		let p = &raw mut PROBE;
		core::ptr::write_volatile(p, 0xA5A5_1234_5678_C3C3);
		core::ptr::read_volatile(p) == 0xA5A5_1234_5678_C3C3
	};
	crate::serial_println!("aarch64: post-MMU RAM read/write = {}", if ram_ok { "ok" } else { "FAIL" });

	// Install the EL1 exception vectors (VBAR_EL1).
	super::exceptions::init_vectors();
	crate::serial_println!("aarch64: VBAR_EL1 exception vectors installed");

	// Bring up the GIC + the generic timer, enable interrupts, and confirm the
	// timer IRQ fires by watching the tick counter advance (each tick arrives
	// through the IRQ vector -> gic::handle_irq -> eret).
	super::gic::init();
	crate::serial_println!("aarch64: GIC + generic timer up ({} Hz counter)", super::gic::timer_hz());
	// Read the GICv2m frame's MSI SPI range so userspace drivers can acquire per-device
	// MSI-X vectors (the delivery path for virtio-net/input/snd, xhci, virtio-gpu).
	super::interrupts::init();
	super::enable_interrupts();
	let start = super::gic::ticks();
	let mut spins: u64 = 0;
	while super::gic::ticks() < start + 5 && spins < 2_000_000_000 {
		super::idle_halt();
		spins += 1;
	}
	crate::serial_println!("aarch64: timer IRQs delivered - {} ticks", super::gic::ticks() - start);

	// Parse the device tree (QEMU leaves it in low RAM; x0 arrives as 0 for a bare
	// ELF, so the parser scans for it) to learn the real RAM size and CPU count
	// instead of hard-coding them.
	let boot_info = super::dtb::parse(dtb);
	let (ram_top, cpu_count, fwcfg_base) = match boot_info {
		Some(bi) => {
			crate::serial_println!("aarch64: DTB parsed - RAM {:#x}..{:#x} ({} MB), {} CPU(s)", bi.ram_base, bi.ram_base + bi.ram_size, bi.ram_size / (1024 * 1024), bi.cpu_count);
			// The boot stub already maps the 256 GB device region (BOOT_L1[256]), so
			// the PCIe ECAM is reachable through phys_to_virt; just point PCI at it.
			if bi.pcie_ecam != 0 {
				super::pci::set_ecam_base(bi.pcie_ecam);
			}
			(bi.ram_base + bi.ram_size, bi.cpu_count, bi.fwcfg_base)
		}
		None => {
			crate::serial_println!("aarch64: no DTB found - using built-in defaults");
			(0, 1, 0)
		}
	};

	// Seed the portable frame allocator from the device-tree memory map, then bring
	// up the kernel heap in the higher half (the TTBR1 root is already live from the
	// boot stub). After this, `alloc` collections (Box, Vec, ...) are usable.
	use super::paging;
	// Publish the direct-map offset so the portable subsystems (heap, ELF loader,
	// ...) reach physical frames the same way this backend does (phys | KOFF).
	crate::mem::set_hhdm_offset(paging::KERNEL_VA_OFFSET);
	let (region_base, region_len) = paging::usable_region(ram_top);
	let regions = [bootproto::MemRegion { base: region_base, length: region_len, kind: bootproto::MEM_USABLE, _pad: 0 }];
	crate::mem::frame::init(&regions);
	crate::serial_println!("aarch64: frame allocator up - {} MB free DRAM", paging::frames_free() * 4 / 1024);
	crate::mem::heap::init();
	crate::mem::frame::upgrade_to_heap();
	// Retain the boot memory map now the heap is up, so SYS_MEMMAP_GET (lsmem) can
	// report the physical layout - the x86 loader path retains it inside mem::init.
	crate::mem::retain_memmap(&regions);
	// Bring up the QEMU ramfb early framebuffer (from `-device ramfb`), so the kernel
	// draws the boot log to the display pixel-by-pixel like x86 - QEMU virt has no VGA,
	// so without ramfb the boot is serial-only. Runs after the heap + frame pool are up
	// (it allocates the framebuffer and the console grid). A no-op if fw-cfg / ramfb is
	// absent.
	init_ramfb_console(fwcfg_base);
	{
		let mut v: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
		for i in 0..8 {
			v.push(i * i);
		}
		let (mapped, free) = crate::mem::heap::stats();
		crate::serial_println!("aarch64: heap up - Vec sum={} | {} kB mapped, {} kB free", v.iter().sum::<u64>(), mapped / 1024, free / 1024);
	}

	// Prove the real 4 kB map_page works: map a fresh frame at a high (top-bit-set)
	// virtual address, write a pattern through it, and confirm it reads back both
	// via the high VA (TTBR1 walk) and via the frame's direct-map address.
	let frame = paging::alloc_frame().expect("aarch64: no frame for map test");
	let hva: u64 = 0xFFFF_8000_0000_0000;
	paging::map_page(hva, frame, paging::PRESENT | paging::WRITABLE | paging::NO_EXECUTE);
	let pattern: u64 = 0xCAFE_BABE_D00D_F00D;
	let (via_high, via_phys) = unsafe {
		core::ptr::write_volatile(hva as *mut u64, pattern);
		(core::ptr::read_volatile(hva as *const u64), core::ptr::read_volatile(paging::phys_to_virt(frame) as *const u64))
	};
	let ok = via_high == pattern && via_phys == pattern;
	crate::serial_println!("aarch64: map_page {hva:#x} -> {frame:#x} | high={via_high:#x} phys={via_phys:#x} = {}", if ok { "ok" } else { "FAIL" });

	// Enumerate the PCIe ECAM bus (heap is up, so scan can return a Vec).
	let devices = super::pci::scan();
	crate::serial_println!("aarch64: PCI - {} device(s) on the ECAM bus", devices.len());
	for d in &devices {
		crate::serial_println!("aarch64:   {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}:{:02x}", d.bus, d.dev, d.func, d.vendor, d.device_id, d.class, d.subclass);
	}

	// Resolve each virtio device's modern MMIO layout (assigns its BARs, then walks
	// its capability list for the common/notify/isr/device config structures).
	let virtio = super::pci::scan_virtio();
	crate::serial_println!("aarch64: virtio - {} device(s) resolved", virtio.len());
	for v in &virtio {
		crate::serial_println!("aarch64:   {} @ BAR{} phys={:#x} len={:#x} | common+{:#x} notify+{:#x}(x{}) isr+{:#x} device+{:#x}", super::pci::virtio_type_name(v.virtio_type), v.bar, v.bar_phys, v.region_len, v.common.offset, v.notify.offset, v.notify.notify_multiplier, v.isr.offset, v.device.offset);
	}

	// If a virtio-blk device is present, read sector 0 to confirm the driver works.
	// The device is NOT written here: once userspace is up its virtio_blk driver +
	// StorageService own the disk (the system volume), so the kernel must not touch
	// its contents.
	if let Some(blk) = virtio.iter().find(|v| v.virtio_type as u32 == abi::VIRTIO_TYPE_BLOCK) {
		if let Some(mut disk) = super::virtio_blk::BlkDevice::init(blk) {
			let mut buf = [0u8; 512];
			if disk.read(0, &mut buf) {
				crate::serial_println!("aarch64: virtio-blk sector 0 read - first16={:02x?}", &buf[..16]);
			} else {
				crate::serial_println!("aarch64: virtio-blk sector 0 read - FAILED");
			}
		} else {
			crate::serial_println!("aarch64: virtio-blk init - FAILED");
		}
	}

	// Clocks + entropy: the generic timer (monotonic), the PL031 RTC (wall clock),
	// and the seeded RNG.
	super::tsc::init();
	let mut rnd = [0u8; 6];
	super::random::fill(&mut rnd);
	crate::serial_println!("aarch64: clocks - timer {} MHz, uptime {} ms, RTC unix {} | random {:02x?}", super::tsc::hz() / 1_000_000, super::tsc::cycles_to_ns(super::tsc::now()) / 1_000_000, super::rtc::read_unix(), rnd);

	// Per-CPU block for the boot core, reachable through TPIDR_EL1.
	let mpidr: u64;
	unsafe {
		core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
	}
	super::percpu::allocate(cpu_count as usize);
	super::percpu::init(0, mpidr as u32);
	let cpu = super::percpu::this_cpu();
	crate::serial_println!("aarch64: per-CPU up (TPIDR_EL1) cpu_id={} mpidr={:#x} of {} CPU(s)", cpu.cpu_id(), cpu.lapic_id() & 0xff_ffff, cpu_count);

	// Wake the secondary cores via PSCI CPU_ON (each brings up its own per-CPU
	// block + local GIC/timer, then idles).
	super::psci::bring_up_secondaries(cpu_count);

	// The portable scheduler on top of the arch context/percpu contract, sized for
	// every online core so a secondary's timer tick indexes its own (empty) run queue
	// rather than running off the end. The same scheduler the x86_64/riscv64 kernels
	// use - the aarch64 arch backend (context switch, per-CPU, read/write_cr3, timer)
	// satisfies its whole contract.
	crate::smp::set_cpu_count(cpu_count as usize);
	crate::sched::allocate(cpu_count as usize);

	// Under `cargo test`, give the kernel address space a fresh, empty low (TTBR0) half
	// before sched::init captures it as KERNEL_CR3: the boot identity map's low half is
	// 1 GB blocks, but the ring-3 test probes map 4 kB USER pages into the shared address
	// space (the x86 kernel's low half is 4 kB-granular and empty). The kernel runs
	// entirely in the higher half (TTBR1), so swapping TTBR0 is safe; kernel threads and
	// their EL0 excursions then use this table, and each probe's low-VA map_page lands in
	// clean 4 kB entries.
	#[cfg(test)]
	unsafe {
		super::context::write_cr3(super::paging::new_address_space().expect("aarch64: test address space"));
	}

	crate::sched::init();

	// Under `cargo test`, the core subsystems (heap, paging, per-CPU, SMP, scheduler)
	// are up: populate the device table + boot info, hand off to the kernel test
	// harness, and exit QEMU. The production userspace boot chain below is the
	// interactive (non-test) bring-up.
	#[cfg(test)]
	{
		crate::device::init();
		publish_embedded_boot_info();
		crate::test_main();
		super::exit_qemu(true)
	}

	// Production boot (the interactive, non-test path): the GIC + generic timer are
	// already armed and interrupts enabled above, and the EL1 SVC vectors are
	// installed, so bring up the real userspace boot chain (SystemManager -> the
	// service set -> the interactive shell) and idle on the interrupt-driven console
	// loop. The same clean sequence the x86_64/riscv64 kernels run - no port demos.
	#[cfg(not(test))]
	{
		run_system_manager();
		crate::serial_println!("aarch64: halting");
		super::halt_loop()
	}
}

// The QEMU ramfb framebuffer set up at boot (None if fw-cfg / ramfb is absent), read by
// publish_embedded_boot_info to fill the BootInfo framebuffer for a userspace consumer.
static BOOT_FB: crate::sync::SpinLock<Option<crate::arch::common::fwcfg::RamFb>> = crate::sync::SpinLock::new(None);

// Program the ramfb early framebuffer over fw-cfg and bring up the kernel framebuffer
// console on it, so the boot log is drawn to the display (XRGB8888: red at bit 16,
// green at 8, blue at 0). Serial-only if fw-cfg / ramfb is not present.
fn init_ramfb_console(fwcfg_base: u64) {
	let Some(fb) = crate::arch::common::fwcfg::setup_ramfb(fwcfg_base, 1280, 800, super::paging::phys_to_virt) else {
		return;
	};
	crate::console::init(crate::console::FbInfo { addr: super::paging::phys_to_virt(fb.phys) as *mut u8, width: fb.width as usize, height: fb.height as usize, pitch: fb.stride as usize, bytes_per_pixel: 4, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8 });
	*BOOT_FB.lock() = Some(fb);
	crate::serial_println!("aarch64: ramfb framebuffer {}x{} at {:#x}", fb.width, fb.height, fb.phys);
}

// Publish a kernel-constructed BootInfo pointing at the embedded init.pkg / volume.pkg
// (aarch64 boots directly, with no bootloader hand-off, so the kernel builds its own).
// Both the userspace boot chain and the test harness read the packages through it
// (module_bytes -> volume_package_bytes / init_package_bytes).
fn publish_embedded_boot_info() {
	// A boot-info module descriptor for an embedded package (its kernel .rodata
	// address is directly readable, so no HHDM translation is needed).
	fn module(name: &[u8], bytes: &[u8]) -> bootproto::Module {
		let mut nm = [0u8; 32];
		nm[..name.len()].copy_from_slice(name);
		bootproto::Module { addr: bytes.as_ptr() as u64, size: bytes.len() as u64, name: nm }
	}
	let modules: &'static mut [bootproto::Module; 2] = alloc::boxed::Box::leak(alloc::boxed::Box::new([module(b"init.pkg", INIT_PKG), module(b"volume.pkg", VOLUME_PKG)]));
	// Hand the ramfb framebuffer (if any) to a userspace consumer of the boot info.
	let (framebuffer, fb_present) = match *BOOT_FB.lock() {
		Some(f) => (bootproto::Framebuffer { addr: super::paging::phys_to_virt(f.phys), width: f.width, height: f.height, pitch: f.stride, bpp: 32, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8, _pad: [0; 2] }, 1u32),
		None => (bootproto::Framebuffer { addr: 0, width: 0, height: 0, pitch: 0, bpp: 0, red_shift: 0, red_size: 0, green_shift: 0, green_size: 0, blue_shift: 0, blue_size: 0, _pad: [0; 2] }, 0u32),
	};
	let bi: &'static bootproto::BootInfo = alloc::boxed::Box::leak(alloc::boxed::Box::new(bootproto::BootInfo { magic: bootproto::MAGIC, version: bootproto::VERSION, _pad0: 0, hhdm_offset: super::paging::KERNEL_VA_OFFSET, memmap: 0, memmap_len: 0, modules: modules.as_ptr() as u64, modules_len: modules.len() as u64, framebuffer, fb_present, _pad1: 0, rsdp: 0, smp_trampoline: 0 }));
	crate::publish_boot_info(bi);
}

// Spawn the real SystemManager from the embedded init package and drive the
// userspace boot chain as far as it runs, draining its reports. This is the same
// mechanism the x86 kernel uses (pkg::Package + loader::spawn_elf_process + the
// PACKAGE/RAMDISK/MODE bootstrap protocol); the kernel builds a BootInfo pointing
// at the embedded packages so crate::spawn_system_manager finds them. A userspace
// fault is isolated (the process is terminated), so the kernel always returns here.
#[cfg(not(test))]
fn run_system_manager() {
	if INIT_PKG.is_empty() || VOLUME_PKG.is_empty() {
		return;
	}

	// Populate the kernel device table from the PCI scan, so DeviceManager can
	// enumerate the virtio devices and spawn their drivers (the same one-time boot
	// scan the x86 kmain does before starting userspace).
	crate::device::init();

	publish_embedded_boot_info();

	match crate::spawn_system_manager() {
		Ok((ep, koid)) => {
			crate::serial_println!("aarch64: system - SystemManager spawned (koid {koid}), bringing up userspace");
			// Drive the boot chain until the interactive shell attaches (the last
			// component to come up), draining its reports as they arrive: run the
			// scheduler to quiescence, then let the timer advance (idle_halt) so
			// periodic / timed waiters wake and the next service starts. The cap is
			// generous so the loop always returns even if a component never settles.
			for _ in 0..400 {
				crate::sched::run_until_idle();
				while let Ok(msg) = ep.recv() {
					crate::serial_println!("aarch64: userspace: {}", core::str::from_utf8(&msg.bytes).unwrap_or("<bad>"));
				}
				if crate::console_input::shell_listening() {
					break;
				}
				super::idle_halt();
			}
			crate::serial_println!("aarch64: system - userspace boot chain settled");
			// Hand control to the interactive shell over the serial console: the shell
			// registered a console channel during bring-up, and this pumps polled PL011
			// keystrokes to it (running the cooperative schedule after each) until the
			// user types `exit`. The same portable driver the x86 kernel hands off to.
			crate::console_shell_loop();
		}
		Err(reason) => {
			crate::serial_println!("aarch64: system - SystemManager failed to start: {reason}");
		}
	}
}
