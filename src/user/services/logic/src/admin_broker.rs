//! ADMINSERVICE'S AUTHORITY: request connections, requests, grants, their deadlines, and every way they end.
//!
//! ONE STATE MACHINE, AND IT IS THE ONLY ONE. `Preparing -> Prepared -> AwaitingAttention -> AwaitingDecision
//! -> Approving -> Granted -> Consuming -> Consumed -> Completed | Failed | OutcomeUnknown`, with `Declining ->
//! Declined`, `Expired` and `Cancelled` ending a request before it is consumed. Everything the service does -
//! ask an executor, write the journal, show or hide the protected screen, answer a requester, dispatch an
//! effect - is an `Effect` this module asks for, and every answer comes back through a call that checks the
//! request is still where the answer expects it. A late answer for a request that has moved on changes
//! nothing: cancellation and expiry are terminal, and nothing revives them.
//!
//! CHECKED AGAIN BEFORE IT IS GRANTED. An approval asks the executor to revalidate the prepared operation -
//! the same live target, the same generation - before the approval is even recorded; a target replaced,
//! reset or withdrawn since the person looked is declined, and the executor checks once more under its own
//! start guard before the one attempt.
//!
//! RECORDED BEFORE IT HAPPENS. A request is journaled before it can be confirmed, an approval before a grant
//! exists, consumption before the effect is dispatched, and every decline before it is answered. A journal
//! write that fails, or whose answer never comes, is not a commit: it issues no authority and no effect.
//!
//! THE OWNER IS THE LAUNCHED TASK, NOT A CHANNEL. A connection records whether its launching task is alive
//! and whether its own channel is open, and either loss ends its pending request and any unconsumed grant -
//! even when some other process holds both endpoints. The owner is checked before an approval, before a
//! redemption, and again after the consumption record is acknowledged; the last check is the execution
//! admission boundary, and a death observed before it means no dispatch, ever, for that grant.
//!
//! Time is in the system's 100 Hz ticks.

use crate::admin_descriptor::{self as descriptor, Action, Asked, Descriptor};
use alloc::string::String;
use alloc::vec::Vec;

pub const MAX_CONNECTIONS: usize = 16;
/// A request waits at most this long, from its commit, for a person's decision.
pub const CONFIRM_TICKS: u64 = 6000;
/// A grant expires this long after confirmation.
pub const GRANT_TICKS: u64 = 3000;
/// Every local exchange - a preparation, a journal write, a control acknowledgment - has this long.
pub const EXCHANGE_TICKS: u64 = 500;

pub type ConnectionId = u32;
pub type RequestId = u64;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Scope {
	pub actions: Vec<Action>,
	pub target_prefix: String,
}

impl Scope {
	fn admits(&self, asked: &Asked) -> bool {
		self.actions.contains(&asked.action) && asked.target.starts_with(self.target_prefix.as_str())
	}
}

#[derive(Clone, Debug)]
pub struct Connection {
	pub id: ConnectionId,
	pub component: String,
	pub scope: Scope,
	pub launch: u64,
	/// Its channel is open.
	pub open: bool,
	/// Its launching task has not ended.
	pub owner_alive: bool,
	pub request: Option<RequestId>,
}

impl Connection {
	fn live(&self) -> bool {
		self.open && self.owner_alive
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
	Preparing,
	Prepared,
	AwaitingAttention,
	AwaitingDecision,
	Approving,
	Granted,
	Consuming,
	Consumed,
	Completed,
	Failed,
	OutcomeUnknown,
	Declining,
	Declined,
	Expired,
	Cancelled,
}

impl State {
	pub fn terminal(self) -> bool {
		matches!(self, State::Completed | State::Failed | State::OutcomeUnknown | State::Declined | State::Expired | State::Cancelled)
	}

	/// Before consumption: cancellable, and never dispatched.
	fn unconsumed(self) -> bool {
		matches!(self, State::Preparing | State::Prepared | State::AwaitingAttention | State::AwaitingDecision | State::Approving | State::Granted)
	}

	/// In the confirmation path: the one request that may hold the prompt.
	fn confirming(self) -> bool {
		matches!(self, State::Preparing | State::Prepared | State::AwaitingAttention | State::AwaitingDecision | State::Approving)
	}
}

/// A journal event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
	Requested,
	Granted,
	Declined,
	Consumed,
	Completed,
	Failed,
	OutcomeUnknown,
}

/// What an executor reported.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
	Completed,
	Failed,
	Unknown,
}

#[derive(Clone, Debug)]
pub struct Request {
	pub id: RequestId,
	pub connection: ConnectionId,
	pub state: State,
	pub asked: Asked,
	pub descriptor: Option<Descriptor>,
	pub executor: u32,
	pub operation: u64,
	/// The confirmation deadline, from the commit of the request.
	pub confirm_by: u64,
	/// The grant's deadline, from the confirmation.
	pub grant_by: u64,
	/// The deadline of the local exchange in flight.
	pub exchange_by: u64,
	/// The protected session the request was shown under.
	pub session: u64,
	/// The journal event whose acknowledgment is awaited.
	pub journaling: Option<Event>,
	/// Why it was declined, for the journal.
	pub reason: &'static str,
	/// How a decline ends: `Declined`, `Expired` for a deadline, `Cancelled` for a requester that withdrew or
	/// was lost.
	pub ending: State,
	/// The requester has had its answer.
	pub answered: bool,
	/// The effect was dispatched.
	pub dispatched: bool,
	/// A redemption is waiting for its result.
	pub redeeming: bool,
	/// The approval is waiting for the executor's revalidation, before its record.
	pub revalidating: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Effect {
	/// Ask the request's executor to prepare it.
	Prepare { request: RequestId },
	/// Ask the executor whether the prepared operation still names the live target; `revalidated` answers.
	Revalidate { request: RequestId, executor: u32, operation: u64 },
	/// Journal an event for a request; `journaled` answers.
	Journal { request: RequestId, event: Event, reason: &'static str },
	/// Show the protected screen with this request, or with nothing waiting.
	Show { request: Option<RequestId>, session: u64 },
	/// Close the protected screen.
	Hide,
	/// Answer the requester: a grant, or `declined`.
	Answer { connection: ConnectionId, request: RequestId, granted: bool },
	/// Dispatch the operation - once.
	Dispatch { request: RequestId, executor: u32, operation: u64 },
	/// Release a preparation that will never run.
	Release { request: RequestId, executor: u32, operation: u64 },
	/// The grant is dead: its channel closes.
	CloseGrant { request: RequestId },
	/// Answer the redemption.
	Redeemed { request: RequestId, outcome: Option<Outcome> },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	Exhausted,
	NotFound,
}

pub struct Broker {
	pub epoch: u64,
	connections: Vec<Connection>,
	requests: Vec<Request>,
	next_connection: ConnectionId,
	next_request: RequestId,
	/// The protected screen is showing this request, under this session.
	showing: Option<(Option<RequestId>, u64)>,
	/// An outcome could not be recorded: no approval is admitted until storage recovers.
	pub blocked: bool,
	/// The path to a person is there: a trusted display and a trusted keyboard. Without it every request is
	/// declined.
	pub path: bool,
}

impl Broker {
	pub fn new(epoch: u64) -> Broker {
		Broker { epoch, connections: Vec::new(), requests: Vec::new(), next_connection: 1, next_request: 1, showing: None, blocked: false, path: false }
	}

	pub fn connection(&self, id: ConnectionId) -> Option<&Connection> {
		self.connections.iter().find(|connection| connection.id == id)
	}

	pub fn request(&self, id: RequestId) -> Option<&Request> {
		self.requests.iter().find(|request| request.id == id)
	}

	pub fn requests(&self) -> &[Request] {
		&self.requests
	}

	pub fn connections(&self) -> &[Connection] {
		&self.connections
	}

	fn at(&self, id: RequestId) -> Option<usize> {
		self.requests.iter().position(|request| request.id == id)
	}

	fn connection_mut(&mut self, id: ConnectionId) -> Option<&mut Connection> {
		self.connections.iter_mut().find(|connection| connection.id == id)
	}

	fn live(&self, id: ConnectionId) -> bool {
		self.connection(id).is_some_and(Connection::live)
	}

	/// A request connection for one launched task, under the component's name and scope. Sixteen at most.
	pub fn open(&mut self, component: String, scope: Scope, launch: u64) -> Result<ConnectionId, Refusal> {
		if self.connections.len() >= MAX_CONNECTIONS {
			return Err(Refusal::Exhausted);
		}
		let id = self.next_connection;
		self.next_connection = self.next_connection.wrapping_add(1).max(1);
		self.connections.push(Connection { id, component, scope, launch, open: true, owner_alive: true, request: None });
		Ok(id)
	}

	/// ADMISSION: an open connection whose owner lives, with nothing outstanding, asking within its scope and
	/// within bounds, with no other request holding the confirmation path and a path to a person at all.
	/// Anything else is declined - and recorded.
	pub fn ask(&mut self, connection: ConnectionId, asked: Asked, now: u64, effects: &mut Vec<Effect>) -> Option<RequestId> {
		let Some(owner) = self.connection(connection) else { return None };
		if owner.request.is_some() {
			return None;
		}
		let scope_ok = owner.scope.admits(&asked);
		let live = owner.live();
		let id = self.next_request;
		self.next_request += 1;
		self.requests.push(Request { id, connection, state: State::Preparing, asked, descriptor: None, executor: 0, operation: 0, confirm_by: 0, grant_by: 0, exchange_by: now + EXCHANGE_TICKS, session: 0, journaling: None, reason: "", ending: State::Declined, answered: false, dispatched: false, redeeming: false, revalidating: false });
		if let Some(owner) = self.connection_mut(connection) {
			owner.request = Some(id);
		}
		let at = self.requests.len() - 1;
		let reason = if !live {
			Some("requester lost")
		} else if !scope_ok {
			Some("outside the connection's scope")
		} else if descriptor::check_asked(&self.requests[at].asked).is_err() {
			Some("request past its bounds")
		} else if !self.path {
			Some("no trusted display and keyboard")
		} else if self.blocked {
			Some("an outcome is unrecorded")
		} else if self.requests.iter().any(|other| other.id != id && other.state.confirming()) {
			Some("another request holds the confirmation")
		} else {
			None
		};
		match reason {
			Some(reason) => self.decline(at, reason, now, effects),
			None => effects.push(Effect::Prepare { request: id }),
		}
		Some(id)
	}

	/// The executor chose the executor key for a preparation before it began.
	pub fn bind_executor(&mut self, request: RequestId, executor: u32) {
		if let Some(at) = self.at(request) {
			self.requests[at].executor = executor;
		}
	}

	/// The executor answered a preparation. Checked against what was asked; a request that moved on while it
	/// was preparing releases what it prepared.
	pub fn prepared(&mut self, request: RequestId, answer: Result<(Descriptor, usize, u64), &'static str>, now: u64, effects: &mut Vec<Effect>) {
		let Some(at) = self.at(request) else {
			if let Ok((_, _, operation)) = answer {
				effects.push(Effect::Release { request, executor: 0, operation });
			}
			return;
		};
		if self.requests[at].state != State::Preparing {
			if let Ok((_, _, operation)) = answer {
				let executor = self.requests[at].executor;
				effects.push(Effect::Release { request, executor, operation });
			}
			return;
		}
		match answer {
			Ok((prepared, encoded, operation)) => {
				self.requests[at].operation = operation;
				if descriptor::check_prepared(&self.requests[at].asked, &prepared, encoded).is_err() {
					let executor = self.requests[at].executor;
					effects.push(Effect::Release { request, executor, operation });
					self.requests[at].operation = 0;
					self.decline(at, "the executor prepared something else", now, effects);
					return;
				}
				self.requests[at].descriptor = Some(prepared);
				self.requests[at].state = State::Prepared;
				self.journal(at, Event::Requested, "", now, effects);
			}
			Err(reason) => self.decline(at, reason, now, effects),
		}
	}

	fn journal(&mut self, at: usize, event: Event, reason: &'static str, now: u64, effects: &mut Vec<Effect>) {
		self.requests[at].journaling = Some(event);
		self.requests[at].exchange_by = now + EXCHANGE_TICKS;
		effects.push(Effect::Journal { request: self.requests[at].id, event, reason });
	}

	/// DECLINE: the decline is recorded first, then answered - whatever the record's fate.
	fn decline(&mut self, at: usize, reason: &'static str, now: u64, effects: &mut Vec<Effect>) {
		self.decline_as(at, State::Declined, reason, now, effects);
	}

	fn decline_as(&mut self, at: usize, ending: State, reason: &'static str, now: u64, effects: &mut Vec<Effect>) {
		if self.requests[at].state.terminal() || self.requests[at].state == State::Declining {
			return;
		}
		self.requests[at].ending = ending;
		self.hide_if_showing(self.requests[at].id, effects);
		if self.requests[at].operation != 0 {
			let (request, executor, operation) = (self.requests[at].id, self.requests[at].executor, self.requests[at].operation);
			effects.push(Effect::Release { request, executor, operation });
			self.requests[at].operation = 0;
		}
		if self.requests[at].state == State::Granted {
			effects.push(Effect::CloseGrant { request: self.requests[at].id });
		}
		self.requests[at].state = State::Declining;
		self.requests[at].reason = reason;
		self.journal(at, Event::Declined, reason, now, effects);
	}

	fn hide_if_showing(&mut self, request: RequestId, effects: &mut Vec<Effect>) {
		if matches!(self.showing, Some((Some(shown), _)) if shown == request) {
			self.showing = None;
			effects.push(Effect::Hide);
		}
	}

	/// A journal write answered: `ok` is a durable commit and nothing else is. Correlated by request and
	/// event; an answer for an event no longer awaited is dropped.
	pub fn journaled(&mut self, request: RequestId, event: Event, ok: bool, now: u64, effects: &mut Vec<Effect>) {
		let Some(at) = self.at(request) else { return };
		if self.requests[at].journaling != Some(event) {
			return;
		}
		self.requests[at].journaling = None;
		let connection = self.requests[at].connection;
		match (self.requests[at].state, event) {
			(State::Prepared, Event::Requested) => {
				if !ok {
					return self.decline(at, "the request could not be recorded", now, effects);
				}
				if !self.live(connection) {
					return self.decline(at, "requester lost", now, effects);
				}
				self.requests[at].state = State::AwaitingAttention;
				self.requests[at].confirm_by = now + CONFIRM_TICKS;
			}
			(State::Approving, Event::Granted) => {
				// RECHECKED AFTER THE ACKNOWLEDGMENT, before any authority exists.
				if !ok {
					return self.decline(at, "the approval could not be recorded", now, effects);
				}
				if !self.live(connection) {
					return self.decline(at, "requester lost", now, effects);
				}
				self.requests[at].state = State::Granted;
				self.requests[at].grant_by = now + GRANT_TICKS;
				self.requests[at].answered = true;
				effects.push(Effect::Answer { connection, request, granted: true });
			}
			(State::Consuming, Event::Consumed) => {
				if !ok {
					// NOT RECORDED, NOT DISPATCHED. The grant is spent all the same: its one attempt was asked
					// for, and nothing retries it.
					self.requests[at].state = State::Failed;
					self.requests[at].redeeming = false;
					effects.push(Effect::Redeemed { request, outcome: Some(Outcome::Failed) });
					effects.push(Effect::CloseGrant { request });
					return;
				}
				// THE EXECUTION ADMISSION BOUNDARY. A death observed now means no dispatch, and the spent grant
				// is recorded as failed without one.
				if !self.live(connection) || now >= self.requests[at].grant_by {
					self.requests[at].state = State::Failed;
					self.requests[at].redeeming = false;
					effects.push(Effect::Redeemed { request, outcome: Some(Outcome::Failed) });
					effects.push(Effect::CloseGrant { request });
					self.requests[at].journaling = Some(Event::Failed);
					self.requests[at].exchange_by = now + EXCHANGE_TICKS;
					effects.push(Effect::Journal { request, event: Event::Failed, reason: "owner lost or deadline passed before dispatch" });
					return;
				}
				self.requests[at].state = State::Consumed;
				self.requests[at].dispatched = true;
				let (executor, operation) = (self.requests[at].executor, self.requests[at].operation);
				self.requests[at].operation = 0;
				effects.push(Effect::Dispatch { request, executor, operation });
			}
			(State::Declining, Event::Declined) => {
				self.requests[at].state = self.requests[at].ending;
				self.answer_declined(at, effects);
			}
			// An outcome record: if it did not commit, the consumed record stands with its outcome unknown in
			// the journal, and nothing further is approved until storage recovers.
			(State::Completed | State::Failed | State::OutcomeUnknown, _) => {
				if !ok {
					self.blocked = true;
				}
			}
			_ => {}
		}
	}

	fn answer_declined(&mut self, at: usize, effects: &mut Vec<Effect>) {
		if self.requests[at].answered {
			return;
		}
		self.requests[at].answered = true;
		let (connection, request) = (self.requests[at].connection, self.requests[at].id);
		effects.push(Effect::Answer { connection, request, granted: false });
	}

	/// The person pressed the secure-attention chord: the protected screen shows the request waiting for it,
	/// or that nothing is waiting.
	pub fn attention(&mut self, session: u64, now: u64, effects: &mut Vec<Effect>) {
		// ONE PROTECTED SCREEN AT A TIME: the chord while one is up changes nothing.
		if self.showing.is_some() {
			return;
		}
		let waiting = self.requests.iter().position(|request| request.state == State::AwaitingAttention && self.live(request.connection) && now < request.confirm_by);
		let shown = waiting.map(|at| {
			self.requests[at].state = State::AwaitingDecision;
			self.requests[at].session = session;
			self.requests[at].id
		});
		self.showing = Some((shown, session));
		effects.push(Effect::Show { request: shown, session });
	}

	/// ENTER, on the armed session showing a request: approved, if its owner still lives and its deadline has
	/// not passed. The grant exists only once the approval is recorded.
	pub fn approve(&mut self, session: u64, now: u64, effects: &mut Vec<Effect>) {
		let Some((shown, showing)) = self.showing else { return };
		if showing != session {
			return;
		}
		self.showing = None;
		effects.push(Effect::Hide);
		let Some(id) = shown else { return };
		let Some(at) = self.at(id) else { return };
		if self.requests[at].state != State::AwaitingDecision || self.requests[at].session != session {
			return;
		}
		if !self.live(self.requests[at].connection) {
			return self.decline(at, "requester lost", now, effects);
		}
		if now >= self.requests[at].confirm_by {
			return self.decline(at, "the confirmation expired", now, effects);
		}
		if self.blocked {
			return self.decline(at, "an outcome is unrecorded", now, effects);
		}
		self.requests[at].state = State::Approving;
		self.requests[at].revalidating = true;
		self.requests[at].exchange_by = now + EXCHANGE_TICKS;
		let (request, executor, operation) = (self.requests[at].id, self.requests[at].executor, self.requests[at].operation);
		effects.push(Effect::Revalidate { request, executor, operation });
	}

	/// The executor answered the revalidation: the same live target, or not. Only then is the approval
	/// recorded - and only if its owner still lives.
	pub fn revalidated(&mut self, request: RequestId, still: bool, now: u64, effects: &mut Vec<Effect>) {
		let Some(at) = self.at(request) else { return };
		if self.requests[at].state != State::Approving || !self.requests[at].revalidating {
			return;
		}
		self.requests[at].revalidating = false;
		if !still {
			return self.decline(at, "the target changed after it was prepared", now, effects);
		}
		if !self.live(self.requests[at].connection) {
			return self.decline(at, "requester lost", now, effects);
		}
		self.journal(at, Event::Granted, "", now, effects);
	}

	/// ESCAPE, or the protected screen lost: the request shown is declined, and the screen closes.
	pub fn refuse(&mut self, session: u64, reason: &'static str, now: u64, effects: &mut Vec<Effect>) {
		let Some((shown, showing)) = self.showing else { return };
		if showing != session {
			return;
		}
		self.showing = None;
		effects.push(Effect::Hide);
		if let Some(at) = shown.and_then(|id| self.at(id)) {
			self.decline(at, reason, now, effects);
		}
	}

	/// THE REQUESTER WITHDREW, closed its connection, or its launching task ended: whatever it had pending
	/// ends, and an unconsumed grant with it. Nothing already dispatched is recalled.
	fn lose(&mut self, connection: ConnectionId, reason: &'static str, now: u64, effects: &mut Vec<Effect>) {
		let Some(id) = self.connection(connection).and_then(|owner| owner.request) else { return };
		let Some(at) = self.at(id) else { return };
		if self.requests[at].state.unconsumed() {
			self.decline_as(at, State::Cancelled, reason, now, effects);
		}
	}

	pub fn cancel(&mut self, connection: ConnectionId, now: u64, effects: &mut Vec<Effect>) {
		self.lose(connection, "cancelled by the requester", now, effects);
	}

	pub fn closed(&mut self, connection: ConnectionId, now: u64, effects: &mut Vec<Effect>) {
		if let Some(owner) = self.connection_mut(connection) {
			owner.open = false;
		}
		self.lose(connection, "requester connection closed", now, effects);
	}

	pub fn owner_died(&mut self, connection: ConnectionId, now: u64, effects: &mut Vec<Effect>) {
		if let Some(owner) = self.connection_mut(connection) {
			owner.owner_alive = false;
		}
		self.lose(connection, "requester task ended", now, effects);
	}

	/// THE OWNER CANNOT BE OBSERVED ANY MORE - its observer is invalid, or a wait on it failed. That is not
	/// proof it ended, and it is treated exactly as if it had: nothing is issued to a task nobody can watch.
	pub fn observer_failed(&mut self, connection: ConnectionId, now: u64, effects: &mut Vec<Effect>) {
		if let Some(owner) = self.connection_mut(connection) {
			owner.owner_alive = false;
		}
		self.lose(connection, "the requester could not be observed", now, effects);
	}

	/// A request is over and answered: its connection may ask again.
	pub fn settle(&mut self) {
		let finished: Vec<(RequestId, ConnectionId)> = self.requests.iter().filter(|request| request.state.terminal() && request.answered && request.journaling.is_none() && !request.redeeming).map(|request| (request.id, request.connection)).collect();
		for (id, connection) in finished {
			self.requests.retain(|request| request.id != id);
			if let Some(owner) = self.connection_mut(connection)
				&& owner.request == Some(id)
			{
				owner.request = None;
			}
		}
		// A CONNECTION STAYS WHILE ITS CHANNEL DOES - a delegate may hold it after its owner ended, and what it
		// asks is declined rather than unknown - or while a request of its own is still settling.
		let requests = &self.requests;
		self.connections.retain(|connection| connection.open || requests.iter().any(|request| request.connection == connection.id));
	}

	/// EXECUTE, on a grant's channel. Serialized with everything else here: the grant must be unconsumed,
	/// within its deadline, and its original connection and launching task alive - whoever holds the grant.
	/// Consumption is recorded before anything is dispatched.
	pub fn redeem(&mut self, request: RequestId, now: u64, effects: &mut Vec<Effect>) -> bool {
		let Some(at) = self.at(request) else { return false };
		if self.requests[at].state != State::Granted || !self.live(self.requests[at].connection) || now >= self.requests[at].grant_by || self.blocked {
			return false;
		}
		self.requests[at].state = State::Consuming;
		self.requests[at].redeeming = true;
		self.journal(at, Event::Consumed, "", now, effects);
		true
	}

	/// The executor answered the one dispatch.
	pub fn executed(&mut self, request: RequestId, outcome: Outcome, now: u64, effects: &mut Vec<Effect>) {
		let Some(at) = self.at(request) else { return };
		if self.requests[at].state != State::Consumed {
			return;
		}
		let (state, event) = match outcome {
			Outcome::Completed => (State::Completed, Event::Completed),
			Outcome::Failed => (State::Failed, Event::Failed),
			Outcome::Unknown => (State::OutcomeUnknown, Event::OutcomeUnknown),
		};
		self.requests[at].state = state;
		self.requests[at].redeeming = false;
		effects.push(Effect::Redeemed { request, outcome: Some(outcome) });
		effects.push(Effect::CloseGrant { request });
		self.journal(at, event, "", now, effects);
	}

	/// An executor went away. Its preparations and unused grants are void; a dispatch it never answered is an
	/// unknown outcome - and is not retried.
	pub fn executor_lost(&mut self, executor: u32, now: u64, effects: &mut Vec<Effect>) {
		let ids: Vec<RequestId> = self.requests.iter().filter(|request| request.executor == executor && !request.state.terminal()).map(|request| request.id).collect();
		for id in ids {
			let Some(at) = self.at(id) else { continue };
			self.requests[at].operation = 0;
			match self.requests[at].state {
				State::Consumed => self.executed(id, Outcome::Unknown, now, effects),
				State::Consuming => {}
				state if state.unconsumed() => self.decline(at, "the executor went away", now, effects),
				_ => {}
			}
		}
	}

	/// The trusted path is there, or it is gone. Gone, the request on the screen ends with it.
	pub fn set_path(&mut self, present: bool, now: u64, effects: &mut Vec<Effect>) {
		self.path = present;
		if !present && let Some((_, session)) = self.showing {
			self.refuse(session, "the trusted display or keyboard was lost", now, effects);
		}
	}

	/// Storage recovered: approvals are admitted again.
	pub fn recovered(&mut self) {
		self.blocked = false;
	}

	/// DEADLINES. A local exchange that has not answered in five seconds has failed - a lost journal reply is not
	/// a commit - and a confirmation or a grant past its deadline has expired.
	pub fn tick(&mut self, now: u64, effects: &mut Vec<Effect>) {
		for at in 0..self.requests.len() {
			let request = &self.requests[at];
			let id = request.id;
			match request.state {
				State::Preparing | State::Prepared | State::Approving if now >= request.exchange_by => {
					self.requests[at].journaling = None;
					self.requests[at].revalidating = false;
					self.decline(at, "a local exchange did not answer", now, effects);
				}
				State::Consuming if now >= request.exchange_by => {
					// THE CONSUMPTION RECORD WAS NOT CONFIRMED, so nothing is dispatched - and the grant is spent.
					self.requests[at].journaling = None;
					self.requests[at].state = State::Failed;
					self.requests[at].redeeming = false;
					effects.push(Effect::Redeemed { request: id, outcome: Some(Outcome::Failed) });
					effects.push(Effect::CloseGrant { request: id });
				}
				State::Declining if now >= request.exchange_by => {
					// THE DECLINE IS ANSWERED EVEN IF ITS RECORD NEVER WAS.
					self.requests[at].journaling = None;
					self.requests[at].state = self.requests[at].ending;
					self.answer_declined(at, effects);
				}
				State::AwaitingAttention | State::AwaitingDecision if now >= request.confirm_by => self.decline_as(at, State::Expired, "the confirmation expired", now, effects),
				State::Granted if now >= request.grant_by => {
					self.requests[at].state = State::Expired;
					effects.push(Effect::CloseGrant { request: id });
					self.requests[at].journaling = Some(Event::Declined);
					self.requests[at].exchange_by = now + EXCHANGE_TICKS;
					effects.push(Effect::Journal { request: id, event: Event::Declined, reason: "the grant expired unused" });
				}
				_ => {}
			}
		}
		// A terminal request's own record gets its five seconds too.
		for request in self.requests.iter_mut() {
			if request.state.terminal() && request.journaling.is_some() && now >= request.exchange_by {
				request.journaling = None;
			}
		}
	}

	pub fn next_deadline(&self) -> Option<u64> {
		self.requests
			.iter()
			.filter_map(|request| match request.state {
				State::Preparing | State::Prepared | State::Approving | State::Consuming | State::Declining => Some(request.exchange_by),
				State::AwaitingAttention | State::AwaitingDecision => Some(request.confirm_by),
				State::Granted => Some(request.grant_by),
				_ if request.journaling.is_some() => Some(request.exchange_by),
				_ => None,
			})
			.min()
	}
}

#[cfg(test)]
mod tests;
