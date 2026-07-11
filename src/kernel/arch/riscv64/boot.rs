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

// The init + volume packages assembled by build.rs from the riscv64 userspace build.
// riscv64 boots directly (no bootloader hand-off), so the kernel embeds them and
// publishes its own BootInfo pointing at them. Empty if the userspace was not built.
const INIT_PKG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/init.pkg"));
const VOLUME_PKG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/volume.pkg"));

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
	let (ram_top, cpu_count, pcie_ecam, _plic_base) = match super::dtb::parse(dtb) {
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
	let _ = _plic_base;

	// Record the device-tree PCIe ECAM base (under 8 GiB, so the boot direct map already
	// reaches it). The AIA IMSIC uses its fixed QEMU virt S-file base (imsic.rs).
	super::pci::set_ecam_base(pcie_ecam);

	crate::mem::set_hhdm_offset(paging::KERNEL_VA_OFFSET);
	let (region_base, region_len) = paging::usable_region(ram_top);
	let regions = [bootproto::MemRegion { base: region_base, length: region_len, kind: bootproto::MEM_USABLE, _pad: 0 }];
	crate::mem::frame::init(&regions);
	crate::serial_println!("riscv64: frame allocator up - {} MB free DRAM", paging::frames_free() * 4 / 1024);
	crate::mem::heap::init();
	crate::mem::frame::upgrade_to_heap();
	// Retain the boot memory map so the `lsmem` inventory tool can render it (heap-backed,
	// so after heap::init).
	crate::mem::retain_memmap(&regions);
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

// Publish a kernel-constructed BootInfo pointing at the embedded init.pkg / volume.pkg
// (riscv64 boots directly, with no bootloader hand-off, so the kernel builds its own).
// Both the userspace boot chain and the test harness read the packages through it.
fn publish_embedded_boot_info() {
	fn module(name: &[u8], bytes: &[u8]) -> bootproto::Module {
		let mut nm = [0u8; 32];
		nm[..name.len()].copy_from_slice(name);
		bootproto::Module { addr: bytes.as_ptr() as u64, size: bytes.len() as u64, name: nm }
	}
	let modules: &'static mut [bootproto::Module; 2] = alloc::boxed::Box::leak(alloc::boxed::Box::new([module(b"init.pkg", INIT_PKG), module(b"volume.pkg", VOLUME_PKG)]));
	let bi: &'static bootproto::BootInfo = alloc::boxed::Box::leak(alloc::boxed::Box::new(bootproto::BootInfo { magic: bootproto::MAGIC, version: bootproto::VERSION, _pad0: 0, hhdm_offset: super::paging::KERNEL_VA_OFFSET, memmap: 0, memmap_len: 0, modules: modules.as_ptr() as u64, modules_len: modules.len() as u64, framebuffer: bootproto::Framebuffer { addr: 0, width: 0, height: 0, pitch: 0, bpp: 0, red_shift: 0, red_size: 0, green_shift: 0, green_size: 0, blue_shift: 0, blue_size: 0, _pad: [0; 2] }, fb_present: 0, _pad1: 0, rsdp: 0, smp_trampoline: 0 }));
	crate::publish_boot_info(bi);
}

// Spawn the real SystemManager from the embedded init package and drive the userspace
// boot chain as far as it runs, draining its reports, then hand off to the interactive
// shell over the serial console. The same portable mechanism the x86/aarch64 kernels
// use (pkg::Package + loader::spawn_elf_process + the PACKAGE/RAMDISK/MODE bootstrap).
#[cfg(not(test))]
fn run_system_manager() {
	if INIT_PKG.is_empty() || VOLUME_PKG.is_empty() {
		crate::serial_println!("riscv64: system - packages not embedded (build the riscv64 userspace first)");
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
