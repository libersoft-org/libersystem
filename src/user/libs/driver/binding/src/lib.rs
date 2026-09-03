#![no_std]

// WHERE A BINDING IS, AND WHY IT GOT THERE.
//
// A state and a reason are not alternatives - "quarantined" and "the teardown did not confirm" are
// two halves of one answer - so the record carries both, and the transitions between states are a
// TABLE rather than whatever each call site decided. A graph with a hole in it is one that gets
// closed by whoever hits the hole first.
//
// DEFINED HERE rather than where it is finally rendered, because five milestones would otherwise
// each invent their own numbers and strings for the same thing.

use driver_protocol::DriverFailureCode;

// The states a device node's binding can be in.
//
// THERE IS NO `Removed`. Nothing rescans, so no removal event exists to enter it, and a state
// nothing can produce is a state every reader has to reason about for nothing.
//
// BOTH OF THE LATER TWO HAVE ARRIVED, each with the milestone that first PRODUCED it:
// `DependencyPending` when a declared requirement first had something to wait for, and `Disabled`
// when an operator first had a way to set it. One enum, in one file, which is what stopped five
// milestones each inventing their own numbers and strings for the same thing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BindingState {
	// Where a node starts, and what invites a bind. A FAILED bind must never land here: it would be
	// a bind that immediately happens again with nothing recorded about why the last one did not
	// work.
	Unbound,
	// A declared requirement is not published yet. A STATE, NOT A FAILURE: a machine without a NIC
	// is a machine, not a broken image, and a node here is waiting rather than broken.
	//
	// There is no edge back to `Unbound`. A node waiting for a provider that then goes away is
	// waiting HARDER, not waiting less - and `Unbound` invites an immediate bind that would fail for
	// the reason the node is already recording.
	DependencyPending,
	// An operator turned it off. NOT `Unbound`, which is where a node STARTS and what invites a
	// rebind - a disabled device that read as unbound would be bound again by the next thing that
	// looked at it, which is the opposite of what was asked for.
	Disabled,
	// The transaction is open: a driver has been selected and resources are being assembled.
	Binding,
	// The driver said `READY` with the current generation.
	Online,
	// There is a device to quieten. The only way out is through the teardown.
	Stopping,
	// Waiting to try again, with attempts left.
	Backoff,
	// Not bound, and not going to be without something changing. Carries the cause.
	Failed,
	// The teardown could not be CONFIRMED. Terminal for the boot: the device's frames, vectors and
	// grants stay charged and out of circulation, because the alternative is handing back memory a
	// device may still be writing to.
	Quarantined,
}

impl BindingState {
	// A name for the log and for whatever renders this later. Deliberately not `Debug`: these
	// strings are read by people and one of them is a boot line.
	pub fn name(self) -> &'static [u8] {
		match self {
			BindingState::Unbound => b"unbound",
			BindingState::DependencyPending => b"waiting for a dependency",
			BindingState::Disabled => b"disabled by an operator",
			BindingState::Binding => b"binding",
			BindingState::Online => b"online",
			BindingState::Stopping => b"stopping",
			BindingState::Backoff => b"backoff",
			BindingState::Failed => b"failed",
			BindingState::Quarantined => b"quarantined",
		}
	}

	// THE TABLE, ENFORCED RATHER THAN IMPLIED. Anything not here is refused and logged as a
	// transition that was ATTEMPTED, because a state machine that silently ignores an illegal event
	// is one nobody can debug.
	//
	// | from | to | on |
	// | `Unbound` | `Failed` | nothing could be attempted: no artifact on the volume, or one that does not speak this protocol |
	// | `Unbound` | `Binding` | a driver is selected and the transaction opens |
	// | `Binding` | `Online` | `READY` arrives with the current generation |
	// | `Binding` | `Backoff` | the transaction failed BEFORE the claim was taken, attempts left |
	// | `Binding` | `Failed` | the same, for a reason retrying cannot change OR the budget is spent |
	// | `Binding` | `Stopping` | the transaction failed AFTER the claim was taken |
	// | `Online` | `Stopping` | the driver exited |
	// | `Stopping` | `Backoff` | teardown confirmed, attempts left |
	// | `Stopping` | `Failed` | teardown confirmed, and retrying cannot help OR the budget is spent |
	// | `Stopping` | `Quarantined` | teardown NOT confirmed - the device may still be live |
	// | `Backoff` | `Binding` | the delay expired |
	// | `Backoff` | `Failed` | the absolute TIME budget expired |
	// | `Failed` | `Binding` | an operator retry |
	// | `Quarantined` | - | terminal for the boot |
	// Whether a TERMINAL HANDSHAKE FRAME - `READY` or `FAILED` - may still be acted on.
	//
	// The handshake ends in exactly one of the two, and a second is refused. That rule lived at one
	// of its two call sites and only by accident: `READY` was refused because `Online -> Online` is
	// not an edge in the table above, while `FAILED` computed a failure cause and tore the binding
	// down from wherever it was - so `READY` followed by `FAILED` on one generation dismantled a
	// binding that had come up, which is the opposite of refusing a second terminal frame.
	//
	// `Binding` is the whole of it because `Binding -> Online` is the only edge into `Online`: a
	// handshake that can still end is one that has not ended. A driver that dies AFTER coming up is
	// a different fact with its own events - the channel closing, the watchdog going unanswered, the
	// process exiting - and none of them is a frame the driver sent about its handshake.
	pub fn accepts_terminal_frame(self) -> bool {
		matches!(self, BindingState::Binding)
	}

	pub fn may_move_to(self, to: BindingState) -> bool {
		matches!(
			(self, to),
			(BindingState::Unbound, BindingState::Binding)
				// A FAILURE THAT HAPPENS BEFORE ANY ATTEMPT IS STILL THIS NODE'S FAILURE.
				//
				// The table had `Failed` reachable only from `Binding`, `Stopping` and `Backoff` -
				// from a driver that RAN - and two permanent pre-bind failures are neither: every
				// candidate artifact missing from the volume, and an artifact that does not declare
				// this build's driver protocol, which is refused BEFORE the claim precisely so a
				// device is never handed to it. Both were recorded by attempting this transition and
				// discarding the refusal, so the served record stayed `Unbound` with no cause and an
				// operator could not tell a packaging fault from a device nothing had tried.
				//
				// From `DependencyPending` too: a node that has just learnt its requirements are
				// satisfied is still in that state when the artifact is read and its protocol note
				// checked.
				| (BindingState::Unbound, BindingState::Failed)
				| (BindingState::DependencyPending, BindingState::Failed)
				| (BindingState::Binding, BindingState::Online)
				| (BindingState::Binding, BindingState::Backoff)
				| (BindingState::Binding, BindingState::Failed)
				| (BindingState::Binding, BindingState::Stopping)
				| (BindingState::Online, BindingState::Stopping)
				| (BindingState::Stopping, BindingState::Backoff)
				| (BindingState::Stopping, BindingState::Failed)
				| (BindingState::Stopping, BindingState::Quarantined)
				// A BIND THAT DISCOVERS THE DEVICE IS ALREADY QUARANTINED ADOPTS THAT, rather than
				// inventing a failure of its own.
				//
				// `observe_claim` reads the kernel's claim snapshot AFTER the node has entered
				// `Binding`, and `CLAIM_STATE_QUARANTINED` is a terminal fact about the DEVICE - some
				// earlier holder's teardown was never confirmed and nothing will claim it again this
				// boot. Without this edge the move was refused in silence and the node then reported
				// `Failed`/`teardown-unconfirmed`, which says this attempt tore something down badly.
				// It did not: it took no claim at all. The state a reader needs is the device's.
				| (BindingState::Binding, BindingState::Quarantined)
				| (BindingState::Backoff, BindingState::Binding)
				| (BindingState::Backoff, BindingState::Failed)
				| (BindingState::Failed, BindingState::Binding)
				// The dependency edges. A node enters this state from wherever it learns a declared
				// requirement is missing, and leaves it only for a bind - when EVERY requirement is
				// published, not the first, which would start a bind that fails on the second.
				| (BindingState::Unbound, BindingState::DependencyPending)
				| (BindingState::DependencyPending, BindingState::Binding)
				| (BindingState::Binding, BindingState::DependencyPending)
				| (BindingState::Backoff, BindingState::DependencyPending)
				| (BindingState::Stopping, BindingState::DependencyPending)
				| (BindingState::Failed, BindingState::DependencyPending)
				// P02M0166's. A disable on a binding that is RUNNING goes through the teardown like
				// any other stop, carrying the stop intent so it does not rebind; one on a binding
				// that is not running has nothing to tear down and lands directly.
				| (BindingState::Binding, BindingState::Disabled)
				| (BindingState::Stopping, BindingState::Disabled)
				| (BindingState::Unbound, BindingState::Disabled)
				| (BindingState::Backoff, BindingState::Disabled)
				| (BindingState::Failed, BindingState::Disabled)
				| (BindingState::DependencyPending, BindingState::Disabled)
				// And back. `enable` is the only way out, and it goes to `Unbound` because that is
				// what invites the bind an enable is asking for.
				| (BindingState::Disabled, BindingState::Unbound)
		)
	}
}

// WHY A NODE IS IN `Stopping`, which is what decides where it goes next.
//
// P02M0162's table sends a CONFIRMED teardown on to `Backoff` and then back to `Binding` - which is
// right for a driver that died and exactly wrong for one that was asked to stop: the operator stops
// it and it starts again. The intent is what `Stopping` resolves against, and there are four.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum StopIntent {
	// It crashed, exited, or stopped answering. The ordinary path, and the default.
	#[default]
	Fault,
	// A provider it declared in `requires` went away.
	DependencyLost,
	// An operator disabled it. Where it lands is P02M0166's `Disabled`.
	OperatorDisable,
	// The machine is going down. No further state is entered at all - the manager is going away, so
	// there is no next binding to describe.
	Shutdown,
}

impl StopIntent {
	// Where a CONFIRMED teardown under this intent lands, or None for one that describes no next
	// state because nothing will read it.
	//
	// AN UNCONFIRMED TEARDOWN IS NOT HERE, and that is the point: it ends at `Quarantined` whatever
	// the intent was, because what is unknown is whether the device is still live and no intent
	// changes that.
	pub fn confirmed_lands_at(self, attempts_left: bool) -> Option<BindingState> {
		match self {
			StopIntent::Fault => Some(if attempts_left { BindingState::Backoff } else { BindingState::Failed }),
			StopIntent::DependencyLost => Some(BindingState::DependencyPending),
			StopIntent::OperatorDisable => Some(BindingState::Disabled),
			StopIntent::Shutdown => None,
		}
	}

	pub fn name(self) -> &'static [u8] {
		match self {
			StopIntent::Fault => b"a fault",
			StopIntent::DependencyLost => b"a dependency it declared went away",
			StopIntent::OperatorDisable => b"an operator disabled it",
			StopIntent::Shutdown => b"the machine is going down",
		}
	}
}

// WHAT AN OPERATOR'S DISABLE DOES TO A NODE.
//
// The state alone cannot answer it, which is why this takes two more facts. `Binding` spans both
// sides of the claim - before it, the transaction holds nothing and the record can simply say
// `Disabled`; after it, there is a live process and a claimed device, and relabelling the record
// would leave both attached to a node reported disabled. And a teardown that is ALREADY under way is
// the one that completes: a second disable replaces where it lands rather than starting another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DisableAction {
	// A teardown is in flight. Replace its intent and its landing; start nothing.
	RelandTheTeardown,
	// The binding holds the device. It goes through the teardown like any other stop.
	StopTheBinding,
	// Nothing is held, so there is nothing to give back and the record moves straight there.
	RecordItDirectly,
}

pub fn disable_action(state: BindingState, holds_the_device: bool, teardown_in_flight: bool) -> DisableAction {
	if teardown_in_flight || state == BindingState::Stopping {
		return DisableAction::RelandTheTeardown;
	}
	match state {
		BindingState::Online => DisableAction::StopTheBinding,
		// The half of `Binding` the table's two rows are about: `Binding -> Disabled` before the
		// claim, `Binding -> Stopping` after it.
		BindingState::Binding if holds_the_device => DisableAction::StopTheBinding,
		_ => DisableAction::RecordItDirectly,
	}
}

// WHAT AN ATTEMPT BUDGET IS AFTER SOMETHING THAT DID NOT SPEND ONE.
//
// A candidate whose artifact was missing, and a node parked on a requirement that has since arrived,
// both leave the node without having run anything - so the automatic budget starts again. An
// OPERATOR'S single granted attempt is the exception and it is the whole reason this is a function:
// that grant is expressed as `attempt = MAX - 1` with a flag beside it, so resetting the counter
// hands the operator the entire automatic budget they were deliberately not given. The flag is spent
// where an attempt actually ends, not here.
pub fn budget_after_nothing_ran(retry_once: bool, attempt: u32) -> u32 {
	if retry_once { attempt } else { 0 }
}

// WHY A BINDING IS NOT UP.
//
// EVERY VARIANT ANSWERS WHETHER IT IS RETRYABLE, in one place. A flag some variants leave unset is a
// flag each call site guesses at, and the table's `Binding -> Backoff` versus `Binding -> Failed`
// reads this rather than deciding again.
//
// The driver-reported one CARRIES the driver's code rather than flattening it away: "the driver said
// it failed" without saying what it said is a cause that explains nothing - and the retryability
// comes from the driver's own closed set, which is the only party that can honestly answer it about
// itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FailureCause {
	// The registry names a driver the image does not contain.
	DriverMissing,
	// Its ELF note declares a protocol version this build does not implement.
	ProtocolMismatch,
	// Somebody else holds the device. NOT retryable: waiting changes nothing this milestone
	// controls.
	ClaimRefused,
	// The driver's DMA policy demands translation on a machine that is not enforcing it.
	IommuRequired,
	// A quota or an allocation refused.
	ResourceExhausted,
	// The process could not be started. RETRYABLE: usually a transient shortage.
	SpawnFailed,
	// The driver was sent `BIND` and never reached a terminal frame inside the window.
	HandshakeTimeout,
	// It exited without saying anything.
	DriverExited,
	// IT WAS ASKED TO STOP AND IT DID - which is not a failure, and is here because the teardown path
	// carries a cause and a planned stop needs one that is TRUE (added 2026-09-01).
	//
	// The planned stop used to travel as `DriverExited`, with a comment saying the cause "only
	// travels so the shared teardown path has one to carry". It travelled further than that: the
	// shared path captures an incident, reports it and PERSISTS it, and `DriverExited` renders as
	// "it exited without saying anything" - about a driver that had said exactly what it was asked
	// to say. M3 requires a planned stop not to be classified as a crash, and an operator reading the
	// stored incident row saw one.
	//
	// NOT RETRYABLE, and that is the point rather than an omission: a driver that stopped because it
	// was told to is not a driver to bring back automatically. What brings it back is the operator
	// verb or the dependency that returns, both of which ask for a bind on their own.
	Stopped,
	// It came up and then stopped answering its control path. RETRYABLE: nothing about a driver
	// going quiet says the device is unusable.
	//
	// A DIFFERENT CAUSE FROM `handshake-timeout`, which is a driver that never answered AT ALL. The
	// two were one cause until this milestone had to render them, and a reader cannot act on "it did
	// not answer" without knowing whether it had ever been up.
	Hung,
	// It said `FAILED`, and this is what it said.
	DriverReported(DriverFailureCode),
	// The release did not confirm. NOT IN EITHER COLUMN: it never reaches the question, because it
	// ends at `Quarantined`.
	TeardownUnconfirmed,
}

impl FailureCause {
	pub fn retryable(self) -> bool {
		match self {
			FailureCause::HandshakeTimeout | FailureCause::DriverExited | FailureCause::SpawnFailed | FailureCause::Hung => true,
			FailureCause::DriverReported(code) => code.retryable(),
			FailureCause::DriverMissing | FailureCause::ProtocolMismatch | FailureCause::ClaimRefused | FailureCause::IommuRequired | FailureCause::ResourceExhausted => false,
			// A stop that was ASKED FOR is not a thing to retry. See the variant.
			FailureCause::Stopped => false,
			// Asked and answered `false` so the match is total, but the state machine never gets
			// here: this cause ends at `Quarantined`, which has no edge out.
			FailureCause::TeardownUnconfirmed => false,
		}
	}

	pub fn name(self) -> &'static [u8] {
		match self {
			FailureCause::DriverMissing => b"driver-missing",
			FailureCause::ProtocolMismatch => b"protocol-mismatch",
			FailureCause::ClaimRefused => b"claim-refused",
			FailureCause::IommuRequired => b"iommu-required",
			FailureCause::ResourceExhausted => b"resource-exhausted",
			FailureCause::SpawnFailed => b"spawn-failed",
			FailureCause::HandshakeTimeout => b"handshake-timeout",
			FailureCause::DriverExited => b"driver-exited",
			FailureCause::DriverReported(_) => b"driver-reported-failure",
			FailureCause::Hung => b"hung",
			FailureCause::TeardownUnconfirmed => b"teardown-unconfirmed",
			FailureCause::Stopped => b"stopped",
		}
	}
}

// One device node's binding record: where it is, why, and which binding this is.
pub struct BindingRecord {
	pub state: BindingState,
	pub failure: Option<FailureCause>,
	// P02M0098's claim generation for the CURRENT binding, or 0 for a node that has never been
	// claimed. Every event carries the generation it is about, and one that does not match is
	// dropped - which is what stops a dying driver from touching its replacement's state.
	pub generation: u64,
	// Automatic attempts spent on THIS incident. An operator retry is not one of them.
	pub attempts: u32,
}

impl BindingRecord {
	pub fn new() -> Self {
		Self { state: BindingState::Unbound, failure: None, generation: 0, attempts: 0 }
	}

	// Move, if the table allows it. Answers false for an illegal transition, which the caller LOGS -
	// refusing quietly would be the same defect as ignoring it.
	//
	// A DUPLICATE EVENT CHANGES NOTHING, and that falls out of the table rather than needing a rule:
	// no state has an edge to itself, so a second arrival of the event that already moved the node
	// is refused like any other illegal transition.
	pub fn move_to(&mut self, to: BindingState, failure: Option<FailureCause>) -> bool {
		if !self.state.may_move_to(to) {
			return false;
		}
		self.state = to;
		// The cause is kept while it explains the state and cleared when it stops doing so: a node
		// that is `Online` carrying the reason its LAST attempt failed reads as a node that is
		// broken and running.
		self.failure = match to {
			BindingState::Online | BindingState::Binding => None,
			_ => failure.or(self.failure),
		};
		true
	}
}

#[cfg(test)]
mod tests;

// ------------------------------------------------- one ordered queue per device node

// How many events one node may hold before a new one is refused.
//
// A node's events are the frames of one handshake plus its terminal ones, so the bound is the
// protocol's own: at most `MAX_INITIAL_OFFERS` offers, one terminal frame, one exit, one claim
// answer, and a timeout - and a driver that sends more is a driver whose offers are already being
// refused past the bound.
pub const MAX_NODE_EVENTS: usize = driver_protocol::MAX_INITIAL_OFFERS + 4;

// WHAT HAPPENED TO A BINDING, carrying the generation it is about.
//
// The generation is ON the event rather than checked where the frame arrived, because an event may
// be read long after the frame that produced it: the exit of a driver arrives while its binding is
// being torn down, and the next binding's `READY` arrives on the same device afterwards.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BindingEvent {
	// The driver said it is up.
	Ready { generation: u64 },
	// It said it failed, and this is what it said.
	Failed { generation: u64, code: DriverFailureCode },
	// It offered a provider. The handle itself is held elsewhere, unpublished until the bind commits.
	Offered { generation: u64 },
	// Its process ended - however it ended.
	Exited { generation: u64 },
	// Its channel closed with nothing terminal on it.
	Closed { generation: u64 },
	// This attempt spent its share of the boot window.
	TimedOut { generation: u64 },
	// A provider this driver published under `token` is going away. NOT terminal: the driver stays
	// bound and its other publications stay published.
	Withdrawn { generation: u64, token: u16 },
	// A CONSUMER of the provider published under `token` has gone. Not a withdrawal and not
	// terminal: the provider stays published, and what is released is one place against the
	// `consumers` bound its registry entry declares. Without this the count only ever rose, so a
	// provider admitting one consumer was spent by its first client leaving.
	Disconnected { generation: u64, token: u16 },
	// It answered a `PING` with this sequence. Whether that COUNTS is not this event's business:
	// an answer echoing a number nobody asked with is still an answer that arrived, and the reader
	// is what decides it does not reset the watchdog.
	Ponged { generation: u64, sequence: u32 },
	// Its control path stopped answering inside the deadline its registry entry declared.
	Wedged { generation: u64 },
	// It answered a `STOP`: everything it accepted is finished or abandoned and its device is quiet.
	// A PLANNED stop completing, which is a different fact from a channel that simply closed.
	Stopped { generation: u64 },
	// THE CLAIM REACHED A TERMINAL STATE, which is the OTHER half of a teardown.
	//
	// A rollback used to be one call that killed the process, released the device and answered where
	// the node had landed - so `Stopping` was a label the record passed through rather than a state
	// the node was ever IN, the teardown deadline had nothing to apply to, and the manager was inside
	// the release syscall for its whole duration while every other node waited behind it. M4 says the
	// exit and the claim reaching `Free` ARRIVE, separately, on this node's queue: `state` is one of
	// `abi::CLAIM_STATE_*`, and anything that is not `Free` is a device that is not back.
	ClaimSettled { generation: u64, state: u32 },
}

impl BindingEvent {
	pub fn generation(self) -> u64 {
		match self {
			BindingEvent::Ready { generation } | BindingEvent::Failed { generation, .. } | BindingEvent::Offered { generation } | BindingEvent::Exited { generation } | BindingEvent::Closed { generation } | BindingEvent::TimedOut { generation } | BindingEvent::Withdrawn { generation, .. } | BindingEvent::Disconnected { generation, .. } | BindingEvent::Ponged { generation, .. } | BindingEvent::Wedged { generation } | BindingEvent::Stopped { generation } | BindingEvent::ClaimSettled { generation, .. } => generation,
		}
	}
}

// A DEVICE NODE'S QUEUE, NOT A BINDING'S.
//
// A binding is not the thing that outlives its own events. A queue owned by a binding has nowhere
// to put the events on either side of it, and two consecutive bindings on one device would each
// hold a queue with the interesting moment falling between them.
//
// HERE RATHER THAN IN THE MANAGER, for the reason `BindingState` is here: a ring buffer with a
// generation filter inside a `no_std` binary is one nobody can drive on a host, and "an exit racing
// a restart" is exactly the case that has to be driven rather than reasoned about.
pub struct BindingQueue {
	events: [Option<BindingEvent>; MAX_NODE_EVENTS],
	head: usize,
	count: usize,
}

impl Default for BindingQueue {
	fn default() -> Self {
		Self::new()
	}
}

impl BindingQueue {
	pub const fn new() -> Self {
		Self { events: [None; MAX_NODE_EVENTS], head: 0, count: 0 }
	}

	pub fn len(&self) -> usize {
		self.count
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	// Append one event. A full queue REFUSES the new one rather than dropping the oldest: the oldest
	// is the one whose ordering matters - an exit before a `READY` is a different story from an exit
	// after it - and a queue that silently forgets its front is a queue that reorders.
	pub fn push(&mut self, event: BindingEvent) -> bool {
		if self.count == MAX_NODE_EVENTS {
			return false;
		}
		let at = (self.head + self.count) % MAX_NODE_EVENTS;
		self.events[at] = Some(event);
		self.count += 1;
		true
	}

	// Take the oldest event that is about `generation`, dropping anything that is not.
	//
	// `generation` of 0 means the node holds no binding, and then nothing in the queue can be about
	// anything - so the queue drains and answers None. That is the state a node is in between a
	// teardown and the next attempt, and it is where a dying driver's last events land.
	//
	// STALE EVENTS ARE DROPPED HERE AND NOWHERE ELSE, so every reader gets the same answer to "is
	// this about the binding I am holding".
	pub fn pop(&mut self, generation: u64) -> Option<BindingEvent> {
		while self.count > 0 {
			let event = self.events[self.head].take();
			self.head = (self.head + 1) % MAX_NODE_EVENTS;
			self.count -= 1;
			let event = event?;
			if generation == 0 || event.generation() != generation {
				continue;
			}
			return Some(event);
		}
		None
	}
}

// ------------------------------------------------------- what a binding IS about

// A FUNCTION'S CANONICAL IDENTITY, paired with the generation of the binding it names.
//
// The BDF is the identity: the ABI has no PCI domain and all three implementations are
// single-segment, so bus/device/function names a function on this machine and nothing else does.
//
// THE ROW NUMBER IS NOT PART OF IT. P02M0098's `ClaimKey { device_index, generation }` addresses the
// same binding by the kernel's row number, and two identities for one thing with no stated way
// between them is how a revocation ends up unable to find what it is revoking. The answer is a
// LOOKUP, not a wider name: a node holds its row number as its own field, absent for a function the
// kernel's resolver never gave a row. A name that grows a field for every table that wants to find
// it is not a name.
//
// THE GENERATION IS LOAD-BEARING FROM THE FIRST CRASH-RESTART, not from some future hotplug. A node
// rebound after its driver died keeps its BDF and takes a NEW claim, and P02M0098 gives that claim a
// new generation - so a message stamped with the previous one is refused by arithmetic rather than
// by anyone remembering to check.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct BindingId {
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	pub generation: u64,
}

impl BindingId {
	pub const fn new(bus: u8, dev: u8, func: u8, generation: u64) -> Self {
		Self { bus, dev, func, generation }
	}

	// Whether this names the same FUNCTION as `other`, whatever binding either is about.
	//
	// The question a rebind asks: "is this the device I was driving", which is about the location
	// and not about the binding. `==` answers the other question - "is this the same binding" - and
	// the two must not be one operator, because a rebind that compared whole identities would
	// conclude a device it just rebound is a different device.
	pub fn same_function(self, other: BindingId) -> bool {
		self.bus == other.bus && self.dev == other.dev && self.func == other.func
	}

	// The same function, one binding later.
	pub fn rebound(self, generation: u64) -> Self {
		Self { generation, ..self }
	}
}

// ------------------------------------------------------- what a driver publishes

// A PROVIDER'S IDENTITY, WHICH THE MANAGER ASSIGNS AND A DRIVER NEVER CHOOSES.
//
// Handing one channel to two subscribers is not two connections - it is two consumers competing
// over one reply queue - so a connection is minted per consumer and the catalogue entry is what they
// are minted from. A driver that could choose its own identity could advertise itself as the system
// disk, which is why the manager mints these and the driver only ever sees its own token.
//
// `slot` is the manager's index for this publication and `generation` distinguishes reuse of the
// slot, so an id that named a withdrawn provider cannot silently name its replacement.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct ProviderId {
	pub binding: BindingId,
	pub slot: u16,
	pub generation: u32,
}

impl ProviderId {
	pub const fn new(binding: BindingId, slot: u16, generation: u32) -> Self {
		Self { binding, slot, generation }
	}

	// WHETHER THIS PUBLICATION BELONGS TO THAT BINDING - the same function AND the same generation.
	//
	// A provider published by a binding that is over is not this binding's, however identical the
	// bus address is: `BindingId`'s equality carries the generation, which is what makes a rebind of
	// one device distinguishable from the binding it replaced. The catalogue's withdrawal and the
	// model below both ask this, so what is tested is what runs.
	pub fn belongs_to(self, binding: BindingId) -> bool {
		self.binding == binding
	}
}

// THE WITHDRAWAL ITSELF, over any slot array, so the model and the production catalogue run ONE
// implementation instead of two that agree today.
//
// They used to share only the leaf predicate `ProviderId::belongs_to`: DeviceManager's
// `Catalogue::withdraw_binding` had its own loop, its own handle close and its own subscriber
// announcement, and `Publications::withdraw_binding` had a loop of its own. So the named
// publish/crash/subscribe test drove the model and would have passed unchanged if the production
// loop had stopped selecting correctly - which is the gap M7's race is supposed to close.
//
// What is shared is what CAN be: which slots belong to the binding, that each is emptied exactly
// once, and how many that was. What cannot is the side effect per slot - the production catalogue
// closes a channel handle and announces the withdrawal to subscribers, and the model has neither - so
// that arrives as a closure and is the caller's. Stating the split here is the point: a reader can
// see exactly how much of the decision the host test covers.
pub fn withdraw_slots<T>(slots: &mut [Option<T>], binding: BindingId, id_of: impl Fn(&T) -> ProviderId, mut withdrawn: impl FnMut(T)) -> usize {
	let mut gone = 0;
	for slot in slots.iter_mut() {
		if slot.as_ref().is_some_and(|held| id_of(held).belongs_to(binding)) {
			if let Some(held) = slot.take() {
				withdrawn(held);
			}
			gone += 1;
		}
	}
	gone
}

// THE SAME, AND IT CARRIES WHAT IT REMOVED, so nothing between the removal and the announcement is
// the caller's to get right.
//
// `withdraw_slots` hands each withdrawn item to a closure, and the production catalogue's closure did
// two things: closed the channel and copied the item somewhere the announcement loop could reach.
// The COPY is what went wrong - it collected into a `Vec` whose `try_reserve` failure was survivable,
// so on that path every provider was removed and closed and NOT ONE withdrawal was announced, leaving
// every subscriber holding metadata for providers that no longer exist. The host test could not see
// it: the closure is production code and the model has neither handles nor subscribers.
//
// So the transfer moves in here, where it is driven by a test: `out[..returned]` is exactly the items
// that were emptied, in slot order, one to one. What stays the caller's is the side effect per item -
// closing a channel, announcing to subscribers - which is a syscall and a send, and neither is a
// decision. `out` must be at least as long as `slots`; a shorter one is a caller that cannot receive
// what it is about to remove, so nothing is removed and the answer is `None`.
pub fn withdraw_slots_into<T>(slots: &mut [Option<T>], binding: BindingId, id_of: impl Fn(&T) -> ProviderId, out: &mut [Option<T>]) -> Option<usize> {
	if out.len() < slots.len() {
		return None;
	}
	let mut gone = 0;
	for slot in slots.iter_mut() {
		if slot.as_ref().is_some_and(|held| id_of(held).belongs_to(binding))
			&& let Some(held) = slot.take()
		{
			out[gone] = Some(held);
			gone += 1;
		}
	}
	Some(gone)
}

// WHAT AN OPERATOR'S `retry` MEANS ON A NODE IN THIS STATE, AND WHAT IT DOES TO THE CURSOR.
//
// Both halves lived in DeviceManager - `decide_policy` and `apply_policy` - where no host test can
// reach them, and both are exactly the kind of arithmetic that has been wrong here twice: a retry
// after exhaustion once handed back the WHOLE automatic budget because it subtracted from a counter
// `Step::NextCandidate` had already reset to zero, and a retry once rewound to the registry order
// rather than to the operator's stored choice. Each was found by reading. `select` and the one-shot
// `retry` are required to be under test, and a decision that lives in a binary nobody can drive is
// not.
//
// So the RULES are here and the effects on the node stay there: which states admit a retry, and
// where the cursor and the budget end up when one is granted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RetryVerdict {
	// An attempt has ended without a binding: `Failed`, `Backoff`, or `Unbound` with the candidates
	// exhausted. Exactly one further attempt is opened.
	Grant,
	// OUT OF REACH FOR THIS BOOT. A quarantined node's resources are charged and out of circulation
	// precisely because nothing confirmed the device was quiet, and an operator saying so does not
	// make it so.
	Quarantined,
	// There is a binding in flight, running, stopping, waiting or deliberately off. "Not now, and
	// here is why" - the operator's next move is to look at the state, not to try another verb.
	Busy,
	// A boot-critical binding takes no stored policy at all: it would live on a volume that is not
	// mounted when those bindings are made.
	Refused,
}

pub fn decide_retry(state: BindingState, boot_critical: bool) -> RetryVerdict {
	if boot_critical {
		return RetryVerdict::Refused;
	}
	match state {
		BindingState::Quarantined => RetryVerdict::Quarantined,
		BindingState::Failed | BindingState::Backoff | BindingState::Unbound => RetryVerdict::Grant,
		_ => RetryVerdict::Busy,
	}
}

// Where a granted retry leaves the attempt counter and the candidate cursor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Retry {
	// ONE BELOW THE BOUND, SET RATHER THAN DECREMENTED. `may_try_again` allows another attempt while
	// `attempt + 1 < max`, so leaving exactly one means starting from one below the bound whatever
	// the counter happened to hold - and on the case this verb exists for it holds ZERO, because
	// advancing past the final candidate resets it.
	pub attempt: u32,
	// REWOUND ONLY WHEN THERE IS NOTHING LEFT TO TRY, and then to the operator's own choice where
	// there is one. A cursor past the end is how a node records "every candidate has been tried",
	// and a retry that left it there would ask the loop to start nothing at all.
	pub candidate: usize,
}

pub fn one_more_attempt(candidate: usize, candidates: usize, preferred: Option<usize>, max_attempts: u32) -> Retry {
	Retry { attempt: max_attempts.saturating_sub(1), candidate: if candidate >= candidates { preferred.unwrap_or(0) } else { candidate } }
}

// WHICH CANDIDATE AN OPERATOR'S `select` NAMES, or none.
//
// POLICY NARROWS AND NEVER WIDENS: an artifact the registry did not declare for THIS device is
// refused rather than obeyed, which is the whole point of bounding a preference by the candidate
// list. The caller supplies the names in registry order; the answer is the cursor position.
pub fn selected_candidate(names: &[&[u8]], artifact: &[u8]) -> Option<usize> {
	names.iter().position(|name| *name == artifact)
}

// THE TWO EFFECTS AN EMPTIED SLOT OWES, AS A TRAIT, so the ORDER and the COMPLETENESS of them are
// something a test drives rather than something a reader checks.
//
// `withdraw_slots_into` answers WHICH publications a binding's end takes and hands them back; what
// each one then owes is a channel closed and a withdrawal announced. Both of those lived in a loop
// in DeviceManager, where no host test can reach them, and the count comparison beside it catches
// only a loop that stops VISITING - delete the `close` or the `announce` from the body and every
// test in this crate still passes while a consumer is left holding a channel whose server is gone,
// or a subscriber keeps metadata for a publication that no longer exists. That is M7's
// no-stale-provider and no-handle-leak rule, unproved.
//
// So the loop is here and its effects are named. The one order there is: the channel is closed
// BEFORE the announcement, because a consumer told the provider is gone must not then find the
// channel still open and use it. `Closes` is the same shape for the rollback, for the same reason.
pub trait Withdrawn<T> {
	// The channel this publication handed out, given back.
	fn close_channel(&mut self, provider: &T);
	// And everyone watching that kind is told.
	fn announce_gone(&mut self, provider: &T);
}

// Give both effects to every slot the withdrawal emptied, and answer how many got them.
//
// The caller compares that against what `withdraw_slots_into` said it emptied: the two numbers
// disagreeing is a subscriber left holding a provider that is gone.
pub fn apply_withdrawal<T, W: Withdrawn<T>>(taken: &[Option<T>], effects: &mut W) -> usize {
	let mut applied = 0;
	for provider in taken.iter().flatten() {
		effects.close_channel(provider);
		effects.announce_gone(provider);
		applied += 1;
	}
	applied
}

// WHAT A BINDING'S END TAKES WITH IT, AND WHAT A SUBSCRIBER MAY STILL REACH.
//
// The catalogue that answers subscribers lives in DeviceManager, holds channel handles and cannot be
// built on a host - so the RACE M7 names, a driver that publishes and crashes before any consumer
// subscribes, had no test that ran the decision at all. The named case compared two identities and
// said "whatever the catalogue does next", which is the half that was missing.
//
// This is that decision with the handles taken out: which publications a binding's end withdraws,
// and which generation a subscriber arriving afterwards reaches. It shares `belongs_to` with the
// production withdrawal, so the rule is one rule; what it does not carry is the channel bookkeeping,
// and that is stated rather than implied.
pub struct Publications<const N: usize> {
	slots: [Option<(ProviderId, u16)>; N],
}

impl<const N: usize> Default for Publications<N> {
	fn default() -> Self {
		Self::new()
	}
}

impl<const N: usize> Publications<N> {
	pub const fn new() -> Self {
		Self { slots: [None; N] }
	}

	// Record a publication. `None` when there is no room, which a caller must treat as a refusal
	// rather than as a publication nobody can find.
	pub fn publish(&mut self, id: ProviderId, kind: u16) -> Option<usize> {
		let at = self.slots.iter().position(Option::is_none)?;
		self.slots[at] = Some((id, kind));
		Some(at)
	}

	// EVERYTHING THAT BINDING PUBLISHED, GONE. Returns how many were withdrawn.
	//
	// The LOOP is `withdraw_slots` below rather than a second copy of it - see there for why.
	pub fn withdraw_binding(&mut self, binding: BindingId) -> usize {
		withdraw_slots(&mut self.slots, binding, |(id, _)| *id, |_| {})
	}

	// What a subscriber asking for `kind` can reach. `None` is an answer: a consumer that arrives
	// after the only publisher's binding ended must find nothing rather than a provider whose server
	// is gone.
	pub fn reachable(&self, kind: u16) -> Option<ProviderId> {
		self.slots.iter().flatten().find(|(_, published)| *published == kind).map(|(id, _)| *id)
	}

	pub fn live(&self) -> usize {
		self.slots.iter().flatten().count()
	}
}

// ------------------------------------------------------- the incident window

// WHEN ONE BIND-OR-RECOVER ATTEMPT-CHAIN MUST BE OVER.
//
// AN INCIDENT, NOT A NODE AND NOT A BOOT. Measuring from a node's first `BIND` ever would mean a
// driver that ran happily for an hour and then crashed has no budget left to be rebound with; its
// recovery would be `Failed` on arithmetic about a boot that finished long ago.
//
// Here rather than in the manager for the reason the state table is here: the arithmetic below has
// two clamps and an off-by-one between them decides whether a machine recovers, and that is not
// something to reason about inside a `no_std` binary nobody can drive.
pub struct IncidentWindow;

impl IncidentWindow {
	// The deadline one incident gets, given the boot window's length, the boot's own deadline and
	// the instant the incident opens.
	//
	// `boot_deadline` of 0 means none was published, or the boot's own has already been spent on
	// the first incident - either way nothing clamps this one.
	pub fn deadline(window: u64, share: u64, boot_deadline: u64, now: u64) -> u64 {
		if window == 0 || share == 0 {
			return 0;
		}
		let slice = window / share;
		let own = now.saturating_add(slice);
		// THE CLAMP APPLIES ONLY WHILE THERE IS STILL A BOOT TO OUTLAST. An hour after boot, `own`
		// is far past a boot deadline that expired long ago - and clamping to it would hand the
		// recovery a deadline ALREADY IN THE PAST, which is a budget spent before the work it
		// bounds was asked for.
		if boot_deadline > now && boot_deadline < own { boot_deadline } else { own }
	}
}

// ------------------------------------------------------------------ what one bind holds
//
// THE TRANSACTION'S LEDGER, IN THE CRATE WHERE IT CAN BE DRIVEN.
//
// It lived in DeviceManager, which is a `no_std` binary nothing can run on a host - so the one
// property the whole milestone rests on, that a bind either completes or leaves nothing behind, was
// asserted by reading the code. The fault cases M7 names could not be written: no test could
// instantiate the transaction, fail it at the claim, the resource, the spawn or the handshake, and
// then ask what was still held. The audit that found two leaked handles found them by reading too.
//
// So the ledger and the ORDER it gives things back in are here, over a `Closes` the caller supplies.
// DeviceManager's implementation calls the syscalls; the tests' implementation records what was done
// and in what order, which is what makes "closed exactly once, in this order, and nothing left" a
// thing a machine checks.

// The most resources one bind hands over: the device MMIO, an MSI vector, a key sink, a power
// connection and a console feed.
pub const MAX_BIND_RESOURCES: usize = 5;

// What a rollback does to the world. Separated from the ledger so the ledger can be driven.
//
// `kill` does NOT close the process handle: M4 keeps it until the exit event arrives, which is the
// difference between observing a child die and assuming it did.
pub trait Closes {
	fn kill(&mut self, process: u64);
	fn close(&mut self, handle: u64);
	// The claim's terminal state, or `None` where the release is still running and the answer will
	// arrive on the claim handle.
	fn release(&mut self, claim: u64) -> Option<u32>;
	fn kill_domain(&mut self, domain: u64);
}

// Everything one bind transaction has acquired and not yet handed over.
#[derive(Clone, Copy, Default)]
pub struct Holdings {
	pub domain: u64,
	pub process: u64,
	// The manager's end of the bootstrap channel.
	pub channel: u64,
	pub claim: u64,
	// The DRIVER's end, until the spawn consumes it. Zero afterwards.
	pub driver_side: u64,
	resources: [(u16, u64); MAX_BIND_RESOURCES],
	count: usize,
}

impl Holdings {
	pub fn new() -> Self {
		Self::default()
	}

	// The four an INSTALLED binding holds. A binding that reached `Online` has no untransferred
	// resources and no driver-side channel end - everything the bind assembled either reached the
	// driver or the attempt was rolled back before the binding existed - so those are empty by
	// construction rather than by omission.
	pub fn installed(domain: u64, process: u64, channel: u64, claim: u64) -> Self {
		Self { domain, process, channel, claim, ..Self::default() }
	}

	// Record a resource this attempt has acquired and not yet handed over. Answers false when the
	// ledger is full, which is a bind asking for more than the format carries.
	pub fn hold(&mut self, kind: u16, handle: u64) -> bool {
		if self.count >= MAX_BIND_RESOURCES {
			return false;
		}
		self.resources[self.count] = (kind, handle);
		self.count += 1;
		true
	}

	// That resource has been transferred: it is the driver's now, and a rollback must not close it.
	pub fn handed_over(&mut self, handle: u64) {
		for entry in self.resources[..self.count].iter_mut() {
			if entry.1 == handle {
				entry.1 = 0;
			}
		}
	}

	pub fn resources(&self) -> &[(u16, u64)] {
		&self.resources[..self.count]
	}

	// M4'S STEPS 1 TO 3, IN THE ONE ORDER THERE IS.
	//
	// 1. `SIG_KILL` the process, keeping its handle - the exit is what confirms it, not the kill.
	// 2. Close the manager's own handles: everything acquired and not handed over, then the channel.
	//    AFTER the kill, so a driver cannot be reading one as it goes, and BEFORE the release,
	//    because the release is what tears the device down under them.
	// 3. Release the claim, which STARTS the teardown rather than finishing it.
	//
	// The Domain is not touched here: killing it before the exit and the release would take the
	// process out from under a teardown still reading its handles. That is step 4's, in `settle`.
	pub fn begin_teardown<C: Closes>(&mut self, closes: &mut C) -> Pending {
		if self.process != 0 {
			closes.kill(self.process);
		}
		if self.driver_side != 0 {
			closes.close(self.driver_side);
			self.driver_side = 0;
		}
		for entry in self.resources[..self.count].iter_mut() {
			if entry.1 != 0 {
				closes.close(entry.1);
				entry.1 = 0;
			}
		}
		self.count = 0;
		if self.channel != 0 {
			closes.close(self.channel);
			self.channel = 0;
		}
		// A transaction that took no device has nothing to release and nothing to confirm. That is
		// not the same as a claim that reached `Free`, and it is not a quarantine either.
		let state: Option<u32> = if self.claim == 0 { Some(CLAIM_FREE) } else { closes.release(self.claim) };
		let claim = if state.is_some() {
			if self.claim != 0 {
				closes.close(self.claim);
			}
			0
		} else {
			self.claim
		};
		let pending = Pending { process: self.process, claim, domain: self.domain, exited: self.process == 0, state };
		// Handed to the teardown; a second call finds nothing to do.
		self.process = 0;
		self.claim = 0;
		self.domain = 0;
		pending
	}
}

// `abi::CLAIM_STATE_FREE`, named here so this crate does not depend on `abi` for one number. The
// value is the wire's and changing it in one place without the other is what the test below is for.
pub const CLAIM_FREE: u32 = 0;

// A teardown that has been started and not yet confirmed.
#[derive(Clone, Copy)]
pub struct Pending {
	pub process: u64,
	pub claim: u64,
	pub domain: u64,
	pub exited: bool,
	pub state: Option<u32>,
}

// Where a settled teardown leaves the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Settled {
	// Both confirmations arrived and the claim reached `Free`: the device is back.
	Free,
	// Something did not confirm, or confirmed as something other than `Free`. Its frames, vectors
	// and grants stay charged and out of circulation.
	Unconfirmed,
}

impl Pending {
	// What the node's queue delivered. Anything else a dying binding emits is about a binding that
	// is already over.
	pub fn note(&mut self, event: BindingEvent) {
		match event {
			BindingEvent::Exited { .. } => self.exited = true,
			BindingEvent::ClaimSettled { state, .. } => self.state = Some(state),
			_ => {}
		}
	}

	// M4'S STEP 4. `Some` once both confirmations are in or the deadline has passed, and only then
	// are the two handles closed and the Domain killed.
	pub fn settle<C: Closes>(&mut self, closes: &mut C, now: u64, deadline: u64) -> Option<Settled> {
		let confirmed: bool = self.exited && self.state.is_some();
		if !confirmed && now < deadline {
			return None;
		}
		let free: bool = confirmed && self.state == Some(CLAIM_FREE);
		if self.process != 0 {
			closes.close(self.process);
			self.process = 0;
		}
		if self.claim != 0 {
			closes.close(self.claim);
			self.claim = 0;
		}
		if self.domain != 0 {
			closes.kill_domain(self.domain);
			closes.close(self.domain);
			self.domain = 0;
		}
		Some(if free { Settled::Free } else { Settled::Unconfirmed })
	}
}

// ------------------------------------------------------------------------ the watchdog's decisions
//
// WHEN TO ASK, WHEN TO GIVE UP, AND WHAT COUNTS AS AN ANSWER - in the crate where a test can drive
// them. The state and its three decisions lived in DeviceManager, and the refusal tests M7 names
// could therefore only compare enum variants and integers: they would have passed against a
// supervisor that reset its watchdog on any frame, any generation and any sequence, which is exactly
// what `rt::heartbeat` used to do and what the milestone exists to have stopped doing.
#[derive(Clone, Copy, Default)]
pub struct Heartbeat {
	// The entry's declared deadline in ticks, or 0 for a driver that is not supervised this way.
	deadline: u32,
	// The sequence the outstanding `PING` was sent with, and whether one is outstanding.
	sequence: u32,
	awaiting: bool,
	// When the next `PING` is due, and when an outstanding one stops being answerable.
	due: u64,
	expires: u64,
	// THE VERDICT HAS BEEN GIVEN. A wedged binding is being torn down; asking it again would queue a
	// second verdict on every pass of the loop until that finishes, and the schedule this watchdog
	// was keeping belongs to a binding that is over. Cleared by `arm`, which is what the NEXT
	// binding on this node calls.
	spent: bool,
}

// What the watchdog wants done this tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Beat {
	// Nothing is due.
	Idle,
	// Send a `PING` carrying this sequence.
	Ask(u32),
	// The outstanding one was not answered inside the deadline the entry declared.
	Wedged,
}

impl Heartbeat {
	// Arm from the entry's declared deadline. The cadence is `heartbeat_period` and not a second
	// number: a driver always gets one whole period to answer inside the deadline it declared.
	pub fn arm(&mut self, deadline: Option<u32>, now: u64, period: u32) {
		match deadline {
			Some(deadline) if deadline != 0 => *self = Heartbeat { deadline, sequence: 0, awaiting: false, due: now.saturating_add(period as u64), expires: 0, spent: false },
			_ => *self = Heartbeat::default(),
		}
	}

	pub fn supervised(&self) -> bool {
		self.deadline != 0
	}

	pub fn deadline(&self) -> u32 {
		self.deadline
	}

	pub fn awaiting(&self) -> bool {
		self.awaiting
	}

	// The soonest tick this node needs the wait to come back at, or 0 for "nothing to wake for".
	pub fn wake_at(&self) -> u64 {
		if !self.supervised() || self.spent {
			return 0;
		}
		if self.awaiting { self.expires } else { self.due }
	}

	// AN ANSWER THAT ECHOES THE NUMBER IT WAS ASKED WITH, AND NOTHING ELSE COUNTS.
	//
	// A pong with a sequence nobody is waiting for - a duplicate, one from an earlier round, one
	// invented - does NOT reset the watchdog. Answers whether it counted, so a caller can say so.
	pub fn answered(&mut self, sequence: u32, now: u64, period: u32) -> bool {
		if self.spent || !self.awaiting || sequence != self.sequence {
			return false;
		}
		self.awaiting = false;
		self.due = now.saturating_add(period as u64);
		true
	}

	// What to do at `now`. `Ask` hands out the next sequence and starts the deadline; the caller
	// reports whether the send happened, because a channel that has gone is a driver that ended
	// rather than one that is slow.
	pub fn tick(&mut self, now: u64) -> Beat {
		if !self.supervised() || self.spent {
			return Beat::Idle;
		}
		if self.awaiting {
			if now < self.expires {
				return Beat::Idle;
			}
			// ONCE, AND THEN NOTHING. Clearing `awaiting` alone left `due` in the past, so the very
			// next pass sent another `PING` to a binding that was being torn down - and a supervisor
			// that keeps asking after its own verdict is one whose verdict meant nothing.
			self.awaiting = false;
			self.spent = true;
			return Beat::Wedged;
		}
		if now < self.due {
			return Beat::Idle;
		}
		self.sequence = self.sequence.wrapping_add(1);
		Beat::Ask(self.sequence)
	}

	// The `PING` went out: the deadline for answering it starts now.
	pub fn asked(&mut self, now: u64) {
		self.awaiting = true;
		self.expires = now.saturating_add(self.deadline as u64);
	}

	// The `PING` could not be sent. Not an answer and not a wedge - stop asking until the next
	// period, and let the exit event arrive on its own.
	pub fn unsendable(&mut self, now: u64) {
		self.due = now.saturating_add(self.deadline as u64);
	}
}

// ------------------------------------------------------------------ which driver a function gets
//
// THE RUNTIME MATCH DECISION, IN THE CRATE WHERE A TEST CAN DRIVE IT.
//
// It was `device_manager::Rule::matches`, private to a binary nothing runs on a host, and the
// host-tested predicate beside it - `system_manifest::MatchRule::overlaps` - is a DIFFERENT
// operation: that one asks whether two rules could both match something, this one asks whether one
// rule matches one function. So the milestone's decisive negative case was checked against neither.
// Deleting the transport check here would have left every named test green while an ordinary PCI
// function whose class byte happens to equal a virtio type was offered to a virtio driver.

// What a discovered function IS, as the matcher reads it. The fields the kernel's inventory carries,
// named here so this crate does not depend on the ABI for a match it performs on integers.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Discovered {
	pub transport: u8,
	pub virtio_type: u32,
	pub class: u8,
	pub subclass: u8,
	pub prog_if: u8,
	pub vendor: u16,
	pub product: u16,
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
}

// One registry rule. Every predicate that is PRESENT must hold; `None` is "do not ask", which is not
// the same as "must be absent".
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Match {
	pub transport: Option<u8>,
	pub virtio_type: Option<u32>,
	pub class: Option<u8>,
	pub subclass: Option<u8>,
	pub prog_if: Option<u8>,
	pub vendor: Option<u16>,
	pub product: Option<u16>,
	// bus, dev, func - a rule pinning one function by where it is plugged in.
	pub address: Option<(u8, u8, u8)>,
}

impl Match {
	pub fn matches(self, found: &Discovered) -> bool {
		if self.class.is_some_and(|class| found.class != class) {
			return false;
		}
		if self.subclass.is_some_and(|subclass| found.subclass != subclass) {
			return false;
		}
		if self.prog_if.is_some_and(|interface| found.prog_if != interface) {
			return false;
		}
		// THE TRANSPORT IS ASKED BEFORE THE TYPE, and that ordering is the whole point of the pair:
		// `virtio_type` is only a virtio number on a function whose transport says so. Without it a
		// rule for virtio type 2 matches anything this system happens to number 2 next - an ordinary
		// PCI function offered to a virtio driver, which is the case M4 exists to prevent.
		if self.transport.is_some_and(|transport| found.transport != transport) {
			return false;
		}
		if self.virtio_type.is_some_and(|kind| found.virtio_type != kind) {
			return false;
		}
		if self.vendor.is_some_and(|vendor| found.vendor != vendor) {
			return false;
		}
		if self.product.is_some_and(|product| found.product != product) {
			return false;
		}
		if let Some((bus, dev, func)) = self.address
			&& (found.bus != bus || found.dev != dev || found.func != func)
		{
			return false;
		}
		true
	}
}
