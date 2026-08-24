//! Who owns a logical CPU id, and for how long.
//!
//! Both device-tree ports wake their secondaries the same way - name a target to firmware, hand it
//! a logical id, wait for it to report in - and both used to get the same two things wrong. This
//! crate holds the part that is neither PSCI nor SBI:
//!
//! - **Sizing.** A firmware topology is not a promise. It can leave out the core reading it, name
//!   one core twice, or declare more cores than the per-CPU pool holds. Every one of those turns
//!   "N declared CPUs" into more than N logical ids, and the id is an index into the per-CPU table,
//!   the run queues and the stack block. [`Topology::resolve`] answers what to size those from.
//!
//! - **Ownership.** A firmware call that RETURNS SUCCESS has handed the entry point to a core that
//!   may still be on its way. A start attempt that times out therefore ABANDONS its id rather than
//!   returning it: the late core arrives holding an id and a stack, and both must still be its own.
//!   Only a call the firmware REFUSED is safe to take back, because in that one case nothing was
//!   ever released. [`Bringup`] is that state machine, and [`Bringup::claim`] is the arriving
//!   core's side of it.
//!
//! It is here rather than in the kernel because the interesting cases - a refusal, a timeout
//! followed by a late arrival, an id claimed twice, a tree that forgot the boot core - are the ones
//! QEMU has no switch for. A host drives them through [`Firmware`].

#![no_std]

use core::sync::atomic::{AtomicU32, Ordering};

/// Nobody has been offered this id.
pub const SLOT_FREE: u32 = 0;
/// Offered to a core whose start attempt has not resolved. Reserved BEFORE the firmware call,
/// because the core can arrive during it.
pub const SLOT_PENDING: u32 = 1;
/// Claimed by the core it was offered to.
pub const SLOT_ONLINE: u32 = 2;
/// Offered, the firmware took the call, and the core did not report in within the bound. The id
/// stays spoken for until the machine is reset.
pub const SLOT_ABANDONED: u32 = 3;

/// What the usable firmware topology says, once it has been made safe to size an array from.
pub struct Topology<'a> {
	secondaries: &'a [u64],
	boot_declared: bool,
	duplicates: usize,
	parked: usize,
	declared: usize,
}

impl<'a> Topology<'a> {
	/// Reduce a declared id list to the secondaries this kernel may start.
	///
	/// `declared` is the firmware's list in its own order, `boot_id` names the core running this
	/// code, and `out` is the caller's buffer - its length is the per-CPU pool bound, which is what
	/// makes a tree with more cores than the pool a parked remainder rather than an overrun.
	///
	/// Three things are dropped, and each is counted rather than hidden:
	/// - every occurrence of the boot core, which is already running and must not be started again;
	/// - a repeat of an id already in the list, because a second `CPU_ON` to a core that is on
	///   consumes an id and a stack for a core that will never claim them;
	/// - the remainder past the pool bound.
	pub fn resolve(declared: &[u64], boot_id: u64, out: &'a mut [u64]) -> Topology<'a> {
		// One entry is the boot core's, whatever the tree says: it holds logical id 0 and it is
		// running. So the pool has `len() - 1` places left for cores that are not it.
		let room = out.len().saturating_sub(1);
		let mut n = 0usize;
		let mut boot_declared = false;
		let mut duplicates = 0usize;
		let mut parked = 0usize;
		for &id in declared {
			if id == boot_id {
				// NOT AN ERROR, AND NOT A SECONDARY. The tree naming the running core is the
				// ordinary case; naming it twice is the odd one, and neither may be started.
				boot_declared = true;
				continue;
			}
			if out[..n].contains(&id) {
				duplicates += 1;
				continue;
			}
			if n == room {
				parked += 1;
				continue;
			}
			out[n] = id;
			n += 1;
		}
		Topology { secondaries: &out[..n], boot_declared, duplicates, parked, declared: declared.len() }
	}

	/// The cores to start, in the firmware's order, each named exactly once and none of them the
	/// core asking.
	pub fn secondaries(&self) -> &[u64] {
		self.secondaries
	}

	/// How many logical ids this boot can ever hand out: the boot core's, plus one per secondary
	/// that may be started.
	///
	/// THIS, NOT THE DECLARED COUNT, is what a per-CPU table or a stack block is sized from. A tree
	/// that omits the boot core has N secondaries and N+1 ids, and an N-entry allocation is then
	/// one short of what a successful boot writes.
	pub fn slots(&self) -> usize {
		self.secondaries.len() + 1
	}

	/// Whether the firmware list named the core reading it. False means the topology describes a
	/// machine this core is not in, which is worth saying out loud even though the boot continues.
	pub fn boot_declared(&self) -> bool {
		self.boot_declared
	}

	/// Repeated ids dropped from the list.
	pub fn duplicates(&self) -> usize {
		self.duplicates
	}

	/// Cores the pool has no room for. They are never started and never counted as failures.
	pub fn parked(&self) -> usize {
		self.parked
	}

	/// How many entries the firmware list had, which is what an "n of m online" line reports
	/// against.
	pub fn declared(&self) -> usize {
		self.declared
	}
}

/// What a start attempt did, for the caller to report.
pub enum Event {
	/// The firmware would not take the call. No core was released, so the id goes back.
	Refused { target: u64, logical_id: u64, status: i64 },
	/// The firmware took the call and the core did not report in within the bound. The id is
	/// abandoned.
	Abandoned { target: u64, logical_id: u64 },
	/// The core claimed its id and reported in.
	Online { target: u64, logical_id: u64 },
	/// More secondaries than there are slots. Defensive: [`Topology::resolve`] already parks the
	/// remainder, so reaching this means the caller sized something from a different number.
	PoolExhausted { target: u64 },
}

/// The firmware side of a start attempt, and the tally the started core reports into.
pub trait Firmware {
	/// Ask the firmware to release `target` at the secondary entry point holding `logical_id`.
	/// Zero is success and means the entry point HAS BEEN HANDED OVER; anything else means it has
	/// not.
	fn start(&mut self, target: u64, logical_id: u64) -> i64;

	/// Wait, under whatever bound the caller judges right, for the secondary tally to reach
	/// `reported`. True if it did.
	///
	/// The bound is the implementation's because it is a property of the machine, not of this rule:
	/// a spin count on a real core, a scripted answer on a host.
	fn await_report(&mut self, reported: u32) -> bool;

	/// Say what happened. The kernel prints it; a test records it.
	fn note(&mut self, event: Event);
}

/// What a whole bring-up did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
	/// Secondaries that claimed their id and reported in.
	pub online: usize,
	/// Attempts the firmware refused. Their ids were reused.
	pub refused: usize,
	/// Attempts that timed out. Their ids are spoken for until reset.
	pub abandoned: usize,
	/// How many logical ids this boot has handed out and not taken back, the boot core's included.
	///
	/// NOT `online + 1`. An abandoned id leaves a gap, so a core that came up after one holds an id
	/// higher than the number of cores online - and the per-CPU table, the run queues and the
	/// controller-id map are all indexed by that id. Sizing them from the online count is what would
	/// put a working core one entry past the end of every one of them.
	pub ids_used: usize,
}

/// The logical ids of one boot, and who owns each.
pub struct Bringup<'a> {
	slots: &'a [AtomicU32],
}

impl<'a> Bringup<'a> {
	/// Wrap the id table. `slots` is shared with the arriving cores, so it is the same storage
	/// [`Bringup::claim`] is called against.
	pub const fn new(slots: &'a [AtomicU32]) -> Self {
		Self { slots }
	}

	/// The arriving core's side: take the id offered to it, or refuse to run.
	///
	/// True means the id is this core's and nothing else holds it. False means the attempt that
	/// offered it has already been resolved - it timed out and was abandoned, or this is a second
	/// arrival on an id already claimed - and the caller must park WITHOUT touching per-CPU state,
	/// because the state under that id may belong to another core.
	pub fn claim(&self, logical_id: u64) -> bool {
		let Some(slot) = self.slots.get(logical_id as usize) else {
			return false;
		};
		slot.compare_exchange(SLOT_PENDING, SLOT_ONLINE, Ordering::AcqRel, Ordering::Acquire).is_ok()
	}

	/// Read an id's state. For a caller that reports what a boot ended up holding.
	pub fn state(&self, logical_id: u64) -> u32 {
		self.slots.get(logical_id as usize).map_or(SLOT_FREE, |s| s.load(Ordering::Acquire))
	}

	/// Start each secondary in turn, waiting for it before offering the next id.
	///
	/// Serial by design: it is what keeps the ids of the cores that answer contiguous, and it is
	/// the only way a timeout can be told from a slow arrival at all.
	pub fn run<F: Firmware>(&self, secondaries: &[u64], fw: &mut F) -> Outcome {
		let mut out = Outcome { ids_used: 1, ..Outcome::default() };
		// The next id to offer. It tracks `online` while every core answers and runs ahead of it
		// once one does not, because an abandoned id is not given away twice.
		let mut next_id = 1usize;
		for &target in secondaries {
			if next_id >= self.slots.len() {
				fw.note(Event::PoolExhausted { target });
				continue;
			}
			let logical_id = next_id as u64;
			// Reserved BEFORE the call, because the core can arrive during it.
			self.slots[next_id].store(SLOT_PENDING, Ordering::Release);
			let status = fw.start(target, logical_id);
			if status != 0 {
				// A REFUSED CALL IS THE ONE CASE NOTHING CAN ARRIVE FROM. The firmware answering
				// with an error means no core took the entry point, so this id and its stack were
				// never handed to anybody and the next target may have them.
				self.slots[next_id].store(SLOT_FREE, Ordering::Release);
				out.refused += 1;
				fw.note(Event::Refused { target, logical_id, status });
				continue;
			}
			if !fw.await_report(out.online as u32 + 1) {
				// ABANDONED, NOT RETURNED. The firmware took the call, so this core may still be
				// on its way with this id in hand; the id and its stack stay its own for the life
				// of the boot and the next target gets the following one.
				self.slots[next_id].store(SLOT_ABANDONED, Ordering::Release);
				next_id += 1;
				out.abandoned += 1;
				fw.note(Event::Abandoned { target, logical_id });
				continue;
			}
			out.online += 1;
			next_id += 1;
			fw.note(Event::Online { target, logical_id });
		}
		out.ids_used = next_id;
		out
	}
}
