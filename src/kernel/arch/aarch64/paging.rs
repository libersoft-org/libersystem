// aarch64 paging - VMSAv8-64 translation (M116).
//
// The boot MMU: an identity map (VA == PA) of the low physical space via TTBR0 -
// the device MMIO region (0..1 GB, covers the PL011 UART, the GIC, the low ECAM)
// as Device memory, and up to 3 GB of DRAM (1..4 GB) as Normal cacheable RWX -
// built from 1 GB block descriptors in a static boot table pool, then the MMU is
// turned on (MAIR / TCR / TTBR0 / SCTLR.M). Because the map is identity, the PC,
// the stack, and the UART keep working across the enable.
//
// `translate` walks those tables. On top of the boot map this module now also
// brings up a bump frame allocator (free DRAM above the kernel image), a real
// 4 kB `map_page` that allocates the intermediate L1/L2/L3 tables, and a TTBR1
// higher-half root - enough to start routing kernel virtual addresses through
// the portable memory subsystem. `unmap` / `new_address_space` / W^X fill in as
// the port matures; those stay `todo!()` for now.

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

// Portable page-table permission bits (the flag set the portable callers OR
// together). The real per-PTE VMSAv8 encoding is applied by `map_page` when it
// lands; these keep the contract's constant names meaningful.
pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const NO_CACHE: u64 = 1 << 4;
pub const NO_EXECUTE: u64 = 1 << 63;

// A 4 kB translation table: 512 64-bit descriptors, naturally aligned.
#[repr(C, align(4096))]
struct Table([u64; 512]);

// The boot table pool (in .bss, zeroed by _start): one L0 and one L1 table are
// enough for the identity map (L0[0] -> L1; L1 holds 1 GB block descriptors).
static mut BOOT_L0: Table = Table([0; 512]);
static mut BOOT_L1: Table = Table([0; 512]);

// Descriptor bits (VMSAv8-64, stage 1).
const VALID: u64 = 1 << 0; // entry is valid
const TABLE: u64 = 1 << 1; // at L0/L1/L2: a table descriptor (VALID|TABLE = 0b11); a block clears this bit
const AF: u64 = 1 << 10; // access flag (a 0 here faults on first access)
const SH_INNER: u64 = 3 << 8; // inner shareable (for Normal memory)
const ATTR_DEVICE: u64 = 0 << 2; // MAIR index 0 = Device-nGnRnE
const ATTR_NORMAL: u64 = 1 << 2; // MAIR index 1 = Normal write-back
const PXN: u64 = 1 << 53; // privileged execute-never
const UXN: u64 = 1 << 54; // unprivileged execute-never

// A 1 GB block of Device memory (execute-never), and one of Normal RWX memory.
const BLOCK_DEVICE: u64 = VALID | ATTR_DEVICE | AF | PXN | UXN;
const BLOCK_NORMAL: u64 = VALID | ATTR_NORMAL | AF | SH_INNER;

// MAIR_EL1: index 0 = Device-nGnRnE (0x00), index 1 = Normal WB non-transient (0xFF).
const MAIR: u64 = 0x00 | (0xFF << 8);

// The physical-address mask for a table/page pointer (bits [47:12]).
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

// Build the boot identity map and turn on the MMU. Called once, early, with the
// MMU off (so every access is still physical). After it returns the MMU is on
// and the low physical space is identity-mapped.
pub unsafe fn init_boot_mmu() {
	let l0 = &raw mut BOOT_L0;
	let l1 = &raw mut BOOT_L1;

	unsafe {
		// L0[0] -> L1 (covers VA 0 .. 512 GB).
		(*l0).0[0] = (l1 as u64) | VALID | TABLE;
		// L1[0]: 0..1 GB device MMIO (UART, GIC, low ECAM).
		(*l1).0[0] = 0x0000_0000 | BLOCK_DEVICE;
		// L1[1..4]: 1..4 GB DRAM as Normal RWX (QEMU virt places RAM from 1 GB up;
		// mapping unused entries is harmless - only touched RAM is ever accessed).
		(*l1).0[1] = 0x4000_0000 | BLOCK_NORMAL;
		(*l1).0[2] = 0x8000_0000 | BLOCK_NORMAL;
		(*l1).0[3] = 0xC000_0000 | BLOCK_NORMAL;

		// Physical address size the CPU supports (ID_AA64MMFR0_EL1.PARange), for TCR.IPS.
		let mmfr0: u64;
		asm!("mrs {}, id_aa64mmfr0_el1", out(reg) mmfr0, options(nomem, nostack, preserves_flags));
		let ips = mmfr0 & 0x7; // 0:32b 1:36b 2:40b 3:42b 4:44b 5:48b

		// TCR_EL1: 4 kB granule, 48-bit VA on TTBR0 (T0SZ=16), Normal WB inner-shareable
		// walks; TTBR1 walks disabled for now (EPD1=1).
		let tcr: u64 = 16          // T0SZ = 16 (48-bit VA)
			| (1 << 8)             // IRGN0 = WB WA
			| (1 << 10)            // ORGN0 = WB WA
			| (3 << 12)            // SH0   = inner shareable
			| (0 << 14)            // TG0   = 4 kB granule
			| (1 << 23)            // EPD1  = disable TTBR1 table walks
			| (ips << 32); // IPS

		asm!(
			"msr mair_el1, {mair}",
			"msr tcr_el1, {tcr}",
			"msr ttbr0_el1, {ttbr0}",
			"isb",
			// Flush any stale TLB state before turning translation on.
			"tlbi vmalle1",
			"dsb ish",
			"isb",
			mair = in(reg) MAIR,
			tcr = in(reg) tcr,
			ttbr0 = in(reg) l0 as u64,
			options(nostack, preserves_flags),
		);

		// Enable the MMU (SCTLR_EL1.M). Caches stay as they are (off) for this
		// bring-up: Normal memory is then accessed non-cacheably, which is correct
		// and avoids any early cache-coherency concern.
		let mut sctlr: u64;
		asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack, preserves_flags));
		sctlr |= 1 << 0; // M = 1
		asm!("msr sctlr_el1, {}", "isb", in(reg) sctlr, options(nostack, preserves_flags));
	}
}

// Translate a TTBR0 virtual address to its physical address by walking the boot
// tables (4 kB granule, 48-bit, levels L0..L3, honoring block descriptors).
pub fn translate(va: u64) -> Option<u64> {
	let ttbr0: u64;
	unsafe {
		asm!("mrs {}, ttbr0_el1", out(reg) ttbr0, options(nomem, nostack, preserves_flags));
	}
	let mut table = (ttbr0 & ADDR_MASK) as *const u64;
	for level in 0..4u64 {
		let shift = 39 - level * 9; // L0=39, L1=30, L2=21, L3=12
		let idx = ((va >> shift) & 0x1ff) as usize;
		let desc = unsafe { core::ptr::read_volatile(table.add(idx)) };
		if desc & VALID == 0 {
			return None;
		}
		// A block (bit 1 clear) at L1/L2, or a page at L3, is a leaf; a table
		// descriptor (bit 1 set) at L0..L2 points at the next level.
		if desc & TABLE == 0 || level == 3 {
			let region = 1u64 << shift; // the leaf's coverage
			let base = desc & ADDR_MASK & !(region - 1);
			return Some(base | (va & (region - 1)));
		}
		table = (desc & ADDR_MASK) as *const u64;
	}
	None
}

// ---- frame allocator (bump) -------------------------------------------------
//
// Free DRAM starts just above the loaded kernel image (`__kernel_end`, page
// aligned by the linker) and runs to the top of RAM. Frames are handed out one
// 4 kB page at a time and zeroed in place - the identity map means the physical
// address is directly usable as a pointer while caches are off.

unsafe extern "C" {
	static __kernel_end: u8;
}

// Top of DRAM on QEMU virt with `-m 512M` (base 0x4000_0000 + 512 MiB).
const DRAM_BASE: u64 = 0x4000_0000;
const DRAM_TOP: u64 = DRAM_BASE + 512 * 1024 * 1024;

// Next free physical frame (0 until `init_frame_allocator` runs).
static FRAME_NEXT: AtomicU64 = AtomicU64::new(0);

// Point the bump allocator at the free DRAM above the kernel image.
pub fn init_frame_allocator() {
	let start = ((&raw const __kernel_end as u64) + 0xFFF) & !0xFFF;
	FRAME_NEXT.store(start, Ordering::Relaxed);
}

// Allocate one zeroed 4 kB physical frame, or `None` when DRAM is exhausted.
pub fn alloc_frame() -> Option<u64> {
	let pa = FRAME_NEXT.fetch_add(4096, Ordering::Relaxed);
	if pa == 0 || pa + 4096 > DRAM_TOP {
		return None;
	}
	unsafe {
		core::ptr::write_bytes(pa as *mut u8, 0, 4096);
	}
	Some(pa)
}

// How many 4 kB frames the allocator still has (for bring-up reporting).
pub fn frames_free() -> u64 {
	let next = FRAME_NEXT.load(Ordering::Relaxed);
	if next == 0 || next >= DRAM_TOP { 0 } else { (DRAM_TOP - next) / 4096 }
}

// ---- TTBR1 higher-half root -------------------------------------------------

// Physical address of the TTBR1 L0 table (0 until `init_higher_half` runs).
static TTBR1_ROOT: AtomicU64 = AtomicU64::new(0);

// Allocate the TTBR1 top-level table and enable TTBR1 walks (4 kB granule,
// 48-bit high half, Normal write-back inner-shareable) so higher-half kernel
// virtual addresses (top bit set) translate through their own tree.
pub unsafe fn init_higher_half() {
	let root = alloc_frame().expect("aarch64: no frame for TTBR1 root");
	TTBR1_ROOT.store(root, Ordering::Relaxed);

	unsafe {
		let mut tcr: u64;
		asm!("mrs {}, tcr_el1", out(reg) tcr, options(nomem, nostack, preserves_flags));
		tcr &= !(0x3f << 16); // clear T1SZ
		tcr |= 16 << 16; //     T1SZ = 16 (48-bit high half)
		tcr &= !(1 << 23); //   EPD1 = 0 (enable TTBR1 walks)
		tcr &= !(3 << 24); //   IRGN1
		tcr |= 1 << 24; //      IRGN1 = WB WA
		tcr &= !(3 << 26); //   ORGN1
		tcr |= 1 << 26; //      ORGN1 = WB WA
		tcr &= !(3 << 28); //   SH1
		tcr |= 3 << 28; //      SH1   = inner shareable
		tcr &= !(3 << 30); //   TG1
		tcr |= 2 << 30; //      TG1   = 0b10 = 4 kB granule
		asm!(
			"msr tcr_el1, {tcr}",
			"msr ttbr1_el1, {root}",
			"isb",
			"tlbi vmalle1",
			"dsb ish",
			"isb",
			tcr = in(reg) tcr,
			root = in(reg) root,
			options(nostack, preserves_flags),
		);
	}
}

// ---- 4 kB page mapping ------------------------------------------------------

// Translate the portable permission flags to a VMSAv8-64 stage-1 L3 page leaf.
fn leaf_bits(flags: u64) -> u64 {
	// A valid L3 page descriptor: bits[1:0] = 0b11 (VALID | "page"), AF set.
	let mut bits = VALID | TABLE | AF;
	if flags & NO_CACHE != 0 {
		bits |= ATTR_DEVICE;
	} else {
		bits |= ATTR_NORMAL | SH_INNER;
	}
	// AP[2:1]: bit6 = accessible at EL0, bit7 = read-only.
	if flags & USER != 0 {
		bits |= 1 << 6;
	}
	if flags & WRITABLE == 0 {
		bits |= 1 << 7;
	}
	// Execute permissions: honour NO_EXECUTE; a user page is never privileged-
	// executable (PXN) even when it stays user-executable (UXN clear).
	if flags & NO_EXECUTE != 0 {
		bits |= PXN | UXN;
	} else if flags & USER != 0 {
		bits |= PXN;
	}
	bits
}

// Map one 4 kB page `va -> pa` in the table tree rooted at `root` (a physical L0
// table address), allocating any missing intermediate tables from the frame
// allocator, then invalidate the TLB for that VA.
unsafe fn map_page_root(root: u64, va: u64, pa: u64, flags: u64) {
	let mut table = root as *mut u64;
	for level in 0..3u64 {
		let shift = 39 - level * 9; // L0=39, L1=30, L2=21
		let idx = ((va >> shift) & 0x1ff) as usize;
		let desc = unsafe { core::ptr::read_volatile(table.add(idx)) };
		let next = if desc & VALID == 0 {
			let frame = alloc_frame().expect("aarch64 map_page: out of frames");
			unsafe { core::ptr::write_volatile(table.add(idx), frame | VALID | TABLE) };
			frame
		} else {
			desc & ADDR_MASK
		};
		table = next as *mut u64;
	}
	let idx = ((va >> 12) & 0x1ff) as usize;
	unsafe {
		core::ptr::write_volatile(table.add(idx), (pa & ADDR_MASK) | leaf_bits(flags));
		asm!(
			"dsb ishst",
			"tlbi vae1, {page}",
			"dsb ish",
			"isb",
			page = in(reg) va >> 12,
			options(nostack, preserves_flags),
		);
	}
}

// The active TTBR0 (low-half) root physical address.
fn current_ttbr0() -> u64 {
	let ttbr0: u64;
	unsafe {
		asm!("mrs {}, ttbr0_el1", out(reg) ttbr0, options(nomem, nostack, preserves_flags));
	}
	ttbr0 & ADDR_MASK
}

// ---- the rest of the paging contract (fills in as the port matures) ----

pub fn enable_nx() {}
pub fn enable_smap_smep() {}
pub fn smap_enabled() -> bool {
	false
}
pub fn smep_enabled() -> bool {
	false
}
pub fn nx_enabled() -> bool {
	false
}
pub fn clac_on_entry() {}

pub fn user_access<R>(f: impl FnOnce() -> R) -> R {
	f()
}

pub unsafe fn copy_to_user_page(_dst: u64, _bytes: &[u8]) {
	todo!("aarch64 4 kB map_page (M116)")
}

pub fn map_page(virt: u64, phys: u64, flags: u64) {
	// A top-bit-set virtual address translates through TTBR1 (higher half); a
	// low address through the active TTBR0 tree.
	let root = if virt >> 63 == 1 { TTBR1_ROOT.load(Ordering::Relaxed) } else { current_ttbr0() };
	unsafe {
		map_page_root(root, virt, phys, flags);
	}
}
pub fn map_page_in(ttbr: u64, virt: u64, phys: u64, flags: u64) {
	unsafe {
		map_page_root(ttbr & ADDR_MASK, virt, phys, flags);
	}
}
pub fn unmap_page(_virt: u64) -> Option<u64> {
	todo!("aarch64 4 kB unmap (M116)")
}
pub fn unmap_pages(_base: u64, _count: usize) {
	todo!("aarch64 4 kB unmap (M116)")
}
pub fn unmap_page_in(_ttbr: u64, _virt: u64) -> Option<u64> {
	todo!("aarch64 4 kB unmap (M116)")
}
pub fn new_address_space() -> Option<u64> {
	todo!("aarch64 per-AS tables (M116)")
}
pub fn free_address_space(_ttbr: u64) {
	todo!("aarch64 per-AS tables (M116)")
}
pub fn remove_bootstrap_identity() {}
