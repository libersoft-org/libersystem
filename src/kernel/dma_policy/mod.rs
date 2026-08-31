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

// WHETHER THE ISOLATION SUMMARY HAS ALREADY BEEN PUBLISHED.
//
// `report` is printed once the boot devices have bound, and "have bound" is a judgement the kernel
// makes from the outside: it is the moment its supervisor decides the system is up. That moment can
// legitimately arrive while a driver is still on its way to the bus - a machine whose console never
// attaches gets there on a deadline, not on a device count - and a summary taken then says "every
// bus-mastering device is translated" about a machine that may be about to admit one that is not.
//
// Nothing else in the log would contradict it: admissions are RECORDED rather than printed, on
// purpose, so the list replaces a scattering of lines. So the summary is the only statement a reader
// has, and a stale one is indistinguishable from a true one. This flag is what lets the claim
// retract itself: once it has been published, a later degraded admission says so at the moment it
// happens.
static REPORTED: AtomicBool = AtomicBool::new(false);

// HOW MANY TIMES A PUBLISHED CLAIM HAS BEEN RETRACTED, so a test can assert the retraction rather
// than assert the flag that leads to it. The line itself goes to the serial console, which a kernel
// test cannot read back; the count is the same event, observed from inside.
#[cfg(test)]
static RETRACTIONS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub fn retractions_for_test() -> usize {
	RETRACTIONS.load(Ordering::Relaxed)
}

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
// says so explicitly rather than leaning on a static's initial value. The `virtio-iommu` backend's
// bring-up is what this calls, and its confirmed bypass-off transition is what turns the answer
// into `true`.
pub fn init() {
	// The bring-up is the answer. `iommu::init` returns whether a controller was found, negotiated
	// what an enforcing profile needs, and READ BACK its own bypass byte as off - and nothing short
	// of all three is enforcement.
	let enforcing = crate::iommu::init();
	// AND WHETHER THIS MACHINE WAS SUPPOSED TO BE ISOLATED IS A DIFFERENT QUESTION, asked of the bus
	// rather than of the bring-up: a controller that is present and did not come up is the case a
	// protected driver must refuse, and a machine with no controller is the case it must not.
	set_isolation_expected(crate::iommu::present());
	set_enforcing(enforcing);
}

// Called by the IOMMU backend once translation is confirmed - and by nothing else.
pub fn set_enforcing(enforcing: bool) {
	ENFORCING.store(enforcing, Ordering::Release);
}

pub fn enforcing() -> bool {
	ENFORCING.load(Ordering::Acquire)
}

// WHICH DEVICE TYPES REFUSE TO RUN UNTRANSLATED, on a machine that has an IOMMU to be translated by.
//
// THE LIST WAS EMPTY, AND THE EMPTINESS WAS THE PROBLEM. The mechanism was built, exercised by a
// kernel test and declared by nothing: every production device type answered `TrustedUntranslated`,
// so a system claiming to confine its bus-mastering endpoints had no endpoint that insisted on being
// confined. The argument for leaving it empty was real - putting `virtio-net` in an unconditional
// list removes networking from every profile with no controller in it, which is the ordinary
// harness, every developer's run and every machine whose firmware offers no IOMMU - but the
// conclusion did not follow. What was wrong was the word "unconditional", not the list.
//
// SO THE POLICY ASKS THE MACHINE FIRST. A machine with no `virtio-iommu` on its bus is one where
// untranslated DMA is the only DMA there is, and a protected driver runs there exactly as it did
// before. A machine that HAS one and did not bring it up is a different machine: isolation was
// available and something went wrong with it, and that is precisely the case where a driver that
// declared it needs translation must not quietly run without it. `isolation_expected` is what tells
// those two apart, and `iommu::init` sets it from whether a controller was found on the bus rather
// than from whether the bring-up succeeded.
//
// The EDU fixture needs no row: it has no driver and never binds through this path at all.
const IOMMU_REQUIRED_TYPES: &[u16] = &[abi::VIRTIO_TYPE_NET as u16];

// Whether this machine has an IOMMU at all - not whether it is working. See the list above.
static ISOLATION_EXPECTED: AtomicBool = AtomicBool::new(false);

// Called by the IOMMU bring-up once it knows whether the machine carries a controller, and by
// nothing else. A machine that carries one is held to the list above; a machine that does not is not.
pub fn set_isolation_expected(expected: bool) {
	ISOLATION_EXPECTED.store(expected, Ordering::Release);
}

pub fn isolation_expected() -> bool {
	ISOLATION_EXPECTED.load(Ordering::Acquire)
}

// THE POLICY EVERY DEVICE IS UNDER, in one place rather than at each call site.
pub fn policy_for(device_type: u16) -> Policy {
	if isolation_expected() && IOMMU_REQUIRED_TYPES.contains(&device_type) { Policy::IommuRequired } else { Policy::TrustedUntranslated }
}

// May this device master the bus, and under what terms?
//
// The three answers are `dma::decide_bind`'s and there is no fourth. A refusal is a refusal: the
// caller does not fall back to untranslated DMA, because falling back is precisely the failure this
// milestone's Goal names - "It must never silently become untranslated DMA."
pub fn admit(device_type: u16, bus: u8, dev: u8, func: u8) -> BindDecision {
	let decision = dma::decide_bind(policy_for(device_type), enforcing());
	// AND THE DEGRADED ANSWER IS ONLY GIVEN IF IT CAN BE AUDITED.
	//
	// This called `record_degraded` and returned `DegradedUntranslated` whatever it answered, while
	// `record_degraded` returned silently when its row could not be allocated - so under memory
	// pressure a device mastered memory untranslated with NO durable row: nothing to retract, and a
	// `report` that could go on to print "every bus-mastering device is translated" over a machine
	// where one is not. M7 makes the degraded state an AUDITED one, and an unaudited degradation is
	// not a weaker version of it - it is the untracked bypass this milestone exists to remove.
	//
	// Failing CLOSED costs a device that could have run; failing open costs the isolation claim.
	if decision == BindDecision::DegradedUntranslated && !record_degraded(Degraded { device_type, bus, dev, func }) {
		crate::serial_println!("dma: {} at {:02x}:{:02x}.{} would master the bus untranslated and its audit row could not be recorded - REFUSED rather than admitted unaudited", abi::device_type_name(device_type as u32), bus, dev, func);
		return BindDecision::Refused;
	}
	decision
}

// RECORDED, NOT PRINTED. `report` is what prints, once the devices have bound, and its own comment
// says why: "the answer is a list rather than a scattering of lines". This printed a line per device
// as well, so a boot with eleven untranslated endpoints carried twenty-two lines saying so - the
// scattering AND the list it exists to replace.
//
// The record is still deduplicated: a driver that opens its device repeatedly must not grow the
// list it is audited from.
// Answers whether the row is RECORDED - which includes "it was already there", because a duplicate
// is one audited device and not a failure to audit one.
fn record_degraded(entry: Degraded) -> bool {
	// THE LOCK IS RELEASED BEFORE ANYTHING IS PRINTED. The line below is the only place this module
	// prints while a caller is mid-admission, and doing it under the list's lock would put the
	// serial path inside a section every other reader of the list waits on.
	let late: bool = {
		let mut degraded = DEGRADED.lock();
		if degraded.iter().any(|held| *held == entry) {
			return true;
		}
		// ALLOC-OK: bounded by the number of PCI functions the boot scan resolved, and this is the
		// boot path rather than an interrupt.
		if degraded.try_reserve(1).is_err() {
			return false;
		}
		degraded.push(entry);
		REPORTED.load(Ordering::Relaxed)
	};
	// A PUBLISHED CLAIM THAT STOPPED BEING TRUE SAYS SO, HERE, AT THE MOMENT IT STOPS.
	//
	// The summary is printed when the supervisor decides the system is up, and that decision can be
	// made on a deadline over a machine whose devices are still binding. Everything admitted
	// afterwards would then be invisible: the record is silent by design and the summary is already
	// behind in the log. One line at the moment of admission costs nothing on the boots where it
	// never fires, and on the boot where it does it is the difference between an audit trail and a
	// wrong sentence nobody can see is wrong.
	if late {
		#[cfg(test)]
		RETRACTIONS.fetch_add(1, Ordering::Relaxed);
		crate::serial_println!("dma: ADMITTED UNTRANSLATED AFTER THE ISOLATION SUMMARY - {} at {:02x}:{:02x}.{} masters the bus untranslated, so the summary above is no longer this machine's state", abi::device_type_name(entry.device_type as u32), entry.bus, entry.dev, entry.func);
	}
	true
}

// The device has given the bus back, so it is no longer one of the devices reaching memory
// untranslated - and the audit list must stop saying it is.
//
// THE RECORD HAS THE SAME LIFETIME AS THE OWNERSHIP IT DESCRIBES. `admit` writes the row at the
// moment a driver ASKS to master the bus, which is the right moment to take the decision and the
// wrong one to make the record permanent: a driver that asked, failed to bind and released was
// audited forever as reaching memory untranslated. A boot with a driver that does not come up
// printed one more degraded row than there were degraded devices, beside a device summary that
// counted correctly - two adjacent lines disagreeing about the same machine.
pub fn forget_degraded(bus: u8, dev: u8, func: u8) {
	DEGRADED.lock().retain(|held| !(held.bus == bus && held.dev == dev && held.func == func));
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
// enforcing IOMMU and an empty list is the only shape that carries the isolation claim.
//
// AND "THE END" HAS TO BE ONE. This was called from the machine report at the top of
// `boot_userspace`, before a single driver had bound - so it printed `0 device(s) master the bus
// untranslated` and was then followed by ten lines naming devices that do, which is the exact
// scattering it exists to replace, under a summary contradicting it. The devices bind in userspace;
// the summary is printed once they have, which is the first moment its own sentence is true.
pub fn report() {
	// AND THE CONTROLLER'S OWN STATE WITH IT. What this machine has to translate WITH and who is
	// running untranslated are one question with two halves, and a reader wants them together.
	crate::iommu::report();
	let degraded = degraded_devices();
	if enforcing() && degraded.is_empty() {
		crate::serial_println!("dma: every bus-mastering device is translated");
		// ARMED ON THE CLEAN PATH TOO, and this is the path that needs it. "Every bus-mastering
		// device is translated" is the strongest sentence this kernel prints about isolation, and it
		// is the one a later admission falsifies.
		REPORTED.store(true, Ordering::Relaxed);
		return;
	}
	crate::serial_println!("dma: DEGRADED ISOLATION - {} device(s) master the bus untranslated (enforcing={})", degraded.len(), enforcing());
	for entry in &degraded {
		crate::serial_println!("dma:   {} at {:02x}:{:02x}.{}", abi::device_type_name(entry.device_type as u32), entry.bus, entry.dev, entry.func);
	}
	REPORTED.store(true, Ordering::Relaxed);
}

#[cfg(test)]
pub fn forget_degraded_for_test() {
	DEGRADED.lock().clear();
	// AND THE PUBLISHED-CLAIM FLAG WITH IT, because a test that calls `report` would otherwise leave
	// every later test's admission printing a retraction of a summary that belongs to another test.
	REPORTED.store(false, Ordering::Relaxed);
	RETRACTIONS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests;
