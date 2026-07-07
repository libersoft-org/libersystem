// aarch64 paging - VMSAv8-64 translation (M116).
//
// The boot MMU: an identity map (VA == PA) of the low physical space via TTBR0 -
// the device MMIO region (0..1 GB, covers the PL011 UART, the GIC, the low ECAM)
// as Device memory, and up to 3 GB of DRAM (1..4 GB) as Normal cacheable RWX -
// built from 1 GB block descriptors in a static boot table pool, then the MMU is
// turned on (MAIR / TCR / TTBR0 / SCTLR.M). Because the map is identity, the PC,
// the stack, and the UART keep working across the enable.
//
// `translate` walks those tables. The full contract (4 kB `map_page` / `unmap` /
// per-address-space `new_address_space` + TTBR1 higher-half + W^X) fills in as the
// port routes through the portable memory subsystem; those stay `todo!()` for now.

use core::arch::asm;

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

pub fn map_page(_virt: u64, _phys: u64, _flags: u64) {
	todo!("aarch64 4 kB map_page (M116)")
}
pub fn map_page_in(_ttbr: u64, _virt: u64, _phys: u64, _flags: u64) {
	todo!("aarch64 4 kB map_page (M116)")
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
