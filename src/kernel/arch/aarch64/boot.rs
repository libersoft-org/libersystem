// aarch64 direct-boot entry (M116 bring-up).
//
// QEMU `-machine virt -kernel <elf>` enters the ELF entry `_start` with the MMU
// off, at EL1 (or EL2), on an undefined stack, with x0 = the DTB physical
// address. `_start` (below, in `.text.boot` so it lands first) sets up the boot
// stack, zeroes the BSS, then calls `aarch64_main` with the DTB pointer.
//
// This is the M0-equivalent for the new architecture: it brings up the PL011
// serial console and reports in, then halts. The real port - the MMU + higher
// half, the VBAR_EL1 vectors, the GIC + generic timer, PSCI SMP, the SVC syscall
// path, and routing through the portable `kmain` - fills in from here in M116.

use core::arch::global_asm;

global_asm!(
	r#"
.section .text.boot, "ax"
.global _start
_start:
	// x0 holds the DTB pointer QEMU passed; preserve it across BSS zeroing.
	mov     x19, x0
	// Set up the boot stack (grows down from the linker-reserved top).
	adrp    x1, __boot_stack_top
	add     x1, x1, :lo12:__boot_stack_top
	mov     sp, x1
	// Zero the BSS: [__bss_start, __bss_end).
	adrp    x0, __bss_start
	add     x0, x0, :lo12:__bss_start
	adrp    x1, __bss_end
	add     x1, x1, :lo12:__bss_end
0:
	cmp     x0, x1
	b.hs    1f
	str     xzr, [x0], #8
	b       0b
1:
	// aarch64_main(dtb) - never returns.
	mov     x0, x19
	bl      aarch64_main
2:
	wfe
	b       2b
"#
);

#[unsafe(no_mangle)]
extern "C" fn aarch64_main(dtb: u64) -> ! {
	super::serial::init();

	// Current exception level (CurrentEL bits [3:2]).
	let current_el: u64;
	unsafe {
		core::arch::asm!("mrs {}, CurrentEL", out(reg) current_el, options(nomem, nostack, preserves_flags));
	}
	let el = (current_el >> 2) & 0b11;

	crate::serial_println!("{} kernel is starting ...", crate::product::NAME);
	crate::serial_println!("arch: aarch64 | EL{el} | DTB {dtb:#x}");

	// Turn on the MMU with a boot identity map. Serial keeps working across the
	// enable because the UART's device page is mapped and the map is identity.
	unsafe {
		super::paging::init_boot_mmu();
	}
	crate::serial_println!("aarch64: MMU on (identity map, 4 kB granule)");

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
			// Map the PCIe ECAM window (QEMU virt puts it at 256 GB, above the boot
			// map) and point the PCI code at it.
			if bi.pcie_ecam != 0 {
				super::paging::identity_map_device_gb(bi.pcie_ecam);
				super::pci::set_ecam_base(bi.pcie_ecam);
			}
			(bi.ram_base + bi.ram_size, bi.cpu_count)
		}
		None => {
			crate::serial_println!("aarch64: no DTB found - using built-in defaults");
			(0, 1)
		}
	};

	// Seed the portable frame allocator from the device-tree memory map, bring up
	// the TTBR1 higher-half root, then bring up the kernel heap in the higher half.
	// After this, `alloc` collections (Box, Vec, ...) are usable.
	use super::paging;
	let (region_base, region_len) = paging::usable_region(ram_top);
	let regions = [bootproto::MemRegion { base: region_base, length: region_len, kind: bootproto::MEM_USABLE, _pad: 0 }];
	crate::mem::frame::init(&regions);
	crate::serial_println!("aarch64: frame allocator up - {} MB free DRAM", paging::frames_free() * 4 / 1024);
	unsafe {
		paging::init_higher_half();
	}
	crate::mem::heap::init();
	crate::mem::frame::upgrade_to_heap();
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
	// via the high VA (TTBR1 walk) and via the frame's own identity address.
	let frame = paging::alloc_frame().expect("aarch64: no frame for map test");
	let hva: u64 = 0xFFFF_8000_0000_0000;
	paging::map_page(hva, frame, paging::PRESENT | paging::WRITABLE | paging::NO_EXECUTE);
	let pattern: u64 = 0xCAFE_BABE_D00D_F00D;
	let (via_high, via_phys) = unsafe {
		core::ptr::write_volatile(hva as *mut u64, pattern);
		(core::ptr::read_volatile(hva as *const u64), core::ptr::read_volatile(frame as *const u64))
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

	// If a virtio-blk device is present, exercise it: read sector 0, then write a
	// pattern to sector 1 and read it back to confirm the write path.
	if let Some(blk) = virtio.iter().find(|v| v.virtio_type as u32 == abi::VIRTIO_TYPE_BLOCK) {
		if let Some(mut disk) = super::virtio_blk::BlkDevice::init(blk) {
			let mut buf = [0u8; 512];
			if disk.read(0, &mut buf) {
				crate::serial_println!("aarch64: virtio-blk sector 0 read - first16={:02x?}", &buf[..16]);
			} else {
				crate::serial_println!("aarch64: virtio-blk sector 0 read - FAILED");
			}

			let mut wbuf = [0u8; 512];
			let pattern = b"aarch64 virtio-blk write path OK";
			wbuf[..pattern.len()].copy_from_slice(pattern);
			let wrote = disk.write(1, &wbuf);
			let mut rbuf = [0u8; 512];
			let read_back = disk.read(1, &mut rbuf);
			let ok = wrote && read_back && rbuf == wbuf;
			crate::serial_println!("aarch64: virtio-blk sector 1 write+readback = {}", if ok { "ok" } else { "FAIL" });
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
	crate::sched::allocate(1);
	crate::sched::init();
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

	crate::serial_println!("aarch64 bring-up: serial + MMU + vectors + GIC/timer + paging + heap + pci + percpu + SMP + threads + EL0 + addrspace OK - halting");
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
// argument x0). `msg`, if non-empty, is placed at code+0x500 for the program to
// send. The code page is writable so a receive buffer / handle slots can live in it.
fn build_ipc_endpoint_process(prog: &[u32], msg: &[u8], endpoint: alloc::sync::Arc<dyn crate::object::KernelObject>) {
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
		if !msg.is_empty() {
			core::ptr::copy_nonoverlapping(msg.as_ptr(), (code + 0x500) as *mut u8, msg.len());
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
	build_ipc_endpoint_process(&sender, msg, ep0);
	build_ipc_endpoint_process(&receiver, &[], ep1);
	crate::sched::run_until_idle();
	crate::serial_println!("aarch64: xIPC - done");
}
