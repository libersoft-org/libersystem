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

struct Slot {
	cap: Option<Capability>,
	generation: u32,
}

pub struct HandleTable {
	slots: Vec<Slot>,
	free: Vec<u32>,
	// The Domain whose handle quota this table charges. None for tables not tied
	// to a Domain (e.g. unit-test tables), which skip accounting entirely.
	domain: Option<Arc<Domain>>,
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
			self.slots.push(Slot { cap: Some(cap), generation: 1 });
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
	pub fn try_insert(&mut self, cap: Capability) -> Option<Handle> {
		if let Some(domain) = &self.domain {
			if !domain.try_charge_handle() {
				return None;
			}
		}
		Some(self.place(cap))
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

	// Inspect the rights a handle carries (a get_info-style query).
	pub fn rights_of(&self, handle: Handle) -> Result<Rights, HandleError> {
		Ok(self.cap_of(handle)?.rights)
	}

	// Inspect the badge a handle carries (stamped onto messages it sends).
	pub fn badge_of(&self, handle: Handle) -> Result<u64, HandleError> {
		Ok(self.cap_of(handle)?.badge)
	}

	// Introspect a handle: the identity, type, rights, and badge behind it. Like
	// rights_of/badge_of this is a get_info-style query; it underlies the
	// object_info_get syscall. Returns None for a bad or stale handle.
	pub fn info(&self, handle: Handle) -> Option<HandleInfo> {
		let cap = self.cap_of(handle).ok()?;
		Some(HandleInfo { koid: cap.object.header().koid(), object_type: cap.object_type(), rights: cap.rights, badge: cap.badge, generation: cap.object.header().generation() })
	}

	// A snapshot of every live handle in the table, for enumeration by the System
	// Graph. Order follows the slot indices.
	pub fn entries(&self) -> Vec<HandleInfo> {
		let mut out = Vec::new();
		for slot in &self.slots {
			if let Some(cap) = &slot.cap {
				out.push(HandleInfo { koid: cap.object.header().koid(), object_type: cap.object_type(), rights: cap.rights, badge: cap.badge, generation: cap.object.header().generation() });
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
		Ok(self.insert(Capability::new(object, new_rights, badge)))
	}

	// Close a handle: drop its capability (releasing one object reference) and
	// recycle the slot under a new generation so the old handle value is dead.
	pub fn close(&mut self, handle: Handle) -> Result<(), HandleError> {
		let index = handle.index() as usize;
		let slot = self.slots.get_mut(index).ok_or(HandleError::BadHandle)?;
		if slot.cap.is_none() || slot.generation != handle.generation() {
			return Err(HandleError::BadHandle);
		}
		slot.cap = None;
		slot.generation = slot.generation.wrapping_add(1);
		self.free.push(index as u32);
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
	pub fn close_all(&mut self) {
		let mut closed: u64 = 0;
		self.free.clear();
		for index in 0..self.slots.len() {
			let slot = &mut self.slots[index];
			if slot.cap.is_some() {
				slot.cap = None;
				slot.generation = slot.generation.wrapping_add(1);
				closed += 1;
			}
			self.free.push(index as u32);
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
