// riscv64 higher-half boot entry (M117).
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

.section .text.boot, "ax"
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

#[unsafe(no_mangle)]
extern "C" fn riscv64_main(hartid: u64, dtb: u64) -> ! {
	super::serial::init();
	crate::serial_println!("riscv64: hello from the kernel (higher half)");
	crate::serial_println!("riscv64: hart {hartid}, DTB @ {dtb:#x}");

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
	// Permit S-mode access to U-mapped pages (SSTATUS.SUM, bit 18) so the kernel can
	// seed user pages later; and enable the FPU (SSTATUS.FS = Initial, bit 13) so the
	// context switch's fsd/fld and any compiler-emitted FP do not trap.
	unsafe { core::arch::asm!("csrs sstatus, {}", in(reg) (1u64 << 18) | (1u64 << 13), options(nostack, preserves_flags)) };
	crate::serial_println!("riscv64: STVEC trap vector installed");

	// Trap round-trip: `ebreak` traps to S-mode (OpenSBI delegates it via medeleg);
	// the handler advances SEPC past it and returns via sret, so execution resumes.
	unsafe { core::arch::asm!("ebreak") };
	crate::serial_println!("riscv64: trap round-trip (ebreak) OK");

	// Increment 4: parse the device tree (the shared FDT parser), seed the portable
	// frame allocator, and bring up the kernel heap in the higher half.
	use super::paging;
	let (ram_top, cpu_count, pcie_ecam, plic_base) = match super::dtb::parse(dtb) {
		Some(bi) => {
			crate::serial_println!("riscv64: DTB parsed - RAM {:#x}..{:#x} ({} MB), {} CPU(s), ECAM {:#x}, PLIC {:#x}", bi.ram_base, bi.ram_base + bi.ram_size, bi.ram_size / (1024 * 1024), bi.cpu_count, bi.pcie_ecam, bi.plic_base);
			(bi.ram_base + bi.ram_size, bi.cpu_count, bi.pcie_ecam, bi.plic_base)
		}
		None => {
			crate::serial_println!("riscv64: no DTB found - using built-in defaults");
			(0, 1, 0, 0)
		}
	};
	let cpu_count = cpu_count.max(1);

	// Record the device-tree MMIO bases for the interrupt controller and PCIe config
	// space (both under 8 GiB, so the boot direct map already reaches them).
	super::plic::set_base(plic_base);
	super::pci::set_ecam_base(pcie_ecam);

	crate::mem::set_hhdm_offset(paging::KERNEL_VA_OFFSET);
	let (region_base, region_len) = paging::usable_region(ram_top);
	let regions = [bootproto::MemRegion { base: region_base, length: region_len, kind: bootproto::MEM_USABLE, _pad: 0 }];
	crate::mem::frame::init(&regions);
	crate::serial_println!("riscv64: frame allocator up - {} MB free DRAM", paging::frames_free() * 4 / 1024);
	crate::mem::heap::init();
	crate::mem::frame::upgrade_to_heap();
	{
		let mut v: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
		for i in 0..8 {
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
	paging::dealloc_frame(frame);

	// Increment 5: monotonic clock, per-CPU block, context switch, scheduler, timer.
	super::tsc::init();
	super::apic::set_boot_hart(hartid);
	crate::serial_println!("riscv64: clock - timebase {} MHz, uptime {} ms", super::tsc::hz() / 1_000_000, super::tsc::cycles_to_ns(super::tsc::now()) / 1_000_000);

	// Per-CPU block for the boot hart, reachable through `tp`.
	super::percpu::allocate(cpu_count as usize);
	super::percpu::init(0, hartid as u32);
	// Bring up the PLIC on the boot hart (mask all sources, open this hart's S-mode
	// threshold) so PLIC-routed device interrupts can be enabled per source.
	super::plic::init(hartid);
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

	// Cooperative context switch: two kernel threads ping-pong through switch_context,
	// then hand control back to the boot context.
	unsafe {
		A_SP = super::context::init_thread_stack(&mut *(&raw mut STACK_A), thread_a, 0xAA);
		B_SP = super::context::init_thread_stack(&mut *(&raw mut STACK_B), thread_b, 0xBB);
	}
	crate::serial_println!("riscv64: context switch - starting kernel threads");
	unsafe { super::context::switch_context(&raw mut MAIN_SP, A_SP) };
	crate::serial_println!("riscv64: context switch - returned to boot context");

	// The portable scheduler on top of the arch context/percpu contract.
	crate::sched::allocate(cpu_count as usize);
	crate::sched::init();

	// Cooperative: three yielding kernel threads, drained to completion.
	for id in 1..=3u64 {
		crate::sched::spawn(sched_task, id);
	}
	crate::serial_println!("riscv64: portable scheduler - draining 3 yielding threads");
	crate::sched::run_until_idle();
	crate::serial_println!("riscv64: portable scheduler - all threads exited");

	// Preemptive: arm the S-mode timer + enable interrupts, then three non-yielding
	// threads that only the timer IRQ can rotate.
	super::apic::init();
	super::enable_interrupts();
	for id in 1..=3u64 {
		crate::sched::spawn(preempt_task, id);
	}
	crate::serial_println!("riscv64: preemptive scheduler - 3 non-yielding threads");
	crate::sched::run_until_idle();
	crate::serial_println!("riscv64: preemptive scheduler - all threads exited (ticks={})", super::apic::ticks());

	// Increment 6: real userspace. Two portable Processes whose U-mode programs call
	// the REAL kernel syscall table (SYS_DEBUG_WRITE) and interleave via SYS_YIELD.
	// Each thread parks its U-mode resume state in its own slot, so the excursions
	// coexist; each has its own address space, so both use the same low user VAs.
	super::syscall::init();
	run_user_processes();

	// Increment 8: enumerate the PCIe ECAM bus (the shared arch::common::pci over the
	// riscv64 ECAM ConfigAccess) and resolve any virtio devices' modern MMIO layout.
	run_pci_scan();

	crate::serial_println!("riscv64: M117 increment 8 (ECAM PCI + PLIC + DTB pcie fix) - OK, halting");
	super::halt_loop()
}

// Enumerate the PCIe ECAM bus and report what is present, resolving virtio devices'
// modern MMIO layout through the shared PCI code. On QEMU virt the pci bus is empty
// unless devices are added (e.g. `-device virtio-blk-pci`), so a device count of zero
// just means none were attached.
fn run_pci_scan() {
	let devices = super::pci::scan();
	crate::serial_println!("riscv64: PCI - {} device(s) on the ECAM bus", devices.len());
	for d in &devices {
		crate::serial_println!("riscv64:   {:02x}:{:02x}.{} vendor={:04x} device={:04x} class={:02x}", d.bus, d.dev, d.func, d.vendor, d.device_id, d.class);
	}
	let virtio = super::pci::scan_virtio();
	for v in &virtio {
		crate::serial_println!("riscv64:   virtio {} @ BAR{} phys={:#x} len={:#x} | common+{:#x} notify+{:#x} isr+{:#x} device+{:#x}", super::pci::virtio_type_name(v.virtio_type), v.bar, v.bar_phys, v.region_len, v.common.offset, v.notify.offset, v.isr.offset, v.device.offset);
	}
}

// -------------------------------------------------------- increment-5 threads

static mut MAIN_SP: u64 = 0;
static mut A_SP: u64 = 0;
static mut B_SP: u64 = 0;
static mut STACK_A: [u8; 8192] = [0; 8192];
static mut STACK_B: [u8; 8192] = [0; 8192];

// Ping-pong partner A: print + switch to B three times, then return to the boot
// context (which resumes right after the initial switch_context above).
extern "C" fn thread_a(arg: u64) {
	for i in 0..3 {
		crate::serial_println!("riscv64: thread A step {i} (arg={arg:#x})");
		unsafe { super::context::switch_context(&raw mut A_SP, B_SP) };
	}
	unsafe { super::context::switch_context(&raw mut A_SP, MAIN_SP) };
}

// Ping-pong partner B: print + switch back to A forever (A drives the count).
extern "C" fn thread_b(arg: u64) {
	loop {
		crate::serial_println!("riscv64: thread B step (arg={arg:#x})");
		unsafe { super::context::switch_context(&raw mut B_SP, A_SP) };
	}
}

// A portable-scheduler kernel thread: print a few times, yielding the core between
// steps, then return (which retires it through sched::exit).
extern "C" fn sched_task(id: u64) {
	for i in 0..3 {
		crate::serial_println!("riscv64: [sched] thread {id} step {i}");
		crate::sched::yield_now();
	}
}

// A preemption test thread: busy-wait ~15 ms per step (no yield) so the 10 ms timer
// quantum forces a preemptive rotation; interleaved output proves the timer IRQ
// drives the scheduler.
extern "C" fn preempt_task(id: u64) {
	for i in 0..3 {
		let target = super::tsc::now() + super::tsc::hz() * 15 / 1000;
		while super::tsc::now() < target {
			core::hint::spin_loop();
		}
		crate::serial_println!("riscv64: [preempt] thread {id} step {i}");
	}
}

// ---------------------------------------------------------- increment-6 users

struct UserCtx {
	entry: u64,
	stack_top: u64,
	arg: u64,
}

// The kernel-side entry of a user process's thread: drop to U-mode at the mapped
// program. Returns when the program calls SYS_USER_EXIT (then the thread exits).
extern "C" fn user_trampoline(ctx_raw: u64) {
	let ctx = unsafe { alloc::boxed::Box::from_raw(ctx_raw as *mut UserCtx) };
	unsafe {
		super::usermode::enter(ctx.entry, ctx.stack_top, ctx.arg);
	}
}

// Build a portable user Process and enqueue its U-mode thread. The program (assembled
// below) prints `msg` via SYS_DEBUG_WRITE, yields, prints + yields again, then
// SYS_USER_EXIT - so two such processes interleave through the scheduler, which only
// works because each thread parks its U-mode resume state in its own slot. Each
// process has its own address space, so both can use the same low user VAs.
fn spawn_user_process(msg: &[u8]) {
	use super::paging;
	use crate::object::address_space::AddressSpace;
	use crate::object::process::Process;

	let addr_space = match AddressSpace::create() {
		Some(a) => a,
		None => {
			crate::serial_println!("riscv64: userspace - no address space");
			return;
		}
	};
	let code = paging::alloc_frame().expect("riscv64: no frame for user code");
	let stack = paging::alloc_frame().expect("riscv64: no frame for user stack");

	let code_va: u64 = 0x4000_0000; // 1 GiB - a low user VA (below the kernel half)
	let msg_va = code_va + 0x800;
	let stack_va: u64 = 0x4001_0000;
	let stack_top = stack_va + 0x1000;

	// a0 holds the message pointer (the entry argument); keep it in s1 across the
	// syscalls. Tiny RV64I encoders (a7 = number, a0..a1 = args).
	let addi = |rd: u32, rs1: u32, imm: i32| -> u32 { (((imm as u32) & 0xfff) << 20) | (rs1 << 15) | (rd << 7) | 0x13 };
	let li = |rd: u32, imm: i32| -> u32 { addi(rd, 0, imm) };
	let mv = |rd: u32, rs: u32| -> u32 { addi(rd, rs, 0) };
	const ECALL: u32 = 0x0000_0073;
	const S1: u32 = 9;
	const A0: u32 = 10;
	const A1: u32 = 11;
	const A7: u32 = 17;
	let len = msg.len() as i32;
	let prog: [u32; 16] = [
		mv(S1, A0),  // s1 = msg ptr
		mv(A0, S1),  // a0 = msg ptr
		li(A1, len), // a1 = len
		li(A7, abi::SYS_DEBUG_WRITE as i32),
		ECALL,
		li(A7, abi::SYS_YIELD as i32),
		ECALL,
		mv(A0, S1),
		li(A1, len),
		li(A7, abi::SYS_DEBUG_WRITE as i32),
		ECALL,
		li(A7, abi::SYS_YIELD as i32),
		ECALL,
		li(A7, abi::SYS_USER_EXIT as i32),
		ECALL,
		0x0000_006f, // j . (guard against running off the end)
	];
	unsafe {
		let cp = paging::phys_to_virt(code) as *mut u32;
		for (i, w) in prog.iter().enumerate() {
			core::ptr::write_volatile(cp.add(i), *w);
		}
		core::ptr::copy_nonoverlapping(msg.as_ptr(), (paging::phys_to_virt(code) + 0x800) as *mut u8, msg.len());
		core::arch::asm!("fence.i", options(nostack, preserves_flags));
	}

	addr_space.map(code_va, code, paging::PRESENT | paging::USER);
	addr_space.map(stack_va, stack, paging::PRESENT | paging::WRITABLE | paging::USER | paging::NO_EXECUTE);

	let process = Process::new(addr_space, crate::sched::root_domain());
	process.adopt_frames(alloc::vec![code, stack]);
	let ctx = alloc::boxed::Box::new(UserCtx { entry: code_va, stack_top, arg: msg_va });
	crate::sched::thread_create(process, user_trampoline, alloc::boxed::Box::into_raw(ctx) as u64);
}

// Run two concurrent user processes: they interleave via SYS_YIELD, each making real
// SYS_DEBUG_WRITE syscalls, proving per-thread U-mode excursions coexist.
fn run_user_processes() {
	spawn_user_process(b"userspace A: hello via SYS_DEBUG_WRITE\n");
	spawn_user_process(b"userspace B: hello via SYS_DEBUG_WRITE\n");
	crate::serial_println!("riscv64: userspace - running 2 U-mode processes");
	crate::sched::run_until_idle();
	crate::serial_println!("riscv64: userspace - both U-mode processes exited");
}
