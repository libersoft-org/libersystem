// Per-process handle table and the capability records it holds.
//
// A Handle is an opaque, per-process token (like a file descriptor) that indexes
// a slot holding a Capability. Userspace never sees a Capability directly - only
// the kernel does. Each slot carries a generation, so a stale handle (to a closed
// and possibly reused slot) is reliably rejected.
//
// A HandleTable is owned by a process. It is not internally locked; the owner
// wraps it in a SpinLock when it is shared between a process's threads.

#![allow(dead_code)]

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

// A capability = a reference to a kernel object + a set of rights + a badge.
// Held only inside the kernel (in a handle table or a message in transit).
pub struct Capability {
	object: Arc<dyn KernelObject>,
	rights: Rights,
	badge: u64,
	generation: u32,
}

impl Capability {
	pub fn new(object: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> Self {
		let generation = object.header().generation();
		Self { object, rights, badge, generation }
	}

	pub fn rights(&self) -> Rights {
		self.rights
	}

	pub fn badge(&self) -> u64 {
		self.badge
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
	pub badge: u64,
	pub generation: u32,
}

impl HandleInfo {
	// Snapshot a capability's introspection fields, so the info / entries queries
	// map them in exactly one place.
	fn from_cap(cap: &Capability) -> HandleInfo {
		HandleInfo { koid: cap.object.header().koid(), object_type: cap.object_type(), rights: cap.rights, badge: cap.badge, generation: cap.object.header().generation() }
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
			free.push(index as u32);
		}
		None => slot.generation = u32::MAX,
	}
}

impl HandleTable {
	pub const fn new() -> Self {
		Self { slots: Vec::new(), free: Vec::new(), domain: None }
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

	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	// Place a capability into a free or fresh slot and return its handle. Does not
	// touch accounting; the public insert paths charge before calling this.
	fn place(&mut self, cap: Capability) -> Handle {
		if let Some(index) = self.free.pop() {
			let slot = &mut self.slots[index as usize];
			slot.cap = Some(cap);
			Handle::new(slot.generation, index)
		} else {
			let index = self.slots.len() as u32;
			self.slots.push(Slot { cap: Some(cap), generation: 1, reserved: false });
			Handle::new(1, index)
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
		self.place(cap)
	}

	// Mint a fresh capability for `object` with `rights`/`badge` and install it.
	pub fn insert_object(&mut self, object: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> Handle {
		self.insert(Capability::new(object, rights, badge))
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
		let needed = count.saturating_sub(self.free.len());
		if needed > 0 && self.slots.try_reserve(needed).is_err() {
			return false;
		}
		let Some(domain) = &self.domain else {
			return true;
		};
		for taken in 0..count {
			if !domain.try_charge_handle() {
				domain.uncharge_handles(taken as u64);
				return false;
			}
		}
		true
	}

	// Install a capability against a reservation already taken by `reserve`. Charges
	// nothing: the quota for this handle was paid when the room was booked, and charging
	// again here would bill twice for one handle. Cannot fail, and `reserve` is what makes
	// that true of the memory as well as of the quota.
	pub fn insert_reserved(&mut self, cap: Capability) -> Handle {
		self.place(cap)
	}

	// Give back part of a reservation that was not used.
	pub fn release_reservation(&mut self, count: usize) {
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
		Ok(self.place(cap))
	}

	// Mint and install a fresh capability under the Domain's handle quota.
	pub fn try_insert_object(&mut self, object: Arc<dyn KernelObject>, rights: Rights, badge: u64) -> Option<Handle> {
		self.try_insert(Capability::new(object, rights, badge))
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
	// directly - so a revoked handle still reported its rights and its badge, and still
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

	// Inspect the badge a handle carries (stamped onto messages it sends).
	pub fn badge_of(&self, handle: Handle) -> Result<u64, HandleError> {
		Ok(self.live_cap_of(handle)?.badge)
	}

	// Introspect a handle: the identity, type, rights, and badge behind it. Like
	// rights_of/badge_of this is a get_info-style query; it underlies the
	// object_info_get syscall. Returns None for a bad or stale handle.
	pub fn info(&self, handle: Handle) -> Option<HandleInfo> {
		let cap = self.live_cap_of(handle).ok()?;
		Some(HandleInfo::from_cap(cap))
	}

	// A snapshot of every live handle in the table, for enumeration by the System
	// Graph. Order follows the slot indices.
	pub fn entries(&self) -> Vec<HandleInfo> {
		let mut out = Vec::new();
		for slot in &self.slots {
			if let Some(cap) = &slot.cap {
				// a revoked capability is not an entry of this table any more, however
				// intact its slot looks - the System Graph showed them as live.
				if !cap.is_valid() {
					continue;
				}
				out.push(HandleInfo::from_cap(cap));
			}
		}
		out
	}

	// Derive a weaker handle to the same object. Requires the DUPLICATE right,
	// and `new_rights` must be a subset of the original's (attenuation only).
	pub fn duplicate(&mut self, handle: Handle, new_rights: Rights) -> Result<Handle, HandleError> {
		let (object, badge) = {
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
			(cap.object.clone(), cap.badge)
		};
		// Through the QUOTA, like every other user-reachable install. This was the
		// unbounded `insert`, so a process holding one duplicable handle could pass
		// `PROP_HANDLE_LIMIT` indefinitely by asking - and other checks that bound
		// themselves by "how many handles the caller holds" were bounded by nothing.
		self.try_insert(Capability::new(object, new_rights, badge)).ok_or(HandleError::LimitReached)
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
	pub fn put_back(&mut self, cap: Capability) -> Handle {
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
		Ok(cap)
	}

	// The transfer happened: the handle value dies now, and the quota is refunded.
	pub fn commit_taken(&mut self, handle: Handle) {
		let index = handle.index() as usize;
		if let Some(slot) = self.slots.get_mut(index) {
			slot.reserved = false;
			retire_or_recycle(slot, &mut self.free, index);
		}
		if let Some(domain) = &self.domain {
			domain.uncharge_handles(1);
		}
	}

	// The transfer did not happen: the capability goes back to the handle it came from, still
	// live, still the same value. A rejected send costs the caller nothing at all - not the
	// capability, and not the handle it was named by.
	pub fn restore_taken(&mut self, handle: Handle, cap: Capability) {
		let index = handle.index() as usize;
		if let Some(slot) = self.slots.get_mut(index) {
			slot.cap = Some(cap);
			slot.reserved = false;
			return;
		}
		// The slot vanished under us, which nothing in this kernel does while a table is locked.
		// Reissuing is still better than dropping the capability on the floor.
		self.insert(cap);
	}

	pub fn close(&mut self, handle: Handle) -> Result<(), HandleError> {
		let index = handle.index() as usize;
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
	pub fn free_indices_for_test(&self) -> alloc::vec::Vec<u32> {
		self.free.clone()
	}

	pub fn close_all(&mut self) {
		let mut closed: u64 = 0;
		self.free.clear();
		for index in 0..self.slots.len() {
			let slot = &mut self.slots[index];
			// A slot with a transfer in flight is not this function's to reclaim. Its capability is
			// somewhere else and exactly one of `commit_taken`/`restore_taken` is still to come; put
			// it on the free list and that call writes into a slot something else may already own.
			if slot.reserved {
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
