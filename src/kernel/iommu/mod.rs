// The `virtio-iommu` controller: discovery, negotiation, and the one transition that turns
// enforcement on.
//
// THE ORDER IS THE SECURITY PROPERTY. On this boot path the firmware needs untranslated DMA to read
// the boot medium, so the controller starts in bypass - anything else stops OVMF before the loader
// runs. What this module does is make the transition out of bypass a MEASURED one: quiesce every
// endpoint that is not the controller, disable its bus mastering, write bypass off, READ IT BACK,
// and only then let anything be attached and mapped. A failed transition is a refusal for every
// driver that declared it needs translation, never a quiet continuation.
//
// AND THE BOOTSTRAP EXCEPTION IS RESOLVED RATHER THAN WAIVED. The controller's own queues live in
// kernel frames that the controller reaches untranslated - it is the thing doing the translating,
// so it is not behind itself. Global bypass is NOT used to make that work: bypass is off before any
// other endpoint masters the bus, and the controller's own access is not an exception the profile
// grants, it is what the device is.

// TEST-ONLY, and that is the honest scope: `edu` has no driver, no device-table entry and no user
// but the conformance suite. A production kernel carrying a bounded arbitrary-DMA engine would be
// carrying the capability this milestone exists to remove.
#[cfg(test)]
pub mod edu;
mod virtqueue;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use dma::virtio_iommu::{Config, Transport, VirtioIommu};
use dma::{Fault, Generation};

use crate::sync::SpinLock;
use virtqueue::VirtQueue;

// The virtio device type the specification assigns to an IOMMU.
const VIRTIO_TYPE_IOMMU: u16 = 23;

// The two queues this device defines: requests, and the events it reports faults on.
const QUEUE_REQUEST: u16 = 0;
const QUEUE_EVENT: u16 = 1;

// Whether a controller was found and brought up. Read by the boot report; written once.
static PRESENT: AtomicBool = AtomicBool::new(false);

// The controller itself. One per machine: a second `virtio-iommu` would be a topology this fixture
// does not claim to handle, and it is refused rather than half-supported.
static CONTROLLER: SpinLock<Option<Controller>> = SpinLock::new(None);

// Which isolation domain each attached device index is in. One row per exclusive binding: a device
// with no row is a device that is not translated, and there is no shared default domain for one to
// fall into.
static DOMAINS: SpinLock<alloc::vec::Vec<(u32, dma::DomainId)>> = SpinLock::new(alloc::vec::Vec::new());

// What the transport needs to reach the device.
pub struct Wire {
	requests: VirtQueue,
	events: VirtQueue,
	// The direct-map address of the event buffer the device writes into.
	event_buffer: u64,
	event_physical: u64,
	// The descriptor the event buffer is outstanding on. A completion naming any other descriptor is
	// not this buffer coming back, and reading the buffer for it would be reading whatever is there.
	event_descriptor: Option<u16>,
}

impl Transport for Wire {
	fn request(&mut self, request: &[u8], tail: &mut [u8], status_at: usize) -> Result<(), Fault> {
		if request.len() > 2048 || tail.len() > 2048 {
			return Err(Fault::Malformed);
		}
		let scratch = self.requests.scratch_virtual();
		// SAFETY: the scratch frame is this queue's own, and the length is bounded above by the
		// check above against a page.
		unsafe {
			core::ptr::copy_nonoverlapping(request.as_ptr(), scratch as *mut u8, request.len());
			// The tail goes after the request, aligned enough for the device to write into.
			core::ptr::write_bytes((scratch + 2048) as *mut u8, 0, tail.len());
		}
		let physical = self.requests.scratch_physical();
		let Some(written) = self.requests.request(physical, request.len() as u32, physical + 2048, tail.len() as u32) else {
			// THE DEVICE DID NOT ANSWER. Not a failure of the operation - a failure to learn
			// anything about it, which is the one state a caller must never round down to success.
			return Err(Fault::Unconfirmed);
		};
		// AND A DEVICE THAT WROTE NOTHING DID NOT ANSWER EITHER.
		//
		// The scratch tail is zeroed above, and `VIRTIO_IOMMU_S_OK` is zero, so an answer the device
		// never wrote decoded as the device confirming the request. A completion carrying a length of
		// zero was accepted because the only length check was against writing too MUCH: the range was
		// closed at one end. Attach, map, unmap and detach all came back confirmed from a device that
		// completed the descriptor and touched no memory, and the kernel then recorded an isolation
		// nothing had established and freed IOVAs and frames on the strength of it.
		//
		// The status byte has to be INSIDE what the device wrote, which is why the offset is compared
		// rather than the length alone - a probe's status sits after its properties, so "some bytes
		// arrived" is not the same question as "the status arrived".
		if (written as usize) <= status_at {
			crate::serial_println!("iommu: the device completed request type {} without writing its status ({written} byte(s), status at {status_at}) - the operation is unconfirmed", request.first().copied().unwrap_or(0));
			return Err(Fault::Unconfirmed);
		}
		// SAFETY: as above; the device wrote into the second half of this queue's own frame.
		unsafe { core::ptr::copy_nonoverlapping((scratch + 2048) as *const u8, tail.as_mut_ptr(), tail.len()) };
		// A NON-ZERO STATUS IS NAMED WITH THE REQUEST THAT EARNED IT. Which request type the device
		// refused, and with which code, is the difference between "the kernel's own check refused
		// this" and "the device did" - and the two are indistinguishable from the typed fault alone.
		//
		// AT THE OFFSET THE CALLER GAVE, not at zero. This read `tail[0]`, which is the status only
		// while a request's device-writable part IS the tail; a probe's is not, so the first
		// successful probe printed "the device refused request type 5 with status 1" - the 1 being
		// the low byte of a property type.
		if tail.get(status_at).copied().unwrap_or(0) != 0 {
			crate::serial_println!("iommu: the device refused request type {} with status {}", request.first().copied().unwrap_or(0), tail[status_at]);
		}
		Ok(())
	}

	// WHAT THE DEVICE WROTE, AND ONLY THAT.
	//
	// This copied a full `FAULT_LEN` out of the event buffer whatever the device had put there, and
	// handed the same buffer back without clearing it. A report shorter than a whole record was
	// therefore completed with the tail of the PREVIOUS one, and a device that moved the used index
	// without writing anything had the driver decode a stale report as a fresh fault. The descriptor
	// id was not looked at either, so a completion for something else was read as this buffer coming
	// back.
	fn take_event(&mut self, out: &mut [u8]) -> usize {
		let Some((id, written)) = self.events.poll_used() else { return 0 };
		let expected = self.event_descriptor.take();
		// The buffer goes back either way: a completion this side cannot account for still means the
		// device has one fewer buffer to write into, and a fault queue with none is a fault queue.
		let hand_back = |wire: &mut Self| {
			// CLEARED BEFORE IT IS OFFERED. The next report may be shorter than this one, and what
			// is not overwritten is what was here.
			// SAFETY: this module's own frame, for exactly the length the device is given.
			unsafe { core::ptr::write_bytes(wire.event_buffer as *mut u8, 0, dma::virtio_iommu::FAULT_LEN) };
			wire.event_descriptor = wire.events.offer(wire.event_physical, dma::virtio_iommu::FAULT_LEN as u32);
		};
		// COMPARED WITHOUT TRUNCATING. A used-ring id is 32 bits on the wire and a descriptor index
		// is 16, so narrowing the device's number before comparing would let a bogus id alias onto
		// the one this side is waiting for.
		if expected.map(u32::from) != Some(id) {
			crate::serial_println!("iommu: the device completed descriptor {id} on the event queue and the fault buffer is on {expected:?} - the report is not read");
			hand_back(self);
			return 0;
		}
		let len = out.len().min(dma::virtio_iommu::FAULT_LEN).min(written as usize);
		// SAFETY: the event buffer is this module's own frame, and the length is bounded above by
		// both the caller's buffer and the record size the device was given.
		unsafe { core::ptr::copy_nonoverlapping(self.event_buffer as *const u8, out.as_mut_ptr(), len) };
		hand_back(self);
		len
	}
}

pub struct Controller {
	iommu: dma::Iommu<VirtioIommu<Wire>>,
	// The device's common configuration structure and its device-specific one, as direct-map
	// addresses.
	common: u64,
	device_config: u64,
}

unsafe fn read8(at: u64) -> u8 {
	unsafe { core::ptr::read_volatile(at as *const u8) }
}

unsafe fn write8(at: u64, value: u8) {
	unsafe { core::ptr::write_volatile(at as *mut u8, value) }
}

unsafe fn read32(at: u64) -> u32 {
	unsafe { core::ptr::read_volatile(at as *const u32) }
}

unsafe fn write32(at: u64, value: u32) {
	unsafe { core::ptr::write_volatile(at as *mut u32, value) }
}

// Read the device's configuration structure into bytes the portable parser can validate.
//
// THROUGH THE PARSER RATHER THAN FIELD BY FIELD, because the parser is where the validation lives
// and it is host-tested: an input range that ends before it starts is refused there, once, rather
// than by whichever field read happened to notice.
unsafe fn read_config(at: u64, len: usize) -> Vec<u8> {
	let mut bytes = Vec::new();
	if bytes.try_reserve(len).is_err() {
		return bytes;
	}
	for offset in 0..len {
		// SAFETY: the caller passes the device-specific configuration structure resolved from the
		// device's own PCI capabilities, and `len` is bounded by what it published.
		bytes.push(unsafe { read8(at + offset as u64) });
	}
	bytes
}

// Bring the controller up, if this machine has one.
//
// Returns whether translation is ENFORCING afterwards. Everything about that answer is measured:
// the feature bits the device offered, the configuration it published, and the bypass byte read
// back after it was written.
pub fn init() -> bool {
	let Some(index) = find_controller() else {
		return false;
	};
	// PRESENT IS A FACT ABOUT THE BUS, NOT ABOUT THE BRING-UP, and it is recorded HERE - at the point
	// the controller was found - rather than after it came up.
	//
	// It was stored in the success arm only, so a controller that failed feature negotiation, queue
	// creation or the bypass read-back left `present()` answering false. `dma_policy::init` reads
	// exactly that value to decide whether this machine was SUPPOSED to be isolated, so the one case
	// a protected driver must refuse - isolation was available and something went wrong with it -
	// became indistinguishable from a machine that never had a controller, and `virtio-net` bound
	// degraded instead of being refused. The line below announced that every such driver would be
	// refused, one statement before the code made that untrue.
	PRESENT.store(true, Ordering::Release);
	// ONE RETAINED SLOT PER DEVICE, before any binding can need one. `device::init` has already
	// resolved the table - `main` calls it first, and says so - so the size is known and this is the
	// only allocation the association ever needs.
	//
	// AND A TABLE THAT CANNOT BE SIZED REFUSES TRANSLATION RATHER THAN DEGRADING ITS ACCOUNTING
	// (corrected 2026-08-31). This used to print a line saying an unconfirmed detach would report
	// ZERO holdings for its device, and then bring the controller up anyway. That is the one number
	// an operator reads to find out whether a quarantined address space is still out of circulation,
	// and answering zero says the opposite of what quarantine means - on a controller this file
	// otherwise refuses to let anybody trust an unconfirmed answer from.
	//
	// So the allocation is a PREREQUISITE of translating at all. `PRESENT` stays set, which is what
	// makes this fail CLOSED: `dma_policy` reads it to mean "this machine was supposed to be
	// isolated", so every `iommu-required` driver is refused rather than silently admitted degraded.
	// The condition is a boot-time heap failure sizing one `Option` per device, which is to say it
	// does not happen - and if it ever does, losing the machine's DMA-capable drivers is the answer
	// this file gives everywhere else.
	{
		let mut retained = RETAINED.lock();
		retained.clear();
		if retained.try_reserve_exact(crate::device::count()).is_err() {
			crate::serial_println!("iommu: no room for the retained-domain table - translation is refused rather than run with accounting it cannot keep");
			return false;
		}
		retained.resize(crate::device::count(), None);
		let mut counts = RETAINED_FAULTS.lock();
		counts.clear();
		if counts.try_reserve_exact(crate::device::count()).is_err() {
			crate::serial_println!("iommu: no room for the retained fault counts - translation is refused rather than run with accounting it cannot keep");
			return false;
		}
		counts.resize(crate::device::count(), 0);
	}
	match bring_up(index) {
		Ok(controller) => {
			*CONTROLLER.lock() = Some(controller);
			crate::serial_println!("iommu: virtio-iommu is translating - bypass is off and read back as off");
			true
		}
		Err(reason) => {
			// A CONTROLLER THAT IS PRESENT AND DID NOT COME UP IS WORSE NEWS THAN NONE AT ALL: the
			// profile expected enforcement and does not have it, and every `iommu-required` driver
			// is about to be refused. Said loudly for that reason.
			crate::serial_println!("iommu: virtio-iommu present but NOT enforcing ({reason:?}) - every driver that requires translation will be refused");
			// AND IT IS LEFT INERT RATHER THAN HALF CONFIGURED. A bring-up can fail after the device
			// has been reset, acknowledged, given feature bits and had queues written into it, and
			// walking away there leaves a device that masters the bus with rings this kernel has
			// stopped tracking. The reset puts it back where the boot found it and the bus-master bit
			// goes with it.
			quiesce_controller(index);
			false
		}
	}
}

// Put a controller that did not come up back to where the boot found it: FAILED status, device
// reset, bus mastering off. Best effort by nature - the device is one that has already misbehaved -
// but it costs nothing and the alternative is a bus master nothing owns.
fn quiesce_controller(index: usize) {
	let Some((bar, common_offset, bus, dev, func)) = crate::device::with(index, |entry| (entry.bar_phys, entry.common_offset, entry.bus, entry.dev, entry.func)) else {
		return;
	};
	let common = crate::mem::hhdm_offset() + bar + common_offset as u64;
	// SAFETY: this device's own common configuration structure, mapped by `bring_up` before it
	// failed; the two writes are the specification's reset sequence and touch nothing else.
	unsafe {
		write8(common + abi::VIRTIO_CFG_DEVICE_STATUS, abi::VIRTIO_STATUS_FAILED);
		write8(common + abi::VIRTIO_CFG_DEVICE_STATUS, 0);
	}
	crate::arch::pci::set_bus_master(bus, dev, func, false);
	crate::serial_println!("iommu: the controller was reset and no longer masters the bus");
}

pub fn present() -> bool {
	PRESENT.load(Ordering::Acquire)
}

// Whether there is a controller to ASK, which is a different question from whether one is on the bus.
//
// `present` is about the machine: a controller was found, so this machine was supposed to be
// isolated, and a protected driver refuses when it is not. `translating` is about this boot: the
// controller came up and there is something to attach endpoints to and map buffers through. They
// were one value until a controller that failed bring-up was found to be indistinguishable from a
// machine that never had one - and the three callers that want to USE a controller want this one,
// while the policy that decides what a failure MEANS wants the other.
pub fn translating() -> bool {
	CONTROLLER.lock().is_some()
}

fn find_controller() -> Option<usize> {
	for index in 0..crate::device::count() {
		let is_iommu = crate::device::with(index, |entry| entry.device_type == VIRTIO_TYPE_IOMMU).unwrap_or(false);
		if is_iommu {
			return Some(index);
		}
	}
	None
}

// The controller's register window, reachable from the kernel.
//
// THE DIRECT MAP DOES NOT COVER MMIO, and assuming it does is a page fault on the direct `-kernel`
// boot path with no message attached. It happens to work under the UEFI loader, whose page tables
// are wider - which is the worst kind of working, because it makes the defect invisible in exactly
// the profile the gate boots. The window is mapped at its direct-map address, uncached, so the
// arithmetic everywhere else stays `hhdm + physical`.
fn map_registers(physical: u64, len: u64) -> u64 {
	use crate::arch::paging::{NO_CACHE, PRESENT, WRITABLE};
	use crate::mem::frame::PAGE_SIZE;
	let hhdm = crate::mem::hhdm_offset();
	if crate::mem::within_direct_map(physical, len) {
		return hhdm + physical;
	}
	let first = physical & !(PAGE_SIZE - 1);
	let last = (physical + len - 1) & !(PAGE_SIZE - 1);
	let mut at = first;
	while at <= last {
		crate::arch::paging::map_page(hhdm + at, at, PRESENT | WRITABLE | NO_CACHE);
		at += PAGE_SIZE;
	}
	hhdm + physical
}

fn bring_up(index: usize) -> Result<Controller, Fault> {
	let Some((bar, common_offset, device_offset, device_len, notify_offset, notify_multiplier, bus, dev, func)) = crate::device::with(index, |entry| (entry.bar_phys, entry.common_offset, entry.device_offset, entry.device_len, entry.notify_offset, entry.notify_multiplier, entry.bus, entry.dev, entry.func)) else {
		return Err(Fault::Unconfirmed);
	};
	// A device whose configuration structure it did not publish is not one this can validate.
	if device_len == 0 {
		return Err(Fault::Malformed);
	}
	// The whole BAR at once: the common configuration structure, the notify region and the
	// device-specific structure all live inside it, and mapping it in one go keeps the three
	// derived addresses plain arithmetic.
	let (bar_len, hhdm) = (crate::device::with(index, |entry| entry.bar_len).unwrap_or(0), crate::mem::hhdm_offset());
	if bar_len == 0 {
		return Err(Fault::Malformed);
	}
	map_registers(bar, bar_len);
	let common = hhdm + bar + common_offset as u64;
	let device_config = hhdm + bar + device_offset as u64;

	// THE CONTROLLER MASTERS THE BUS BEFORE ITS QUEUES EXIST, because the queues are memory it must
	// reach. It is the one endpoint for which that is not a hole: it is the device doing the
	// translating.
	crate::arch::pci::set_bus_master(bus, dev, func, true);

	// SAFETY: `common` is this device's common configuration structure, resolved from its own PCI
	// capabilities by the boot scan.
	let offered = unsafe {
		write8(common + abi::VIRTIO_CFG_DEVICE_STATUS, 0);
		let mut spins = 0u64;
		while read8(common + abi::VIRTIO_CFG_DEVICE_STATUS) != 0 && spins < 1_000_000 {
			spins += 1;
		}
		write8(common + abi::VIRTIO_CFG_DEVICE_STATUS, abi::VIRTIO_STATUS_ACKNOWLEDGE);
		write8(common + abi::VIRTIO_CFG_DEVICE_STATUS, abi::VIRTIO_STATUS_ACKNOWLEDGE | abi::VIRTIO_STATUS_DRIVER);
		write32(common + abi::VIRTIO_CFG_DEVICE_FEATURE_SELECT, 0);
		let low = read32(common + abi::VIRTIO_CFG_DEVICE_FEATURE) as u64;
		write32(common + abi::VIRTIO_CFG_DEVICE_FEATURE_SELECT, 1);
		let high = read32(common + abi::VIRTIO_CFG_DEVICE_FEATURE) as u64;
		low | (high << 32)
	};
	// VIRTIO_F_VERSION_1 is bit 32 and is not optional: a legacy device would place its structures
	// somewhere else entirely.
	if offered & (1 << 32) == 0 {
		return Err(Fault::Unconfirmed);
	}
	// ONLY WHAT IS IMPLEMENTED IS ACKNOWLEDGED. The decision is the portable one, and it refuses a
	// device that does not offer what an enforcing profile needs.
	let accepted = dma::virtio_iommu::negotiate(offered)?;
	// SAFETY: as above.
	unsafe {
		write32(common + abi::VIRTIO_CFG_DRIVER_FEATURE_SELECT, 0);
		write32(common + abi::VIRTIO_CFG_DRIVER_FEATURE, accepted as u32);
		write32(common + abi::VIRTIO_CFG_DRIVER_FEATURE_SELECT, 1);
		write32(common + abi::VIRTIO_CFG_DRIVER_FEATURE, ((accepted >> 32) as u32) | 1);
		write8(common + abi::VIRTIO_CFG_DEVICE_STATUS, abi::VIRTIO_STATUS_ACKNOWLEDGE | abi::VIRTIO_STATUS_DRIVER | abi::VIRTIO_STATUS_FEATURES_OK);
		if read8(common + abi::VIRTIO_CFG_DEVICE_STATUS) & abi::VIRTIO_STATUS_FEATURES_OK == 0 {
			return Err(Fault::Unconfirmed);
		}
	}

	// The configuration, validated by the portable parser rather than trusted field by field.
	// SAFETY: the device published this structure's offset and length in its own capabilities.
	let config_bytes = unsafe { read_config(device_config, device_len as usize) };
	let config = Config::parse(&config_bytes)?;

	let notify_base = hhdm + bar + notify_offset as u64;
	let mut requests = VirtQueue::create(common, QUEUE_REQUEST, notify_base, notify_multiplier, 16).ok_or(Fault::NoSpace)?;
	let mut events = VirtQueue::create(common, QUEUE_EVENT, notify_base, notify_multiplier, 16).ok_or(Fault::NoSpace)?;
	let event_physical = events.scratch_physical();
	let event_buffer = events.scratch_virtual();
	// SAFETY: the queue's own scratch frame, cleared before the device is given it.
	unsafe { core::ptr::write_bytes(event_buffer as *mut u8, 0, dma::virtio_iommu::FAULT_LEN) };
	let event_descriptor = events.offer(event_physical, dma::virtio_iommu::FAULT_LEN as u32);
	// SAFETY: the common configuration structure, as above.
	unsafe { write8(common + abi::VIRTIO_CFG_DEVICE_STATUS, abi::VIRTIO_STATUS_ACKNOWLEDGE | abi::VIRTIO_STATUS_DRIVER | abi::VIRTIO_STATUS_FEATURES_OK | abi::VIRTIO_STATUS_DRIVER_OK) };
	let _ = &mut requests;

	// EVERY OTHER ENDPOINT STOPS MASTERING THE BUS BEFORE BYPASS GOES OFF, and that ordering is the
	// point: an endpoint still mastering when translation turns on is an endpoint whose in-flight
	// DMA lands wherever it was already aimed. Quiescing them first makes the transition a moment
	// with no traffic across it rather than one that races whatever the firmware left running.
	quiesce_other_endpoints(index);

	// The transition, and the read-back that is the only reason to believe it.
	if accepted & dma::virtio_iommu::F_BYPASS_CONFIG == 0 {
		// Without the feature the bypass byte is not writable, so the profile cannot be made
		// enforcing at all. Refused rather than continued: continuing is untranslated DMA with a
		// controller present to make it look otherwise.
		return Err(Fault::Unconfirmed);
	}
	// SAFETY: the device-specific configuration structure, whose `bypass` byte is at offset 36 and
	// is driver-writable exactly when `F_BYPASS_CONFIG` was negotiated.
	let bypass = unsafe {
		write8(device_config + 36, 0);
		read8(device_config + 36)
	};
	if bypass != 0 {
		return Err(Fault::Unconfirmed);
	}

	let wire = Wire { requests, events, event_buffer, event_physical, event_descriptor };
	// NO GENERATION HERE ANY MORE. The backend used to be built with one and keep it as a
	// controller-wide value; binding identity belongs to the domain, which `attach_endpoint` creates
	// with the claim's generation.
	let backend = VirtioIommu::new(wire, config, accepted)?;
	Ok(Controller { iommu: dma::Iommu::new(backend, 64), common, device_config })
}

// Stop every endpoint that is not the controller from mastering the bus.
//
// INCLUDING THE FIRMWARE'S. OVMF read the boot medium through a SATA function with bus mastering on
// and bypass on; that function has no driver in this kernel and no reason to keep the ability. This
// is where it loses it.
fn quiesce_other_endpoints(controller: usize) {
	let keep = crate::device::with(controller, |entry| (entry.bus, entry.dev, entry.func));
	// THE WHOLE BUS, NOT THE DRIVER TABLE. The device table holds the functions this kernel binds
	// drivers to; the firmware's SATA controller is not one of them and is exactly the endpoint
	// that was mastering the bus a moment ago. `pci_get` walks every function the boot scan found.
	for index in 0..crate::device::pci_count() {
		let Some(function) = crate::device::pci_get(index) else { continue };
		if Some((function.bus, function.dev, function.func)) == keep {
			continue;
		}
		crate::arch::pci::set_bus_master(function.bus, function.dev, function.func, false);
	}
}

impl Controller {
	// The manager, for the binding lifecycle above.
	pub fn iommu(&mut self) -> &mut dma::Iommu<VirtioIommu<Wire>> {
		&mut self.iommu
	}

	// Whether the device still reports bypass as off. Read rather than remembered: a device that
	// changed its own configuration under the kernel is exactly the case a remembered answer misses.
	pub fn still_enforcing(&self) -> bool {
		// SAFETY: this controller's own device-specific configuration structure.
		unsafe { read8(self.device_config + 36) == 0 }
	}

	// The device's status register, so a controller that has entered its NEEDS_RESET state is
	// noticed rather than driven.
	pub fn healthy(&self) -> bool {
		// SAFETY: this controller's own common configuration structure.
		let status = unsafe { read8(self.common + abi::VIRTIO_CFG_DEVICE_STATUS) };
		status & abi::VIRTIO_STATUS_DRIVER_OK != 0 && status & 0x40 == 0
	}
}

// The requester identity a PCI function puts on the bus.
//
// DERIVED FROM THE KERNEL'S OWN PCI IDENTITY, never from a userspace-supplied device index - which
// is the difference between "this endpoint" and "whichever endpoint the caller named". This first
// topology contains direct-root-port functions only; a bridge that aliases its children's requester
// ids is a different rule, and one this fixture refuses rather than guesses at.
pub fn requester_of(bus: u8, dev: u8, func: u8) -> dma::EndpointId {
	dma::EndpointId(((bus as u32) << 8) | ((dev as u32) << 3) | func as u32)
}

// Bring one device's endpoint under translation: a domain of its own, attached and confirmed.
//
// ONE DOMAIN PER EXCLUSIVE BINDING. Two devices sharing a domain share every mapping in it, which
// makes "endpoint A cannot reach endpoint B's page" false by construction. `generation` is the
// binding's, so a reused slot's mappings are stale by arithmetic rather than by bookkeeping.
pub fn attach_endpoint(bus: u8, dev: u8, func: u8, generation: u64) -> Result<dma::DomainId, Fault> {
	let endpoint = requester_of(bus, dev, func);
	with(|controller| {
		let config = *controller.iommu().backend().config();
		let iommu = controller.iommu();
		// THE ENDPOINT IS ASKED BEFORE IT IS ATTACHED, and the domain is built from the answer.
		//
		// This ran the other way round - create, attach, then probe - under a comment saying the
		// device "cannot be asked before the endpoint exists". It can: a PROBE names a requester id,
		// which a function on the bus has whether or not it belongs to a domain. Asking afterwards
		// meant the address space was created whole and the reserved holes arrived too late to be
		// carved out of it, so they were printed instead - and the allocator could hand a device an
		// address inside a range the device itself had declared unusable.
		let regions = match iommu.probe(endpoint) {
			Ok(regions) => regions,
			Err(reason) => {
				// AN ENDPOINT WHOSE HOLES ARE UNKNOWN IS NOT ATTACHED. This used to be a warning
				// after the fact, with the endpoint already translating; now there is nothing to
				// warn about, because nothing has been built yet.
				crate::serial_println!("iommu: endpoint {:#x} could not be probed ({reason:?}) - its reserved regions are unknown, so it is not attached", endpoint.0);
				return Err(reason);
			}
		};
		let mut reserved: alloc::vec::Vec<dma::Reserved> = alloc::vec::Vec::new();
		// ALLOC-OK: one entry per reserved region this endpoint published, at binding time.
		if reserved.try_reserve(regions.len()).is_err() {
			return Err(Fault::NoSpace);
		}
		for region in regions.iter().filter(|r| r.kind == dma::RegionKind::Reserved) {
			crate::serial_println!("iommu: endpoint {:#x} reserves {:#x}+{:#x} - its domain never allocates there", endpoint.0, region.base, region.len);
			reserved.push(dma::Reserved { base: region.base, len: region.len });
		}
		let domain = iommu.create_domain(config.input_start, config.input_len(), reserved, Generation(generation))?;
		// A FAILED ATTACH TAKES ITS DOMAIN WITH IT. This returned on the `?` with the domain created,
		// so every refused bind left one behind and consumed an id - and `next_domain` only advances.
		if let Err(reason) = iommu.attach(domain, endpoint) {
			let _ = iommu.destroy_domain(domain);
			return Err(reason);
		}
		// THE DOORBELL IS NOT MAPPED HERE, AND THAT IS THE FIX RATHER THAN THE OMISSION.
		//
		// Three shapes were tried. Mapping it here and IGNORING a failure enabled bus mastering for a
		// binding already known not to receive interrupts - silently. Mapping it here and FAILING THE
		// ATTACH is too blunt, and was measured to be: most drivers in this tree POLL and never
		// acquire an MSI vector at all, so refusing their attach denies translation to devices the
		// doorbell is irrelevant to, and the boot came up with one endpoint attached and no network.
		// Mapping it here and RECORDING the failure for a later refusal - what this did - fixes the
		// silence and leaves a map failure ending in a published binding, which the milestone rule
		// does not allow.
		//
		// The dilemma is created by mapping it EAGERLY for an endpoint that may never want one. So it
		// is mapped LAZILY, by `SYS_DEVICE_MSIX_ACQUIRE`, at the moment an interrupt is actually
		// asked for and before MSI-X is enabled on the device. A polling driver asks for no doorbell
		// and no map can fail; a driver that wants an interrupt gets the map or gets a refusal, and a
		// refusal there IS a refused binding - it is handed no vector and the device is never allowed
		// to raise one. Nothing is published on a failed map, and no endpoint is left translated with
		// a route nobody checked.
		Ok(domain)
	})
	.unwrap_or(Err(Fault::Unconfirmed))
}

// HOW MANY TRANSLATIONS THIS DEVICE'S DOMAIN STILL HOLDS - live and quarantined together, because a
// quarantined one is charged exactly like a live one: nobody knows the device has stopped resolving
// it. Read by the claim snapshot so a manager that did not make the binding can reconstruct the
// charge. A device that is not translated holds none, which is the honest answer rather than zero
// standing in for "no controller".
pub fn grants_for(index: u32) -> usize {
	let Some(domain) = domain_of(index).or_else(|| retained_domain_of(index)) else { return 0 };
	with(|controller| controller.iommu().mappings_in(domain)).unwrap_or(0)
}

// HOW MANY OF THEM ARE QUARANTINED - a mapping whose invalidation the device never confirmed.
//
// The counter existed and was read by the DMA crate's own unit tests and by nothing else, so the
// production lifecycle carried a total that could not be broken down. This is the half that decides
// whether a released claim's address space may be reused: a quarantined range is out of circulation
// for the life of the boot, and a manager that cannot see one cannot reconstruct that fact.
pub fn quarantined_grants_for(index: u32) -> usize {
	let Some(domain) = domain_of(index).or_else(|| retained_domain_of(index)) else { return 0 };
	with(|controller| controller.iommu().quarantined_addresses(domain)).unwrap_or(0)
}

// AND HOW MANY FAULTS THIS BINDING RAISED. The fourth of the four counters M4 names - mapping, IOVA,
// endpoint and fault - and the one the claim snapshot did not carry.
//
// A controller-wide total cannot answer "did this restart come back to its baseline" for one device:
// a driver that crashes, is rebound and faults again adds to the same number as the device beside it.
// The count lives with the DOMAIN, which is the binding, so a retained domain still answers for the
// binding that left it behind.
pub fn faults_for(index: u32) -> u64 {
	// THE LIVE DOMAIN, THEN THE RETAINED ONE, THEN THE RETAINED COUNT. The third is what answers
	// after a CONFIRMED teardown, whose domain is destroyed on purpose - see `RETAINED_FAULTS`.
	match domain_of(index).or_else(|| retained_domain_of(index)) {
		Some(domain) => with(|controller| controller.iommu().faults_in(domain)).unwrap_or(0),
		None => RETAINED_FAULTS.lock().get(index as usize).copied().unwrap_or(0),
	}
}

// DEVICES WHOSE DETACH WAS NOT CONFIRMED, AND THE DOMAIN THAT OUTLIVED THEM.
//
// `detach_for_inner` removes the live `DOMAINS` row before it revokes, because after the revoke the
// device is no longer translated and must not read as though it were. On the UNCONFIRMED path the
// backend's domain and its quarantined mappings are deliberately retained - nobody knows whether the
// device can still reach them - and the two facts together meant `grants_for` and
// `quarantined_grants_for` answered ZERO for exactly the binding whose holdings matter most: a
// manager reconstructing the charge saw a claim with no IOMMU holdings and quarantined pages behind
// it. This list is the association that survives the row, and nothing else reads it: a retained
// domain is not a live one, so admission, `msi_deliverable` and `domain_of` are untouched by it.
// ONE SLOT PER DEVICE, SIZED ONCE AT BRING-UP, so recording the association cannot fail.
//
// It was a growable list with a `try_reserve` in front of it, and the failure arm kept the
// quarantine and LOST the association - so an unconfirmed detach under memory pressure left a device
// whose claim reports zero IOMMU holdings while its address space is permanently out of circulation.
// M4 asks for an exact post-teardown baseline, and "exact unless the heap was short at the wrong
// moment" is not one. The table is the device table's shape and is filled at `init`, which runs
// after `device::init` has resolved it.
static RETAINED: SpinLock<Vec<Option<dma::DomainId>>> = SpinLock::new(Vec::new());

fn retained_domain_of(index: u32) -> Option<dma::DomainId> {
	RETAINED.lock().get(index as usize).copied().flatten()
}

// THE FAULT COUNT OF THE BINDING THAT LAST HELD THIS DEVICE, kept after its domain is gone (added
// 2026-09-01).
//
// The count lives with the DOMAIN, which is the binding - that is the right home and it is what M4
// asks for. But a CONFIRMED teardown destroys the domain, and destroying it takes the counter with
// it: the attributed drain that runs just before it correctly charges the binding for the faults its
// own revoke raised, and then `destroy_domain` removes the `DomainState` those faults were charged
// to. So `faults_for` answered zero for exactly the faults the drain was moved earlier to capture -
// the reorder fixed the durable EVENT attribution and left the exposed per-binding COUNT unchanged.
//
// A retained association does not help, because a destroyed domain answers zero however it is
// reached. What survives is a number, so a number is what is kept: the domain's final count, copied
// out while the domain still answers and read after it does not. Sized with `RETAINED`, at `init`,
// for the reason stated there.
static RETAINED_FAULTS: SpinLock<Vec<u64>> = SpinLock::new(Vec::new());

// What asking for an MSI vector found. THREE ANSWERS, because the two failures call for opposite
// responses: an endpoint whose domain has no route is a live binding to disable, and one whose
// domain belongs to a later binding is a caller to refuse without touching the device at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MsiRoute {
	// Either the endpoint is not translated, or its domain now carries the doorbell mapping.
	Deliverable,
	// The endpoint is translated and its interrupts have nowhere to land.
	NoRoute,
	// The device is claimed by a DIFFERENT binding than the caller's. Nothing was done to it.
	Stale,
}

// Whether an MSI vector for this device can actually be delivered: either it is not translated at all,
// or its domain carries a mapping for the doorbell its interrupts are written to.
// MAP THE DOORBELL FOR THIS DEVICE, NOW THAT AN INTERRUPT IS BEING ASKED FOR.
//
// `true` for an endpoint this controller does not translate: its interrupts do not pass through a
// domain, so there is nothing to map and nothing that could fail. Otherwise the doorbell region is
// mapped into the endpoint's domain and the answer is whether it is now reachable.
//
// IDEMPOTENT, because a driver may acquire, release and acquire again across one binding: mapping a
// region that is already mapped answers `AlreadyMapped`, which is the same success as mapping it.
pub fn msi_deliverable(index: u32, generation: u64, bus: u8, dev: u8, func: u8) -> MsiRoute {
	let domain = match domain_for_generation(index, generation) {
		Ok(Some(domain)) => domain,
		// AN UNTRANSLATED ENDPOINT IS STILL CHECKED, AGAINST THE CLAIM (2026-08-31).
		//
		// `Ok(None)` says "this device has no domain", and this arm read it as "the caller is
		// current" - which is a different statement and was never verified. On a machine with no
		// IOMMU, or for any endpoint the controller does not translate, that made the whole
		// generation defence inapplicable: a release and a reclaim during the call left the stale
		// caller programming the REPLACEMENT binding's MSI-X entry, and only `register_derived`
		// noticed - after the hardware effect. The claim slot is the authority that can answer for a
		// device with no domain, and the comparison is the same arithmetic one.
		Ok(None) if crate::device::claim_is_current(abi::ClaimKey { device_index: index, _pad: 0, generation }) => return MsiRoute::Deliverable,
		Ok(None) => return MsiRoute::Stale,
		// THE DOMAIN UNDER THIS INDEX IS SOMEBODY ELSE'S. Nothing is mapped and nothing is disabled:
		// the caller's own binding has ended, and acting on the device now would be this generation
		// reaching into the next one. See `domain_for_generation`.
		Err(_) => return MsiRoute::Stale,
	};
	let endpoint = requester_of(bus, dev, func);
	let installed = with(|controller| {
		let iommu = controller.iommu();
		let regions = iommu.probe(endpoint).map_err(|_| ())?;
		install_doorbell(iommu, domain, endpoint, &regions).map_err(|_| ())
	});
	match installed {
		Some(Ok(())) => MsiRoute::Deliverable,
		_ => {
			crate::serial_println!("iommu: domain {} has no route for its interrupts - no MSI vector is handed out for it", domain.0);
			MsiRoute::NoRoute
		}
	}
}

// Whether the doorbell fact has been printed. Every endpoint on a machine writes the same one.
static DOORBELL_REPORTED: AtomicBool = AtomicBool::new(false);

// Map the doorbell this endpoint's interrupts are written to.
//
// AN INTERRUPT IS A MEMORY WRITE, and this is the function that fact costs. A translated endpoint
// puts its MSI on the bus like any other write, so the doorbell goes through the same domain as its
// DMA - and a domain with no mapping for it drops the interrupt. Nothing faults, because a write the
// IOMMU cannot translate is not a request the device asked a question about; nothing is logged; the
// driver simply waits for an interrupt that was never delivered.
//
// It was measured rather than reasoned about: with a `virtio-iommu` in the machine, `virtio-net`
// transmitted, the host answered, and the guest never saw a packet. QEMU's own trace showed one
// unmatched translation in the whole boot - `addr=0xfee00000`, the x86 MSI doorbell.
//
// A FAILURE HERE IS A REFUSAL, and it did not used to be.
//
// The comment here said the failure "is not fatal and is not silent": the endpoint was attached and
// translating, and what it lost was interrupt delivery - "a driver that does not work rather than a
// driver that reaches memory it should not". That reasoning is about MEMORY SAFETY and the rule it
// was measured against is about BINDINGS: a map failure ends in refusal, disabled bus mastering or
// quarantine. Reporting success meant `device::claim` went on to enable bus mastering for a binding
// already known not to receive interrupts - a device that can write to memory and cannot tell anyone
// it did. The caller now rolls the domain back and the claim fails, which is the first of the three.
fn install_doorbell(iommu: &mut dma::Iommu<VirtioIommu<Wire>>, domain: dma::DomainId, endpoint: dma::EndpointId, regions: &[dma::ProbedRegion]) -> Result<(), Fault> {
	let mut mapped = false;
	for region in regions.iter().filter(|r| r.kind == dma::RegionKind::MsiDoorbell) {
		// THE DEVICE WRITES ITS DOORBELL. `ToDevice` is the direction that reads, and mapping it
		// that way produced a translation the interrupt controller's address had - and that
		// refused the one access anybody makes to it. QEMU's trace named it exactly:
		// `virt_start=0xfee00000 ... flags=1`, a read-only mapping, and the MSI arriving as
		// `flag=2`.
		// ALREADY INSTALLED IS ALREADY DONE. The doorbell is mapped when a vector is ASKED FOR, so a
		// driver that acquires, releases and acquires again within one binding arrives here twice for
		// the same range - and `map_identity` refuses a duplicate with `Overlaps`, which this read as
		// "no route for its interrupts" and turned into a refused second acquire. Asking first is
		// what makes the lazy install idempotent; `map_identity` keeps its refusal for every other
		// caller.
		if iommu.identity_mapped(domain, region.base, region.len, dma::Direction::FromDevice) {
			mapped = true;
			continue;
		}
		match iommu.map_identity(domain, region.base, region.len, dma::Direction::FromDevice) {
			// SAID ONCE, NOT ONCE PER DOMAIN. Every endpoint on this machine writes the same
			// doorbell, so a line per domain is eleven copies of one fact - the scattering the
			// boot report keeps being cleaned of. A failure is per-domain and is always printed:
			// that one IS about this endpoint.
			Ok(_) => {
				mapped = true;
				if !DOORBELL_REPORTED.swap(true, Ordering::AcqRel) {
					crate::serial_println!("iommu: every domain carries the MSI doorbell at {:#x}+{:#x}, mapped one to one - a device's interrupt is a memory write and goes through its own domain", region.base, region.len);
				}
			}
			Err(reason) => {
				crate::serial_println!("iommu: domain {} could not map its MSI doorbell at {:#x}+{:#x} ({reason:?}) - this endpoint's interrupts could not be delivered", domain.0, region.base, region.len);
				return Err(reason);
			}
		}
	}
	// AN ENDPOINT THAT REPORTED NO DOORBELL STILL HAS ONE.
	//
	// A device that does not offer `F_PROBE`, or offers it and lists no MSI region, left this
	// function with nothing to do - and the endpoint then lost every interrupt for exactly the
	// reason above, silently, which is the failure the mapping was added to fix. The platform's own
	// doorbell is the fallback, and it is mapped the same way: one to one, written by the device.
	if !mapped {
		let Some((base, len)) = crate::arch::pci::msi_doorbell() else {
			// NOTHING TO MAP AND NOTHING TO REFUSE. A port that names no doorbell is one where an
			// interrupt is not a memory write at all, so there is no translation this endpoint is
			// missing - unlike the branches above, where one was needed and could not be made.
			crate::serial_println!("iommu: endpoint {:#x} reported no MSI doorbell and this port names none", endpoint.0);
			return Ok(());
		};
		// ALREADY INSTALLED IS ALREADY DONE, on this branch as much as on the probed one above.
		// The guard was added where the endpoint REPORTS its doorbell and not here, so an endpoint
		// on the platform fallback - one offering no `F_PROBE`, or probing and listing no MSI region
		// - still met `Overlaps` on its second acquire and lost its vector for it. The two branches
		// map the same kind of region for the same reason and are idempotent on the same terms.
		if iommu.identity_mapped(domain, base, len, dma::Direction::FromDevice) {
			return Ok(());
		}
		match iommu.map_identity(domain, base, len, dma::Direction::FromDevice) {
			Ok(_) => {
				if !DOORBELL_REPORTED.swap(true, Ordering::AcqRel) {
					crate::serial_println!("iommu: every domain carries this port's MSI doorbell at {base:#x}+{len:#x}, mapped one to one - the endpoint reported none of its own");
				}
			}
			Err(reason) => {
				crate::serial_println!("iommu: domain {} could not map this port's MSI doorbell at {base:#x}+{len:#x} ({reason:?}) - this endpoint's interrupts could not be delivered", domain.0);
				return Err(reason);
			}
		}
	}
	Ok(())
}

// Install one translation for a device that is behind the IOMMU, and hand back the address the
// DEVICE will use. The physical address never leaves this function.
pub fn map_for_device(domain: dma::DomainId, physical: u64, len: u64, direction: dma::Direction) -> Result<(dma::MappingId, dma::DmaAddress), Fault> {
	with(|controller| {
		let config = *controller.iommu().backend().config();
		// The device's own smallest page is the alignment, and it is a power of two by construction -
		// `smallest_page` derives it from the bitmap the device published. A device that published none
		// leaves nothing to map through.
		let Ok(requirements) = dma::Requirements::new(64, config.smallest_page(), 1, true) else {
			return Err(Fault::Unconfirmed);
		};
		let iommu = controller.iommu();
		let id = match iommu.map(domain, physical, len, direction, &requirements) {
			Ok(id) => id,
			Err(reason) => {
				// SAID WITH ITS NUMBERS. A refused mapping is a security-relevant refusal and the
				// interesting part is always which constraint refused it - the device's page size,
				// its input range, or the device itself.
				crate::serial_println!("iommu: map refused ({reason:?}) domain {} physical {:#x} len {:#x} - device pages {:#x}, input {:#x}..={:#x}", domain.0, physical, len, config.smallest_page(), config.input_start, config.input_end);
				return Err(reason);
			}
		};
		let address = iommu.address_of(id).ok_or(Fault::NotMapped)?;
		Ok((id, address))
	})
	.unwrap_or(Err(Fault::Unconfirmed))
}

// Bring the device at `index` under translation, recording the domain it now belongs to.
//
// Returns whether the endpoint is attached and confirmed. A device that is already attached is
// already attached - the count above this is what decides when that happens, and asking twice is
// not an error.
pub fn attach_for(index: usize, bus: u8, dev: u8, func: u8, generation: u64) -> bool {
	if domain_of(index as u32).is_some() {
		return true;
	}
	// THE GENERATION IS THE BINDING'S, and it now arrives as one. This passed a hardcoded `1` under a
	// comment saying binding identity was the driver lifecycle's to own and that the constant was a
	// weaker token stated as such. The lifecycle owns it now: the caller is `device::claim`, the
	// number is the claim's, and a mapping made under a previous binding of this device is stale by
	// arithmetic rather than by bookkeeping - which is the fault `StaleGeneration` was written for
	// and could not reach while every binding was generation 1.
	match attach_endpoint(bus, dev, func, generation) {
		Ok(domain) => {
			let mut domains = DOMAINS.lock();
			// ALLOC-OK: one row per device the boot scan resolved, and this is a binding transition
			// rather than an interrupt.
			//
			// AND A ROW THAT CANNOT BE RECORDED UNDOES THE ATTACH. This returned `false` and walked
			// away, leaving the endpoint attached in the hardware to a domain nothing knew about -
			// so `domain_of` answered `None`, the next attempt built a SECOND domain for the same
			// endpoint, and the first was unreachable for the life of the boot. There is one state
			// worse than failing to attach, and it is attaching and forgetting.
			if domains.try_reserve(1).is_err() {
				drop(domains);
				let endpoint = requester_of(bus, dev, func);
				let undone = matches!(with(|controller| controller.iommu().revoke_endpoint(domain, endpoint)), Some(Ok(dma::Release::FramesReusable)));
				// AND THE DOMAIN GOES WITH IT, on the same rule as the ordinary detach: a rollback
				// that revoked the endpoint cleanly has nothing left to keep the domain for, and one
				// that did not leaves it standing. Without this the failed attach left a domain
				// behind exactly as the normal path did - and this is the path taken when memory is
				// already short, which is the worst moment to start leaking domain IDs.
				if undone {
					let _ = with(|controller| controller.iommu().destroy_domain(domain));
				}
				crate::serial_println!("iommu: {:02x}:{:02x}.{} attached and its row could not be recorded - the attach was {}", bus, dev, func, if undone { "undone" } else { "NOT undone, and this endpoint is translating under a domain nothing tracks" });
				return false;
			}
			domains.push((index as u32, domain));
			// ONE LINE PER ENDPOINT, not one per mapping. Which devices are translated is the state
			// a reader wants; how many buffers each of them has is noise that would bury it.
			crate::serial_println!("iommu: {:02x}:{:02x}.{} attached to domain {}", bus, dev, func, domain.0);
			true
		}
		Err(reason) => {
			crate::serial_println!("iommu: {:02x}:{:02x}.{} could not be attached ({reason:?}) - it does not master the bus", bus, dev, func);
			false
		}
	}
}

// The device is done. Everything it could reach stops being reachable, or is quarantined.
// ANSWERS WHETHER THE TEARDOWN WAS CONFIRMED, because the claim's terminal state is that answer.
// This returned nothing and printed the unconfirmed case, so a caller had no way to tell a device
// that is quiet from one whose mappings may still be live - and the release above has to distinguish
// them: a confirmed teardown frees the slot, an unconfirmed one quarantines it.
pub fn detach_for(index: usize, bus: u8, dev: u8, func: u8) -> bool {
	// A SHORTER WAIT THAN THE BOOT'S, because this one runs inside DeviceManager's event loop.
	//
	// `Claim::release` does not return until this does, and the manager's single loop supervises
	// every other device behind it - so a controller that has stopped answering could hold the whole
	// control plane for the boot-time budget. The budget is restored before returning, on every
	// path, because the boot's own attaches must keep the longer one.
	let previous_budget = virtqueue::set_spin_budget(virtqueue::TEARDOWN_SPINS);
	// AND A WALL-CLOCK BOUND WITH IT, which the spin count is not.
	//
	// A million iterations is a duration only on a machine whose speed you already know; on an
	// emulated target under load it is an unstated one. The manager's loop is what waits behind this,
	// so the bound it needs is in the units it budgets in - ticks. Whichever expires first ends the
	// wait, and either expiry is `Unconfirmed`, which quarantines the claim rather than freeing
	// anything. Restored on every path, like the budget, because the boot's own attaches must not
	// inherit a deadline that has already passed.
	let previous_deadline = virtqueue::set_deadline(crate::arch::apic::ticks().saturating_add(virtqueue::TEARDOWN_TICKS));
	let confirmed = detach_for_inner(index, bus, dev, func);
	virtqueue::set_deadline(previous_deadline);
	virtqueue::set_spin_budget(previous_budget);
	confirmed
}

fn detach_for_inner(index: usize, bus: u8, dev: u8, func: u8) -> bool {
	// NOTHING ATTACHED IS NOTHING TO CONFIRM, and that is a success rather than a failure: a device
	// this controller never translated has no mapping that could outlive its binding.
	let Some(domain) = domain_of(index as u32) else { return true };
	// DRAINED BEFORE THE REVOKE, because after it there is nothing left to attribute a fault TO.
	//
	// This ran only at the end, and by then the endpoint had been removed from `DOMAINS` and revoked -
	// so `VirtioIommu::drain_faults` could no longer resolve a queued endpoint to its domain and
	// reported `DomainId(0)`, with `Generation(0)` behind it. That is the teardown path, which is
	// exactly where a fault most needs attributing: a device faulting as its binding ends is the case
	// the containment policy is for. Drained again afterwards, because the revoke itself can produce
	// one.
	// The generation this binding was made at, captured while the domain still answers - it is what
	// the post-revoke drain stamps on a fault the backend can no longer attribute.
	let generation = with(|controller| controller.iommu().generation_of(domain)).flatten();
	let endpoint = requester_of(bus, dev, func);
	poll_faults();
	DOMAINS.lock().retain(|(device, _)| *device != index as u32);
	let confirmed = match revoke_endpoint(domain, bus, dev, func) {
		Ok(dma::Release::FramesReusable) => true,
		other => {
			crate::serial_println!("iommu: {:02x}:{:02x}.{} was not confirmed detached ({other:?}) - its pages stay quarantined", bus, dev, func);
			// AND THE ASSOCIATION IS KEPT SO THE HOLDINGS STAY REPORTABLE. The live row is gone -
			// this device is not translated any more - but its domain and its quarantined mappings
			// are not, and a claim snapshot that answered zero for them would say the opposite of
			// what quarantine means. ALLOC-OK: one entry per unconfirmed detach, which is a
			// per-binding event and not a hot path; a short heap loses the association rather than
			// the quarantine, and says so.
			let mut retained = RETAINED.lock();
			match retained.get_mut(index) {
				Some(slot) => *slot = Some(domain),
				// UNREACHABLE BY CONSTRUCTION, and kept as a refusal rather than deleted. `init`
				// now refuses to translate at all unless it could size one slot per device, and the
				// device table does not grow after boot - so reaching this means one of those two
				// facts stopped being true, and a lost association is a device reporting zero
				// holdings while its address space is out of circulation.
				None => crate::serial_println!("iommu: no slot to record that {:02x}:{:02x}.{} left a quarantined domain behind - its holdings would report as zero, which is not an answer this controller may give", bus, dev, func),
			}
			false
		}
	};
	// A BINDING CHANGE IS WHEN THE FAULTS ARE COLLECTED. Bounded, and said plainly: this kernel
	// polls the event queue at binding transitions and at boot rather than on an interrupt, so a
	// fault raised between two of those moments is reported late. It is never LOST - the device
	// keeps it queued - and it is never unbounded, because the drain is bounded by the buffer.
	// Interrupt-driven delivery is not implemented here.
	//
	// ATTRIBUTED, because by here the backend cannot be: the revoke above took this endpoint out of
	// its attached table, so a fault queued since the pre-drain would otherwise be reported as
	// domain zero at generation zero. This drain supplies the binding that was ending.
	//
	// AND IT RUNS BEFORE THE DOMAIN IS DESTROYED, WHICH IS WHAT LETS THE BINDING BE CHARGED
	// (corrected 2026-08-31). This drain used to sit AFTER `destroy_domain`, and destroying a domain
	// removes its `DomainState` - the structure the per-domain fault counter lives in. So
	// `drain_faults_during` attributed each event correctly into the durable ring and then found no
	// state to increment, and the last faults a binding ever raised - the ones raised DURING its
	// revoke, which is exactly when a misbehaving endpoint raises them - were charged to nobody.
	// M5's event attribution was right and M4's per-binding counter was short by them.
	poll_faults_attributed(generation.map(|g| (endpoint, domain, g)));
	// AND THE BINDING'S FINAL COUNT IS COPIED OUT WHILE THE DOMAIN STILL ANSWERS (added 2026-09-01).
	//
	// Between this line and the next block the domain may be destroyed, and its `DomainState` carries
	// the per-binding fault counter the drain above has just finished updating. Reading it here is
	// what lets `faults_for` answer after the domain is gone - which is the whole of M4's EXPOSED
	// per-binding fault accounting, and the half the drain's reorder did not reach on its own.
	//
	// Written on BOTH paths, confirmed or not: an unconfirmed teardown keeps its domain and answers
	// from it, and having the number in two places that agree costs one store on a per-binding path.
	if let Some(count) = with(|controller| controller.iommu().faults_in(domain))
		&& let Some(slot) = RETAINED_FAULTS.lock().get_mut(index)
	{
		*slot = count;
	}
	// AND A CONFIRMED REVOKE DESTROYS THE DOMAIN, which nothing did.
	//
	// The public row was removed and the endpoint revoked, and the `DomainState` behind them stayed:
	// its terminal mapping rows, its entry in the controller's map, and its domain ID. A machine that
	// binds and unbinds a device - a driver restart, an operator stop, a rebind after a crash - grew
	// one dead domain per cycle and never gave a domain ID back, which is the lifecycle leak M4's
	// restart baseline cannot hold across.
	//
	// UNCONFIRMED IS THE CASE THAT MUST NOT, and `destroy_domain` already refuses it rather than
	// trusting this caller: it returns `Unconfirmed` while any endpoint is still attached or any
	// mapping of the domain is quarantined, because forgetting a quarantined mapping would hand its
	// address space back for reuse on exactly the evidence that says not to. So the rule is stated
	// here and enforced there.
	//
	// AND IT HAPPENS AFTER THE DRAIN ABOVE, which is what lets the binding be charged for the faults
	// its own revoke raised. See that paragraph.
	if confirmed {
		match with(|controller| controller.iommu().destroy_domain(domain)) {
			Some(Ok(_)) => {}
			Some(Err(fault)) => crate::serial_println!("iommu: domain {} outlived the endpoint that used it ({fault:?}) - it is retained rather than reused", domain.0),
			None => {}
		}
	}
	confirmed
}

// Take whatever the device has reported and record it. Bounded by a fixed buffer, so a flooding
// endpoint does bounded work per call and cannot decide how much the kernel allocates.
pub fn poll_faults() {
	poll_faults_attributed_with(None, Containment::WhateverFaulted)
}

// WHICH DEVICES A DRAIN MAY TAKE OFF THE BUS - see `device::contain_faulting_endpoint_of_a_live_binding`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Containment {
	// The device that faulted, claimed or not. The teardown and the boot report, which are already
	// inside one device's story and are not sweeping every endpoint on the machine.
	WhateverFaulted,
	// Only a device a binding currently holds. The periodic drain, which runs at an arbitrary moment
	// against every endpoint - including the ones this kernel's own hostile-DMA fixtures are making
	// fault on purpose, under a synthetic binding and no claim.
	OnlyLiveBindings,
}

// HOW OFTEN THE PERIODIC DRAIN MAY RUN, as a tick the last drain was taken on.
//
// One drain per tick across the whole machine: the compare-exchange is what makes it one core rather
// than all of them, and the tick is what stops an idle core spinning through the controller's queue
// as fast as it can loop.
static LAST_PERIODIC_DRAIN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// SERVICE FAULTS WHILE THE BINDINGS THAT CAUSED THEM ARE STILL RUNNING.
//
// M5 asks a hardware fault to reach the binding lifecycle's containment, and until now nothing drove
// that in production: the only callers were the one boot report and the synchronous teardown, so a
// fault raised by a long-lived endpoint sat in the controller's queue until that binding eventually
// ended - and for a binding that never ends, for ever. The containment existed and nothing ran it.
//
// HERE, AND NOT IN THE TIMER INTERRUPT. `poll_faults` takes the controller's lock and then
// `device`'s, and an interrupted context may hold either; the idle loop holds neither. It already
// services TLB shootdowns and drains the serial ring for the same reason, and it is entered with
// interrupts enabled. What it is NOT is a guarantee: a machine whose every core is busy does not
// reach it, so this bounds the delay on any core that goes idle rather than promising a deadline.
// That is a strict improvement on "never until teardown" and is written here as what it is.
pub fn service_faults_if_due() {
	// ONE CORE EVER LOOKS, AND THAT IS THE FIRST TEST BECAUSE IT IS THE ONLY FREE ONE (measured
	// 2026-09-01).
	//
	// The first version asked `translating()` first, which takes the controller's spinlock; the
	// second replaced that with two relaxed loads of SHARED words - the tick counter and the
	// last-drain stamp. Both are cheap natively and neither is cheap where it lands: `cpu_idle_loop`
	// does NOT halt while its run queue is non-empty, so during a spawn storm every idle hart spins
	// the whole loop body at full speed, and under TCG several harts touching one line is emulated
	// rather than coherent hardware.
	//
	// It showed up as a real regression rather than a theory.
	// `kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick` measures exactly
	// that woken-and-spinning path on emulated riscv64, and it failed two of three targeted runs
	// with the shared loads in, against gaps consistently larger than any run before this change.
	//
	// So the gate is the core id, which is per-core state and touches nothing another hart can see.
	// Every hart but one returns having read a register. What this costs is honesty about the
	// guarantee, which was already weak: the drain bounds the delay on cpu 0 settling rather than on
	// ANY core going idle. It is still the only production path that services a fault while the
	// binding that caused it is running, which is the property M5 asks for.
	//
	// AND THE CALLER MOVED TO MATCH IT (2026-09-01). Gating to cpu 0 while the only call site was
	// `cpu_idle_loop` made this unreachable: that loop is entered by SECONDARY cores alone, and the
	// BSP settles in `main`'s own `run_until_idle`-and-halt. The two changes were each right and
	// together they were dead code. The call is now in the BSP loop, so the core this gate admits is
	// the core that arrives - and a single-core machine, which had no periodic drain under either
	// arrangement, has one.
	if crate::sched::current_cpu_id() != 0 {
		return;
	}
	let now = crate::arch::apic::ticks();
	let last = LAST_PERIODIC_DRAIN.load(core::sync::atomic::Ordering::Relaxed);
	if now == last {
		return;
	}
	if LAST_PERIODIC_DRAIN.compare_exchange(last, now, core::sync::atomic::Ordering::AcqRel, core::sync::atomic::Ordering::Relaxed).is_err() {
		return;
	}
	if !translating() {
		return;
	}
	poll_faults_attributed_with(None, Containment::OnlyLiveBindings);
}

// Drain and report faults, optionally supplying the attribution a TEARDOWN knows and the backend no
// longer does.
//
// THE TEARDOWN DRAIN IS THE ONE THAT LOSES THE ANSWER. The backend resolves an event's domain from
// its own attached table, and the revoke that ends a binding removes the endpoint from it - so a
// fault queued between the pre-drain and the revoke, or caused BY the revoke, is drained afterwards
// with `DomainId(0)` and `Generation(0)` behind it. That is the moment a fault most needs
// attributing: a device faulting as its binding ends is what the containment policy exists for, and
// "domain 0, generation 0" is the report saying it does not know.
//
// So the caller that is tearing an endpoint down passes what it still knows. It is applied ONLY to
// an event whose endpoint matches and whose domain the backend could not resolve - a fault from
// another endpoint keeps its own attribution, and a resolved one is never overwritten by a guess.
pub fn poll_faults_attributed(during_teardown: Option<(dma::EndpointId, dma::DomainId, Generation)>) {
	poll_faults_attributed_with(during_teardown, Containment::WhateverFaulted)
}

pub fn poll_faults_attributed_with(during_teardown: Option<(dma::EndpointId, dma::DomainId, Generation)>, containment: Containment) {
	// THE CEILING IS THE BOUND, and it is a whole number of drains rather than a buffer size. This
	// used to loop until the queue answered empty, with a comment saying the work was bounded -
	// which was true of each drain and not of the loop around them. An endpoint faulting faster than
	// this can print stays inside this call for as long as it keeps producing, and it is the same
	// endpoint that decides how long that is.
	const MOST_PER_CALL: usize = 64;
	let mut out = [dma::FaultEvent { endpoint: dma::EndpointId(0), domain: dma::DomainId(0), generation: Generation(0), address: None, access: dma::Access::Read, reason: Fault::NotMapped }; 8];
	let mut reported = 0usize;
	while reported < MOST_PER_CALL {
		// THE ATTRIBUTION GOES IN WITH THE DRAIN, not over the buffer afterwards. It used to be
		// applied here, to events `record` had already stored - so the line below named the binding
		// and the bounded ring a supervisor reads kept `domain 0, generation 0` for the same fault.
		let taken = drain_faults(&mut out, during_teardown);
		if taken == 0 {
			return;
		}
		for event in out.iter().take(taken) {
			// SAID WITH THE ADDRESS WHEN THERE IS ONE. A report the controller sent without an
			// address is a real fault reported in less detail, and printing a zero for it would be
			// this side inventing a number the device did not send.
			match event.address {
				Some(address) => crate::serial_println!("iommu: FAULT endpoint {:#x} domain {} {:?} at {:#x}: {:?}", event.endpoint.0, event.domain.0, event.access, address.get(), event.reason),
				None => crate::serial_println!("iommu: FAULT endpoint {:#x} domain {} {:?} at an address the controller did not report: {:?}", event.endpoint.0, event.domain.0, event.access, event.reason),
			}
			// AND THE DEVICE IS TAKEN OFF THE BUS. Reporting a fault and leaving the endpoint
			// mastering memory is a diagnostic where the milestone asks for containment: a device
			// resolving addresses its own domain refuses is a device to stop, not to describe.
			let (bus, dev, func) = ((event.endpoint.0 >> 8) as u8, ((event.endpoint.0 >> 3) & 0x1f) as u8, (event.endpoint.0 & 7) as u8);
			// AN ENDED BINDING'S FAULT IS CHARGED WHERE A MANAGER CAN READ IT (added 2026-09-02).
			//
			// The ledger charges a tail to its `DetachedTail`, and `Iommu::faults_in` sums that with
			// the domain's own - which is right and is not what a manager asks. It asks
			// `iommu::faults_for(device)`, and after a confirmed teardown that answers from
			// `RETAINED_FAULTS`, a scalar copied ONCE at detach time; after a rebind it answers from
			// the replacement's domain. Either way a fault drained later reached the DMA ledger and
			// never reached the number M4 exposes, so "the accounting outlives the domain" was true
			// inside the crate and false at the seam a supervisor reads.
			//
			// So a fault whose generation is not this device's current binding is added to the
			// retained count as it is drained. A LIVE binding's faults are not: they are already in
			// its own domain, which `faults_for` reads directly, and adding them here would count
			// them twice.
			if let Some((index, current)) = crate::device::binding_of_faulting_endpoint(bus, dev, func, event.generation.0)
				&& !current && let Some(slot) = RETAINED_FAULTS.lock().get_mut(index)
			{
				*slot = slot.saturating_add(1);
			}
			let contained = match containment {
				Containment::WhateverFaulted => crate::device::contain_faulting_endpoint(bus, dev, func),
				// AND NOT ON A GENERATION THE LEDGER SAYS IT CANNOT TRUST (added 2026-09-02).
				//
				// A tail record given up while it was still attributing leaves its endpoint's queued
				// faults resolving through whatever is attached NOW, which is how an ended binding's
				// fault comes to contain a healthy replacement. The ledger reports that state rather
				// than hiding it, and this is the decision that must not be taken while it holds:
				// withholding containment leaves a faulting device on the bus for longer, and acting
				// on attribution known to be incomplete takes a working one off it. The state clears
				// when the transport is next seen empty, and the next drain contains what it should.
				Containment::OnlyLiveBindings if !attribution_trustworthy() => {
					crate::serial_println!("iommu:   containment withheld at {bus:02x}:{dev:02x}.{func} - a tail record was given up while it was still attributing, so this fault cannot be told from a replacement's");
					None
				}
				Containment::OnlyLiveBindings => crate::device::contain_faulting_endpoint_of_a_live_binding(bus, dev, func, event.generation.0),
			};
			if let Some(index) = contained {
				crate::serial_println!("iommu:   device {index} at {bus:02x}:{dev:02x}.{func} no longer masters the bus - its binding is contained until whoever holds it tears it down");
			}
		}
		reported += taken;
	}
	// STOPPED, AND SAID SO. A drain that gives up quietly is indistinguishable from a queue that ran
	// dry, and the difference is exactly what a storm looks like. What is left stays queued on the
	// device and is reported at the next binding transition; the log keeps its own total either way.
	crate::serial_println!("iommu: {MOST_PER_CALL} fault(s) reported and more are queued - stopping here so one endpoint cannot hold this core");
}

// Map a DMA buffer's pages for the device at `index`, if that device is behind the IOMMU.
//
// `Ok(None)` is "this device is not translated", which is not a failure: it is every device in this
// tree today. `Err` is a device that IS translated and whose mapping did not confirm - and the
// caller must not hand out an address for one of those.
pub fn map_device_buffer(index: u32, generation: u64, physical: u64, len: u64) -> Result<Option<(dma::MappingId, dma::DmaAddress)>, Fault> {
	let Some(domain) = domain_for_generation(index, generation)? else { return Ok(None) };
	map_for_device(domain, physical, len, dma::Direction::Bidirectional).map(Some)
}

// Which domain a device's endpoint was attached to, if it was.
fn domain_of(index: u32) -> Option<dma::DomainId> {
	DOMAINS.lock().iter().find(|(device, _)| *device == index).map(|(_, domain)| *domain)
}

// The same, CONFIRMED to be the domain of the binding the caller holds.
//
// `domain_of` answers by device INDEX, and an index is a slot that outlives the binding sitting in
// it. A syscall reads the claim off the capability it was given, does its allocation, and only then
// asks which domain to map into - so a release and a reclaim in between put the REPLACEMENT
// binding's domain under the old caller's index, and the mapping or the doorbell landed in a domain
// that caller never had authority over. Registering the derived object afterwards refuses the stale
// generation, which is a rollback of the bookkeeping after the hardware effect has already happened.
//
// The claim's generation is what tells the two apart and the controller already records one per
// domain, so the check is arithmetic - which is the form M2 requires a stale binding to be refused
// in. `Ok(None)` is "this device is not translated"; `Err(StaleGeneration)` is "it is, and not for
// you"; a controller that cannot answer at all is the same refusal, because a mapping made into a
// domain nothing can confirm is a mapping made on trust.
fn domain_for_generation(index: u32, generation: u64) -> Result<Option<dma::DomainId>, Fault> {
	let Some(domain) = domain_of(index) else { return Ok(None) };
	match with(|controller| controller.iommu().generation_of(domain)) {
		Some(Some(current)) if current.0 == generation => Ok(Some(domain)),
		_ => Err(Fault::StaleGeneration),
	}
}

// Close one translation. The frames behind it are reusable only when this says `FramesReusable`.
pub fn unmap_for_device(id: dma::MappingId) -> Result<dma::Release, Fault> {
	// `close` rather than the two phases by hand, because the endpoint revoke reaches the same
	// mapping and there is no order between them: a driver that exits drops its device capability
	// and its DMA buffers in whatever order the process teardown runs them. Whichever arrives second
	// finds a mapping already taken down, and `close` answers with the verdict the first one reached
	// instead of reporting a failure that already happened successfully.
	with(|controller| controller.iommu().close(id)).unwrap_or(Err(Fault::Unconfirmed))
}

// The endpoint is going away. Everything it could reach stops being reachable, or is quarantined.
pub fn revoke_endpoint(domain: dma::DomainId, bus: u8, dev: u8, func: u8) -> Result<dma::Release, Fault> {
	let endpoint = requester_of(bus, dev, func);
	with(|controller| controller.iommu().revoke_endpoint(domain, endpoint)).unwrap_or(Err(Fault::Unconfirmed))
}

// Run `f` against the controller, if there is one.
pub fn with<R>(f: impl FnOnce(&mut Controller) -> R) -> Option<R> {
	CONTROLLER.lock().as_mut().map(f)
}

// Take the faults the device has reported since last time, bounded by the caller's buffer.
//
// `during_teardown` is what the caller ending a binding still knows and the backend no longer does -
// it reaches the DURABLE record rather than only the returned buffer. See `Iommu::drain_faults_during`.
fn drain_faults(out: &mut [dma::FaultEvent], during_teardown: Option<(dma::EndpointId, dma::DomainId, Generation)>) -> usize {
	with(|controller| controller.iommu().drain_faults_during(out, during_teardown)).unwrap_or(0)
}

// WHETHER A CONTAINMENT DECISION TAKEN ON A FAULT'S GENERATION CAN BE BELIEVED RIGHT NOW.
//
// False while the ledger has had to give up a tail record that was still attributing - see
// `Iommu::attribution_is_complete`. A machine with no controller has nothing to distrust and answers
// true, which is also what keeps the untranslated path unchanged.
fn attribution_trustworthy() -> bool {
	with(|controller| controller.iommu().attribution_is_complete()).unwrap_or(true)
}

// The controller's state, for the boot report.
//
// READ FROM THE DEVICE, NOT REMEMBERED. A controller that has entered its NEEDS_RESET state, or
// whose bypass byte has changed under the kernel, is reporting that translation is no longer what
// the kernel thinks it is - and a remembered answer is exactly what would miss it.
pub fn report() {
	let Some((enforcing, healthy)) = with(|controller| (controller.still_enforcing(), controller.healthy())) else {
		crate::serial_println!("iommu: no virtio-iommu on this machine");
		return;
	};
	crate::serial_println!("iommu: virtio-iommu present, enforcing={enforcing}, healthy={healthy}");
	poll_faults();
	// THE LEDGER, PRINTED. `live_mappings`, `quarantined_mappings` and the fault total existed and
	// were read only by the DMA crate's own tests, so M4's "exact post-restart endpoint, mapping and
	// IOVA baseline" could not be asserted from outside the crate at all. These are that baseline:
	// a machine that has torn every binding down and is holding nothing reads zero live and zero
	// quarantined, and a non-zero quarantined count is the deliberate leak this kernel takes when a
	// teardown could not be confirmed - which is a number an operator should be able to see.
	if let Some((endpoints, live, quarantined, faults, dropped)) = with(|controller| {
		let iommu = controller.iommu();
		(iommu.attached_endpoints(), iommu.live_mappings(), iommu.quarantined_mappings(), iommu.faults().total(), iommu.faults().dropped())
	}) {
		crate::serial_println!("iommu: {endpoints} endpoint(s) attached, {live} mapping(s) live, {quarantined} quarantined, {faults} fault(s) reported ({dropped} not kept)");
	}
	if !enforcing || !healthy {
		crate::serial_println!("iommu: WARNING - the controller is not in the state this profile requires");
	}
}

#[cfg(test)]
mod tests;
