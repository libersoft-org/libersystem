// The system device table.
//
// The kernel scans the PCI bus once at boot (it alone can issue the I/O-port
// instructions PCI config space needs) and records each discovered device's MMIO
// layout here - the virtio devices and any xHCI USB host controller. DeviceManager
// queries this table over the device syscalls and is handed a DeviceMemory
// capability per device, so it can map each device to a userspace driver and give
// that driver only its own device's MMIO window. The per-structure offsets travel
// as plain data (`device_info`) since a ring-3 driver cannot read PCI config space
// itself.

use alloc::vec::Vec;

use crate::sync::SpinLock;

// One discovered device, resolved from its PCI configuration.
pub struct DeviceEntry {
	pub device_type: u16,
	// Physical base + length of the MMIO BAR the driver maps.
	pub bar_phys: u64,
	pub bar_len: u64,
	// Byte offsets of the virtio structures within that BAR (zero for a non-virtio
	// device such as the xHCI controller, whose registers start at the BAR base).
	pub common_offset: u32,
	pub notify_offset: u32,
	pub notify_multiplier: u32,
	pub isr_offset: u32,
	// The optional device-specific structure. A length of zero is how "this device has none" is
	// said, because offset zero is also a legal offset for one that does (KERN-ARCH-014).
	pub device_offset: u32,
	pub device_len: u32,
	// MSI-X (when present): the config-space offset of the device's MSI-X capability
	// (0 = none) and the physical address of its MSI-X table. The kernel programs table
	// entry 0 and enables MSI-X so a driver gets its own per-device edge-triggered
	// vector instead of the shared INTx line above.
	pub msix_cap: u16,
	pub msix_table_phys: u64,
	// The device's PCI address, so the interrupt-acquire path can re-enable its INTx pin
	// (init disables every device's pin by default; see below).
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	// The standards identity, carried from the same scan that resolved the BAR. It was resolved,
	// retained for `lspci`, and not passed to the one consumer that binds drivers by it.
	pub class: u8,
	pub subclass: u8,
	pub prog_if: u8,
	// WHICH TRANSPORT THIS FUNCTION SPEAKS, and the PCI identity of the part - both resolved by the
	// same scan that resolved the BAR, both retained for `lspci` alone until a rule could ask for
	// them. `device_type` is a virtio type only when `transport` says virtio-pci; for anything else
	// it is a LiberSystem number standing in for a class triple, which is not an identity a rule may
	// be written against.
	pub transport: u8,
	pub vendor: u16,
	pub product: u16,
	// WHETHER THIS ENTRY DESCRIBES A FUNCTION THAT IS ACTUALLY ON THE BUS.
	//
	// True for everything the boot scan resolved, which is everything on a real machine. False for
	// the synthetic entries the claim tests append: those exist so a test can name a device nothing
	// else is driving, and their bus/dev/func name a function this machine does not have - on the
	// ECAM ports that address is outside the mapped window, so a config-space write to it is a fault
	// rather than a write nobody reads.
	//
	// Read by `bus_master`, which is the only thing here that touches config space.
	pub on_bus: bool,
}

static DEVICES: SpinLock<Vec<DeviceEntry>> = SpinLock::new(Vec::new());

// The full boot PCI scan - every present function, not just the virtio / xHCI ones
// drivers bind - retained so the bus stays inspectable at runtime. SYS_PCI_INFO
// reads it for `lspci`.
static PCI_FUNCTIONS: SpinLock<Vec<abi::PciInfo>> = SpinLock::new(Vec::new());

// Populate the table from a PCI scan. Called once at boot, after the heap is up.
pub fn init() {
	let mut functions = PCI_FUNCTIONS.lock();
	functions.clear();
	for p in crate::arch::pci::scan() {
		// NOBODY IS DRIVING ANYTHING YET, so nothing on this bus may write to memory.
		//
		// `assign_bars_ecam` clears the bit as it places the BARs, but only two of the three ports
		// place their own: on x86 the firmware placed them AND enabled bus mastering, and that path
		// never runs, so the bit arrived set and stayed set. This is the sweep that covers every
		// port, over every function the scan found - including the ones no driver will ever bind,
		// which are exactly the devices nobody would notice mastering the bus.
		//
		// BRIDGES ARE LEFT ALONE (header type 1): their bus-master bit forwards transactions from
		// everything behind them rather than granting the bridge anything of its own, and clearing
		// it here would silently cut off a device whose own driver had legitimately acquired it.
		if p.header_type & 0x7F == 0 {
			crate::arch::pci::set_bus_master(p.bus, p.dev, p.func, false);
		}
		// ALLOC-OK: the device inventory is built once at boot from what the bus reports.
		functions.push(abi::PciInfo { vendor: p.vendor, device: p.device_id, class: p.class, subclass: p.subclass, prog_if: p.prog_if, bus: p.bus, dev: p.dev, func: p.func, _pad: 0 });
	}
	drop(functions);
	let mut table = DEVICES.lock();
	table.clear();
	for v in crate::arch::pci::scan_virtio() {
		// Silence every device's legacy INTx pin: the kernel takes all device interrupts via
		// per-device MSI-X (input, net, snd) and the remaining drivers poll, so no driver uses
		// a shared INTx line. Disabling the pins keeps a stray assertion off the (fully masked)
		// I/O APIC by construction.
		crate::arch::pci::set_intx_disabled(v.pci.bus, v.pci.dev, v.pci.func, true);
		// ALLOC-OK: the device inventory is built once at boot from what the bus reports.
		table.push(DeviceEntry { device_type: v.virtio_type, transport: abi::TRANSPORT_VIRTIO_PCI, vendor: v.pci.vendor, product: v.pci.device_id, bar_phys: v.bar_phys, bar_len: v.region_len, common_offset: v.common.offset, notify_offset: v.notify.offset, notify_multiplier: v.notify.notify_multiplier, isr_offset: v.isr.offset, device_offset: v.device.map_or(0, |cap| cap.offset), device_len: v.device.map_or(0, |cap| cap.length), msix_cap: v.msix_cap, msix_table_phys: v.msix_table_phys, bus: v.pci.bus, dev: v.pci.dev, func: v.pci.func, class: v.pci.class, subclass: v.pci.subclass, prog_if: v.pci.prog_if, on_bus: true });
	}
	for x in crate::arch::pci::scan_xhci() {
		// The xHCI controller joins the same table: its whole register file lives in
		// BAR 0, so the virtio structure offsets are zero and the driver reads the
		// operational/runtime/doorbell offsets from the capability registers at the base.
		crate::arch::pci::set_intx_disabled(x.pci.bus, x.pci.dev, x.pci.func, true);
		// ALLOC-OK: the device inventory is built once at boot from what the bus reports.
		table.push(DeviceEntry { device_type: abi::DEVICE_TYPE_XHCI as u16, transport: abi::TRANSPORT_PLAIN_PCI, vendor: x.pci.vendor, product: x.pci.device_id, bar_phys: x.bar_phys, bar_len: x.bar_len, common_offset: 0, notify_offset: 0, notify_multiplier: 0, isr_offset: 0, device_offset: 0, device_len: 0, msix_cap: x.msix_cap, msix_table_phys: x.msix_table_phys, bus: x.pci.bus, dev: x.pci.dev, func: x.pci.func, class: x.pci.class, subclass: x.pci.subclass, prog_if: x.pci.prog_if, on_bus: true });
	}
	// AND EVERY OTHER FUNCTION ON THE BUS, so the inventory is the machine rather than the two
	// families this kernel happens to resolve.
	//
	// `PCI_FUNCTIONS` held the full scan and its only reader was `SYS_PCI_INFO`, for `lspci`. The
	// table below - which answers `SYS_DEVICE_COUNT`, supplies identity to the binder, and owns the
	// claim slots - was filled by `scan_virtio()` and `scan_xhci()` alone. So a function outside those
	// two resolvers had no identity row anywhere the registry, a stable node, the binding catalogue or
	// DeviceService look: it was visible through one diagnostic syscall and nowhere else.
	//
	// A ROW WITH NO RESOURCE PROFILE IS THE POINT, not a gap in one. These carry the standards
	// identity the scan resolved - vendor, product, the class triple, the address - and no BAR, no
	// MSI-X and no virtio structure offsets, because this kernel did not resolve any for them. That is
	// what "discoverable and capability-free" means: a rule can match one, nothing can claim resources
	// it has none of, and a device nothing binds is still a device this machine reports having.
	//
	// APPENDED, so the indices the two resolvers produced keep the values they had.
	for p in crate::arch::pci::scan() {
		// Bridges are not endpoints: they forward for what is behind them and there is nothing to
		// bind to one. `header_type & 0x7F == 0` is the same test `init` uses above.
		if p.header_type & 0x7F != 0 {
			continue;
		}
		if table.iter().any(|entry| entry.bus == p.bus && entry.dev == p.dev && entry.func == p.func) {
			continue;
		}
		// ALLOC-OK: the device inventory is built once at boot from what the bus reports.
		table.push(DeviceEntry { device_type: abi::DEVICE_TYPE_UNKNOWN as u16, transport: abi::TRANSPORT_PLAIN_PCI, vendor: p.vendor, product: p.device_id, bar_phys: 0, bar_len: 0, common_offset: 0, notify_offset: 0, notify_multiplier: 0, isr_offset: 0, device_offset: 0, device_len: 0, msix_cap: 0, msix_table_phys: 0, bus: p.bus, dev: p.dev, func: p.func, class: p.class, subclass: p.subclass, prog_if: p.prog_if, on_bus: true });
	}
	// One claim slot per device, all `Free`: nothing is driving anything yet, and enumeration left
	// every device with bus mastering off.
	let len = table.len();
	drop(table);
	reset_claims(len);
}

// The number of discovered devices.
// A DEVICE THAT FAULTED AGAINST ITS OWN TRANSLATION IS TAKEN OFF THE BUS.
//
// M0153's M5 asks for a hardware fault to reach the binding lifecycle's containment, and nothing did:
// `poll_faults` printed a line and the device carried on mastering the bus. This is the narrowest of
// the three answers the milestone permits - refusal, DISABLED BUS MASTERING, or quarantine - and it
// is the one that needs no cooperation from a driver that is by definition doing something wrong.
//
// The claim is NOT released here. Releasing is the manager's decision and the teardown has to be
// confirmed; what this does is stop the device before that conversation, which is the point of
// containment. Answers the index it contained, so the caller can say which device it was.
pub fn contain_faulting_endpoint(bus: u8, dev: u8, func: u8) -> Option<usize> {
	let table = DEVICES.lock();
	let index = table.iter().position(|entry| entry.bus == bus && entry.dev == dev && entry.func == func)?;
	bus_master(&table[index], false);
	Some(index)
}

pub fn count() -> usize {
	DEVICES.lock().len()
}

// The number of retained PCI functions.
pub fn pci_count() -> usize {
	PCI_FUNCTIONS.lock().len()
}

// One retained PCI function by index.
pub fn pci_get(index: usize) -> Option<abi::PciInfo> {
	PCI_FUNCTIONS.lock().get(index).copied()
}

// Run `f` against the device at `index`, returning None if it is out of range. The
// closure runs under the table lock, so callers must not block inside it.
pub fn with<R>(index: usize, f: impl FnOnce(&DeviceEntry) -> R) -> Option<R> {
	let table = DEVICES.lock();
	table.get(index).map(f)
}

// ------------------------------------------------------------------- the claim
//
// WHO OWNS THIS DEVICE, held by the kernel so that nothing else has to be trusted to respect it.
//
// This was a REFERENCE COUNT. `acquire_bus_master` incremented `OWNERS[index]` and turned bus
// mastering on at the 0 -> 1 transition, and nothing refused a second acquisition - so two drivers
// naming one index both got a `DeviceMemory` for its BAR and both drove it. Exclusivity was not
// enforced anywhere; it held because `DeviceManager` happens to launch one driver per device, which
// is a property of today's userspace and not of the kernel. A count is a way of REPRESENTING two
// owners, so the count is what had to go: a device is claimed by exactly one holder or by none.

// The four states a device-table slot can be in, and there is exactly one list of them - `abi` has
// the codes userspace reads and this is the same four by the same names. Two lists is how a state
// ends up meaning different things in two places.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClaimState {
	// Nothing holds it. The only state a new claim may begin from.
	Free,
	// Exactly one holder.
	Claimed,
	// A teardown is under way. A new claim does NOT begin here: the previous holder's mappings and
	// vectors may still exist, and a claim that started now would be a second binding overlapping
	// the first.
	Releasing,
	// The teardown could not be CONFIRMED, so nothing it held goes back into circulation and the
	// device is not claimable again for the life of the boot.
	Quarantined,
}

impl ClaimState {
	// The wire code userspace reads out of `ClaimInfo`. `abi`'s values, not this file's: they cross
	// the syscall boundary, so they belong where userspace can see them.
	pub fn code(self) -> u32 {
		match self {
			ClaimState::Free => abi::CLAIM_STATE_FREE,
			ClaimState::Claimed => abi::CLAIM_STATE_CLAIMED,
			ClaimState::Releasing => abi::CLAIM_STATE_RELEASING,
			ClaimState::Quarantined => abi::CLAIM_STATE_QUARANTINED,
		}
	}
}

// Why a claim or a release was refused. Each maps to one errno at the syscall boundary, and they are
// distinct because a caller that cannot tell them apart cannot retry correctly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClaimError {
	// The index names no device.
	NoSuchDevice,
	// Somebody else holds it, or is in the middle of giving it back. Worth waiting on.
	AlreadyClaimed,
	// Its teardown was never confirmed. Not worth waiting on - this is the rest of the boot.
	Quarantined,
	// The slot ran out of generations. Also the rest of the boot; see `claim`.
	Retired,
	// The DMA policy refused it, or the IOMMU would not confirm the attach. The device is not
	// isolated and therefore does not master the bus.
	Refused,
	// The key names a generation that is no longer current: it belongs to a PREVIOUS binding of this
	// device, and applying it would reach whoever holds the device now.
	Stale,
}

struct ClaimSlot {
	state: ClaimState,
	// The generation of the current claim, or of the last one that ended. 0 before the first, which
	// is why generations start at 1 and no real key ever carries 0.
	generation: u64,
	// Generation space exhausted: never claimed again this boot. See `claim`.
	retired: bool,
	// WHEN A TEARDOWN THAT IS UNDER WAY STOPS BEING ANSWERABLE. Zero unless the state is
	// `Releasing`; stamped fresh at every `Claimed -> Releasing`, from the constant below.
	//
	// THE DEADLINE IS THE CLAIM'S OWN AND IT IS MINTED AT THE RELEASE, not inherited from the bind.
	// "The same hard deadline" fails in both directions: a driver that ran an hour in `Online` has a
	// bind deadline long past, so a release would be born already expired - and when the manager
	// that held the claim DIES, the last close starts the release before any new manager exists to
	// supply one.
	release_deadline: u64,
}

// How long a teardown has to confirm before the device is quarantined.
//
// A CONSTANT OF THE KERNEL'S, not a value handed in, for the reason above: the party that would hand
// one in is exactly the party that may have just died. Two seconds at the 100-ticks-per-second
// monotonic clock - the same order as every other bounded wait in this system, and long enough that
// only a teardown which is genuinely not completing reaches it.
const RELEASE_DEADLINE_TICKS: u64 = 200;

// Parallel to `DEVICES` and taken under the SAME lock as the config-space write below, so two
// acquisitions racing cannot leave the PCI bit disagreeing with the state.
static CLAIMS: SpinLock<Vec<ClaimSlot>> = SpinLock::new(Vec::new());

// Size the claim table from the device table. Called by `init` once the devices are known.
fn reset_claims(len: usize) {
	let mut claims = CLAIMS.lock();
	claims.clear();
	// ALLOC-OK: sized once at boot from the table just built.
	claims.resize_with(len, || ClaimSlot { state: ClaimState::Free, generation: 0, retired: false, release_deadline: 0 });
}

// What state the device at `index` is in, or None if the index names no device.
pub fn claim_state(index: usize) -> Option<ClaimState> {
	CLAIMS.lock().get(index).map(|slot| slot.state)
}

// Whether `key` names the CURRENT binding of its device.
//
// This is the arithmetic the whole milestone rests on: "everything derived from the previous claim"
// is a set a comparison can name, so a mapping or a release carrying an old generation is refused
// without any bookkeeping about what was derived when.
pub fn claim_is_current(key: abi::ClaimKey) -> bool {
	let claims = CLAIMS.lock();
	match claims.get(key.device_index as usize) {
		Some(slot) => slot.state == ClaimState::Claimed && slot.generation == key.generation,
		None => false,
	}
}

// Take the device at `index`. The FIRST claim succeeds and any other is refused.
//
// The generation is minted here and it does not wrap: `checked_add` rather than `wrapping_add`, and
// a slot that runs out is RETIRED for the life of the boot rather than wrapped onto a number a dead
// handle still names. That is the rule handle slots already follow, and the reason is the same - a
// wrapped generation makes a stale key valid again, which is the one thing the key exists to
// prevent. At one claim per microsecond a `u64` lasts about six hundred thousand years, so the
// branch is unreachable in practice and cheap to be right about.
pub fn claim(index: usize) -> Result<abi::ClaimKey, ClaimError> {
	let table = DEVICES.lock();
	let mut claims = CLAIMS.lock();
	let Some(entry) = table.get(index) else { return Err(ClaimError::NoSuchDevice) };
	let Some(slot) = claims.get_mut(index) else { return Err(ClaimError::NoSuchDevice) };
	match slot.state {
		ClaimState::Claimed | ClaimState::Releasing => return Err(ClaimError::AlreadyClaimed),
		ClaimState::Quarantined => return Err(ClaimError::Quarantined),
		ClaimState::Free => {}
	}
	if slot.retired {
		return Err(ClaimError::Retired);
	}
	let Some(generation) = slot.generation.checked_add(1) else {
		slot.retired = true;
		crate::serial_println!("device: {index} has run out of binding generations and is retired for this boot - a wrapped generation would make a dead binding's mappings valid again");
		return Err(ClaimError::Retired);
	};
	// THE DMA THREAT MODEL IS DECIDED HERE, at the one moment a device gains the ability to reach
	// memory on its own. A driver that declared it needs translation does not master the bus without
	// it - and a refusal is a refusal: there is no fall-back to untranslated DMA, because falling
	// back is the failure the isolation claim names in as many words.
	if crate::dma_policy::admit(entry.device_type, entry.bus, entry.dev, entry.func) == dma::BindDecision::Refused {
		return Err(ClaimError::Refused);
	}
	// ATTACHED BEFORE IT CAN MASTER THE BUS, and refused if the attach does not confirm. The window
	// between "this device can reach memory" and "this device is translated" is the one place
	// untranslated DMA could happen under an enforcing profile, and the way to have no such window
	// is to do them in this order.
	//
	// AND THE GENERATION IS THIS BINDING'S. `attach_for` used to pass a hardcoded 1 and say so in
	// its own comment; it now takes the number minted three lines up, which is what makes a mapping
	// from a previous binding refusable by arithmetic.
	//
	// AN ENTRY THAT IS NOT ON THE BUS HAS NO ENDPOINT TO TRANSLATE, for the same reason it has no
	// config space to write: there is no function there. Asking the controller to probe one is asking
	// about hardware that does not exist, and under an ENFORCING profile the honest refusal that
	// comes back would make a synthetic entry unclaimable - which is the opposite of what it is for.
	//
	// Found by `check.sh --gate qemu-virtio-iommu-x86_64`, which is exactly the profile that can tell
	// this apart from a device that is merely absent: `endpoint 0xffff could not be probed
	// (NotMapped) - its reserved regions are unknown, so it is not attached`.
	if entry.on_bus && crate::iommu::translating() && !crate::iommu::attach_for(index, entry.bus, entry.dev, entry.func, generation) {
		return Err(ClaimError::Refused);
	}
	bus_master(entry, true);
	slot.state = ClaimState::Claimed;
	slot.generation = generation;
	Ok(abi::ClaimKey { device_index: index as u32, _pad: 0, generation })
}

// Move a live claim into `Releasing`, so nothing new begins while the teardown runs.
//
// A KEY NAMING A STALE GENERATION IS REFUSED rather than applied to whoever holds the device now.
// That is the whole reason the key carries a generation: without it, a release from a driver that
// died three bindings ago tears down the current one.
fn begin_release(key: abi::ClaimKey) -> Result<(), ClaimError> {
	let mut claims = CLAIMS.lock();
	let Some(slot) = claims.get_mut(key.device_index as usize) else { return Err(ClaimError::NoSuchDevice) };
	if slot.generation != key.generation {
		return Err(ClaimError::Stale);
	}
	match slot.state {
		ClaimState::Claimed => {
			slot.state = ClaimState::Releasing;
			// A FRESH ABSOLUTE DEADLINE, every time. Immutable once stamped: a teardown that keeps
			// pushing its own deadline out is not bounded by it.
			slot.release_deadline = crate::arch::apic::ticks().saturating_add(RELEASE_DEADLINE_TICKS);
			Ok(())
		}
		// Already on its way out, and the caller's key is current - so this is a second release of
		// one claim. Not an error the caller can act on and not a state change: whoever got here
		// first owns the teardown.
		ClaimState::Releasing => Err(ClaimError::AlreadyClaimed),
		ClaimState::Quarantined => Err(ClaimError::Quarantined),
		ClaimState::Free => Err(ClaimError::Stale),
	}
}

// End the teardown: `Free` when every resource was confirmed given back, `Quarantined` when it was
// not. A quarantined slot is never claimed again this boot and its frames and vectors never return
// to circulation.
fn finish_release(index: usize, confirmed: bool) -> ClaimState {
	let mut claims = CLAIMS.lock();
	let Some(slot) = claims.get_mut(index) else { return ClaimState::Quarantined };
	// A COMPLETION AFTER THE LATCH RELEASES NOTHING, and this is where that is enforced rather than
	// hoped for. `snapshot` latches `Releasing -> Quarantined` at the deadline; a teardown finishing
	// afterwards would otherwise put the frames and vectors back into circulation against a state
	// already declared terminal, which is two authorities over one device.
	if slot.state == ClaimState::Quarantined {
		crate::serial_println!("device: {index} finished its teardown after the deadline had already quarantined it - the completion is recorded and releases nothing");
		return ClaimState::Quarantined;
	}
	slot.state = if confirmed { ClaimState::Free } else { ClaimState::Quarantined };
	slot.release_deadline = 0;
	slot.state
}

// WHAT A NEW MANAGER NEEDS TO KNOW ABOUT ONE DEVICE, in one read.
//
// The generation, the claim state and the deadline a teardown under way must confirm by. ONE READ
// ANSWERS BOTH QUESTIONS a reconstruction has - "may I bind this?" and "how long is it reasonable to
// wait?" - rather than two sources that can disagree.
//
// AND IT LATCHES. A binding marked `Quarantined` in userspace while the kernel's claim stays
// `Releasing` is two authorities again, so the transition happens HERE, atomically, under the same
// lock every other claim decision is taken under.
pub fn snapshot(index: usize) -> Option<abi::DeviceClaimSnapshot> {
	let mut claims = CLAIMS.lock();
	let slot = claims.get_mut(index)?;
	if slot.state == ClaimState::Releasing && slot.release_deadline != 0 && crate::arch::apic::ticks() >= slot.release_deadline {
		slot.state = ClaimState::Quarantined;
		crate::serial_println!("device: {index} did not confirm its teardown inside the deadline - quarantined, and nothing it held is reused");
	}
	// WHAT THE CLAIM STILL HOLDS, counted from the kernel's own records rather than from a number
	// somebody remembered to keep. `DERIVED` is every capability minted under this key - the MMIO
	// window a driver maps is one - the MSI registry knows which slots this device owns, and the
	// IOMMU knows what its domain still has mapped. See `DeviceClaimSnapshot`.
	let key = abi::ClaimKey { device_index: index as u32, _pad: 0, generation: slot.generation };
	let mmio_windows = DERIVED.lock().iter().filter(|row| row.key == key && row.object.strong_count() > 0).count() as u32;
	let irq_vectors = crate::arch::interrupts::msi_held_by_device(index as u32) as u32;
	let iommu_grants = crate::iommu::grants_for(index as u32) as u32;
	// AND THE QUARANTINED SUBSET, because the total alone cannot say whether this claim's address
	// space may ever be reused. See `DeviceClaimSnapshot`.
	let iommu_quarantined = crate::iommu::quarantined_grants_for(index as u32) as u32;
	Some(abi::DeviceClaimSnapshot { state: slot.state as u32, _pad0: 0, generation: slot.generation, release_deadline: slot.release_deadline, mmio_windows, irq_vectors, iommu_grants, iommu_quarantined })
}

// RELEASE THE DEVICE, and prove it is quiet before anything it held is reused.
//
// The order is the property, and it is the order M5 of this milestone names:
//
//   1. bus mastering off - the device cannot start a new transaction;
//   2. every capability derived from this claim revoked - including the MMIO MAPPING itself, so the
//      raw virtual address the driver had already mapped faults rather than reaching the BAR;
//   3. interrupts masked and their vectors held;
//   4. the IOMMU teardown, CONFIRMED - and only then do frames and vectors go back into circulation.
//
// Reversing any pair of these leaves a window: a device that can still master the bus and is no
// longer translated, or a frame handed to the next allocation while a descriptor still points at it.
//
// THIS RUNS WHETHER THE HOLDER COOPERATED OR NOT. Nothing here asks the holder for anything, which
// is what makes it a forced release rather than a request.
pub fn release_claim(key: abi::ClaimKey) -> Result<ClaimState, ClaimError> {
	begin_release(key)?;
	let index = key.device_index as usize;
	let Some((bus, dev, func)) = with(index, |d| (d.bus, d.dev, d.func)) else {
		// The device table shrank under a live claim, which cannot happen after boot - but if it
		// ever did, nothing can be confirmed about hardware that is not described.
		return Ok(finish_release(index, false));
	};
	// 1. BUS MASTERING FIRST. Everything below assumes the device is not starting new transactions.
	with(index, |entry| bus_master(entry, false));
	// 2. Everything the claim minted stops working, without the holder's cooperation - and every
	//    interrupt it derived is unbound HERE rather than whenever its last reference goes.
	let interrupts_quiet = revoke_derived(key);
	// 3. The translation, which is the other step that can fail to CONFIRM. `detach_for` reports a
	//    detach it could not confirm and leaves the pages quarantined; the claim goes the same way,
	//    because a device whose mappings may still be live is not a device to hand to anyone else.
	let translation_quiet = if crate::iommu::translating() { crate::iommu::detach_for(index, bus, dev, func) } else { true };
	// EVERY RESOURCE, NOT ONE OF THEM. The terminal state was derived from the IOMMU alone, so a
	// vector whose unbind could not be confirmed - a riscv64 hart that did not answer, which
	// quarantines the still-armed slot - was charged to a claim this then published as `Free`.
	let confirmed = translation_quiet && interrupts_quiet;
	if !interrupts_quiet {
		crate::serial_println!("device: {index} could not confirm that every interrupt it derived was unbound - the claim is not free");
	}
	// 4. THE TERMINAL STATE, AND ONLY THEN THE VECTORS.
	//
	// The vectors used to be released before this, on the strength of `confirmed` alone. But
	// `finish_release` is where the DEADLINE LATCH is observed - `snapshot` can quarantine a claim
	// from another core while this teardown is still waiting for the IOMMU - so a release that took
	// too long could put its vectors back into circulation and then be told it was quarantined. The
	// state decides, and it is decided first.
	//
	// `release_for_device` clears `pending`, clears the owner and marks the slot UNUSED, which is
	// the slot becoming allocatable again. The rule this file states everywhere else is that an
	// unconfirmed teardown keeps its resources charged and out of circulation, and a vector is one
	// of them: a device that may still be translating may still be raising interrupts.
	let state = finish_release(index, confirmed);
	if state == ClaimState::Free {
		let vectors = crate::arch::interrupts::release_msi_for_device(index as u32);
		if vectors != 0 {
			crate::serial_println!("device: {index} released - {vectors} MSI vector(s) given back");
		}
	} else {
		crate::serial_println!("device: {index} did not confirm its teardown - its MSI vector(s) stay masked and held rather than given back");
	}
	// AND THE AUDIT RECORD GOES WITH IT. `admit` wrote the degraded row when this device was asking
	// to master the bus; it is not mastering it any more, and a list of "devices reaching memory
	// untranslated" that keeps a device which gave the bus back is a list reporting a machine
	// nobody is running.
	crate::dma_policy::forget_degraded(bus, dev, func);
	if state == ClaimState::Quarantined {
		crate::serial_println!("device: {:02x}:{:02x}.{} was NOT confirmed torn down - device {index} is quarantined for this boot and nothing it held is reused", bus, dev, func);
	}
	Ok(state)
}

// The one MMIO capability a binding derived has gone, so the device stops mastering the bus.
//
// NOT A RELEASE. The claim belongs to whoever took it and only a release ends it - a device whose
// driver died is not free for the next claimant until the teardown has been run and confirmed. What
// this ends is narrower and must not wait for anybody: nothing holds this device's registers any
// more, so nothing should be able to write to memory on its behalf.
//
// Refused for a STALE key, which is the case that makes this worth a lookup rather than a bare
// write: a `DeviceMemory` from a previous binding can be dropped at any time - it may have been
// sitting in a message queue, or in a process being torn down - and turning bus mastering off for
// whoever holds the device NOW is exactly the cross-binding damage the generation exists to stop.
pub fn mmio_capability_dropped(key: abi::ClaimKey) {
	let table = DEVICES.lock();
	let claims = CLAIMS.lock();
	let index = key.device_index as usize;
	let (Some(entry), Some(slot)) = (table.get(index), claims.get(index)) else { return };
	if slot.state != ClaimState::Claimed || slot.generation != key.generation {
		return;
	}
	bus_master(entry, false);
}

// Let the device at `index` master the bus, or stop it.
//
// ONE PLACE, so that the one kind of entry that must not be written to config space can be excluded
// in exactly one place rather than at each of the three call sites.
fn bus_master(entry: &DeviceEntry, on: bool) {
	// AN ENTRY THAT IS NOT ON THE BUS HAS NO CONFIG SPACE TO WRITE. See `DeviceEntry::on_bus`.
	if !entry.on_bus {
		return;
	}
	crate::arch::pci::set_bus_master(entry.bus, entry.dev, entry.func, on);
}

// STOP THIS DEVICE MASTERING THE BUS, by index, without ending its claim.
//
// The one caller is the MSI-doorbell map failure in `SYS_DEVICE_MSIX_ACQUIRE`. The isolation rule
// says a map failure ends in a refused binding, disabled bus mastering, or quarantine, and refusing
// only the vector was none of the three: the device kept its claim and kept reaching memory while
// losing the one channel it had for saying so. This is the middle ending, and it is deliberately
// narrower than a release - the claim, its record and its report all survive, so the manager can
// still see what happened to a binding it made.
pub fn disable_bus_master(index: usize) {
	with(index, |entry| bus_master(entry, false));
}

// A DEVICE THE MACHINE DOES NOT HAVE, so that the claim table can be tested without taking a device
// something else is driving.
//
// The hardware suite's bus-master check is what this exists to avoid repeating: it looked for a
// device nobody was driving and returned quietly when every device on the machine was claimed, which
// on a healthy boot is most of them. A gate whose subject can vanish is a gate that passes when
// there was nothing to test. A synthetic entry cannot vanish and cannot be claimed by anything else,
// so a test that uses one has no way to skip itself green.
//
// Nothing reaches the bus for these: they are marked `on_bus: false`, which is what `bus_master`
// reads, and no BAR of theirs is ever mapped - the tests that use one are about the TABLE.

// Append one, and answer with its index. It stays for the life of the boot, which is the life of the
// test kernel.
#[cfg(test)]
pub fn add_synthetic_device() -> usize {
	let mut table = DEVICES.lock();
	let index = table.len();
	// ALLOC-OK: `#[cfg(test)]`, and a test that cannot allocate has already failed.
	table.push(DeviceEntry { device_type: u16::MAX, transport: abi::TRANSPORT_PLAIN_PCI, vendor: 0xffff, product: 0xffff, bar_phys: 0, bar_len: 0, common_offset: 0, notify_offset: 0, notify_multiplier: 0, isr_offset: 0, device_offset: 0, device_len: 0, msix_cap: 0, msix_table_phys: 0, bus: 0xff, dev: 0x1f, func: 7, class: 0xff, subclass: 0xff, prog_if: 0xff, on_bus: false });
	drop(table);
	CLAIMS.lock().push(ClaimSlot { state: ClaimState::Free, generation: 0, retired: false, release_deadline: 0 });
	index
}

// Start a teardown without running it, so a test can hold a claim in `Releasing` and look at what
// the deadline does. The real path is `release_claim`, which does this and then the work.
#[cfg(test)]
pub fn begin_release_for_test(key: abi::ClaimKey) -> Result<(), ClaimError> {
	begin_release(key)
}

// Wind a live teardown's deadline into the past, which is what one that does not complete looks
// like from outside. A test cannot wait two seconds of wall clock for it and should not have to.
#[cfg(test)]
pub fn expire_release_for_test(index: usize) {
	let mut claims = CLAIMS.lock();
	if let Some(slot) = claims.get_mut(index) {
		slot.release_deadline = 1;
	}
}

// Finish a teardown started by `begin_release_for_test`, so the LATE completion can be driven.
#[cfg(test)]
pub fn finish_release_for_test(index: usize, confirmed: bool) -> ClaimState {
	finish_release(index, confirmed)
}

// Put a synthetic slot one generation below the ceiling, so the retirement branch is reachable in a
// test instead of being a line nobody has ever executed.
#[cfg(test)]
pub fn exhaust_generations_of(index: usize) {
	if let Some(slot) = CLAIMS.lock().get_mut(index) {
		slot.generation = u64::MAX;
	}
}

// ------------------------------------------------- what a claim has derived
//
// EVERY CAPABILITY THE KERNEL MINTED UNDER A CLAIM, so that ending the claim can end all of them.
//
// Weakly, because this table must not be what keeps an object alive: an entry whose object is gone
// is an entry with nothing left to revoke. It is swept on every release, so a claim that ends takes
// its own rows out; rows of a claim that never ends live as long as the boot, which is bounded by
// the devices the boot scan found times the capabilities one binding derives.

struct Derived {
	key: abi::ClaimKey,
	object: alloc::sync::Weak<dyn crate::object::KernelObject>,
}

static DERIVED: SpinLock<Vec<Derived>> = SpinLock::new(Vec::new());

// Record that `object` was minted under `key`. False when the table could not grow, which the caller
// must treat as a failed mint: a capability the revocation cannot reach is exactly what this
// milestone exists to make impossible, so it must not be handed out.
pub fn register_derived(key: abi::ClaimKey, object: alloc::sync::Weak<dyn crate::object::KernelObject>) -> bool {
	// THE KEY HAS TO STILL BE THE LIVE CLAIM, AND THE CHECK HOLDS THE RELEASE'S OWN LOCK.
	//
	// This pushed a row for any key at all. `begin_release` moves the slot to `Releasing` under
	// `CLAIMS` and the sweep then drains `DERIVED` under a different lock, so a syscall that had
	// already passed capability lookup could register AFTER the sweep and hand out a capability the
	// revocation would never reach - which is exactly what this table exists to prevent.
	//
	// `CLAIMS` is taken first and held across the push, so a release cannot begin between the check
	// and the row landing. That is the same order `snapshot` takes them in, and the only order in
	// this file.
	let claims = CLAIMS.lock();
	let Some(slot) = claims.get(key.device_index as usize) else { return false };
	if slot.generation != key.generation || slot.state != ClaimState::Claimed {
		return false;
	}
	let mut derived = DERIVED.lock();
	// ALLOC-OK: one row per capability a binding derives, checked before it is taken.
	if derived.try_reserve(1).is_err() {
		return false;
	}
	derived.push(Derived { key, object });
	true
}

// Everything minted under `key` stops working, whatever process it ended up in and however many it
// was passed through.
//
// TWO MECHANISMS, because a handle that refuses is not evidence that a MAPPING is gone. The object's
// revocation generation is bumped, which invalidates every capability to it at lookup - that is the
// handle half, and it is O(1). And a `DeviceMemory` that was already mapped has its mapping torn
// out of the address space it was mapped in, which is the half the driver notices: the raw virtual
// address it has been using faults instead of reaching the BAR.
// Returns whether every interrupt this key derived CONFIRMED its teardown. See `revoke_effects_of`.
fn revoke_derived(key: abi::ClaimKey) -> bool {
	// TAKEN OUT UNDER THE LOCK AND RELEASED BEFORE THE WORK, because tearing a mapping down takes
	// the address space's own locks and calling into them from under this one would be a lock order
	// nothing else in the kernel uses.
	let mut taken: Vec<alloc::sync::Weak<dyn crate::object::KernelObject>> = Vec::new();
	{
		let mut derived = DERIVED.lock();
		let mut kept: Vec<Derived> = Vec::new();
		// ALLOC-OK: bounded by the table being swept, on a binding transition.
		if kept.try_reserve(derived.len()).is_err() || taken.try_reserve(derived.len()).is_err() {
			// A SWEEP THAT CANNOT ALLOCATE STILL REVOKES. Falling back to the slower shape - one
			// pass, in place - costs time on a path that runs once per binding and keeps the
			// property, which is the thing that must not depend on a heap.
			let mut quiet = true;
			for row in derived.iter() {
				if row.key == key {
					if let Some(object) = row.object.upgrade() {
						object.header().revoke();
						quiet &= revoke_effects_of(&object);
					}
				}
			}
			derived.retain(|row| row.key != key);
			return quiet;
		}
		for row in derived.drain(..) {
			if row.key == key {
				taken.push(row.object);
			} else {
				kept.push(row);
			}
		}
		*derived = kept;
	}
	let mut quiet = true;
	for weak in taken {
		if let Some(object) = weak.upgrade() {
			object.header().revoke();
			quiet &= revoke_effects_of(&object);
		}
	}
	quiet
}

// The type-specific half of revocation.
//
// A `DeviceMemory` whose mapping is live LOSES THE MAPPING, which is the half a driver notices: the
// raw virtual address it has been using for as long as it has been running faults instead of
// reaching the BAR. Revoking the capability alone would not touch it.
//
// A `DmaBuffer` is marked as one its owner never released, so its frames are HELD rather than
// returned to circulation. That is the same rule a terminated process's buffers already follow and
// for the same reason: a forced release means nobody reset this device, so the physical addresses it
// was handed may still be sitting in a live descriptor. `SYS_DEVICE_QUIESCED` is the one claim that
// can say otherwise, and it comes from a driver that has just reset the hardware.
//
// An `Interrupt` LOSES ITS VECTOR HERE, and this said it needed nothing.
//
// The argument was that `release_msi_for_device` masks and holds the vector. It does that for a slot
// already marked pending, and does nothing at all to an ordinary live binding - and the actual
// unbind sat in `Interrupt::drop`, which a forced release cannot reach: the holder is still running
// by definition, and a wait in progress keeps the object alive as long as it likes. So a released
// device kept a bound, deliverable vector aimed at the old driver, and the next claimant could not
// be given that slot either.
//
// Returns whether the teardown CONFIRMED, which only an interrupt can answer no to - see
// `Interrupt::revoke` and riscv64's `unbind`. Everything else here is a local operation that cannot
// fail, so `true` is a statement rather than an assumption.
fn revoke_effects_of(object: &alloc::sync::Arc<dyn crate::object::KernelObject>) -> bool {
	if let Some(memory) = object.as_any().downcast_ref::<crate::object::device_memory::DeviceMemory>() {
		memory.teardown_mapping();
	}
	if let Some(buffer) = object.as_any().downcast_ref::<crate::object::dma_buffer::DmaBuffer>() {
		buffer.mark_orphaned();
	}
	if let Some(interrupt) = object.as_any().downcast_ref::<crate::object::interrupt::Interrupt>() {
		return interrupt.revoke();
	}
	true
}

// Forget every row belonging to an object that is gone, for the tests that ask how big this gets.
#[cfg(test)]
pub fn derived_rows() -> usize {
	DERIVED.lock().len()
}
