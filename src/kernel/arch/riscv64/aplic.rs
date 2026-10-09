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
// A CLAIMED LINE IS A SOURCE LIKE ANY OTHER, armed by the same function to an identity of its own, and it is
// the one kind that is masked and unmasked after arming: a level line is disabled when it fires and enabled
// again when its driver acknowledges (see `interrupts::bind_wired`).
//
// ONLY THE REGISTERS A WIRED SOURCE NEEDS ARE NAMED. The rest of the domain's configuration belongs
// to M-mode, which sets the MSI address registers this domain delivers through before it hands the
// machine over; a supervisor that wrote them would be writing registers it does not own.

// The APLIC memory map (AIA specification, chapter 4). `sourcecfg` and `target` are arrays indexed
// from ONE - source 0 does not exist - so each is addressed as `base + (source - 1) * 4`.
const DOMAINCFG: u64 = 0x0000;
const SOURCECFG: u64 = 0x0004;
const SETIPNUM: u64 = 0x1cdc;
const SETIE: u64 = 0x1e00;
const SETIENUM: u64 = 0x1edc;
const CLRIENUM: u64 = 0x1fdc;
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

// THE DOMAIN A CLAIMED LINE IS ARMED THROUGH: the S-mode APLIC the device tree's nodes name as their interrupt
// parent, recorded by the boot's describe pass. Zero until then - and on a machine whose nodes name none, which
// has no line to claim.
static DOMAIN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Record `base` as the domain claimed lines are armed through. `false` when another domain was recorded
/// first: a line of a second domain is one this kernel does not arm, and says so rather than arming it in the
/// wrong register block.
pub fn adopt_domain(base: u64) -> bool {
	use core::sync::atomic::Ordering;
	match DOMAIN.compare_exchange(0, base, Ordering::AcqRel, Ordering::Acquire) {
		Ok(_) => true,
		Err(recorded) => recorded == base,
	}
}

/// The domain claimed lines are armed through, if one was described.
pub fn domain() -> Option<u64> {
	let base = DOMAIN.load(core::sync::atomic::Ordering::Acquire);
	(base != 0).then_some(base)
}

// One write of `value` to the register at `offset`.
//
// SAFETY: `base` is a domain this kernel adopted, inside the direct map.
unsafe fn write(base: u64, offset: u64, value: u32) {
	unsafe { core::ptr::write_volatile(super::paging::phys_to_virt(base + offset) as *mut u32, value) };
}

/// Stop forwarding `source`, leaving its configuration: a level line disabled when it fired. An assertion
/// while it is disabled still pends it, and enabling it again forwards that.
///
/// # Safety
/// `base` is the domain `domain` answers.
pub unsafe fn mask_source(base: u64, source: u32) {
	unsafe { write(base, CLRIENUM, source) };
}

/// Forward `source` again - and for a LEVEL line, ask for it again. In MSI delivery mode a level source is
/// pended by its rising edge and its pending bit is cleared by the forward, so a line still asserted when its
/// driver acknowledges has no new edge and would never be forwarded again. Writing its number to `setipnum`
/// pends it if - and only if - it is still asserted, which is the level semantics the driver was promised.
///
/// # Safety
/// As `mask_source`.
pub unsafe fn unmask_source(base: u64, source: u32, level: bool) {
	unsafe {
		write(base, SETIENUM, source);
		if level {
			write(base, SETIPNUM, source);
		}
	}
}

/// Take `source` out of service: disabled, and its mode inactive, so it forwards nothing until it is armed
/// again.
///
/// # Safety
/// As `mask_source`.
pub unsafe fn disarm_source(base: u64, source: u32) {
	if source == 0 || source > MAX_SOURCE {
		return;
	}
	unsafe {
		write(base, CLRIENUM, source);
		write(base, SOURCECFG + (source as u64 - 1) * 4, SM_INACTIVE);
	}
}

/// A temporary kernel alarm may borrow an inactive source, but must not replace a firmware or driver
/// route. The domain must already deliver MSIs, so arming it does not change shared domain settings.
///
/// # Safety
/// `base` is the adopted domain inside the direct map; the binding lock excludes another source owner.
pub unsafe fn inactive_target(base: u64, source: u32) -> Option<u32> {
	if source == 0 || source > MAX_SOURCE {
		return None;
	}
	unsafe {
		let read = |offset| core::ptr::read_volatile(super::paging::phys_to_virt(base + offset) as *const u32);
		if read(DOMAINCFG) & (DOMAINCFG_IE | DOMAINCFG_DM) != DOMAINCFG_IE | DOMAINCFG_DM || read(SOURCECFG + (source as u64 - 1) * 4) != SM_INACTIVE || read(SETIE + (source / 32) as u64 * 4) & (1 << (source % 32)) != 0 {
			return None;
		}
		Some(read(TARGET + (source as u64 - 1) * 4))
	}
}

/// Restore the target of the inactive source borrowed above, after its temporary binding was released.
///
/// # Safety
/// The source was borrowed by `inactive_target` and the binding lock still excludes a replacement owner.
pub unsafe fn restore_inactive_target(base: u64, source: u32, target: u32) {
	unsafe {
		disarm_source(base, source);
		write(base, TARGET + (source as u64 - 1) * 4, target);
	}
}

// THE DOMAIN, WHOLE, ACROSS A HIBERNATION: its configuration, every source's mode and target, and every enable - what a
// restore's fresh boot reset and armed its own way, written back as the image's kernel left it. The sources are read
// to the highest an APLIC addresses; one the controller does not implement reads as inactive and is written so.
struct SavedDomain {
	domaincfg: u32,
	sources: alloc::vec::Vec<(u32, u32)>,
	enables: alloc::vec::Vec<u32>,
}

static SAVED_DOMAIN: crate::sync::SpinLock<Option<SavedDomain>> = crate::sync::SpinLock::new(None);

/// The domain saved; false when there is no memory to save it in - a machine with no APLIC domain saves nothing and
/// answers true.
pub fn save_domain() -> bool {
	let Some(base) = domain() else { return true };
	let words = (MAX_SOURCE as usize + 1).div_ceil(32);
	let mut sources = alloc::vec::Vec::new();
	let mut enables = alloc::vec::Vec::new();
	if sources.try_reserve_exact(MAX_SOURCE as usize).is_err() || enables.try_reserve_exact(words).is_err() {
		return false;
	}
	// SAFETY: the adopted domain, inside the direct map; reads.
	let domaincfg = unsafe {
		let at = |offset: u64| super::paging::phys_to_virt(base + offset) as *const u32;
		for source in 1..=MAX_SOURCE {
			let index = (source as u64 - 1) * 4;
			sources.push((core::ptr::read_volatile(at(SOURCECFG + index)), core::ptr::read_volatile(at(TARGET + index))));
		}
		for word in 0..words {
			enables.push(core::ptr::read_volatile(at(SETIE + word as u64 * 4)));
		}
		core::ptr::read_volatile(at(DOMAINCFG))
	};
	*SAVED_DOMAIN.lock() = Some(SavedDomain { domaincfg, sources, enables });
	true
}

/// The saved domain written back: delivery off while the sources take their modes and targets, every source disabled
/// first, the enables last, then the configuration.
pub fn restore_domain() {
	let Some(base) = domain() else { return };
	let Some(saved) = SAVED_DOMAIN.lock().take() else { return };
	// SAFETY: as `save_domain`, written.
	unsafe {
		let at = |offset: u64| super::paging::phys_to_virt(base + offset) as *mut u32;
		core::ptr::write_volatile(at(DOMAINCFG), saved.domaincfg & !DOMAINCFG_IE);
		for source in 1..=MAX_SOURCE {
			let index = (source as u64 - 1) * 4;
			let (mode, target) = saved.sources[source as usize - 1];
			core::ptr::write_volatile(at(CLRIENUM), source);
			core::ptr::write_volatile(at(SOURCECFG + index), mode);
			if mode != SM_INACTIVE {
				core::ptr::write_volatile(at(TARGET + index), target);
			}
		}
		for (word, &enables) in saved.enables.iter().enumerate() {
			core::ptr::write_volatile(at(SETIE + word as u64 * 4), enables);
		}
		core::ptr::write_volatile(at(DOMAINCFG), saved.domaincfg);
	}
}

// The trigger types a device tree states, which are the standard `IRQ_TYPE_*` values every binding
// uses. A PCI INTx line is level-sensitive; the other two are here because the tree may say so and
// a reader that only understood one would arm the others wrongly rather than refuse them.
const IRQ_TYPE_EDGE_RISING: u32 = 1;
const IRQ_TYPE_EDGE_FALLING: u32 = 2;
const IRQ_TYPE_LEVEL_HIGH: u32 = 4;
const IRQ_TYPE_LEVEL_LOW: u32 = 8;

/// The device-tree trigger type a claimed line's trigger and polarity are.
pub fn trigger_type(level: bool, active_low: bool) -> u32 {
	match (level, active_low) {
		(false, false) => IRQ_TYPE_EDGE_RISING,
		(false, true) => IRQ_TYPE_EDGE_FALLING,
		(true, false) => IRQ_TYPE_LEVEL_HIGH,
		(true, true) => IRQ_TYPE_LEVEL_LOW,
	}
}

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

/// Whether `source` is active and enabled - for the suite, which checks that a level line is disabled while
/// its driver runs and that a release took it out of service.
///
/// # Safety
/// As `mask_source`.
#[cfg(test)]
pub unsafe fn source_armed(base: u64, source: u32) -> bool {
	if source == 0 || source > MAX_SOURCE {
		return false;
	}
	unsafe {
		let at = |offset: u64| super::paging::phys_to_virt(base + offset) as *const u32;
		let mode = core::ptr::read_volatile(at(SOURCECFG + (source as u64 - 1) * 4));
		let enabled = core::ptr::read_volatile(at(SETIE + (source as u64 / 32) * 4)) & (1 << (source % 32)) != 0;
		mode != SM_INACTIVE && enabled
	}
}
