// The RISC-V Advanced Platform-Level Interrupt Controller, for the WIRED lines this kernel answers.
//
// WHY THIS PORT HAD NOTHING LIKE IT UNTIL NOW. Every device driver on this machine takes its
// interrupts as MSIs: a device DMA-writes an identity into a hart's IMSIC file and that is the whole
// delivery path, so the wired controller beside it had no customer. The board's device tree parsed
// its address and the boot discarded it. PCI legacy INTx is the one thing that cannot work that way
// - it is a signal a function asserts, not a write it makes - and a hot-plug port asserts one.
//
// THE MACHINE THIS RUNS ON DELIVERS WIRED LINES AS MSIs, which sounds like a contradiction and is
// the point. QEMU's `virt,aia=aplic-imsic` gives the S-mode domain an APLIC whose delivery mode is
// MSI: the controller watches the wire, and when a source asserts it WRITES the identity this code
// puts in the source's target register to the hart's IMSIC file. So the interrupt arrives at the
// same place every device MSI arrives, is claimed by the same `stopei` read, and needs no second
// dispatch path in the trap handler - only a handler table that is asked first. What this module
// does is the half the IMSIC cannot: tell the APLIC which wire, how it asserts, and what identity
// to send when it does.
//
// ONLY THE REGISTERS A WIRED SOURCE NEEDS ARE NAMED. The rest of the domain's configuration belongs
// to M-mode, which sets the MSI address registers this domain delivers through before it hands the
// machine over; a supervisor that wrote them would be writing registers it does not own.

// The APLIC memory map (AIA specification, chapter 4). `sourcecfg` and `target` are arrays indexed
// from ONE - source 0 does not exist - so each is addressed as `base + (source - 1) * 4`.
const DOMAINCFG: u64 = 0x0000;
const SOURCECFG: u64 = 0x0004;
const SETIENUM: u64 = 0x1edc;
const TARGET: u64 = 0x3004;

// `domaincfg`: deliver interrupts at all, and deliver them as MSIs.
const DOMAINCFG_IE: u32 = 1 << 8;
const DOMAINCFG_DM: u32 = 1 << 2;

// `sourcecfg` source modes, for a source this domain has not delegated to a child.
const SM_INACTIVE: u32 = 0;
const SM_EDGE_RISING: u32 = 4;
const SM_EDGE_FALLING: u32 = 5;
const SM_LEVEL_HIGH: u32 = 6;
const SM_LEVEL_LOW: u32 = 7;

// `target`, in MSI delivery mode: the hart index in the top fourteen bits, the guest index in six
// more, and the identity the controller writes in the low eleven.
const TARGET_HART_SHIFT: u32 = 18;
const TARGET_EIID_MASK: u32 = 0x7ff;

// The highest source an APLIC addresses.
const MAX_SOURCE: u32 = 1023;

// The trigger types a device tree states, which are the standard `IRQ_TYPE_*` values every binding
// uses. A PCI INTx line is level-sensitive; the other two are here because the tree may say so and
// a reader that only understood one would arm the others wrongly rather than refuse them.
const IRQ_TYPE_EDGE_RISING: u32 = 1;
const IRQ_TYPE_EDGE_FALLING: u32 = 2;
const IRQ_TYPE_LEVEL_HIGH: u32 = 4;
const IRQ_TYPE_LEVEL_LOW: u32 = 8;

/// This controller's source mode for a trigger type the device tree stated, or `None` for one it
/// does not describe - which is a board this kernel refuses to arm rather than one it guesses at.
pub fn source_mode(trigger: u32) -> Option<u32> {
	match trigger {
		IRQ_TYPE_EDGE_RISING => Some(SM_EDGE_RISING),
		IRQ_TYPE_EDGE_FALLING => Some(SM_EDGE_FALLING),
		IRQ_TYPE_LEVEL_HIGH => Some(SM_LEVEL_HIGH),
		IRQ_TYPE_LEVEL_LOW => Some(SM_LEVEL_LOW),
		_ => None,
	}
}

/// Arm one wired source: say how it asserts, point it at `hart`'s interrupt file with identity
/// `eid`, and enable it. `false` when the controller did not take it, which is a source the tree
/// named and the hardware does not have.
///
/// # Safety
/// `base` must be the S-mode APLIC domain's register block as this machine's device tree places it,
/// reachable through the direct map.
pub unsafe fn arm_source(base: u64, source: u32, trigger: u32, hart: u64, eid: u32) -> bool {
	// EIGHTEEN BITS OF HART INDEX AND ELEVEN OF IDENTITY, and both are checked rather than masked:
	// a hart or an identity that does not fit is one this write would silently change into another
	// one, and the interrupt would then be delivered somewhere real and wrong.
	if source == 0 || source > MAX_SOURCE || eid == 0 || eid > TARGET_EIID_MASK || hart > 0x3fff {
		return false;
	}
	let Some(mode) = source_mode(trigger) else { return false };
	let index: u64 = (source as u64 - 1) * 4;
	unsafe {
		let at = |offset: u64| super::paging::phys_to_virt(base + offset) as *mut u32;
		// READ-MODIFY-WRITE, BECAUSE THE OTHER BITS ARE NOT OURS. `domaincfg` also carries the
		// domain's byte-order flag and a read-only identity in its top byte; a blind write would
		// clear the endianness of a machine that set it and report nothing.
		let domaincfg = at(DOMAINCFG);
		core::ptr::write_volatile(domaincfg, core::ptr::read_volatile(domaincfg) | DOMAINCFG_IE | DOMAINCFG_DM);
		// SOURCECFG, THEN TARGET, THEN THE ENABLE, in that order and not another. Making a source
		// active while its target still holds whatever reset left there would deliver its first
		// assertion to that identity - and a hot-plug slot that was occupied at reset has its
		// presence bit latched already, so the first assertion can be immediate.
		core::ptr::write_volatile(at(SOURCECFG + index), mode);
		core::ptr::write_volatile(at(TARGET + index), ((hart as u32) << TARGET_HART_SHIFT) | (eid & TARGET_EIID_MASK));
		core::ptr::write_volatile(at(SETIENUM), source);
		// AND READ THE MODE BACK. An APLIC that does not implement a source reads its `sourcecfg`
		// back as inactive whatever was written, so a tree naming a source this controller does not
		// have would otherwise leave a line reported as armed that can never fire - which is the
		// one failure a hot-plug path must not have, because nothing else would ever say so.
		let settled = core::ptr::read_volatile(at(SOURCECFG + index));
		settled != SM_INACTIVE && settled == mode
	}
}
