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
		if !self.requests.request(physical, request.len() as u32, physical + 2048, tail.len() as u32) {
			// THE DEVICE DID NOT ANSWER. Not a failure of the operation - a failure to learn
			// anything about it, which is the one state a caller must never round down to success.
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

	fn take_event(&mut self, out: &mut [u8]) -> usize {
		let Some(_slot) = self.events.poll_used() else { return 0 };
		let len = out.len().min(dma::virtio_iommu::FAULT_LEN);
		// SAFETY: the event buffer is this module's own frame, and the device was given exactly its
		// length.
		unsafe { core::ptr::copy_nonoverlapping(self.event_buffer as *const u8, out.as_mut_ptr(), len) };
		// Give the buffer back so the device can report the next one.
		self.events.offer(self.event_physical, dma::virtio_iommu::FAULT_LEN as u32);
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
	match bring_up(index) {
		Ok(controller) => {
			*CONTROLLER.lock() = Some(controller);
			PRESENT.store(true, Ordering::Release);
			crate::serial_println!("iommu: virtio-iommu is translating - bypass is off and read back as off");
			true
		}
		Err(reason) => {
			// A CONTROLLER THAT IS PRESENT AND DID NOT COME UP IS WORSE NEWS THAN NONE AT ALL: the
			// profile expected enforcement and does not have it, and every `iommu-required` driver
			// is about to be refused. Said loudly for that reason.
			crate::serial_println!("iommu: virtio-iommu present but NOT enforcing ({reason:?}) - every driver that requires translation will be refused");
			false
		}
	}
}

pub fn present() -> bool {
	PRESENT.load(Ordering::Acquire)
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
	events.offer(event_physical, dma::virtio_iommu::FAULT_LEN as u32);
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

	let wire = Wire { requests, events, event_buffer, event_physical };
	let backend = VirtioIommu::new(wire, config, accepted, Generation(1))?;
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
		// The domain's address space is the device's published input range. What must be carved out
		// of it, and what must be mapped one-to-one inside it, is what the device is asked below -
		// it cannot be asked before the endpoint exists, so the space starts whole and the answers
		// are applied to it.
		let iommu = controller.iommu();
		let domain = iommu.create_domain(config.input_start, config.input_len(), alloc::vec::Vec::new(), Generation(generation))?;
		iommu.attach(domain, endpoint)?;
		install_probed_regions(iommu, domain, endpoint);
		Ok(domain)
	})
	.unwrap_or(Err(Fault::Unconfirmed))
}

// Whether the doorbell fact has been printed. Every endpoint on a machine writes the same one.
static DOORBELL_REPORTED: AtomicBool = AtomicBool::new(false);

// Ask the device what this endpoint's domain must treat specially, and do it.
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
// A FAILURE HERE IS NOT FATAL AND IS NOT SILENT. The endpoint is attached and translating either
// way; what it loses is interrupt delivery, which is a driver that does not work rather than a
// driver that reaches memory it should not. So it is named and the boot goes on.
fn install_probed_regions(iommu: &mut dma::Iommu<VirtioIommu<Wire>>, domain: dma::DomainId, endpoint: dma::EndpointId) {
	let regions = match iommu.probe(endpoint) {
		Ok(regions) => regions,
		Err(reason) => {
			crate::serial_println!("iommu: endpoint {:#x} could not be probed ({reason:?}) - its reserved regions are unknown", endpoint.0);
			return;
		}
	};
	for region in regions {
		match region.kind {
			// THE DEVICE WRITES ITS DOORBELL. `ToDevice` is the direction that reads, and mapping it
			// that way produced a translation the interrupt controller's address had - and that
			// refused the one access anybody makes to it. QEMU's trace named it exactly:
			// `virt_start=0xfee00000 ... flags=1`, a read-only mapping, and the MSI arriving as
			// `flag=2`.
			dma::RegionKind::MsiDoorbell => match iommu.map_identity(domain, region.base, region.len, dma::Direction::FromDevice) {
				// SAID ONCE, NOT ONCE PER DOMAIN. Every endpoint on this machine writes the same
				// doorbell, so a line per domain is eleven copies of one fact - the scattering the
				// boot report keeps being cleaned of. A failure is per-domain and is always printed:
				// that one IS about this endpoint.
				Ok(_) => {
					if !DOORBELL_REPORTED.swap(true, Ordering::AcqRel) {
						crate::serial_println!("iommu: every domain carries the MSI doorbell at {:#x}+{:#x}, mapped one to one - a device's interrupt is a memory write and goes through its own domain", region.base, region.len);
					}
				}
				Err(reason) => crate::serial_println!("iommu: domain {} could not map its MSI doorbell at {:#x}+{:#x} ({reason:?}) - this endpoint's interrupts will not be delivered", domain.0, region.base, region.len),
			},
			// Nothing is mapped in a reserved hole, and nothing may be allocated there either. The
			// space is told rather than trusted to avoid it by accident.
			dma::RegionKind::Reserved => crate::serial_println!("iommu: domain {} has a reserved hole at {:#x}+{:#x}", domain.0, region.base, region.len),
		}
	}
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
pub fn attach_for(index: usize, bus: u8, dev: u8, func: u8) -> bool {
	if domain_of(index as u32).is_some() {
		return true;
	}
	// THE GENERATION IS THE BINDING'S. Binding identity is the driver lifecycle's to own and this
	// does not recreate it: what is used here is the owner count's transition, which is a weaker token
	// than a real binding generation and is stated as such rather than dressed up as one.
	let generation = 1;
	match attach_endpoint(bus, dev, func, generation) {
		Ok(domain) => {
			let mut domains = DOMAINS.lock();
			// ALLOC-OK: one row per device the boot scan resolved, and this is a binding transition
			// rather than an interrupt.
			if domains.try_reserve(1).is_err() {
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
pub fn detach_for(index: usize, bus: u8, dev: u8, func: u8) {
	let Some(domain) = domain_of(index as u32) else { return };
	DOMAINS.lock().retain(|(device, _)| *device != index as u32);
	match revoke_endpoint(domain, bus, dev, func) {
		Ok(dma::Release::FramesReusable) => {}
		other => crate::serial_println!("iommu: {:02x}:{:02x}.{} was not confirmed detached ({other:?}) - its pages stay quarantined", bus, dev, func),
	}
	// A BINDING CHANGE IS WHEN THE FAULTS ARE COLLECTED. Bounded, and said plainly: this kernel
	// polls the event queue at binding transitions and at boot rather than on an interrupt, so a
	// fault raised between two of those moments is reported late. It is never LOST - the device
	// keeps it queued - and it is never unbounded, because the drain is bounded by the buffer.
	// Interrupt-driven delivery is not implemented here.
	poll_faults();
}

// Take whatever the device has reported and record it. Bounded by a fixed buffer, so a flooding
// endpoint does bounded work per call and cannot decide how much the kernel allocates.
pub fn poll_faults() {
	// THE CEILING IS THE BOUND, and it is a whole number of drains rather than a buffer size. This
	// used to loop until the queue answered empty, with a comment saying the work was bounded -
	// which was true of each drain and not of the loop around them. An endpoint faulting faster than
	// this can print stays inside this call for as long as it keeps producing, and it is the same
	// endpoint that decides how long that is.
	const MOST_PER_CALL: usize = 64;
	let mut out = [dma::FaultEvent { endpoint: dma::EndpointId(0), domain: dma::DomainId(0), generation: Generation(0), address: dma::DmaAddress(0), access: dma::Access::Read, reason: Fault::NotMapped }; 8];
	let mut reported = 0usize;
	while reported < MOST_PER_CALL {
		let taken = drain_faults(&mut out);
		if taken == 0 {
			return;
		}
		for event in out.iter().take(taken) {
			crate::serial_println!("iommu: FAULT endpoint {:#x} domain {} {:?} at {:#x}: {:?}", event.endpoint.0, event.domain.0, event.access, event.address.get(), event.reason);
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
pub fn map_device_buffer(index: u32, physical: u64, len: u64) -> Result<Option<(dma::MappingId, dma::DmaAddress)>, Fault> {
	let Some(domain) = domain_of(index) else { return Ok(None) };
	map_for_device(domain, physical, len, dma::Direction::Bidirectional).map(Some)
}

// Which domain a device's endpoint was attached to, if it was.
fn domain_of(index: u32) -> Option<dma::DomainId> {
	DOMAINS.lock().iter().find(|(device, _)| *device == index).map(|(_, domain)| *domain)
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
pub fn drain_faults(out: &mut [dma::FaultEvent]) -> usize {
	with(|controller| controller.iommu().drain_faults(out)).unwrap_or(0)
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
	if !enforcing || !healthy {
		crate::serial_println!("iommu: WARNING - the controller is not in the state this profile requires");
	}
}

#[cfg(test)]
mod tests;
