// Domain kernel object and resource accounting.
//
// A Domain is a hierarchical container that groups threads (and, later, whole
// process subtrees) under a shared resource budget. Each Domain owns a
// ResourceAccount that counts and caps the kernel resources the MVP enforces:
// physical memory held, live handles, and live threads.
//
// Enforcement is at the boundary: the operation that creates a resource
// (*_create, memory_map) atomically charges the account, and either succeeds with
// the resource counted or fails with a typed error - never half-allocated. The
// charge is a compare-and-swap loop so the check-and-add cannot race across
// cores. Cleanup is automatic: dropping an object refunds its charge, so a
// crashed thread's resources are returned without its cooperation.

#![allow(dead_code)]

use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use super::{KernelObject, ObjectHeader, ObjectType};

// Sentinel limit meaning "no cap" for a resource counter.
pub const UNLIMITED: u64 = u64::MAX;

// A single counted, capped resource. `used` and `limit` are in bytes or counts
// depending on the resource. All operations are atomic so accounting stays
// correct when several cores charge the same Domain concurrently.
pub struct ResourceCounter {
	used: AtomicU64,
	limit: AtomicU64,
}

impl ResourceCounter {
	const fn new(limit: u64) -> Self {
		Self { used: AtomicU64::new(0), limit: AtomicU64::new(limit) }
	}

	pub fn used(&self) -> u64 {
		self.used.load(Ordering::Acquire)
	}

	pub fn limit(&self) -> u64 {
		self.limit.load(Ordering::Acquire)
	}

	pub fn set_limit(&self, limit: u64) {
		self.limit.store(limit, Ordering::Release);
	}

	// Atomically add `amount` only if it keeps `used` within `limit`. Returns true
	// and applies the charge on success, or false and changes nothing on failure.
	// This is the enforcement primitive: the check and the add are a single CAS.
	fn try_charge(&self, amount: u64) -> bool {
		let limit = self.limit.load(Ordering::Acquire);
		let mut cur = self.used.load(Ordering::Acquire);
		loop {
			let next = cur.saturating_add(amount);
			if limit != UNLIMITED && next > limit {
				return false;
			}
			match self.used.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire) {
				Ok(_) => return true,
				Err(observed) => cur = observed,
			}
		}
	}

	// Atomically add `amount` regardless of the limit. Used by operations that
	// must always succeed (e.g. installing a transferred capability) but still
	// keep the count exact; the limit is enforced at the create boundaries.
	fn charge(&self, amount: u64) {
		self.used.fetch_add(amount, Ordering::AcqRel);
	}

	// Return `amount` to the pool, saturating at zero so an accounting slip can
	// never wrap the counter.
	fn uncharge(&self, amount: u64) {
		let mut cur = self.used.load(Ordering::Acquire);
		loop {
			let next = cur.saturating_sub(amount);
			match self.used.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire) {
				Ok(_) => return,
				Err(observed) => cur = observed,
			}
		}
	}
}

// The resource account for a Domain: the three quantities the MVP counts and
// enforces. Memory is in bytes (physical RAM held by MemoryObjects); handles and
// threads are counts.
pub struct ResourceAccount {
	memory: ResourceCounter,
	handles: ResourceCounter,
	threads: ResourceCounter,
}

impl ResourceAccount {
	const fn new(memory_limit: u64, handle_limit: u64, thread_limit: u64) -> Self {
		Self { memory: ResourceCounter::new(memory_limit), handles: ResourceCounter::new(handle_limit), threads: ResourceCounter::new(thread_limit) }
	}

	pub fn memory(&self) -> &ResourceCounter {
		&self.memory
	}

	pub fn handles(&self) -> &ResourceCounter {
		&self.handles
	}

	pub fn threads(&self) -> &ResourceCounter {
		&self.threads
	}

	// Charge `bytes` of physical memory, enforcing the cap.
	pub fn try_charge_memory(&self, bytes: u64) -> bool {
		self.memory.try_charge(bytes)
	}

	pub fn uncharge_memory(&self, bytes: u64) {
		self.memory.uncharge(bytes);
	}

	// Charge one handle, enforcing the cap (for handle-creating syscalls).
	pub fn try_charge_handle(&self) -> bool {
		self.handles.try_charge(1)
	}

	// Charge one handle unconditionally (for paths that must not fail but still
	// need to be counted, such as installing a transferred capability).
	pub fn charge_handle(&self) {
		self.handles.charge(1);
	}

	pub fn uncharge_handles(&self, count: u64) {
		self.handles.uncharge(count);
	}

	// Charge one thread, enforcing the cap.
	pub fn try_charge_thread(&self) -> bool {
		self.threads.try_charge(1)
	}

	// Charge one thread unconditionally (for the infallible spawn paths used by
	// kernel threads in the unlimited root Domain).
	pub fn charge_thread(&self) {
		self.threads.charge(1);
	}

	pub fn uncharge_thread(&self) {
		self.threads.uncharge(1);
	}
}

// A Domain groups threads under a shared ResourceAccount. Holding an Arc<Domain>
// keeps the account alive, so an object can refund its charge on drop even after
// the owning thread is gone.
pub struct Domain {
	header: ObjectHeader,
	account: ResourceAccount,
}

impl Domain {
	// Create a Domain with the given resource caps (UNLIMITED for no cap).
	pub fn new(memory_limit: u64, handle_limit: u64, thread_limit: u64) -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), account: ResourceAccount::new(memory_limit, handle_limit, thread_limit) })
	}

	// The root Domain: no caps. Kernel threads live here so existing behavior is
	// unchanged; bounded Domains are created explicitly for sandboxed work.
	pub fn root() -> Arc<Self> {
		Self::new(UNLIMITED, UNLIMITED, UNLIMITED)
	}

	pub fn account(&self) -> &ResourceAccount {
		&self.account
	}
}

impl KernelObject for Domain {
	fn header(&self) -> &ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> ObjectType {
		ObjectType::Domain
	}

	fn as_any(&self) -> &dyn Any {
		self
	}
}
