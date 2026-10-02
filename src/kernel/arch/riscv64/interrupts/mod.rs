// riscv64 device-interrupt binding + MSI-X delivery via the AIA IMSIC.
//
// With QEMU's `virt,aia=aplic-imsic`, PCIe devices deliver MSI-X instead of wired INTx:
// a device signals by DMA-writing an interrupt identity (EID) to a hart's IMSIC S-mode
// file (imsic.rs), which pends that EID and raises the hart's external interrupt. So a
// device's MSI "vector" here is its EID: acquire_msi hands out a free EID, programs the
// device's MSI-X table entry to write it to the acquiring hart's IMSIC file, enables the
// EID there, and imsic::handle_external wakes the bound Interrupt when that EID fires.
//
// This mirrors the x86 (LAPIC-MSI) and aarch64 (GICv2m) backends: every PCI driver that needs
// an interrupt uses MSI-X and the polled drivers (virtio-blk) need none; a wired line reaches a
// driver only as a platform row's claimed line (`bind_wired`), which the APLIC turns into an
// identity of its own. Unlike the old PLIC INTx path, EIDs are per-device - no shared
// identity, reliable delivery. The MSI-X table lives in a device BAR reached through the
// higher-half direct map (phys_to_virt), so no separate uncacheable mapping is needed.

use alloc::sync::{Arc, Weak};

use crate::arch::common::msi::MsiRegistry;
use crate::object::interrupt::Interrupt;
use crate::sync::SpinLock;

// Device EIDs run 1..=MAX_MSI (EID 0 is "no interrupt"; the IMSIC EIE0 register holds
// EIDs 0..63 on RV64, so a single register covers them). Slot i (in the registry) maps
// to EID EID_BASE + i.
const EID_BASE: u32 = 1;
const MAX_MSI: usize = 54; // EIDs 1..=54, all within IMSIC EIE0

// THE CLAIMED LINES' IDENTITIES, 55..=62: between the device window and `WIRED_EID`, one per claimed line,
// so a line's driver is woken by the identity its line was armed with rather than by a sweep.
const LINE_EID_BASE: u32 = EID_BASE + MAX_MSI as u32;
const MAX_LINES: usize = 8;

// The per-device MSI slot bindings (reserve / bind / dispatch / free bookkeeping, shared
// with x86/aarch64 via arch::common::msi). Slot i maps to EID EID_BASE + i.
static REGISTRY: MsiRegistry<MAX_MSI> = MsiRegistry::new();
// THE MSI-X ENTRY EACH SLOT PROGRAMMED, by its physical address - 0 for none: what a sleep masks the slot's messages at.
static SLOT_TABLE: [core::sync::atomic::AtomicU64; MAX_MSI] = [const { core::sync::atomic::AtomicU64::new(0) }; MAX_MSI];

// The registry slot an EID maps to, or None if it is outside the MSI window.
fn eid_slot(eid: u32) -> Option<usize> {
	if eid >= EID_BASE && ((eid - EID_BASE) as usize) < MAX_MSI { Some((eid - EID_BASE) as usize) } else { None }
}

// THE IDENTITY THIS KERNEL'S OWN WIRED LINES ARRIVE UNDER.
//
// The device window above is EIDs 1..=54, which is what `MAX_MSI` says and what `eid_slot` maps, and
// the claimed lines' 55..=62 follow it.
// The IMSIC's `EIE0` register holds identities 0..63 on RV64, so 63 is the one identity inside the
// register this kernel can address and outside everything it hands to a device. A wired line armed
// on it can never collide with a driver's vector, and the two paths never have to agree about
// anything beyond that number.
//
// ONE IDENTITY FOR EVERY WIRED LINE, WHICH IS WHAT A SHARED LINE ALREADY IS. Four hot-plug ports
// swizzle onto four APLIC sources and any two can land on one; the handler answers by looking at
// every slot rather than at the one it hopes asserted, so giving each source its own identity would
// buy nothing and spend the identities drivers need.
// `not(test)` like the registration below and the controller it targets: the only thing that names
// this identity is the boot's arming pass.
#[cfg(not(test))]
pub const WIRED_EID: u32 = 63;

// THE KERNEL'S OWN WIRED LINES, WHICH ARE NOT DRIVER BINDINGS. See `arch::common::wired`, and
// aarch64's copy of this comment: what these answer has no process behind it and must run inside
// the interrupt.
static WIRED: crate::arch::common::wired::Wired<8> = crate::arch::common::wired::Wired::new();

/// `not(test)` ON THE TYPE AND THE REGISTRATION, AND NOT ON THE DISPATCH, which is the honest
/// split: the interrupt path calls `dispatch_wired` on every event and is compiled in every build,
/// while the only thing that REGISTERS a handler is the boot's arming pass.
///
/// What answers one of this kernel's own wired lines: told the identity it was raised for.
#[cfg(not(test))]
pub type HandlerFn = crate::arch::common::wired::Handler;

/// Answer `eid` with `handler` from now on. `false` when this kernel already answers as many wired
/// lines as it carries rows for, which the caller reports rather than swallows.
#[cfg(not(test))]
pub fn register(eid: u32, handler: HandlerFn) -> bool {
	WIRED.register(eid, handler)
}

/// Run this kernel's own handler for `eid`, and say whether there was one. `false` sends the
/// identity on to the claimed lines and the MSI registry, which is where every device's belongs.
pub fn dispatch_wired(eid: u32) -> bool {
	WIRED.dispatch(eid)
}

// THE KERNEL'S OWN SOURCES - the console UART's and each hot-plug slot's - which no claim may re-arm: a claim
// that rewrote such a source's target would move the kernel's own line to a driver's identity without a word.
// One bit per source the controller addresses.
static KERNEL_SOURCES: [core::sync::atomic::AtomicU64; 16] = [const { core::sync::atomic::AtomicU64::new(0) }; 16];

/// Record `source` as one the kernel armed for itself.
#[cfg(not(test))]
pub fn hold_source(source: u32) {
	if let Some(word) = KERNEL_SOURCES.get((source / 64) as usize) {
		word.fetch_or(1 << (source % 64), core::sync::atomic::Ordering::AcqRel);
	}
}

fn kernel_holds(source: u32) -> bool {
	KERNEL_SOURCES.get((source / 64) as usize).is_some_and(|word| word.load(core::sync::atomic::Ordering::Acquire) & (1 << (source % 64)) != 0)
}

// A CLAIMED WIRED LINE: the APLIC source it is, whether it is level-triggered, and the `Interrupt` its driver
// waits on - held weakly, so the driver letting go (its Interrupt's `Drop`) is what unbinds it. Its identity is
// its slot's.
struct Line {
	source: u32,
	level: bool,
	intr: Weak<Interrupt>,
}

// A slot is FREE, HELD by a claim, or STRANDED: its line's teardown has not confirmed - the identity is still
// enabled in a file whose hart did not answer, or is being disabled now - so it is not handed out, since the
// hart that owes the disable would clear the next owner's enable when it finally ran it.
enum LineSlot {
	Free,
	Held(Line),
	Stranded,
}

static LINES: [SpinLock<LineSlot>; MAX_LINES] = [const { SpinLock::new(LineSlot::Free) }; MAX_LINES];

// Binds one at a time, so two cannot choose the same free identity.
static BINDING: SpinLock<()> = SpinLock::new(());

// The `LINES` slot an identity names.
fn line_slot(eid: u32) -> Option<usize> {
	(eid >= LINE_EID_BASE && ((eid - LINE_EID_BASE) as usize) < MAX_LINES).then(|| (eid - LINE_EID_BASE) as usize)
}

// BIND A PLATFORM ROW'S WIRED LINE to a new `Interrupt`: its APLIC source armed with the row's trigger and
// polarity, in the adopted domain, to an identity of the claimed-line window in THIS hart's interrupt file -
// the file an MSI acquired here would target. Refused, in words: a line of another controller, a machine whose
// tree named no APLIC domain (one without AIA among them), a source the kernel armed for itself, one another
// claim holds, a hart with no file, or a full table.
pub fn bind_wired(line: &abi::WiredLine) -> Result<Arc<Interrupt>, &'static str> {
	if line.controller != abi::LINE_CONTROLLER_APLIC {
		return Err("its line is not an APLIC's");
	}
	let source = line.number;
	let Some(base) = super::aplic::domain() else { return Err("this machine described no APLIC domain to arm it through") };
	if kernel_holds(source) {
		return Err("the kernel answers that line itself");
	}
	if !super::imsic::usable() {
		return Err("this machine's interrupt files are out of service");
	}
	let level = line.trigger == abi::LINE_TRIGGER_LEVEL;
	let trigger = super::aplic::trigger_type(level, line.polarity == abi::LINE_POLARITY_LOW);
	// THE HART IS READ UNDER THE LOCK, which masks interrupts: the identity is enabled in the file of the hart
	// the controller is pointed at, and a migration between the two would split them.
	let _binding = BINDING.lock();
	let hart = super::percpu::this_cpu().lapic_id();
	if !super::imsic::has_file(hart) {
		return Err("this hart has no interrupt file to deliver it to");
	}
	if LINES.iter().any(|slot| matches!(&*slot.lock(), LineSlot::Held(held) if held.source == source)) {
		return Err("another claim holds that line");
	}
	let Some(free) = LINES.iter().position(|slot| matches!(&*slot.lock(), LineSlot::Free)) else {
		return Err("this kernel's table of claimed lines is full");
	};
	let eid = LINE_EID_BASE + free as u32;
	let Some(intr) = Interrupt::new(eid) else { return Err("the Interrupt object could not be allocated") };
	*LINES[free].lock() = LineSlot::Held(Line { source, level, intr: Arc::downgrade(&intr) });
	intr.mark_bound();
	super::imsic::enable_eid(eid);
	// SAFETY: `base` is the domain the boot's own tree named as the platform nodes' interrupt parent, inside
	// the direct map.
	if !unsafe { super::aplic::arm_source(base, source, trigger, hart, eid) } {
		let _ = intr.disown();
		// SAFETY: as above.
		unsafe { super::aplic::disarm_source(base, source) };
		let confirmed = super::imsic::disable_eid_on_owner(eid);
		*LINES[free].lock() = if confirmed { LineSlot::Free } else { LineSlot::Stranded };
		return Err("the controller did not take its source");
	}
	// A LEVEL LINE ALREADY ASSERTED is pended by no edge: asked for once, so a device waiting since before the
	// claim is heard.
	if level {
		// SAFETY: as above.
		unsafe { super::aplic::unmask_source(base, source, true) };
	}
	Ok(intr)
}

// Whether a claimed line is live at its controller - for the suite, as on x86_64.
#[cfg(test)]
pub fn line_armed(line: &abi::WiredLine) -> bool {
	// SAFETY: the adopted domain, as in `bind_wired`.
	super::aplic::domain().is_some_and(|base| unsafe { super::aplic::source_armed(base, line.number) })
}

// A CLAIMED LINE FIRED: its driver is woken, and a LEVEL line is disabled at the controller until the driver
// acknowledges. True for any identity of the window - a stranded slot's late message is consumed here, with
// nobody to wake.
pub fn signal_line(eid: u32) -> bool {
	let Some(at) = line_slot(eid) else { return false };
	let held = LINES[at].lock();
	let LineSlot::Held(line) = &*held else { return true };
	if line.level
		&& let Some(base) = super::aplic::domain()
	{
		// SAFETY: the adopted domain, as in `bind_wired`.
		unsafe { super::aplic::mask_source(base, line.source) };
	}
	if let Some(intr) = line.intr.upgrade() {
		intr.signal();
	}
	true
}

// THE DRIVER ACKNOWLEDGED: a level line disabled when it fired is enabled again, and asked for again in case it
// is still asserted - see `aplic::unmask_source`.
pub fn acknowledge(vector: u32) {
	let Some(at) = line_slot(vector) else { return };
	if let LineSlot::Held(line) = &*LINES[at].lock()
		&& line.level
		&& let Some(base) = super::aplic::domain()
	{
		// SAFETY: as in `signal_line`.
		unsafe { super::aplic::unmask_source(base, line.source, true) };
	}
}

// Remove any binding for `vector` (an EID; called from an Interrupt's Drop). The EID's IMSIC enable
// bit is cleared IN THE FILE THAT HOLDS IT - which is not necessarily this hart's - so a later stray
// MSI to it pends and dispatches to no one WHILE IT STAYS UNOWNED. Reallocate the EID and that same
// stray message wakes its next owner, which is the defect the x86 backend spells out; so the slot is
// retired rather than freed and waits for `SYS_DEVICE_QUIESCED`.
//
// AND IF THE OWNING HART DOES NOT ANSWER, the identity is still armed somewhere, so the slot is
// quarantined instead: it leaks a vector rather than handing a live one to the next driver.
// Returns whether the teardown CONFIRMED. This is the port where it can fail: the EID is disabled by
// the hart that owns it, and a hart that does not answer leaves the slot armed - which is why the
// unconfirmed branch quarantines rather than retires. The answer is reported so a claim's terminal
// state can include it, instead of being decided by the IOMMU alone while a still-armed vector is
// charged to the claim.
pub fn unbind(vector: u32) -> bool {
	// A CLAIMED WIRED LINE: its source disarmed, then its identity disabled where it was enabled - outside the
	// slot's lock, since the owning hart may be in `signal_line` for this very slot and must be able to take it
	// to reach its mailbox - and only then the slot given back. An unanswered disable strands it.
	if let Some(at) = line_slot(vector) {
		let source = {
			let mut held = LINES[at].lock();
			match core::mem::replace(&mut *held, LineSlot::Stranded) {
				LineSlot::Held(line) => line.source,
				other => {
					*held = other;
					return true;
				}
			}
		};
		if let Some(base) = super::aplic::domain() {
			// SAFETY: as in `bind_wired`.
			unsafe { super::aplic::disarm_source(base, source) };
		}
		let confirmed = super::imsic::disable_eid_on_owner(vector);
		if confirmed {
			*LINES[at].lock() = LineSlot::Free;
		}
		return confirmed;
	}
	let Some(slot) = eid_slot(vector) else { return true };
	SLOT_TABLE[slot].store(0, core::sync::atomic::Ordering::Release);
	if super::imsic::disable_eid_on_owner(vector) {
		REGISTRY.retire(slot);
		return true;
	}
	REGISTRY.quarantine(slot);
	false
}

// Allocate a free EID and program a device's MSI-X table entry 0 so the device delivers
// it: message address = the acquiring hart's IMSIC S-file, message data = the EID. The
// EID is enabled on THIS hart (the one running the acquire), so the device's MSI targets
// it. `table_phys` is the device's MSI-X table (reached through the higher-half direct
// map). Returns the EID as the vector (None if every slot is taken); the caller enables
// MSI-X on the device (pci::msix_enable) and binds an Interrupt with bind_msi. `owner` is
// the discovered-device index (for the `lsirq` inventory); `dest` (the x86 LAPIC target)
// is unused - IMSIC targets the current hart.
// Give back a vector whose Interrupt never reached its owner - see the x86_64 `release_unused_msi`
// for why this is a free rather than a retire.
pub fn release_unused_msi(vector: u32) {
	if let Some(slot) = eid_slot(vector) {
		SLOT_TABLE[slot].store(0, core::sync::atomic::Ordering::Release);
		// Same rule as `unbind`: an identity that could not be disabled is not one to hand back,
		// even though this vector never reached a driver.
		if super::imsic::disable_eid_on_owner(vector) {
			REGISTRY.free(slot);
		} else {
			REGISTRY.quarantine(slot);
		}
	}
}

// One vector per device, entry 0 - see the x86_64 `acquire_msi`, which states the limit and why it
// exists. `MsiRegistry::acquire` does NOT enforce it, which this comment used to claim: the form that
// does is `acquire_unique_live`, reached through `acquire_msi_unique` below.
#[cfg(test)]
pub fn acquire_msi(table_phys: u64, _dest: u8, owner: u32) -> Option<u32> {
	program_acquired(REGISTRY.acquire(owner, MAX_MSI)?, table_phys)
}

// The same, and ONLY IF the device holds no live vector already - see
// `MsiRegistry::acquire_unique_live`. This is what `sys_device_msix_acquire` calls; the form above
// stays for the kernel's own bring-up test.
pub fn acquire_msi_unique(table_phys: u64, _dest: u8, owner: u32) -> Option<u32> {
	program_acquired(REGISTRY.acquire_unique_live(owner, MAX_MSI)?, table_phys)
}

fn program_acquired(slot: usize, table_phys: u64) -> Option<u32> {
	// A MACHINE WHOSE IMSIC THIS KERNEL COULD NOT ADDRESS HANDS OUT NO VECTOR. The boot said so and
	// took the path out of service; programming a table entry now would write the compiled address
	// this port refused to use, which is the whole point of having refused it.
	if !super::imsic::usable() {
		REGISTRY.free(slot);
		return None;
	}
	let eid = EID_BASE + slot as u32;
	let hart = super::percpu::this_cpu().lapic_id();
	// A HART WITH NO INTERRUPT FILE IS NOT AN MSI TARGET. `msi_address` is `base + hart * stride`,
	// so a hart past the array the controller declares names an address inside something else.
	if !super::imsic::has_file(hart) {
		REGISTRY.free(slot);
		return None;
	}
	program_msix_entry(table_phys, super::imsic::msi_address(hart), eid);
	SLOT_TABLE[slot].store(table_phys, core::sync::atomic::Ordering::Release);
	super::imsic::enable_eid(eid);
	// The whole EID (KERN-ARCH-017): IMSIC identifiers are eleven bits, so narrowing one here
	// would arm the hardware under an identifier the kernel never records.
	Some(eid)
}

// Write a device's MSI-X table entry 0 (reached through the physical direct map): the
// message address is a hart's IMSIC S-file, so the device's DMA write of the message
// data (the EID) pends that EID on that hart. Vector control = 1 (MASKED until
// `unmask_msi`). A driver must never write its own MSI-X table; only the kernel
// programs it here.
//
// PROGRAMMED MASKED, AND UNMASKED ONLY WHEN THE ACQUIRE HAS COMMITTED (2026-09-01) - see the
// x86_64 port's comment at the same function for the stale-generation window this closes.
fn program_msix_entry(table_phys: u64, msg_addr: u64, eid: u32) {
	let entry = super::paging::phys_to_virt(table_phys) as *mut u32;
	unsafe {
		entry.add(0).write_volatile(msg_addr as u32); // message address low
		entry.add(1).write_volatile((msg_addr >> 32) as u32); // message address high
		entry.add(2).write_volatile(eid); // message data = the EID
		entry.add(3).write_volatile(1); // vector control (MASKED until `unmask_msi`)
	}
}

// Make the entry programmed above deliverable - the other half of its mask. This port reaches the
// table through the physical direct map, so the caller supplies the address it programmed.
pub fn unmask_msi(_vector: u32, table_phys: u64) {
	let entry = super::paging::phys_to_virt(table_phys) as *mut u32;
	// SAFETY: the same entry `program_msix_entry` wrote, through the same mapping.
	unsafe { entry.add(3).write_volatile(0) };
}

// Bind `intr` to an MSI `vector` (an EID) so dispatch wakes it when the EID fires.
// Returns false if the vector is already bound to a live Interrupt.
pub fn bind_msi(vector: u32, intr: &Arc<Interrupt>) -> bool {
	match eid_slot(vector) {
		Some(slot) => REGISTRY.bind(slot, intr),
		None => false,
	}
}

// Whether `vector` (an EID) currently has a live driver binding - an MSI or a claimed line.
pub fn is_bound(vector: u32) -> bool {
	if let Some(at) = line_slot(vector) {
		return matches!(&*LINES[at].lock(), LineSlot::Held(line) if line.intr.strong_count() != 0);
	}
	match eid_slot(vector) {
		Some(slot) => REGISTRY.is_bound(slot),
		None => false,
	}
}

// End-of-interrupt for a serviced vector. IMSIC MSI is edge-triggered and unshared, so
// there is no level source to complete: a no-op (the stopei claim in handle_external
// already cleared the EID's pending bit), kept for the portable SYS_INTERRUPT_ACK path.
pub fn eoi(_vector: u32) {}

// Deliver a fired EID to its bound MSI driver. Returns true when the EID was a bound MSI
// vector (signaled here). Edge-triggered: just wake the bound driver.
pub fn dispatch_msi(eid: u32) -> bool {
	match eid_slot(eid) {
		Some(slot) => {
			REGISTRY.dispatch(slot);
			true
		}
		None => false,
	}
}

// The state of the vector at `index`, for the `lsirq` inventory. Index 0 is the kernel's
// own timer - the S-mode timer interrupt (SCAUSE code 5) - shown as a fixed vector like
// x86's LAPIC timer and aarch64's EL1 physical-timer PPI; the MSI window (each a device's
// EID) follows.
// Free every MSI vector that is masked and waiting for `device` to be confirmed stopped, and answer
// how many. Reached from `SYS_DEVICE_QUIESCED`.
// How many MSI slots this device still holds. See `MsiRegistry::held_by_device`.
// WHETHER THIS DEVICE STILL HOLDS A SLOT WHOSE TEARDOWN HAS NOT HAPPENED.
//
// `unbind` RETIRES a slot (pending) and a disable the controller refused QUARANTINES it, so a slot
// that is neither is one still bound - and a claim release that publishes `Free` over one of those
// gives a vector back while an unbind is still on its way to it. See `device::settled_vectors` and
// `MsiRegistry::has_unbound`, which states why a quarantined slot is settled rather than outstanding.
pub fn msi_live_for_device(device: u32) -> bool {
	REGISTRY.has_unbound(device)
}

// How many of this device's slots are QUARANTINED - a vector stranded by a teardown the controller
// refused. Sampled either side of a release so the ones stranded by THAT release can be told from
// the ones it inherited. See `MsiRegistry::quarantined_for` and `device::release_claim`.
pub fn msi_quarantined_for_device(device: u32) -> usize {
	REGISTRY.quarantined_for(device)
}

pub fn msi_held_by_device(device: u32) -> usize {
	REGISTRY.held_by_device(device)
}

pub fn release_msi_for_device(device: u32) -> usize {
	REGISTRY.release_for_device(device)
}

pub fn irq_info(index: usize) -> Option<abi::IrqInfo> {
	const TIMER_VECTOR: u32 = 5; // supervisor timer interrupt (scause code 5)
	if index == 0 {
		return Some(abi::IrqInfo { vector: TIMER_VECTOR, kind: abi::IRQ_KIND_FIXED, bound: 1, device: abi::IRQ_NO_DEVICE });
	}
	let slot = index - 1;
	if slot >= MAX_MSI {
		return None;
	}
	let eid = EID_BASE + slot as u32;
	Some(abi::IrqInfo { vector: eid, kind: abi::IRQ_KIND_MSI, bound: is_bound(eid) as u32, device: REGISTRY.owner(slot) })
}

// Take a vector out of circulation the way an unanswered cross-hart disable does, so the rule can
// be tested without wedging a hart with interrupts off.
#[cfg(test)]
pub fn quarantine_for_test(vector: u32) {
	if let Some(slot) = eid_slot(vector) {
		REGISTRY.quarantine(slot);
	}
}

// Whether `vector` is out of circulation for the life of the boot.
#[cfg(test)]
pub fn is_quarantined(vector: u32) -> bool {
	eid_slot(vector).is_some_and(|slot| REGISTRY.is_quarantined(slot))
}

// The number of vectors irq_info reports over (the timer entry plus the MSI window).
pub fn irq_info_len() -> usize {
	1 + MAX_MSI
}

#[cfg(test)]
mod tests;

// ------------------------------------------------------------------ a sleep's device lines

// A SUSPEND TO IDLE MASKS EVERY CLAIMED LINE at the APLIC and every MSI-X entry it programmed at the device - each driver
// has quiesced its device - but the wake set's, which its driver marked and whose firing is what ends the sleep; and
// puts back exactly what it masked.
static SLEEP_MASKED_LINES: [core::sync::atomic::AtomicBool; MAX_LINES] = [const { core::sync::atomic::AtomicBool::new(false) }; MAX_LINES];
static SLEEP_MASKED_MSI: [core::sync::atomic::AtomicBool; MAX_MSI] = [const { core::sync::atomic::AtomicBool::new(false) }; MAX_MSI];

pub fn mask_claimed_lines() {
	let Some(base) = super::aplic::domain() else { return };
	for (at, slot) in LINES.iter().enumerate() {
		let source = match &*slot.lock() {
			LineSlot::Held(line) => Some(line.source),
			_ => None,
		};
		if let Some(source) = source
			&& !crate::sleep::is_wake(LINE_EID_BASE + at as u32)
		{
			// SAFETY: the adopted domain, as in `bind_wired`.
			unsafe { super::aplic::mask_source(base, source) };
			SLEEP_MASKED_LINES[at].store(true, core::sync::atomic::Ordering::Release);
		}
	}
}

pub fn unmask_claimed_lines() {
	let Some(base) = super::aplic::domain() else { return };
	for (at, slot) in LINES.iter().enumerate() {
		if !SLEEP_MASKED_LINES[at].swap(false, core::sync::atomic::Ordering::AcqRel) {
			continue;
		}
		if let LineSlot::Held(line) = &*slot.lock() {
			// SAFETY: as above - and asked for again if a level line is still asserted.
			unsafe { super::aplic::unmask_source(base, line.source, line.level) };
		}
	}
}

pub fn mask_msi_entries() {
	for slot in 0..MAX_MSI {
		let table = SLOT_TABLE[slot].load(core::sync::atomic::Ordering::Acquire);
		if table == 0 || !REGISTRY.is_bound(slot) || crate::sleep::is_wake(EID_BASE + slot as u32) {
			continue;
		}
		let entry = super::paging::phys_to_virt(table) as *mut u32;
		// SAFETY: the entry `program_msix_entry` wrote, through the same mapping.
		unsafe { entry.add(3).write_volatile(1) };
		SLEEP_MASKED_MSI[slot].store(true, core::sync::atomic::Ordering::Release);
	}
}

// EVERY ENTRY THE KERNEL PROGRAMMED, ACROSS A HIBERNATION: its address, data and vector control saved as the snapshot
// found them - the sleep's masks in them - and written back once a restore has given the functions their addresses again,
// the control last; `unmask_msi_entries` then takes the sleep's masks off.
static SAVED_MSI: [[core::sync::atomic::AtomicU32; 4]; MAX_MSI] = [const { [const { core::sync::atomic::AtomicU32::new(0) }; 4] }; MAX_MSI];

pub fn save_msi_entries() {
	for slot in 0..MAX_MSI {
		let table = SLOT_TABLE[slot].load(core::sync::atomic::Ordering::Acquire);
		if table == 0 {
			continue;
		}
		let entry = super::paging::phys_to_virt(table) as *const u32;
		for (word, saved) in SAVED_MSI[slot].iter().enumerate() {
			// SAFETY: the entry `program_msix_entry` wrote, through the same mapping.
			saved.store(unsafe { entry.add(word).read_volatile() }, core::sync::atomic::Ordering::Relaxed);
		}
	}
}

pub fn restore_msi_entries() {
	for slot in 0..MAX_MSI {
		let table = SLOT_TABLE[slot].load(core::sync::atomic::Ordering::Acquire);
		if table == 0 {
			continue;
		}
		let entry = super::paging::phys_to_virt(table) as *mut u32;
		let saved = |word: usize| SAVED_MSI[slot][word].load(core::sync::atomic::Ordering::Relaxed);
		// SAFETY: as `save_msi_entries`: masked while the message goes back, then the control as saved.
		unsafe {
			entry.add(3).write_volatile(1);
			entry.add(0).write_volatile(saved(0));
			entry.add(1).write_volatile(saved(1));
			entry.add(2).write_volatile(saved(2));
			entry.add(3).write_volatile(saved(3));
		}
	}
}

pub fn unmask_msi_entries() {
	for slot in 0..MAX_MSI {
		let table = SLOT_TABLE[slot].load(core::sync::atomic::Ordering::Acquire);
		if !SLEEP_MASKED_MSI[slot].swap(false, core::sync::atomic::Ordering::AcqRel) || table == 0 {
			continue;
		}
		let entry = super::paging::phys_to_virt(table) as *mut u32;
		// SAFETY: as above.
		unsafe { entry.add(3).write_volatile(0) };
	}
}
