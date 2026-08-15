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
	// The membership count at creation. `members` is pruned as processes die, so it
	// cannot answer a question about the original set.
	original_size: usize,
	// WHAT EACH STAGE FINISHED AS, in creation order - which for a pipeline is the order of the
	// line, so "which stage failed" is answerable and not just "something did".
	//
	// Captured when the member reaches a terminal state rather than read when somebody asks,
	// because by the time anybody asks the process may be gone: the group holds it weakly on
	// purpose, and holding it strongly would keep its user frames alive for the length of the job's
	// history. A record is a handful of bytes; a Process is an address space.
	//
	// `None` means the stage has not finished. A slot is written once - the first terminal
	// transition wins, so a process killed after exiting cleanly is still recorded as having
	// exited cleanly.
	records: SpinLock<Vec<Option<StageRecord>>>,
}

/// What one stage of a group finished as.
#[derive(Clone, Copy)]
pub struct StageRecord {
	/// `PROC_STATE_STOPPED` for a clean exit, `PROC_STATE_FAILED` for a fault or a kill.
	pub state: u64,
	/// What the program passed to `exit_with`, when it got to say. Zero is both the commonest
	/// success value and the natural "nothing here", so a caller must not have to guess which it is
	/// looking at - which is why `completion_valid` exists beside it.
	pub completion: u64,
	pub completion_valid: u64,
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
		// BOOKED, then filled. `collect` allocated infallibly for a length a ring-3 caller chose -
		// bounded by `MAX_GROUP_MEMBERS`, which says the vector will never be enormous and nothing
		// about whether the heap can hold it. The gate did not look at `collect` at all.
		let mut weak: Vec<Weak<Process>> = Vec::new();
		if weak.try_reserve_exact(members.len()).is_err() {
			return None;
		}
		for member in members {
			weak.push(Arc::downgrade(member));
		}
		let original_size = weak.len();
		let mut records: Vec<Option<StageRecord>> = Vec::new();
		if records.try_reserve_exact(original_size).is_err() {
			return None;
		}
		records.resize(original_size, None);
		let group = crate::mem::heap::try_arc(Self { header: ObjectHeader::new(), members: SpinLock::new(weak), original_size, records: SpinLock::new(records) })?;
		// THE BACK-LINK, and it is what makes a finished stage answerable for. Each member takes a
		// weak reference to this group so that when it reaches a terminal state it can say what it
		// finished as - see `records`. A member that cannot take one fails the whole creation: a
		// group that would silently never learn what one of its stages did is worse than no group,
		// because the caller would read the missing record as "still running".
		for member in members {
			if !member.join_group(&group) {
				return None;
			}
			// ALREADY GONE IS STILL AN ANSWER. A process that terminated between the caller looking
			// it up and this line has already run its notification, with this group not yet on its
			// list - so the record is taken here instead. Without it a group built over a stage that
			// exited immediately would report that stage as running forever.
			if member.is_terminated() {
				group.record_member(member);
			}
		}
		Some(group)
	}

	// Write what `process` finished as into its slot, if it has not been written already.
	//
	// Called from the process's own terminal paths and from `create` for a member that was already
	// gone. Matching by koid rather than by pointer because the members are weak and the caller is
	// a `&Process` - the koid is the identity that survives both.
	pub fn record_member(&self, process: &Process) {
		let koid = process.header().koid();
		let position = {
			let members = self.members.lock();
			members.iter().position(|member| member.upgrade().is_some_and(|member| member.header().koid() == koid))
		};
		let Some(position) = position else { return };
		let mut records = self.records.lock();
		if records[position].is_some() {
			return;
		}
		let state = if process.is_killed() { crate::syscall::PROC_STATE_FAILED } else { crate::syscall::PROC_STATE_STOPPED };
		let (completion, completion_valid) = match process.exit_status() {
			Some(status) => (status, 1),
			None => (0, 0),
		};
		records[position] = Some(StageRecord { state, completion, completion_valid });
	}

	/// What each stage finished as, in creation order. `None` for a stage still running.
	pub fn records(&self) -> Vec<Option<StageRecord>> {
		self.records.lock().clone()
	}

	// The members still alive, as owning references. Dead entries are dropped from the list as
	// they are noticed, so a long-lived group does not accumulate tombstones.
	pub fn live(&self) -> Vec<Arc<Process>> {
		let mut guard = self.members.lock();
		guard.retain(|member| member.strong_count() > 0);
		// ALLOC-OK: the owned-list form, kept for the test suites. Every ring-3 path goes through
		// `live_into`, which fills storage the caller already has - a group is bounded at
		// `MAX_GROUP_MEMBERS`, so one fixed array covers a whole group in one pass.
		guard.iter().filter_map(Weak::upgrade).collect()
	}

	// The members still alive, into storage the caller already has - answering how many were
	// written. `out` should be `MAX_GROUP_MEMBERS` long, which covers any group in one pass.
	//
	// The lock is released before the caller acts on what it was given: signalling a member takes
	// the scheduler's lock, and dropping the last reference to one runs its teardown.
	pub fn live_into(&self, out: &mut [Option<Arc<Process>>]) -> usize {
		let mut guard = self.members.lock();
		guard.retain(|member| member.strong_count() > 0);
		let mut written = 0;
		for member in guard.iter() {
			if written == out.len() {
				break;
			}
			if let Some(process) = member.upgrade() {
				out[written] = Some(process);
				written += 1;
			}
		}
		written
	}

	// Whether every member has reached a terminal state - the condition a group wait completes
	// on. Uses the same `is_terminated` a single Process handle becomes waitable on, so a group
	// and a lone process agree on what "finished" means rather than each deciding. An empty
	// group is finished by definition, which is the state a fully reaped job reaches.
	pub fn finished(&self) -> bool {
		let mut members: [Option<Arc<Process>>; MAX_GROUP_MEMBERS] = [const { None }; _];
		let live = self.live_into(&mut members);
		members.iter().take(live).all(|member| member.as_ref().is_some_and(|process| process.is_terminated()))
	}

	// How many processes this group was created over, live or not.
	//
	// Stored, because it is documented as never changing and `live()` prunes dead weak
	// references out of the same vector it was being read from - so it shrank as members
	// died, which is the opposite of what it says.
	pub fn size(&self) -> usize {
		self.original_size
	}

	// Wake anyone waiting on this group. Called when a member reaches a terminal state.
	//
	// A waiter registers on the GROUP's koid while a process termination wakes only the
	// process's own, and nothing connected the two - so a group could report `finished()`
	// while a waiter without a timeout stayed parked forever. The group has to be told.
	pub fn notify_member_terminated(&self) {
		crate::sched::wake_object(self.header.koid());
	}
}
