// TypeCService - every USB Type-C connector the system's transports report, observed by anyone granted the read
// authority and asked for a role swap or an alternate mode by the one component granted the operator authority.
//
// WHERE ITS STATE COMES FROM. The `ucsi-acpi` and `tcpci` drivers publish `typec-connector` providers through their
// own bindings; this service is the one consumer of that kind, through a catalogue connection minted for it alone,
// and never hands a provider connection to anybody. It holds no claim and converts nothing: a connector record is
// checked and kept as its driver sent it, and a connector's supply reaches PowerService from the same driver as a
// `usb-c` source, not from here.
//
// EVERY DECISION IS SERVICE LOGIC'S: the connectors, their order, coalescing, the subscribers' bounded queues and
// their closure are PowerService's registry, holding connectors where it holds sources; the operator's requests -
// one outstanding per connector, fifteen seconds, indeterminate and never retried past that - are
// `typec_requests`'. What is here is the IO around them, and nothing in it waits on anybody: the loop blocks in one
// place, `wait_any`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::generated::liber::typec::v1 as typec;
use proto::system::{Error, ProviderInfo, ProviderKind, provider_catalogue};
use rt::*;
use service_logic::power_registry::{Change, Controls, Effect, Frame, Payload, ProviderId, Publication, Registry, SourceKey, SubscriberId};
use service_logic::typec_requests::{Refusal as RequestRefusal, Requests};
use typec::{Answer, ChangeKind, Connector, ConnectorId, ConnectorSnapshot, DataRole, Outcome, PowerRole, Refusal, Request, RequestKind, TypecChange, UpdateKind, typec_control, typec_provider};
use wire::Sink;

include!(concat!(env!("OUT_DIR"), "/roles_typec_service.rs"));

// Client connections minted from the two roots, together.
const MAX_CLIENTS: usize = 32;
// How deep a subscriber's channel is past its snapshot.
const LIVE_DEPTH: u64 = 8;
// How long a provider has to answer the request that opens its update stream.
const OPEN_TICKS: u64 = 100;
// The largest frame this service writes: one connector with eleven offers and sixteen modes fits well inside.
const FRAME_BYTES: usize = 2048;
// Connectors are numbered from 1 and held by the registry at local identities from 0: sixteen of them.
const MAX_CONNECTORS: u8 = 16;

// A connector, as the registry holds it.
#[derive(Clone)]
struct Held(Connector);

impl Payload for Held {
	// WHAT IS NEVER COALESCED AWAY - the registry's word for it is an alarm transition: a partner attached or gone,
	// a role, the contract, the operation mode, an entered mode, a refusal, the transport falling silent. Offers
	// and cable details refreshed beside an unchanged state may be.
	fn alarm_transition(&self, before: &Self) -> bool {
		let (now, then) = (&self.0, &before.0);
		now.partner != then.partner || now.power_role != then.power_role || now.data_role != then.data_role || now.contract != then.contract || now.operation_mode != then.operation_mode || now.answering != then.answering || now.last_refusal != then.last_refusal || now.displayport != then.displayport || now.modes.iter().map(|mode| mode.entered).ne(then.modes.iter().map(|mode| mode.entered))
	}
	// A connector advertises no power control: its requests are this service's own, below.
	fn controls(&self) -> Controls {
		Controls::default()
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Interface {
	State,
	Control,
}

struct Client {
	chan: u64,
	interface: Interface,
}

struct Provider {
	id: ProviderId,
	chan: u64,
	stream: u64,
}

// An operator waiting for its request to complete.
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

struct TypeC {
	registry: Registry<Held>,
	requests: Requests,
	providers: Vec<Provider>,
	subscribers: Vec<Subscriber>,
	waiting: Vec<Waiting>,
	next_provider: u32,
}

// ------------------------------------------------------------------ identities

fn connector_id(key: SourceKey) -> ConnectorId {
	ConnectorId { slot: key.slot, generation: key.generation, binding_generation: key.binding_generation, number: key.local as u8 + 1 }
}

fn key_of(id: &ConnectorId) -> Option<SourceKey> {
	(1..=MAX_CONNECTORS).contains(&id.number).then(|| SourceKey { slot: id.slot, generation: id.generation, binding_generation: id.binding_generation, local: u32::from(id.number) - 1 })
}

fn publication_of(info: &ProviderInfo) -> Publication {
	Publication { slot: info.slot, generation: info.provider_generation, binding_generation: info.binding_generation }
}

fn snapshot_of(key: SourceKey, received: u64, connector: &Connector) -> ConnectorSnapshot {
	ConnectorSnapshot { id: connector_id(key), received, connector: connector.clone() }
}

fn change_frame(epoch: u64, change: &Change<Held>) -> TypecChange {
	match change {
		Change::Added { key, revision, received, state } => TypecChange { epoch, revision: *revision, kind: ChangeKind::Added, connector: Some(snapshot_of(*key, *received, &state.0)), gone: None },
		Change::Updated { key, revision, received, state } => TypecChange { epoch, revision: *revision, kind: ChangeKind::Updated, connector: Some(snapshot_of(*key, *received, &state.0)), gone: None },
		Change::Removed { key, revision } => TypecChange { epoch, revision: *revision, kind: ChangeKind::Removed, connector: None, gone: Some(connector_id(*key)) },
	}
}

// A CONNECTOR RECORD IS KEPT ONLY IF IT IS ONE: numbered 1 to 16, its bounded lists within their bounds (the decoder
// already refused longer ones), a contract's offer at a position the offers could hold, and no contract without a
// partner. A driver that sends another is refused, and everything it published with it.
fn well_formed(connector: &Connector) -> bool {
	(1..=MAX_CONNECTORS).contains(&connector.number) && connector.offers.iter().all(|offer| (1..=11).contains(&offer.position)) && connector.contract.as_ref().is_none_or(|contract| (1..=11).contains(&contract.offer.position) && connector.partner != typec::PartnerKind::None)
}

// ------------------------------------------------------------------ replies written by hand

// `result<answer, error>` under a correlation - written here because a request's answer is sent long after the
// request was decoded, which the generated dispatch cannot.
fn reply_answer(chan: u64, corr: u32, result: Result<Answer, Error>) {
	let mut writer = wire::VecWriter::new();
	let encoded = (|| {
		writer.u32(corr)?;
		match result {
			Ok(answer) => {
				writer.u8(1)?;
				answer.write(&mut writer)
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
		// NEVER WAITED ON: an operator that stopped reading its connection is not a reason for this service to stop.
		let _ = try_send(chan, &bytes, 0);
	}
}

fn refused(reason: Refusal) -> Answer {
	Answer { outcome: Outcome::Refused, reason: Some(reason), error: 0 }
}

const INDETERMINATE: Answer = Answer { outcome: Outcome::Indeterminate, reason: None, error: 0 };

// ------------------------------------------------------------------ the views the generated code calls

struct StateView<'a> {
	registry: &'a Registry<Held>,
}

impl typec::typec::Service for StateView<'_> {
	fn connectors(&mut self) -> Result<Vec<ConnectorSnapshot>, Error> {
		Ok(self.registry.sources().iter().map(|(key, received, held)| snapshot_of(*key, *received, &held.0)).collect())
	}
	fn subscribe(&mut self) -> Result<Vec<TypecChange>, Error> {
		Ok(Vec::new())
	}
}

// The operator's interface, DECODED by the generated dispatch and ANSWERED later: this records what was asked, and
// the placeholder reply the dispatch writes is discarded.
struct ControlView {
	asked: Option<(ConnectorId, Request)>,
}

impl ControlView {
	fn ask(&mut self, connector: ConnectorId, kind: RequestKind, data_role: Option<DataRole>, power_role: Option<PowerRole>, svid: u16, vdo: u32) -> Result<Answer, Error> {
		let number = connector.number;
		self.asked = Some((connector, Request { kind, connector: number, data_role, power_role, svid, vdo }));
		Err(Error::Again)
	}
}

impl typec_control::Service for ControlView {
	fn data_role_swap(&mut self, connector: ConnectorId, role: DataRole) -> Result<Answer, Error> {
		self.ask(connector, RequestKind::DataRoleSwap, Some(role), None, 0, 0)
	}
	fn power_role_swap(&mut self, connector: ConnectorId, role: PowerRole) -> Result<Answer, Error> {
		self.ask(connector, RequestKind::PowerRoleSwap, None, Some(role), 0, 0)
	}
	fn enter_mode(&mut self, connector: ConnectorId, svid: u16, vdo: u32) -> Result<Answer, Error> {
		self.ask(connector, RequestKind::EnterMode, None, None, svid, vdo)
	}
	fn exit_mode(&mut self, connector: ConnectorId, svid: u16, vdo: u32) -> Result<Answer, Error> {
		self.ask(connector, RequestKind::ExitMode, None, None, svid, vdo)
	}
}

// ------------------------------------------------------------------ providers

impl TypeC {
	// A publication arrived: open it and its update stream, and give it the registry's time to describe itself.
	// BEYOND THE PROVIDER LIMIT IT IS REFUSED AND SAID, never admitted by evicting another.
	fn adopt(&mut self, catalogue: u64, info: ProviderInfo) {
		let now = clock();
		if !self.registry.provider_room() {
			print(b"TypeCService: a typec-connector provider was refused: this service holds eight (resource exhausted)\n");
			return;
		}
		if self.registry.provider_of(publication_of(&info)).is_some() {
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) else {
			print(b"TypeCService: a published Type-C provider could not be opened\n");
			return;
		};
		let Some(stream) = typec_provider::Client::with_deadline(ChannelTransport { chan }, now + OPEN_TICKS).updates() else {
			print(b"TypeCService: a Type-C provider did not open its update stream\n");
			close(chan);
			return;
		};
		let id = ProviderId(self.next_provider);
		self.next_provider = self.next_provider.wrapping_add(1);
		if !self.registry.add_provider(id, publication_of(&info), now) {
			close(stream);
			close(chan);
			return;
		}
		self.providers.push(Provider { id, chan, stream });
	}

	// A provider is over: everything it published is removed, and a request outstanding on it completes as
	// indeterminate.
	fn lose(&mut self, id: ProviderId, why: &[u8]) {
		if let Some(at) = self.providers.iter().position(|provider| provider.id == id) {
			let provider = self.providers.remove(at);
			close(provider.stream);
			close(provider.chan);
			print(b"TypeCService: a provider is gone: ");
			print(why);
			print(b"\n");
		}
		for corr in self.requests.provider_gone(id) {
			self.complete(corr, Ok(INDETERMINATE));
		}
		let effects = self.registry.remove_provider(id);
		self.apply(effects);
	}

	fn withdraw(&mut self, info: &ProviderInfo) {
		if let Some(id) = self.registry.provider_of(publication_of(info)) {
			self.lose(id, b"its publication was withdrawn");
		}
	}

	fn apply(&mut self, effects: Vec<Effect>) {
		for effect in effects {
			// The registry's control effects never arise: no connector advertises a power control.
			if let Effect::ProviderFailed(id) = effect
				&& let Some(at) = self.providers.iter().position(|provider| provider.id == id)
			{
				let provider = self.providers.remove(at);
				close(provider.stream);
				close(provider.chan);
				print(b"TypeCService: a provider did not finish its snapshot in time and is closed\n");
				for corr in self.requests.provider_gone(id) {
					self.complete(corr, Ok(INDETERMINATE));
				}
			}
		}
	}

	// Answer the operator waiting on `corr`, if one still is.
	fn complete(&mut self, corr: u32, result: Result<Answer, Error>) {
		if let Some(at) = self.waiting.iter().position(|waiting| waiting.corr == corr) {
			let waiting = self.waiting.remove(at);
			reply_answer(waiting.chan, waiting.request, result);
		}
	}

	// Frames from one provider's update stream. AN ERROR ENDS THE PROVIDER: its stream closed, a frame did not
	// decode, a connector was not one, or the registry refused the order it came in.
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
			let Some(update) = typec_provider::updates_read(&buf[..len], &mut frame_handles) else { return Err(b"a frame of its update stream did not decode") };
			let held = |update: &typec::ProviderUpdate| -> Result<(u32, Held), &'static [u8]> {
				let Some(connector) = update.connector.as_ref() else { return Err(b"an update carried no connector") };
				if !well_formed(connector) {
					return Err(b"it published a connector that is not one");
				}
				Ok((u32::from(connector.number) - 1, Held(connector.clone())))
			};
			let frame = match update.kind {
				UpdateKind::Snapshot => {
					let (local, state) = held(&update)?;
					Frame::Snapshot { revision: update.revision, local, state }
				}
				UpdateKind::SnapshotEnd => Frame::SnapshotEnd { revision: update.revision },
				UpdateKind::Added => {
					let (local, state) = held(&update)?;
					Frame::Added { revision: update.revision, local, state }
				}
				UpdateKind::Updated => {
					let (local, state) = held(&update)?;
					Frame::Updated { revision: update.revision, local, state }
				}
				UpdateKind::Removed => match update.gone {
					Some(number) if (1..=MAX_CONNECTORS).contains(&number) => Frame::Removed { revision: update.revision, local: u32::from(number) - 1 },
					_ => return Err(b"a removal named no connector"),
				},
			};
			match self.registry.frame(id, frame, clock()) {
				Ok(admitted) => {
					if admitted.exhausted > 0 {
						print(b"TypeCService: a connector was refused: this service holds 128 (resource exhausted)\n");
					}
				}
				Err(_) => return Err(b"it broke the provider protocol"),
			}
		}
	}

	// An answer on a provider's request channel.
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
			let Some(corr) = reader.u32() else { return Err(b"an answer carried no correlation") };
			let decoded = (|| {
				let value = if reader.tag()? { Ok(Answer::read(&mut reader)?) } else { Err(Error::read(&mut reader)?) };
				reader.finish()?;
				Some(value)
			})();
			let Some(result) = decoded else { return Err(b"an answer did not decode") };
			// ANSWERED ONCE. An answer after the deadline completed the request is not the outstanding one.
			if self.requests.answered(id, corr) {
				self.complete(corr, result);
			}
		}
	}

	// ------------------------------------------------------------------ operators

	fn control(&mut self, chan: u64, request: &[u8], handles: &mut wire::Handles) -> bool {
		let mut view = ControlView { asked: None };
		let mut discarded = [0u8; 256];
		let mut discarded_handles = wire::Handles::new();
		let decoded = typec_control::dispatch(&mut view, request, handles, &mut discarded, &mut discarded_handles);
		for &leftover in discarded_handles.as_slice() {
			close(leftover);
		}
		let Some(_) = decoded else { return false };
		let Some((id, asked)) = view.asked else {
			// The protocol's own info request, answered as written.
			if let Some(len) = decoded {
				let _ = try_send(chan, &discarded[..len], 0);
			}
			return true;
		};
		let corr = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
		// A FORGED OR STALE IDENTITY NAMES NOTHING: denied before anything is sent.
		let Some(key) = key_of(&id) else {
			reply_answer(chan, corr, Err(Error::Denied));
			return true;
		};
		let live = self.registry.sources().iter().any(|(held, _, _)| *held == key);
		let Some(provider_id) = self.registry.provider_of(Publication { slot: key.slot, generation: key.generation, binding_generation: key.binding_generation }).filter(|_| live) else {
			reply_answer(chan, corr, Err(Error::Denied));
			return true;
		};
		let dispatched = match self.requests.start(key, provider_id, clock()) {
			Ok(dispatched) => dispatched,
			Err(RequestRefusal::Busy) => {
				reply_answer(chan, corr, Ok(refused(Refusal::Busy)));
				return true;
			}
			Err(RequestRefusal::Exhausted) => {
				reply_answer(chan, corr, Err(Error::Exhausted));
				return true;
			}
		};
		let Some(provider) = self.providers.iter().find(|provider| provider.id == provider_id) else {
			self.requests.abandon(dispatched);
			reply_answer(chan, corr, Err(Error::Closed));
			return true;
		};
		let mut writer = wire::VecWriter::new();
		let sent = writer.u16(typec_provider::OP_REQUEST).is_some() && writer.u32(dispatched).is_some() && asked.write(&mut writer).is_some() && writer.into_inner().is_some_and(|bytes| try_send(provider.chan, &bytes, 0));
		if !sent {
			// NOT SENT, SO NOT POSSIBLY DELIVERED: released, and the operator told to try again - the one case a
			// request may be repeated, by its caller, never by this.
			self.requests.abandon(dispatched);
			reply_answer(chan, corr, Err(Error::Again));
			return true;
		}
		self.waiting.push(Waiting { corr: dispatched, chan, request: corr });
		true
	}

	// ------------------------------------------------------------------ subscribers

	// `subscribe`: one atomic snapshot at (epoch, revision) and live changes after it, the snapshot charged before
	// the subscriber is admitted and the subscriber registered in the step the snapshot was read in.
	fn subscribe(&mut self, chan: u64, request: &[u8], handles: &mut wire::Handles) -> bool {
		let mut view = StateView { registry: &self.registry };
		let Some((corr, _)) = typec::typec::subscribe_open(&mut view, request, handles) else { return false };
		let mut reply = [0u8; 64];
		let refuse = |error: Error, reply: &mut [u8]| {
			if let Some(len) = typec::typec::subscribe_reply_err(corr, &error, reply) {
				let _ = try_send(chan, &reply[..len], 0);
			}
		};
		if !self.registry.subscriber_room() {
			refuse(Error::Exhausted, &mut reply);
			return true;
		}
		let epoch = self.registry.epoch();
		let revision = self.registry.revision();
		let connectors = self.registry.sources();
		let mut frames: Vec<Vec<u8>> = Vec::new();
		if frames.try_reserve_exact(connectors.len() + 1).is_err() {
			refuse(Error::Exhausted, &mut reply);
			return true;
		}
		let mut buf = [0u8; FRAME_BYTES];
		let items = connectors.iter().map(|(key, received, held)| TypecChange { epoch, revision, kind: ChangeKind::Snapshot, connector: Some(snapshot_of(*key, *received, &held.0)), gone: None }).chain(core::iter::once(TypecChange { epoch, revision, kind: ChangeKind::SnapshotEnd, connector: None, gone: None }));
		for (seq, item) in items.enumerate() {
			let mut frame_handles = wire::Handles::new();
			let Some(len) = typec::typec::subscribe_frame(seq as u32, &item, &mut buf, &mut frame_handles) else {
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
		match typec::typec::subscribe_reply_ok(corr, &mut reply) {
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

	// Hand every subscriber with something waiting what may go now; a subscription the registry closed, or whose
	// reader went, is closed here - which is what tells its reader that continuity was lost.
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
				let Some(len) = typec::typec::subscribe_frame(seq, &frame, &mut buf, &mut frame_handles) else { return true };
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
					print(b"TypeCService: a subscription is closed - its reader fell behind, and continuity is lost\n");
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
	// 1. The roles: a catalogue connection minted for `typec-connector` alone, and the two roots its clients reach it
	//    on.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (catalogue, state_root, control_root) = (roles[0], roles[1], roles[2]);

	// 2. The providers this machine publishes, as a snapshot and then live. A machine with none has none, which is a
	//    subscription that stays quiet. THE EPOCH IS THIS INSTANCE'S: a restarted service starts a new one.
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::TypecConnector).unwrap_or(0) } else { 0 };
	let mut epoch_bytes = [0u8; 8];
	let epoch = if random_get(&mut epoch_bytes) == epoch_bytes.len() { u64::from_le_bytes(epoch_bytes) } else { clock() };
	let mut service = TypeC { registry: Registry::new(epoch), requests: Requests::new(), providers: Vec::new(), subscribers: Vec::new(), waiting: Vec::new(), next_provider: 1 };
	print(b"TypeCService: online\n");
	send_blocking(bootstrap, b"TypeCService: online", 0);

	let mut clients: Vec<Client> = Vec::new();
	let mut buf = alloc::vec![0u8; 8192];
	let mut reply = alloc::vec![0u8; 16384];
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
		let now = clock();
		let retry = if service.registry.pending_subscribers().is_empty() { None } else { Some(now + service_logic::power_registry::COALESCE_TICKS) };
		let deadline = [service.registry.next_deadline(), service.requests.next_deadline(), retry].into_iter().flatten().min().unwrap_or(0);
		let ready = wait_any(&waitset, if deadline != 0 { deadline.max(now + 1) } else { 0 });
		if ready >= 0 {
			let handle = waitset[ready as usize];
			serve(&mut service, &mut clients, handle, catalogue, subscription, &mut subscribed, (state_root, control_root), &mut buf, &mut reply);
		}
		// TIME: a request past its fifteen seconds completes as indeterminate, a provider past its snapshot deadline
		// is closed, and what the request just served queued goes out.
		let now = clock();
		for corr in service.requests.tick(now) {
			service.complete(corr, Ok(INDETERMINATE));
		}
		let effects = service.registry.tick(now);
		service.apply(effects);
		service.drain_subscribers();
	}
}

// One ready handle.
fn serve(service: &mut TypeC, clients: &mut Vec<Client>, handle: u64, catalogue: u64, subscription: u64, subscribed: &mut bool, roots: (u64, u64), buf: &mut [u8], reply: &mut [u8]) {
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

	// A SUBSCRIBER'S CHANNEL: readiness is its reader going, or sending what it has no business sending.
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
				// An operator that went while waiting is answered by nobody; its request still completes.
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
			if len >= 2 && u16::from_le_bytes([buf[0], buf[1]]) == typec::typec::OP_SUBSCRIBE {
				service.subscribe(handle, &buf[..len], &mut handles)
			} else {
				let mut reply_handles = wire::Handles::new();
				match typec::typec::dispatch(&mut StateView { registry: &service.registry }, &buf[..len], &mut handles, reply, &mut reply_handles) {
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
	// A REQUEST THIS CONNECTION'S INTERFACE DOES NOT HAVE is answered by closing the connection.
	if !understood {
		clients.retain(|client| client.chan != handle);
		service.waiting.retain(|waiting| waiting.chan != handle);
		close(handle);
	}
}
