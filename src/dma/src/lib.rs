// What a device may reach, said once, for every backend there will ever be.
//
// WHY THIS IS A CRATE. The rules here are arithmetic and ordering: which addresses an endpoint can
// name, which direction a mapping permits, and what must have COMPLETED before a frame goes back to
// the allocator. None of that is architecture-specific and none of it needs a device, so it lives
// where a host can drive every failure and every rollback order through it - millions of times if
// need be - rather than only inside a booted kernel with an emulated IOMMU attached.
//
// WHAT IT DELIBERATELY IS NOT. It does not talk to hardware. `Backend` is the seam: `virtio-iommu`,
// VT-d, AMD-Vi and the SMMUs are implementations of it, and the one this milestone writes is a
// SECOND implementation of a tested interface rather than the first implementation of an untested
// one. It also does not own frames - it says when they may be reused, and the kernel's allocator is
// what reuses them.
//
// THE CLAIM THIS CODE EXISTS TO SUPPORT is that a device reaches only the live mappings attached to
// its current binding generation. Everything below is in service of that sentence: generations that
// make a stale mapping nameless, directions that make a read-only mapping unwritable, an IOVA space
// that cannot hand out what it has not reclaimed, and a close path in which the frame is the LAST
// thing released.

#![no_std]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub mod fake;
#[cfg(test)]
mod tests;
pub mod virtio_iommu;
#[cfg(test)]
mod virtio_tests;

// An address a DEVICE names. Not a physical address and not a virtual one: what the endpoint puts on
// the bus and the IOMMU translates. The distinction is the milestone's subject - `SYS_DMA_BUFFER_PHYS`
// hands a driver an integer that no translation constrains, and the whole point of this type is that
// the kernel can take it back.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct DmaAddress(pub u64);

impl DmaAddress {
	pub const fn get(self) -> u64 {
		self.0
	}

	// The address `bytes` past this one, or `None` where that would leave the address space. A
	// device range that wraps is not a range.
	pub fn checked_add(self, bytes: u64) -> Option<DmaAddress> {
		self.0.checked_add(bytes).map(DmaAddress)
	}
}

// Which way the data moves, which is the same question as which access the IOMMU must permit.
//
// A MAPPING IS NOT AUTOMATICALLY BOTH. A ring the device only reads is a mapping the device may not
// write, and a backend that maps every request read-write has silently dropped half of the Goal's
// claim - which is why `Backend` is required to support all three and to REFUSE the enforcing
// profile if it cannot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
	// The device reads; the CPU wrote. A device write to one of these is a fault.
	ToDevice,
	// The device writes; the CPU will read. A device read of one of these is a fault.
	FromDevice,
	Bidirectional,
}

impl Direction {
	pub const fn device_may_read(self) -> bool {
		matches!(self, Direction::ToDevice | Direction::Bidirectional)
	}

	pub const fn device_may_write(self) -> bool {
		matches!(self, Direction::FromDevice | Direction::Bidirectional)
	}

	// Whether an access of this kind is permitted by a mapping made in this direction.
	pub const fn permits(self, access: Access) -> bool {
		match access {
			Access::Read => self.device_may_read(),
			Access::Write => self.device_may_write(),
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
	Read,
	Write,
}

// Which endpoint - a requester identity the KERNEL derives, never one a driver supplies.
//
// On PCI this is segment/bus/device/function as the backend's requester rules alias it. It is
// deliberately opaque here: this crate's rules do not depend on how an endpoint is named, only on
// two endpoints being distinguishable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct EndpointId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct DomainId(pub u32);

// WHICH BINDING THE MAPPING BELONGS TO. A reused device slot gets a new generation, and every
// mapping, attachment and fault carries the one it was made under. This is what makes a stale
// completion arriving after a rebind nameless rather than dangerous: it names a generation that has
// moved on, and there is nothing for it to apply to.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Generation(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct MappingId(pub u64);

// What a device can address, and what it needs from the memory behind an address.
//
// AN ADDRESS-LIMITED DEVICE IS THE ORDINARY CASE, not an exception: a 32-bit engine cannot name a
// frame above 4 GiB however correct everything else is. `plan` below answers with a direct mapping,
// a scatter/gather list, or a bounce - and refuses rather than handing back a plan the device
// cannot execute.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Requirements {
	// How many bits of address the device puts on the bus. 32 for a legacy engine, 64 for a modern
	// one.
	pub address_bits: u32,
	// The alignment every segment must satisfy.
	pub alignment: u64,
	// How many segments the device's descriptor format can hold. One means "contiguous or bounce".
	pub max_segments: usize,
	// Whether the device's accesses are coherent with the CPU's caches. Where they are not, the
	// `sync` calls below are not advisory.
	pub coherent: bool,
}

impl Requirements {
	// The highest address this device can name, inclusive.
	pub fn ceiling(&self) -> u64 {
		if self.address_bits >= 64 { u64::MAX } else { (1u64 << self.address_bits) - 1 }
	}

	pub fn permits(&self, address: u64, len: u64) -> bool {
		match address.checked_add(len) {
			// `len` bytes from `address` must END at or below the ceiling; the ceiling is inclusive
			// so the exclusive end may be one past it.
			Some(end) => address % self.alignment == 0 && end - 1 <= self.ceiling(),
			None => false,
		}
	}
}

// Why an operation was refused, or why a device access faulted.
//
// TYPED RATHER THAN A STRING, because the lifecycle above this makes decisions on it: a permission
// fault quarantines an endpoint, an exhausted IOVA space refuses a binding, and an unconfirmed
// invalidation must never be mistaken for either.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
	// The endpoint is not attached to any domain, or to a different one than the access implies.
	UnknownEndpoint,
	// The mapping named a generation that is no longer current.
	StaleGeneration,
	// The address is not mapped in this domain.
	NotMapped,
	// The mapping exists but does not permit this direction of access.
	Permission,
	// The device cannot address this, or the request left the negotiated input range.
	OutOfRange,
	// The IOVA space has no room for the request.
	NoSpace,
	// The backend refused, or its completion never arrived. Distinct from every case above: the
	// kernel does not know the state of the hardware, so nothing may be released.
	Unconfirmed,
	// The request was malformed - a zero length, an unaligned address, a range that wraps.
	Malformed,
	// The request overlaps a live mapping or a reserved region.
	Overlaps,
}

// A COMPLETION, NOT AN ACCEPTANCE, and the type is what keeps the two apart.
//
// A backend that returns "the request was accepted" has said nothing about the hardware's state, and
// releasing a frame on the strength of it is exactly the bug this milestone exists to prevent. Only
// a backend can construct one of these, and it may only do so once the operation is known to have
// taken effect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Confirmed(());

impl Confirmed {
	// Called by a backend when - and only when - the hardware has confirmed the operation.
	pub const fn by_backend() -> Self {
		Confirmed(())
	}
}

// One device access the hardware refused, as the backend reports it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FaultEvent {
	pub endpoint: EndpointId,
	pub domain: DomainId,
	pub generation: Generation,
	pub address: DmaAddress,
	pub access: Access,
	pub reason: Fault,
}

// The kernel-owned interface every IOMMU is reached through.
//
// EVERY METHOD EITHER CONFIRMS OR FAILS. There is no third answer, because a third answer is what a
// caller turns into "probably fine" - and "probably fine" about an invalidation is a frame handed to
// its next owner while a device can still write to it.
pub trait Backend {
	fn domain_create(&mut self) -> Result<(DomainId, Confirmed), Fault>;
	fn domain_destroy(&mut self, domain: DomainId) -> Result<Confirmed, Fault>;
	fn attach(&mut self, domain: DomainId, endpoint: EndpointId) -> Result<Confirmed, Fault>;
	fn detach(&mut self, domain: DomainId, endpoint: EndpointId) -> Result<Confirmed, Fault>;
	fn map(&mut self, domain: DomainId, iova: DmaAddress, physical: u64, len: u64, direction: Direction) -> Result<Confirmed, Fault>;
	fn unmap(&mut self, domain: DomainId, iova: DmaAddress, len: u64) -> Result<Confirmed, Fault>;
	// Translation state for this domain is no longer cached anywhere the device can reach. Separate
	// from `unmap` because a backend may complete the one without the other, and the frame is not
	// reusable until BOTH have completed.
	fn invalidate(&mut self, domain: DomainId) -> Result<Confirmed, Fault>;
	// Take up to `out.len()` pending fault events. Returns how many were written. BOUNDED BY THE
	// CALLER'S BUFFER on purpose: a flooding device must not be able to decide how much the kernel
	// allocates.
	fn drain_faults(&mut self, out: &mut [FaultEvent]) -> usize;
	// Whether this backend can enforce all three directions. A backend that cannot must not be used
	// for an enforcing profile - mapping everything read-write would silently drop half the claim.
	fn enforces_directions(&self) -> bool;
}

// Which side of the threat model a driver is on.
//
// CAPABILITY ISOLATION DOES NOT COVER DMA, and this enum is where that is said rather than implied.
// A bus-mastering device programmed by a malicious driver writes wherever it is told; no handle
// table stops it. A system that behaves as though capabilities cover DMA has a hole exactly where
// its strongest claim is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
	// The ordinary case: this driver may not bind unless an enforcing IOMMU is present. It receives
	// revocable addresses and nothing else.
	IommuRequired,
	// The minimal boot-critical set, which may run untranslated - LOUDLY. Binding one of these
	// without an IOMMU puts the system into a degraded-isolation state that names the driver.
	TrustedUntranslated,
}

// What a bind attempt is allowed to do, given the policy and whether enforcement is actually there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindDecision {
	// Translated, revocable addresses. The Goal's claim covers this driver.
	Translated,
	// Untranslated and audited: the caller must record the degraded state and which driver caused
	// it. The Goal's claim does NOT cover this driver.
	DegradedUntranslated,
	// Refused. An `iommu-required` driver without enforcement does not start.
	Refused,
}

// The decision, in one place, so no call site can invent a fourth answer.
pub fn decide_bind(policy: Policy, enforcing: bool) -> BindDecision {
	match (policy, enforcing) {
		(Policy::IommuRequired, true) => BindDecision::Translated,
		(Policy::IommuRequired, false) => BindDecision::Refused,
		// A trusted driver still gets translation where translation exists: the trust is permission
		// to run WITHOUT it, not a preference for running without it.
		(Policy::TrustedUntranslated, true) => BindDecision::Translated,
		(Policy::TrustedUntranslated, false) => BindDecision::DegradedUntranslated,
	}
}

// A reserved region: an address range the backend says must never be handed out.
//
// The negotiated input range has holes in it - MSI windows, firmware-reserved areas - and a mapping
// placed in one is a mapping the hardware will not honour however correct the request looked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reserved {
	pub base: u64,
	pub len: u64,
}

// The addresses this domain may hand out, and which of them are in use.
//
// A FREED IOVA IS NOT IMMEDIATELY REUSABLE, and that is the whole design. An address goes back into
// the space only once the unmap AND the invalidation have completed; until then the range is held,
// because handing it to a second mapping while a device may still resolve the first is precisely the
// confusion an IOMMU exists to prevent. `held` is that state and it has its own name.
pub struct IovaSpace {
	base: u64,
	len: u64,
	// In use by a live mapping, or held after a close that has not completed. Ordered so a lookup
	// can find the range covering an address.
	taken: BTreeMap<u64, u64>,
	reserved: Vec<Reserved>,
	// Ranges whose release could not be confirmed. They are never handed out again in this domain's
	// lifetime - one address is a negligible loss, a device reaching a stranger's memory is not.
	quarantined: Vec<Reserved>,
}

impl IovaSpace {
	// `base`/`len` are the backend's NEGOTIATED input range. Nothing outside it is ever offered,
	// because nothing outside it is guaranteed to translate.
	pub fn new(base: u64, len: u64, reserved: Vec<Reserved>) -> Self {
		Self { base, len, taken: BTreeMap::new(), reserved, quarantined: Vec::new() }
	}

	fn end(&self) -> u64 {
		self.base.saturating_add(self.len)
	}

	fn overlaps_any(&self, at: u64, len: u64) -> bool {
		let end = at.saturating_add(len);
		let reserved = self.reserved.iter().any(|r| at < r.base.saturating_add(r.len) && r.base < end);
		let quarantined = self.quarantined.iter().any(|r| at < r.base.saturating_add(r.len) && r.base < end);
		// A live or held range. `range(..end)` finds the last one starting before this request; only
		// that one can overlap on the low side, and any starting inside the request overlap by
		// definition.
		let before = self.taken.range(..end).next_back().is_some_and(|(start, length)| start.saturating_add(*length) > at);
		reserved || quarantined || before
	}

	// The lowest aligned address with room for `len`, or `NoSpace`.
	//
	// FIRST FIT, DELIBERATELY. The alternative - reusing the most recently freed range - is exactly
	// what makes a stale descriptor land on somebody else's mapping, and this space is about being
	// hard to confuse rather than about fragmentation.
	pub fn allocate(&mut self, len: u64, alignment: u64) -> Result<DmaAddress, Fault> {
		if len == 0 || alignment == 0 || !alignment.is_power_of_two() {
			return Err(Fault::Malformed);
		}
		let mut at = self.base.next_multiple_of(alignment);
		while at.saturating_add(len) <= self.end() {
			if !self.overlaps_any(at, len) {
				self.taken.insert(at, len);
				return Ok(DmaAddress(at));
			}
			// Past whatever is in the way, rather than one byte at a time.
			at = self.next_free_after(at).next_multiple_of(alignment);
			if at >= self.end() {
				break;
			}
		}
		Err(Fault::NoSpace)
	}

	// The first address past whatever occupies `at`. Used to step the search rather than scan.
	fn next_free_after(&self, at: u64) -> u64 {
		let mut past = at.saturating_add(1);
		for region in self.reserved.iter().chain(self.quarantined.iter()) {
			let end = region.base.saturating_add(region.len);
			if at >= region.base && at < end {
				past = past.max(end);
			}
		}
		if let Some((start, len)) = self.taken.range(..=at).next_back() {
			let end = start.saturating_add(*len);
			if at < end {
				past = past.max(end);
			}
		}
		past
	}

	// Hand a range back. Only ever called once the release is CONFIRMED - see `Iommu::finish_close`.
	pub fn release(&mut self, at: DmaAddress) -> Result<(), Fault> {
		self.taken.remove(&at.0).map(|_| ()).ok_or(Fault::NotMapped)
	}

	// Take a range out of circulation permanently. The unmap or the invalidation did not complete,
	// so nobody knows whether a device can still resolve it.
	pub fn quarantine(&mut self, at: DmaAddress) -> Result<(), Fault> {
		let len = self.taken.remove(&at.0).ok_or(Fault::NotMapped)?;
		self.quarantined.push(Reserved { base: at.0, len });
		Ok(())
	}

	pub fn quarantined_ranges(&self) -> usize {
		self.quarantined.len()
	}

	pub fn live_ranges(&self) -> usize {
		self.taken.len()
	}
}

// One segment of a plan: a physical range the device will address, and the IOVA it appears at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Segment {
	pub physical: u64,
	pub len: u64,
}

// How a buffer will actually be reached.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Plan {
	// The device addresses the pages where they are.
	Direct(Vec<Segment>),
	// The device cannot: the data is staged in a buffer it can address, and copied at the sync
	// points. Bounce solves REACHABILITY and coherency; it does not solve an attacker programming
	// an arbitrary address, which is why it is not a substitute for translation.
	Bounce { len: u64 },
}

// Decide how a device with these requirements reaches these pages.
//
// The two failure directions are different and both matter: too many segments for the descriptor
// format is a bounce, and an address the device cannot name is ALSO a bounce - but only if a
// reachable staging buffer could exist at all, which is what `address_bits` decides.
pub fn plan(segments: &[Segment], requirements: &Requirements) -> Result<Plan, Fault> {
	if segments.is_empty() || segments.iter().any(|s| s.len == 0) {
		return Err(Fault::Malformed);
	}
	let total: u64 = segments.iter().map(|s| s.len).sum();
	let reachable = segments.iter().all(|s| requirements.permits(s.physical, s.len));
	if reachable && segments.len() <= requirements.max_segments {
		return Ok(Plan::Direct(segments.to_vec()));
	}
	// A device that cannot address a single page of anything cannot be staged for either.
	if requirements.ceiling() < requirements.alignment {
		return Err(Fault::OutOfRange);
	}
	Ok(Plan::Bounce { len: total })
}

// Where a mapping is in its life. The states exist because the interesting moments are BETWEEN them:
// a close that has begun and not completed is exactly when a frame must not be reused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MappingState {
	// Installed and confirmed. The device may reach it.
	Live,
	// Closing: no new submissions, translation not yet known to be gone. The frame is NOT reusable.
	Closing,
	// Unmapped and invalidated, both confirmed. Only now may the frame go back to the allocator.
	Released,
	// The unmap or the invalidation did not complete. Nobody knows whether the device can still
	// reach it, so neither the address nor the frame is ever reused.
	Quarantined,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mapping {
	pub id: MappingId,
	pub domain: DomainId,
	pub iova: DmaAddress,
	pub physical: u64,
	pub len: u64,
	pub direction: Direction,
	pub generation: Generation,
	pub state: MappingState,
}

// What a completed close permits the caller to do with the frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Release {
	// Unmap and invalidation both confirmed: the pages may be refunded.
	FramesReusable,
	// Something did not confirm. The pages stay out of circulation, and so does the address.
	Quarantined,
}

struct DomainState {
	space: IovaSpace,
	generation: Generation,
	endpoints: Vec<EndpointId>,
	attached_confirmed: bool,
}

// A bounded record of what the hardware refused.
//
// COUNTERS ARE UNBOUNDED, EVENTS ARE NOT. A flooding device may increment a number for ever; what it
// may not do is decide how much memory the kernel keeps. The ring holds the most recent events and
// says how many it dropped.
pub struct FaultLog {
	recent: Vec<FaultEvent>,
	capacity: usize,
	total: u64,
	dropped: u64,
}

impl FaultLog {
	pub fn new(capacity: usize) -> Self {
		Self { recent: Vec::new(), capacity, total: 0, dropped: 0 }
	}

	fn record(&mut self, event: FaultEvent) {
		self.total += 1;
		if self.recent.len() == self.capacity {
			// The OLDEST goes, so what is kept is what just happened. A ring that dropped the newest
			// would answer "what is attacking me now" with the first thing that ever went wrong.
			self.recent.remove(0);
			self.dropped += 1;
		}
		self.recent.push(event);
	}

	pub fn total(&self) -> u64 {
		self.total
	}

	pub fn dropped(&self) -> u64 {
		self.dropped
	}

	pub fn recent(&self) -> &[FaultEvent] {
		&self.recent
	}
}

// The kernel's side of the contract: it owns the addresses, the ordering and the ledger, and it
// reaches the hardware only through `Backend`.
pub struct Iommu<B: Backend> {
	backend: B,
	domains: BTreeMap<DomainId, DomainState>,
	mappings: BTreeMap<MappingId, Mapping>,
	next_mapping: u64,
	faults: FaultLog,
}

impl<B: Backend> Iommu<B> {
	pub fn new(backend: B, fault_capacity: usize) -> Self {
		Self { backend, domains: BTreeMap::new(), mappings: BTreeMap::new(), next_mapping: 1, faults: FaultLog::new(fault_capacity) }
	}

	pub fn backend(&self) -> &B {
		&self.backend
	}

	// The backend, mutably, for a test that needs to arm a failure in it. Not part of the contract:
	// nothing in the kernel reaches past this interface to the hardware behind it.
	#[cfg(test)]
	pub fn backend_mut_for_test(&mut self) -> &mut B {
		&mut self.backend
	}

	pub fn faults(&self) -> &FaultLog {
		&self.faults
	}

	// One domain per exclusive binding. `generation` is the binding's, taken from the device
	// registry rather than invented here - a mapping bound to a generation that cannot change is a
	// weaker claim than one bound to a generation that can, and this crate does not pretend to own
	// the stronger one.
	pub fn create_domain(&mut self, base: u64, len: u64, reserved: Vec<Reserved>, generation: Generation) -> Result<DomainId, Fault> {
		let (id, _confirmed) = self.backend.domain_create()?;
		self.domains.insert(id, DomainState { space: IovaSpace::new(base, len, reserved), generation, endpoints: Vec::new(), attached_confirmed: false });
		Ok(id)
	}

	// ATTACH BEFORE BUS MASTERING, which is the caller's obligation and this method's whole purpose:
	// an endpoint that can master the bus before it is attached is an endpoint whose DMA is
	// untranslated for exactly as long as that window lasts.
	pub fn attach(&mut self, domain: DomainId, endpoint: EndpointId) -> Result<Confirmed, Fault> {
		let state = self.domains.get_mut(&domain).ok_or(Fault::UnknownEndpoint)?;
		let confirmed = self.backend.attach(domain, endpoint)?;
		state.endpoints.push(endpoint);
		state.attached_confirmed = true;
		Ok(confirmed)
	}

	// Whether this endpoint may be allowed to master the bus: attached, and the attachment confirmed.
	pub fn may_master(&self, domain: DomainId, endpoint: EndpointId) -> bool {
		self.domains.get(&domain).is_some_and(|d| d.attached_confirmed && d.endpoints.contains(&endpoint))
	}

	// Install one mapping, or leave nothing behind.
	//
	// THE ROLLBACK IS THE POINT. An address is reserved, the backend is asked, and a backend that
	// refuses leaves the caller with no mapping, no reserved address and no charge - because a
	// half-installed mapping is an address the driver may be told about and the hardware does not
	// honour, or worse, one the hardware honours and the kernel has forgotten.
	pub fn map(&mut self, domain: DomainId, physical: u64, len: u64, direction: Direction, requirements: &Requirements) -> Result<MappingId, Fault> {
		if len == 0 {
			return Err(Fault::Malformed);
		}
		let generation = {
			let state = self.domains.get(&domain).ok_or(Fault::UnknownEndpoint)?;
			state.generation
		};
		let iova = {
			let state = self.domains.get_mut(&domain).ok_or(Fault::UnknownEndpoint)?;
			state.space.allocate(len, requirements.alignment)?
		};
		// The address the DEVICE will name has to be one the device can name. An IOVA above a 32-bit
		// engine's ceiling is as useless as a physical address above it.
		if !requirements.permits(iova.get(), len) {
			let state = self.domains.get_mut(&domain).expect("checked above");
			let _ = state.space.release(iova);
			return Err(Fault::OutOfRange);
		}
		match self.backend.map(domain, iova, physical, len, direction) {
			Ok(_confirmed) => {
				let id = MappingId(self.next_mapping);
				self.next_mapping += 1;
				self.mappings.insert(id, Mapping { id, domain, iova, physical, len, direction, generation, state: MappingState::Live });
				Ok(id)
			}
			Err(reason) => {
				// NOTHING INSTALLED, so the address goes straight back: no translation was made, so
				// there is nothing a device could still resolve.
				let state = self.domains.get_mut(&domain).expect("checked above");
				let _ = state.space.release(iova);
				Err(reason)
			}
		}
	}

	pub fn mapping(&self, id: MappingId) -> Option<&Mapping> {
		self.mappings.get(&id)
	}

	pub fn address_of(&self, id: MappingId) -> Option<DmaAddress> {
		self.mappings.get(&id).map(|m| m.iova)
	}

	// Stop accepting new work against this mapping. The translation is still installed - this is the
	// half a cooperative driver participates in, and it is not what makes the frame safe.
	pub fn begin_close(&mut self, id: MappingId) -> Result<(), Fault> {
		let mapping = self.mappings.get_mut(&id).ok_or(Fault::NotMapped)?;
		if mapping.state != MappingState::Live {
			return Err(Fault::NotMapped);
		}
		mapping.state = MappingState::Closing;
		Ok(())
	}

	// COMPLETION PRECEDES CPU REUSE, and this is the method that says so. Unmap, then invalidate,
	// then - and only then - the address goes back and the caller is told the frames may be refunded.
	// Either step failing quarantines both the address and the frames.
	pub fn finish_close(&mut self, id: MappingId) -> Result<Release, Fault> {
		let mapping = *self.mappings.get(&id).ok_or(Fault::NotMapped)?;
		if mapping.state != MappingState::Closing {
			return Err(Fault::NotMapped);
		}
		let unmapped = self.backend.unmap(mapping.domain, mapping.iova, mapping.len);
		let invalidated = if unmapped.is_ok() { self.backend.invalidate(mapping.domain) } else { Err(Fault::Unconfirmed) };
		let space = &mut self.domains.get_mut(&mapping.domain).ok_or(Fault::UnknownEndpoint)?.space;
		if unmapped.is_ok() && invalidated.is_ok() {
			space.release(mapping.iova)?;
			self.mappings.get_mut(&id).expect("present").state = MappingState::Released;
			return Ok(Release::FramesReusable);
		}
		space.quarantine(mapping.iova)?;
		self.mappings.get_mut(&id).expect("present").state = MappingState::Quarantined;
		Ok(Release::Quarantined)
	}

	// The endpoint goes away - a clean unbind or a crash. Detach and invalidate, and quarantine
	// everything if either does not confirm. The lifecycle above this owns the reset and containment
	// policy; what this owes it is a confirmed answer or an explicit failure, never a guess.
	pub fn revoke_endpoint(&mut self, domain: DomainId, endpoint: EndpointId) -> Result<Release, Fault> {
		let detached = self.backend.detach(domain, endpoint);
		let invalidated = if detached.is_ok() { self.backend.invalidate(domain) } else { Err(Fault::Unconfirmed) };
		let confirmed = detached.is_ok() && invalidated.is_ok();
		let live: Vec<MappingId> = self.mappings.values().filter(|m| m.domain == domain && matches!(m.state, MappingState::Live | MappingState::Closing)).map(|m| m.id).collect();
		let state = self.domains.get_mut(&domain).ok_or(Fault::UnknownEndpoint)?;
		state.endpoints.retain(|e| *e != endpoint);
		if state.endpoints.is_empty() {
			state.attached_confirmed = false;
		}
		for id in live {
			let mapping = self.mappings.get_mut(&id).expect("listed above");
			mapping.state = if confirmed { MappingState::Released } else { MappingState::Quarantined };
			let space = &mut self.domains.get_mut(&domain).expect("checked above").space;
			let _ = if confirmed { space.release(mapping.iova) } else { space.quarantine(mapping.iova) };
		}
		if confirmed { Ok(Release::FramesReusable) } else { Ok(Release::Quarantined) }
	}

	// REBIND. The binding's generation moves, so every mapping made under the old one is stale by
	// construction - and `translate` below refuses them by that alone, without needing to have been
	// told about each one.
	pub fn set_generation(&mut self, domain: DomainId, generation: Generation) -> Result<(), Fault> {
		let state = self.domains.get_mut(&domain).ok_or(Fault::UnknownEndpoint)?;
		state.generation = generation;
		Ok(())
	}

	// WHAT THE HARDWARE WOULD DO, as this crate understands it: the claim in the Goal, written as a
	// function. A test can ask it directly, and a fake device can be driven through it, which is how
	// "the endpoint cannot reach that page" becomes something checkable rather than asserted.
	//
	// Every refusal is recorded as a fault event, because that is what an IOMMU does with one.
	pub fn translate(&mut self, endpoint: EndpointId, address: DmaAddress, access: Access) -> Result<u64, Fault> {
		let Some((domain, generation)) = self.domains.iter().find(|(_, d)| d.endpoints.contains(&endpoint) && d.attached_confirmed).map(|(id, d)| (*id, d.generation)) else {
			// Not attached anywhere: there is no domain in which this address means anything.
			self.faults.record(FaultEvent { endpoint, domain: DomainId(0), generation: Generation(0), address, access, reason: Fault::UnknownEndpoint });
			return Err(Fault::UnknownEndpoint);
		};
		let found = self.mappings.values().find(|m| m.domain == domain && m.state == MappingState::Live && address.get() >= m.iova.get() && address.get() < m.iova.get() + m.len).copied();
		let Some(mapping) = found else {
			self.faults.record(FaultEvent { endpoint, domain, generation, address, access, reason: Fault::NotMapped });
			return Err(Fault::NotMapped);
		};
		// A mapping made under a previous binding does not answer to this one, however live it looks.
		if mapping.generation != generation {
			self.faults.record(FaultEvent { endpoint, domain, generation, address, access, reason: Fault::StaleGeneration });
			return Err(Fault::StaleGeneration);
		}
		if !mapping.direction.permits(access) {
			self.faults.record(FaultEvent { endpoint, domain, generation, address, access, reason: Fault::Permission });
			return Err(Fault::Permission);
		}
		Ok(mapping.physical + (address.get() - mapping.iova.get()))
	}

	// Take what the hardware reported, bounded by the caller's buffer, and fold it into the log.
	pub fn drain_faults(&mut self, out: &mut [FaultEvent]) -> usize {
		let taken = self.backend.drain_faults(out);
		for event in out.iter().take(taken) {
			self.faults.record(*event);
		}
		taken
	}

	pub fn live_mappings(&self) -> usize {
		self.mappings.values().filter(|m| m.state == MappingState::Live).count()
	}

	pub fn quarantined_mappings(&self) -> usize {
		self.mappings.values().filter(|m| m.state == MappingState::Quarantined).count()
	}

	pub fn live_addresses(&self, domain: DomainId) -> usize {
		self.domains.get(&domain).map_or(0, |d| d.space.live_ranges())
	}

	pub fn quarantined_addresses(&self, domain: DomainId) -> usize {
		self.domains.get(&domain).map_or(0, |d| d.space.quarantined_ranges())
	}
}
