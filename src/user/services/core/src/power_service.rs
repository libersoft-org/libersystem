// PowerService - normalised power-source and thermal state, observed by anyone granted the read
// authority and controlled, where a device advertises a control, by the one component granted the
// operator authority.
//
// WHAT IT IS NOT. Rebooting and powering off the machine stay with SystemManager's `system-power`, and
// this service holds no client of it. Turning a UPS's output off turns the UPS's output off. Suspend,
// fan curves and platform power policy are not here at all: an alarm is an observation.
//
// WHERE ITS STATE COMES FROM. Drivers publish `power-source` providers through their own bindings;
// this service is the one consumer of that kind, through a catalogue connection minted for it alone,
// and never hands a provider connection to anybody. A provider's sources arrive ALREADY NORMALISED -
// the drivers call `power_model`'s adapters - so this service converts nothing: it checks each record
// is canonical and refuses the provider that sends one that is not.
//
// EVERY DECISION IS `service_logic::power_registry`'s: admission, ordering, coalescing, queue bounds,
// closure, control deadlines and reconciliation, all host-tested as functions of what arrived and what
// time it is. What is here is the IO around them - and the rule that nothing in it waits on anybody:
// a provider's reply, a subscriber's reading, an operator's patience. The loop blocks in one place,
// `wait_any`, and everything else is taken or refused as it comes.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{ChangeKind, ControlOutcome, Error, PowerChange, ProviderCommand, ProviderCommandKind, ProviderInfo, ProviderKind, ProviderUpdateKind, SourceId, SourceSnapshot, SourceState, power, power_control, power_provider, provider_catalogue};
use rt::*;
use service_logic::power_registry::{Action, Change, ControlRefusal, Controls, Dispatch, Effect, Frame, Payload, ProviderId, Publication, Registry, SourceKey, SubscriberId};
use wire::Sink;

include!(concat!(env!("OUT_DIR"), "/roles_power_service.rs"));

// Client connections minted from the two roots, together.
const MAX_CLIENTS: usize = 32;
// How deep a subscriber's channel is PAST its snapshot: a few live frames, so that what coalesces and
// what overflows is this service's own bounded queue, whose rules are the registry's, and not the
// kernel's.
const LIVE_DEPTH: u64 = 8;
// How long a provider has to answer the request that opens its update stream.
const OPEN_TICKS: u64 = 100;
// The largest frame this service writes: one source with every trip and alarm it may carry fits in
// well under a kilobyte.
const FRAME_BYTES: usize = 2048;

// A normalised state, as the registry holds it.
#[derive(Clone)]
struct Held(SourceState);

impl Payload for Held {
	fn alarm_transition(&self, before: &Self) -> bool {
		power_model::canon::alarm_transition(&before.0, &self.0)
	}
	fn controls(&self) -> Controls {
		let advertised = &self.0.controls;
		Controls { set_output: advertised.set_output, schedule_off: advertised.schedule_off, cancel_off: advertised.cancel_off, outlets: advertised.outlets }
	}
}

// Which interface a client connection speaks. The ROOT it was minted from decides, and nothing the
// client sends can change it: a read connection has no control operation to smuggle one through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interface {
	State,
	Control,
}

struct Client {
	chan: u64,
	interface: Interface,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sent {
	Command,
	Query,
}

struct Provider {
	id: ProviderId,
	chan: u64,
	stream: u64,
	// What this service asked and has not had answered, so a reply can be read for what it is - and a
	// late one, for a control the deadline already completed, dropped rather than delivered twice.
	sent: Vec<(u32, Sent)>,
}

// An operator waiting for a control to complete.
struct Waiting {
	corr: u32,
	chan: u64,
	request: u32,
}

struct Subscriber {
	id: SubscriberId,
	chan: u64,
	seq: u32,
}

struct Power {
	registry: Registry<Held>,
	providers: Vec<Provider>,
	subscribers: Vec<Subscriber>,
	waiting: Vec<Waiting>,
	next_provider: u32,
}

// ------------------------------------------------------------------ identities

fn source_id(key: SourceKey) -> SourceId {
	SourceId { slot: key.slot, generation: key.generation, binding_generation: key.binding_generation, local: key.local }
}

fn key_of(id: &SourceId) -> SourceKey {
	SourceKey { slot: id.slot, generation: id.generation, binding_generation: id.binding_generation, local: id.local }
}

fn publication_of(info: &ProviderInfo) -> Publication {
	Publication { slot: info.slot, generation: info.provider_generation, binding_generation: info.binding_generation }
}

fn snapshot_of(key: SourceKey, received: u64, state: &SourceState) -> SourceSnapshot {
	SourceSnapshot { id: source_id(key), received, state: state.clone() }
}

fn change_frame(epoch: u64, change: &Change<Held>) -> PowerChange {
	match change {
		Change::Added { key, revision, received, state } => PowerChange { epoch, revision: *revision, kind: ChangeKind::Added, source: Some(snapshot_of(*key, *received, &state.0)), gone: None },
		Change::Updated { key, revision, received, state } => PowerChange { epoch, revision: *revision, kind: ChangeKind::Updated, source: Some(snapshot_of(*key, *received, &state.0)), gone: None },
		Change::Removed { key, revision } => PowerChange { epoch, revision: *revision, kind: ChangeKind::Removed, source: None, gone: Some(source_id(*key)) },
	}
}

// ------------------------------------------------------------------ replies written by hand

// `result<T, error>` under a correlation, the shape every generated reply has - written here because
// a control's answer is sent long after the request was decoded, which the generated dispatch cannot.
fn reply_result(chan: u64, corr: u32, result: Result<ControlOutcome, Error>) {
	let mut writer = wire::VecWriter::new();
	let encoded = (|| {
		writer.u32(corr)?;
		match result {
			Ok(outcome) => {
				writer.u8(1)?;
				outcome.write(&mut writer)
			}
			Err(error) => {
				writer.u8(0)?;
				error.write(&mut writer)
			}
		}
	})();
	if encoded.is_some()
		&& let Some(bytes) = writer.into_inner()
	{
		// NEVER WAITED ON. An operator that stopped reading its own connection is not a reason for this
		// service to stop.
		let _ = try_send(chan, &bytes, 0);
	}
}

fn refusal_error(refusal: ControlRefusal) -> Error {
	match refusal {
		ControlRefusal::Denied => Error::Denied,
		ControlRefusal::Unsupported => Error::Unsupported,
		ControlRefusal::Invalid => Error::Invalid,
		ControlRefusal::Busy => Error::Again,
		ControlRefusal::Unavailable => Error::Io,
	}
}

// A request to a provider, sent without waiting for anything: `op`, the correlation, the body.
fn send_request(chan: u64, op: u16, corr: u32, body: impl FnOnce(&mut wire::VecWriter) -> Option<()>) -> bool {
	let mut writer = wire::VecWriter::new();
	if writer.u16(op).is_none() || writer.u32(corr).is_none() || body(&mut writer).is_none() {
		return false;
	}
	match writer.into_inner() {
		Some(bytes) => try_send(chan, &bytes, 0),
		None => false,
	}
}

// ------------------------------------------------------------------ the views the generated code calls

// The read interface, for `sources`. `subscribe` is a stream and is answered in `subscribe` below; the
// generated opener only validates the request through this.
struct StateView<'a> {
	registry: &'a Registry<Held>,
}

impl power::Service for StateView<'_> {
	fn sources(&mut self) -> Result<Vec<SourceSnapshot>, Error> {
		Ok(self.registry.sources().iter().map(|(key, received, held)| snapshot_of(*key, *received, &held.0)).collect())
	}
	fn subscribe(&mut self) -> Result<Vec<PowerChange>, Error> {
		Ok(Vec::new())
	}
}

// The control interface, DECODED by the generated dispatch and ANSWERED later: this records what was
// asked, and the placeholder reply dispatch writes is discarded.
struct ControlView {
	asked: Option<(SourceId, Action)>,
}

impl power_control::Service for ControlView {
	fn set_output(&mut self, source: SourceId, outlet: u8, on: bool) -> Result<ControlOutcome, Error> {
		self.asked = Some((source, Action::SetOutput { outlet, on }));
		Err(Error::Again)
	}
	fn schedule_output_off(&mut self, source: SourceId, delay_seconds: u32) -> Result<ControlOutcome, Error> {
		self.asked = Some((source, Action::ScheduleOff { delay_seconds }));
		Err(Error::Again)
	}
	fn cancel_output_off(&mut self, source: SourceId) -> Result<ControlOutcome, Error> {
		self.asked = Some((source, Action::CancelOff));
		Err(Error::Again)
	}
}

// ------------------------------------------------------------------ providers

impl Power {
	// A publication arrived: open it, open its update stream, and give it SNAPSHOT_TICKS to describe
	// itself. BEYOND THE PROVIDER LIMIT IT IS REFUSED AND SAID, never admitted by evicting another.
	fn adopt(&mut self, catalogue: u64, info: ProviderInfo) {
		let now = clock();
		if !self.registry.provider_room() {
			print(b"PowerService: a power-source provider was refused: this service holds eight (resource exhausted)\n");
			return;
		}
		if self.registry.provider_of(publication_of(&info)).is_some() {
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) else {
			print(b"PowerService: a published power source could not be opened\n");
			return;
		};
		let stream = match power_provider::Client::with_deadline(ChannelTransport { chan }, now + OPEN_TICKS).updates() {
			Some(stream) => stream,
			None => {
				print(b"PowerService: a power-source provider did not open its update stream\n");
				close(chan);
				return;
			}
		};
		let id = ProviderId(self.next_provider);
		self.next_provider = self.next_provider.wrapping_add(1);
		if !self.registry.add_provider(id, publication_of(&info), now) {
			close(stream);
			close(chan);
			return;
		}
		self.providers.push(Provider { id, chan, stream, sent: Vec::new() });
	}

	// A provider is over - withdrawn, disconnected, or refused for what it sent. Everything it
	// published is removed, and a control outstanding on it completes as indeterminate.
	fn lose(&mut self, id: ProviderId, why: &[u8]) {
		if let Some(at) = self.providers.iter().position(|provider| provider.id == id) {
			let provider = self.providers.remove(at);
			close(provider.stream);
			close(provider.chan);
			print(b"PowerService: a provider is gone: ");
			print(why);
			print(b"\n");
		}
		let effects = self.registry.remove_provider(id);
		self.apply(effects);
	}

	fn withdraw(&mut self, info: &ProviderInfo) {
		if let Some(id) = self.registry.provider_of(publication_of(info)) {
			self.lose(id, b"its publication was withdrawn");
		}
	}

	// What time passing, or a provider going, requires.
	fn apply(&mut self, effects: Vec<Effect>) {
		for effect in effects {
			match effect {
				Effect::Indeterminate { corr } => self.complete(corr, Ok(ControlOutcome::Indeterminate)),
				Effect::ProviderFailed(id) => {
					if let Some(at) = self.providers.iter().position(|provider| provider.id == id) {
						let provider = self.providers.remove(at);
						close(provider.stream);
						close(provider.chan);
						print(b"PowerService: a provider did not finish its snapshot in time and is closed\n");
					}
				}
				Effect::Query(dispatch) => self.query(dispatch),
			}
		}
	}

	// Answer the operator waiting on `corr`, if one still is.
	fn complete(&mut self, corr: u32, result: Result<ControlOutcome, Error>) {
		if let Some(at) = self.waiting.iter().position(|waiting| waiting.corr == corr) {
			let waiting = self.waiting.remove(at);
			reply_result(waiting.chan, waiting.request, result);
		}
	}

	fn remember(provider: &mut Provider, corr: u32, what: Sent) {
		// Bounded: one control and one query are all a provider can have outstanding, and a few late
		// ones behind them are all worth recognising.
		if provider.sent.len() >= 8 {
			provider.sent.remove(0);
		}
		provider.sent.push((corr, what));
	}

	// A reconciliation query: the provider's fresh state for one source.
	fn query(&mut self, dispatch: Dispatch) {
		let now = clock();
		let Some(provider) = self.providers.iter_mut().find(|provider| provider.id == dispatch.provider) else { return };
		if send_request(provider.chan, power_provider::OP_QUERY, dispatch.corr, |w| w.u32(dispatch.local)) {
			Self::remember(provider, dispatch.corr, Sent::Query);
		} else {
			// Not sent is not answered: the source's controls stay unavailable, and nothing is retried.
			self.registry.query_answered(dispatch.provider, dispatch.corr, None, now);
		}
	}

	// Frames from one provider's update stream. FALSE WHEN THE PROVIDER IS OVER: its stream closed, a
	// frame did not decode, a record was not canonical, or the registry refused the order it came in.
	fn drain_stream(&mut self, at: usize, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		let id = self.providers[at].id;
		let stream = self.providers[at].stream;
		loop {
			let (len, handles) = match try_recv_caps(stream, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its update stream closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = wire::Handles::new();
			let Some(update) = power_provider::updates_read(&buf[..len], &mut frame_handles) else { return Err(b"a frame of its update stream did not decode") };
			let sourced = |update: &proto::system::ProviderUpdate| -> Result<(u32, Held), &'static [u8]> {
				let Some(source) = update.source.as_ref() else { return Err(b"an update carried no source") };
				// CANONICAL OR REFUSED. The adapters a producer calls are the only route to a record;
				// one that fails this was not built by them, and is not translated here.
				if power_model::canon::validate(&source.state).is_err() {
					return Err(b"it published a state that is not canonical");
				}
				Ok((source.local, Held(source.state.clone())))
			};
			let frame = match update.kind {
				ProviderUpdateKind::Snapshot => {
					let (local, state) = sourced(&update)?;
					Frame::Snapshot { revision: update.revision, local, state }
				}
				ProviderUpdateKind::SnapshotEnd => Frame::SnapshotEnd { revision: update.revision },
				ProviderUpdateKind::Added => {
					let (local, state) = sourced(&update)?;
					Frame::Added { revision: update.revision, local, state }
				}
				ProviderUpdateKind::Updated => {
					let (local, state) = sourced(&update)?;
					Frame::Updated { revision: update.revision, local, state }
				}
				ProviderUpdateKind::Removed => match update.gone {
					Some(local) => Frame::Removed { revision: update.revision, local },
					None => return Err(b"a removal named no source"),
				},
			};
			match self.registry.frame(id, frame, clock()) {
				Ok(admitted) => {
					if admitted.exhausted > 0 {
						print(b"PowerService: a source was refused: this service holds 128 (resource exhausted)\n");
					}
				}
				Err(_) => return Err(b"it broke the provider protocol"),
			}
		}
	}

	// A reply on a provider's request channel.
	fn on_reply(&mut self, at: usize, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		let id = self.providers[at].id;
		let chan = self.providers[at].chan;
		loop {
			let (len, handles) = match try_recv_caps(chan, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its connection closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut reader = wire::Reader::new(&buf[..len]);
			let Some(corr) = reader.u32() else { return Err(b"a reply carried no correlation") };
			let Some(place) = self.providers[at].sent.iter().position(|(sent, _)| *sent == corr) else {
				// Nothing this service asked, or asked and has forgotten: dropped.
				continue;
			};
			let (_, what) = self.providers[at].sent.remove(place);
			match what {
				Sent::Command => {
					let decoded = (|| {
						let value = if reader.tag()? { Ok(ControlOutcome::read(&mut reader)?) } else { Err(Error::read(&mut reader)?) };
						reader.finish()?;
						Some(value)
					})();
					let Some(result) = decoded else { return Err(b"a control reply did not decode") };
					// ANSWERED ONCE. A reply after the deadline already completed the control is not
					// the outstanding one, and the operator has had its answer.
					if self.registry.control_answered(id, corr) {
						self.complete(corr, result);
					}
				}
				Sent::Query => {
					let decoded = (|| {
						let value = if reader.tag()? { Some(SourceState::read(&mut reader)?) } else { Error::read(&mut reader).map(|_| None)? };
						reader.finish()?;
						Some(value)
					})();
					let Some(fresh) = decoded else { return Err(b"a query reply did not decode") };
					if fresh.as_ref().is_some_and(|state| power_model::canon::validate(state).is_err()) {
						return Err(b"it answered a query with a state that is not canonical");
					}
					self.registry.query_answered(id, corr, fresh.map(Held), clock());
				}
			}
		}
	}

	// ------------------------------------------------------------------ operators

	fn control(&mut self, chan: u64, request: &[u8], handles: &mut wire::Handles) -> bool {
		let mut view = ControlView { asked: None };
		let mut discarded = [0u8; 256];
		let mut discarded_handles = wire::Handles::new();
		let decoded = power_control::dispatch(&mut view, request, handles, &mut discarded, &mut discarded_handles);
		for &leftover in discarded_handles.as_slice() {
			close(leftover);
		}
		let Some(_) = decoded else { return false };
		let Some((source, action)) = view.asked else {
			// The protocol's own info request, answered as written.
			if let Some(len) = decoded {
				let _ = try_send(chan, &discarded[..len], 0);
			}
			return true;
		};
		let corr = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
		let dispatch = match self.registry.control(key_of(&source), action, clock()) {
			Ok(dispatch) => dispatch,
			Err(refusal) => {
				reply_result(chan, corr, Err(refusal_error(refusal)));
				return true;
			}
		};
		let command = match action {
			Action::SetOutput { outlet, on } => ProviderCommand { kind: ProviderCommandKind::SetOutput, local: dispatch.local, outlet, on, delay_seconds: 0 },
			Action::ScheduleOff { delay_seconds } => ProviderCommand { kind: ProviderCommandKind::ScheduleOutputOff, local: dispatch.local, outlet: 0, on: false, delay_seconds },
			Action::CancelOff => ProviderCommand { kind: ProviderCommandKind::CancelOutputOff, local: dispatch.local, outlet: 0, on: false, delay_seconds: 0 },
		};
		let Some(provider) = self.providers.iter_mut().find(|provider| provider.id == dispatch.provider) else {
			self.registry.control_answered(dispatch.provider, dispatch.corr);
			reply_result(chan, corr, Err(Error::Closed));
			return true;
		};
		if !send_request(provider.chan, power_provider::OP_COMMAND, dispatch.corr, |w| command.write(w)) {
			// NOT SENT, SO NOT POSSIBLY DELIVERED: the slot is released and the operator told to try
			// again, which is the one case a control may be repeated - by its caller, never by this.
			self.registry.control_answered(dispatch.provider, dispatch.corr);
			reply_result(chan, corr, Err(Error::Again));
			return true;
		}
		Self::remember(provider, dispatch.corr, Sent::Command);
		self.waiting.push(Waiting { corr: dispatch.corr, chan, request: corr });
		true
	}

	// ------------------------------------------------------------------ subscribers

	// `subscribe`: one atomic snapshot at (epoch, revision) and live changes after it. THE SNAPSHOT IS
	// CHARGED BEFORE THE SUBSCRIBER IS ADMITTED - every frame encoded and a channel allocated deep
	// enough for all of them - and the subscriber is registered in the same step the snapshot was read
	// in, so nothing can fall between the two.
	fn subscribe(&mut self, chan: u64, request: &[u8], handles: &mut wire::Handles) -> bool {
		let mut view = StateView { registry: &self.registry };
		let Some((corr, _)) = power::subscribe_open(&mut view, request, handles) else { return false };
		let mut reply = [0u8; 64];
		let refuse = |error: Error, reply: &mut [u8]| {
			if let Some(len) = power::subscribe_reply_err(corr, &error, reply) {
				let _ = try_send(chan, &reply[..len], 0);
			}
		};
		if !self.registry.subscriber_room() {
			refuse(Error::Exhausted, &mut reply);
			return true;
		}
		let epoch = self.registry.epoch();
		let revision = self.registry.revision();
		let sources = self.registry.sources();
		let mut frames: Vec<Vec<u8>> = Vec::new();
		if frames.try_reserve_exact(sources.len() + 1).is_err() {
			refuse(Error::Exhausted, &mut reply);
			return true;
		}
		let mut buf = [0u8; FRAME_BYTES];
		let items = sources.iter().map(|(key, received, held)| PowerChange { epoch, revision, kind: ChangeKind::Snapshot, source: Some(snapshot_of(*key, *received, &held.0)), gone: None }).chain(core::iter::once(PowerChange { epoch, revision, kind: ChangeKind::SnapshotEnd, source: None, gone: None }));
		for (seq, item) in items.enumerate() {
			let mut frame_handles = wire::Handles::new();
			let Some(len) = power::subscribe_frame(seq as u32, &item, &mut buf, &mut frame_handles) else {
				refuse(Error::Exhausted, &mut reply);
				return true;
			};
			frames.push(buf[..len].to_vec());
		}
		let Some((producer, consumer)) = channel_with_depth(frames.len() as u64 + LIVE_DEPTH) else {
			refuse(Error::Exhausted, &mut reply);
			return true;
		};
		for frame in &frames {
			if !try_send(producer, frame, 0) {
				close(producer);
				close(consumer);
				refuse(Error::Exhausted, &mut reply);
				return true;
			}
		}
		let Some(id) = self.registry.subscribe() else {
			close(producer);
			close(consumer);
			refuse(Error::Exhausted, &mut reply);
			return true;
		};
		match power::subscribe_reply_ok(corr, &mut reply) {
			Some(len) if send_caps_blocking(chan, &reply[..len], &[consumer]) => {
				self.subscribers.push(Subscriber { id, chan: producer, seq: frames.len() as u32 });
			}
			_ => {
				self.registry.unsubscribe(id);
				close(producer);
				close(consumer);
			}
		}
		true
	}

	// Hand every subscriber with something waiting what may go now. A subscription the registry closed
	// - overflow, or a reader that took nothing for five seconds - is closed here, which is what tells
	// its reader that continuity was lost; so is one whose reader has gone.
	fn drain_subscribers(&mut self) {
		let now = clock();
		let epoch = self.registry.epoch();
		for id in self.registry.pending_subscribers() {
			let Some(at) = self.subscribers.iter().position(|subscriber| subscriber.id == id) else {
				self.registry.unsubscribe(id);
				continue;
			};
			let chan = self.subscribers[at].chan;
			let mut seq = self.subscribers[at].seq;
			let mut gone = false;
			let mut buf = [0u8; FRAME_BYTES];
			let result = self.registry.drain(id, now, |change| {
				let frame = change_frame(epoch, change);
				let mut frame_handles = wire::Handles::new();
				let Some(len) = power::subscribe_frame(seq, &frame, &mut buf, &mut frame_handles) else { return true };
				match try_send_outcome(chan, &buf[..len], 0) {
					SendOutcome::Delivered => {
						seq = seq.wrapping_add(1);
						true
					}
					SendOutcome::Stalled => false,
					SendOutcome::Failed => {
						gone = true;
						false
					}
				}
			});
			self.subscribers[at].seq = seq;
			if result.is_err() || gone {
				if result.is_err() {
					print(b"PowerService: a subscription is closed - its reader fell behind, and continuity is lost\n");
				}
				close(chan);
				self.subscribers.remove(at);
				self.registry.unsubscribe(id);
			}
		}
	}

	fn drop_subscriber(&mut self, chan: u64) {
		if let Some(at) = self.subscribers.iter().position(|subscriber| subscriber.chan == chan) {
			let subscriber = self.subscribers.remove(at);
			self.registry.unsubscribe(subscriber.id);
			close(chan);
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// 1. The roles: a catalogue connection minted for `power-source` alone, and the two roots its
	//    clients reach it on. Nothing else - no system-power client, no provider connection handed in.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (catalogue, state_root, control_root) = (roles[0], roles[1], roles[2]);

	// 2. The providers this machine publishes, as a snapshot and then live. A machine with none has
	//    none, which is a subscription that stays quiet rather than a failure. THE EPOCH IS THIS
	//    INSTANCE'S: a restarted service starts a new one, and its clients resubscribe to it.
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::PowerSource).unwrap_or(0) } else { 0 };
	let mut epoch_bytes = [0u8; 8];
	let epoch = if random_get(&mut epoch_bytes) == epoch_bytes.len() { u64::from_le_bytes(epoch_bytes) } else { clock() };
	let mut service = Power { registry: Registry::new(epoch), providers: Vec::new(), subscribers: Vec::new(), waiting: Vec::new(), next_provider: 1 };
	send_blocking(bootstrap, b"PowerService: online", 0);

	let mut clients: Vec<Client> = Vec::new();
	let mut buf = alloc::vec![0u8; 8192];
	let mut reply = alloc::vec![0u8; 8192];
	let mut subscribed = subscription != 0;
	loop {
		let mut waitset: Vec<u64> = Vec::with_capacity(4 + clients.len() + 2 * service.providers.len() + service.subscribers.len());
		for root in [state_root, control_root] {
			if root != 0 {
				waitset.push(root);
			}
		}
		if subscribed {
			waitset.push(subscription);
		}
		for provider in &service.providers {
			waitset.push(provider.stream);
			waitset.push(provider.chan);
		}
		waitset.extend(service.subscribers.iter().map(|subscriber| subscriber.chan));
		waitset.extend(clients.iter().map(|client| client.chan));
		// A subscriber with records waiting is retried at the coalescing interval, since nothing
		// wakes this loop when a reader makes room.
		let now = clock();
		let retry = if service.registry.pending_subscribers().is_empty() { None } else { Some(now + service_logic::power_registry::COALESCE_TICKS) };
		let deadline = match (service.registry.next_deadline(), retry) {
			(Some(a), Some(b)) => a.min(b),
			(a, b) => a.or(b).unwrap_or(0),
		};
		let ready = wait_any(&waitset, if deadline != 0 { deadline.max(now + 1) } else { 0 });
		if ready >= 0 {
			let handle = waitset[ready as usize];
			serve(&mut service, &mut clients, handle, catalogue, subscription, &mut subscribed, (state_root, control_root), &mut buf, &mut reply);
		}
		// TIME, AND WHAT THE REQUEST JUST SERVED QUEUED. A refused control can start the fresh query that
		// brings a source's controls back, and the query leaves on a tick: taken before the request, the
		// tick left it for the next deadline - which was the query's own, so it was sent as it expired and
		// its answer was dropped, and the source never came back.
		let effects = service.registry.tick(clock());
		service.apply(effects);
		service.drain_subscribers();
	}
}

// One ready handle.
fn serve(service: &mut Power, clients: &mut Vec<Client>, handle: u64, catalogue: u64, subscription: u64, subscribed: &mut bool, roots: (u64, u64), buf: &mut [u8], reply: &mut [u8]) {
	// A PROVIDER: its update stream, or a reply on its request channel.
	if let Some(at) = service.providers.iter().position(|provider| provider.stream == handle) {
		if let Err(why) = service.drain_stream(at, buf) {
			let id = service.providers[at].id;
			service.lose(id, why);
		}
		return;
	}
	if let Some(at) = service.providers.iter().position(|provider| provider.chan == handle) {
		if let Err(why) = service.on_reply(at, buf) {
			let id = service.providers[at].id;
			service.lose(id, why);
		}
		return;
	}

	// A SUBSCRIBER'S CHANNEL: nothing is read from it, so readiness is its reader going - or sending
	// something it has no business sending, which is read and dropped.
	if service.subscribers.iter().any(|subscriber| subscriber.chan == handle) {
		match try_recv_caps(handle, buf) {
			PolledCaps::Message { handles, .. } => {
				for &leftover in handles.as_slice() {
					close(leftover);
				}
			}
			PolledCaps::Empty => {}
			PolledCaps::Closed => service.drop_subscriber(handle),
		}
		return;
	}

	// THE CATALOGUE: a provider arriving or leaving.
	if *subscribed && handle == subscription {
		loop {
			let (len, handles) = match try_recv_caps(subscription, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => break,
				PolledCaps::Closed => {
					*subscribed = false;
					break;
				}
			};
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			let mut frame_handles = wire::Handles::new();
			let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else { continue };
			if info.live {
				service.adopt(catalogue, info);
			} else {
				service.withdraw(&info);
			}
		}
		return;
	}

	// A ROOT mints a connection for the interface it serves; a CLIENT speaks the one it was minted for.
	let (state_root, control_root) = roots;
	let root_interface = if handle == state_root {
		Some(Interface::State)
	} else if handle == control_root {
		Some(Interface::Control)
	} else {
		None
	};
	let (interface, is_root) = match root_interface {
		Some(interface) => (interface, true),
		None => match clients.iter().find(|client| client.chan == handle) {
			Some(client) => (client.interface, false),
			None => return,
		},
	};
	let (len, mut handles) = match try_recv_caps(handle, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			if !is_root {
				clients.retain(|client| client.chan != handle);
				// An operator that went while waiting is answered by nobody: the control still
				// completes in the registry, and its answer is simply not sent.
				service.waiting.retain(|waiting| waiting.chan != handle);
				close(handle);
			}
			return;
		}
	};
	if len >= 2 {
		let op = u16::from_le_bytes([buf[0], buf[1]]);
		if op == HEARTBEAT_OP {
			send_blocking(handle, b"PONG", 0);
			return;
		}
		if op == CONNECT_OP {
			if clients.len() >= MAX_CLIENTS {
				send_blocking(handle, &[], 0);
				return;
			}
			match channel() {
				Some((mine, theirs)) => {
					clients.push(Client { chan: mine, interface });
					send_blocking(handle, &[], theirs);
				}
				None => {
					send_blocking(handle, &[], 0);
				}
			}
			return;
		}
	}
	if is_root {
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		return;
	}
	let understood = match interface {
		Interface::State => {
			if len >= 2 && u16::from_le_bytes([buf[0], buf[1]]) == power::OP_SUBSCRIBE {
				service.subscribe(handle, &buf[..len], &mut handles)
			} else {
				let mut reply_handles = wire::Handles::new();
				match power::dispatch(&mut StateView { registry: &service.registry }, &buf[..len], &mut handles, reply, &mut reply_handles) {
					Some(written) => {
						if !send_caps_blocking(handle, &reply[..written], reply_handles.as_slice()) {
							for &leftover in reply_handles.as_slice() {
								close(leftover);
							}
						}
						true
					}
					None => false,
				}
			}
		}
		Interface::Control => service.control(handle, &buf[..len], &mut handles),
	};
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	// A REQUEST THIS CONNECTION'S INTERFACE DOES NOT HAVE IS NOT ANSWERED WITH SILENCE: the connection
	// is closed, so a reader that tried a control - or a publication - finds out at once, and nothing
	// it sent reached anything.
	if !understood {
		clients.retain(|client| client.chan != handle);
		service.waiting.retain(|waiting| waiting.chan != handle);
		close(handle);
	}
}
