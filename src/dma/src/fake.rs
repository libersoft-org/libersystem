// A backend that does what a backend does, and fails wherever it is told to.
//
// WHY A FAKE IS THE POINT RATHER THAN A CONVENIENCE. Every interesting rule in this crate is about
// what happens when an operation does NOT complete: an unmap that is refused, an invalidation that
// never confirms, an attach that fails after the domain was created. Against real hardware those are
// unreachable on demand; here each is one line, and the orders they can arrive in are enumerable.
//
// It also records the call order, because several of the rules are about ORDER - unmap before
// invalidate, attach before the first map - and a test that only checks the end state would pass a
// backend that did them backwards.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::{Access, Backend, Confirmed, Direction, DmaAddress, DomainId, EndpointId, Fault, FaultEvent, Generation};

// Which operation a fake was told to refuse, and how.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Injection {
	DomainCreate,
	Attach,
	Detach,
	Map,
	Unmap,
	Invalidate,
}

// What the fake was asked to do, in the order it was asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Call {
	DomainCreate(DomainId),
	DomainDestroy(DomainId),
	Attach(DomainId, EndpointId),
	Detach(DomainId, EndpointId),
	Map(DomainId, DmaAddress, u64),
	Unmap(DomainId, DmaAddress, u64),
	Invalidate(DomainId),
}

pub struct Fake {
	next_domain: u32,
	// What each domain currently translates, so a test can check the fake's own idea of the hardware
	// state against the kernel's - the two disagreeing is the interesting failure.
	installed: BTreeMap<(DomainId, u64), (u64, u64, Direction)>,
	attached: Vec<(DomainId, EndpointId)>,
	calls: Vec<Call>,
	// Each injection fires once and then clears itself: a fault that never stops is a fault that
	// prevents the test from checking what happens afterwards.
	injected: Vec<(Injection, Fault)>,
	pending_faults: Vec<FaultEvent>,
	directions: bool,
	// What this fake's transport claims it can hold at once, for the tests that drive the bound on
	// a detached tail. `None` is the default and means "cannot say", which is what an ordinary
	// backend answers.
	fault_queue_capacity: Option<u64>,
}

impl Default for Fake {
	fn default() -> Self {
		Self::new()
	}
}

impl Fake {
	pub fn new() -> Self {
		Self { next_domain: 1, installed: BTreeMap::new(), attached: Vec::new(), calls: Vec::new(), injected: Vec::new(), pending_faults: Vec::new(), directions: true, fault_queue_capacity: None }
	}

	// State what this fake's transport can hold at once, so the bound on a detached tail is
	// reachable by a test.
	pub fn with_fault_queue_capacity(mut self, most: u64) -> Self {
		self.fault_queue_capacity = Some(most);
		self
	}

	// Refuse the next call of this kind with this fault, once.
	pub fn inject(&mut self, which: Injection, fault: Fault) {
		self.injected.push((which, fault));
	}

	// A backend that cannot enforce directions. Used to check that the enforcing profile refuses it
	// rather than mapping everything read-write.
	pub fn without_direction_support(mut self) -> Self {
		self.directions = false;
		self
	}

	pub fn queue_fault(&mut self, event: FaultEvent) {
		self.pending_faults.push(event);
	}

	pub fn calls(&self) -> &[Call] {
		&self.calls
	}

	// Whether the fake believes this address is translated - the hardware's view, against which the
	// kernel's bookkeeping is checked.
	pub fn translates(&self, domain: DomainId, address: DmaAddress) -> Option<(u64, Direction)> {
		self.installed.range(..=(domain, address.get())).next_back().and_then(|((d, base), (physical, len, direction))| if *d == domain && address.get() < base + len { Some((physical + (address.get() - base), *direction)) } else { None })
	}

	pub fn installed_ranges(&self) -> usize {
		self.installed.len()
	}

	pub fn attachments(&self) -> usize {
		self.attached.len()
	}

	fn take(&mut self, which: Injection) -> Option<Fault> {
		let at = self.injected.iter().position(|(kind, _)| *kind == which)?;
		Some(self.injected.remove(at).1)
	}
}

impl Backend for Fake {
	fn domain_create(&mut self) -> Result<(DomainId, Confirmed), Fault> {
		if let Some(fault) = self.take(Injection::DomainCreate) {
			return Err(fault);
		}
		let id = DomainId(self.next_domain);
		self.next_domain += 1;
		self.calls.push(Call::DomainCreate(id));
		Ok((id, Confirmed::by_backend()))
	}

	fn domain_destroy(&mut self, domain: DomainId) -> Result<Confirmed, Fault> {
		self.calls.push(Call::DomainDestroy(domain));
		self.installed.retain(|(d, _), _| *d != domain);
		self.attached.retain(|(d, _)| *d != domain);
		Ok(Confirmed::by_backend())
	}

	fn attach(&mut self, domain: DomainId, endpoint: EndpointId) -> Result<Confirmed, Fault> {
		if let Some(fault) = self.take(Injection::Attach) {
			return Err(fault);
		}
		self.calls.push(Call::Attach(domain, endpoint));
		self.attached.push((domain, endpoint));
		Ok(Confirmed::by_backend())
	}

	fn detach(&mut self, domain: DomainId, endpoint: EndpointId) -> Result<Confirmed, Fault> {
		if let Some(fault) = self.take(Injection::Detach) {
			return Err(fault);
		}
		self.calls.push(Call::Detach(domain, endpoint));
		self.attached.retain(|pair| *pair != (domain, endpoint));
		Ok(Confirmed::by_backend())
	}

	fn map(&mut self, domain: DomainId, iova: DmaAddress, physical: u64, len: u64, direction: Direction) -> Result<Confirmed, Fault> {
		if let Some(fault) = self.take(Injection::Map) {
			return Err(fault);
		}
		// A REAL BACKEND REFUSES AN OVERLAP AND SO DOES THIS ONE. A fake that quietly accepted two
		// mappings of one address would hide the very confusion the IOVA space exists to prevent.
		let overlaps = self.installed.iter().any(|((d, base), (_, length, _))| *d == domain && iova.get() < base + length && *base < iova.get() + len);
		if overlaps {
			return Err(Fault::Overlaps);
		}
		self.calls.push(Call::Map(domain, iova, len));
		self.installed.insert((domain, iova.get()), (physical, len, direction));
		Ok(Confirmed::by_backend())
	}

	fn unmap(&mut self, domain: DomainId, iova: DmaAddress, len: u64) -> Result<Confirmed, Fault> {
		if let Some(fault) = self.take(Injection::Unmap) {
			return Err(fault);
		}
		self.calls.push(Call::Unmap(domain, iova, len));
		self.installed.remove(&(domain, iova.get()));
		Ok(Confirmed::by_backend())
	}

	fn invalidate(&mut self, domain: DomainId) -> Result<Confirmed, Fault> {
		if let Some(fault) = self.take(Injection::Invalidate) {
			return Err(fault);
		}
		self.calls.push(Call::Invalidate(domain));
		Ok(Confirmed::by_backend())
	}

	fn transport_was_emptied(&self) -> bool {
		self.pending_faults.is_empty()
	}

	fn fault_queue_capacity(&self) -> Option<u64> {
		self.fault_queue_capacity
	}

	fn drain_faults(&mut self, out: &mut [FaultEvent]) -> usize {
		let taken = out.len().min(self.pending_faults.len());
		for (slot, event) in out.iter_mut().zip(self.pending_faults.drain(..taken)) {
			*slot = event;
		}
		taken
	}

	fn enforces_directions(&self) -> bool {
		self.directions
	}
}

// A fault event with the shape a test usually wants, so a queued one is one line rather than seven.
pub fn event(endpoint: u32, domain: u32, address: u64, access: Access, reason: Fault) -> FaultEvent {
	FaultEvent { endpoint: EndpointId(endpoint), domain: DomainId(domain), generation: Generation(1), address: Some(DmaAddress(address)), access, reason }
}

// The same, for a controller that faulted and did not record where.
pub fn event_without_address(endpoint: u32, domain: u32, access: Access, reason: Fault) -> FaultEvent {
	FaultEvent { endpoint: EndpointId(endpoint), domain: DomainId(domain), generation: Generation(1), address: None, access, reason }
}
