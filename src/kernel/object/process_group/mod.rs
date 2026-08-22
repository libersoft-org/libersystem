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
//
// A MEMBER'S POSITION IS PART OF THE ABI, which is what the slot arrangement below exists for.
// `SYS_PROCESS_GROUP_STATS` promises per-member stats "in the order the processes were created
// into the group", and for a pipeline that order is the order of the line - so "which stage
// failed" is only answerable while slot `i` still means stage `i`. This was two collections,
// `members` and `records`, joined by index, with `live()` and `live_into()` compacting the first
// and nothing compacting the second: after the first member was dropped the two disagreed, a
// later stage's record was written into an earlier stage's slot or silently discarded, and
// `finished()` - the condition an ordinary group wait completes on - was one of the callers doing
// the compacting. One slot per stage, never moved and never removed, is what removes the class:
// there is no second collection to fall out of step with, and at `MAX_GROUP_MEMBERS` a tombstone
// costs less than the renumbering did.

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

// One stage, for the life of the group: its identity, a weak handle on it while it lives, and
// what it finished as once it has.
//
// The koid is held separately from the `Weak` because it is the identity that survives the
// process: a record is written from the process's own terminal path, and a slot must be findable
// after the last strong reference is gone.
struct MemberSlot {
	koid: u64,
	process: Weak<Process>,
	// `None` means the stage has not finished. Written once - the first terminal transition wins,
	// so a process killed after exiting cleanly is still recorded as having exited cleanly.
	record: Option<StageRecord>,
}

pub struct ProcessGroup {
	header: ObjectHeader,
	// ONE SLOT PER STAGE, in creation order, never moved and never removed. Weak, so membership
	// never extends a member's life; fixed at creation, because there is no join.
	//
	// The record lives beside the member rather than in a second vector indexed by the same
	// number. Captured when the member reaches a terminal state rather than read when somebody
	// asks, because by the time anybody asks the process may be gone: the group holds it weakly on
	// purpose, and holding it strongly would keep its user frames alive for the length of the job's
	// history. A record is a handful of bytes; a Process is an address space.
	slots: SpinLock<Vec<MemberSlot>>,
	// The membership count, which is also the slot count: slots are never removed, so this cannot
	// drift from the vector the way the pruned member list did. Stored so `size()` answers without
	// taking the lock.
	#[cfg(test)]
	original_size: usize,
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
		// DISTINCT STAGES, refused rather than deduplicated. Two capabilities naming one process is
		// ordinary, so a caller can hand the same process twice - and a repeated member cannot mean
		// what a pipeline needs it to mean: a record is found by koid and would always land in the
		// first slot, leaving the second reading as permanently running, and one group signal would
		// reach that process twice. Quadratic over a list bounded at 64.
		for (index, member) in members.iter().enumerate() {
			let koid = member.header().koid();
			if members[..index].iter().any(|earlier| earlier.header().koid() == koid) {
				return None;
			}
		}
		// BOOKED, then filled. `collect` allocated infallibly for a length a ring-3 caller chose -
		// bounded by `MAX_GROUP_MEMBERS`, which says the vector will never be enormous and nothing
		// about whether the heap can hold it. The gate did not look at `collect` at all.
		let mut slots: Vec<MemberSlot> = Vec::new();
		if slots.try_reserve_exact(members.len()).is_err() {
			return None;
		}
		for member in members {
			slots.push(MemberSlot { koid: member.header().koid(), process: Arc::downgrade(member), record: None });
		}
		#[cfg(test)]
		let original_size = slots.len();
		let group = crate::mem::heap::try_arc(Self {
			header: ObjectHeader::new(),
			slots: SpinLock::new(slots),
			#[cfg(test)]
			original_size,
		})?;
		// THE BACK-LINK, and it is what makes a finished stage answerable for. Each member takes a
		// weak reference to this group so that when it reaches a terminal state it can say what it
		// finished as - see `MemberSlot::record`. A member that cannot take one fails the whole
		// creation: a group that would silently never learn what one of its stages did is worse than
		// no group, because the caller would read the missing record as "still running".
		for (index, member) in members.iter().enumerate() {
			if !member.join_group(&group) {
				// UNWIND, so a refused creation leaves nothing behind. Without this the members
				// before the failure keep a back-link to a group that never came into existence -
				// which they would then walk on every terminal path for as long as they live.
				let koid = group.header.koid();
				for earlier in &members[..index] {
					earlier.leave_group(koid);
				}
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
	// gone. Matching by the koid held in the slot rather than by upgrading the weak reference: the
	// koid is the identity that survives the process, so a stage is still findable after the last
	// strong reference to it is gone.
	pub fn record_member(&self, process: &Process) {
		let koid = process.header().koid();
		let mut slots = self.slots.lock();
		let Some(slot) = slots.iter_mut().find(|slot| slot.koid == koid) else { return };
		if slot.record.is_some() {
			return;
		}
		let state = if process.is_killed() { crate::syscall::PROC_STATE_FAILED } else { crate::syscall::PROC_STATE_STOPPED };
		let (completion, completion_valid) = match process.exit_status() {
			Some(status) => (status, 1),
			None => (0, 0),
		};
		slot.record = Some(StageRecord { state, completion, completion_valid });
	}

	/// What each stage finished as, in creation order, into storage the caller already has -
	/// answering how many slots were written. `None` for a stage still running.
	///
	/// `out` should be `MAX_GROUP_MEMBERS` long, which covers any group in one pass.
	#[cfg(test)]
	pub fn records_into(&self, out: &mut [Option<StageRecord>]) -> usize {
		let slots = self.slots.lock();
		let mut written = 0;
		for slot in slots.iter() {
			if written == out.len() {
				break;
			}
			out[written] = slot.record;
			written += 1;
		}
		written
	}

	/// Per-stage stats in creation order, into the caller's buffer, from ONE pass under ONE lock -
	/// answering how many were written.
	///
	/// One pass because the alternative is joining two snapshots taken a moment apart: the records
	/// and the live members are the same slots, read at the same instant, and a caller that took
	/// them separately would be reading a group that could change between the two reads.
	///
	/// A FINISHED STAGE COMES FROM ITS RECORD and a running one from the process itself. The group
	/// holds its members weakly, so a finished pipeline's processes may already be gone; the record
	/// was taken when each reached a terminal state, which is the only moment it is certainly
	/// available. The live counters of a process that no longer exists are reported as zero rather
	/// than as a number nobody can act on.
	pub fn snapshot_into(&self, out: &mut [abi::ProcessStats]) -> usize {
		let slots = self.slots.lock();
		let mut written = 0;
		for slot in slots.iter() {
			if written == out.len() {
				break;
			}
			out[written] = match slot.record {
				Some(record) => abi::ProcessStats { messages_sent: 0, messages_received: 0, handle_count: 0, memory_bytes: 0, state: record.state, completion: record.completion, completion_valid: record.completion_valid },
				None => match slot.process.upgrade().filter(|process| !process.is_terminated()) {
					Some(process) => abi::ProcessStats { messages_sent: process.messages_sent(), messages_received: process.messages_received(), handle_count: process.handle_count(), memory_bytes: process.memory_bytes(), state: crate::syscall::PROC_STATE_RUNNING, completion: 0, completion_valid: 0 },
					None => abi::ProcessStats { state: crate::syscall::PROC_STATE_RUNNING, ..Default::default() },
				},
			};
			written += 1;
		}
		written
	}

	// The members still alive, as owning references. Dead slots are skipped and never removed, so
	// the living keep the positions they were created at.
	#[cfg(test)]
	pub fn live(&self) -> Vec<Arc<Process>> {
		let slots = self.slots.lock();
		// ALLOC-OK: the owned-list form, kept for the test suites. Every ring-3 path goes through
		// `live_into`, which fills storage the caller already has - a group is bounded at
		// `MAX_GROUP_MEMBERS`, so one fixed array covers a whole group in one pass.
		slots.iter().filter_map(|slot| slot.process.upgrade()).collect()
	}

	// The members still alive, into storage the caller already has - answering how many were
	// written. `out` should be `MAX_GROUP_MEMBERS` long, which covers any group in one pass.
	//
	// This READS. It used to open with `retain`, which renumbered the slot list from a function
	// whose name says otherwise - and everything that reads the live set reaches it, `finished()`
	// included, so an ordinary pipeline waiting on its own job compacted its own member list
	// mid-run.
	//
	// The lock is released before the caller acts on what it was given: signalling a member takes
	// the scheduler's lock, and dropping the last reference to one runs its teardown.
	pub fn live_into(&self, out: &mut [Option<Arc<Process>>]) -> usize {
		let slots = self.slots.lock();
		let mut written = 0;
		for slot in slots.iter() {
			if written == out.len() {
				break;
			}
			if let Some(process) = slot.process.upgrade() {
				out[written] = Some(process);
				written += 1;
			}
		}
		written
	}

	// Whether every member has reached a terminal state - the condition a group wait completes
	// on. Uses the same `is_terminated` a single Process handle becomes waitable on, so a group
	// and a lone process agree on what "finished" means rather than each deciding. A slot whose
	// process is gone is finished by definition, which is the state a fully reaped stage reaches.
	pub fn finished(&self) -> bool {
		let slots = self.slots.lock();
		slots.iter().all(|slot| match slot.process.upgrade() {
			Some(process) => process.is_terminated(),
			None => true,
		})
	}

	// How many processes this group was created over, live or not.
	#[cfg(test)]
	pub fn size(&self) -> usize {
		self.original_size
	}
}
