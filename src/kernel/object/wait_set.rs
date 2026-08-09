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
	// Who pays for the memberships. A registration is one `Arc` reference here plus one scheduler
	// observer entry, and the Domain is charged one handle for the whole SET - so without this the
	// quota that was supposed to bound the registrations bounded nothing resembling their cost.
	// `None` for a set created inside the kernel, which is accounted nowhere else either.
	domain: Option<Arc<super::domain::Domain>>,
}

impl WaitSet {
	pub fn create() -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), members: SpinLock::new(Vec::new()), domain: None })
	}

	// The same, charging its memberships to `domain`. What `sys_waitset_create` builds.
	pub fn create_in(domain: Arc<super::domain::Domain>) -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), members: SpinLock::new(Vec::new()), domain: Some(domain) })
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
		// The Domain's ceiling on registrations, charged before anything is recorded so a refusal
		// leaves nothing to undo. `MAX_WAIT_SET_MEMBERS` bounds ONE set and the handle quota bounds
		// how many sets exist; neither bounds the registrations, which is what the memory is.
		if let Some(domain) = &self.domain {
			if !domain.try_charge_wait_registration() {
				return Err(WaitSetError::Full);
			}
		}
		members.push(object);
		// Registered while the membership lock is held, so a wake arriving now finds either both or
		// neither - never a member the set does not know about.
		//
		// And rolled back when it cannot be. This ignored the result, so a refused registration
		// left a member in the set whose wakes reach nobody, and answered `Ok`: a thread waiting on
		// the set with no deadline then parks with nothing left to rouse it. Success has to mean
		// the member will be woken, or it means nothing at all.
		if let Err(reason) = crate::sched::register_set_observer(koid, self.header.koid()) {
			members.pop();
			if let Some(domain) = &self.domain {
				domain.uncharge_wait_registrations(1);
			}
			return Err(match reason {
				crate::sched::ObserveError::TooManySets => WaitSetError::TooManySets,
				crate::sched::ObserveError::NoMemory => WaitSetError::Full,
			});
		}
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
		if let Some(domain) = &self.domain {
			domain.uncharge_wait_registrations((before - members.len()) as u64);
		}
		Ok(())
	}

	// Run `f` over the membership, under the lock, without copying it.
	//
	// This was a `snapshot` returning a fresh `Vec` - one heap allocation and one atomic increment
	// per member, twice per pass, on the path whose entire purpose is to be cheaper than the
	// alternative. It was: `wait_any` answered a round trip in 189 us at sixty-two clients and the
	// set took 434 us. The measurement is in M0147 and it is the reason this is written the way it
	// is.
	//
	// Under the lock is safe here and worth stating, because it is the kind of thing that stops
	// being safe quietly. Asking an object whether it is ready takes that object's own lock, so the
	// order is set-membership then object. Nothing goes the other way: a wake takes the OBSERVER
	// bucket, never this, and `add` takes this and then the observer bucket. No cycle, and any new
	// caller has to keep it that way.
	pub fn with_members<R>(&self, f: impl FnOnce(&[Arc<dyn KernelObject>]) -> R) -> R {
		f(&self.members.lock())
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
		let members = self.members.lock();
		for member in members.iter() {
			crate::sched::unregister_set_observer(member.header().koid(), koid);
		}
		if let Some(domain) = &self.domain {
			domain.uncharge_wait_registrations(members.len() as u64);
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
	// At `MAX_WAIT_SET_MEMBERS`, at the Domain's registration ceiling, or the machine would not
	// give the room.
	Full,
	// This object is already in as many sets as one object's wake can reach
	// (`sched::MAX_SETS_PER_OBJECT`). Kept apart from `Full`, which is about THIS set: the caller's
	// next move differs - one is "use a smaller set", the other is "something else is watching this
	// object and one of us should stop".
	TooManySets,
}

impl_kernel_object!(WaitSet, WaitSet);
