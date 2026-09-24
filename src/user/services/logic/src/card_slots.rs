//! SMARTCARDSERVICE'S DECISIONS: who holds which slot of which reader, what is in flight on it, and
//! what has to happen - and in what order - before anybody else may use it.
//!
//! ONE SLOT, ONE THING AT A TIME. A slot is absent, resetting, idle, owned by a transaction, carrying
//! that transaction's one operation, cancelling it, or unavailable. A transaction captures the reader,
//! the slot, the card's generation and the service's reset epoch for that slot, so a reinsertion or a
//! reset invalidates it whatever the ATR says.
//!
//! EVERY HANDOFF RESETS THE CARD. However a transaction ends - released, cancelled, its lease out, its
//! owner gone - the slot is powered off and on and the PIV application selected again before the next
//! owner is admitted, because a card's authentication state can outlive a clean release. An operation
//! still in flight is aborted first, and the reset waits for the provider to say the slot is
//! quiescent. Abort and reset each have five seconds; past either, the slot is UNAVAILABLE until its
//! provider proves recovery by itself, and nobody is handed a slot in an unknown state.
//!
//! ONE WINNER PER REQUEST. A client's operation is answered exactly once: by the provider's reply, by
//! its deadline, by removal or by cancellation - whichever the serialized loop sees first - and a
//! provider reply for a request that is no longer the slot's current one is dropped.
//!
//! Time is in the system's 100 Hz ticks.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::piv::{self, Continued, Next, PinStatus, Refused};

pub const MAX_READERS: usize = 8;
pub const MAX_SLOTS: u8 = 4;
pub const MAX_GRANTS: usize = 32;
/// Transactions and queued acquisitions together.
pub const MAX_TRANSACTIONS: usize = 32;
pub const MAX_WAITERS: usize = 8;
/// A lease is at most sixty seconds and is never renewed.
pub const LEASE_TICKS: u64 = 6000;
pub const APDU_TICKS: u64 = 1000;
pub const PINPAD_TICKS: u64 = 3000;
/// Abort, and reset, each.
pub const RECOVERY_TICKS: u64 = 500;
pub const MAX_ATR: usize = 33;

/// A client request waiting for its answer: the grant it arrived on and its correlation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Call {
	pub grant: u32,
	pub corr: u32,
}

/// What a grant may do.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Operations {
	pub read: bool,
	pub transact: bool,
	pub authenticate: bool,
}

/// What a reader told the service about itself, reduced to what the decisions need.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReaderSpec {
	pub slots: u8,
	/// Short-APDU exchange: the only level this service drives.
	pub short_apdu: bool,
	/// A secure-verification pinpad whose format fits the PIV template.
	pub pinpad: bool,
	/// Bit n: protocol T=n.
	pub protocols: u8,
}

/// A slot as the provider reported it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SlotReport {
	pub slot: u8,
	pub present: bool,
	pub generation: u64,
	pub atr: Vec<u8>,
}

/// A slot as clients see it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SlotView {
	pub slot: u8,
	pub present: bool,
	pub generation: u64,
	pub atr: Vec<u8>,
	pub available: bool,
}

/// What the service asks a provider to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Request {
	PowerOff,
	PowerOn,
	Protocol(u8),
	Exchange(Vec<u8>),
	Verify,
	Abort,
}

/// How a provider request ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProviderOutcome {
	Done,
	CardRemoved,
	Aborted,
	TimedOut,
	Cancelled,
	Fault,
}

/// A provider's reply.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Answer {
	pub request: u64,
	pub slot: u8,
	pub generation: u64,
	pub outcome: ProviderOutcome,
	pub response: Vec<u8>,
	pub atr: Vec<u8>,
	/// What `atr` offers, as the shared ATR parser read it - `None` when it did not parse. Read by the caller
	/// with `smartcard_model::atr::parse`, because this crate is also a shared library and imports nothing a
	/// staged library does not publish.
	pub offer: Option<Offer>,
}

/// The protocols an answer-to-reset offers (bit n set: T=n), and the one it starts in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Offer {
	pub protocols: u16,
	pub first: u8,
}

impl Offer {
	fn offers(&self, protocol: u8) -> bool {
		protocol < 16 && self.protocols & (1 << protocol) != 0
	}
}

/// How an operation that reached the card ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
	Done,
	CardRemoved,
	Cancelled,
	TimedOut,
	ProviderError,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Exchanged {
	pub outcome: Outcome,
	pub data: Vec<u8>,
	pub sw: Option<(u8, u8)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verified {
	Verified,
	Incorrect(u8),
	Blocked,
	Cancelled,
	TimedOut,
	CardRemoved,
	ProviderError,
	TrustedInputUnavailable,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Authenticated {
	pub outcome: Outcome,
	pub signature: Option<Vec<u8>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Acquired {
	pub transaction: u64,
	pub slot: u8,
	pub generation: u64,
	pub lease_ticks: u64,
}

/// Why a request was refused before it reached a card.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// The grant does not carry the operation, or the transaction is somebody else's.
	Denied,
	/// Not in the allowlist, or a mode this service does not drive.
	Unsupported,
	/// Malformed: a slot the reader lacks, a command whose lengths do not add up, a wrong-sized challenge.
	Invalid,
	/// No such transaction, or no card in the slot.
	NotFound,
	/// The transaction's card or reset epoch is no longer the slot's.
	Stale,
	/// The slot already has an operation in flight.
	Busy,
	/// A bound was reached: the queue, or the service's transactions.
	Exhausted,
	/// The acquisition waited out its deadline without touching the card.
	TimedOut,
	/// The slot could not be proven quiescent and waits for its provider.
	Unavailable,
	/// The reader is gone.
	Closed,
}

/// What the service has to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Effect {
	Send {
		reader: u32,
		request: u64,
		slot: u8,
		generation: u64,
		what: Request,
	},
	Acquired {
		call: Call,
		result: Result<Acquired, Refusal>,
	},
	Exchanged {
		call: Call,
		result: Result<Exchanged, Refusal>,
	},
	Verified {
		call: Call,
		result: Result<Verified, Refusal>,
	},
	Authenticated {
		call: Call,
		result: Result<Authenticated, Refusal>,
	},
	/// A presence or availability change clients of this reader are told about.
	Event {
		reader: u32,
		kind: EventKind,
		slot: u8,
	},
	/// A grant whose reader went: close its connection.
	CloseGrant(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventKind {
	Inserted,
	Removed,
	Unavailable,
	Available,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
	PowerOff,
	PowerOn,
	Protocol,
	Select,
}

enum Work {
	Exchange { call: Call, continued: Continued },
	Verify { call: Call },
	Authenticate { call: Call, continued: Continued },
}

enum Phase {
	Absent,
	Resetting { step: Step, request: u64, deadline: u64, continued: Continued },
	Idle,
	Owned { txn: u64 },
	InFlight { txn: u64, request: u64, deadline: u64, work: Work },
	Cancelling { request: u64, deadline: u64 },
	Unavailable,
}

struct Waiter {
	call: Call,
	deadline: u64,
	lease: u64,
}

struct Slot {
	present: bool,
	generation: u64,
	atr: Vec<u8>,
	epoch: u64,
	phase: Phase,
	waiters: VecDeque<Waiter>,
}

struct Reader {
	key: u32,
	spec: ReaderSpec,
	slots: Vec<Slot>,
}

struct Grant {
	id: u32,
	reader: u32,
	operations: Operations,
}

struct Transaction {
	id: u64,
	grant: u32,
	reader: u32,
	slot: u8,
	generation: u64,
	epoch: u64,
	lease_end: u64,
	verified: bool,
}

pub struct Cards {
	readers: Vec<Reader>,
	grants: Vec<Grant>,
	transactions: Vec<Transaction>,
	next_transaction: u64,
	next_request: u64,
}

impl Default for Cards {
	fn default() -> Self {
		Self::new()
	}
}

impl Cards {
	pub fn new() -> Self {
		Self { readers: Vec::new(), grants: Vec::new(), transactions: Vec::new(), next_transaction: 1, next_request: 1 }
	}

	fn request(&mut self) -> u64 {
		let id = self.next_request;
		self.next_request += 1;
		id
	}

	fn reader_at(&self, key: u32) -> Option<usize> {
		self.readers.iter().position(|reader| reader.key == key)
	}

	fn waiting(&self) -> usize {
		self.readers.iter().flat_map(|reader| reader.slots.iter()).map(|slot| slot.waiters.len()).sum()
	}

	// ------------------------------------------------------------------ readers

	/// A reader whose provider session opened with every slot powered off. REFUSED past eight readers
	/// or four slots - a larger advertisement is refused, never truncated.
	pub fn add_reader(&mut self, key: u32, spec: ReaderSpec, reports: &[SlotReport], now: u64) -> Result<Vec<Effect>, Refusal> {
		if self.readers.len() >= MAX_READERS || spec.slots == 0 || spec.slots > MAX_SLOTS || self.reader_at(key).is_some() {
			return Err(Refusal::Exhausted);
		}
		let slots = (0..spec.slots).map(|slot| {
			let report = reports.iter().find(|report| report.slot == slot);
			let present = report.is_some_and(|report| report.present);
			Slot { present, generation: report.map_or(0, |report| report.generation), atr: Vec::new(), epoch: 0, phase: Phase::Absent, waiters: VecDeque::new() }
		});
		self.readers.push(Reader { key, spec, slots: slots.collect() });
		let at = self.readers.len() - 1;
		let mut effects = Vec::new();
		for slot in 0..spec.slots {
			if self.readers[at].slots[slot as usize].present && spec.short_apdu {
				self.start_reset(at, slot, now, &mut effects);
			}
		}
		Ok(effects)
	}

	/// A reader withdrawn or disconnected: every transaction on it ends, every waiter and every
	/// operation in flight is answered `closed`, and every grant to it is closed.
	pub fn remove_reader(&mut self, key: u32) -> Vec<Effect> {
		let mut effects = Vec::new();
		let Some(at) = self.reader_at(key) else { return effects };
		let reader = self.readers.remove(at);
		for slot in reader.slots {
			for waiter in slot.waiters {
				effects.push(Effect::Acquired { call: waiter.call, result: Err(Refusal::Closed) });
			}
			if let Phase::InFlight { work, .. } = slot.phase {
				effects.push(refuse_work(work, Refusal::Closed));
			}
		}
		self.transactions.retain(|transaction| transaction.reader != key);
		for grant in self.grants.iter().filter(|grant| grant.reader == key) {
			effects.push(Effect::CloseGrant(grant.id));
		}
		self.grants.retain(|grant| grant.reader != key);
		effects
	}

	pub fn has_reader(&self, key: u32) -> bool {
		self.reader_at(key).is_some()
	}

	pub fn spec(&self, key: u32) -> Option<ReaderSpec> {
		self.reader_at(key).map(|at| self.readers[at].spec)
	}

	/// The reader's slots as clients see them.
	pub fn slots(&self, key: u32) -> Vec<SlotView> {
		let Some(at) = self.reader_at(key) else { return Vec::new() };
		self.readers[at].slots.iter().enumerate().map(|(index, slot)| view(index as u8, slot)).collect()
	}

	// ------------------------------------------------------------------ grants

	/// A minted connection, bound to one reader and one set of operations. False at the limit.
	pub fn add_grant(&mut self, id: u32, reader: u32, operations: Operations) -> bool {
		if self.grants.len() >= MAX_GRANTS || self.reader_at(reader).is_none() {
			return false;
		}
		self.grants.push(Grant { id, reader, operations });
		true
	}

	pub fn grant_reader(&self, id: u32) -> Option<u32> {
		self.grants.iter().find(|grant| grant.id == id).map(|grant| grant.reader)
	}

	pub fn grant_count(&self) -> usize {
		self.grants.len()
	}

	pub fn transaction_count(&self) -> usize {
		self.transactions.len() + self.waiting()
	}

	/// A grant's owner died or its connection closed: its transactions end - an operation in flight is
	/// aborted - and its queued acquisitions go, unanswered, with it.
	pub fn drop_grant(&mut self, id: u32, now: u64) -> Vec<Effect> {
		let mut effects = Vec::new();
		for reader in &mut self.readers {
			for slot in &mut reader.slots {
				slot.waiters.retain(|waiter| waiter.call.grant != id);
			}
		}
		let owned: Vec<u64> = self.transactions.iter().filter(|transaction| transaction.grant == id).map(|transaction| transaction.id).collect();
		for transaction in owned {
			self.end(transaction, Outcome::Cancelled, now, &mut effects);
		}
		self.grants.retain(|grant| grant.id != id);
		effects
	}

	fn operations(&self, grant: u32) -> Option<(u32, Operations)> {
		self.grants.iter().find(|held| held.id == grant).map(|held| (held.reader, held.operations))
	}

	// ------------------------------------------------------------------ acquisition

	/// Acquire a slot: at once when it is idle and nobody is queued, behind at most eight others
	/// otherwise, never past the service's transaction bound.
	pub fn acquire(&mut self, call: Call, slot: u8, wait_ticks: u64, lease_ticks: u64, now: u64) -> Vec<Effect> {
		let mut effects = Vec::new();
		let refuse = |effects: &mut Vec<Effect>, refusal| effects.push(Effect::Acquired { call, result: Err(refusal) });
		let Some((reader_key, operations)) = self.operations(call.grant) else {
			refuse(&mut effects, Refusal::Denied);
			return effects;
		};
		if !operations.transact {
			refuse(&mut effects, Refusal::Denied);
			return effects;
		}
		let Some(at) = self.reader_at(reader_key) else {
			refuse(&mut effects, Refusal::Closed);
			return effects;
		};
		let spec = self.readers[at].spec;
		if !spec.short_apdu {
			refuse(&mut effects, Refusal::Unsupported);
			return effects;
		}
		if slot >= spec.slots {
			refuse(&mut effects, Refusal::Invalid);
			return effects;
		}
		let lease = if lease_ticks == 0 { LEASE_TICKS } else { lease_ticks.min(LEASE_TICKS) };
		let (unavailable, present, free, queued) = {
			let held = &self.readers[at].slots[slot as usize];
			(matches!(held.phase, Phase::Unavailable), held.present, matches!(held.phase, Phase::Idle) && held.waiters.is_empty(), held.waiters.len())
		};
		if unavailable {
			refuse(&mut effects, Refusal::Unavailable);
			return effects;
		}
		if !present {
			refuse(&mut effects, Refusal::NotFound);
			return effects;
		}
		if self.transaction_count() >= MAX_TRANSACTIONS {
			refuse(&mut effects, Refusal::Exhausted);
			return effects;
		}
		if free {
			self.grant_slot(at, slot, call, lease, now, &mut effects);
			return effects;
		}
		if wait_ticks == 0 {
			refuse(&mut effects, Refusal::TimedOut);
			return effects;
		}
		if queued >= MAX_WAITERS {
			refuse(&mut effects, Refusal::Exhausted);
			return effects;
		}
		self.readers[at].slots[slot as usize].waiters.push_back(Waiter { call, deadline: now + wait_ticks, lease });
		effects
	}

	fn grant_slot(&mut self, at: usize, slot: u8, call: Call, lease: u64, now: u64, effects: &mut Vec<Effect>) {
		let id = self.next_transaction;
		self.next_transaction += 1;
		let reader = self.readers[at].key;
		let held = &mut self.readers[at].slots[slot as usize];
		held.phase = Phase::Owned { txn: id };
		self.transactions.push(Transaction { id, grant: call.grant, reader, slot, generation: held.generation, epoch: held.epoch, lease_end: now + lease, verified: false });
		effects.push(Effect::Acquired { call, result: Ok(Acquired { transaction: id, slot, generation: held.generation, lease_ticks: lease }) });
	}

	// The next waiter whose deadline has not passed gets an idle slot.
	fn serve_waiters(&mut self, at: usize, slot: u8, now: u64, effects: &mut Vec<Effect>) {
		while matches!(self.readers[at].slots[slot as usize].phase, Phase::Idle) {
			let Some(waiter) = self.readers[at].slots[slot as usize].waiters.pop_front() else { return };
			if waiter.deadline <= now {
				effects.push(Effect::Acquired { call: waiter.call, result: Err(Refusal::TimedOut) });
				continue;
			}
			self.grant_slot(at, slot, waiter.call, waiter.lease, now, effects);
		}
	}

	// ------------------------------------------------------------------ operations

	// The transaction, checked: it exists, the calling grant owns it and carries the operation, and its
	// card and reset epoch are still the slot's.
	fn checked(&self, call: Call, transaction: u64, needs: fn(&Operations) -> bool) -> Result<(usize, u8), Refusal> {
		let Some(held) = self.transactions.iter().find(|held| held.id == transaction) else { return Err(Refusal::NotFound) };
		if held.grant != call.grant {
			return Err(Refusal::Denied);
		}
		let Some((_, operations)) = self.operations(call.grant) else { return Err(Refusal::Denied) };
		if !needs(&operations) {
			return Err(Refusal::Denied);
		}
		let Some(at) = self.reader_at(held.reader) else { return Err(Refusal::Closed) };
		let slot = &self.readers[at].slots[held.slot as usize];
		if slot.generation != held.generation || slot.epoch != held.epoch || !slot.present {
			return Err(Refusal::Stale);
		}
		match slot.phase {
			Phase::Owned { txn } if txn == transaction => Ok((at, held.slot)),
			Phase::InFlight { txn, .. } if txn == transaction => Err(Refusal::Busy),
			_ => Err(Refusal::Stale),
		}
	}

	fn dispatch(&mut self, at: usize, slot: u8, txn: u64, what: Request, deadline: u64, work: Work, effects: &mut Vec<Effect>) {
		let request = self.request();
		let reader = self.readers[at].key;
		let held = &mut self.readers[at].slots[slot as usize];
		held.phase = Phase::InFlight { txn, request, deadline, work };
		effects.push(Effect::Send { reader, request, slot, generation: held.generation, what });
	}

	fn lease_end(&self, transaction: u64) -> u64 {
		self.transactions.iter().find(|held| held.id == transaction).map_or(0, |held| held.lease_end)
	}

	/// One command a client asked to send: the allowlist first, then the transaction, then the card.
	pub fn exchange(&mut self, call: Call, transaction: u64, apdu: &[u8], now: u64) -> Vec<Effect> {
		let mut effects = Vec::new();
		let (at, slot) = match self.checked(call, transaction, |operations| operations.transact) {
			Ok(found) => found,
			Err(refusal) => {
				effects.push(Effect::Exchanged { call, result: Err(refusal) });
				return effects;
			}
		};
		if let Err(refused) = piv::classify(apdu) {
			let refusal = match refused {
				Refused::Forbidden => Refusal::Denied,
				Refused::Unsupported => Refusal::Unsupported,
				Refused::Malformed => Refusal::Invalid,
			};
			effects.push(Effect::Exchanged { call, result: Err(refusal) });
			return effects;
		}
		let deadline = (now + APDU_TICKS).min(self.lease_end(transaction));
		self.dispatch(at, slot, transaction, Request::Exchange(apdu.to_vec()), deadline, Work::Exchange { call, continued: Continued::new() }, &mut effects);
		effects
	}

	/// PIN verification on the reader's own keypad. One attempt, never retried; no PIN anywhere.
	pub fn verify(&mut self, call: Call, transaction: u64, timeout_ticks: u64, now: u64) -> Vec<Effect> {
		let mut effects = Vec::new();
		let (at, slot) = match self.checked(call, transaction, |operations| operations.authenticate) {
			Ok(found) => found,
			Err(refusal) => {
				effects.push(Effect::Verified { call, result: Err(refusal) });
				return effects;
			}
		};
		if !self.readers[at].spec.pinpad {
			effects.push(Effect::Verified { call, result: Ok(Verified::TrustedInputUnavailable) });
			return effects;
		}
		let timeout = if timeout_ticks == 0 { PINPAD_TICKS } else { timeout_ticks.min(PINPAD_TICKS) };
		let deadline = (now + timeout).min(self.lease_end(transaction));
		self.dispatch(at, slot, transaction, Request::Verify, deadline, Work::Verify { call }, &mut effects);
		effects
	}

	/// A P-256 signature over a 32-byte challenge, after a verification in this same transaction.
	pub fn authenticate(&mut self, call: Call, transaction: u64, challenge: &[u8], now: u64) -> Vec<Effect> {
		let mut effects = Vec::new();
		let (at, slot) = match self.checked(call, transaction, |operations| operations.authenticate) {
			Ok(found) => found,
			Err(refusal) => {
				effects.push(Effect::Authenticated { call, result: Err(refusal) });
				return effects;
			}
		};
		let Ok(challenge) = <&[u8; piv::CHALLENGE_BYTES]>::try_from(challenge) else {
			effects.push(Effect::Authenticated { call, result: Err(Refusal::Invalid) });
			return effects;
		};
		if !self.transactions.iter().any(|held| held.id == transaction && held.verified) {
			effects.push(Effect::Authenticated { call, result: Err(Refusal::Denied) });
			return effects;
		}
		let deadline = (now + APDU_TICKS).min(self.lease_end(transaction));
		self.dispatch(at, slot, transaction, Request::Exchange(piv::general_authenticate(challenge).to_vec()), deadline, Work::Authenticate { call, continued: Continued::new() }, &mut effects);
		effects
	}

	/// Release or cancel: the transaction ends at once, an operation in flight is aborted and answered
	/// `cancelled`, and the slot is reset before anybody else is admitted. The answer to the caller
	/// itself is immediate.
	pub fn finish(&mut self, call: Call, transaction: u64, now: u64) -> Result<Vec<Effect>, Refusal> {
		let Some(held) = self.transactions.iter().find(|held| held.id == transaction) else { return Err(Refusal::NotFound) };
		if held.grant != call.grant {
			return Err(Refusal::Denied);
		}
		let mut effects = Vec::new();
		self.end(transaction, Outcome::Cancelled, now, &mut effects);
		Ok(effects)
	}

	// End a transaction. The slot moves to cancelling (an operation in flight) or straight to a reset.
	fn end(&mut self, transaction: u64, outcome: Outcome, now: u64, effects: &mut Vec<Effect>) {
		let Some(index) = self.transactions.iter().position(|held| held.id == transaction) else { return };
		let held = self.transactions.remove(index);
		let Some(at) = self.reader_at(held.reader) else { return };
		let slot = held.slot;
		let phase = core::mem::replace(&mut self.readers[at].slots[slot as usize].phase, Phase::Absent);
		match phase {
			Phase::InFlight { txn, work, .. } if txn == transaction => {
				effects.push(answer_work(work, outcome));
				self.start_abort(at, slot, now, effects);
			}
			Phase::Owned { txn } if txn == transaction => self.start_reset(at, slot, now, effects),
			other => self.readers[at].slots[slot as usize].phase = other,
		}
	}

	// ------------------------------------------------------------------ recovery

	fn start_abort(&mut self, at: usize, slot: u8, now: u64, effects: &mut Vec<Effect>) {
		let request = self.request();
		let reader = self.readers[at].key;
		let held = &mut self.readers[at].slots[slot as usize];
		held.phase = Phase::Cancelling { request, deadline: now + RECOVERY_TICKS };
		effects.push(Effect::Send { reader, request, slot, generation: held.generation, what: Request::Abort });
	}

	// Power off, power on, the protocol, and SELECT: the slot's reset, from the top. A slot with no card
	// has nothing to reset.
	fn start_reset(&mut self, at: usize, slot: u8, now: u64, effects: &mut Vec<Effect>) {
		if !self.readers[at].slots[slot as usize].present {
			self.readers[at].slots[slot as usize].phase = Phase::Absent;
			return;
		}
		let request = self.request();
		let reader = self.readers[at].key;
		let held = &mut self.readers[at].slots[slot as usize];
		held.epoch += 1;
		held.phase = Phase::Resetting { step: Step::PowerOff, request, deadline: now + RECOVERY_TICKS, continued: Continued::new() };
		effects.push(Effect::Send { reader, request, slot, generation: held.generation, what: Request::PowerOff });
	}

	fn unavailable(&mut self, at: usize, slot: u8, effects: &mut Vec<Effect>) {
		let reader = self.readers[at].key;
		let held = &mut self.readers[at].slots[slot as usize];
		held.phase = Phase::Unavailable;
		// ITS WAITERS FAIL: nothing will hand them this slot until its provider recovers it.
		for waiter in held.waiters.drain(..) {
			effects.push(Effect::Acquired { call: waiter.call, result: Err(Refusal::Unavailable) });
		}
		self.transactions.retain(|transaction| !(transaction.reader == reader && transaction.slot == slot));
		effects.push(Effect::Event { reader, kind: EventKind::Unavailable, slot });
	}

	// ------------------------------------------------------------------ what providers say

	/// A provider's reply. One for a request that is not the slot's current one is dropped.
	pub fn answered(&mut self, reader: u32, answer: Answer, now: u64) -> Vec<Effect> {
		let mut effects = Vec::new();
		let Some(at) = self.reader_at(reader) else { return effects };
		let slot = answer.slot;
		if slot >= self.readers[at].spec.slots {
			return effects;
		}
		let current = match &self.readers[at].slots[slot as usize].phase {
			Phase::Resetting { request, .. } | Phase::InFlight { request, .. } | Phase::Cancelling { request, .. } => *request,
			_ => return effects,
		};
		if answer.request != current {
			return effects;
		}
		let phase = core::mem::replace(&mut self.readers[at].slots[slot as usize].phase, Phase::Absent);
		match phase {
			Phase::Cancelling { .. } => {
				// QUIESCENT: the provider says the abort is complete, whatever the aborted request's own
				// ending was. Only now may the slot be reset.
				if matches!(answer.outcome, ProviderOutcome::Fault) {
					self.unavailable(at, slot, &mut effects);
				} else {
					self.start_reset(at, slot, now, &mut effects);
				}
			}
			Phase::Resetting { step, deadline, mut continued, .. } => self.reset_step(at, slot, step, deadline, &mut continued, answer, now, &mut effects),
			Phase::InFlight { txn, deadline, work, .. } => self.work_answered(at, slot, txn, deadline, work, answer, now, &mut effects),
			other => self.readers[at].slots[slot as usize].phase = other,
		}
		effects
	}

	fn reset_step(&mut self, at: usize, slot: u8, step: Step, deadline: u64, continued: &mut Continued, answer: Answer, now: u64, effects: &mut Vec<Effect>) {
		match answer.outcome {
			ProviderOutcome::Done => {}
			ProviderOutcome::CardRemoved => {
				self.readers[at].slots[slot as usize].phase = Phase::Absent;
				return;
			}
			_ => {
				self.unavailable(at, slot, effects);
				return;
			}
		}
		let reader = self.readers[at].key;
		let next = |cards: &mut Cards, step: Step, what: Request, continued: Continued, effects: &mut Vec<Effect>| {
			let request = cards.request();
			let held = &mut cards.readers[at].slots[slot as usize];
			held.phase = Phase::Resetting { step, request, deadline, continued };
			effects.push(Effect::Send { reader, request, slot, generation: held.generation, what });
		};
		match step {
			Step::PowerOff => next(self, Step::PowerOn, Request::PowerOn, Continued::new(), effects),
			Step::PowerOn => {
				// THE ATR THE CARD GAVE, validated by the same parser every provider uses. A card that
				// is not a card, or offers no protocol this reader speaks, cannot be used.
				let spec = self.readers[at].spec;
				let protocol = match answer.offer {
					Some(atr) if atr.offers(atr.first) && spec.protocols & (1 << atr.first) != 0 => Some(atr.first),
					Some(atr) => [0u8, 1].into_iter().find(|&candidate| atr.offers(candidate) && spec.protocols & (1 << candidate) != 0),
					None => None,
				};
				let Some(protocol) = protocol else {
					self.unavailable(at, slot, effects);
					return;
				};
				self.readers[at].slots[slot as usize].atr = answer.atr.clone();
				next(self, Step::Protocol, Request::Protocol(protocol), Continued::new(), effects);
			}
			Step::Protocol => next(self, Step::Select, Request::Exchange(piv::SELECT_PIV.to_vec()), Continued::new(), effects),
			Step::Select => match continued.feed(&answer.response) {
				Next::More(get_response) => {
					let taken = core::mem::take(continued);
					next(self, Step::Select, Request::Exchange(get_response.to_vec()), taken, effects);
				}
				// SELECTED OR NOT, THE CARD IS RESET: a card without the PIV application is still a card a
				// client may read the ATR of, and its own SELECT will say what the card said here.
				Next::Complete(..) | Next::Refused => {
					self.readers[at].slots[slot as usize].phase = Phase::Idle;
					self.serve_waiters(at, slot, now, effects);
				}
			},
		}
	}

	fn work_answered(&mut self, at: usize, slot: u8, txn: u64, deadline: u64, work: Work, answer: Answer, now: u64, effects: &mut Vec<Effect>) {
		match answer.outcome {
			ProviderOutcome::Done => {}
			ProviderOutcome::CardRemoved => {
				effects.push(answer_work(work, Outcome::CardRemoved));
				self.card_gone(at, slot, now, effects);
				return;
			}
			ProviderOutcome::TimedOut | ProviderOutcome::Cancelled if matches!(work, Work::Verify { .. }) => {
				let Work::Verify { call } = work else { unreachable!() };
				let result = if answer.outcome == ProviderOutcome::TimedOut { Verified::TimedOut } else { Verified::Cancelled };
				effects.push(Effect::Verified { call, result: Ok(result) });
				self.readers[at].slots[slot as usize].phase = Phase::Owned { txn };
				return;
			}
			_ => {
				// A FAULT LEAVES THE CARD IN A STATE NOBODY KNOWS: answered as a provider error, the
				// transaction ends, and the slot is aborted and reset like any other ending.
				effects.push(answer_work(work, Outcome::ProviderError));
				self.transactions.retain(|held| held.id != txn);
				self.start_abort(at, slot, now, effects);
				return;
			}
		}
		let reader = self.readers[at].key;
		let continue_with = |cards: &mut Cards, work: Work, get_response: [u8; 5], effects: &mut Vec<Effect>| {
			let request = cards.request();
			let held = &mut cards.readers[at].slots[slot as usize];
			held.phase = Phase::InFlight { txn, request, deadline, work };
			effects.push(Effect::Send { reader, request, slot, generation: held.generation, what: Request::Exchange(get_response.to_vec()) });
		};
		match work {
			Work::Exchange { call, mut continued } => match continued.feed(&answer.response) {
				Next::More(get_response) => continue_with(self, Work::Exchange { call, continued }, get_response, effects),
				Next::Complete(sw1, sw2) => {
					effects.push(Effect::Exchanged { call, result: Ok(Exchanged { outcome: Outcome::Done, data: continued.into_data(), sw: Some((sw1, sw2)) }) });
					self.readers[at].slots[slot as usize].phase = Phase::Owned { txn };
				}
				Next::Refused => {
					effects.push(Effect::Exchanged { call, result: Ok(Exchanged { outcome: Outcome::ProviderError, data: Vec::new(), sw: None }) });
					self.readers[at].slots[slot as usize].phase = Phase::Owned { txn };
				}
			},
			Work::Verify { call } => {
				let result = match piv::split_status(&answer.response).map(|(_, sw1, sw2)| piv::pin_status(sw1, sw2)) {
					Some(PinStatus::Verified) => {
						if let Some(held) = self.transactions.iter_mut().find(|held| held.id == txn) {
							held.verified = true;
						}
						Verified::Verified
					}
					Some(PinStatus::Incorrect { retries }) => Verified::Incorrect(retries),
					Some(PinStatus::Blocked) => Verified::Blocked,
					Some(PinStatus::TimedOut) => Verified::TimedOut,
					Some(PinStatus::Cancelled) => Verified::Cancelled,
					Some(PinStatus::Error) | None => Verified::ProviderError,
				};
				effects.push(Effect::Verified { call, result: Ok(result) });
				self.readers[at].slots[slot as usize].phase = Phase::Owned { txn };
			}
			Work::Authenticate { call, mut continued } => match continued.feed(&answer.response) {
				Next::More(get_response) => continue_with(self, Work::Authenticate { call, continued }, get_response, effects),
				Next::Complete(0x90, 0x00) => {
					let result = match piv::authentication_signature(continued.data()) {
						Ok(signature) => Ok(Authenticated { outcome: Outcome::Done, signature: Some(signature) }),
						Err(_) => Ok(Authenticated { outcome: Outcome::ProviderError, signature: None }),
					};
					effects.push(Effect::Authenticated { call, result });
					self.readers[at].slots[slot as usize].phase = Phase::Owned { txn };
				}
				Next::Complete(sw1, sw2) => {
					// THE CARD'S REFUSAL, TYPED: an algorithm or key it does not have is unsupported, a
					// security condition it does not see met is a denial.
					let result = match (sw1, sw2) {
						(0x6a, 0x80) | (0x6a, 0x81) | (0x6a, 0x86) | (0x6a, 0x88) => Err(Refusal::Unsupported),
						(0x69, 0x82) => Err(Refusal::Denied),
						_ => Ok(Authenticated { outcome: Outcome::ProviderError, signature: None }),
					};
					effects.push(Effect::Authenticated { call, result });
					self.readers[at].slots[slot as usize].phase = Phase::Owned { txn };
				}
				Next::Refused => {
					effects.push(Effect::Authenticated { call, result: Ok(Authenticated { outcome: Outcome::ProviderError, signature: None }) });
					self.readers[at].slots[slot as usize].phase = Phase::Owned { txn };
				}
			},
		}
	}

	// The card this slot held is gone: its transactions end, its generation is dead, and the transport
	// is drained - an abort, awaited - before a reinserted card can be reset.
	fn card_gone(&mut self, at: usize, slot: u8, now: u64, effects: &mut Vec<Effect>) {
		let reader = self.readers[at].key;
		self.transactions.retain(|held| !(held.reader == reader && held.slot == slot));
		self.start_abort(at, slot, now, effects);
	}

	/// A presence change the provider reported. A REMOVAL AND A REINSERTION ARE TWO EVENTS, and a new
	/// card is a new generation even with the same ATR.
	pub fn presence(&mut self, reader: u32, report: SlotReport, now: u64) -> Vec<Effect> {
		let mut effects = Vec::new();
		let Some(at) = self.reader_at(reader) else { return effects };
		let slot = report.slot;
		if slot >= self.readers[at].spec.slots {
			return effects;
		}
		let held = &self.readers[at].slots[slot as usize];
		let removed = held.present && (!report.present || report.generation != held.generation);
		let inserted = report.present && (!held.present || report.generation != held.generation);
		if removed {
			let phase = core::mem::replace(&mut self.readers[at].slots[slot as usize].phase, Phase::Absent);
			self.readers[at].slots[slot as usize].present = false;
			self.readers[at].slots[slot as usize].atr.clear();
			effects.push(Effect::Event { reader, kind: EventKind::Removed, slot });
			match phase {
				Phase::InFlight { work, .. } => {
					effects.push(answer_work(work, Outcome::CardRemoved));
					self.card_gone(at, slot, now, &mut effects);
				}
				// Already draining, or waiting on its provider: that continues whatever the card did.
				Phase::Cancelling { request, deadline } => self.readers[at].slots[slot as usize].phase = Phase::Cancelling { request, deadline },
				Phase::Unavailable => self.readers[at].slots[slot as usize].phase = Phase::Unavailable,
				// A reset in progress was for the card that left: abort whatever it had outstanding.
				Phase::Resetting { .. } => self.card_gone(at, slot, now, &mut effects),
				Phase::Owned { .. } | Phase::Idle | Phase::Absent => {
					let reader_key = self.readers[at].key;
					self.transactions.retain(|held| !(held.reader == reader_key && held.slot == slot));
				}
			}
		}
		if inserted {
			let held = &mut self.readers[at].slots[slot as usize];
			held.present = true;
			held.generation = report.generation;
			effects.push(Effect::Event { reader, kind: EventKind::Inserted, slot });
			// A DRAINING SLOT STAYS DRAINING: the reset that admits the new card comes after the abort.
			if matches!(held.phase, Phase::Absent) {
				self.start_reset(at, slot, now, &mut effects);
			}
		}
		effects
	}

	/// The provider reports a fault on a slot: whatever it was doing is over and nothing about it is
	/// known, so the slot waits for the provider's own recovery.
	pub fn fault(&mut self, reader: u32, slot: u8) -> Vec<Effect> {
		let mut effects = Vec::new();
		let Some(at) = self.reader_at(reader) else { return effects };
		if slot >= self.readers[at].spec.slots {
			return effects;
		}
		let phase = core::mem::replace(&mut self.readers[at].slots[slot as usize].phase, Phase::Absent);
		if let Phase::InFlight { work, .. } = phase {
			effects.push(answer_work(work, Outcome::ProviderError));
		}
		self.unavailable(at, slot, &mut effects);
		effects
	}

	/// The provider proves a slot quiescent after a recovery this service could not complete: it is reset
	/// again, and then available.
	pub fn quiescent(&mut self, reader: u32, slot: u8, now: u64) -> Vec<Effect> {
		let mut effects = Vec::new();
		let Some(at) = self.reader_at(reader) else { return effects };
		if slot >= self.readers[at].spec.slots || !matches!(self.readers[at].slots[slot as usize].phase, Phase::Unavailable) {
			return effects;
		}
		effects.push(Effect::Event { reader, kind: EventKind::Available, slot });
		self.readers[at].slots[slot as usize].phase = Phase::Absent;
		self.start_reset(at, slot, now, &mut effects);
		effects
	}

	// ------------------------------------------------------------------ time

	/// Deadlines: acquisitions that waited too long, leases, operations in flight, aborts and resets.
	pub fn tick(&mut self, now: u64) -> Vec<Effect> {
		let mut effects = Vec::new();
		for at in 0..self.readers.len() {
			for slot in 0..self.readers[at].slots.len() {
				let expired: Vec<Waiter> = {
					let waiters = &mut self.readers[at].slots[slot].waiters;
					let (keep, gone): (VecDeque<Waiter>, VecDeque<Waiter>) = waiters.drain(..).partition(|waiter| waiter.deadline > now);
					*waiters = keep;
					gone.into_iter().collect()
				};
				// WITHOUT TOUCHING THE CARD: an acquisition that timed out never reached it.
				for waiter in expired {
					effects.push(Effect::Acquired { call: waiter.call, result: Err(Refusal::TimedOut) });
				}
			}
		}
		// Leases, which end their transaction whatever it is doing.
		let lapsed: Vec<u64> = self.transactions.iter().filter(|held| held.lease_end <= now).map(|held| held.id).collect();
		for transaction in lapsed {
			self.end(transaction, Outcome::TimedOut, now, &mut effects);
		}
		for at in 0..self.readers.len() {
			for slot in 0..self.readers[at].slots.len() as u8 {
				let due = match &self.readers[at].slots[slot as usize].phase {
					Phase::InFlight { deadline, .. } | Phase::Cancelling { deadline, .. } | Phase::Resetting { deadline, .. } => *deadline <= now,
					_ => false,
				};
				if !due {
					continue;
				}
				let phase = core::mem::replace(&mut self.readers[at].slots[slot as usize].phase, Phase::Absent);
				match phase {
					Phase::InFlight { txn, work, .. } => {
						// THE DEADLINE WINS: answered as timed out, the transaction ends, the operation is
						// aborted and the card reset.
						let answered = match work {
							Work::Verify { call } => Effect::Verified { call, result: Ok(Verified::TimedOut) },
							other => answer_work(other, Outcome::TimedOut),
						};
						effects.push(answered);
						self.transactions.retain(|held| held.id != txn);
						self.start_abort(at, slot, now, &mut effects);
					}
					// NEITHER ABORT NOR RESET MAY BE ASSUMED: past five seconds without proof, the slot is
					// unavailable until its provider proves it quiescent.
					Phase::Cancelling { .. } | Phase::Resetting { .. } => self.unavailable(at, slot, &mut effects),
					other => self.readers[at].slots[slot as usize].phase = other,
				}
			}
		}
		effects
	}

	/// When `tick` next has something to do.
	pub fn next_deadline(&self) -> Option<u64> {
		let phases = self.readers.iter().flat_map(|reader| reader.slots.iter()).filter_map(|slot| match &slot.phase {
			Phase::InFlight { deadline, .. } | Phase::Cancelling { deadline, .. } | Phase::Resetting { deadline, .. } => Some(*deadline),
			_ => None,
		});
		let waiters = self.readers.iter().flat_map(|reader| reader.slots.iter()).flat_map(|slot| slot.waiters.iter().map(|waiter| waiter.deadline));
		let leases = self.transactions.iter().map(|held| held.lease_end);
		phases.chain(waiters).chain(leases).min()
	}
}

/// The most card events waiting for one client.
pub const MAX_EVENTS: usize = 16;

/// One client's pending card events. NOTHING IS COALESCED - a removal and a reinsertion are two events
/// - and past sixteen the stream is to be CLOSED: the client takes a new snapshot, and nothing a slow
/// reader does holds up a transaction's removal or cancellation, which never wait on this queue.
pub struct EventQueue<T> {
	queue: VecDeque<T>,
	overflowed: bool,
}

impl<T> Default for EventQueue<T> {
	fn default() -> Self {
		Self { queue: VecDeque::new(), overflowed: false }
	}
}

impl<T> EventQueue<T> {
	/// Queue one event; false once the queue has overflowed, and for ever after.
	pub fn push(&mut self, event: T) -> bool {
		if self.overflowed || self.queue.len() >= MAX_EVENTS {
			self.overflowed = true;
			self.queue.clear();
			return false;
		}
		self.queue.push_back(event);
		true
	}

	pub fn front(&self) -> Option<&T> {
		self.queue.front()
	}

	pub fn pop(&mut self) -> Option<T> {
		self.queue.pop_front()
	}

	pub fn overflowed(&self) -> bool {
		self.overflowed
	}

	pub fn len(&self) -> usize {
		self.queue.len()
	}

	pub fn is_empty(&self) -> bool {
		self.queue.is_empty()
	}
}

fn view(slot: u8, held: &Slot) -> SlotView {
	SlotView { slot, present: held.present, generation: held.generation, atr: held.atr.clone(), available: !matches!(held.phase, Phase::Unavailable) }
}

// The answer an interrupted operation gets.
fn answer_work(work: Work, outcome: Outcome) -> Effect {
	match work {
		Work::Exchange { call, .. } => Effect::Exchanged { call, result: Ok(Exchanged { outcome, data: Vec::new(), sw: None }) },
		Work::Verify { call } => Effect::Verified {
			call,
			result: Ok(match outcome {
				Outcome::CardRemoved => Verified::CardRemoved,
				Outcome::TimedOut => Verified::TimedOut,
				Outcome::Cancelled => Verified::Cancelled,
				Outcome::Done | Outcome::ProviderError => Verified::ProviderError,
			}),
		},
		Work::Authenticate { call, .. } => Effect::Authenticated { call, result: Ok(Authenticated { outcome, signature: None }) },
	}
}

fn refuse_work(work: Work, refusal: Refusal) -> Effect {
	match work {
		Work::Exchange { call, .. } => Effect::Exchanged { call, result: Err(refusal) },
		Work::Verify { call } => Effect::Verified { call, result: Err(refusal) },
		Work::Authenticate { call, .. } => Effect::Authenticated { call, result: Err(refusal) },
	}
}

#[cfg(test)]
mod tests;
