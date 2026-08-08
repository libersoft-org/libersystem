// A wait set the kernel KEEPS: objects registered once, waited on many times.
//
// `SYS_WAIT_ANY` takes a fresh array of handles on every call, so the kernel registers a waiter on
// every object in it, blocks, and unregisters all of them again - once per pass, for as long as the
// service runs. The cost of one pass is linear in how many things the caller is listening to, and
// nothing the caller can do makes it otherwise.
//
// That was measured rather than suspected. While bounding StorageService's replies (M0139) a test
// was written to connect until the service refused, and finding a client ceiling the test could
// actually REACH took several attempts: forty-eight connections cost nothing measurable, and a
// ceiling of 256 could not be reached inside fifteen minutes of emulated time. `MAX_CLIENTS` is 64
// because that is where the service is still brisk - a number chosen around the defect rather than
// on its merits.
//
// A set registers each member ONCE, when it is added. What a member's wake then finds in the bucket
// is not a thread but this set, and waking the set wakes whatever thread is parked on it. Adding a
// client costs one registration; a pass costs one registration and a readiness scan, whatever the
// membership.
//
// ## What a registration outlives
//
// The set holds a reference to each member, so a member does not vanish because the handle that
// introduced it was closed. That is the semantics chosen out of the three the design could have had
// - silent removal, a revocation event, or a stale registration that keeps reporting - and it is the
// one that cannot lose an edge: a channel whose peer closes becomes READY, the waiter is told, and
// what it does about it is a decision it makes with the whole picture rather than one the kernel
// makes for it by dropping the member. Leaving the set is explicit, through `remove`.

use alloc::sync::Arc;
use alloc::vec::Vec;

use core::any::Any;

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::sync::SpinLock;

// The most objects one set will hold.
//
// A set is kernel memory whose size a userspace caller decides, which is the shape of every quota in
// this kernel and gets the same treatment: a fixed ceiling first, then a fallible allocation. 256
// matches `MAX_WAIT_HANDLES` - a set is the persistent form of the same question, and a service that
// needed more from one wait would have needed more from the other.
pub const MAX_WAIT_SET_MEMBERS: usize = 256;

pub struct WaitSet {
	header: ObjectHeader,
	members: SpinLock<Vec<Arc<dyn KernelObject>>>,
}

impl WaitSet {
	pub fn create() -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), members: SpinLock::new(Vec::new()) })
	}

	// Add `object` to the set, registering the set as a persistent observer of it.
	//
	// Refuses a duplicate rather than counting it twice: a member registered twice would wake the
	// set twice for one event and would need removing twice, and neither is anything a caller
	// wants. Refuses a set as a member too, because a wake would then have to chase a chain and
	// there is no use for one that a flat set does not serve.
	pub fn add(&self, object: Arc<dyn KernelObject>) -> Result<(), WaitSetError> {
		if object.object_type() == ObjectType::WaitSet {
			return Err(WaitSetError::NotWaitable);
		}
		let koid = object.header().koid();
		let mut members = self.members.lock();
		if members.iter().any(|m| m.header().koid() == koid) {
			return Err(WaitSetError::AlreadyPresent);
		}
		if members.len() >= MAX_WAIT_SET_MEMBERS {
			return Err(WaitSetError::Full);
		}
		if members.try_reserve(1).is_err() {
			return Err(WaitSetError::Full);
		}
		members.push(object);
		// Registered while the membership lock is held, so a wake arriving now finds either both or
		// neither - never a member the set does not know about.
		crate::sched::register_set_observer(koid, self.header.koid());
		Ok(())
	}

	// Take `koid` out of the set, and its registration with it.
	pub fn remove(&self, koid: u64) -> Result<(), WaitSetError> {
		let mut members = self.members.lock();
		let before = members.len();
		members.retain(|m| m.header().koid() != koid);
		if members.len() == before {
			return Err(WaitSetError::NotPresent);
		}
		crate::sched::unregister_set_observer(koid, self.header.koid());
		Ok(())
	}

	// The members, as a snapshot. The readiness scan works from this rather than under the lock:
	// asking an object whether it is ready can take that object's own locks, and holding the
	// membership lock across that is how lock orders get invented by accident.
	pub fn snapshot(&self) -> Vec<Arc<dyn KernelObject>> {
		self.members.lock().clone()
	}

	pub fn len(&self) -> usize {
		self.members.lock().len()
	}

	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}
}

impl Drop for WaitSet {
	fn drop(&mut self) {
		// Every registration this set made comes out with it. A bucket entry naming a set that no
		// longer exists would be chased on every wake of that member, forever.
		let koid = self.header.koid();
		for member in self.members.lock().iter() {
			crate::sched::unregister_set_observer(member.header().koid(), koid);
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaitSetError {
	// The object cannot be waited on, or is a set.
	NotWaitable,
	// Already a member: registering it twice would wake the set twice for one event.
	AlreadyPresent,
	// Not a member, so there is nothing to remove.
	NotPresent,
	// At `MAX_WAIT_SET_MEMBERS`, or the machine would not give the room.
	Full,
}

impl_kernel_object!(WaitSet, WaitSet);
