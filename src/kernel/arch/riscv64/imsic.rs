// RISC-V AIA IMSIC (Incoming MSI Controller) - per-hart MSI target (M117).
//
// With QEMU's `virt,aia=aplic-imsic`, PCIe devices deliver MSI-X messages instead of
// wired INTx: a device signals by DMA-writing its interrupt identity (EID) to the target
// hart's IMSIC S-mode interrupt file (a 4 KiB MMIO page). The IMSIC then sets that EID's
// pending bit and raises the hart's S-mode external-interrupt line (SCAUSE code 9). This
// gives every device its own edge-triggered EID - no INTx line sharing - so, unlike the
// PLIC's four shared PCIe INTx sources, interrupt delivery to the full device set is
// reliable (mirroring the x86 LAPIC-MSI and aarch64 GICv2m backends).
//
// The IMSIC's registers (EIDELIVERY, EITHRESHOLD, the EIP/EIE arrays) are accessed per
// hart through the indirect S-mode CSRs siselect (0x150) / sireg (0x151); the top pending
// EID is claimed through stopei (0x15C). Each hart programs only its own file, so a
// device's MSI targets the hart that acquired it (the one running the setup syscall).

#![allow(dead_code)]

use core::sync::atomic::{AtomicUsize, Ordering};

// QEMU virt (aia=aplic-imsic): the S-mode IMSIC files start at 0x2800_0000, one 4 KiB
// page per hart, HART_STRIDE apart. A device MSI targets hart H by writing its EID to
// that hart's page. Overridable via set_base from the device tree.
const IMSIC_S_DEFAULT: usize = 0x2800_0000;
const HART_STRIDE: usize = 0x1000;
static IMSIC_S_BASE: AtomicUsize = AtomicUsize::new(IMSIC_S_DEFAULT);

// Indirect-CSR register selects for siselect.
const EIDELIVERY: usize = 0x70; // interrupt delivery enable
const EITHRESHOLD: usize = 0x72; // priority threshold (0 = accept all)
const EIE0: usize = 0xC0; // enable bits for EIDs 0..63 (RV64: one 64-bit register)

// Record the S-mode IMSIC base (from the device tree; defaults to the QEMU virt layout).
pub fn set_base(addr: u64) {
	if addr != 0 {
		IMSIC_S_BASE.store(addr as usize, Ordering::Relaxed);
	}
}

fn base() -> usize {
	IMSIC_S_BASE.load(Ordering::Relaxed)
}

pub fn ready() -> bool {
	base() != 0
}

// The physical MSI target address for hart `hart`'s S-mode interrupt file - what a
// device's MSI-X table entry stores so its DMA write lands in that hart's IMSIC.
pub fn msi_address(hart: u64) -> u64 {
	(base() + hart as usize * HART_STRIDE) as u64
}

// Select an IMSIC register on THIS hart and write it (siselect then sireg).
unsafe fn ireg_write(select: usize, val: usize) {
	unsafe {
		core::arch::asm!(
			"csrw 0x150, {s}",
			"csrw 0x151, {v}",
			s = in(reg) select,
			v = in(reg) val,
			options(nostack, preserves_flags),
		);
	}
}

// Select an IMSIC register on THIS hart and read it.
unsafe fn ireg_read(select: usize) -> usize {
	let val: usize;
	unsafe {
		core::arch::asm!(
			"csrw 0x150, {s}",
			"csrr {v}, 0x151",
			s = in(reg) select,
			v = out(reg) val,
			options(nostack, preserves_flags),
		);
	}
	val
}

// Bring up THIS hart's IMSIC S-file: enable interrupt delivery and accept any priority,
// so an EID a device targets here raises the hart's S-mode external interrupt.
pub fn init_hart() {
	unsafe {
		ireg_write(EIDELIVERY, 1);
		ireg_write(EITHRESHOLD, 0);
	}
}

// Enable EID `eid` on THIS hart's IMSIC (set its EIE bit). Must run on the hart the
// device's MSI targets - acquire_msi enables it on the hart doing the acquire.
pub fn enable_eid(eid: u32) {
	if eid == 0 || eid >= 64 {
		return;
	}
	unsafe {
		let cur = ireg_read(EIE0);
		ireg_write(EIE0, cur | (1usize << eid));
	}
}

// Disable EID `eid` on THIS hart's IMSIC (clear its EIE bit).
pub fn disable_eid(eid: u32) {
	if eid == 0 || eid >= 64 {
		return;
	}
	unsafe {
		let cur = ireg_read(EIE0);
		ireg_write(EIE0, cur & !(1usize << eid));
	}
}

// Claim the top pending-and-enabled external interrupt through stopei, clearing its
// pending bit (edge-triggered). Returns its EID (identity in bits 26:16), 0 if none.
pub fn claim() -> u32 {
	let top: usize;
	unsafe {
		// csrrw rd, stopei, rs1 with rd == rs1 (seeded 0): writes 0 (claims the top
		// interrupt, clearing its pending bit) and reads the pre-claim top into rd.
		core::arch::asm!(
			"csrrw {t}, 0x15c, {t}",
			t = inout(reg) 0usize => top,
			options(nostack, preserves_flags),
		);
	}
	(top >> 16) as u32
}

// Service an S-mode external interrupt (SCAUSE code 9): claim each pending EID and wake
// its bound driver, until none remain. Called from the trap handler.
pub fn handle_external() {
	loop {
		let eid = claim();
		if eid == 0 {
			break;
		}
		super::interrupts::dispatch_msi(eid);
	}
}
