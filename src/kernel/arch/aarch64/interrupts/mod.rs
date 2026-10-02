// aarch64 device-interrupt binding + MSI-X delivery, through whichever MSI controller the machine
// has: a GICv2m frame or a GICv3 ITS.
//
// The GIC has no per-vector "IDT" like x86: a device interrupt arrives at the core
// as a GIC INTID read from GICC_IAR in gic::handle_irq. MSI-X on a GICv2 is done with
// a GICv2m frame - a device signals by writing an SPI number to the frame's
// MSI_SETSPI_NS register (a DMA memory write), and the GIC then pends that SPI. So a
// device's MSI "vector" IS its GIC SPI INTID: acquire_msi hands out a free SPI,
// programs the device's MSI-X table entry to write it to the frame, enables the SPI
// in the distributor (edge-triggered, routed to the boot core), and gic::handle_irq
// wakes the bound Interrupt when that INTID fires.
//
// A GICv3 has no v2m frame. Its MSI controller is the ITS (`its.rs`), where a device writes an
// EVENT ID to one translation register and the controller decides - from tables this kernel owns -
// which LPI to raise and where. So a vector here is an SPI INTID on one machine and an LPI INTID on
// the other, and the slot bookkeeping below is the same either way.
//
// This mirrors x86 interrupts.rs, minus the legacy-INTx window: every aarch64 PCI driver
// that needs an interrupt (virtio-net/input/snd, xhci, virtio-gpu) uses MSI-X, and
// the polled drivers (virtio-blk/console) need none. A wired line reaches a driver only
// as a platform row's claimed line (`bind_wired`). The MSI-X table lives in a device BAR
// reachable through the higher-half physical direct map (phys_to_virt), so - unlike x86 -
// no separate uncacheable mapping is set up here.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use alloc::sync::Arc;

use crate::arch::common::msi::MsiRegistry;
use crate::object::interrupt::Interrupt;

// The device-IRQ vector window base (mirrors the contract; only the MSI window is
// live on aarch64).
// The GICv2m frame on QEMU's `virt` machine (gic-version=2), fixed just above the GIC
// CPU interface at 0x0801_0000. Its MSI_TYPER reports the SPI range the frame owns; a
// device writes an SPI number to MSI_SETSPI_NS to raise it.
// WHERE THE FRAME IS, FROM THE MACHINE DESCRIPTION. This was a constant naming QEMU's `virt`
// machine; the tree describes the frame as a child of the interrupt controller and the prologue
// passes what it read. Zero means the machine has no frame this kernel can drive - which is a
// machine without message-signalled interrupts, not an error, and `init` says so.
static FRAME_BASE: AtomicU64 = AtomicU64::new(0);
const MSI_TYPER: u64 = 0x008; // [25:16] base SPI, [9:0] number of SPIs
const MSI_SETSPI_NS: u64 = 0x040; // a device writes its SPI number here to signal

// The MSI SPI range the GICv2m frame owns, read from MSI_TYPER at init: slot index
// 0..MSI_LEN maps to SPI INTID BASE_SPI + slot, and the SPI is the vector handed out.
static BASE_SPI: AtomicU32 = AtomicU32::new(0);
// WHICH MSI CONTROLLER THIS MACHINE HAS, and they are not variations of one thing. A v2m frame
// turns a device's write of an SPI NUMBER into that SPI; an ITS turns a device's write of an EVENT
// id into an LPI it was mapped to. So the vector a slot names is an SPI on one and an LPI on the
// other, and every path that converts between the two has to ask which.
static USING_ITS: AtomicBool = AtomicBool::new(false);
// The ITS DeviceID each live slot was mapped under, so a teardown can name the same device the
// setup did. `u32::MAX` for a slot that holds none.
static SLOT_DEVID: [AtomicU32; MAX_MSI] = [const { AtomicU32::new(u32::MAX) }; MAX_MSI];
// THE MSI-X ENTRY EACH SLOT PROGRAMMED, by its physical address - 0 for none: what a sleep masks the slot's messages at.
static SLOT_TABLE: [AtomicU64; MAX_MSI] = [const { AtomicU64::new(0) }; MAX_MSI];
// Each mapped device's interrupt translation table, kept for the life of the boot: an ITT belongs
// to the DeviceID it was mapped with, and a device that acquires a second vector must not be given
// a second table under the same id.
static ITT_DEVID: [AtomicU32; MAX_MSI] = [const { AtomicU32::new(u32::MAX) }; MAX_MSI];
static ITT_FRAME: [AtomicU64; MAX_MSI] = [const { AtomicU64::new(0) }; MAX_MSI];
// The host bridge's `msi-map`: how a PCI RequesterID becomes an ITS DeviceID.
static MSI_MAP: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];
// Set when an ITS command was not consumed. The queue is the only way to map or unmap anything, so
// a stall is not one lost vector - it is the end of this controller's usefulness, and continuing to
// hand out vectors it cannot map or release is what would put a live identity on a second device.
static ITS_STALLED: AtomicBool = AtomicBool::new(false);
static MSI_LEN: AtomicUsize = AtomicUsize::new(0);

// Upper bound on the GICv2m SPIs tracked (QEMU virt exposes 64). Fixed-size tables
// keep the bindings off the heap and safe to touch from the interrupt path.
const MAX_MSI: usize = 64;

// The per-device MSI-X slot bindings (reserve / bind / dispatch / free bookkeeping,
// shared with x86 via arch::common::msi). Slot index i maps to SPI INTID
// BASE_SPI + i; only the first MSI_LEN slots (the frame's real SPI range) are used.
static REGISTRY: MsiRegistry<MAX_MSI> = MsiRegistry::new();

// The ITS DeviceID for a discovered device, through the host bridge's mapping.
//
// A DEVICE CANNOT BE NAMED TO THE CONTROLLER WITHOUT IT. The RequesterID is what the hardware puts
// on the bus; `msi-map` says which DeviceIDs that bridge's RIDs become, and a machine that states no
// mapping is one where this kernel has no name to give - so it refuses rather than sending the RID
// and hoping the identity holds.
fn its_devid(owner: u32) -> Option<u32> {
	let (bus, dev, func) = crate::device::with(owner as usize, |d| (d.bus, d.dev, d.func))?;
	let rid = (u32::from(bus) << 8) | (u32::from(dev) << 3) | u32::from(func);
	let rid_base = MSI_MAP[0].load(Ordering::Relaxed);
	let devid_base = MSI_MAP[1].load(Ordering::Relaxed);
	let length = MSI_MAP[2].load(Ordering::Relaxed);
	if length == 0 || rid < rid_base || rid - rid_base >= length {
		crate::serial_println!("interrupts: {bus:02x}:{dev:02x}.{func} has requester id {rid:#x}, which this machine's msi-map does not cover");
		return None;
	}
	Some(devid_base + (rid - rid_base))
}

// The interrupt translation table for `devid`, allocated on the device's first vector and kept for
// the life of the boot: an ITT belongs to the DeviceID it was mapped with, and a device that
// acquires a second vector must not be given a second table under the same id.
fn device_itt(devid: u32) -> Option<u64> {
	for (slot, held) in ITT_DEVID.iter().enumerate() {
		if held.load(Ordering::Acquire) == devid {
			return Some(ITT_FRAME[slot].load(Ordering::Acquire));
		}
	}
	for (slot, held) in ITT_DEVID.iter().enumerate() {
		if held.compare_exchange(u32::MAX, devid, Ordering::AcqRel, Ordering::Acquire).is_ok() {
			// ALLOC-OK: one page per device that ever takes an MSI, bounded by the vector count.
			let Some(frame) = crate::mem::frame::allocate() else {
				held.store(u32::MAX, Ordering::Release);
				return None;
			};
			unsafe { core::ptr::write_bytes(super::paging::phys_to_virt(frame) as *mut u8, 0, 4096) };
			// MAPPED FIRST, PUBLISHED SECOND.
			//
			// This stored the frame and THEN issued MAPD, and left both published when MAPD failed -
			// so a later acquisition for the same DeviceID took that frame straight out of the cache
			// at the top of this function and went on to MAPTI as though MAPD had been confirmed. An
			// unconfirmed controller operation reused as a confirmed one is precisely what M3 forbids,
			// and it applies to a bounded command failure and to an explicit MAPD refusal alike.
			//
			// A FAILED MAPD GIVES EVERYTHING BACK: the slot is released so the id can be tried again,
			// and the frame goes back to the allocator, because nothing was ever told about it.
			if !super::its::map_device(devid, MAX_MSI as u32, frame) {
				held.store(u32::MAX, Ordering::Release);
				// SAFETY: allocated by this call, never mapped into any address space, and the ITS
				// was not told about it - the MAPD is what failed.
				// NEVER-MAPPED: allocated three lines above, never entered a page table, and the
				// controller was never told about it - the MAPD that would have told it is the call
				// that failed. So no core and no device can translate this frame, which is what the
				// plain door requires and what `retire` exists for when it cannot be said.
				unsafe { crate::mem::frame::deallocate(frame) };
				return None;
			}
			ITT_FRAME[slot].store(frame, Ordering::Release);
			return Some(frame);
		}
	}
	None
}

// The slot index of a vector INTID, or None if it is outside this controller's MSI window.
fn spi_slot(intid: u32) -> Option<usize> {
	let base = if USING_ITS.load(Ordering::Relaxed) { super::its::LPI_BASE } else { BASE_SPI.load(Ordering::Relaxed) };
	let len = MSI_LEN.load(Ordering::Relaxed);
	if intid >= base && ((intid - base) as usize) < len { Some((intid - base) as usize) } else { None }
}

// WHETHER AN ACKNOWLEDGED INTID IS AN LPI THIS CONTROLLER'S ITS TRANSLATED.
//
// Asked by the exception path in `gic.rs` and by nothing else, because that is the only caller whose
// INTID came out of the hardware. An LPI exists only where an ITS does: an ITS translates a device's
// write to the translater register into an LPI and delivers it, so an intid at or above `LPI_BASE`
// on a machine using the ITS is a message a DEVICE sent - which is the one thing this kernel's own
// MSI oracles cannot produce, since they call `dispatch_msi` directly with a number they chose.
pub fn is_device_lpi(intid: u32) -> bool {
	USING_ITS.load(Ordering::Relaxed) && intid >= super::its::LPI_BASE && spi_slot(intid).is_some()
}

// The vector INTID a slot names, which is an SPI on a v2m machine and an LPI on an ITS one.
fn slot_vector(slot: usize) -> u32 {
	let base = if USING_ITS.load(Ordering::Relaxed) { super::its::LPI_BASE } else { BASE_SPI.load(Ordering::Relaxed) };
	base + slot as u32
}

#[cfg(test)]
fn is_msi(vector: u32) -> bool {
	spi_slot(vector).is_some()
}

// THE KERNEL'S OWN WIRED LINES, WHICH ARE NOT DRIVER BINDINGS.
//
// Everything else in this file hands an interrupt to a USERSPACE driver: a slot is reserved for a
// process, an `Interrupt` object is signalled, and the whole lifecycle is about what happens when
// that process dies. These are the lines this binary answers itself, and today there is exactly
// one - a PCI hot-plug port's INTx, which the board's `interrupt-map` routes to an SPI. It has to
// be answered INSIDE the interrupt, because the line is level-sensitive and the source has to be
// cleared before the EOI; there is no process to wake and nothing to wait for.
//
// EIGHT ROWS, WHICH IS MORE HOT-PLUG PORTS THAN A MACHINE HAS. See `arch::common::wired`, which
// this shares with riscv64 for the reason both ports needed it at once: neither controller hands
// over a vector it chose, so neither can index a table by one.
static WIRED: crate::arch::common::wired::Wired<8> = crate::arch::common::wired::Wired::new();

/// `not(test)` ON THE TYPE AND THE REGISTRATION, AND NOT ON THE DISPATCH, which is the honest
/// split: the interrupt path calls `dispatch_wired` on every event and is compiled in every build,
/// while the only thing that REGISTERS a handler is the boot's arming pass.
///
/// What answers one of this kernel's own wired lines: told the INTID it was raised for, because one
/// function answers for every port on a shared line.
#[cfg(not(test))]
pub type HandlerFn = crate::arch::common::wired::Handler;

/// Answer `intid` with `handler` from now on. `false` when this kernel already answers as many
/// wired lines as it carries rows for, which the caller reports rather than swallows.
#[cfg(not(test))]
pub fn register(intid: u32, handler: HandlerFn) -> bool {
	WIRED.register(intid, handler)
}

/// Run this kernel's own handler for `intid`, and say whether there was one. `false` sends the
/// interrupt on to the MSI registry, which is where every other one belongs.
pub fn dispatch_wired(intid: u32) -> bool {
	WIRED.dispatch(intid)
}

// A CLAIMED WIRED LINE: a shared peripheral interrupt by INTID, whether it is level-triggered, and the
// `Interrupt` its driver waits on - held weakly, so the driver letting go (its Interrupt's `Drop`) is what
// unbinds it. A slot is free only when it is empty - see the x86_64 `Line`.
struct Line {
	intid: u32,
	level: bool,
	intr: alloc::sync::Weak<Interrupt>,
}

const MAX_LINES: usize = 32;
static LINES: [crate::sync::SpinLock<Option<Line>>; MAX_LINES] = [const { crate::sync::SpinLock::new(None) }; MAX_LINES];

// Binds one at a time, so two cannot choose the same free slot.
static BINDING: crate::sync::SpinLock<()> = crate::sync::SpinLock::new(());

fn line_slot(intid: u32) -> Option<usize> {
	LINES.iter().position(|slot| slot.lock().as_ref().is_some_and(|line| line.intid == intid))
}

// BIND A PLATFORM ROW'S WIRED LINE to a new `Interrupt`: its SPI configured with the row's trigger, routed to
// the boot core and enabled. Refused, in words: a line of another controller, an INTID that is not a shared
// peripheral interrupt, one in the MSI frame's window, one the kernel answers itself, one another claim holds,
// or a full table.
pub fn bind_wired(line: &abi::WiredLine) -> Result<Arc<Interrupt>, &'static str> {
	if line.controller != abi::LINE_CONTROLLER_GIC {
		return Err("its line is not a GIC's");
	}
	let intid = line.number;
	if !(32..1020).contains(&intid) {
		return Err("its INTID is not a shared peripheral interrupt");
	}
	if spi_slot(intid).is_some() {
		return Err("that SPI belongs to the MSI frame");
	}
	if WIRED.holds(intid) {
		return Err("the kernel answers that line itself");
	}
	let _binding = BINDING.lock();
	if line_slot(intid).is_some() {
		return Err("another claim holds that line");
	}
	let Some(free) = LINES.iter().position(|slot| slot.lock().is_none()) else {
		return Err("this kernel's table of claimed lines is full");
	};
	let Some(intr) = Interrupt::new(intid) else { return Err("the Interrupt object could not be allocated") };
	let level = line.trigger == abi::LINE_TRIGGER_LEVEL;
	*LINES[free].lock() = Some(Line { intid, level, intr: Arc::downgrade(&intr) });
	intr.mark_bound();
	super::gic::enable_spi(intid, if level { super::gic::Trigger::Level } else { super::gic::Trigger::Edge });
	Ok(intr)
}

// Whether a claimed line is live at its controller - for the suite, as on x86_64.
#[cfg(test)]
pub fn line_armed(line: &abi::WiredLine) -> bool {
	super::gic::spi_enabled(line.number)
}

// A CLAIMED LINE FIRED: its driver is woken, and a LEVEL line is disabled at the distributor until the driver
// acknowledges - its source stays asserted until the driver has run. False when no claim holds `intid`.
pub fn signal_line(intid: u32) -> bool {
	let Some(at) = line_slot(intid) else { return false };
	let held = LINES[at].lock();
	let Some(line) = held.as_ref() else { return false };
	if line.level {
		super::gic::disable_spi(intid);
	}
	if let Some(intr) = line.intr.upgrade() {
		intr.signal();
	}
	true
}

// THE DRIVER ACKNOWLEDGED: a level line disabled when it fired is enabled again.
pub fn acknowledge(vector: u32) {
	if let Some(at) = line_slot(vector)
		&& let Some(line) = LINES[at].lock().as_ref()
		&& line.level
	{
		super::gic::unmask_spi(line.intid);
	}
}

// Remove any binding for `vector` (called from an Interrupt's Drop, or its claim's revocation).
//
// The SPI is retired rather than freed, for the reason the x86 backend spells out: the device's
// MSI-X entry was programmed to write the GICv2m frame, and nothing here can prove a write already
// on its way will not land. The SPI waits for `SYS_DEVICE_QUIESCED`.
// Returns whether the teardown CONFIRMED - always, on this port: releasing the ITS translation and
// retiring the slot are local operations with no remote agreement to wait for. The signature is
// shared with riscv64, where the answer can be false, so a caller can fold it into a claim's
// terminal state without asking which architecture it is on.
pub fn unbind(vector: u32) -> bool {
	// A CLAIMED WIRED LINE: disabled at the distributor, and only then its slot given back.
	if let Some(at) = line_slot(vector) {
		let mut held = LINES[at].lock();
		if let Some(line) = held.as_ref() {
			super::gic::disable_spi(line.intid);
		}
		*held = None;
		return true;
	}
	if let Some(slot) = spi_slot(vector) {
		release_translation(slot, vector);
		SLOT_TABLE[slot].store(0, Ordering::Release);
		REGISTRY.retire(slot);
	}
	true
}

// Allocate a free MSI SPI and program a device's MSI-X table entry 0 so the device
// delivers it: message address = the GICv2m frame's MSI_SETSPI_NS register, message
// data = the SPI number. `table_phys` is the physical address of the device's MSI-X
// table (reached through the higher-half direct map). Returns the SPI as the vector
// (None if every slot is taken); the caller enables MSI-X on the device and binds an
// Interrupt to the returned vector with bind_msi. `owner` is the discovered-device
// index, retained for the `lsirq` inventory. `dest` (the x86 LAPIC target) is unused:
// GICv2m MSIs route through the distributor, which enable_msi_spi points at the boot
// core.
// WHERE A DEVICE WRITES TO RAISE AN INTERRUPT ON THIS MACHINE.
//
// A translated endpoint's MSI is a memory write and needs a mapping like any other. Which register
// it is depends on which controller this machine has - a v2m frame's `MSI_SETSPI_NS`, or an ITS's
// `GITS_TRANSLATER` - and a machine with neither raises no MSI to translate. Answered as the PAGE
// holding the register, because a mapping is made of pages.
pub fn msi_doorbell() -> Option<(u64, u64)> {
	const PAGE: u64 = 0x1000;
	let at = if USING_ITS.load(Ordering::Acquire) {
		super::its::translater()
	} else {
		let frame = FRAME_BASE.load(Ordering::Acquire);
		if frame == 0 {
			return None;
		}
		frame + MSI_SETSPI_NS
	};
	if at == 0 {
		return None;
	}
	Some((at & !(PAGE - 1), PAGE))
}

// Give back a vector whose Interrupt never reached its owner - see the x86_64 `release_unused_msi`
// for why this is a free rather than a retire.
pub fn release_unused_msi(vector: u32) {
	if let Some(slot) = spi_slot(vector) {
		release_translation(slot, vector);
		SLOT_TABLE[slot].store(0, Ordering::Release);
		REGISTRY.free(slot);
	}
}

// ------------------------------------------------------------------ a sleep's device lines

// A SUSPEND TO IDLE MASKS EVERY CLAIMED LINE at the distributor and every MSI-X entry it programmed at the device - each
// driver has quiesced its device - but the wake set's, which its driver marked and whose firing is what ends the sleep;
// and puts back exactly what it masked.
static SLEEP_MASKED_LINES: [AtomicBool; MAX_LINES] = [const { AtomicBool::new(false) }; MAX_LINES];
static SLEEP_MASKED_MSI: [AtomicBool; MAX_MSI] = [const { AtomicBool::new(false) }; MAX_MSI];

pub fn mask_claimed_lines() {
	for (at, slot) in LINES.iter().enumerate() {
		let intid = slot.lock().as_ref().map(|line| line.intid);
		if let Some(intid) = intid
			&& !crate::sleep::is_wake(intid)
			&& super::gic::spi_enabled(intid)
		{
			super::gic::disable_spi(intid);
			SLEEP_MASKED_LINES[at].store(true, Ordering::Release);
		}
	}
}

pub fn unmask_claimed_lines() {
	for (at, slot) in LINES.iter().enumerate() {
		if SLEEP_MASKED_LINES[at].swap(false, Ordering::AcqRel)
			&& let Some(intid) = slot.lock().as_ref().map(|line| line.intid)
		{
			super::gic::unmask_spi(intid);
		}
	}
}

// Whether MSIs go through an ITS - whose tables and mappings a hibernation's restore does not put back.
pub fn using_its() -> bool {
	USING_ITS.load(Ordering::Relaxed)
}

pub fn mask_msi_entries() {
	let base = if USING_ITS.load(Ordering::Relaxed) { super::its::LPI_BASE } else { BASE_SPI.load(Ordering::Relaxed) };
	for slot in 0..MAX_MSI {
		let table = SLOT_TABLE[slot].load(Ordering::Acquire);
		if table == 0 || !REGISTRY.is_bound(slot) || crate::sleep::is_wake(base + slot as u32) {
			continue;
		}
		let entry = super::paging::phys_to_virt(table) as *mut u32;
		// SAFETY: the entry `program_msix_entry_at` wrote, through the same mapping.
		unsafe { entry.add(3).write_volatile(1) };
		SLEEP_MASKED_MSI[slot].store(true, Ordering::Release);
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
			// SAFETY: the entry `program_msix_entry_at` wrote, through the same mapping.
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
		let table = SLOT_TABLE[slot].load(Ordering::Acquire);
		if !SLEEP_MASKED_MSI[slot].swap(false, Ordering::AcqRel) || table == 0 {
			continue;
		}
		let entry = super::paging::phys_to_virt(table) as *mut u32;
		// SAFETY: as above.
		unsafe { entry.add(3).write_volatile(0) };
	}
}

// Take an ITS translation back: the LPI is disabled in the configuration table, the mapping is
// discarded, and the DeviceID this slot was mapped under is forgotten.
//
// A v2m vector has nothing to release here - its SPI stays enabled in the distributor and the
// registry's own pending rule is what keeps a message already in flight from waking somebody else.
// An ITS mapping is different: it is state in a table, and leaving it in place means the device can
// still raise the identity after the slot is gone.
fn release_translation(slot: usize, vector: u32) {
	if !USING_ITS.load(Ordering::Relaxed) {
		return;
	}
	let devid = SLOT_DEVID[slot].swap(u32::MAX, Ordering::AcqRel);
	if devid == u32::MAX {
		return;
	}
	if !super::its::discard_event(devid, slot as u32, vector) {
		// The queue did not consume it, so the mapping may still stand. Nothing further can be
		// mapped either - every acquire needs the same queue - so the window closes rather than
		// handing out vectors this kernel can no longer take back.
		ITS_STALLED.store(true, Ordering::Release);
		crate::serial_println!("interrupts: the ITS did not take back device {devid}'s event {slot} - no further MSI vectors are handed out");
	}
}

// One vector per device, entry 0. `MsiRegistry::acquire` does not refuse a device that already
// holds a live slot - `acquire_unique_live` below is the form that does, and it is what the syscall
// path uses. This one stays for the kernel's own bring-up test, which is its only caller.
#[cfg(test)]
pub fn acquire_msi(table_phys: u64, _dest: u8, owner: u32) -> Option<u32> {
	let len = MSI_LEN.load(Ordering::Relaxed);
	program_acquired(REGISTRY.acquire(owner, len)?, table_phys, owner)
}

// The same, and ONLY IF the device holds no live vector already - see
// `MsiRegistry::acquire_unique_live`. This is what `sys_device_msix_acquire` calls; the form above
// stays for the kernel's own bring-up test.
pub fn acquire_msi_unique(table_phys: u64, _dest: u8, owner: u32) -> Option<u32> {
	if ITS_STALLED.load(Ordering::Acquire) {
		return None;
	}
	let len = MSI_LEN.load(Ordering::Relaxed);
	program_acquired(REGISTRY.acquire_unique_live(owner, len)?, table_phys, owner)
}

fn program_acquired(slot: usize, table_phys: u64, owner: u32) -> Option<u32> {
	if USING_ITS.load(Ordering::Relaxed) {
		// THE DEVICE IS NAMED TO THE CONTROLLER BEFORE THE DEVICE IS TOLD ANYTHING. A mapping that
		// does not exist yet turns an early message into an ITS error rather than an interrupt, so
		// the order is: give the device an ITT, map its event to this LPI, and only then write the
		// MSI-X entry that lets it write at all.
		let lpi = super::its::LPI_BASE + slot as u32;
		// A REFUSAL GIVES THE SLOT BACK. These two exits used `?` on a slot `REGISTRY.acquire*` had
		// already marked USED, so a requester outside `msi-map`, an exhausted ITT table, a failed
		// allocation or a refused MAPD each consumed one of the sixty-four MSI slots and returned
		// `None`. Repeated SAFE refusals - the ones a hostile or misconfigured device produces on
		// purpose - exhausted the controller. `map_event` below already frees on failure; these did
		// not, and the difference was invisible because both answer the caller the same way.
		let Some(devid) = its_devid(owner) else {
			REGISTRY.free(slot);
			return None;
		};
		let Some(itt) = device_itt(devid) else {
			REGISTRY.free(slot);
			return None;
		};
		let _ = itt;
		if !super::its::map_event(devid, slot as u32, lpi) {
			ITS_STALLED.store(true, Ordering::Release);
			REGISTRY.free(slot);
			return None;
		}
		SLOT_DEVID[slot].store(devid, Ordering::Release);
		// The message address is the ONE translation register, and the data is the event id - the
		// device says which of ITS events happened and the controller decides what that means.
		program_msix_entry_at(table_phys, super::its::translater(), slot as u32);
		SLOT_TABLE[slot].store(table_phys, Ordering::Release);
		return Some(lpi);
	}
	let spi = BASE_SPI.load(Ordering::Relaxed) + slot as u32;
	program_msix_entry(table_phys, spi);
	SLOT_TABLE[slot].store(table_phys, Ordering::Release);
	super::gic::enable_msi_spi(spi);
	// THE WHOLE SPI (KERN-ARCH-017). GICv2m's base and count are ten-bit fields, so a frame
	// starting at SPI 256 or above returned an identifier that wrapped: the hardware stayed armed
	// under the real SPI while the registry, the bind, the teardown and `lsirq` all named another.
	Some(spi)
}

// Write a device's MSI-X table entry 0 (reached through the physical direct map): the
// message address is the GICv2m frame's MSI_SETSPI_NS register, so the device's DMA
// write of the message data (the SPI number) raises that SPI in the GIC. Vector
// control = 1 (MASKED until `unmask_msi`). A driver must never write its own MSI-X
// table; only the kernel programs it here.
//
// PROGRAMMED MASKED, AND UNMASKED ONLY WHEN THE ACQUIRE HAS COMMITTED (2026-09-01) - see the
// x86_64 port's comment at the same function for the stale-generation window this closes.
fn program_msix_entry(table_phys: u64, spi: u32) {
	program_msix_entry_at(table_phys, FRAME_BASE.load(Ordering::Relaxed) + MSI_SETSPI_NS, spi);
}

// The same write, with the address and data the caller's controller wants.
fn program_msix_entry_at(table_phys: u64, msg_addr: u64, data: u32) {
	let entry = super::paging::phys_to_virt(table_phys) as *mut u32;
	unsafe {
		entry.add(0).write_volatile(msg_addr as u32); // message address low
		entry.add(1).write_volatile((msg_addr >> 32) as u32); // message address high
		// The message data: an SPI number to a v2m frame, an event id to an ITS.
		entry.add(2).write_volatile(data);
		entry.add(3).write_volatile(1); // vector control (MASKED until `unmask_msi`)
	}
}

// Make the entry programmed above deliverable - the other half of its mask. This port reaches the
// table through the physical direct map, so the caller supplies the address it programmed.
pub fn unmask_msi(_vector: u32, table_phys: u64) {
	let entry = super::paging::phys_to_virt(table_phys) as *mut u32;
	// SAFETY: the same entry `program_msix_entry_at` wrote, through the same mapping.
	unsafe { entry.add(3).write_volatile(0) };
}

// Bind `intr` to an MSI `vector` (an SPI INTID) so dispatch wakes it when the SPI
// fires. Returns false if the vector is already bound to a live Interrupt.
pub fn bind_msi(vector: u32, intr: &Arc<Interrupt>) -> bool {
	match spi_slot(vector) {
		Some(slot) => REGISTRY.bind(slot, intr),
		None => false,
	}
}

// Whether `vector` currently has a live driver binding. Used to confirm a crashed
// driver's IRQ was detached during cleanup.
pub fn is_bound(vector: u32) -> bool {
	if let Some(at) = line_slot(vector) {
		return LINES[at].lock().as_ref().is_some_and(|line| line.intr.strong_count() != 0);
	}
	match spi_slot(vector) {
		Some(slot) => REGISTRY.is_bound(slot),
		None => false,
	}
}

// End-of-interrupt for a serviced vector. MSI on aarch64 is edge-triggered and
// unshared, so there is no level source to complete: a no-op, kept for the portable
// SYS_INTERRUPT_ACK path (the riscv PLIC completes its level source here).
pub fn eoi(_vector: u32) {}

// Deliver a fired GIC INTID to a bound MSI driver, if it is one of the frame's SPIs.
// Returns true when the INTID was an MSI vector (handled here), so gic::handle_irq can
// tell it apart from the timer and other INTIDs. Edge-triggered: just wake the bound
// driver - there is no level source to mask.
pub fn dispatch_msi(intid: u32) -> bool {
	match spi_slot(intid) {
		Some(slot) => {
			REGISTRY.dispatch(slot);
			true
		}
		None => false,
	}
}

// The state of the MSI vector at `index` (its slot), for the `lsirq` inventory. Index
// 0 is the kernel's own timer - the EL1 physical-timer PPI (INTID 30 on QEMU virt),
// always in use - so the inventory shows a fixed kernel vector like x86's; the MSI
// window (each a device's per-device SPI) follows.
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
	// FROM THE MACHINE, like the controller's addresses. This mirrored a compiled constant, so the
	// inventory reported an interrupt the tree may never have named.
	let timer_intid: u32 = super::gic::timer_intid_for_report();
	if index == 0 {
		return Some(abi::IrqInfo { vector: timer_intid, kind: abi::IRQ_KIND_FIXED, bound: 1, device: abi::IRQ_NO_DEVICE });
	}
	let slot = index - 1;
	let len = MSI_LEN.load(Ordering::Relaxed);
	if slot >= len {
		return None;
	}
	let vector = slot_vector(slot);
	Some(abi::IrqInfo { vector, kind: abi::IRQ_KIND_MSI, bound: is_bound(vector) as u32, device: REGISTRY.owner(slot) })
}

// The number of vectors irq_info reports over (the timer entry plus the frame's MSI SPIs).
pub fn irq_info_len() -> usize {
	1 + MSI_LEN.load(Ordering::Relaxed)
}

// Read the GICv2m frame's MSI SPI range (base SPI + count) so acquire_msi/dispatch can
// map slots to SPI INTIDs. Called once, after the GIC is up.
// Read the frame's SPI range, or record that this machine has none.
//
// A GIC WITHOUT AN MSI CONTROLLER IS STILL A GIC. The timer PPI arrives through the distributor
// either way, so the machine boots and schedules; what it loses is device interrupts, and saying
// that once here is better than a driver discovering it as a vector that never fires.
pub fn init(frame: u64, its: u64, its_size: u64, msi_map: (u32, u32, u32)) -> bool {
	MSI_MAP[0].store(msi_map.0, Ordering::Relaxed);
	MSI_MAP[1].store(msi_map.1, Ordering::Relaxed);
	MSI_MAP[2].store(msi_map.2, Ordering::Relaxed);
	if frame != 0 {
		FRAME_BASE.store(frame, Ordering::Relaxed);
		let typer = unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(frame + MSI_TYPER) as *const u32) };
		let base = (typer >> 16) & 0x3ff;
		let count = (typer & 0x3ff) as usize;
		BASE_SPI.store(base, Ordering::Relaxed);
		MSI_LEN.store(count.min(MAX_MSI), Ordering::Relaxed);
		return true;
	}
	// No frame: a GICv3 machine's MSI controller is the ITS, and it needs a redistributor to aim
	// its one collection at - this core's, which is the same core the v2m path routes SPIs to.
	if its != 0 {
		let Some(redistributor) = super::gic::redistributor() else {
			crate::serial_println!("interrupts: an ITS with no redistributor to aim it at - no MSI");
			MSI_LEN.store(0, Ordering::Relaxed);
			return false;
		};
		if msi_map.2 == 0 {
			crate::serial_println!("interrupts: the machine describes an ITS but no msi-map, so a device cannot be named to it - no MSI");
			MSI_LEN.store(0, Ordering::Relaxed);
			return false;
		}
		if super::its::init(its, its_size, redistributor) {
			USING_ITS.store(true, Ordering::Relaxed);
			MSI_LEN.store(MAX_MSI, Ordering::Relaxed);
			return true;
		}
	}
	MSI_LEN.store(0, Ordering::Relaxed);
	false
}

#[cfg(test)]
mod tests;
