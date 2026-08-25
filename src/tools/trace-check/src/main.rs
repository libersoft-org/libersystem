// Every step the kernel took, asked of the model: was that enabled?
//
// WHAT THIS IS. `docs/spec/capability/Transfer.tla` says which actions are possible from which
// states. The kernel emits what it actually did, in the same vocabulary (`object::trace`). This
// replays the second against the first: for each event it checks the model's GUARD for that action,
// and refuses a trace containing a step the model would not have allowed.
//
// WHAT IT IS NOT, and the milestone says so in as many words: this is SAMPLED trace refinement over
// a selected boundary. It shows that the steps this run took are model steps. It does not show that
// every model trace is a Rust execution, and no amount of running it would.
//
// THE GUARDS ARE A HAND-WRITTEN MIRROR of the specification's, reviewed against it, and each one
// names the action it mirrors. That is a weaker link than generating one from the other, and it is
// the link this milestone has; the mapping is in `MODEL_MAP.md` and the actions are numbered in
// `object::trace`.

use std::collections::VecDeque;

const TAKE: u8 = 1;
const COMMIT_TAKE: u8 = 2;
const RESTORE_TAKE: u8 = 3;
const ABANDON_TAKE: u8 = 4;
const BOOK: u8 = 5;
const UNBOOK: u8 = 6;
const INSTALL: u8 = 7;
const CLOSE: u8 = 8;
const TERMINATE: u8 = 9;
const ENQUEUE: u8 = 10;
const PEEK: u8 = 11;
const DEQUEUE: u8 = 12;
const RETURN_TO_HEAD: u8 = 13;
const COMMIT_DELIVERY: u8 = 14;
const INSTALL_INTO_CLOSED: u8 = 15;
const SEED: u8 = 16;

// The high bit `object::trace` sets on a channel endpoint's identity, keeping it out of the handle
// tables' numbering.
const CHANNEL_PARTY: u16 = 0x8000;

const OK: u8 = 0;
const REFUSED: u8 = 1;

// `Rights::TRANSFER`, as `abi` numbers it: the eighth bit. A take requires it, which is the one
// rights check in the model's transfer path.
const RIGHT_TRANSFER: u32 = 1 << 7;

#[derive(Clone, Copy, Debug)]
struct Event {
	action: u8,
	outcome: u8,
	party: u16,
	slot: u16,
	generation: u32,
	rights: u32,
	message: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SlotState {
	Free,
	Live,
	Reserved,
}

#[derive(Clone, Copy)]
struct Slot {
	state: SlotState,
	generation: u32,
}

#[derive(Default)]
struct Table {
	// `None` is a slot this trace has not mentioned yet. It is NOT the same as a free one: an
	// unmentioned slot has no generation the checker can hold the implementation to, and the first
	// event to name it is what fixes one.
	slots: Vec<Option<Slot>>,
	booked: usize,
	outstanding: Vec<u16>,
	closed: bool,
}

impl Table {
	// The slot as the model believes it stands, adopting the generation the trace presents if this
	// is the first time the slot has been named.
	fn slot(&mut self, index: u16, generation: u32) -> &mut Slot {
		while self.slots.len() <= index as usize {
			self.slots.push(None);
		}
		self.slots[index as usize].get_or_insert(Slot { state: SlotState::Free, generation })
	}

	// Recycle, mirroring `retire_or_recycle`: the generation advances, and a slot at the end of its
	// generations is retired rather than wrapped back onto a value a dead handle still names.
	fn recycle(&mut self, index: u16) {
		let slot = self.slot(index, 0);
		slot.state = SlotState::Free;
		slot.generation = slot.generation.checked_add(1).unwrap_or(u32::MAX);
	}
}

// THE ONE CHECK THAT MAKES A GENERATION MEAN ANYTHING: the value a handle presents is the value the
// slot holds. A trace in which they differ is a trace containing a use of a stale handle, which the
// model has no step for.
fn generations_agree(slot: &Slot, presented: u32, what: &str) -> Result<(), String> {
	if slot.generation == presented {
		return Ok(());
	}
	Err(format!("{what} presented generation {presented} for a slot standing at {}", slot.generation))
}

#[derive(Default)]
struct Model {
	tables: std::collections::BTreeMap<u16, Table>,
	// PER ENDPOINT, because the model's `queue` is one channel's. A checker with a single queue
	// would hold a peek at one channel to the order of another, and call the result a violation.
	queues: std::collections::BTreeMap<u16, Queue>,
}

#[derive(Default)]
struct Queue {
	pending: VecDeque<u64>,
	held: Option<u64>,
	committed: bool,
	peeked: Option<u64>,
}

// What each fault and rollback class needs at least one of. A trace that never reaches one is a
// trace that proves nothing about it, and the milestone requires the counters rather than a total.
#[derive(Default)]
struct Covers {
	seed: usize,
	take: usize,
	commit_take: usize,
	restore_open: usize,
	restore_closed: usize,
	abandon: usize,
	install: usize,
	install_into_closed: usize,
	terminate: usize,
	return_to_head: usize,
	commit_delivery: usize,
}

fn die(message: String) -> ! {
	eprintln!("trace-check: {message}");
	std::process::exit(1)
}

impl Model {
	fn table(&mut self, id: u16) -> &mut Table {
		self.tables.entry(id).or_default()
	}

	fn queue(&mut self, id: u16) -> &mut Queue {
		self.queues.entry(id).or_default()
	}

	// One event, checked against the guard of the action it names and then applied. The comments
	// name the model action each guard mirrors.
	fn step(&mut self, index: usize, event: Event, covers: &mut Covers) -> Result<(), String> {
		match event.action {
			// `Init`: a capability appearing in a slot by a path outside the transfer protocol -
			// `try_place`, which is what an ordinary "create an object, hand back a handle" syscall
			// uses.
			//
			// THE MODEL'S `Init` IS PER-CAPABILITY, NOT PER-TABLE. `Transfer.tla` follows ONE
			// capability - `TheCap` - from the sender's slot through the queue to the receiver, and
			// `TransferIsLinear` counts the copies of that one. A process creating a second object
			// later is not a step of that behaviour; it is the start of another. So a seed is
			// accepted whenever the SLOT is free to receive one, and what it must not do is
			// overwrite something: a live capability, or a slot a transfer is holding, or a slot in
			// a table that has closed. Those three are the ways a capability could appear where the
			// accounting says none should be, which is what `NoForgery` is about.
			SEED => {
				let table = self.table(event.party);
				if table.closed {
					return Err(String::from("a capability placed into a CLOSED table"));
				}
				let slot = table.slot(event.slot, event.generation);
				if slot.state != SlotState::Free {
					return Err(format!("a capability placed into a slot that is {:?} - it would displace what is there", slot.state));
				}
				let standing = *slot;
				generations_agree(&standing, event.generation, "a placed capability")?;
				let slot = table.slot(event.slot, event.generation);
				slot.state = SlotState::Live;
				covers.seed += 1;
			}
			// `Take(p, i)`: the slot is Live and the capability carries TRANSFER.
			TAKE => {
				let table = self.table(event.party);
				if table.closed {
					return Err(String::from("a take in a closed table"));
				}
				let slot = *table.slot(event.slot, event.generation);
				if slot.state != SlotState::Live {
					return Err(format!("a take from a slot that is {:?}", slot.state));
				}
				generations_agree(&slot, event.generation, "a take")?;
				if event.rights & RIGHT_TRANSFER == 0 {
					return Err(String::from("a take of a capability that does not carry TRANSFER"));
				}
				table.slot(event.slot, event.generation).state = SlotState::Reserved;
				table.outstanding.push(event.slot);
				covers.take += 1;
			}
			// `CommitTake(p)` and `AbandonTake(p)`: the slot is Reserved and this transfer's.
			COMMIT_TAKE | ABANDON_TAKE => {
				let table = self.table(event.party);
				let slot = *table.slot(event.slot, event.generation);
				if slot.state != SlotState::Reserved {
					return Err(String::from("a transfer resolved on a slot that is not reserved"));
				}
				generations_agree(&slot, event.generation, "a resolved transfer")?;
				let held = table.outstanding.iter().position(|slot| *slot == event.slot);
				let Some(position) = held else {
					return Err(String::from("a transfer resolved that was never taken"));
				};
				table.outstanding.remove(position);
				table.recycle(event.slot);
				if event.action == COMMIT_TAKE {
					covers.commit_take += 1;
				} else {
					covers.abandon += 1;
				}
			}
			// `RestoreTake(p)`: back to Live, or dropped when the table has closed.
			RESTORE_TAKE => {
				let table = self.table(event.party);
				let slot = *table.slot(event.slot, event.generation);
				if slot.state != SlotState::Reserved {
					return Err(String::from("a restore into a slot that is not reserved"));
				}
				generations_agree(&slot, event.generation, "a restore")?;
				if let Some(position) = table.outstanding.iter().position(|slot| *slot == event.slot) {
					table.outstanding.remove(position);
				}
				if event.outcome == REFUSED {
					// The closed-table arm: the capability is dropped and the slot recycled.
					if !table.closed {
						return Err(String::from("a restore reported as refused in a table that is not closed"));
					}
					table.recycle(event.slot);
					covers.restore_closed += 1;
				} else {
					if table.closed {
						return Err(String::from("a restore into a CLOSED table produced a live handle"));
					}
					table.slot(event.slot, event.generation).state = SlotState::Live;
					covers.restore_open += 1;
				}
			}
			// `Book(p)` / `Unbook(p)`: the count is slots taken out of circulation.
			BOOK => {
				if event.outcome == OK {
					self.table(event.party).booked += event.slot as usize;
				}
			}
			UNBOOK => {
				let table = self.table(event.party);
				table.booked = table.booked.saturating_sub(event.slot as usize);
			}
			// `Install(p)`: into a slot this booking owns, and the booking is spent.
			INSTALL => {
				let table = self.table(event.party);
				if table.closed {
					return Err(String::from("an install into a closed table produced a handle"));
				}
				if table.booked == 0 {
					return Err(String::from("an install against no booking"));
				}
				table.booked -= 1;
				let slot = table.slot(event.slot, event.generation);
				if slot.state != SlotState::Free {
					return Err(format!("an install into a slot that is {:?}", slot.state));
				}
				let standing = *slot;
				generations_agree(&standing, event.generation, "an install")?;
				table.slot(event.slot, event.generation).state = SlotState::Live;
				covers.install += 1;
			}
			// `InstallIntoClosed(p)`: the barrier `restore_taken` stands behind.
			INSTALL_INTO_CLOSED => {
				let table = self.table(event.party);
				if !table.closed {
					return Err(String::from("an install-into-closed in a table that is open"));
				}
				table.booked = table.booked.saturating_sub(1);
				covers.install_into_closed += 1;
			}
			// `Close(p, i)`: a live slot is recycled under a new generation.
			CLOSE => {
				let table = self.table(event.party);
				let slot = *table.slot(event.slot, event.generation);
				generations_agree(&slot, event.generation, "a close")?;
				if slot.state == SlotState::Live {
					table.recycle(event.slot);
				}
			}
			// `Terminate(p)`: live slots are recycled, RESERVED ones are not this function's.
			TERMINATE => {
				let table = self.table(event.party);
				table.closed = true;
				for slot in table.slots.iter_mut().flatten() {
					if slot.state == SlotState::Live {
						slot.state = SlotState::Free;
						slot.generation = slot.generation.checked_add(1).unwrap_or(u32::MAX);
					}
				}
				covers.terminate += 1;
			}
			// `Enqueue(p)`, `Peek(p)`, `Dequeue(p)`, `ReturnToHead`, `CommitDelivery`.
			ENQUEUE => self.queue(event.party).pending.push_back(event.message),
			PEEK => {
				let queue = self.queue(event.party);
				let Some(front) = queue.pending.front() else {
					return Err(String::from("a peek at an empty queue"));
				};
				if *front != event.message {
					return Err(String::from("a peek that named a message which is not at the head"));
				}
				queue.peeked = Some(event.message);
			}
			DEQUEUE => {
				let queue = self.queue(event.party);
				let Some(front) = queue.pending.front().copied() else {
					return Err(String::from("a dequeue from an empty queue"));
				};
				if front != event.message {
					return Err(String::from("a dequeue of a message that is not at the head"));
				}
				// `MessageIdentityStable`: a receive acts on the message it looked at, or the
				// shape it sized its buffers from belongs to something else.
				if queue.peeked != Some(event.message) {
					return Err(String::from("a dequeue of a message this receiver never inspected"));
				}
				queue.pending.pop_front();
				queue.held = Some(event.message);
				queue.committed = false;
			}
			RETURN_TO_HEAD => {
				let queue = self.queue(event.party);
				if queue.held != Some(event.message) {
					return Err(String::from("a message returned that was not the one in hand"));
				}
				// `PostCommitCopyoutIsTerminal`: past the commit the message cannot go back.
				if queue.committed {
					return Err(String::from("a message returned to the queue AFTER its delivery committed"));
				}
				queue.pending.push_front(event.message);
				queue.held = None;
				covers.return_to_head += 1;
			}
			COMMIT_DELIVERY => {
				let queue = self.queue(event.party);
				if queue.held != Some(event.message) {
					return Err(String::from("a delivery committed for a message that is not in hand"));
				}
				queue.committed = true;
				covers.commit_delivery += 1;
			}
			other => return Err(format!("event {index} names action {other}, which this checker does not know")),
		}
		Ok(())
	}
}

// Read the traces out of whatever the guest printed around them.
//
// ONE LOG CARRIES SEVERAL RUNS: the hand-written conformance fixture, and a seeded schedule per seed.
// Each is bounded by its own begin/end markers and each is an INDEPENDENT model run - replaying them
// as one sequence would hold a later run's first step to the state a previous one ended in.
fn runs_of(path: &str) -> Vec<Vec<Event>> {
	let text = std::fs::read_to_string(path).unwrap_or_else(|e| die(format!("could not read {path}: {e}")));

	let mut runs: Vec<Vec<Event>> = Vec::new();
	let mut events: Vec<Event> = Vec::new();
	let mut open = false;
	for line in text.lines() {
		let Some(rest) = line.split("captrace:").nth(1) else { continue };
		let rest = rest.trim();
		if let Some(count) = rest.strip_prefix("begin ") {
			if open {
				die(String::from("a trace begins inside another one - the log is interleaved and no run in it can be replayed"));
			}
			open = true;
			let _ = count;
			continue;
		}
		if rest == "end" {
			// A TRUNCATED RUN IS NOT A SHORT ONE. Without both markers this is a log that was cut,
			// and a checker that accepted it would report a clean result over the part that arrived.
			if !open {
				die(String::from("a trace ends without having begun - the log is truncated"));
			}
			open = false;
			runs.push(core::mem::take(&mut events));
			continue;
		}
		if !open {
			continue;
		}
		let fields: Vec<u64> = rest.split_whitespace().filter_map(|f| f.parse().ok()).collect();
		if fields.len() != 7 {
			die(format!("a trace line has {} fields and an event has 7: {rest}", fields.len()));
		}
		events.push(Event { action: fields[0] as u8, outcome: fields[1] as u8, party: fields[2] as u16, slot: fields[3] as u16, generation: fields[4] as u32, rights: fields[5] as u32, message: fields[6] });
	}

	if open {
		die(String::from("the log ends in the middle of a trace"));
	}
	if runs.is_empty() {
		die(String::from("the log contains no trace at all"));
	}
	if let Some(at) = runs.iter().position(Vec::is_empty) {
		die(format!("run {at} is empty - a fixture that emitted nothing is not a pass"));
	}
	runs
}

// NORMALIZE. A run's message identities and channel identities come from counters that have been
// running since boot, so two recordings of the same schedule differ in every one of them while
// describing the same behaviour. Renumbering both by order of first appearance is what makes a trace
// something a repository can hold a later one against.
fn normalize(events: &[Event]) -> Vec<Event> {
	let mut messages: Vec<u64> = Vec::new();
	let mut parties: Vec<u16> = Vec::new();
	let mut out = Vec::with_capacity(events.len());
	for event in events {
		let mut event = *event;
		if event.message != 0 {
			let at = messages.iter().position(|m| *m == event.message).unwrap_or_else(|| {
				messages.push(event.message);
				messages.len() - 1
			});
			event.message = at as u64 + 1;
		}
		if event.party & CHANNEL_PARTY != 0 {
			let at = parties.iter().position(|p| *p == event.party).unwrap_or_else(|| {
				parties.push(event.party);
				parties.len() - 1
			});
			event.party = CHANNEL_PARTY | at as u16;
		}
		out.push(event);
	}
	out
}

fn print_trace(events: &[Event]) {
	println!("captrace: begin {}", events.len());
	for e in events {
		println!("captrace: {} {} {} {} {} {} {}", e.action, e.outcome, e.party, e.slot, e.generation, e.rights, e.message);
	}
	println!("captrace: end");
}

// The covers of a whole log: each run is replayed on its own, and the classes they reached are
// added up. A class reached by ANY run is reached - which is the whole point of driving several.
fn replay_all(runs: &[Vec<Event>]) -> Result<Covers, String> {
	let mut total = Covers::default();
	for (index, events) in runs.iter().enumerate() {
		let covers = replay(events).map_err(|reason| format!("run {index}: {reason}"))?;
		total.seed += covers.seed;
		total.take += covers.take;
		total.commit_take += covers.commit_take;
		total.restore_open += covers.restore_open;
		total.restore_closed += covers.restore_closed;
		total.abandon += covers.abandon;
		total.install += covers.install;
		total.install_into_closed += covers.install_into_closed;
		total.terminate += covers.terminate;
		total.return_to_head += covers.return_to_head;
		total.commit_delivery += covers.commit_delivery;
	}
	Ok(total)
}

// Replay, reporting the first step the model does not allow.
fn replay(events: &[Event]) -> Result<Covers, String> {
	let mut model = Model::default();
	let mut covers = Covers::default();
	for (index, event) in events.iter().enumerate() {
		if let Err(reason) = model.step(index, *event, &mut covers) {
			return Err(format!("event {index} is not an enabled model step: {reason}"));
		}
	}
	Ok(covers)
}

// The classes a trace must have reached for its clean result to mean anything.
fn missing_covers(covers: &Covers) -> Vec<&'static str> {
	let required: [(&str, usize); 11] = [
		("a starting capability", covers.seed),
		("take", covers.take),
		("commit-take", covers.commit_take),
		("restore into an open table", covers.restore_open),
		("restore into a CLOSED table", covers.restore_closed),
		("abandon", covers.abandon),
		("install", covers.install),
		("install into a CLOSED table", covers.install_into_closed),
		("terminate", covers.terminate),
		("return to head (a receive that failed BEFORE its commit)", covers.return_to_head),
		("commit delivery", covers.commit_delivery),
	];
	required.iter().filter(|(_, count)| *count == 0).map(|(name, _)| *name).collect()
}

fn report(runs: usize, steps: usize, covers: &Covers) {
	println!("trace-check: {runs} run(s), {steps} step(s) replayed, every one an enabled model step");
	println!("trace-check:   a starting capability: {}", covers.seed);
	println!("trace-check:   take: {}", covers.take);
	println!("trace-check:   commit-take: {}", covers.commit_take);
	println!("trace-check:   restore into an open table: {}", covers.restore_open);
	println!("trace-check:   restore into a CLOSED table: {}", covers.restore_closed);
	println!("trace-check:   abandon: {}", covers.abandon);
	println!("trace-check:   install: {}", covers.install);
	println!("trace-check:   install into a CLOSED table: {}", covers.install_into_closed);
	println!("trace-check:   terminate: {}", covers.terminate);
	println!("trace-check:   return to head: {}", covers.return_to_head);
	println!("trace-check:   commit delivery: {}", covers.commit_delivery);
}

fn main() {
	let arguments: Vec<String> = std::env::args().skip(1).collect();
	match arguments.first().map(String::as_str) {
		Some("--normalize") => {
			let Some(path) = arguments.get(1) else { die(String::from("--normalize takes a log")) };
			for events in runs_of(path) {
				print_trace(&normalize(&events));
			}
			return;
		}
		Some("--self-test") => {
			let Some(path) = arguments.get(1) else { die(String::from("--self-test takes a reference trace")) };
			let runs = runs_of(path);
			self_test(&runs[0]);
			return;
		}
		Some(flag) if flag.starts_with("--") => die(format!("{flag} is not a mode this knows")),
		None => die(String::from("the guest log is the first argument")),
		_ => {}
	}
	let runs = runs_of(&arguments[0]);
	let steps: usize = runs.iter().map(Vec::len).sum();

	let covers = replay_all(&runs).unwrap_or_else(|reason| die(reason));

	// EVERY FAULT AND ROLLBACK CLASS AT LEAST ONCE. A run that never reached one proves nothing
	// about it, and an all-green result over a trace that only ever succeeded is the shape of
	// evidence this milestone refuses.
	let missing = missing_covers(&covers);
	if !missing.is_empty() {
		die(format!("{} step(s) the trace never reached: {}", missing.len(), missing.join(", ")));
	}
	report(runs.len(), steps, &covers);
}

// A CHECKER THAT HAS NEVER REFUSED ANYTHING IS A CHECKER NOBODY HAS TESTED, and one that accepts
// every trace passes every run it will ever see. So the reference trace is broken on purpose, one
// rule at a time, and each mutation must be refused for the reason it was made.
//
// The mutations are on a COPY of the parsed trace. Nothing here writes the reference.
fn self_test(reference: &[Event]) {
	let covers = match replay(reference) {
		Ok(covers) => covers,
		Err(reason) => die(format!("the REFERENCE trace does not replay: {reason}")),
	};
	let missing = missing_covers(&covers);
	if !missing.is_empty() {
		die(format!("the reference trace never reaches: {}", missing.join(", ")));
	}

	// Find the nth event of an action, so a mutation names what it breaks rather than a line number.
	let nth = |events: &[Event], action: u8, n: usize| -> usize { events.iter().enumerate().filter(|(_, e)| e.action == action).map(|(i, _)| i).nth(n).unwrap_or_else(|| die(format!("the reference trace has no action {action} number {n} - this mutation no longer tests anything"))) };

	type Mutation = (&'static str, &'static str, fn(&[Event], &dyn Fn(&[Event], u8, usize) -> usize) -> Vec<Event>);
	let mutations: &[Mutation] = &[
		("a take of a capability without TRANSFER", "does not carry TRANSFER", |events, nth| {
			let mut out = events.to_vec();
			let at = nth(events, TAKE, 0);
			out[at].rights &= !RIGHT_TRANSFER;
			out
		}),
		("a take presenting a stale generation", "presented generation", |events, nth| {
			let mut out = events.to_vec();
			let at = nth(events, TAKE, 0);
			out[at].generation += 1;
			out
		}),
		("a take of a slot that holds nothing", "a take from a slot that is Free", |events, nth| {
			let mut out = events.to_vec();
			let at = nth(events, SEED, 0);
			out.remove(at);
			out
		}),
		("a transfer resolved that was never taken", "not reserved", |events, nth| {
			let mut out = events.to_vec();
			let at = nth(events, TAKE, 0);
			out.remove(at);
			out
		}),
		("an install against no booking", "an install against no booking", |events, nth| {
			let mut out = events.to_vec();
			let at = nth(events, BOOK, 0);
			out.remove(at);
			out
		}),
		("a refused restore in a table that never closed", "in a table that is not closed", |events, nth| {
			let mut out = events.to_vec();
			let at = nth(events, RESTORE_TAKE, 0);
			out[at].outcome = REFUSED;
			out
		}),
		("an install-into-closed in a table that never closed", "in a table that is open", |events, nth| {
			let mut out = events.to_vec();
			let at = nth(events, INSTALL_INTO_CLOSED, 0);
			let party = out[at].party;
			out.retain(|e| !(e.action == TERMINATE && e.party == party));
			out
		}),
		("a dequeue of a message nobody inspected", "never inspected", |events, nth| {
			let mut out = events.to_vec();
			let at = nth(events, PEEK, 0);
			out.remove(at);
			out
		}),
		("a message returned AFTER its delivery committed", "AFTER its delivery committed", |events, nth| {
			let mut out = events.to_vec();
			// Take the return-to-head and give the message a commit before it.
			let at = nth(events, RETURN_TO_HEAD, 0);
			let mut commit = out[at];
			commit.action = COMMIT_DELIVERY;
			out.insert(at, commit);
			out
		}),
		("a capability placed into a slot a transfer is holding", "it would displace what is there", |events, nth| {
			let mut out = events.to_vec();
			let seed = out[nth(events, SEED, 0)];
			// After the take that reserved the slot, and naming that same slot: the transfer's
			// capability would be displaced by one that appeared from nowhere.
			let after = nth(events, TAKE, 0);
			let mut forged = seed;
			forged.slot = out[after].slot;
			forged.party = out[after].party;
			forged.generation = out[after].generation;
			out.insert(after + 1, forged);
			out
		}),
		("a capability placed into a table that has closed", "into a CLOSED table", |events, nth| {
			let mut out = events.to_vec();
			let at = nth(events, TERMINATE, 0);
			let mut forged = out[nth(events, SEED, 0)];
			forged.party = out[at].party;
			forged.slot = 9;
			forged.generation = 1;
			out.insert(at + 1, forged);
			out
		}),
		("a trace that never reaches a rollback", "never reached", |events, nth| {
			let mut out = events.to_vec();
			let at = nth(events, ABANDON_TAKE, 0);
			out.remove(at);
			// And the take it resolved, so the only thing wrong is the class that is now absent.
			let take = out.iter().position(|e| e.action == TAKE && e.party == events[at].party).unwrap_or(0);
			out.remove(take);
			out
		}),
	];

	let mut refused = 0;
	for (name, expected, mutate) in mutations {
		let mutated = mutate(reference, &nth);
		let verdict = match replay(&mutated) {
			Err(reason) => reason,
			Ok(covers) => {
				let missing = missing_covers(&covers);
				if missing.is_empty() {
					die(format!("the checker ACCEPTED \"{name}\" - it does not test what it claims to"));
				}
				format!("{} step(s) the trace never reached: {}", missing.len(), missing.join(", "))
			}
		};
		if !verdict.contains(expected) {
			die(format!("\"{name}\" was refused, but for the wrong reason:\n  wanted: {expected}\n  got:    {verdict}"));
		}
		println!("trace-check: refused {name}");
		refused += 1;
	}
	println!("trace-check: the reference replays and {refused} deliberate defects are refused");
}
