// aarch64 higher-half boot entry.
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

	// INVALIDATE THE DATA CACHE BEFORE ANYTHING IS WRITTEN, because this stub is about to turn the
	// caches back ON and it must not inherit a line from whoever ran before it.
	//
	// The loader hands over with SCTLR.C clear, so every store below goes straight to memory - but
	// the cache may still hold lines for those addresses from when the firmware and the loader ran
	// with caching on. Enabling C afterwards would let a read hit one of them and see a page table
	// entry that was never written.
	//
	// INVALIDATE, NOT CLEAN-AND-INVALIDATE, and the order is the reason. A stale DIRTY line for an
	// address this stub is about to write would, on a clean, be written back OVER the fresh value -
	// so the maintenance has to happen BEFORE the writes, and at that point there is nothing of
	// this kernel's worth keeping. What the loader hands over it publishes itself, to the point of
	// coherency, while its own cache is still enabled (`publish_handoff`).
	//
	// By set/way: architecturally that is only meaningful with nothing else running, which is
	// exactly this instruction of this boot. The loop is the canonical one - for each level CLIDR_EL1
	// names as having a data cache, read its geometry from CCSIDR_EL1 and walk every set of every
	// way. QEMU's caches are coherent and model none of this, so it is written from the architecture
	// manual and has never been observed mattering.
	mrs     x0, clidr_el1
	and     w3, w0, #0x07000000     // LoC, bits 26:24
	lsr     w3, w3, #23             // ... as 2 * level
	cbz     w3, 5f
	mov     w10, #0                 // w10 = 2 * current level
1:
	add     w2, w10, w10, lsr #1    // w2 = 3 * level: this level's CLIDR field offset
	lsr     w1, w0, w2
	and     w1, w1, #0x7            // cache type at this level
	cmp     w1, #2
	b.lt    4f                      // no data or unified cache here
	msr     csselr_el1, x10
	isb
	mrs     x1, ccsidr_el1
	and     w2, w1, #7              // log2(line bytes) - 4
	add     w2, w2, #4
	mov     w4, #0x3ff
	and     w4, w4, w1, lsr #3      // highest way
	clz     w5, w4                  // where the way field starts
	mov     w7, #0x7fff
	and     w7, w7, w1, lsr #13     // highest set
2:
	mov     w9, w4
3:
	lsl     w6, w9, w5
	orr     w11, w10, w6            // level | way
	lsl     w6, w7, w2
	orr     w11, w11, w6            // ... | set
	dc      isw, x11
	subs    w9, w9, #1
	b.ge    3b
	subs    w7, w7, #1
	b.ge    2b
4:
	add     w10, w10, #2
	cmp     w3, w10
	b.gt    1b
5:
	mov     x0, #0
	msr     csselr_el1, x0
	dsb     sy
	ic      iallu
	dsb     sy
	isb

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
	// Enable the MMU (SCTLR_EL1.M), the data cache (C) and the instruction cache (I).
	//
	// C AND I WERE NOT SET HERE, AND NOTHING ELSE SET THEM. The loader clears all three on purpose -
	// this stub builds translation from scratch - and this line put back only M, so both caches
	// stayed off for the life of the machine, on the primary core and, through the same sequence in
	// `psci.rs`, on every secondary. Page-table memory attributes do not re-enable the architectural
	// caches; only these bits do. On QEMU TCG it costs nothing measurable, which is why it survived;
	// on a real part it is orders of magnitude of memory traffic, and services that look hung.
	mrs     x0, sctlr_el1
	orr     x0, x0, #1     // M: translation on
	orr     x0, x0, #0x4   // C: data and unified caches
	orr     x0, x0, #0x1000 // I: instruction caches
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

// The boot modules, handed over at RUN time rather than compiled in. aarch64 virt has no
// bootloader module hand-off, so on a direct boot the runner loads the archive at a fixed address
// and this reads it back from there; under UEFI the loader passes it as a named module.
//
// NOT AN INITRD, which is what this comment used to say. QEMU enters an ELF kernel with x0 = 0 and
// places no device tree for it, so the runner dumps a tree from a separate invocation - and that
// invocation cannot be given `-kernel`/`-initrd` (it segfaults qemu-system-aarch64 10.0.11), so no
// tree this kernel can ever read carries `/chosen/linux,initrd-start`. riscv64 is the arch where
// that mechanism works, and it uses it.
//
// The kernel used to `include_bytes!` the packages out of its own OUT_DIR, which made the
// kernel binary contain the userspace and made building the kernel depend on having built the
// userspace first. It no longer does either: the kernel is the same binary whatever userspace
// is handed to it, exactly as on x86_64 where its own loader passes the modules.
static BOOT_MODULES: crate::sync::SpinLock<Option<&'static [u8]>> = crate::sync::SpinLock::new(None);

// Where `boot/qemu-run.sh aarch64` loads the archive on a direct boot, and the one number the two
// have to agree on (`MODULES_ADDR` there). It sits 16 MiB above the dumped DTB at 0x4A00_0000,
// which is itself 1 MiB, and both are inside the 512 MiB the `virt` machine gets by default.
//
// The LENGTH is deliberately not a second constant: the archive names its own extent, so a longer
// or shorter package needs no change on either side. See `bootmem::archive_len`.
const RUNNER_MODULES_ADDR: u64 = 0x4B00_0000;

// The archive the initrd carries, or an empty slice when none was supplied - a kernel booted
// with no userspace still comes up, it just has nothing to start.
// The bootstrap archive the UEFI loader passed as a module, translated into the kernel's map.
//
// The loader hands over physical addresses because it runs before this kernel builds its own
// higher-half map; reading them raw is how the first attempt failed, with the kernel reporting a
// malformed init package it had never actually read.
// A named module from the loader's hand-off, translated into the kernel's map.
fn loader_module_at(arg: u64, want: &[u8]) -> Option<&'static [u8]> {
	if arg == 0 {
		return None;
	}
	// The same test `decode_boot_arg` makes: a BootInfo or a raw DTB pointer. Read here rather
	// than from the published BootInfo because this runs before the kernel publishes one.
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

// The boot argument, kept so a module can be looked up after the early boot has moved on.
static BOOT_ARG: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

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

// The boot stack's extent, placed by the linker script. Read only to bound it - see the call in
// `aarch64_main`.
unsafe extern "C" {
	static __boot_stack_bottom: u8;
	static __boot_stack_top: u8;
}

#[unsafe(no_mangle)]
extern "C" fn aarch64_main(arg: u64) -> ! {
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

	// The entry argument is either a raw DTB pointer (QEMU `-kernel`) or a
	// `bootproto::BootInfo` pointer (the UEFI loader). Tell them apart by the BootInfo
	// magic at the target; the UEFI path also carries a GOP framebuffer, so the kernel
	// draws its earliest boot log to the display instead of programming ramfb itself.
	let (dtb, uefi_fb) = decode_boot_arg(arg);

	// The loader's hand-off, taken BEFORE anything else looks at the boot argument.
	//
	// It has to be outside the device-tree branch: under UEFI the firmware exposes no DTB at all,
	// so everything conditioned on one is dead there - which is exactly how the first attempt
	// failed, with the kernel reporting a malformed init package that its own code had skipped
	// over. The addresses are physical, because the loader runs before this kernel has a map.
	if let Some(archive) = loader_archive(arg) {
		*BOOT_MODULES.lock() = Some(archive);
		crate::serial_println!("aarch64: boot packages handed over by the loader ({} bytes)", archive.len());
	}

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
	let boot_info = unsafe { super::dtb::parse(dtb) };
	let ram_banks = boot_info.map(|bi| (bi.ram_regions, bi.ram_region_count));
	let (ram_top, cpu_count, fwcfg_base) = match boot_info {
		Some(bi) => {
			crate::serial_println!("aarch64: DTB parsed - RAM {:#x}..{:#x} ({} MB), {} CPU(s)", bi.ram_base, bi.ram_base + bi.ram_size, bi.ram_size / (1024 * 1024), bi.cpu_count);
			// The boot stub already maps the 256 GB device region (BOOT_L1[256]), so
			// the PCIe ECAM is reachable through phys_to_virt; just point PCI at it.
			if bi.pcie_ecam != 0 {
				super::pci::set_ecam_base(bi.pcie_ecam);
			}
			super::set_fwcfg_base(bi.fwcfg_base);
			(bi.ram_base + bi.ram_size, bi.cpu_count, bi.fwcfg_base)
		}
		None => {
			crate::serial_println!("aarch64: no DTB found - using built-in defaults");
			(0, 1, 0)
		}
	};

	use super::paging;

	// THE BOOT ARCHIVE, when this is a direct boot rather than a UEFI one.
	//
	// The UEFI branch above already took it out of the loader's module list. This is the other
	// path, and it used to have nothing: `-kernel` on `virt` has no module hand-off, the comment in
	// the runner claimed an initrd carried the archive, and no invocation had ever passed one - so
	// every direct boot reached `run_system_manager` with an empty archive and said it was starting
	// no userspace. The runner loads the init package at MODULES_ADDR now, and this reads it back.
	//
	// The address is the agreement, and it is one number rather than two: the archive states its own
	// extent in its PKGARCH1 header. `boot_archive_range` prefers a device-tree initrd range when a
	// bootloader wrote one, which this machine cannot do and riscv64 can.
	let archive = match *BOOT_MODULES.lock() {
		Some(_) => None,
		None => unsafe { crate::arch::common::bootmem::boot_archive_range(boot_info.map_or(0, |bi| bi.modules_start), boot_info.map_or(0, |bi| bi.modules_end), RUNNER_MODULES_ADDR, paging::phys_to_virt) },
	};
	if let Some((base, len)) = archive {
		*BOOT_MODULES.lock() = Some(unsafe { core::slice::from_raw_parts(paging::phys_to_virt(base) as *const u8, len as usize) });
		crate::serial_println!("aarch64: boot packages at {base:#x}..{:#x} ({len} bytes) - direct boot", base + len);
	}

	// Seed the portable frame allocator from the device-tree memory map, then bring
	// up the kernel heap in the higher half (the TTBR1 root is already live from the
	// boot stub). After this, `alloc` collections (Box, Vec, ...) are usable.
	// Publish the direct-map offset so the portable subsystems (heap, ELF loader,
	// ...) reach physical frames the same way this backend does (phys | KOFF).
	crate::mem::set_hhdm_offset(paging::KERNEL_VA_OFFSET);
	// AND WHAT THE STUB ACTUALLY MAPPED. The boot assembly maps 1 GB blocks over 0..4 GiB before any memory
	// map is read, so the direct map's extent is that number and not the top of whatever the device
	// tree reports. Without this a machine with more RAM than the stub maps has `within_direct_map`
	// answering true for addresses `phys_to_virt` does not translate.
	crate::mem::set_direct_map_extent(4 * 1024 * 1024 * 1024);
	// The pool runs to the top of RAM, MINUS what the loader left in it.
	//
	// It used to be one region and nothing else, on the reasoning that a clamp without the thing
	// it protects only loses frames. The thing it protects is still there: this machine has no
	// bootloader module hand-off and no memory map, so the loader reads the boot packages off the
	// volume into RAM above the kernel and passes their addresses - and those bytes are read for
	// the whole life of the boot, because they are where every program's ELF image comes from.
	//
	// Declaring them free is only harmless while nothing allocates that far up. See
	// `arch::common::bootmem` for how long that held and what ended it.
	let mut holes = [crate::arch::common::bootmem::Hole { start: 0, end: 0 }; crate::arch::common::bootmem::MAX_HOLES];
	let mut hole_count = unsafe { crate::arch::common::bootmem::loader_reservations(BOOT_ARG.load(core::sync::atomic::Ordering::SeqCst), |phys| paging::phys_to_virt(phys), &mut holes) };
	// AND WHAT THE DEVICE TREE RESERVES, including the blob itself - none of which anything carved.
	//
	// This kernel keeps reading the tree after the allocator is up, and the specification requires a
	// client not to overwrite it or use the reservation block's regions. See
	// `bootmem::devicetree_reservations`.
	if let Some(tree) = (unsafe { super::dtb::located(dtb) }) {
		hole_count = unsafe { crate::arch::common::bootmem::devicetree_reservations(&tree, &mut holes, hole_count) };
	}
	// AND THE DIRECT BOOT'S ARCHIVE. Nothing else carves it: `loader_reservations` reads a hand-off
	// this boot does not have, and the device tree reserves its own blob and its reservation block
	// and knows nothing about a range the runner loaded with `-device loader`. Those are the bytes
	// every program's ELF image is read from for the whole life of the boot, so a pool spanning
	// them hands one out - the `BadImage` this milestone has already paid for once.
	if let Some((base, len)) = archive {
		if hole_count < holes.len() {
			holes[hole_count] = crate::arch::common::bootmem::Hole { start: base, end: base + len };
			hole_count += 1;
		} else {
			crate::serial_println!("aarch64: NO ROOM to reserve the boot archive {base:#x}..{:#x} - the allocator may hand it out", base + len);
		}
	}
	// `banks + holes`, because each reservation splits at most one bank into one extra region.
	let mut regions = [bootproto::MemRegion { base: 0, length: 0, kind: bootproto::MEM_USABLE, _pad: 0 }; fdt::MAX_RAM_REGIONS + crate::arch::common::bootmem::MAX_HOLES];
	// EVERY BANK, not the contiguous run from the first one. `usable_region(0)` answers the floor -
	// the first page above the kernel image - and the fallback below is the machine with no device
	// tree at all, where one range is all there is to know.
	// THE FIRMWARE'S MAP FIRST, when the loader handed one over. Under UEFI it IS the system memory
	// map and the device tree's `/memory` is to be ignored - the EFI map carries runtime services
	// code and data, ACPI NVS and reclaimable regions, unusable memory, firmware reservations,
	// loader allocations and MMIO apertures, none of which a `/memory` node expresses. Without a
	// loader - a QEMU `-kernel` boot - there is none, and the device tree is the best source there
	// is.
	let mut handed_banks = [(0u64, 0u64); fdt::MAX_RAM_REGIONS];
	let mut handed_count = 0usize;
	if let Some((map, len)) = unsafe { crate::arch::common::bootmem::handed_memmap(BOOT_ARG.load(core::sync::atomic::Ordering::SeqCst), |phys| paging::phys_to_virt(phys)) } {
		for region in unsafe { core::slice::from_raw_parts(map, len) } {
			if region.kind == bootproto::MEM_USABLE && handed_count < handed_banks.len() {
				handed_banks[handed_count] = (region.base, region.length);
				handed_count += 1;
			}
		}
		crate::serial_println!("firmware memory map: {len} region(s) from the loader, {handed_count} usable - the device tree's /memory is not used");
	}
	let region_count = match (handed_count > 0, ram_banks) {
		(true, _) => crate::arch::common::bootmem::carve_banks(&handed_banks[..handed_count], paging::usable_region(0).0, 4 * 1024 * 1024 * 1024, &holes[..hole_count], &mut regions),
		(false, Some((banks, count))) if count > 0 => crate::arch::common::bootmem::carve_banks(&banks[..count], paging::usable_region(0).0, 4 * 1024 * 1024 * 1024, &holes[..hole_count], &mut regions),
		_ => {
			let (region_base, region_len) = paging::usable_region(ram_top);
			crate::arch::common::bootmem::carve(region_base, region_len, &mut holes[..hole_count], &mut regions)
		}
	};
	for hole in &holes[..hole_count] {
		crate::serial_println!("aarch64: reserved {:#x}..{:#x} ({} KiB) - handed over by the loader", hole.start, hole.end, (hole.end - hole.start) / 1024);
	}
	crate::mem::frame::init(&regions[..region_count]);
	crate::serial_println!("aarch64: frame allocator up - {} MB free DRAM", paging::frames_free() * 4 / 1024);
	crate::mem::heap::init();
	crate::mem::frame::upgrade_to_heap();
	// Retain the boot memory map now the heap is up, so SYS_MEMMAP_GET (lsmem) can
	// report the physical layout - the x86 loader path retains it inside mem::init.
	crate::mem::retain_memmap(&regions);
	// Bring up the early framebuffer console so the kernel draws the boot log to the
	// display pixel-by-pixel like x86 - QEMU virt has no VGA, so without one the boot is
	// serial-only. The UEFI loader hands a GOP framebuffer in the BootInfo (drawn to
	// directly); the `-kernel` path has no loader, so the kernel programs QEMU ramfb
	// over fw-cfg itself. Runs after the heap + frame pool are up (the console grid, and
	// ramfb's framebuffer, are heap/frame allocations). A no-op if neither is present.
	match uefi_fb {
		Some(fb) => install_console(fb),
		None => init_ramfb_console(fwcfg_base),
	}
	{
		let mut v: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
		for i in 0..8 {
			// ALLOC-OK: boot bring-up self-test, before userspace exists.
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

	// Clocks, and a sample of the seeded generator beside them.
	//
	// `insecure` by name: this port has no hardware source - `FEAT_RNG` is not detected here - so
	// the sample is from the formula, and a boot line printing it should say which it is rather than
	// borrowing the word "random" from the syscall that refuses on this machine.
	super::tsc::init();
	let mut rnd = [0u8; 6];
	super::random::insecure(&mut rnd);
	crate::serial_println!("aarch64: clocks - timer {} MHz, uptime {} ms, RTC unix {} | seeded {:02x?}", super::tsc::hz() / 1_000_000, super::tsc::cycles_to_ns(super::tsc::now()) / 1_000_000, super::rtc::read_unix(), rnd);

	// Per-CPU block for the boot core, reachable through TPIDR_EL1.
	let mpidr: u64;
	unsafe {
		core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
	}
	super::percpu::allocate(cpu_count as usize);
	super::percpu::init(0, mpidr as u32);
	// THE BOOT STACK HAS NO GUARD PAGE EITHER, and the linker script already records what that
	// costs: it was 64 KiB until the in-kernel LiberFS format walked a B-tree and a transaction log
	// off the bottom of it and into `.bss`, and the symptom was "memory corruption rather than a
	// fault - a `serial_println!` printing megabytes of memory, and data aborts at addresses that
	// made no sense". It is 256 KiB now, which makes it unlikely rather than impossible.
	//
	// Bounding it here means the exception entry REPORTS the next one instead of the kernel
	// discovering it three subsystems later. The stack below `__boot_stack_bottom` is `.bss`, so
	// there is nothing to fault on and nothing else would ever say so.
	unsafe {
		let bottom = (&raw const __boot_stack_bottom) as u64;
		let top = (&raw const __boot_stack_top) as u64;
		super::percpu::record_idle_stack(bottom, (top - bottom) as usize);
	}
	let cpu = super::percpu::this_cpu();
	crate::serial_println!("aarch64: per-CPU up (TPIDR_EL1) cpu_id={} mpidr={:#x} of {} CPU(s)", cpu.cpu_id(), cpu.lapic_id() & 0xff_ffff, cpu_count);

	// Wake the secondary cores via PSCI CPU_ON (each brings up its own per-CPU
	// block + local GIC/timer, then idles).
	super::psci::bring_up_secondaries(cpu_count, arg);

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
// UEFI loader handed a `bootproto::BootInfo` here rather than QEMU `-kernel`'s raw DTB
// pointer. Both are physical pointers reachable through the boot stub's direct map; a
// BootInfo is recognised by its magic, and on this arch carries the framebuffer's
// PHYSICAL base (the loader builds no page tables).
fn decode_boot_arg(arg: u64) -> (u64, Option<BootFb>) {
	if arg == 0 {
		return (0, None);
	}
	let magic = unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(arg) as *const u64) };
	if magic != bootproto::MAGIC {
		return (arg, None); // a raw DTB pointer (the QEMU `-kernel` entry state)
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
	// The archive holds the packages as named entries, so the module list is built from what
	// was actually handed over rather than from a fixed pair the kernel was compiled with.
	// Two shapes, because there are two ways the packages arrive.
	//
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
	// AND THE LIVE VOLUME, when the loader handed one over. This kept exactly two modules, so a
	// live medium's `system-volume.img` reached the kernel and was then dropped by the kernel's own
	// republication - `main` looks the name up and found nothing, on the one architecture where
	// that lookup is all there is.
	let live = loader_module(crate::product::SYSTEM_VOLUME.as_bytes()).unwrap_or(&[]);
	// ALLOC-OK: boot, building the BootInfo this kernel was not given; no userspace exists yet
	let modules: &'static mut [bootproto::Module; 3] = alloc::boxed::Box::leak(alloc::boxed::Box::new([module(b"init.pkg", init), module(b"volume.pkg", volume), module(crate::product::SYSTEM_VOLUME.as_bytes(), live)]));
	// Hand the early framebuffer (if any) to a userspace consumer of the boot info.
	let (framebuffer, fb_present) = match *BOOT_FB.lock() {
		Some(f) => (bootproto::Framebuffer { addr: super::paging::phys_to_virt(f.phys), width: f.width, height: f.height, pitch: f.stride, bpp: 32, red_shift: f.red_shift, red_size: f.red_size, green_shift: f.green_shift, green_size: f.green_size, blue_shift: f.blue_shift, blue_size: f.blue_size, _pad: [0; 2] }, 1u32),
		None => (bootproto::Framebuffer { addr: 0, width: 0, height: 0, pitch: 0, bpp: 0, red_shift: 0, red_size: 0, green_shift: 0, green_size: 0, blue_shift: 0, blue_size: 0, _pad: [0; 2] }, 0u32),
	};
	// ALLOC-OK: boot, as above
	let bi: &'static bootproto::BootInfo = alloc::boxed::Box::leak(alloc::boxed::Box::new(bootproto::BootInfo { magic: bootproto::MAGIC, version: bootproto::VERSION, _pad0: 0, hhdm_offset: super::paging::KERNEL_VA_OFFSET, memmap: 0, memmap_len: 0, modules: modules.as_ptr() as u64, modules_len: modules.len() as u64, framebuffer, fb_present, psci_conduit: bootproto::PSCI_HVC, rsdp: 0, smp_trampoline: 0, dtb: 0 }));
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
	if boot_archive().is_empty() {
		crate::serial_println!("aarch64: no boot packages were handed over - userspace is not started");
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
