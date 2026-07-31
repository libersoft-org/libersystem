// ProcessGroup - a bounded, fixed set of processes that are signalled and waited on together.
//
// A pipeline is one job to the person who typed it: Ctrl+C interrupts `a | b | c`, not just
// whichever stage happens to hold the terminal. Today ConsoleService holds a single Process
// handle for the foreground job, so a multi-stage pipeline would leave every stage but one
// running with nothing able to reach it. M0035j deferred this deliberately and named it; this
// is that object.
//
// Three properties matter and each is a refusal of a Unix behaviour rather than an omission.
//
// Membership is fixed by the trusted launcher AT CREATION and never grows. A Unix process group
// can be joined with `setpgid`, which is how a process escapes the group its parent put it in -
// and there is no reason a stage of a pipeline should be able to leave the job it belongs to.
// Sealing membership makes "which processes does this signal reach" answerable from the handle
// alone.
//
// The group holds Weak references, so it never keeps a dead process alive. A group that owned
// its members strongly would be a leak the length of the job's history: the whole point is to
// reach processes that are RUNNING, and a member that has exited is exactly the one nothing
// needs to signal.
//
// Authority comes from the handle, not from membership. Holding a group capability with MANAGE
// is what permits signalling it; being IN the group grants nothing, so one stage cannot signal
// its siblings. That is the confused-deputy shape Unix has with process groups, where any
// member may `kill(0, sig)` the rest.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;

use super::process::Process;
use super::{KernelObject, ObjectHeader, ObjectType, impl_kernel_object};
use crate::sync::SpinLock;

// The most processes one group may hold. A pipeline of this many stages is already far past
// what anyone types, and the cap is what keeps a group from being a way to make the kernel
// allocate without bound - the same reasoning every other bounded structure here follows.
pub const MAX_GROUP_MEMBERS: usize = 64;

pub struct ProcessGroup {
	header: ObjectHeader,
	// Weak, so membership never extends a member's life. Fixed at creation: there is no join.
	members: SpinLock<Vec<Weak<Process>>>,
}

impl_kernel_object!(ProcessGroup, ProcessGroup);

impl ProcessGroup {
	// Create a group over `members`. Returns None above the cap, so a caller that assembled too
	// many stages is refused at creation rather than silently truncated - a group missing a
	// stage would signal an incomplete job and nothing would say so.
	pub fn create(members: &[Arc<Process>]) -> Option<Arc<Self>> {
		if members.is_empty() || members.len() > MAX_GROUP_MEMBERS {
			return None;
		}
		let weak = members.iter().map(Arc::downgrade).collect();
		Some(Arc::new(Self { header: ObjectHeader::new(), members: SpinLock::new(weak) }))
	}

	// The members still alive, as owning references. Dead entries are dropped from the list as
	// they are noticed, so a long-lived group does not accumulate tombstones.
	pub fn live(&self) -> Vec<Arc<Process>> {
		let mut guard = self.members.lock();
		guard.retain(|member| member.strong_count() > 0);
		guard.iter().filter_map(Weak::upgrade).collect()
	}

	// Whether every member has reached a terminal state - the condition a group wait completes
	// on. Uses the same `is_terminated` a single Process handle becomes waitable on, so a group
	// and a lone process agree on what "finished" means rather than each deciding. An empty
	// group is finished by definition, which is the state a fully reaped job reaches.
	pub fn finished(&self) -> bool {
		self.live().iter().all(|process| process.is_terminated())
	}

	// How many processes this group was created over, live or not. Reported for observability;
	// it never changes, because membership is sealed.
	pub fn size(&self) -> usize {
		self.members.lock().len()
	}
}
