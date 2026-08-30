// Claim kernel object.
//
// THE THING A DEVICE MANAGER HOLDS SO THAT A DEVICE HAS AN OWNER IT CANNOT LOSE TRACK OF.
//
// `SYS_DEVICE_ACQUIRE` answered with one `DeviceMemory` handle and nothing else, and that handle is
// precisely what gets sent on to the driver - so after a successful bind the manager held nothing
// about the claim at all. It could not learn which binding of the device this was, could not read
// what state the device was in, and could not take the device back from a driver that had stopped
// cooperating. The only release in the tree happened when the capability was DROPPED, which is the
// cooperative path this whole milestone exists to stop depending on.
//
// So the acquisition answers with two things: a `ClaimKey` copied into the caller's memory, and this
// object, installed in its table and STAYING there.
//
// STAYING IS A PROPERTY IT HAS TO BE GIVEN, not one it has by being called that. It is minted
// without RIGHT_TRANSFER and without RIGHT_DUPLICATE, so it cannot be moved out of the Domain that
// took it and cannot be copied into another. A claim handle that could be moved would leave the
// manager's Domain, survive its killing, and hold the forced release off exactly when the machine
// most needs it - which is the same argument the device capability's attenuating send makes, one
// level up.
//
// AND IT IS WAITABLE, because a manager built on one `wait_any` loop cannot spin on a status. The
// terminal result of a release arrives here, on a handle sitting in the manager's wait set beside
// the driver's process handle.

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::device::ClaimState;
use crate::sched;

// A settled claim's terminal state is one of the `abi::CLAIM_STATE_*` codes; this is the value that
// means "not settled yet", and it is not one of them.
const LIVE: u32 = u32::MAX;

pub struct Claim {
	header: ObjectHeader,
	// Which binding of which device. Fixed at mint time and never edited: a claim object IS one
	// binding, and the next binding of the same device is a different object.
	key: abi::ClaimKey,
	// The terminal state a release reached, or `LIVE`. This is the wait readiness.
	settled: AtomicU32,
	// Whether this object has already started the release. The forced release runs at most once
	// however it is reached - the syscall, the owner's termination, or the last close.
	releasing: AtomicBool,
}

impl Claim {
	// FALLIBLY: `SYS_DEVICE_CLAIM` reaches this, so a short heap is a refusal and not a halt.
	pub fn create(key: abi::ClaimKey) -> Option<Arc<Self>> {
		crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), key, settled: AtomicU32::new(LIVE), releasing: AtomicBool::new(false) })
	}

	pub fn key(&self) -> abi::ClaimKey {
		self.key
	}

	// Whether the release has finished and the state will not change again. The wait readiness: a
	// manager parked in `wait_any` learns the device is back without polling for it.
	pub fn is_settled(&self) -> bool {
		self.settled.load(Ordering::Acquire) != LIVE
	}

	// The terminal state, or None while the claim is live.
	pub fn outcome(&self) -> Option<u32> {
		match self.settled.load(Ordering::Acquire) {
			LIVE => None,
			code => Some(code),
		}
	}

	// RELEASE THE DEVICE, ONCE, whoever asked and however they got here.
	//
	// The three routes are the syscall, the owner's termination, and the last close of this handle,
	// and all three are the same teardown: bus mastering off, everything derived revoked, vectors
	// masked, the IOMMU teardown confirmed. Nothing here asks the holder for anything.
	//
	// Returns the terminal state. A second call returns the first call's answer rather than tearing
	// anything down again - the `compare_exchange` is what makes that true under two CPUs.
	pub fn release(&self) -> u32 {
		if self.releasing.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
			// Somebody else owns this teardown. Answer with its result if it has one; a caller that
			// asked while it was still running gets the state it is in, which is `Releasing`.
			return self.outcome().unwrap_or(abi::CLAIM_STATE_RELEASING);
		}
		let state = match crate::device::release_claim(self.key) {
			Ok(state) => state,
			// The key is stale, quarantined or names nothing: the device is not this claim's any
			// more, and there is nothing to tear down. `release_claim` refused BEFORE touching
			// anything, so nothing was half-done.
			Err(_) => ClaimState::Free,
		};
		self.settle(state.code());
		state.code()
	}

	// Publish the terminal state and wake whoever is waiting on this handle.
	fn settle(&self, code: u32) {
		self.settled.store(code, Ordering::Release);
		sched::wake_object(self.header.koid());
	}
}

impl_kernel_object!(Claim, Claim);

impl Drop for Claim {
	fn drop(&mut self) {
		// THE LAST CLOSE OF A CLAIM HANDLE IS A FORCED RELEASE. Not a leak, not a silent orphan: a
		// DeviceManager that DIED is exactly the case a cold reconstruction has to survive, and it
		// survives it by finding devices that are `Free`, `Releasing` or `Quarantined` rather than
		// claimed by a process that no longer exists.
		//
		// THIS IS THE LAST CLOSE AND NOT MERELY THE LAST `Arc`, and the two coincide here BY
		// CONSTRUCTION rather than by luck: a claim handle carries neither RIGHT_TRANSFER nor
		// RIGHT_DUPLICATE, so it can never be in a message, in a second table, or in two slots of
		// one. Exactly one handle-table entry holds it, and `Process::terminate` closes that table
		// synchronously - so the release starts when the entry goes.
		//
		// The termination path does not rely on this alone: it releases the claims it finds BEFORE
		// closing the table, so the moment is pinned to the kill rather than to the last transient
		// reference some other CPU's syscall happens to be holding. This is the ordinary close - a
		// manager that closed its own claim handle - and the backstop for anything else.
		self.release();
	}
}

#[cfg(test)]
mod tests;
