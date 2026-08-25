// What the implementation DID, in the vocabulary the model speaks.
//
// WHY A SINK AND NOT A LOG. A log says what a person should read; this says what a checker can
// replay. Every record is one atomic action from `docs/spec/capability/MODEL_MAP.md` - the same
// fourteen the specification has - carrying only what the model's guards read: which process, which
// slot, which generation, which rights, and how it ended. No pointers, no payload bytes, no object
// addresses: a trace that carried them would be a trace nobody could publish, and none of it is
// what the model is about.
//
// ALLOCATION-FREE AND FIXED-SIZE. It runs inside the paths it observes - `take_for_transfer` holds
// the handle-table lock - so an allocation here would be an allocation under a lock the allocator
// may itself want. A full ring stops recording and says so: a truncated trace that pretended to be
// complete would be a checker's clean bill of health over the part that fitted.

#[cfg(test)]
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// The actions, numbered as the model names them. The numbers are the wire format: a checker reads
// them, so they are appended to rather than renumbered.
pub const TAKE: u8 = 1;
pub const COMMIT_TAKE: u8 = 2;
pub const RESTORE_TAKE: u8 = 3;
pub const ABANDON_TAKE: u8 = 4;
pub const BOOK: u8 = 5;
pub const UNBOOK: u8 = 6;
pub const INSTALL: u8 = 7;
pub const CLOSE: u8 = 8;
pub const TERMINATE: u8 = 9;
pub const ENQUEUE: u8 = 10;
pub const PEEK: u8 = 11;
pub const DEQUEUE: u8 = 12;
pub const RETURN_TO_HEAD: u8 = 13;
pub const COMMIT_DELIVERY: u8 = 14;
pub const INSTALL_INTO_CLOSED: u8 = 15;
// A capability placed into a slot by a path OUTSIDE the transfer protocol - `try_place`, which is
// what an ordinary "create an object, hand back a handle" syscall uses. The model has no action for
// this: its `Init` simply starts with a live capability in the sender's slot. So a seed is the
// modelled boundary's starting state, and the checker accepts it only BEFORE the table has taken
// any other step. One arriving mid-run is a capability appearing from nowhere, which is the thing
// `NoForgery` exists to forbid.
pub const SEED: u8 = 16;

// How an action ended, when it can end more than one way.
pub const OK: u8 = 0;
pub const REFUSED: u8 = 1;

// One atomic action. `repr(C)` and all-integer so the guest can print it and a host can read it
// without either side knowing the other's layout rules.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Event {
	pub action: u8,
	pub outcome: u8,
	// WHO THE ACTION BELONGS TO, ABSTRACTLY. Not a process id and not a pointer: for a table action
	// it is an index into the small set of tables a driver made, which is what the model's `Procs`
	// is; for a queue action it is the RECEIVING ENDPOINT whose inbox the action touched, since the
	// model's `queue` is one channel's and a checker sharing one across channels would be checking
	// an order that never existed. Channel identities carry the high bit so the two never collide.
	pub party: u16,
	pub slot: u16,
	pub generation: u32,
	pub rights: u32,
	// The message identity a channel action names, 0 when the action names none.
	pub message: u64,
}

#[cfg(test)]
const CAPACITY: usize = 4096;

// A fixed ring, written under whichever lock the observed action already holds. The index is
// atomic because two cores can be in two different tables' actions at once; the RECORD is written
// before the index is published, so a reader never sees a half-written one.
#[cfg(test)]
static mut EVENTS: [Event; CAPACITY] = [Event { action: 0, outcome: 0, party: 0, slot: 0, generation: 0, rights: 0, message: 0 }; CAPACITY];
#[cfg(test)]
static NEXT: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static RECORDING: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static OVERFLOWED: AtomicBool = AtomicBool::new(false);

// Start recording, discarding whatever a previous schedule left.
#[cfg(test)]
pub fn start() {
	NEXT.store(0, Ordering::Release);
	OVERFLOWED.store(false, Ordering::Release);
	RECORDING.store(true, Ordering::Release);
}

#[cfg(test)]
pub fn stop() {
	RECORDING.store(false, Ordering::Release);
}

// Whether the ring ran out. A checker is told, because a trace that stopped early and did not say so
// is a clean result over the part that fitted.
#[cfg(test)]
pub fn overflowed() -> bool {
	OVERFLOWED.load(Ordering::Acquire)
}

#[cfg(test)]
pub fn len() -> usize {
	NEXT.load(Ordering::Acquire).min(CAPACITY)
}

#[cfg(test)]
pub fn get(index: usize) -> Option<Event> {
	if index >= len() {
		return None;
	}
	// SAFETY: `index` is below the published count, so this record was fully written before the
	// index that admits it was published.
	Some(unsafe { (&raw const EVENTS).cast::<Event>().add(index).read() })
}

// Record one action. Called from inside the paths it observes.
//
// PRESENT IN BOTH CONFIGURATIONS, AND EMPTY IN ONE. The call sites name the action constants as
// arguments, so gating the whole module would gate the arguments of calls that remain - and the
// alternative, a `cfg` at every call site, puts the conditional in fourteen places instead of one.
#[cfg(not(test))]
pub fn record(_event: Event) {}

#[cfg(test)]
pub fn record(event: Event) {
	if !RECORDING.load(Ordering::Acquire) {
		return;
	}
	let at = NEXT.fetch_add(1, Ordering::AcqRel);
	if at >= CAPACITY {
		OVERFLOWED.store(true, Ordering::Release);
		return;
	}
	// SAFETY: `at` is this caller's alone - `fetch_add` hands each index out once - and it is below
	// the capacity, so no other writer touches this record and no reader admits it until the count
	// above passes it.
	unsafe {
		(&raw mut EVENTS).cast::<Event>().add(at).write(event);
	}
}

// The shorthands the call sites use, so a boundary reads as one line rather than a struct literal.
pub fn handle_event(action: u8, table: u16, slot: u16, generation: u32, rights: u32, outcome: u8) {
	record(Event { action, outcome, party: table, slot, generation, rights, message: 0 });
}

pub fn channel_event(action: u8, endpoint: u16, message: u64, outcome: u8) {
	record(Event { action, outcome, party: endpoint, slot: 0, generation: 0, rights: 0, message });
}

// The high bit that keeps channel identities out of the handle tables' numbering.
pub const CHANNEL_PARTY: u16 = 0x8000;
