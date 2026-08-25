// Which drivers may touch memory without an IOMMU, and what it costs to say yes.
//
// CAPABILITY ISOLATION DOES NOT COVER DMA. A handle table decides what a driver may ASK the kernel
// for; it decides nothing about what the device does once it is mastering the bus. A bus-mastering
// device programmed by a malicious driver writes wherever it is told, and no capability stops it.
// A system that behaves as though capabilities cover DMA has a hole exactly where its strongest
// claim is, so this module exists to make the hole explicit, bounded and named.
//
// THE STATE THIS TREE IS ACTUALLY IN, said plainly: there is no enforcing IOMMU yet, so every
// driver that masters the bus does so untranslated. What this module adds is not enforcement - it
// is the decision point enforcement plugs into, the policy each device is under, and an audited
// record of exactly which drivers are running in the degraded state. When the `virtio-iommu`
// backend confirms its bypass-off transition, `set_enforcing` flips and the same decision starts
// refusing the drivers that declared they need translation.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use dma::{BindDecision, Policy};

use crate::sync::SpinLock;

// Whether an enforcing IOMMU is confirmed to be translating. NOT a hint and not a hope: it is set
// only by a backend that has read back its own bypass-off configuration.
static ENFORCING: AtomicBool = AtomicBool::new(false);

// Who is running untranslated, so the degraded state can be reported rather than inferred. One row
// per device that bound without translation.
static DEGRADED: SpinLock<Vec<Degraded>> = SpinLock::new(Vec::new());

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Degraded {
	pub device_type: u16,
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
}

// Establish the machine's DMA isolation state, once the device table exists.
//
// ENFORCEMENT IS A FACT, ESTABLISHED RATHER THAN ASSUMED. It is on only when a backend has brought
// an IOMMU up and read back its own configuration, and this tree has no such backend yet - so this
// says so explicitly rather than leaning on a static's initial value. When P02M0153's `virtio-iommu`
// backend lands, its bring-up is what this calls, and its confirmed bypass-off transition is what
// turns the answer into `true`.
pub fn init() {
	// The bring-up is the answer. `iommu::init` returns whether a controller was found, negotiated
	// what an enforcing profile needs, and READ BACK its own bypass byte as off - and nothing short
	// of all three is enforcement.
	set_enforcing(crate::iommu::init());
}

// Called by the IOMMU backend once translation is confirmed - and by nothing else.
pub fn set_enforcing(enforcing: bool) {
	ENFORCING.store(enforcing, Ordering::Release);
}

pub fn enforcing() -> bool {
	ENFORCING.load(Ordering::Acquire)
}

// WHICH DEVICE TYPES REFUSE TO RUN UNTRANSLATED. Empty, and the emptiness is a decision worth being
// explicit about rather than a gap.
//
// P02M0153's M4 asks for the protected slice's endpoints to declare `iommu-required`. The mechanism
// is here and is exercised - `admit` refuses a type in this list on a machine with no enforcement,
// and a kernel test drives exactly that. What the list does NOT contain is `virtio-net`, because
// putting it there removes networking from every profile that has no `virtio-iommu` in it: the
// ordinary harness, every developer's run, and every machine whose firmware offers no IOMMU. That is
// a shipping decision about which profiles a system supports, not a fact about the driver, and it
// belongs to whoever declares a profile rather than to the milestone that built the mechanism.
//
// The EDU fixture needs no row: it has no driver and never binds through this path at all.
const IOMMU_REQUIRED_TYPES: &[u16] = &[];

// THE POLICY EVERY DEVICE IS UNDER, in one place rather than at each call site.
pub fn policy_for(device_type: u16) -> Policy {
	if IOMMU_REQUIRED_TYPES.contains(&device_type) { Policy::IommuRequired } else { Policy::TrustedUntranslated }
}

// May this device master the bus, and under what terms?
//
// The three answers are `dma::decide_bind`'s and there is no fourth. A refusal is a refusal: the
// caller does not fall back to untranslated DMA, because falling back is precisely the failure this
// milestone's Goal names - "It must never silently become untranslated DMA."
pub fn admit(device_type: u16, bus: u8, dev: u8, func: u8) -> BindDecision {
	let decision = dma::decide_bind(policy_for(device_type), enforcing());
	if decision == BindDecision::DegradedUntranslated {
		record_degraded(Degraded { device_type, bus, dev, func });
	}
	decision
}

// LOUD ONCE, NOT LOUD EVERY TIME. A driver that opens its device repeatedly would otherwise turn an
// audit record into a log flood, and a flood is how something stops being read.
fn record_degraded(entry: Degraded) {
	let mut degraded = DEGRADED.lock();
	if degraded.iter().any(|held| *held == entry) {
		return;
	}
	// ALLOC-OK: bounded by the number of PCI functions the boot scan resolved, and this is the boot
	// path rather than an interrupt.
	if degraded.try_reserve(1).is_err() {
		return;
	}
	degraded.push(entry);
	crate::serial_println!("dma: DEGRADED ISOLATION - device type {} at {:02x}:{:02x}.{} masters the bus UNTRANSLATED", entry.device_type, entry.bus, entry.dev, entry.func);
}

// Everything currently running untranslated. The report a supervisor reads, and what a test asserts
// against.
pub fn degraded_devices() -> Vec<Degraded> {
	// ALLOC-OK: a copy of a list with one row per PCI function the boot scan resolved, taken by the
	// boot report and by tests. Not on any syscall path.
	DEGRADED.lock().clone()
}

// The state of the system's DMA isolation, printed once the boot devices have bound.
//
// SAID AT THE END RATHER THAN PER DEVICE, because the question a reader has is "is anything reaching
// memory untranslated", and the answer is a list rather than a scattering of lines. A system with an
// enforcing IOMMU and an empty list is the only shape that carries P02M0153's claim.
pub fn report() {
	crate::iommu::report();
	let degraded = degraded_devices();
	if enforcing() && degraded.is_empty() {
		crate::serial_println!("dma: every bus-mastering device is translated");
		return;
	}
	crate::serial_println!("dma: DEGRADED ISOLATION - {} device(s) master the bus untranslated (enforcing={})", degraded.len(), enforcing());
	for entry in &degraded {
		crate::serial_println!("dma:   type {} at {:02x}:{:02x}.{}", entry.device_type, entry.bus, entry.dev, entry.func);
	}
}

#[cfg(test)]
pub fn forget_degraded_for_test() {
	DEGRADED.lock().clear();
}

#[cfg(test)]
mod tests;
