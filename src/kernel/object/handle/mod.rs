// Per-process handle table and the capability records it holds.
//
// A Handle is an opaque, per-process token (like a file descriptor) that indexes
// a slot holding a Capability. Userspace never sees a Capability directly - only
// the kernel does. Each slot carries a generation, so a stale handle (to a closed
// and possibly reused slot) is reliably rejected.
//
// A HandleTable is owned by a process. It is not internally locked; the owner
// wraps it in a SpinLock when it is shared between a process's threads.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::domain::Domain;
use super::rights::Rights;
use super::{KernelObject, ObjectType};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandleError {
	BadHandle,
	WrongType,
	AccessDenied,
	Revoked,
	// The Domain's handle quota is full. Distinct from AccessDenied on purpose: the
	// caller had the right and the resource was not there, which is a different thing to
	// tell an operator and a different thing for a caller to retry.
	LimitReached,
}

// Opaque per-process handle: packs (slot generation, slot index).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Handle(u64);

impl Handle {
	fn new(generation: u32, index: u32) -> Self {
		Handle(((generation as u64) << 32) | index as u64)
	}

	fn index(self) -> u32 {
		self.0 as u32
	}

	fn generation(self) -> u32 {
		(self.0 >> 32) as u32
	}

	// The raw token value (an opaque handle id from userspace's point of view).
	pub fn raw(self) -> u64 {
		self.0
	}

	// Reconstruct a handle from its raw token (e.g. a syscall argument).
	pub fn from_raw(raw: u64) -> Self {
		Handle(raw)
	}
}

// A capability = a reference to a kernel object + a set of rights.
//
// NO BADGE. One was carried here, stamped onto every message the handle sent, and read by nothing:
// there was no syscall that could set it, so it was zero for every capability in every table, and
// no syscall that could read it back, so a receiver could not have used it either. It was half of a
// design this system does not use - servers here give each client its own channel and know who is
// speaking from which channel the message arrived on, which is the same question answered without
// the kernel labelling anything. If the other design is ever wanted, it arrives with the two
// syscalls that were always missing.
// Held only inside the kernel (in a handle table or a message in transit).
pub struct Capability {
	object: Arc<dyn KernelObject>,
	rights: Rights,
	generation: u32,
}

impl Capability {
	pub fn new(object: Arc<dyn KernelObject>, rights: Rights) -> Self {
		let generation = object.header().generation();
		Self { object, rights, generation }
	}

	pub fn object_type(&self) -> ObjectType {
		self.object.object_type()
	}

	// The kernel object this capability refers to (a new reference). Used by
	// kernel-internal paths that receive a transferred capability and need to act
	// on the object directly, without a handle table to install it into.
	pub fn object(&self) -> Arc<dyn KernelObject> {
		self.object.clone()
	}

	// The same capability with less authority: the INTERSECTION of what it holds and `mask`.
	//
	// NARROWING ONLY, and structurally so - an intersection cannot widen, so there is no way to use
	// this to hand out more than the caller had. That is why it exists as an intersection rather
	// than as an assignment: a mask naming a right the capability does not hold is not an error,
	// because it cannot become one.
	//
	// The generation is CARRIED, not re-snapshotted. Minting a fresh capability with
	// `Capability::new` would read the object's generation as it is NOW, which would quietly
	// resurrect a capability whose object was revoked between the lookup and here.
	pub fn attenuated(&self, mask: Rights) -> Capability {
		Self { object: self.object.clone(), rights: self.rights & mask, generation: self.generation }
	}

	// A capability is stale once the object's generation has moved past the one
	// captured at mint time (i.e. the object was revoked).
	fn is_valid(&self) -> bool {
		self.object.header().generation() == self.generation
	}
}

// A read-only snapshot of one handle's capability, for introspection (the
// object_info_get syscall and the System Graph). It names the object behind the
// handle and the access the handle confers, without exposing the capability.
#[derive(Clone, Copy, Debug)]
pub struct HandleInfo {
	pub koid: u64,
	pub object_type: ObjectType,
	pub rights: Rights,
	pub generation: u32,
}

impl HandleInfo {
	// Snapshot a capability's introspection fields, so the info / entries queries
	// map them in exactly one place.
	fn from_cap(cap: &Capability) -> HandleInfo {
		HandleInfo { koid: cap.object.header().koid(), object_type: cap.object_type(), rights: cap.rights, generation: cap.object.header().generation() }
	}
}

struct Slot {
	cap: Option<Capability>,
	generation: u32,
	// A transfer is in flight over this slot: `take_for_transfer` emptied it and exactly one of
	// `commit_taken` or `restore_taken` will follow.
	//
	// The state used to be implicit - cap `None` and the index absent from the free list - which no
	// caller could test and `close_all` therefore could not respect. It cleared the free list and
	// pushed EVERY index, so a termination racing a transfer put the reserved slot back in
	// circulation: the following `restore_taken` wrote a live capability into a slot that was
	// simultaneously free, and the next insert could hand the same index out again. On the other
	// branch `commit_taken` pushed an index that was already there, and the free list held it twice.
	//
	// A `Free | Live | TransferReserved | Retired` enum is the shape this approximates; the flag
	// carries the one state that was unrepresentable, and `Retired` stays as it was (a generation
	// that can no longer be advanced), which `close_all` now also respects.
	reserved: bool,
}

pub struct HandleTable {
	slots: Vec<Slot>,
	free: Vec<u32>,
	// CONCRETE SLOTS BOOKED FOR AN UPCOMING `insert_reserved`, by index.
	//
	// `reserve` used to reserve vector CAPACITY and Domain quota and nothing else - it did not take
	// the free indices out of circulation. Another thread sharing this process's table could then
	// insert handles between the reservation and its use, consume the free slots, and leave
	// `insert_reserved` calling `place` with nothing to place into: it answers with the raw ZERO
	// handle, which names nothing. In the MSI path the hardware has already been programmed by then,
	// and in channel receive the message and its capabilities have already been committed - so the
	// caller receives handle zero while the capability is dropped, and the quota charged at
	// reservation no longer corresponds to an installed handle.
	//
	// Holding the indices themselves is what makes the reservation a booking.
	booked: Vec<u32>,
	// CLOSED once `close_all` has run: this table is a dead process's and takes nothing new.
	//
	// `close_all` skips reserved slots, which is right - their capability is elsewhere and one of
	// the transfer's outcomes is still to come. `restore_taken` then put the capability back
	// unconditionally, into a table whose process had finished tearing down. Accounting stayed
	// consistent (the take never refunded, and `Drop for HandleTable` refunds what is live), so what
	// actually happened was that an object stayed alive in a dead process's table until the last
	// reference to the `Process` went - a delayed release rather than a leak. It was still true that
	// `close_all` was not the terminal barrier its name and its use imply, and that is what this
	// makes it.
	closed: bool,
	// WHICH TABLE THIS IS, in the model's vocabulary. The model's `Procs` is a small set of tables,
	// and a trace has to say which one an action was in; the conformance driver names them and every
	// other table is zero. One byte in both configurations, because the alternative is a `cfg` on
	// every line that reads it.
	trace_id: u16,
	// The Domain whose handle quota this table charges. None for tables not tied
	// to a Domain (e.g. unit-test tables), which skip accounting entirely.
	domain: Option<Arc<Domain>>,
}

// Advance a freed slot's generation and return it to the free list - unless doing so
// would wrap it back to a value it has already used.
//
// The generation is what makes a closed handle stay closed: a raw handle names a slot
// and the generation it expects, and a mismatch is a bad handle. A wrapping increment
// therefore has an end: after 2^32 recycles of one slot the counter comes back round and
// a long-dead handle matches again. It takes deliberate churn to reach, and "deliberate
// churn" is the threat model.
//
// A slot at the end of its generations is simply not reused. One slot of a table is a
// negligible loss; a handle coming back from the dead is not.
fn retire_or_recycle(slot: &mut Slot, free: &mut Vec<u32>, index: usize) {
	match slot.generation.checked_add(1) {
		Some(next) => {
			slot.generation = next;
			// ALLOC-OK: `try_place` reserves one free entry for every slot it creates, so
			// `free.capacity() >= slots.len()` and this push never allocates.
			free.push(index as u32);
		}
		None => slot.generation = u32::MAX,
	}
}

impl HandleTable {
	pub const fn new() -> Self {
		Self { slots: Vec::new(), free: Vec::new(), booked: Vec::new(), closed: false, trace_id: 0, domain: None }
	}

	// Name this table for the conformance trace. See `trace_id`.
	#[cfg(test)]
	pub fn set_trace_id(&mut self, id: u16) {
		self.trace_id = id;
	}

	// One line at each boundary. In a production build `handle_event` reaches an empty `record`, so
	// this is a call the optimiser removes rather than a branch anybody pays for.
	fn trace(&self, action: u8, slot: u16, generation: u32, rights: u32, outcome: u8) {
		super::trace::handle_event(action, self.trace_id, slot, generation, rights, outcome);
	}

	// Bind this table to a Domain so inserts/closes charge its handle quota.
	// Called once, while the table is still empty, when a thread is created.
	pub fn set_domain(&mut self, domain: Arc<Domain>) {
		self.domain = Some(domain);
	}

	// Number of live handles in the table.
	pub fn len(&self) -> usize {
		self.slots.iter().filter(|s| s.cap.is_some()).count()
	}

	// Place a capability into a free or fresh slot and return its handle, or give it back when
	// there is no room in the SLOT VECTOR for it.
	//
	// The quota and the memory are two different resources and this is the one that was not
	// checked. `reserve` was taught to book physical slots as well as quota - its comment says
	// exactly why - but `insert` and `try_insert_or_return` reached the vector through the
	// infallible form of this function, so the ordinary "create an object and install a handle"
	// syscall still had quota-granted-heap-empty as a kernel abort. A short heap is a userspace
	// -reachable state; aborting on it is a denial of service with no privilege required.
	//
	// Every path into a slot goes through here now, and the two that must not fail
	// (`insert_reserved`, and `restore_taken`'s fallback) reuse a slot that already exists rather
	// than growing the vector - which is what makes their infallibility true rather than assumed.
	fn try_place(&mut self, cap: Capability) -> Result<Handle, Capability> {
		if let Some(index) = self.free.pop() {
			let slot = &mut self.slots[index as usize];
			let (generation, carried) = (slot.generation, cap.rights.bits());
			slot.cap = Some(cap);
			self.trace(super::trace::SEED, index as u16, generation, carried, super::trace::OK);
			return Ok(Handle::new(generation, index));
		}
		// BOTH VECTORS, and the free list first in intent: this reserved only `slots`, so a fresh
		// slot appended here left `free` with no room for the index it will one day carry. Every
		// later `free.push` - in `close`, in `commit_taken`, in `close_all` - then allocated, and
		// those pushes have nowhere to report a failure. A ring-3 process that exhausts the heap and
		// then CLOSES a handle could end the kernel, and bulk teardown could do it at the least
		// recoverable point in a process's life.
		//
		// Reserving one free entry per fresh slot makes `free.capacity() >= slots.len()` an
		// invariant of slot creation, which is what the "it shrank by one on the way in" comments on
		// those pushes have always claimed and only ever been true for a RECYCLED slot.
		// AGAINST THE SLOT COUNT, not `try_reserve(1)`: `free` is usually EMPTY while slots are being
		// appended, and `try_reserve(1)` only promises room for one more than its current length - so
		// it is satisfied immediately and the capacity lags the slot count for ever after. What has
		// to hold is `free.capacity() >= slots.len()`, which is the amount a full teardown pushes.
		let wanted = self.slots.len() + 1;
		if self.slots.try_reserve(1).is_err() || self.free.try_reserve(wanted - self.free.len()).is_err() {
			return Err(cap);
		}
		let index = self.slots.len() as u32;
		let carried = cap.rights.bits();
		self.slots.push(Slot { cap: Some(cap), generation: 1, reserved: false });
		self.trace(super::trace::SEED, index as u16, 1, carried, super::trace::OK);
		Ok(Handle::new(1, index))
	}

	// `try_place` for a caller that has already booked the room - see `reserve`, which reserves the
	// slots and the quota together, and `insert`, which is documented as unable to fail. The
	// `try_reserve` inside cannot fail for a booked slot, and if it somehow did there is nowhere to
	// report it, so the capability is dropped where it stands rather than left in a slot that does
	// not exist.
	fn place(&mut self, cap: Capability) -> Handle {
		match self.try_place(cap) {
			Ok(handle) => handle,
			// A handle value that names nothing: the caller's next use of it fails as a bad handle,
			// which is a refusal rather than a corrupt table.
			Err(_) => Handle::new(0, 0),
		}
	}

	// Install a capability and return a fresh handle to it. Counts the handle
	// against the Domain unconditionally (used by paths that must not fail, such
	// as installing a transferred capability or seeding a bootstrap handle); the
	// per-create quota is enforced by `try_insert`.
	pub fn insert(&mut self, cap: Capability) -> Handle {
		if let Some(domain) = &self.domain {
			domain.charge_handle();
		}
		// GROWING THE VECTOR IS FALLIBLE even here, and here there is nowhere to report it: this is
		// the "must not fail" form, used to seed a bootstrap capability into a process being built
		// and to reissue one whose slot vanished. The failure is therefore made into a handle value
		// that names nothing, which the holder discovers as a bad handle on first use - a refusal,
		// where the infallible push was a kernel abort reachable from ring 3 by exhausting the heap.
		//
		// AND THE CHARGE COMES BACK WHEN IT DOES. The quota was taken above and the failure branch
		// dropped the capability and returned handle 0, so a table that could not grow left the
		// Domain paying for a handle that does not exist - a leak that repeats every time the heap
		// is short and is never reclaimed, because there is no handle to close.
		match self.try_place(cap) {
			Ok(handle) => handle,
			Err(_) => {
				if let Some(domain) = &self.domain {
					domain.uncharge_handles(1);
				}
				Handle::new(0, 0)
			}
		}
	}

	// Mint a fresh capability for `object` with `rights` and install it.
	pub fn insert_object(&mut self, object: Arc<dyn KernelObject>, rights: Rights) -> Handle {
		// ALLOC-OK: `HandleTable::insert`, not a collection insert - and it books its slot through
		// `try_place`, refunding the Domain charge when the table cannot grow.
		self.insert(Capability::new(object, rights))
	}

	// Install a capability, enforcing the Domain's handle quota. Returns None
	// (charging nothing) if the table's Domain is at its handle cap.
	// Reserve room for `count` more handles, or refuse. Both kinds of room.
	//
	// This charged the Domain's quota and nothing else, and its comment claimed that a later
	// `insert` therefore could not be refused for space. It could: `insert_reserved` goes through
	// `place`, which ends in `self.slots.push(...)`, an INFALLIBLE `Vec` growth. The quota said the
	// Domain was allowed another handle; nothing had said the kernel heap could hold one.
	//
	// That gap sits under a caller whose whole reason for reserving is that it is about to destroy
	// something it cannot get back - a receive takes the message out of the queue on the strength
	// of this answer. Quota granted, message dequeued, `slots` needs to grow, the heap is empty:
	// an allocation abort in the kernel, reachable from ring 3 by filling memory and receiving.
	//
	// So the physical slots are reserved first and the quota second. `free` holds indices of slots
	// that already exist, so only the shortfall needs allocating; `try_reserve` may over-allocate,
	// which is fine - it never under-allocates, and that is the direction that matters here.
	pub fn reserve(&mut self, count: usize) -> bool {
		if self.booked.try_reserve(count).is_err() {
			return false;
		}
		let before = self.booked.len();
		for _ in 0..count {
			// A CONCRETE SLOT, taken out of circulation here so nothing else can have it.
			let index = match self.free.pop() {
				Some(index) => index,
				None => {
					// Same pairing `try_place` uses: a fresh slot needs room in `slots` AND a place
					// in `free` for the index it will carry when it is eventually closed.
					let wanted = self.slots.len() + 1;
					if self.slots.try_reserve(1).is_err() || self.free.try_reserve(wanted - self.free.len()).is_err() {
						self.unbook(before);
						return false;
					}
					let index = self.slots.len() as u32;
					self.slots.push(Slot { cap: None, generation: 1, reserved: false });
					index
				}
			};
			self.booked.push(index);
		}
		let Some(domain) = &self.domain else {
			self.trace(super::trace::BOOK, count as u16, 0, 0, super::trace::OK);
			return true;
		};
		for taken in 0..count {
			if !domain.try_charge_handle() {
				domain.uncharge_handles(taken as u64);
				self.unbook(before);
				self.trace(super::trace::BOOK, count as u16, 0, 0, super::trace::REFUSED);
				return false;
			}
		}
		self.trace(super::trace::BOOK, count as u16, 0, 0, super::trace::OK);
		true
	}

	// Put every slot booked since `keep` back on the free list, for a reservation that could not be
	// completed. The free list has room by construction: each index came out of it, or came with a
	// place in it.
	fn unbook(&mut self, keep: usize) {
		while self.booked.len() > keep {
			let index = self.booked.pop().expect("length checked");
			// ALLOC-OK: every booked index either came off this list or arrived with a place
			// reserved on it by `reserve`, so returning it cannot grow the vector.
			self.free.push(index);
		}
	}

	// Install a capability against a reservation already taken by `reserve`. Charges
	// nothing: the quota for this handle was paid when the room was booked, and charging
	// again here would bill twice for one handle. Cannot fail, and `reserve` is what makes
	// that true of the memory as well as of the quota.
	pub fn insert_reserved(&mut self, cap: Capability) -> Handle {
		// THE SAME BARRIER `restore_taken` STANDS BEHIND. `close_all` has run, so there is nobody to
		// install for: the capability is dropped here and the quota `reserve` charged for the
		// booking is refunded, which is what `close_all` would have done to this handle had it
		// existed at the time. Returning a handle into a dead process's table would leave a live
		// capability in a process that has finished tearing down.
		if self.closed {
			self.trace(super::trace::INSTALL_INTO_CLOSED, 0, 0, cap.rights.bits(), super::trace::OK);
			drop(cap);
			if self.booked.pop().is_some()
				&& let Some(domain) = &self.domain
			{
				domain.uncharge_handles(1);
			}
			return Handle::from_raw(0);
		}
		// INTO THE SLOT THIS RESERVATION OWNS. `place` searched the free list, which another thread
		// may have emptied since the booking - and answered with the zero handle when it had, after
		// the hardware was programmed or the message committed.
		let carried = cap.rights.bits();
		match self.booked.pop() {
			Some(index) => {
				let slot = &mut self.slots[index as usize];
				slot.cap = Some(cap);
				let generation = slot.generation;
				self.trace(super::trace::INSTALL, index as u16, generation, carried, super::trace::OK);
				Handle::new(generation, index)
			}
			// No booking: the caller reserved less than it installs, which is a contract error
			// rather than a race. `place` is the honest fallback - it charges nothing, and a handle
			// value naming nothing fails at the caller's next use.
			None => self.place(cap),
		}
	}

	// Give back part of a reservation that was not used.
	pub fn release_reservation(&mut self, count: usize) {
		self.trace(super::trace::UNBOOK, count as u16, 0, 0, super::trace::OK);
		// The booked slots go back too, not only the quota: they were taken out of circulation by
		// `reserve` and nothing else can use them until they are returned.
		let give_back = count.min(self.booked.len());
		let keep = self.booked.len() - give_back;
		self.unbook(keep);
		if let Some(domain) = &self.domain {
			domain.uncharge_handles(count as u64);
		}
	}

	pub fn try_insert(&mut self, cap: Capability) -> Option<Handle> {
		self.try_insert_or_return(cap).ok()
	}

	// The same, GIVING THE CAPABILITY BACK when the quota refuses it.
	//
	// `try_insert` takes the capability by value and answers a refusal with `None`, so the
	// capability is dropped where it stood. That is right for a caller that has just minted one and
	// wrong for a caller in the middle of a TRANSFER: `sys_thread_create` moved a bootstrap
	// capability out of the parent and into the child, and when the child's quota said no, neither
	// party had it afterwards and nothing said so.
	pub fn try_insert_or_return(&mut self, cap: Capability) -> Result<Handle, Capability> {
		if let Some(domain) = &self.domain {
			if !domain.try_charge_handle() {
				return Err(cap);
			}
		}
		match self.try_place(cap) {
			Ok(handle) => Ok(handle),
			Err(cap) => {
				// The quota was charged and the memory was not there: give the quota back with the
				// capability, or the refusal costs the caller a permanent unit of it.
				if let Some(domain) = &self.domain {
					domain.uncharge_handles(1);
				}
				Err(cap)
			}
		}
	}

	// Mint and install a fresh capability under the Domain's handle quota.
	pub fn try_insert_object(&mut self, object: Arc<dyn KernelObject>, rights: Rights) -> Option<Handle> {
		self.try_insert(Capability::new(object, rights))
	}

	fn cap_of(&self, handle: Handle) -> Result<&Capability, HandleError> {
		let slot = self.slots.get(handle.index() as usize).ok_or(HandleError::BadHandle)?;
		if slot.generation != handle.generation() {
			return Err(HandleError::BadHandle);
		}
		slot.cap.as_ref().ok_or(HandleError::BadHandle)
	}

	// Look up the object behind a handle, enforcing revocation and rights.
	// Returns a new reference to the object on success.
	pub fn lookup(&self, handle: Handle, required: Rights) -> Result<Arc<dyn KernelObject>, HandleError> {
		let cap = self.cap_of(handle)?;
		if !cap.is_valid() {
			return Err(HandleError::Revoked);
		}
		if !cap.rights.contains(required) {
			return Err(HandleError::AccessDenied);
		}
		Ok(cap.object.clone())
	}

	// Like `lookup`, but also enforce the object's type ("sealing"): you cannot
	// use a handle to one object type where another is expected.
	pub fn lookup_typed(&self, handle: Handle, expected: ObjectType, required: Rights) -> Result<Arc<dyn KernelObject>, HandleError> {
		let cap = self.cap_of(handle)?;
		if cap.object_type() != expected {
			return Err(HandleError::WrongType);
		}
		if !cap.is_valid() {
			return Err(HandleError::Revoked);
		}
		if !cap.rights.contains(required) {
			return Err(HandleError::AccessDenied);
		}
		Ok(cap.object.clone())
	}

	// The capability behind a handle, checked the way `lookup` checks it: the slot must
	// match its generation AND the object must not have been revoked.
	//
	// `cap_of` alone answers only the first, and the introspection accessors used it
	// directly - so a revoked handle still reported its rights and still
	// appeared alive in the System Graph. One validation, used by everything, is the only
	// way those two answers stay the same answer.
	fn live_cap_of(&self, handle: Handle) -> Result<&Capability, HandleError> {
		let cap = self.cap_of(handle)?;
		if !cap.is_valid() {
			return Err(HandleError::Revoked);
		}
		Ok(cap)
	}

	// Inspect the rights a handle carries (a get_info-style query).
	pub fn rights_of(&self, handle: Handle) -> Result<Rights, HandleError> {
		Ok(self.live_cap_of(handle)?.rights)
	}

	// Introspect a handle: the identity, type, rights, and badge behind it. Like
	// rights_of this is a get_info-style query; it underlies the
	// object_info_get syscall. Returns None for a bad or stale handle.
	pub fn info(&self, handle: Handle) -> Option<HandleInfo> {
		let cap = self.live_cap_of(handle).ok()?;
		Some(HandleInfo::from_cap(cap))
	}

	// A snapshot of every live handle in the table, for enumeration by the System
	// Graph. Order follows the slot indices.
	#[cfg(test)]
	pub fn entries(&self) -> Vec<HandleInfo> {
		let mut out = Vec::new();
		for slot in &self.slots {
			if let Some(cap) = &slot.cap {
				// a revoked capability is not an entry of this table any more, however
				// intact its slot looks - the System Graph showed them as live.
				if !cap.is_valid() {
					continue;
				}
				// ALLOC-OK: the inspection buffer is reserved by its caller from the table's own length before this runs.
				out.push(HandleInfo::from_cap(cap));
			}
		}
		out
	}

	// Derive a weaker handle to the same object. Requires the DUPLICATE right,
	// and `new_rights` must be a subset of the original's (attenuation only).
	pub fn duplicate(&mut self, handle: Handle, new_rights: Rights) -> Result<Handle, HandleError> {
		let object = {
			let cap = self.cap_of(handle)?;
			if !cap.is_valid() {
				return Err(HandleError::Revoked);
			}
			if !cap.rights.contains(Rights::DUPLICATE) {
				return Err(HandleError::AccessDenied);
			}
			if !cap.rights.contains(new_rights) {
				return Err(HandleError::AccessDenied);
			}
			cap.object.clone()
		};
		// Through the QUOTA, like every other user-reachable install. This was the
		// unbounded `insert`, so a process holding one duplicable handle could pass
		// `PROP_HANDLE_LIMIT` indefinitely by asking - and other checks that bound
		// themselves by "how many handles the caller holds" were bounded by nothing.
		self.try_insert(Capability::new(object, new_rights)).ok_or(HandleError::LimitReached)
	}

	// Close a handle: drop its capability (releasing one object reference) and
	// recycle the slot under a new generation so the old handle value is dead.
	// Remove a capability from this table and hand it to the caller, requiring `rights`.
	//
	// This is what a TRANSFER is: the capability leaves here and arrives there, once. It
	// used to be a clone under the lock, a send, and then a re-lookup and a `close` whose
	// result was DISCARDED - so two threads transferring the same handle both cloned it
	// and only one close succeeded, and a single caller could simply name the same handle
	// twice in one batch. Either way one handle became two capabilities without the
	// `DUPLICATE` right, which is the one thing a capability system must not do.
	//
	// The slot's generation is bumped like `close` does, so the raw handle the caller
	// still holds is dead the moment this returns - there is no window in which it names
	// anything.
	pub fn take(&mut self, handle: Handle, rights: Rights) -> Result<Capability, HandleError> {
		let index = handle.index() as usize;
		{
			let cap = self.live_cap_of(handle)?;
			if !cap.rights.contains(rights) {
				return Err(HandleError::AccessDenied);
			}
		}
		let slot = self.slots.get_mut(index).ok_or(HandleError::BadHandle)?;
		let cap = slot.cap.take().ok_or(HandleError::BadHandle)?;
		retire_or_recycle(slot, &mut self.free, index);
		if let Some(domain) = &self.domain {
			domain.uncharge_handles(1);
		}
		Ok(cap)
	}

	// Put a taken capability back, for a transfer that could not be completed. The handle
	// it returns is a NEW one - the old raw value died with its slot generation - which is
	// why the rollback path has to hand the caller its new handles rather than pretending
	// nothing happened.
	#[cfg(test)]
	pub fn put_back(&mut self, cap: Capability) -> Handle {
		// ALLOC-OK: `HandleTable::insert`, not a collection insert - the table's own growth is reserved by its callers and checked where it happens.
		self.insert(cap)
	}

	// Take a capability for a transfer that MIGHT fail, without killing the handle value yet.
	//
	// `take` retires the slot immediately, so a send that then fails leaves the capability with
	// nowhere to go: `put_back` reissues it under a handle the caller has no way to learn, and the
	// caller - following the only discipline available to it, closing what it could not send -
	// closes a value that is already dead. The capability survives, unreachable and still charged.
	// One leak per failed transfer, in userspace code doing exactly the right thing.
	//
	// So the slot is EMPTIED and RESERVED: its generation is untouched and its index does not go
	// back to the free list, so nothing else can occupy it while the message is in flight. The
	// handle names nothing until the outcome is known, which is the truth about it. Exactly one of
	// [`commit_taken`] or [`restore_taken`] must follow.
	pub fn take_for_transfer(&mut self, handle: Handle, rights: Rights) -> Result<Capability, HandleError> {
		let index = handle.index() as usize;
		{
			let cap = self.live_cap_of(handle)?;
			if !cap.rights.contains(rights) {
				return Err(HandleError::AccessDenied);
			}
		}
		let slot = self.slots.get_mut(index).ok_or(HandleError::BadHandle)?;
		let cap = slot.cap.take().ok_or(HandleError::BadHandle)?;
		slot.reserved = true;
		let generation = slot.generation;
		let carried = cap.rights.bits();
		self.trace(super::trace::TAKE, index as u16, generation, carried, super::trace::OK);
		Ok(cap)
	}

	// The transfer happened: the handle value dies now, and the quota is refunded.
	pub fn commit_taken(&mut self, handle: Handle) {
		self.trace(super::trace::COMMIT_TAKE, handle.index() as u16, handle.generation(), 0, super::trace::OK);
		self.resolve_taken(handle);
	}

	// The half `commit_taken` and `abandon_taken` share: the slot goes back under the generation
	// rules and the quota is refunded. Split out so each of the two emits ITS OWN action and only
	// its own - a trace in which an abandon is followed by a commit describes a transfer resolved
	// twice, which is not what happened.
	fn resolve_taken(&mut self, handle: Handle) {
		let index = handle.index() as usize;
		if let Some(slot) = self.slots.get_mut(index) {
			slot.reserved = false;
			retire_or_recycle(slot, &mut self.free, index);
		}
		if let Some(domain) = &self.domain {
			domain.uncharge_handles(1);
		}
	}

	// The transfer can no longer be resolved either way: the capability is GONE, and the slot that
	// was holding its place must not hold it forever.
	//
	// The third outcome, and it exists because a call site reached it. `sys_thread_create` inserted
	// the taken capability into the child ORDINARILY, so a `terminate()` racing the spawn reached
	// `close_all`, which skipped the caller's reserved slot and destroyed the child's copy; the
	// rollback's `take` out of the child then returned `Err` and `restore_taken` was never called.
	// What was left was a slot with `reserved: true` and `cap: None` - not on the free list, never
	// committed, skipped by `close_all` for the life of the process, and one unit of handle quota
	// short. Nothing in the table cleared it, because until now nothing could.
	//
	// The costs are identical to `commit_taken`'s, which is what makes this the right shape rather
	// than a special case: the handle value dies, the slot goes back under the generation rules,
	// and the quota is refunded. What differs is only where the capability went - to the peer on a
	// commit, nowhere at all here.
	pub fn abandon_taken(&mut self, handle: Handle) {
		self.trace(super::trace::ABANDON_TAKE, handle.index() as u16, handle.generation(), 0, super::trace::OK);
		self.resolve_taken(handle);
	}

	// The transfer did not happen: the capability goes back to the handle it came from, still
	// live, still the same value. A rejected send costs the caller nothing at all - not the
	// capability, and not the handle it was named by.
	//
	// UNLESS THE TABLE IS CLOSED, in which case there is nobody to give it back to: the capability
	// is dropped here and the quota refunded, which is what `close_all` would have done to it had it
	// been present at the time.
	pub fn restore_taken(&mut self, handle: Handle, cap: Capability) {
		self.trace(super::trace::RESTORE_TAKE, handle.index() as u16, handle.generation(), cap.rights.bits(), if self.closed { super::trace::REFUSED } else { super::trace::OK });
		if self.closed {
			drop(cap);
			if let Some(index) = self.slots.get_mut(handle.index() as usize) {
				index.reserved = false;
			}
			if let Some(domain) = &self.domain {
				domain.uncharge_handles(1);
			}
			return;
		}
		let index = handle.index() as usize;
		if let Some(slot) = self.slots.get_mut(index) {
			slot.cap = Some(cap);
			slot.reserved = false;
			return;
		}
		// The slot vanished under us, which nothing in this kernel does while a table is locked.
		// Reissuing is still better than dropping the capability on the floor.
		// ALLOC-OK: `HandleTable::insert`, not a collection insert - the table's own growth is reserved by its callers and checked where it happens.
		self.insert(cap);
	}

	pub fn close(&mut self, handle: Handle) -> Result<(), HandleError> {
		let index = handle.index() as usize;
		self.trace(super::trace::CLOSE, index as u16, handle.generation(), 0, super::trace::OK);
		let slot = self.slots.get_mut(index).ok_or(HandleError::BadHandle)?;
		if slot.cap.is_none() || slot.generation != handle.generation() {
			return Err(HandleError::BadHandle);
		}
		slot.cap = None;
		retire_or_recycle(slot, &mut self.free, index);
		if let Some(domain) = &self.domain {
			domain.uncharge_handles(1);
		}
		Ok(())
	}

	// Close every live handle at once, refunding each to the Domain and dropping
	// the objects they held. Used by bulk process termination so a killed process's
	// handles (and the memory those objects pinned) are released eagerly, without
	// the cooperation of its threads. After this the table is empty, so the Drop
	// refund finds nothing left to return - the two paths never double-count.
	// The free list, for a test that has to look at it. The invariant it checks - no index twice -
	// is not observable through the ordinary API until the damage has already been handed out.
	#[cfg(test)]
	// The free list's capacity, so a test can assert the invariant `try_place` establishes rather
	// than only its visible effect.
	#[cfg(test)]
	pub fn free_capacity_for_test(&self) -> usize {
		self.free.capacity()
	}

	#[cfg(test)]
	pub fn slot_count_for_test(&self) -> usize {
		self.slots.len()
	}

	// The slots a reservation is holding, for the test that asks what a termination does to them.
	#[cfg(test)]
	pub fn booked_indices_for_test(&self) -> alloc::vec::Vec<u32> {
		// ALLOC-OK: `#[cfg(test)]`, as below.
		self.booked.clone()
	}

	#[cfg(test)]
	pub fn free_indices_for_test(&self) -> alloc::vec::Vec<u32> {
		// ALLOC-OK: `#[cfg(test)]`, and a test that cannot allocate has already failed. The gate
		// exempts test code by PATH, which does not reach a test helper living in a source file.
		self.free.clone()
	}

	// Every live capability in this table, for a caller that has to touch the objects themselves
	// before they are closed. Used by process teardown, which has to tell a dying process's DMA
	// buffers that their owner never said the device was done with them - a fact that exists only
	// on this side of `close_all`.
	pub fn for_each_object(&self, mut f: impl FnMut(Arc<dyn KernelObject>)) {
		for slot in &self.slots {
			if let Some(cap) = &slot.cap {
				f(cap.object());
			}
		}
	}

	pub fn close_all(&mut self) {
		self.trace(super::trace::TERMINATE, 0, 0, 0, super::trace::OK);
		// From here the table takes nothing new: a transfer that has not resolved yet has no owner
		// left to restore to. See `closed`.
		self.closed = true;
		let mut closed: u64 = 0;
		self.free.clear();
		for index in 0..self.slots.len() {
			// Asked BEFORE the slot is borrowed mutably, which is the only reason it is a line of
			// its own: `booked` and `slots` are two fields of the same table.
			let booked = self.booked.contains(&(index as u32));
			let slot = &mut self.slots[index];
			// A slot with a transfer in flight is not this function's to reclaim. Its capability is
			// somewhere else and exactly one of `commit_taken`/`restore_taken` is still to come; put
			// it on the free list and that call writes into a slot something else may already own.
			//
			// A BOOKED SLOT IS NOT THIS FUNCTION'S TO RECLAIM EITHER, and it used to be. `reserve`
			// takes a CONCRETE slot out of circulation and charges the quota for it, and an
			// `insert_reserved` may still be on its way to that index - a receive between its peek
			// and its install, an MSI acquire between its booking and its handle. Rebuilding the
			// free list over it put the index in two places at once, on `free` and on `booked`, so
			// the install and the next `insert` could be handed the same slot; and the charge was
			// refunded by nobody, because this function refunds the slots that HELD a capability
			// and `Drop` refunds the LIVE ones, and a booking is neither.
			//
			// Found by the model in `docs/spec/capability` rather than by a machine: three states -
			// Init, Book, Terminate - violating `QuotaConserved`.
			if slot.reserved || booked {
				continue;
			}
			let had = slot.cap.take().is_some();
			if had {
				closed += 1;
			}
			// `wrapping_add` was the other half of it: `close` goes through `retire_or_recycle`
			// precisely so a slot whose generation cannot advance is retired rather than reused, and
			// wrapping here walked straight past that - and then pushed the retired slot too. The
			// rule is one rule, so it is spelled the same way in both places.
			let retired = if had {
				match slot.generation.checked_add(1) {
					Some(next) => {
						slot.generation = next;
						false
					}
					None => {
						slot.generation = u32::MAX;
						true
					}
				}
			} else {
				slot.generation == u32::MAX
			};
			if !retired {
				// ALLOC-OK: `free` was cleared above, which keeps its capacity, and `try_place`
				// reserved one entry per slot - so this rebuild cannot exceed what is already there.
				self.free.push(index as u32);
			}
		}
		if closed > 0 {
			if let Some(domain) = &self.domain {
				domain.uncharge_handles(closed);
			}
		}
	}
}

impl Default for HandleTable {
	fn default() -> Self {
		Self::new()
	}
}

impl Drop for HandleTable {
	fn drop(&mut self) {
		// Refund every still-open handle to the Domain so a thread that exits (or
		// crashes) with handles held returns its quota without cooperation.
		if let Some(domain) = &self.domain {
			let live = self.len() as u64;
			if live > 0 {
				domain.uncharge_handles(live);
			}
		}
	}
}

#[cfg(test)]
mod tests;
