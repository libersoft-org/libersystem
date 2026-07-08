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

// The `echo` userspace tool, cross-compiled for aarch64 and embedded by build.rs
// (empty when the userspace was not built first, e.g. a bare `cargo build`).
const ECHO_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/echo_demo.elf"));

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
	let (ram_top, cpu_count) = match boot_info {
		Some(bi) => {
			crate::serial_println!("aarch64: DTB parsed - RAM {:#x}..{:#x} ({} MB), {} CPU(s)", bi.ram_base, bi.ram_base + bi.ram_size, bi.ram_size / (1024 * 1024), bi.cpu_count);
			// The boot stub already maps the 256 GB device region (BOOT_L1[256]), so
			// the PCIe ECAM is reachable through phys_to_virt; just point PCI at it.
			if bi.pcie_ecam != 0 {
				super::pci::set_ecam_base(bi.pcie_ecam);
			}
			(bi.ram_base + bi.ram_size, bi.cpu_count)
		}
		None => {
			crate::serial_println!("aarch64: no DTB found - using built-in defaults");
			(0, 1)
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

	// Cooperative context switch: spin up two kernel threads that ping-pong via
	// switch_context, then thread A returns control here.
	let (a_sp, b_sp) = unsafe { (super::context::init_thread_stack(&mut *(&raw mut STACK_A), thread_a, 0xAA), super::context::init_thread_stack(&mut *(&raw mut STACK_B), thread_b, 0xBB)) };
	unsafe {
		A_SP = a_sp;
		B_SP = b_sp;
	}
	crate::serial_println!("aarch64: context switch - starting kernel threads");
	unsafe {
		super::context::switch_context(&raw mut MAIN_SP, A_SP);
	}
	crate::serial_println!("aarch64: context switch - returned to boot core");

	// The portable scheduler: bring up crate::sched on the boot core and run three
	// real kernel threads (each a Thread in its own Process in the kernel address
	// space, accounted to the root Domain) cooperatively to completion. This is the
	// same scheduler the x86_64 kernel uses - the aarch64 arch backend (context
	// switch, per-CPU, read/write_cr3, timer) now satisfies its whole contract.
	// The scheduler is sized for every online core so a secondary's timer tick
	// indexes its own (empty) run queue rather than running off the end.
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
	// are up: hand off to the kernel test harness and exit QEMU. The scheduler demos
	// and the userspace boot chain are the interactive (non-test) bring-up in
	// aarch64_run_demos below.
	#[cfg(test)]
	{
		crate::device::init();
		publish_embedded_boot_info();
		crate::test_main();
		super::exit_qemu(true)
	}

	#[cfg(not(test))]
	aarch64_run_demos()
}

// The interactive scheduler demos and userspace boot chain that follow the core
// bring-up. Skipped under `cargo test` (which runs the kernel test harness instead).
#[cfg(not(test))]
fn aarch64_run_demos() -> ! {
	use super::paging;
	for id in 1..=3u64 {
		crate::sched::spawn(sched_task, id);
	}
	crate::serial_println!("aarch64: portable scheduler - draining 3 kernel threads");
	crate::sched::run_until_idle();
	crate::serial_println!("aarch64: portable scheduler - all threads exited");

	// Preemptive scheduling: three threads that never yield - each busy-waits
	// ~15 ms per step (longer than the 10 ms timer quantum), so the GIC timer IRQ
	// rotates them via sched::on_timer_preempt. Interleaved output = preemption.
	for id in 1..=3u64 {
		crate::sched::spawn(preempt_task, id);
	}
	crate::serial_println!("aarch64: preemptive scheduler - 3 non-yielding threads");
	crate::sched::run_until_idle();
	crate::serial_println!("aarch64: preemptive scheduler - all threads exited");

	// Real userspace: two portable Processes whose EL0 programs call the REAL
	// kernel syscall table (SYS_DEBUG_WRITE) and interleave via SYS_YIELD. Each
	// thread parks its EL0 resume state in its own slot, so the excursions coexist.
	run_user_processes();

	// Capability IPC from EL0: a user process creates a channel and round-trips a
	// message through SYS_CHANNEL_CREATE / SEND / RECV, then prints what it got.
	run_ipc_process();

	// Cross-process capability IPC: a sender process and a receiver process, each
	// endowed with one endpoint of a channel as its bootstrap handle. The sender
	// sends a message; the receiver waits, receives, and prints it.
	run_cross_process_ipc();

	// Capability delegation: the giver process makes a private channel, puts a
	// secret in it, and TRANSFERS one endpoint to the taker over the control
	// channel. The taker can only read the secret because it received the
	// capability - proving handle transfer between isolated processes.
	run_handle_transfer();

	// Portable ELF loader: build a minimal aarch64 ELF (ET_EXEC, EM_AARCH64) with
	// one low-VA PT_LOAD segment, load it through the SAME crate::elf loader the
	// x86 kernel uses, and run it as a real EL0 process. This is a payoff of the
	// higher-half relink: standard low virtual addresses are now free for user ELFs.
	run_elf_process();

	// Real userspace binary: load the `echo` tool - cross-compiled from the actual
	// userspace tree with the shared `rt` runtime - and run it as an EL0 process,
	// acting as its launcher (a bootstrap channel + the stdout/argv messages rt
	// expects). This exercises the whole real userspace path: the rt ABI handshake
	// (SYS_ABI_CHECK over svc), the rt syscall wrapper, and the program's own logic.
	run_echo_program();

	// The real system: spawn SystemManager from the embedded init package (the same
	// path the x86 kernel uses - pkg::Package + loader::spawn_elf_process) and let it
	// bring up the userspace service tree as far as it goes, draining its boot-chain
	// reports. A no-op when the packages were not staged.
	run_system_manager();

	// Per-address-space isolation: two independent address spaces map the SAME
	// virtual address to different physical frames. Switching TTBR0 (write_cr3)
	// changes what that address reads - the kernel keeps running because each
	// space carries the kernel identity. The user-region tables + frames are then
	// freed back to the allocator.
	let as1 = paging::new_address_space().expect("aarch64: no frame for AS1");
	let as2 = paging::new_address_space().expect("aarch64: no frame for AS2");
	let f1 = paging::alloc_frame().expect("aarch64: no frame for AS1 page");
	let f2 = paging::alloc_frame().expect("aarch64: no frame for AS2 page");
	let shared_va: u64 = 0x2_0000_0000; // 8 GiB - in the free per-AS region
	paging::map_page_in(as1, shared_va, f1, paging::PRESENT | paging::WRITABLE | paging::NO_EXECUTE);
	paging::map_page_in(as2, shared_va, f2, paging::PRESENT | paging::WRITABLE | paging::NO_EXECUTE);
	let boot_ttbr0 = super::context::read_cr3();
	let (r1, r2) = unsafe {
		super::context::write_cr3(as1);
		core::ptr::write_volatile(shared_va as *mut u64, 0x1111_1111);
		super::context::write_cr3(as2);
		core::ptr::write_volatile(shared_va as *mut u64, 0x2222_2222);
		super::context::write_cr3(as1);
		let a = core::ptr::read_volatile(shared_va as *const u64);
		super::context::write_cr3(as2);
		let b = core::ptr::read_volatile(shared_va as *const u64);
		super::context::write_cr3(boot_ttbr0);
		(a, b)
	};
	let iso_ok = r1 == 0x1111_1111 && r2 == 0x2222_2222;
	crate::serial_println!("aarch64: address spaces {as1:#x}/{as2:#x} @ {shared_va:#x} -> {r1:#x}/{r2:#x} = {}", if iso_ok { "isolated" } else { "FAIL" });
	paging::unmap_page_in(as1, shared_va);
	paging::unmap_page_in(as2, shared_va);
	paging::dealloc_frame(f1);
	paging::dealloc_frame(f2);
	paging::free_address_space(as1);
	paging::free_address_space(as2);
	crate::serial_println!("aarch64: address spaces torn down");

	crate::serial_println!("aarch64 bring-up: serial + MMU + vectors + GIC/timer + paging + heap + pci + percpu + SMP + threads + EL0 + ELF + addrspace OK - halting");
	super::halt_loop()
}

// Cooperative context-switch demo: two kernel threads ping-pong via
// switch_context, then thread A hands control back to the boot path.
static mut MAIN_SP: u64 = 0;
static mut A_SP: u64 = 0;
static mut B_SP: u64 = 0;
static mut STACK_A: [u8; 8192] = [0; 8192];
static mut STACK_B: [u8; 8192] = [0; 8192];

extern "C" fn thread_a(arg: u64) {
	for i in 0..3 {
		crate::serial_println!("aarch64: thread A step {i} (arg={arg:#x})");
		unsafe {
			super::context::switch_context(&raw mut A_SP, B_SP);
		}
	}
	// Done - hand control back to the boot path.
	unsafe {
		super::context::switch_context(&raw mut A_SP, MAIN_SP);
	}
}

extern "C" fn thread_b(arg: u64) {
	loop {
		crate::serial_println!("aarch64: thread B step (arg={arg:#x})");
		unsafe {
			super::context::switch_context(&raw mut B_SP, A_SP);
		}
	}
}

// A portable-scheduler kernel thread: print a few times, yielding the core to the
// next ready thread between steps, then return (which retires it via sched::exit).
extern "C" fn sched_task(id: u64) {
	for i in 0..3 {
		crate::serial_println!("aarch64: [sched] thread {id} step {i}");
		crate::sched::yield_now();
	}
}

// A preemption test thread: busy-wait ~15 ms per step (no yield) so the 10 ms
// timer quantum forces a preemptive rotation; the interleaved output proves the
// timer IRQ drives the scheduler.
extern "C" fn preempt_task(id: u64) {
	for i in 0..3 {
		let target = super::tsc::now() + super::tsc::hz() * 15 / 1000;
		while super::tsc::now() < target {
			core::hint::spin_loop();
		}
		crate::serial_println!("aarch64: [preempt] thread {id} step {i}");
	}
}

// The kernel-side entry of the user process's thread: drop to EL0 at the mapped
// program. Returns when the program calls SYS_USER_EXIT (then the thread exits).
extern "C" fn user_trampoline(ctx_raw: u64) {
	let ctx = unsafe { alloc::boxed::Box::from_raw(ctx_raw as *mut UserCtx) };
	unsafe {
		super::usermode::enter(ctx.entry, ctx.stack_top, ctx.arg);
	}
}

struct UserCtx {
	entry: u64,
	stack_top: u64,
	arg: u64,
}

// Build a portable user Process and enqueue its EL0 thread. The program (assembled
// below) prints `msg` via SYS_DEBUG_WRITE, yields, prints + yields again, then
// SYS_USER_EXIT - so two such processes interleave through the scheduler, which
// only works because each thread parks its EL0 resume state in its own slot. Each
// process has its own address space, so both can use the same 64 TiB user VAs
// (above the low kernel identity, below USER_VA_END).
fn spawn_user_process(msg: &[u8]) {
	use crate::object::address_space::AddressSpace;
	use crate::object::process::Process;

	let addr_space = match AddressSpace::create() {
		Some(a) => a,
		None => {
			crate::serial_println!("aarch64: userspace - no address space");
			return;
		}
	};
	let code = super::paging::alloc_frame().expect("aarch64: no frame for user code");
	let stack = super::paging::alloc_frame().expect("aarch64: no frame for user stack");

	let code_va: u64 = 0x0000_4000_0000_0000; // 64 TiB
	let msg_va = code_va + 0x800;
	let stack_va: u64 = 0x0000_4000_0001_0000;
	let stack_top = stack_va + 0x1000;

	// x0 holds the message pointer (the entry argument); keep it in x19 across the
	// syscalls. MOVZ Xd,#imm and MOV Xd,Xm (ORR Xd,XZR,Xm) build the program.
	let movz = |rd: u32, imm: u16| -> u32 { 0xD280_0000 | ((imm as u32) << 5) | rd };
	let mov_reg = |rd: u32, rm: u32| -> u32 { 0xAA00_03E0 | (rm << 16) | rd };
	const SVC0: u32 = 0xD400_0001;
	let len = msg.len() as u16;
	let prog: [u32; 16] = [
		mov_reg(19, 0),                       // mov x19, x0  (save msg ptr)
		mov_reg(0, 19),                       // mov x0, x19
		movz(1, len),                         // mov x1, #len
		movz(8, abi::SYS_DEBUG_WRITE as u16), // mov x8, #SYS_DEBUG_WRITE
		SVC0,
		movz(8, abi::SYS_YIELD as u16), // mov x8, #SYS_YIELD
		SVC0,
		mov_reg(0, 19),
		movz(1, len),
		movz(8, abi::SYS_DEBUG_WRITE as u16),
		SVC0,
		movz(8, abi::SYS_YIELD as u16),
		SVC0,
		movz(8, abi::SYS_USER_EXIT as u16), // mov x8, #SYS_USER_EXIT
		SVC0,
		0x1400_0000, // b .
	];
	unsafe {
		let cp = code as *mut u32;
		for (i, w) in prog.iter().enumerate() {
			core::ptr::write_volatile(cp.add(i), *w);
		}
		core::ptr::copy_nonoverlapping(msg.as_ptr(), (code + 0x800) as *mut u8, msg.len());
		core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
	}

	addr_space.map(code_va, code, super::paging::PRESENT | super::paging::USER);
	addr_space.map(stack_va, stack, super::paging::PRESENT | super::paging::WRITABLE | super::paging::USER | super::paging::NO_EXECUTE);

	let process = Process::new(addr_space, crate::sched::root_domain());
	process.adopt_frames(alloc::vec![code, stack]);
	let ctx = alloc::boxed::Box::new(UserCtx { entry: code_va, stack_top, arg: msg_va });
	crate::sched::thread_create(process, user_trampoline, alloc::boxed::Box::into_raw(ctx) as u64);
}

// Run two concurrent user processes: they interleave via SYS_YIELD, each making
// real SYS_DEBUG_WRITE syscalls, proving per-thread EL0 excursions coexist.
fn run_user_processes() {
	spawn_user_process(b"userspace A: hello via SYS_DEBUG_WRITE\n");
	spawn_user_process(b"userspace B: hello via SYS_DEBUG_WRITE\n");
	crate::serial_println!("aarch64: userspace - running 2 EL0 processes");
	crate::sched::run_until_idle();
	crate::serial_println!("aarch64: userspace - both EL0 processes exited");
}

// Build a minimal aarch64 ELF in memory and run it through the PORTABLE ELF
// loader (crate::elf, the same the x86 kernel uses). The image is a single
// R+X PT_LOAD segment at a standard low virtual address (4 MiB) holding a short
// EL0 program (print a message via SYS_DEBUG_WRITE, then SYS_USER_EXIT) followed
// by the message bytes. This exercises the loader end to end and proves the
// higher-half relink freed the low half for standard low-VA user ELFs.
fn run_elf_process() {
	use crate::object::address_space::AddressSpace;
	use crate::object::process::Process;

	// The EL0 program: x0 already holds the message VA (the entry argument).
	let movz = |rd: u32, imm: u16| -> u32 { 0xD280_0000 | ((imm as u32) << 5) | rd };
	const SVC0: u32 = 0xD400_0001;
	let msg: &[u8] = b"hello from an ELF-loaded aarch64 process\n";
	let prog: [u32; 6] = [
		movz(1, msg.len() as u16),            // mov x1, #len
		movz(8, abi::SYS_DEBUG_WRITE as u16), // mov x8, #SYS_DEBUG_WRITE
		SVC0,
		movz(8, abi::SYS_USER_EXIT as u16), // mov x8, #SYS_USER_EXIT
		SVC0,
		0x1400_0000, // b .
	];

	const CODE_VA: u64 = 0x0040_0000; // 4 MiB - a standard low user vaddr
	const PH_OFF: usize = 64; // program header follows the 64-byte ELF header
	const SEG_OFF: usize = 120; // segment payload follows the 56-byte phdr
	let msg_off = prog.len() * 4;
	let seg_len = msg_off + msg.len();

	// Hand-build the 64-bit little-endian ELF: header + one PT_LOAD phdr + payload.
	let mut elf = alloc::vec![0u8; SEG_OFF + seg_len];
	elf[0..4].copy_from_slice(b"\x7fELF");
	elf[4] = 2; // ELFCLASS64
	elf[5] = 1; // ELFDATA2LSB
	elf[6] = 1; // EV_CURRENT
	elf[16..18].copy_from_slice(&2u16.to_le_bytes()); // e_type = ET_EXEC
	elf[18..20].copy_from_slice(&0xb7u16.to_le_bytes()); // e_machine = EM_AARCH64
	elf[20..24].copy_from_slice(&1u32.to_le_bytes()); // e_version
	elf[24..32].copy_from_slice(&CODE_VA.to_le_bytes()); // e_entry
	elf[32..40].copy_from_slice(&(PH_OFF as u64).to_le_bytes()); // e_phoff
	elf[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
	elf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
	elf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
	elf[PH_OFF..PH_OFF + 4].copy_from_slice(&1u32.to_le_bytes()); // p_type = PT_LOAD
	elf[PH_OFF + 4..PH_OFF + 8].copy_from_slice(&5u32.to_le_bytes()); // p_flags = R|X
	elf[PH_OFF + 8..PH_OFF + 16].copy_from_slice(&(SEG_OFF as u64).to_le_bytes()); // p_offset
	elf[PH_OFF + 16..PH_OFF + 24].copy_from_slice(&CODE_VA.to_le_bytes()); // p_vaddr
	elf[PH_OFF + 24..PH_OFF + 32].copy_from_slice(&CODE_VA.to_le_bytes()); // p_paddr
	elf[PH_OFF + 32..PH_OFF + 40].copy_from_slice(&(seg_len as u64).to_le_bytes()); // p_filesz
	elf[PH_OFF + 40..PH_OFF + 48].copy_from_slice(&(seg_len as u64).to_le_bytes()); // p_memsz
	elf[PH_OFF + 48..PH_OFF + 56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align
	for (i, w) in prog.iter().enumerate() {
		elf[SEG_OFF + i * 4..SEG_OFF + i * 4 + 4].copy_from_slice(&w.to_le_bytes());
	}
	elf[SEG_OFF + msg_off..SEG_OFF + msg_off + msg.len()].copy_from_slice(msg);

	let addr_space = match AddressSpace::create() {
		Some(a) => a,
		None => {
			crate::serial_println!("aarch64: ELF - no address space");
			return;
		}
	};
	let mut frames: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
	let entry = match crate::elf::load_into(&elf, &addr_space, &mut frames) {
		Ok(e) => e,
		Err(_) => {
			crate::serial_println!("aarch64: ELF - load failed");
			for f in frames {
				super::paging::dealloc_frame(f);
			}
			return;
		}
	};
	// The loader wrote code through the data path; invalidate the I-cache so the
	// freshly loaded instructions are fetched from memory.
	unsafe {
		core::arch::asm!("ic iallu", "dsb ish", "isb", options(nostack, preserves_flags));
	}

	// The loader maps only PT_LOAD segments; map a user stack separately.
	let stack = super::paging::alloc_frame().expect("aarch64: no frame for ELF stack");
	let stack_va: u64 = 0x0080_0000; // 8 MiB
	let stack_top = stack_va + 0x1000;
	addr_space.map(stack_va, stack, super::paging::PRESENT | super::paging::WRITABLE | super::paging::USER | super::paging::NO_EXECUTE);
	frames.push(stack);

	let msg_va = CODE_VA + msg_off as u64;
	let process = Process::new(addr_space, crate::sched::root_domain());
	process.adopt_frames(frames);
	let ctx = alloc::boxed::Box::new(UserCtx { entry, stack_top, arg: msg_va });
	crate::sched::thread_create(process, user_trampoline, alloc::boxed::Box::into_raw(ctx) as u64);
	crate::serial_println!("aarch64: ELF - loading + running a low-VA aarch64 ELF (entry {entry:#x})");
	crate::sched::run_until_idle();
	crate::serial_println!("aarch64: ELF - process exited");
}

// Load and run the real `echo` tool (built from the userspace tree with the shared
// rt runtime, embedded by build.rs). The kernel acts as its launcher: it seeds the
// process with a bootstrap channel and sends the two messages rt expects - a STDOUT
// message (no console handle, so echo's output falls back to the debug port) and
// the argument line. echo then does the rt ABI handshake, receives the line, and
// prints it via SYS_DEBUG_WRITE. A no-op when the ELF was not embedded.
fn run_echo_program() {
	use crate::object::address_space::AddressSpace;
	use crate::object::channel::{Channel, Message};
	use crate::object::process::Process;
	use crate::object::rights::Rights;

	if ECHO_ELF.is_empty() {
		return;
	}

	let addr_space = match AddressSpace::create() {
		Some(a) => a,
		None => {
			crate::serial_println!("aarch64: echo - no address space");
			return;
		}
	};
	let mut frames: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
	let entry = match crate::elf::load_into(ECHO_ELF, &addr_space, &mut frames) {
		Ok(e) => e,
		Err(_) => {
			crate::serial_println!("aarch64: echo - ELF load failed");
			for f in frames {
				super::paging::dealloc_frame(f);
			}
			return;
		}
	};
	unsafe {
		core::arch::asm!("ic iallu", "dsb ish", "isb", options(nostack, preserves_flags));
	}

	// User stack (the loader maps only PT_LOAD segments).
	let stack = super::paging::alloc_frame().expect("aarch64: no frame for echo stack");
	let stack_va: u64 = 0x7fff_0000;
	let stack_top = stack_va + 0x1000;
	addr_space.map(stack_va, stack, super::paging::PRESENT | super::paging::WRITABLE | super::paging::USER | super::paging::NO_EXECUTE);
	frames.push(stack);

	// Bootstrap channel: the process holds ep1; the kernel keeps ep0 and, as the
	// launcher, sends the stdout + argument messages the rt runtime consumes.
	let (ep0, ep1) = Channel::create();
	let process = Process::new(addr_space, crate::sched::root_domain());
	process.adopt_frames(frames);
	let bootstrap = process.install(ep1, Rights::ALL, 0);
	// STDOUT message with no console handle -> rt keeps the debug-port fallback.
	let _ = ep0.send(Message::new(alloc::vec::Vec::from(&b"STDOUT"[..]), alloc::vec::Vec::new(), 0));
	// The argument line echo prints.
	let _ = ep0.send(Message::new(alloc::vec::Vec::from(&b"echo running from a real aarch64 ELF"[..]), alloc::vec::Vec::new(), 0));

	let ctx = alloc::boxed::Box::new(UserCtx { entry, stack_top, arg: bootstrap });
	crate::sched::thread_create(process, user_trampoline, alloc::boxed::Box::into_raw(ctx) as u64);
	crate::serial_println!("aarch64: echo - running the real echo tool (entry {entry:#x})");
	crate::sched::run_until_idle();
	crate::serial_println!("aarch64: echo - tool exited");
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
	let bi: &'static bootproto::BootInfo = alloc::boxed::Box::leak(alloc::boxed::Box::new(bootproto::BootInfo { magic: bootproto::MAGIC, version: bootproto::VERSION, _pad0: 0, hhdm_offset: super::paging::KERNEL_VA_OFFSET, memmap: 0, memmap_len: 0, modules: modules.as_ptr() as u64, modules_len: modules.len() as u64, framebuffer: bootproto::Framebuffer { addr: 0, width: 0, height: 0, pitch: 0, bpp: 0, red_shift: 0, red_size: 0, green_shift: 0, green_size: 0, blue_shift: 0, blue_size: 0, _pad: [0; 2] }, fb_present: 0, _pad1: 0, rsdp: 0, smp_trampoline: 0 }));
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
			// Drive the boot chain: run the scheduler to quiescence, drain any
			// reports, then let the timer advance (idle_halt) so periodic / timed
			// waiters wake and the next service starts. Bounded so the demo always
			// returns even if the system keeps a periodic housekeeping tick going.
			for _ in 0..400 {
				crate::sched::run_until_idle();
				while let Ok(msg) = ep.recv() {
					crate::serial_println!("aarch64: userspace: {}", core::str::from_utf8(&msg.bytes).unwrap_or("<bad>"));
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

// A single user process that exercises capability IPC entirely from EL0: it
// creates a channel (two endpoint handles), sends a message on endpoint 0,
// receives it on endpoint 1, and prints what it received via SYS_DEBUG_WRITE - a
// full round-trip through the real channel syscalls. Page layout (in the code
// frame): program@0, endpoint handles@0x400/0x408, send message@0x500, receive
// buffer@0x600. The entry argument (x0) is the code page base.
fn run_ipc_process() {
	use crate::object::address_space::AddressSpace;
	use crate::object::process::Process;

	let addr_space = match AddressSpace::create() {
		Some(a) => a,
		None => {
			crate::serial_println!("aarch64: IPC - no address space");
			return;
		}
	};
	let code = super::paging::alloc_frame().expect("aarch64: no frame for IPC code");
	let stack = super::paging::alloc_frame().expect("aarch64: no frame for IPC stack");

	let code_va: u64 = 0x0000_4000_0000_0000;
	let stack_va: u64 = 0x0000_4000_0001_0000;
	let stack_top = stack_va + 0x1000;

	let msg = b"capability IPC round-trip from EL0!\n";
	let len = msg.len() as u16;

	// Instruction encoders.
	let movz = |rd: u32, imm: u16| -> u32 { 0xD280_0000 | ((imm as u32) << 5) | rd };
	let mov_reg = |rd: u32, rm: u32| -> u32 { 0xAA00_03E0 | (rm << 16) | rd };
	let add_imm = |rd: u32, rn: u32, imm: u32| -> u32 { 0x9100_0000 | (imm << 10) | (rn << 5) | rd };
	let ldr = |rt: u32, rn: u32, off: u32| -> u32 { 0xF940_0000 | ((off / 8) << 10) | (rn << 5) | rt };
	const SVC0: u32 = 0xD400_0001;

	let prog: [u32; 26] = [
		mov_reg(19, 0), // mov x19, x0   (x19 = code base)
		// SYS_CHANNEL_CREATE(&h0=base+0x400, &h1=base+0x408, depth=0)
		add_imm(0, 19, 0x400),
		add_imm(1, 19, 0x408),
		movz(2, 0),
		movz(8, abi::SYS_CHANNEL_CREATE as u16),
		SVC0,
		// SYS_CHANNEL_SEND(h0, msg=base+0x500, len, xfer=0)
		ldr(0, 19, 0x400),
		add_imm(1, 19, 0x500),
		movz(2, len),
		movz(3, 0),
		movz(8, abi::SYS_CHANNEL_SEND as u16),
		SVC0,
		// SYS_CHANNEL_RECV(h1, buf=base+0x600, cap=0x100, out_handle=0)
		ldr(0, 19, 0x408),
		add_imm(1, 19, 0x600),
		movz(2, 0x100),
		movz(3, 0),
		movz(8, abi::SYS_CHANNEL_RECV as u16),
		SVC0,
		// SYS_DEBUG_WRITE(buf=base+0x600, n=recv result)
		mov_reg(20, 0),
		add_imm(0, 19, 0x600),
		mov_reg(1, 20),
		movz(8, abi::SYS_DEBUG_WRITE as u16),
		SVC0,
		// SYS_USER_EXIT
		movz(8, abi::SYS_USER_EXIT as u16),
		SVC0,
		0x1400_0000, // b .
	];
	unsafe {
		let cp = code as *mut u32;
		for (i, w) in prog.iter().enumerate() {
			core::ptr::write_volatile(cp.add(i), *w);
		}
		core::ptr::copy_nonoverlapping(msg.as_ptr(), (code + 0x500) as *mut u8, msg.len());
		core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
	}

	addr_space.map(code_va, code, super::paging::PRESENT | super::paging::WRITABLE | super::paging::USER);
	addr_space.map(stack_va, stack, super::paging::PRESENT | super::paging::WRITABLE | super::paging::USER | super::paging::NO_EXECUTE);

	let process = Process::new(addr_space, crate::sched::root_domain());
	process.adopt_frames(alloc::vec![code, stack]);
	let ctx = alloc::boxed::Box::new(UserCtx { entry: code_va, stack_top, arg: code_va });
	crate::sched::thread_create(process, user_trampoline, alloc::boxed::Box::into_raw(ctx) as u64);

	crate::serial_println!("aarch64: IPC - entering EL0 process");
	crate::sched::run_until_idle();
	crate::serial_println!("aarch64: IPC - EL0 process exited");
}

// Build a user Process running `prog` (its own address space, code+stack at 64 TiB
// VAs), endowed with `endpoint` as its bootstrap handle (delivered as the entry
// argument x0). Each `(offset, bytes)` in `data` is written into the (writable)
// code page, so a program can reference constant strings and use scratch slots.
fn build_ipc_endpoint_process(prog: &[u32], data: &[(u64, &[u8])], endpoint: alloc::sync::Arc<dyn crate::object::KernelObject>) {
	use crate::object::address_space::AddressSpace;
	use crate::object::process::Process;
	use crate::object::rights::Rights;

	let addr_space = match AddressSpace::create() {
		Some(a) => a,
		None => {
			crate::serial_println!("aarch64: xIPC - no address space");
			return;
		}
	};
	let code = super::paging::alloc_frame().expect("aarch64: no frame for xIPC code");
	let stack = super::paging::alloc_frame().expect("aarch64: no frame for xIPC stack");
	let code_va: u64 = 0x0000_4000_0000_0000;
	let stack_va: u64 = 0x0000_4000_0001_0000;
	let stack_top = stack_va + 0x1000;

	unsafe {
		let cp = code as *mut u32;
		for (i, w) in prog.iter().enumerate() {
			core::ptr::write_volatile(cp.add(i), *w);
		}
		for (off, bytes) in data {
			core::ptr::copy_nonoverlapping(bytes.as_ptr(), (code + off) as *mut u8, bytes.len());
		}
		core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
	}

	addr_space.map(code_va, code, super::paging::PRESENT | super::paging::WRITABLE | super::paging::USER);
	addr_space.map(stack_va, stack, super::paging::PRESENT | super::paging::WRITABLE | super::paging::USER | super::paging::NO_EXECUTE);

	let process = Process::new(addr_space, crate::sched::root_domain());
	process.adopt_frames(alloc::vec![code, stack]);
	let handle = process.install(endpoint, Rights::ALL, 0);
	let ctx = alloc::boxed::Box::new(UserCtx { entry: code_va, stack_top, arg: handle });
	crate::sched::thread_create(process, user_trampoline, alloc::boxed::Box::into_raw(ctx) as u64);
}

// Cross-process capability IPC: create a channel, hand one endpoint to a sender
// process and the other to a receiver process (each as its bootstrap handle). The
// sender does SYS_CHANNEL_SEND; the receiver SYS_WAITs, SYS_CHANNEL_RECVs, and
// prints the message. Both programs materialize their code base with a shifted
// MOVZ (the entry argument x0 is their handle, not the base).
fn run_cross_process_ipc() {
	use crate::object::channel::Channel;

	let movz = |rd: u32, imm: u16| -> u32 { 0xD280_0000 | ((imm as u32) << 5) | rd };
	let movz_sh = |rd: u32, imm: u16, hw: u32| -> u32 { 0xD280_0000 | (hw << 21) | ((imm as u32) << 5) | rd };
	let mov_reg = |rd: u32, rm: u32| -> u32 { 0xAA00_03E0 | (rm << 16) | rd };
	let add_imm = |rd: u32, rn: u32, imm: u32| -> u32 { 0x9100_0000 | (imm << 10) | (rn << 5) | rd };
	const SVC0: u32 = 0xD400_0001;

	let msg = b"cross-process capability IPC on aarch64!\n";
	let len = msg.len() as u16;

	// Sender: SYS_CHANNEL_SEND(handle=x0, msg=base+0x500, len, xfer=0), then exit.
	let sender: [u32; 11] = [
		mov_reg(19, 0),         // x19 = handle
		movz_sh(20, 0x4000, 2), // x20 = code base (64 TiB)
		mov_reg(0, 19),         // x0 = handle
		add_imm(1, 20, 0x500),  // x1 = &msg
		movz(2, len),           // x2 = len
		movz(3, 0),             // x3 = xfer (none)
		movz(8, abi::SYS_CHANNEL_SEND as u16),
		SVC0,
		movz(8, abi::SYS_USER_EXIT as u16),
		SVC0,
		0x1400_0000,
	];

	// Receiver: SYS_WAIT(handle,0,0); SYS_CHANNEL_RECV(handle, base+0x600, cap, 0);
	// SYS_DEBUG_WRITE(base+0x600, n); exit.
	let receiver: [u32; 21] = [
		mov_reg(19, 0),         // x19 = handle
		movz_sh(20, 0x4000, 2), // x20 = code base
		mov_reg(0, 19),
		movz(1, 0), // deadline = 0 (no timeout)
		movz(2, 0), // flags = 0
		movz(8, abi::SYS_WAIT as u16),
		SVC0,
		mov_reg(0, 19),
		add_imm(1, 20, 0x600), // x1 = &buf
		movz(2, 0x100),        // cap
		movz(3, 0),            // out_handle = none
		movz(8, abi::SYS_CHANNEL_RECV as u16),
		SVC0,
		mov_reg(21, 0),        // x21 = received length
		add_imm(0, 20, 0x600), // x0 = &buf
		mov_reg(1, 21),        // x1 = length
		movz(8, abi::SYS_DEBUG_WRITE as u16),
		SVC0,
		movz(8, abi::SYS_USER_EXIT as u16),
		SVC0,
		0x1400_0000,
	];

	let (ep0, ep1) = Channel::create();
	crate::serial_println!("aarch64: xIPC - spawning sender + receiver processes");
	build_ipc_endpoint_process(&sender, &[(0x500, msg)], ep0);
	build_ipc_endpoint_process(&receiver, &[], ep1);
	crate::sched::run_until_idle();
	crate::serial_println!("aarch64: xIPC - done");
}

// Capability delegation via handle transfer. The GIVER process: creates a private
// "gift" channel, sends a secret into it, then sends a notice on the control
// channel with the gift's other endpoint as a TRANSFERRED capability. The TAKER
// process: receives the notice + the transferred handle on the control channel,
// then reads the secret through the received capability and prints it. The taker
// has no other way to reach the gift channel, so printing the secret proves the
// capability was delegated across the process boundary.
fn run_handle_transfer() {
	use crate::object::channel::Channel;

	let movz = |rd: u32, imm: u16| -> u32 { 0xD280_0000 | ((imm as u32) << 5) | rd };
	let movz_sh = |rd: u32, imm: u16, hw: u32| -> u32 { 0xD280_0000 | (hw << 21) | ((imm as u32) << 5) | rd };
	let mov_reg = |rd: u32, rm: u32| -> u32 { 0xAA00_03E0 | (rm << 16) | rd };
	let add_imm = |rd: u32, rn: u32, imm: u32| -> u32 { 0x9100_0000 | (imm << 10) | (rn << 5) | rd };
	let ldr = |rt: u32, rn: u32, off: u32| -> u32 { 0xF940_0000 | ((off / 8) << 10) | (rn << 5) | rt };
	const SVC0: u32 = 0xD400_0001;

	let secret = b"secret via a transferred capability!\n";
	let notice = b"gift enclosed";
	let slen = secret.len() as u16;
	let nlen = notice.len() as u16;

	// GIVER: create gift channel (&g0@0x400,&g1@0x408); send secret on g0; send
	// notice on the control handle (x19) transferring g1; exit.
	let giver: [u32; 22] = [
		mov_reg(19, 0),         // x19 = control handle
		movz_sh(20, 0x4000, 2), // x20 = code base
		add_imm(0, 20, 0x400),  // &g0
		add_imm(1, 20, 0x408),  // &g1
		movz(2, 0),             // depth
		movz(8, abi::SYS_CHANNEL_CREATE as u16),
		SVC0,
		ldr(0, 20, 0x400),     // g0
		add_imm(1, 20, 0x500), // secret
		movz(2, slen),
		movz(3, 0),
		movz(8, abi::SYS_CHANNEL_SEND as u16),
		SVC0,
		mov_reg(0, 19),        // control handle
		add_imm(1, 20, 0x550), // notice
		movz(2, nlen),
		ldr(3, 20, 0x408), // xfer = g1
		movz(8, abi::SYS_CHANNEL_SEND as u16),
		SVC0,
		movz(8, abi::SYS_USER_EXIT as u16),
		SVC0,
		0x1400_0000,
	];

	// TAKER: wait+recv on control (out_handle@0x700 = the transferred gift); then
	// wait+recv on the gift handle and print the secret; exit.
	let taker: [u32; 33] = [
		mov_reg(19, 0),         // x19 = control handle
		movz_sh(20, 0x4000, 2), // x20 = code base
		mov_reg(0, 19),
		movz(1, 0),
		movz(2, 0),
		movz(8, abi::SYS_WAIT as u16),
		SVC0,
		mov_reg(0, 19),
		add_imm(1, 20, 0x600), // notice buf
		movz(2, 0x100),
		add_imm(3, 20, 0x700), // &out_handle
		movz(8, abi::SYS_CHANNEL_RECV as u16),
		SVC0,
		ldr(19, 20, 0x700), // x19 = transferred gift handle
		mov_reg(0, 19),
		movz(1, 0),
		movz(2, 0),
		movz(8, abi::SYS_WAIT as u16),
		SVC0,
		mov_reg(0, 19),
		add_imm(1, 20, 0x680), // secret buf
		movz(2, 0x100),
		movz(3, 0),
		movz(8, abi::SYS_CHANNEL_RECV as u16),
		SVC0,
		mov_reg(21, 0), // x21 = length
		add_imm(0, 20, 0x680),
		mov_reg(1, 21),
		movz(8, abi::SYS_DEBUG_WRITE as u16),
		SVC0,
		movz(8, abi::SYS_USER_EXIT as u16),
		SVC0,
		0x1400_0000,
	];

	let (ctrl0, ctrl1) = Channel::create();
	crate::serial_println!("aarch64: hxfer - giver transfers a capability to the taker");
	build_ipc_endpoint_process(&giver, &[(0x500, secret), (0x550, notice)], ctrl0);
	build_ipc_endpoint_process(&taker, &[], ctrl1);
	crate::sched::run_until_idle();
	crate::serial_println!("aarch64: hxfer - done");
}
