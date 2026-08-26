// The `virtio-iommu` wire format, and a `Backend` built on top of it.
//
// A SECOND IMPLEMENTATION OF A TESTED INTERFACE. `fake` was the first, and everything about ordering
// and rollback was settled against it; what is left here is the part that is genuinely about this
// device: which bytes go on the wire, which features must be negotiated before any of it means
// anything, and which status codes are which failure. All three are host-testable, and all three are
// where a driver written against a specification and never run against a malformed answer goes wrong.
//
// WHAT THIS MODULE DOES NOT DO. It does not touch a virtqueue, a BAR or a PCI function. `Transport`
// is that seam, and it is deliberately one method wide: hand these bytes to the device, put its
// answer here. The kernel supplies a real one; the tests supply one that answers however the case
// needs, including badly.
//
// INVALIDATION IS NOT A SEPARATE REQUEST HERE, and that is the specification's design rather than an
// omission: `virtio-iommu` requires the device to have completed the unmap - including any
// invalidation of its own caches - before it writes the status byte. So the contract's `invalidate`
// is satisfied by the unmap's OWN completion, and this backend says so explicitly rather than
// sending a request the device does not define. A backend that needed a separate invalidation and
// did not send one would be releasing frames on a promise; this one is releasing them on a
// completion, and the difference is written down here because it is invisible in the code.

use alloc::vec::Vec;

use crate::{Access, Backend, Confirmed, Direction, DmaAddress, DomainId, EndpointId, Fault, FaultEvent, Generation, ProbedRegion, RegionKind};

// Feature bits, as the specification numbers them.
pub const F_INPUT_RANGE: u64 = 1 << 0;
pub const F_DOMAIN_RANGE: u64 = 1 << 1;
pub const F_MAP_UNMAP: u64 = 1 << 2;
pub const F_BYPASS: u64 = 1 << 3;
pub const F_PROBE: u64 = 1 << 4;
pub const F_MMIO: u64 = 1 << 5;
pub const F_BYPASS_CONFIG: u64 = 1 << 6;

// Request types.
pub const T_ATTACH: u8 = 0x01;
pub const T_DETACH: u8 = 0x02;
pub const T_MAP: u8 = 0x03;
pub const T_UNMAP: u8 = 0x04;
pub const T_PROBE: u8 = 0x05;

// Property types the device may report. Only one is understood; anything else is skipped by its
// declared length, which is what the length field is for.
pub const PROBE_T_NONE: u16 = 0;
pub const PROBE_T_RESV_MEM: u16 = 1;
// The twelve low bits are the type; the top four are reserved and must not be read as part of it.
pub const PROBE_TYPE_MASK: u16 = 0x0fff;
pub const RESV_MEM_T_RESERVED: u8 = 0;
pub const RESV_MEM_T_MSI: u8 = 1;

// Status codes.
pub const S_OK: u8 = 0;
pub const S_IOERR: u8 = 1;
pub const S_UNSUPP: u8 = 2;
pub const S_DEVERR: u8 = 3;
pub const S_INVAL: u8 = 4;
pub const S_RANGE: u8 = 5;
pub const S_NOENT: u8 = 6;
pub const S_FAULT: u8 = 7;
pub const S_NOMEM: u8 = 8;

// Mapping permission flags.
pub const MAP_F_READ: u32 = 1 << 0;
pub const MAP_F_WRITE: u32 = 1 << 1;
pub const MAP_F_MMIO: u32 = 1 << 2;

// Fault reasons and flags, as reported on the event queue.
pub const FAULT_R_UNKNOWN: u8 = 0;
pub const FAULT_R_DOMAIN: u8 = 1;
pub const FAULT_R_MAPPING: u8 = 2;
pub const FAULT_F_READ: u32 = 1 << 0;
pub const FAULT_F_WRITE: u32 = 1 << 1;
pub const FAULT_F_ADDRESS: u32 = 1 << 8;

// The wire sizes. Named because a request that is one byte short is a request the device answers
// with `INVAL` at best and misreads at worst.
// EVERY ONE OF THESE IS A RESERVED FIELD LONGER THAN IT LOOKS. `attach` carries four reserved bytes
// after its flags and `detach` carries eight, so both are twenty rather than the sixteen the named
// fields add up to - and a request four bytes short is answered `VIRTIO_IOMMU_S_INVAL` by a device
// that is behaving perfectly. That is precisely how this was wrong the first time, and the QEMU
// fixture is what said so.
pub const REQ_ATTACH_LEN: usize = 20;
pub const REQ_DETACH_LEN: usize = 20;
pub const REQ_MAP_LEN: usize = 36;
pub const REQ_UNMAP_LEN: usize = 28;
// head(4) + endpoint(4) + reserved(64). What the DEVICE writes after it is `probe_size` bytes of
// properties and then the tail, which is why the probe's device-writable buffer is not `TAIL_LEN`.
pub const REQ_PROBE_LEN: usize = 72;
// One property header: `le16 type` (the low twelve bits) and `le16 length`.
pub const PROBE_PROPERTY_HEADER: usize = 4;
// A `RESV_MEM` property's body: subtype(1) + reserved(3) + start(8) + end(8).
pub const PROBE_RESV_MEM_LEN: usize = 20;
pub const TAIL_LEN: usize = 4;
pub const FAULT_LEN: usize = 24;

// THE FEATURES THIS BACKEND REQUIRES, and the reason each one is not optional.
//
// `MAP_UNMAP` is the whole mechanism. `INPUT_RANGE` is how the device says which addresses translate
// at all, and without it a kernel would be guessing at the space it hands out. `DOMAIN_RANGE` bounds
// the domain ids. A device offering none of these is not one this backend can enforce with, and
// saying so is a refusal rather than a degraded mode.
pub const REQUIRED: u64 = F_MAP_UNMAP | F_INPUT_RANGE | F_DOMAIN_RANGE;

// What to accept from what the device offers: the required set plus the optional ones this code
// actually implements. NEVER THE DEVICE'S WHOLE OFFER - acknowledging a feature is a promise to
// implement it, and acknowledging one blindly is how a driver ends up with a device speaking a
// protocol it does not.
pub fn negotiate(offered: u64) -> Result<u64, Fault> {
	if offered & REQUIRED != REQUIRED {
		return Err(Fault::Unconfirmed);
	}
	Ok(offered & (REQUIRED | F_BYPASS_CONFIG | F_PROBE))
}

// The device's configuration space, as far as this backend reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
	pub page_size_mask: u64,
	pub input_start: u64,
	pub input_end: u64,
	pub domain_start: u32,
	pub domain_end: u32,
	pub probe_size: u32,
	pub bypass: u8,
}

fn le32(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn le64(bytes: &[u8], at: usize) -> u64 {
	let mut out = [0u8; 8];
	out.copy_from_slice(&bytes[at..at + 8]);
	u64::from_le_bytes(out)
}

impl Config {
	// Read and VALIDATE. Every field here comes from the device, which in the threat model this
	// milestone is written for is emulated by something that may be wrong or hostile: an input range
	// that ends before it starts, or a domain range that does the same, is refused rather than
	// turned into an allocator that hands out addresses nothing translates.
	pub fn parse(bytes: &[u8]) -> Result<Config, Fault> {
		if bytes.len() < 36 {
			return Err(Fault::Malformed);
		}
		let config = Config { page_size_mask: le64(bytes, 0), input_start: le64(bytes, 8), input_end: le64(bytes, 16), domain_start: le32(bytes, 24), domain_end: le32(bytes, 28), probe_size: le32(bytes, 32), bypass: if bytes.len() > 36 { bytes[36] } else { 0 } };
		if config.input_end < config.input_start || config.domain_end < config.domain_start {
			return Err(Fault::Malformed);
		}
		// A page-size mask with no bit set describes a device that can map nothing.
		if config.page_size_mask == 0 {
			return Err(Fault::Malformed);
		}
		Ok(config)
	}

	// The smallest page this device supports, which is the alignment every mapping must satisfy.
	pub fn smallest_page(&self) -> u64 {
		1u64 << self.page_size_mask.trailing_zeros()
	}

	pub fn input_len(&self) -> u64 {
		// The range is inclusive at both ends, so a range of one page has `end = start + page - 1`.
		self.input_end - self.input_start + 1
	}

	pub fn contains(&self, address: u64, len: u64) -> bool {
		match address.checked_add(len) {
			Some(end) => address >= self.input_start && end - 1 <= self.input_end,
			None => false,
		}
	}
}

// The encoders. Each writes exactly the bytes the specification names, in little-endian, with the
// reserved fields zeroed - a reserved field left uninitialised is a field the device is entitled to
// reject.
pub fn encode_attach(domain: u32, endpoint: u32, flags: u32) -> Vec<u8> {
	let mut out = Vec::with_capacity(REQ_ATTACH_LEN);
	out.extend_from_slice(&[T_ATTACH, 0, 0, 0]);
	out.extend_from_slice(&domain.to_le_bytes());
	out.extend_from_slice(&endpoint.to_le_bytes());
	out.extend_from_slice(&flags.to_le_bytes());
	out.extend_from_slice(&[0u8; 4]);
	out
}

pub fn encode_detach(domain: u32, endpoint: u32) -> Vec<u8> {
	let mut out = Vec::with_capacity(REQ_DETACH_LEN);
	out.extend_from_slice(&[T_DETACH, 0, 0, 0]);
	out.extend_from_slice(&domain.to_le_bytes());
	out.extend_from_slice(&endpoint.to_le_bytes());
	out.extend_from_slice(&[0u8; 8]);
	out
}

// THE END IS INCLUSIVE, which is the single most common way to get this format wrong: a mapping
// written with an exclusive end is one page longer than it was meant to be, and the extra page is
// somebody else's.
pub fn encode_map(domain: u32, virt_start: u64, len: u64, phys_start: u64, direction: Direction) -> Result<Vec<u8>, Fault> {
	if len == 0 {
		return Err(Fault::Malformed);
	}
	let virt_end = virt_start.checked_add(len - 1).ok_or(Fault::Malformed)?;
	let mut flags = 0u32;
	if direction.device_may_read() {
		flags |= MAP_F_READ;
	}
	if direction.device_may_write() {
		flags |= MAP_F_WRITE;
	}
	let mut out = Vec::with_capacity(REQ_MAP_LEN);
	out.extend_from_slice(&[T_MAP, 0, 0, 0]);
	out.extend_from_slice(&domain.to_le_bytes());
	out.extend_from_slice(&virt_start.to_le_bytes());
	out.extend_from_slice(&virt_end.to_le_bytes());
	out.extend_from_slice(&phys_start.to_le_bytes());
	out.extend_from_slice(&flags.to_le_bytes());
	Ok(out)
}

pub fn encode_unmap(domain: u32, virt_start: u64, len: u64) -> Result<Vec<u8>, Fault> {
	if len == 0 {
		return Err(Fault::Malformed);
	}
	let virt_end = virt_start.checked_add(len - 1).ok_or(Fault::Malformed)?;
	let mut out = Vec::with_capacity(REQ_UNMAP_LEN);
	out.extend_from_slice(&[T_UNMAP, 0, 0, 0]);
	out.extend_from_slice(&domain.to_le_bytes());
	out.extend_from_slice(&virt_start.to_le_bytes());
	out.extend_from_slice(&virt_end.to_le_bytes());
	out.extend_from_slice(&0u32.to_le_bytes());
	Ok(out)
}

// Ask what one endpoint's domain must treat specially.
//
// The reserved field is 64 bytes of zeroes the device ignores; it is written out rather than
// skipped, because the request's length is what tells the device which request this is.
pub fn encode_probe(endpoint: u32) -> Vec<u8> {
	let mut out = Vec::with_capacity(REQ_PROBE_LEN);
	out.extend_from_slice(&[T_PROBE, 0, 0, 0]);
	out.extend_from_slice(&endpoint.to_le_bytes());
	out.resize(REQ_PROBE_LEN, 0);
	out
}

// What the device wrote back, as regions rather than as bytes.
//
// EVERY PROPERTY IS SKIPPED BY ITS OWN DECLARED LENGTH, so a type this driver does not know costs it
// nothing - which is the point of the length field and the only way this survives a device that
// reports more than it did when this was written. A property whose length runs past the buffer ends
// the walk: the device is describing memory it did not send, and the properties behind it cannot be
// located any more.
pub fn decode_probe(properties: &[u8]) -> Vec<ProbedRegion> {
	let mut out = Vec::new();
	let mut at = 0usize;
	while at + PROBE_PROPERTY_HEADER <= properties.len() {
		let kind = u16::from_le_bytes([properties[at], properties[at + 1]]) & PROBE_TYPE_MASK;
		let length = u16::from_le_bytes([properties[at + 2], properties[at + 3]]) as usize;
		// A zero type terminates the list, which is how the device says "that is all of them".
		if kind == PROBE_T_NONE {
			break;
		}
		let body = at + PROBE_PROPERTY_HEADER;
		let Some(end) = body.checked_add(length) else { break };
		if end > properties.len() {
			break;
		}
		if kind == PROBE_T_RESV_MEM && length >= PROBE_RESV_MEM_LEN {
			let subtype = properties[body];
			let start = le64(properties, body + 4);
			// INCLUSIVE ON THE WIRE, a length everywhere else. A region whose end is below its start
			// is a device contradicting itself and is dropped rather than turned into a huge one.
			let last = le64(properties, body + 12);
			if last >= start {
				let kind = if subtype == RESV_MEM_T_MSI { RegionKind::MsiDoorbell } else { RegionKind::Reserved };
				out.push(ProbedRegion { kind, base: start, len: last - start + 1 });
			}
		}
		at = end;
	}
	out
}

// The device's answer. A tail shorter than the specification's is not a status, and a status this
// code does not know is not an success.
pub fn decode_status(tail: &[u8]) -> Result<(), Fault> {
	if tail.len() < TAIL_LEN {
		return Err(Fault::Unconfirmed);
	}
	match tail[0] {
		S_OK => Ok(()),
		S_RANGE => Err(Fault::OutOfRange),
		S_INVAL => Err(Fault::Malformed),
		S_NOENT => Err(Fault::NotMapped),
		S_NOMEM => Err(Fault::NoSpace),
		S_UNSUPP => Err(Fault::Unconfirmed),
		// EVERY OTHER CODE IS UNCONFIRMED, including ones added after this was written. A device
		// error whose meaning this code does not know is a state the kernel does not know either,
		// and the one thing it must not do with an unknown state is release a frame.
		_ => Err(Fault::Unconfirmed),
	}
}

// One event off the event queue, validated.
//
// A MALFORMED EVENT IS DROPPED, NOT INTERPRETED. The event queue is filled by the device, and this
// milestone's threat model includes a device that is wrong: a short record, an unknown reason or an
// address flag that is not set are all "this told me nothing", and a fault the kernel invents from
// nothing is worse than one it did not see.
pub fn decode_fault(bytes: &[u8], generation: Generation, domain: DomainId) -> Result<FaultEvent, Fault> {
	if bytes.len() < FAULT_LEN {
		return Err(Fault::Malformed);
	}
	let reason = bytes[0];
	let flags = le32(bytes, 4);
	let endpoint = le32(bytes, 8);
	let address = le64(bytes, 16);
	// The address is only meaningful when the device says it is.
	if flags & FAULT_F_ADDRESS == 0 {
		return Err(Fault::Malformed);
	}
	let access = if flags & FAULT_F_WRITE != 0 {
		Access::Write
	} else if flags & FAULT_F_READ != 0 {
		Access::Read
	} else {
		return Err(Fault::Malformed);
	};
	let reason = match reason {
		FAULT_R_DOMAIN => Fault::UnknownEndpoint,
		FAULT_R_MAPPING => Fault::NotMapped,
		FAULT_R_UNKNOWN => Fault::Unconfirmed,
		_ => return Err(Fault::Malformed),
	};
	Ok(FaultEvent { endpoint: EndpointId(endpoint), domain, generation, address: DmaAddress(address), access, reason })
}

// The seam between the codec above and a virtqueue. One method wide on purpose.
pub trait Transport {
	// Hand the device a request and place what it writes in `answer`. Returns only once the device
	// has answered - which for this device means the operation has TAKEN EFFECT, not been accepted.
	//
	// `status_at` is where the status byte sits inside `answer`: zero for every request whose
	// device-writable part IS the tail, and `probe_size` for a probe, which the device fills with
	// properties first. The transport is told rather than left to assume, because a transport that
	// assumes reads a property's type as a status - which is what one did, printing a refusal for
	// every successful probe.
	fn request(&mut self, request: &[u8], answer: &mut [u8], status_at: usize) -> Result<(), Fault>;
	// Take one raw event off the event queue, if there is one. Returns how many bytes were written.
	fn take_event(&mut self, out: &mut [u8]) -> usize;
}

// The backend itself: the contract's operations, in this device's words.
pub struct VirtioIommu<T: Transport> {
	transport: T,
	config: Config,
	features: u64,
	next_domain: u32,
	// Every live binding: which domain an endpoint is in, and the generation it was attached under.
	//
	// THE DEVICE REPORTS AN ENDPOINT AND NOTHING ELSE. A fault event names the requester that
	// faulted; which domain that requester is in, and which binding of it, is knowledge only this
	// side has - so it is kept here rather than guessed at from whichever binding happens to be
	// first. The generation is stamped at attach so a later rebind does not restamp the bindings
	// that came before it.
	attached: Vec<(DomainId, EndpointId, Generation)>,
	// The generation the NEXT attach is stamped with. The kernel owns what it means; this holds it.
	generation: Generation,
}

impl<T: Transport> VirtioIommu<T> {
	// Build a backend from a device that has already been probed. `features` must be what
	// `negotiate` returned for what the device offered, and `config` what it published.
	pub fn new(transport: T, config: Config, features: u64, generation: Generation) -> Result<Self, Fault> {
		if features & REQUIRED != REQUIRED {
			return Err(Fault::Unconfirmed);
		}
		Ok(Self { transport, config, features, next_domain: config.domain_start.max(1), attached: Vec::new(), generation })
	}

	pub fn config(&self) -> &Config {
		&self.config
	}

	pub fn features(&self) -> u64 {
		self.features
	}

	// The wire this backend speaks over. For a test that means to check WHAT was sent rather than
	// only what came back - the two are different claims and the second alone would pass a backend
	// that sent the wrong request and got a polite answer.
	#[cfg(test)]
	pub fn transport(&self) -> &T {
		&self.transport
	}

	pub fn set_generation(&mut self, generation: Generation) {
		self.generation = generation;
	}

	fn send(&mut self, request: &[u8]) -> Result<Confirmed, Fault> {
		let mut tail = [0u8; TAIL_LEN];
		self.transport.request(request, &mut tail, 0)?;
		decode_status(&tail)?;
		// THE STATUS IS THE COMPLETION. See the note at the top of this file: this device writes it
		// only once the operation has taken effect, which is what lets a confirmed unmap be a
		// confirmed invalidation too.
		Ok(Confirmed::by_backend())
	}
}

impl<T: Transport> Backend for VirtioIommu<T> {
	fn domain_create(&mut self) -> Result<(DomainId, Confirmed), Fault> {
		// A DOMAIN IS CREATED BY BEING ATTACHED TO. The device has no create request: the first
		// attach naming an id brings it into existence. What this method owns is the id, and the
		// bound the device published on it.
		if self.next_domain > self.config.domain_end {
			return Err(Fault::NoSpace);
		}
		let id = DomainId(self.next_domain);
		self.next_domain += 1;
		Ok((id, Confirmed::by_backend()))
	}

	fn domain_destroy(&mut self, domain: DomainId) -> Result<Confirmed, Fault> {
		// Likewise: a domain with nothing attached does not exist. Detaching what is left is the
		// destruction, and it must CONFIRM - a domain believed destroyed while an endpoint is still
		// attached is an endpoint still translating.
		//
		// THE TRANSLATIONS ARE NOT THIS METHOD'S TO REMOVE, and that is a division of labour rather
		// than an omission: a backend holds no list of what is mapped in a domain - the generic layer
		// above it does - so a `domain_destroy` that tried to unmap would be guessing at a list it
		// cannot have. `Iommu::revoke_endpoint` takes every translation down while the endpoint is
		// still attached and only then detaches, so by the time this is reached the domain has
		// nothing left to translate.
		let attached: Vec<EndpointId> = self.attached.iter().filter(|(d, _, _)| *d == domain).map(|(_, e, _)| *e).collect();
		for endpoint in attached {
			self.detach(domain, endpoint)?;
		}
		Ok(Confirmed::by_backend())
	}

	fn attach(&mut self, domain: DomainId, endpoint: EndpointId) -> Result<Confirmed, Fault> {
		if domain.0 < self.config.domain_start || domain.0 > self.config.domain_end {
			return Err(Fault::OutOfRange);
		}
		// FLAGS ZERO, and deliberately so: `VIRTIO_IOMMU_ATTACH_F_BYPASS` attaches an endpoint that
		// is not translated at all, which is the one thing an enforcing profile must never send.
		let confirmed = self.send(&encode_attach(domain.0, endpoint.0, 0))?;
		self.attached.push((domain, endpoint, self.generation));
		Ok(confirmed)
	}

	fn detach(&mut self, domain: DomainId, endpoint: EndpointId) -> Result<Confirmed, Fault> {
		let confirmed = self.send(&encode_detach(domain.0, endpoint.0))?;
		self.attached.retain(|(d, e, _)| !(*d == domain && *e == endpoint));
		Ok(confirmed)
	}

	fn map(&mut self, domain: DomainId, iova: DmaAddress, physical: u64, len: u64, direction: Direction) -> Result<Confirmed, Fault> {
		// THE DEVICE'S OWN RANGE IS CHECKED BEFORE THE DEVICE IS ASKED. A request outside the
		// published input range is one the device is entitled to answer any way it likes, and the
		// kernel would then be reading a status to find out something it already knew.
		if !self.config.contains(iova.get(), len) {
			return Err(Fault::OutOfRange);
		}
		if iova.get() % self.config.smallest_page() != 0 || len % self.config.smallest_page() != 0 {
			return Err(Fault::Malformed);
		}
		self.send(&encode_map(domain.0, iova.get(), len, physical, direction)?)
	}

	fn unmap(&mut self, domain: DomainId, iova: DmaAddress, len: u64) -> Result<Confirmed, Fault> {
		self.send(&encode_unmap(domain.0, iova.get(), len)?)
	}

	fn invalidate(&mut self, _domain: DomainId) -> Result<Confirmed, Fault> {
		// See the note at the top of this file. The unmap's status IS the invalidation's completion
		// for this device, so this is confirmed by construction rather than by a request nobody
		// defines - and it is written here rather than left as an empty method with no comment,
		// because "this does nothing" and "this is already done" are different facts.
		Ok(Confirmed::by_backend())
	}

	fn drain_faults(&mut self, out: &mut [FaultEvent]) -> usize {
		let mut written = 0;
		let mut raw = [0u8; FAULT_LEN];
		while written < out.len() {
			let taken = self.transport.take_event(&mut raw);
			if taken == 0 {
				break;
			}
			// A malformed record is DROPPED and the drain continues: one bad event must not stop
			// the kernel reading the good ones behind it.
			//
			// THE BINDING COMES FROM THE ENDPOINT THE DEVICE NAMED. This used to take the first
			// entry of `attached` for every event, so on a machine with more than one endpoint -
			// which is every machine this backend was written for - each fault after the first
			// device named the wrong domain, and the quarantine and stale-generation decisions
			// taken on it were about a binding that had not faulted. An endpoint this side has no
			// binding for keeps `DomainId(0)`, which is not a domain this backend ever hands out.
			let Ok(mut event) = decode_fault(&raw[..taken.min(FAULT_LEN)], self.generation, DomainId(0)) else {
				continue;
			};
			if let Some((domain, _, generation)) = self.attached.iter().find(|(_, e, _)| *e == event.endpoint) {
				event.domain = *domain;
				event.generation = *generation;
			}
			out[written] = event;
			written += 1;
		}
		written
	}

	fn enforces_directions(&self) -> bool {
		// `MAP_F_READ` and `MAP_F_WRITE` are separate bits and this backend sets them from the
		// mapping's direction, so all three directions are expressible. The feature that carries
		// them is `MAP_UNMAP`, which `new` refuses to build without.
		self.features & F_MAP_UNMAP != 0
	}

	// ASK THE DEVICE RATHER THAN COMPILE IN AN ADDRESS.
	//
	// `negotiate` has acknowledged `F_PROBE` since this backend was written and nothing ever sent a
	// probe, so the reserved regions were an empty list by omission - and the one that matters is the
	// MSI doorbell. An endpoint whose domain has no mapping for it writes its interrupt into an
	// address the IOMMU cannot translate, the write is dropped, no fault is raised because a dropped
	// write is not a translation the device asked about, and the driver waits forever for an
	// interrupt that was never delivered. That is what this returns the regions for.
	fn probe(&mut self, endpoint: EndpointId) -> Result<Vec<ProbedRegion>, Fault> {
		// A device that did not offer the feature has nothing to say, and asking anyway would be a
		// request it is entitled to answer with an error.
		if self.features & F_PROBE == 0 || self.config.probe_size == 0 {
			return Ok(Vec::new());
		}
		// The device writes the properties and then the tail, so one buffer holds both.
		let size = self.config.probe_size as usize;
		let mut answer = Vec::new();
		if answer.try_reserve_exact(size + TAIL_LEN).is_err() {
			return Err(Fault::NoSpace);
		}
		answer.resize(size + TAIL_LEN, 0);
		self.transport.request(&encode_probe(endpoint.0), &mut answer, size)?;
		decode_status(&answer[size..])?;
		Ok(decode_probe(&answer[..size]))
	}
}
