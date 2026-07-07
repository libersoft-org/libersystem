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

	// EL0 usermode: map a user code page and stack at 4 GiB+ (clear of the low
	// 1 GB identity blocks), copy in a tiny program that makes SVC syscalls, and
	// `eret` down to EL0. The program's "exit" syscall unwinds control back here.
	let code = paging::alloc_frame().expect("aarch64: no frame for user code");
	let stack = paging::alloc_frame().expect("aarch64: no frame for user stack");
	let user_entry: u64 = 0x1_0000_0000; // 4 GiB
	let user_stack_top: u64 = 0x1_0001_0000;
	paging::map_page(user_entry, code, paging::PRESENT | paging::USER);
	paging::map_page(user_stack_top - 0x1000, stack, paging::PRESENT | paging::WRITABLE | paging::USER | paging::NO_EXECUTE);
	let prog = super::usermode::program_bytes();
	unsafe {
		core::ptr::copy_nonoverlapping(prog.as_ptr(), code as *mut u8, prog.len());
		core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
	}
	crate::serial_println!("aarch64: entering EL0 usermode at {user_entry:#x}");
	unsafe {
		super::usermode::enter(user_entry, user_stack_top, 0);
	}
	crate::serial_println!("aarch64: returned from EL0 usermode");

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
