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

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::process::Process;
use super::{KernelObject, ObjectHeader, ObjectType};
use crate::sync::SpinLock;

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

// A Domain is a node in a tree of resource containers. It owns a ResourceAccount
// and links to a parent and children, so limits compose hierarchically (a charge
// counts against the Domain and every ancestor) and a whole subtree can be torn
// down at once. Processes accounted to a Domain are tracked weakly so the Domain
// can terminate them when it is killed. Holding an Arc<Domain> keeps the account
// alive, so an object can refund its charge on drop even after the owning thread
// is gone.
pub struct Domain {
	header: ObjectHeader,
	account: ResourceAccount,
	// Parent in the Domain tree, or None for a standalone/root Domain. Weak so a
	// child does not keep its parent alive; the parent is kept alive by its own
	// parent (up to a root the scheduler holds), so the upgrade succeeds while the
	// child is reachable in the tree.
	parent: Option<Weak<Domain>>,
	// Child Domains, held strongly: a parent owns its subtree.
	children: SpinLock<Vec<Arc<Domain>>>,
	// Processes accounted to this Domain, tracked weakly (their threads hold the
	// strong references). Lets the Domain reach its processes to terminate them.
	processes: SpinLock<Vec<Weak<Process>>>,
	// Set once the Domain is killed; its processes' threads observe this at their
	// next scheduling point and exit.
	killed: AtomicBool,
}

impl Domain {
	// Create a standalone Domain (no parent) with the given resource caps
	// (UNLIMITED for no cap).
	pub fn new(memory_limit: u64, handle_limit: u64, thread_limit: u64) -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), account: ResourceAccount::new(memory_limit, handle_limit, thread_limit), parent: None, children: SpinLock::new(Vec::new()), processes: SpinLock::new(Vec::new()), killed: AtomicBool::new(false) })
	}

	// The root Domain: no caps, no parent. Kernel threads live here so existing
	// behavior is unchanged; bounded Domains are created explicitly.
	pub fn root() -> Arc<Self> {
		Self::new(UNLIMITED, UNLIMITED, UNLIMITED)
	}

	// Create a child Domain under `parent` with the given caps, linked into the
	// parent's subtree. The child's charges also count against the parent and
	// every ancestor, and killing the parent kills the child.
	pub fn new_child(parent: &Arc<Domain>, memory_limit: u64, handle_limit: u64, thread_limit: u64) -> Arc<Self> {
		let child = Arc::new(Self { header: ObjectHeader::new(), account: ResourceAccount::new(memory_limit, handle_limit, thread_limit), parent: Some(Arc::downgrade(parent)), children: SpinLock::new(Vec::new()), processes: SpinLock::new(Vec::new()), killed: AtomicBool::new(false) });
		parent.children.lock().push(child.clone());
		child
	}

	pub fn account(&self) -> &ResourceAccount {
		&self.account
	}

	// The parent Domain, if this is not a standalone/root Domain.
	fn parent(&self) -> Option<Arc<Domain>> {
		self.parent.as_ref().and_then(Weak::upgrade)
	}

	pub fn is_killed(&self) -> bool {
		self.killed.load(Ordering::Acquire)
	}

	// Register a process as accounted to this Domain so it can be terminated when
	// the Domain is killed. Dead weak entries are pruned on the way in so the list
	// stays bounded to live processes.
	pub fn register_process(&self, process: &Arc<Process>) {
		let mut list = self.processes.lock();
		list.retain(|weak| weak.strong_count() > 0);
		list.push(Arc::downgrade(process));
	}

	// Kill this Domain and its entire subtree: mark every Domain killed and
	// terminate every process accounted to them, refunding their resources. The
	// terminated processes' threads observe the kill at their next scheduling
	// point and exit, releasing the last references.
	pub fn kill(&self) {
		self.killed.store(true, Ordering::Release);
		// Upgrade and act outside the lock so termination (which refunds handles
		// to this Domain) does not re-enter a held lock.
		let processes: Vec<Arc<Process>> = self.processes.lock().iter().filter_map(Weak::upgrade).collect();
		for process in processes {
			process.terminate();
		}
		let children: Vec<Arc<Domain>> = self.children.lock().iter().cloned().collect();
		for child in children {
			child.kill();
		}
	}

	// Hierarchical charging. A charge counts against this Domain and every
	// ancestor, so a process exceeds neither its own Domain's limit nor any
	// ancestor's aggregate. The enforced (try_*) charges roll back the level
	// already charged if an ancestor refuses.

	pub fn try_charge_memory(&self, bytes: u64) -> bool {
		if !self.account.try_charge_memory(bytes) {
			return false;
		}
		if let Some(parent) = self.parent() {
			if !parent.try_charge_memory(bytes) {
				self.account.uncharge_memory(bytes);
				return false;
			}
		}
		true
	}

	pub fn uncharge_memory(&self, bytes: u64) {
		self.account.uncharge_memory(bytes);
		if let Some(parent) = self.parent() {
			parent.uncharge_memory(bytes);
		}
	}

	pub fn try_charge_handle(&self) -> bool {
		if !self.account.try_charge_handle() {
			return false;
		}
		if let Some(parent) = self.parent() {
			if !parent.try_charge_handle() {
				self.account.uncharge_handles(1);
				return false;
			}
		}
		true
	}

	pub fn charge_handle(&self) {
		self.account.charge_handle();
		if let Some(parent) = self.parent() {
			parent.charge_handle();
		}
	}

	pub fn uncharge_handles(&self, count: u64) {
		self.account.uncharge_handles(count);
		if let Some(parent) = self.parent() {
			parent.uncharge_handles(count);
		}
	}

	pub fn try_charge_thread(&self) -> bool {
		if !self.account.try_charge_thread() {
			return false;
		}
		if let Some(parent) = self.parent() {
			if !parent.try_charge_thread() {
				self.account.uncharge_thread();
				return false;
			}
		}
		true
	}

	pub fn charge_thread(&self) {
		self.account.charge_thread();
		if let Some(parent) = self.parent() {
			parent.charge_thread();
		}
	}

	pub fn uncharge_thread(&self) {
		self.account.uncharge_thread();
		if let Some(parent) = self.parent() {
			parent.uncharge_thread();
		}
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
