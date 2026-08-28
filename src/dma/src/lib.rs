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
// THE FIELDS ARE PRIVATE AND THE CONSTRUCTOR CAN REFUSE, because two of the four have values that
// are not descriptions of any device. An alignment of zero made `permits` divide by it; a zero
// length made it evaluate `end - 1` at zero and underflow. Both were reachable by any caller,
// because every field was public and nothing was ever checked - the type described a device and
// then accepted numbers no device has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Requirements {
	// How many bits of address the device puts on the bus. 32 for a legacy engine, 64 for a modern
	// one.
	address_bits: u32,
	// The alignment every segment must satisfy. A power of two, and never zero.
	alignment: u64,
	// How many segments the device's descriptor format can hold. One means "contiguous or bounce".
	max_segments: usize,
	// Whether the device's accesses are coherent with the CPU's caches. Where they are not, the
	// `sync` calls below are not advisory.
	coherent: bool,
}

impl Requirements {
	// What a device can do, or a refusal naming what could not be true of one.
	pub fn new(address_bits: u32, alignment: u64, max_segments: usize, coherent: bool) -> Result<Self, Fault> {
		// A device that puts no bits on the bus does not master it; one that puts more than 64 is
		// naming an address space that does not exist.
		if address_bits == 0 || address_bits > 64 {
			return Err(Fault::Malformed);
		}
		// Alignment is a power of two - a segment boundary is not an arbitrary modulus - and a
		// device with no alignment requirement states one byte rather than zero.
		if alignment == 0 || !alignment.is_power_of_two() {
			return Err(Fault::Malformed);
		}
		// A descriptor format that holds no segments describes nothing that can be programmed.
		if max_segments == 0 {
			return Err(Fault::Malformed);
		}
		Ok(Self { address_bits, alignment, max_segments, coherent })
	}

	pub fn alignment(&self) -> u64 {
		self.alignment
	}

	pub fn max_segments(&self) -> usize {
		self.max_segments
	}

	pub fn coherent(&self) -> bool {
		self.coherent
	}

	// The highest address this device can name, inclusive.
	pub fn ceiling(&self) -> u64 {
		if self.address_bits >= 64 { u64::MAX } else { (1u64 << self.address_bits) - 1 }
	}

	pub fn permits(&self, address: u64, len: u64) -> bool {
		// A zero-length span has no last byte, so there is no question here to answer yes to.
		if len == 0 {
			return false;
		}
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
	//
	// `pub(crate)`, so the sentence above is enforced rather than asserted: every `Backend` lives in
	// this crate, and a caller outside it could otherwise mint the token that means "the hardware
	// says this took effect" without having asked any hardware.
	pub(crate) const fn by_backend() -> Self {
		Confirmed(())
	}
}

// One device access the hardware refused, as the backend reports it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FaultEvent {
	pub endpoint: EndpointId,
	pub domain: DomainId,
	pub generation: Generation,
	// WHERE, WHEN THE CONTROLLER KNOWS. A fault report carries a flag saying whether its address
	// field means anything, and a controller is entitled to clear it - the access faulted, and which
	// address it was aimed at was not recorded. That is a less detailed report of a real fault, and
	// it used to be refused as malformed and dropped without a word, which turned the one message
	// the fault queue exists to carry into silence.
	pub address: Option<DmaAddress>,
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
	// What this endpoint's domain must treat specially - reserved holes, and the doorbell its
	// interrupts are written to. The default is "nothing to report", which is the honest answer for
	// a backend that has no way to ask its hardware; a backend that CAN ask must not use it.
	fn probe(&mut self, endpoint: EndpointId) -> Result<Vec<ProbedRegion>, Fault> {
		let _ = endpoint;
		Ok(Vec::new())
	}
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

// What a backend reports about one address range an endpoint's domain must treat specially.
//
// A DEVICE'S INTERRUPT IS A MEMORY WRITE, and that is the fact this type exists for. An endpoint
// behind a translating IOMMU puts its MSI on the bus like any other write, so the doorbell address
// goes through the same translation as its DMA - and a domain with no mapping for it drops the
// interrupt silently. Nothing faults, nothing is logged, and the driver simply never wakes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProbedRegion {
	pub kind: RegionKind,
	pub base: u64,
	// Length in bytes. Inclusive ends are a wire detail; this is a length like every other here.
	pub len: u64,
}

// How a reported region must be treated. The two are opposites, so they are not a flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegionKind {
	// Nothing may be mapped here. The range is taken out of the space and never handed out.
	Reserved,
	// A doorbell the endpoint writes to raise an interrupt. It must be mapped ONE TO ONE, because
	// the address the device writes is fixed by the interrupt controller and not by this allocator.
	MsiDoorbell,
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
		// ZERO IS NEVER HANDED OUT, and this is not tidiness.
		//
		// A device is given these addresses to put in its own descriptors, and a null address means
		// "there is none" to a device exactly as it does to software. A `virtio` device treats a
		// queue whose descriptor table sits at address zero as a queue that was never programmed:
		// QEMU's `virtio_init_region_cache` returns early on a zero address and builds no mapping
		// for the ring, so the device never reads the ring, never fills a buffer and never raises an
		// interrupt - and nothing anywhere reports an error, because from the device's side there is
		// simply no queue there.
		//
		// It was measured. With a `virtio-iommu` in the machine, this space's first-fit search starts
		// at the negotiated input range's base, which QEMU publishes as zero, so the FIRST ring
		// allocated in a domain landed on IOVA 0. `virtio-net` transmitted, the host answered, and
		// its receive queue was never looked at once. Without an IOMMU the same driver works, because
		// a physical frame is never at address zero.
		let floor = self.base.max(alignment);
		let mut at = floor.next_multiple_of(alignment);
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

	// Claim ONE SPECIFIC range rather than the next free one.
	//
	// For an address the allocator does not get to choose: an MSI doorbell is where the interrupt
	// controller says it is, so the only mapping that can work for it is one to itself. Every other
	// rule still applies - it must be inside the negotiated input range and overlap nothing - and a
	// range that breaks either is a refusal rather than a silent overlap.
	pub fn take_exact(&mut self, at: u64, len: u64) -> Result<DmaAddress, Fault> {
		if len == 0 {
			return Err(Fault::Malformed);
		}
		let Some(end) = at.checked_add(len) else { return Err(Fault::Malformed) };
		if at < self.base || end > self.end() {
			return Err(Fault::OutOfRange);
		}
		if self.overlaps_any(at, len) {
			return Err(Fault::Overlaps);
		}
		self.taken.insert(at, len);
		Ok(DmaAddress(at))
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
	if reachable && segments.len() <= requirements.max_segments() {
		return Ok(Plan::Direct(segments.to_vec()));
	}
	// A device that cannot address a single page of anything cannot be staged for either.
	if requirements.ceiling() < requirements.alignment() {
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
		// A LOG THAT KEEPS NOTHING STILL COUNTS. With a capacity of zero the ring below would call
		// `remove(0)` on an empty vector, so the one configuration that keeps no history was the one
		// that panicked instead of keeping none.
		if self.capacity == 0 {
			self.dropped += 1;
			return;
		}
		if self.recent.len() >= self.capacity {
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
	// AN UNCONFIRMED MAP IS NOT A REFUSED ONE, and the difference decides whether an address may be
	// handed out again.
	//
	// `Fault::Unconfirmed` says in its own definition: "the kernel does not know the state of the
	// hardware, so nothing may be released." Both map paths released the IOVA for EVERY error,
	// under a comment asserting that nothing was installed - which is true of a refusal and is
	// exactly what an unconfirmed result does not say. A completion that timed out, named the wrong
	// descriptor, claimed an invalid length or omitted its status leaves a controller that may well
	// have installed the translation; giving that address to the next caller hands the device a
	// second owner's memory.
	//
	// So a refusal releases and an unconfirmed result QUARANTINES: the address stays taken for the
	// life of the domain, and the mapping is recorded so the accounting can see it.
	fn map_failed(&mut self, domain: DomainId, iova: DmaAddress, physical: u64, len: u64, direction: Direction, generation: Generation, reason: Fault) -> Fault {
		if reason == Fault::Unconfirmed {
			let id = MappingId(self.next_mapping);
			self.next_mapping += 1;
			self.mappings.insert(id, Mapping { id, domain, iova, physical, len, direction, generation, state: MappingState::Quarantined });
			return reason;
		}
		let state = self.domains.get_mut(&domain).expect("the caller holds this domain");
		let _ = state.space.release(iova);
		reason
	}

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
			state.space.allocate(len, requirements.alignment())?
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
			// A REFUSAL releases the address - no translation was made, so there is nothing a device
			// could still resolve. An UNCONFIRMED result quarantines it: see `map_failed`.
			Err(reason) => Err(self.map_failed(domain, iova, physical, len, direction, generation, reason)),
		}
	}

	// Map an address TO ITSELF, and take that exact range out of the space.
	//
	// The one mapping an allocator may not choose the address of. `probe` names the doorbell its
	// endpoint writes interrupts to; that address is the interrupt controller's, so the only mapping
	// that can carry an MSI through translation is an identity one. Everything else is `map`'s: the
	// address is claimed first, the backend is asked, and a backend that refuses leaves nothing
	// behind.
	pub fn map_identity(&mut self, domain: DomainId, address: u64, len: u64, direction: Direction) -> Result<MappingId, Fault> {
		if len == 0 {
			return Err(Fault::Malformed);
		}
		let generation = self.domains.get(&domain).ok_or(Fault::UnknownEndpoint)?.generation;
		let iova = {
			let state = self.domains.get_mut(&domain).ok_or(Fault::UnknownEndpoint)?;
			state.space.take_exact(address, len)?
		};
		match self.backend.map(domain, iova, address, len, direction) {
			Ok(_confirmed) => {
				let id = MappingId(self.next_mapping);
				self.next_mapping += 1;
				self.mappings.insert(id, Mapping { id, domain, iova, physical: address, len, direction, generation, state: MappingState::Live });
				Ok(id)
			}
			Err(reason) => Err(self.map_failed(domain, iova, address, len, direction, generation, reason)),
		}
	}

	// What the backend says this endpoint's domain must treat specially.
	pub fn probe(&mut self, endpoint: EndpointId) -> Result<Vec<ProbedRegion>, Fault> {
		self.backend.probe(endpoint)
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

	// This mapping is finished with, and the caller does not have to know who finished it.
	//
	// TWO OWNERS REACH THE SAME MAPPING and they do not agree on who goes first: the buffer that
	// holds the frames closes its own translation, and the endpoint revoke takes down every
	// translation in the domain. Whichever arrives second used to be told `NotMapped` - because
	// `begin_close` refuses anything that is not `Live` - and a kernel that reads that as "the
	// unmap did not complete" quarantines frames the first owner had already released cleanly.
	//
	// So a terminal state is an ANSWER here rather than an error. `Released` means somebody
	// completed the unmap and the invalidation; `Quarantined` means somebody could not, and that
	// verdict does not improve by being asked again. Only an id this table never had, or a close
	// that fails now, is a failure.
	pub fn close(&mut self, id: MappingId) -> Result<Release, Fault> {
		match self.mappings.get(&id).ok_or(Fault::NotMapped)?.state {
			MappingState::Released => return Ok(Release::FramesReusable),
			MappingState::Quarantined => return Ok(Release::Quarantined),
			MappingState::Live => self.begin_close(id)?,
			// Somebody began the close and did not finish it. Finishing it is exactly what this does.
			MappingState::Closing => {}
		}
		self.finish_close(id)
	}

	// The endpoint goes away - a clean unbind or a crash. Every translation in the domain comes down
	// first, then the endpoint leaves, and anything that did not confirm is quarantined. The
	// lifecycle above this owns the reset and containment policy; what this owes it is a confirmed
	// answer or an explicit failure, never a guess.
	//
	// THE UNMAPS GO BEFORE THE DETACH, and the order is not a preference. A `virtio-iommu` destroys a
	// domain when its last endpoint detaches, so an unmap sent afterwards names a domain the device
	// no longer has and is answered `NOENT`. Detaching first also used to mean the mappings were
	// marked `Released` with their addresses handed back while the device had never been told to
	// drop a single translation - the frames were declared reusable on the strength of a detach
	// alone, which is the one thing this crate exists to refuse.
	// RETIRE A DOMAIN, so a failed bind and a rebind cycle do not accumulate them.
	//
	// `Backend::domain_destroy` existed and nothing called it: `revoke_endpoint` left `DomainState`
	// and every terminal `Mapping` in their maps, and `next_domain` only ever advances - so repeated
	// failed binds consumed domain ids and grew two maps that nothing ever shrank. M3/M4's exact
	// post-restart baseline cannot hold while that is true.
	//
	// A DOMAIN HOLDING A QUARANTINED MAPPING IS NOT RETIRED, and that is the whole care this needs.
	// A quarantined mapping is one nobody knows the device has stopped resolving; forgetting it would
	// hand its address space back for reuse on exactly the evidence that says not to. Those domains
	// stay, which is a bounded leak with a reason, and `quarantined_mappings` counts them.
	pub fn destroy_domain(&mut self, domain: DomainId) -> Result<Confirmed, Fault> {
		let Some(state) = self.domains.get(&domain) else { return Err(Fault::UnknownEndpoint) };
		if !state.endpoints.is_empty() {
			// Something is still attached: destroying now would be a domain believed gone with an
			// endpoint still translating through it.
			return Err(Fault::Unconfirmed);
		}
		if self.mappings.values().any(|m| m.domain == domain && m.state == MappingState::Quarantined) {
			return Err(Fault::Unconfirmed);
		}
		let confirmed = self.backend.domain_destroy(domain)?;
		self.mappings.retain(|_, m| m.domain != domain);
		self.domains.remove(&domain);
		Ok(confirmed)
	}

	pub fn revoke_endpoint(&mut self, domain: DomainId, endpoint: EndpointId) -> Result<Release, Fault> {
		let live: Vec<MappingId> = self.mappings.values().filter(|m| m.domain == domain && matches!(m.state, MappingState::Live | MappingState::Closing)).map(|m| m.id).collect();
		// Each one on its own, because one translation the device refuses to drop must not condemn
		// the frames of the ones it dropped cleanly.
		let mut unmapped: Vec<(MappingId, bool)> = Vec::new();
		for id in live {
			let mapping = *self.mappings.get(&id).expect("listed above");
			let ok = self.backend.unmap(mapping.domain, mapping.iova, mapping.len).is_ok();
			unmapped.push((id, ok));
		}
		let invalidated = self.backend.invalidate(domain).is_ok();
		let detached = self.backend.detach(domain, endpoint).is_ok();
		// An endpoint still attached, or an invalidation nobody confirmed, condemns every mapping in
		// the domain however cleanly its own unmap went: the device may still be resolving any of
		// them out of a cache the kernel was never told was flushed.
		let endpoint_gone = invalidated && detached;
		let state = self.domains.get_mut(&domain).ok_or(Fault::UnknownEndpoint)?;
		state.endpoints.retain(|e| *e != endpoint);
		if state.endpoints.is_empty() {
			state.attached_confirmed = false;
		}
		let mut every_one = true;
		for (id, unmap_ok) in unmapped {
			let confirmed = unmap_ok && endpoint_gone;
			every_one &= confirmed;
			let mapping = self.mappings.get_mut(&id).expect("listed above");
			mapping.state = if confirmed { MappingState::Released } else { MappingState::Quarantined };
			let iova = mapping.iova;
			let space = &mut self.domains.get_mut(&domain).expect("checked above").space;
			let _ = if confirmed { space.release(iova) } else { space.quarantine(iova) };
		}
		if every_one && endpoint_gone { Ok(Release::FramesReusable) } else { Ok(Release::Quarantined) }
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
			self.faults.record(FaultEvent { endpoint, domain: DomainId(0), generation: Generation(0), address: Some(address), access, reason: Fault::UnknownEndpoint });
			return Err(Fault::UnknownEndpoint);
		};
		let found = self.mappings.values().find(|m| m.domain == domain && m.state == MappingState::Live && address.get() >= m.iova.get() && address.get() < m.iova.get() + m.len).copied();
		let Some(mapping) = found else {
			self.faults.record(FaultEvent { endpoint, domain, generation, address: Some(address), access, reason: Fault::NotMapped });
			return Err(Fault::NotMapped);
		};
		// A mapping made under a previous binding does not answer to this one, however live it looks.
		if mapping.generation != generation {
			self.faults.record(FaultEvent { endpoint, domain, generation, address: Some(address), access, reason: Fault::StaleGeneration });
			return Err(Fault::StaleGeneration);
		}
		if !mapping.direction.permits(access) {
			self.faults.record(FaultEvent { endpoint, domain, generation, address: Some(address), access, reason: Fault::Permission });
			return Err(Fault::Permission);
		}
		Ok(mapping.physical + (address.get() - mapping.iova.get()))
	}

	// Take what the hardware reported, bounded by the caller's buffer, and fold it into the log.
	//
	// AND STAMP EACH EVENT WITH THE BINDING IT BELONGS TO, which is this layer's answer and no other.
	// The backend reports an endpoint and the domain it is attached to and leaves the generation at
	// zero, because a backend does not own binding identity - it used to hold a controller-wide
	// generation of its own, which made two parallel answers to "which binding is this" and therefore
	// no answer at all. The domain carries the generation the kernel minted for that binding, so a
	// fault from an endpoint whose domain has since been rebound names the generation it was raised
	// under and is stale by comparison rather than by bookkeeping.
	//
	// An endpoint this side has no domain for keeps `Generation(0)`, which is a value no binding ever
	// carries: generations start at 1.
	pub fn drain_faults(&mut self, out: &mut [FaultEvent]) -> usize {
		let taken = self.backend.drain_faults(out);
		for event in out.iter_mut().take(taken) {
			if let Some(state) = self.domains.get(&event.domain) {
				event.generation = state.generation;
			}
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
