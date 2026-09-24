//! POWERSERVICE'S STATE: the sources its providers publish, the subscribers reading them, and the one
//! control each provider may have outstanding - every decision the service makes, as a function of
//! what arrived and what time it is.
//!
//! ONE SERIALISED LOOP OWNS ALL OF IT, so a subscriber's snapshot and its first change are separated
//! by nothing: the snapshot is taken at the current revision, the subscriber is registered in the same
//! step, and every change after it carries a larger revision.
//!
//! EVERY QUEUE IS BOUNDED AND NOBODY WAITS. A provider is never blocked: its frames are taken as they
//! come and anything wrong with one ends the provider, with removals for everything it published. A
//! subscriber gets at most `QUEUE_RECORDS` records waiting for it; an ordinary measurement coalesces
//! into the latest state of its source and is emitted at most once per `COALESCE_TICKS`; an addition,
//! a removal and an alarm transition are never coalesced away, and when they fill the queue - or the
//! reader has taken nothing for `DRAIN_TICKS` - the subscription is CLOSED. A closed subscription is
//! the one way this service says continuity was lost; a revision gap is not.
//!
//! A CONTROL IS NEVER RETRIED. One is outstanding per provider, for at most `CONTROL_TICKS`. A reply
//! that does not arrive in time, or a provider that goes while one is outstanding, completes it as
//! INDETERMINATE - the provider may have acted - and leaves its source uncertain until a fresh state
//! query reconciles it. A reconciliation that also goes unanswered leaves the source's controls
//! unavailable; the rest of the service, and every other provider, carry on.
//!
//! Time is in the system's 100 Hz ticks throughout.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// The most sources the whole service holds.
pub const MAX_SOURCES: usize = 128;
/// The most providers it holds a connection to.
pub const MAX_PROVIDERS: usize = 8;
/// The most public subscriptions it serves at once.
pub const MAX_SUBSCRIBERS: usize = 16;
/// The most sources one provider describes, and so the range of its local identities.
pub const MAX_LOCAL_SOURCES: u32 = 16;
/// The most change records waiting for one subscriber.
pub const QUEUE_RECORDS: usize = 32;
/// An ordinary measurement is emitted at most once per 100 ms per source.
pub const COALESCE_TICKS: u64 = 10;
/// A subscriber that takes nothing for five seconds while records wait is closed.
pub const DRAIN_TICKS: u64 = 500;
/// A provider has five seconds to finish its initial snapshot.
pub const SNAPSHOT_TICKS: u64 = 500;
/// A control exchange, and a reconciliation query, has five seconds.
pub const CONTROL_TICKS: u64 = 500;
/// The longest turn-off a caller may schedule: a day.
pub const MAX_DELAY_SECONDS: u32 = 86_400;

/// What the registry needs to know about a state it otherwise only stores.
pub trait Payload: Clone {
	/// Whether moving from `before` to `self` changes an alarm.
	fn alarm_transition(&self, before: &Self) -> bool;
	/// The controls this state advertises.
	fn controls(&self) -> Controls;
}

/// What a source's provider will do if asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Controls {
	pub set_output: bool,
	pub schedule_off: bool,
	pub cancel_off: bool,
	pub outlets: u8,
}

/// A publication as DeviceManager identified it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct Publication {
	pub slot: u32,
	pub generation: u32,
	pub binding_generation: u64,
}

/// A source's identity: its publication and the provider's own local number for it. A replacement
/// publication has another generation, so its sources are other sources.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct SourceKey {
	pub slot: u32,
	pub generation: u32,
	pub binding_generation: u64,
	pub local: u32,
}

/// The service's own handle on one provider connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProviderId(pub u32);

/// The service's own handle on one subscription.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SubscriberId(pub u32);

/// One frame of a provider's stream, decoded.
#[derive(Clone, PartialEq, Debug)]
pub enum Frame<T> {
	Snapshot { revision: u64, local: u32, state: T },
	SnapshotEnd { revision: u64 },
	Added { revision: u64, local: u32, state: T },
	Updated { revision: u64, local: u32, state: T },
	Removed { revision: u64, local: u32 },
}

/// Why a provider's frame ended the provider.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// A local identity outside the provider's range.
	Local,
	/// A source described twice, or one that was removed described again.
	Duplicate,
	/// An update or removal for a source the provider never added.
	Unregistered,
	/// A revision not after the last one.
	Regression,
	/// A snapshot frame after the snapshot ended, or a change before it did.
	Order,
	/// Another frame for a provider that has already failed or gone.
	Gone,
}

/// One change, as a subscriber receives it.
#[derive(Clone, PartialEq, Debug)]
pub enum Change<T> {
	Added { key: SourceKey, revision: u64, received: u64, state: T },
	Updated { key: SourceKey, revision: u64, received: u64, state: T },
	Removed { key: SourceKey, revision: u64 },
}

impl<T> Change<T> {
	pub fn key(&self) -> SourceKey {
		match self {
			Change::Added { key, .. } | Change::Updated { key, .. } | Change::Removed { key, .. } => *key,
		}
	}
}

/// Why a subscription was closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Closed {
	/// Records that may not be coalesced filled its queue.
	Overflow,
	/// Its reader took nothing for `DRAIN_TICKS` while records waited.
	Stalled,
}

/// A control an operator asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
	SetOutput { outlet: u8, on: bool },
	ScheduleOff { delay_seconds: u32 },
	CancelOff,
}

/// Why a control was refused before anything was sent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlRefusal {
	/// The identity names no live source: forged, stale, or from a publication that is gone.
	Denied,
	/// The source does not advertise that control.
	Unsupported,
	/// An outlet it does not have, or a delay past a day.
	Invalid,
	/// Its provider has a control outstanding, or the source is being reconciled.
	Busy,
	/// Reconciliation went unanswered, so what the source's controls did is not known. A fresh query
	/// has been started if none was running.
	Unavailable,
}

/// A control to send: the provider, the correlation it is sent under, the source's local identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Dispatch {
	pub provider: ProviderId,
	pub corr: u32,
	pub local: u32,
}

/// Something the service has to do because time passed or a provider went.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
	/// A provider missed its snapshot deadline and was ended: close its connection.
	ProviderFailed(ProviderId),
	/// The control sent under `corr` completes as INDETERMINATE: answer its operator so.
	Indeterminate { corr: u32 },
	/// Ask this provider for the fresh state of one source, under `corr`.
	Query(Dispatch),
}

enum Phase<T> {
	Snapshot { since: u64, revision: Option<u64>, pending: Vec<(u32, T)> },
	Live { revision: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Control {
	Idle,
	Pending { corr: u32, local: u32, deadline: u64 },
	Reconciling { corr: u32, local: u32, deadline: u64 },
}

struct Provider<T> {
	id: ProviderId,
	publication: Publication,
	phase: Phase<T>,
	// Local identities in use, retired and refused, one bit each.
	live: u16,
	retired: u16,
	refused: u16,
	control: Control,
}

struct Source<T> {
	key: SourceKey,
	provider: ProviderId,
	state: T,
	received: u64,
	// A control on this source may have acted without saying so.
	uncertain: bool,
	// And reconciling that went unanswered too.
	unavailable: bool,
}

struct Queued<T> {
	change: Change<T>,
	// Not to be coalesced away: an addition, a removal, an alarm transition.
	pinned: bool,
}

struct Subscriber<T> {
	id: SubscriberId,
	queue: VecDeque<Queued<T>>,
	emitted: Vec<(SourceKey, u64)>,
	blocked_since: Option<u64>,
	closed: Option<Closed>,
}

/// What admitting a provider's snapshot came to: sources beyond the service's limit are refused one
/// by one and reported, never made room for by evicting somebody else's.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Admitted {
	pub sources: usize,
	pub exhausted: usize,
}

pub struct Registry<T> {
	epoch: u64,
	revision: u64,
	providers: Vec<Provider<T>>,
	sources: Vec<Source<T>>,
	subscribers: Vec<Subscriber<T>>,
	// Reconciliation queries a refused control started, for the next `tick` to hand out.
	queries: Vec<Dispatch>,
	next_corr: u32,
	next_subscriber: u32,
}

impl<T: Payload> Registry<T> {
	pub fn new(epoch: u64) -> Self {
		Self { epoch, revision: 0, providers: Vec::new(), sources: Vec::new(), subscribers: Vec::new(), queries: Vec::new(), next_corr: 1, next_subscriber: 1 }
	}

	pub fn epoch(&self) -> u64 {
		self.epoch
	}

	pub fn revision(&self) -> u64 {
		self.revision
	}

	// ------------------------------------------------------------------ providers

	/// Whether another provider fits.
	pub fn provider_room(&self) -> bool {
		self.providers.len() < MAX_PROVIDERS
	}

	/// A provider connection opened: it has `SNAPSHOT_TICKS` to describe itself. False at the limit or
	/// for a publication already held.
	pub fn add_provider(&mut self, id: ProviderId, publication: Publication, now: u64) -> bool {
		if !self.provider_room() || self.providers.iter().any(|provider| provider.publication == publication || provider.id == id) {
			return false;
		}
		self.providers.push(Provider { id, publication, phase: Phase::Snapshot { since: now, revision: None, pending: Vec::new() }, live: 0, retired: 0, refused: 0, control: Control::Idle });
		true
	}

	pub fn has_provider(&self, id: ProviderId) -> bool {
		self.providers.iter().any(|provider| provider.id == id)
	}

	/// The provider holding a publication.
	pub fn provider_of(&self, publication: Publication) -> Option<ProviderId> {
		self.providers.iter().find(|provider| provider.publication == publication).map(|provider| provider.id)
	}

	/// One frame from a provider. `Err` ends the provider - the caller closes its connection and calls
	/// `remove_provider`, which publishes the removals.
	pub fn frame(&mut self, id: ProviderId, frame: Frame<T>, now: u64) -> Result<Admitted, Refusal> {
		let Some(at) = self.providers.iter().position(|provider| provider.id == id) else { return Err(Refusal::Gone) };
		let local_of = |frame: &Frame<T>| match frame {
			Frame::Snapshot { local, .. } | Frame::Added { local, .. } | Frame::Updated { local, .. } | Frame::Removed { local, .. } => Some(*local),
			Frame::SnapshotEnd { .. } => None,
		};
		if local_of(&frame).is_some_and(|local| local >= MAX_LOCAL_SOURCES) {
			return Err(Refusal::Local);
		}
		match frame {
			Frame::Snapshot { revision, local, state } => {
				let Phase::Snapshot { revision: seen, pending, .. } = &mut self.providers[at].phase else { return Err(Refusal::Order) };
				if seen.is_some_and(|seen| seen != revision) {
					return Err(Refusal::Order);
				}
				if pending.iter().any(|(held, _)| *held == local) {
					return Err(Refusal::Duplicate);
				}
				*seen = Some(revision);
				pending.push((local, state));
				Ok(Admitted::default())
			}
			Frame::SnapshotEnd { revision } => {
				let Phase::Snapshot { revision: seen, pending, .. } = &mut self.providers[at].phase else { return Err(Refusal::Order) };
				if seen.is_some_and(|seen| seen != revision) {
					return Err(Refusal::Order);
				}
				let pending = core::mem::take(pending);
				self.providers[at].phase = Phase::Live { revision };
				let mut admitted = Admitted::default();
				for (local, state) in pending {
					if self.admit(at, local, state, now) {
						admitted.sources += 1;
					} else {
						admitted.exhausted += 1;
					}
				}
				Ok(admitted)
			}
			Frame::Added { revision, local, state } => {
				self.advance(at, revision)?;
				let bit = 1u16 << local;
				let provider = &self.providers[at];
				if (provider.live | provider.retired | provider.refused) & bit != 0 {
					return Err(Refusal::Duplicate);
				}
				if self.admit(at, local, state, now) { Ok(Admitted { sources: 1, exhausted: 0 }) } else { Ok(Admitted { sources: 0, exhausted: 1 }) }
			}
			Frame::Updated { revision, local, state } => {
				self.advance(at, revision)?;
				let bit = 1u16 << local;
				let provider = &self.providers[at];
				// A source refused at the limit stays refused: its updates go nowhere, and they are not
				// the provider's mistake.
				if provider.refused & bit != 0 {
					return Ok(Admitted::default());
				}
				if provider.live & bit == 0 {
					return Err(Refusal::Unregistered);
				}
				let key = self.key(at, local);
				self.update(key, state, now);
				Ok(Admitted::default())
			}
			Frame::Removed { revision, local } => {
				self.advance(at, revision)?;
				let bit = 1u16 << local;
				let provider = &mut self.providers[at];
				if provider.refused & bit != 0 {
					provider.refused &= !bit;
					provider.retired |= bit;
					return Ok(Admitted::default());
				}
				if provider.live & bit == 0 {
					return Err(Refusal::Unregistered);
				}
				provider.live &= !bit;
				provider.retired |= bit;
				let key = self.key(at, local);
				self.remove_source(key);
				Ok(Admitted::default())
			}
		}
	}

	fn advance(&mut self, at: usize, revision: u64) -> Result<(), Refusal> {
		match &mut self.providers[at].phase {
			Phase::Snapshot { .. } => Err(Refusal::Order),
			Phase::Live { revision: last } => {
				if revision <= *last {
					return Err(Refusal::Regression);
				}
				*last = revision;
				Ok(())
			}
		}
	}

	fn key(&self, at: usize, local: u32) -> SourceKey {
		let publication = self.providers[at].publication;
		SourceKey { slot: publication.slot, generation: publication.generation, binding_generation: publication.binding_generation, local }
	}

	// A new source, or a refusal at the service's limit - which is remembered, so the provider's later
	// frames for it are not mistaken for protocol errors.
	fn admit(&mut self, at: usize, local: u32, state: T, now: u64) -> bool {
		let bit = 1u16 << local;
		if self.sources.len() >= MAX_SOURCES {
			self.providers[at].refused |= bit;
			return false;
		}
		self.providers[at].live |= bit;
		let key = self.key(at, local);
		let provider = self.providers[at].id;
		let place = self.sources.partition_point(|source| source.key < key);
		self.sources.insert(place, Source { key, provider, state: state.clone(), received: now, uncertain: false, unavailable: false });
		self.revision += 1;
		let change = Change::Added { key, revision: self.revision, received: now, state };
		self.broadcast(change, true);
		true
	}

	fn update(&mut self, key: SourceKey, state: T, now: u64) {
		let Some(source) = self.sources.iter_mut().find(|source| source.key == key) else { return };
		let transition = state.alarm_transition(&source.state);
		source.state = state.clone();
		source.received = now;
		self.revision += 1;
		let change = Change::Updated { key, revision: self.revision, received: now, state };
		self.broadcast(change, transition);
	}

	fn remove_source(&mut self, key: SourceKey) {
		let Some(at) = self.sources.iter().position(|source| source.key == key) else { return };
		self.sources.remove(at);
		self.revision += 1;
		let change = Change::Removed { key, revision: self.revision };
		self.broadcast(change, true);
	}

	/// A provider's connection closed, its publication was withdrawn, or it broke the protocol:
	/// EVERYTHING IT PUBLISHED GOES, with a removal each - the last healthy value never stays live -
	/// and an outstanding control completes as indeterminate.
	pub fn remove_provider(&mut self, id: ProviderId) -> Vec<Effect> {
		let mut effects = Vec::new();
		let Some(at) = self.providers.iter().position(|provider| provider.id == id) else { return effects };
		let provider = self.providers.remove(at);
		if let Control::Pending { corr, .. } = provider.control {
			effects.push(Effect::Indeterminate { corr });
		}
		let keys: Vec<SourceKey> = self.sources.iter().filter(|source| source.provider == id).map(|source| source.key).collect();
		for key in keys {
			self.remove_source(key);
		}
		effects
	}

	// ------------------------------------------------------------------ reading

	/// Every live source, sorted by identity: publication slot, generation, local number.
	pub fn sources(&self) -> Vec<(SourceKey, u64, T)> {
		self.sources.iter().map(|source| (source.key, source.received, source.state.clone())).collect()
	}

	pub fn source_count(&self) -> usize {
		self.sources.len()
	}

	// ------------------------------------------------------------------ subscribers

	/// Whether another subscriber fits.
	pub fn subscriber_room(&self) -> bool {
		self.subscribers.len() < MAX_SUBSCRIBERS
	}

	/// Register a subscriber whose snapshot - `sources()` - the caller has just delivered. Every change
	/// after this carries a revision larger than `revision()` read in the same step.
	pub fn subscribe(&mut self) -> Option<SubscriberId> {
		if !self.subscriber_room() {
			return None;
		}
		let id = SubscriberId(self.next_subscriber);
		self.next_subscriber = self.next_subscriber.wrapping_add(1).max(1);
		self.subscribers.push(Subscriber { id, queue: VecDeque::new(), emitted: Vec::new(), blocked_since: None, closed: None });
		Some(id)
	}

	/// A subscriber went, or was closed and its channel with it: everything charged to it is released.
	pub fn unsubscribe(&mut self, id: SubscriberId) {
		self.subscribers.retain(|subscriber| subscriber.id != id);
	}

	pub fn subscriber_count(&self) -> usize {
		self.subscribers.len()
	}

	/// How many records wait for a subscriber.
	pub fn queued(&self, id: SubscriberId) -> usize {
		self.subscribers.iter().find(|subscriber| subscriber.id == id).map_or(0, |subscriber| subscriber.queue.len())
	}

	fn broadcast(&mut self, change: Change<T>, pinned: bool) {
		for subscriber in &mut self.subscribers {
			if subscriber.closed.is_some() {
				continue;
			}
			let key = change.key();
			match &change {
				// AN ORDINARY MEASUREMENT REPLACES THE ONE STILL WAITING for its source, and moves to
				// the back with its newer revision - so the queue stays in revision order.
				Change::Updated { .. } if !pinned => {
					if let Some(at) = subscriber.queue.iter().position(|queued| !queued.pinned && matches!(queued.change, Change::Updated { .. }) && queued.change.key() == key) {
						subscriber.queue.remove(at);
					}
				}
				// A removal supersedes a measurement nobody has read yet. It does not supersede an
				// addition or an alarm transition: those are the history a subscriber was promised.
				Change::Removed { .. } => {
					subscriber.queue.retain(|queued| queued.pinned || queued.change.key() != key);
					subscriber.emitted.retain(|(emitted, _)| *emitted != key);
				}
				_ => {}
			}
			if subscriber.queue.len() >= QUEUE_RECORDS {
				subscriber.closed = Some(Closed::Overflow);
				subscriber.queue.clear();
				continue;
			}
			subscriber.queue.push_back(Queued { change: change.clone(), pinned });
		}
	}

	/// Hand a subscriber what may go now, in order, through `send` - which answers false when the
	/// channel will not take it. An ordinary measurement waits until its source's last emission is
	/// `COALESCE_TICKS` old, and everything behind it waits with it. `Err` means the subscription is
	/// closed: the caller closes its channel and calls `unsubscribe`.
	pub fn drain(&mut self, id: SubscriberId, now: u64, mut send: impl FnMut(&Change<T>) -> bool) -> Result<(), Closed> {
		let Some(subscriber) = self.subscribers.iter_mut().find(|subscriber| subscriber.id == id) else { return Ok(()) };
		if let Some(closed) = subscriber.closed {
			return Err(closed);
		}
		while let Some(head) = subscriber.queue.front() {
			let key = head.change.key();
			if !head.pinned
				&& let Some((_, at)) = subscriber.emitted.iter().find(|(emitted, _)| *emitted == key)
				&& now < at + COALESCE_TICKS
			{
				break;
			}
			if !send(&head.change) {
				let since = *subscriber.blocked_since.get_or_insert(now);
				if now.saturating_sub(since) >= DRAIN_TICKS {
					subscriber.closed = Some(Closed::Stalled);
					subscriber.queue.clear();
					return Err(Closed::Stalled);
				}
				return Ok(());
			}
			subscriber.blocked_since = None;
			let sent = subscriber.queue.pop_front();
			if matches!(sent.map(|queued| queued.change), Some(Change::Removed { .. })) {
				continue;
			}
			match subscriber.emitted.iter_mut().find(|(emitted, _)| *emitted == key) {
				Some(entry) => entry.1 = now,
				None => subscriber.emitted.push((key, now)),
			}
		}
		if subscriber.queue.is_empty() {
			subscriber.blocked_since = None;
		}
		Ok(())
	}

	/// The subscribers with records waiting or a closure to report, for the loop to drain.
	pub fn pending_subscribers(&self) -> Vec<SubscriberId> {
		self.subscribers.iter().filter(|subscriber| subscriber.closed.is_some() || !subscriber.queue.is_empty()).map(|subscriber| subscriber.id).collect()
	}

	// ------------------------------------------------------------------ controls

	/// An operator's control, checked against the live source and what it advertises. `Ok` is the
	/// dispatch to send; the operator is answered when `control_answered` or an `Effect` completes it.
	pub fn control(&mut self, key: SourceKey, action: Action, now: u64) -> Result<Dispatch, ControlRefusal> {
		let Some(source) = self.sources.iter().find(|source| source.key == key) else { return Err(ControlRefusal::Denied) };
		let controls = source.state.controls();
		match action {
			Action::SetOutput { outlet, .. } => {
				if !controls.set_output {
					return Err(ControlRefusal::Unsupported);
				}
				if outlet >= controls.outlets {
					return Err(ControlRefusal::Invalid);
				}
			}
			Action::ScheduleOff { delay_seconds } => {
				if !controls.schedule_off {
					return Err(ControlRefusal::Unsupported);
				}
				if delay_seconds > MAX_DELAY_SECONDS {
					return Err(ControlRefusal::Invalid);
				}
			}
			Action::CancelOff => {
				if !controls.cancel_off {
					return Err(ControlRefusal::Unsupported);
				}
			}
		}
		let (provider_id, unavailable, uncertain) = (source.provider, source.unavailable, source.uncertain);
		let Some(provider) = self.providers.iter().position(|provider| provider.id == provider_id) else { return Err(ControlRefusal::Denied) };
		if uncertain || unavailable {
			// ONLY A QUERY'S ANSWER SETTLES A SOURCE a control may have changed. If none is outstanding
			// - its deadline passed, or the provider refused it - a fresh one is started, which is how
			// a source's controls come back once its provider answers again. Never the control itself.
			if self.providers[provider].control == Control::Idle {
				let corr = self.corr();
				self.providers[provider].control = Control::Reconciling { corr, local: key.local, deadline: now + CONTROL_TICKS };
				self.queries.push(Dispatch { provider: provider_id, corr, local: key.local });
			}
			return Err(if unavailable { ControlRefusal::Unavailable } else { ControlRefusal::Busy });
		}
		if self.providers[provider].control != Control::Idle {
			return Err(ControlRefusal::Busy);
		}
		let corr = self.corr();
		self.providers[provider].control = Control::Pending { corr, local: key.local, deadline: now + CONTROL_TICKS };
		Ok(Dispatch { provider: provider_id, corr, local: key.local })
	}

	fn corr(&mut self) -> u32 {
		let corr = self.next_corr;
		self.next_corr = self.next_corr.wrapping_add(1).max(1);
		corr
	}

	/// The provider answered the control sent under `corr`. True when that is the control outstanding
	/// - the caller relays the answer - and false for anything else: a late reply after the deadline
	/// already completed it is dropped, not delivered twice.
	pub fn control_answered(&mut self, id: ProviderId, corr: u32) -> bool {
		let Some(provider) = self.providers.iter_mut().find(|provider| provider.id == id) else { return false };
		match provider.control {
			Control::Pending { corr: outstanding, .. } if outstanding == corr => {
				provider.control = Control::Idle;
				true
			}
			_ => false,
		}
	}

	/// The provider answered the reconciliation query sent under `corr`, with the source's fresh state
	/// or with a refusal. Fresh state is published like any update and settles the source; a refusal
	/// leaves its controls unavailable. False for a reply that is not the one outstanding.
	pub fn query_answered(&mut self, id: ProviderId, corr: u32, fresh: Option<T>, now: u64) -> bool {
		let Some(at) = self.providers.iter().position(|provider| provider.id == id) else { return false };
		let Control::Reconciling { corr: outstanding, local, .. } = self.providers[at].control else { return false };
		if outstanding != corr {
			return false;
		}
		self.providers[at].control = Control::Idle;
		let key = self.key(at, local);
		match fresh {
			Some(state) => {
				self.update(key, state, now);
				if let Some(source) = self.sources.iter_mut().find(|source| source.key == key) {
					source.uncertain = false;
					source.unavailable = false;
				}
			}
			None => {
				if let Some(source) = self.sources.iter_mut().find(|source| source.key == key) {
					source.unavailable = true;
				}
			}
		}
		true
	}

	/// What time passing requires: snapshots that did not finish, controls that were not answered,
	/// reconciliations that were not either - and queries a refusal started.
	pub fn tick(&mut self, now: u64) -> Vec<Effect> {
		let mut effects: Vec<Effect> = self.queries.drain(..).map(Effect::Query).collect();
		let late: Vec<ProviderId> = self.providers.iter().filter(|provider| matches!(provider.phase, Phase::Snapshot { since, .. } if now.saturating_sub(since) >= SNAPSHOT_TICKS)).map(|provider| provider.id).collect();
		for id in late {
			effects.extend(self.remove_provider(id));
			effects.push(Effect::ProviderFailed(id));
		}
		for at in 0..self.providers.len() {
			match self.providers[at].control {
				Control::Pending { corr, local, deadline } if now >= deadline => {
					// INDETERMINATE, NOT RETRIED. The pending slot is released, the source is uncertain,
					// and the provider is asked what is true now.
					effects.push(Effect::Indeterminate { corr });
					let key = self.key(at, local);
					if let Some(source) = self.sources.iter_mut().find(|source| source.key == key) {
						source.uncertain = true;
					}
					let query = self.corr();
					self.providers[at].control = Control::Reconciling { corr: query, local, deadline: now + CONTROL_TICKS };
					effects.push(Effect::Query(Dispatch { provider: self.providers[at].id, corr: query, local }));
				}
				Control::Reconciling { local, deadline, .. } if now >= deadline => {
					self.providers[at].control = Control::Idle;
					let key = self.key(at, local);
					if let Some(source) = self.sources.iter_mut().find(|source| source.key == key) {
						source.uncertain = true;
						source.unavailable = true;
					}
				}
				_ => {}
			}
		}
		effects
	}

	/// When `tick` or a `drain` next has something to do, if anything waits on time at all.
	pub fn next_deadline(&self) -> Option<u64> {
		let snapshots = self.providers.iter().filter_map(|provider| match provider.phase {
			Phase::Snapshot { since, .. } => Some(since + SNAPSHOT_TICKS),
			Phase::Live { .. } => None,
		});
		let controls = self.providers.iter().filter_map(|provider| match provider.control {
			Control::Pending { deadline, .. } | Control::Reconciling { deadline, .. } => Some(deadline),
			Control::Idle => None,
		});
		let subscribers = self.subscribers.iter().filter_map(|subscriber| {
			let head = subscriber.queue.front()?;
			let coalesced = if head.pinned { None } else { subscriber.emitted.iter().find(|(key, _)| *key == head.change.key()).map(|(_, at)| at + COALESCE_TICKS) };
			let stalled = subscriber.blocked_since.map(|since| since + DRAIN_TICKS);
			match (coalesced, stalled) {
				(Some(a), Some(b)) => Some(a.min(b)),
				(a, b) => a.or(b),
			}
		});
		snapshots.chain(controls).chain(subscribers).min()
	}
}

#[cfg(test)]
mod tests;
