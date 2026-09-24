// ModemService - SIM, registration and the one data context this system supports, over four separately
// granted authorities. NetworkService owns the IP.
//
// WHAT IT HOLDS AND WHAT IT DOES NOT. It consumes every `modem` publication through a catalogue
// connection minted for that kind alone; a provider - the USB CDC-MBIM class module, the in-guest fixture
// - owns the framing, and this service owns who may do what. It serves observation on SERVE (no
// subscriber identifier, no state change), and on ADMIN the minting endpoint PermissionManager alone
// reaches: every data, identity and management connection is minted there, for one component, one
// modem and one SIM, and lives only as long as that component does. It holds one more connection, LINK,
// to NetworkService's private link administration, and installs the context it activates as
// NetworkService's uplink through it - no client of this service is ever handed a packet channel or a
// network administration handle.
//
// EVERY DECISION IS IN `service_logic`: command admission, correlation, deadlines, uncertainty and the
// retry counters in `modem_commands`, the packet bounds beside them. What is here is the IO around them,
// and the rule that nothing in it waits on anybody but `wait_any`: a provider, NetworkService and every
// client are answered as their messages arrive.
//
// NOTHING SECRET IS PRINTED. A PIN or a PUK reaches the provider's command and nowhere else, and the
// buffers that carried it are zeroed once it is sent or refused; subscriber identifiers reach the
// identity connection that asked and nothing else.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{AttemptCount, Attempts, Command, CommandKind, CommandReply, CommandStatus, ContextId, ContextState, ContextStatus, DataPolicy, Datagram, DeviceLimits, DeviceRegistration, DeviceSim, DeviceState, Error, GrantKind, Identity, IpConfig, IpFamily, Ipv4Addr as WireIp, LinkAttachment, LinkFamily, LinkInstalled, LinkProvider, ModemGrant, ModemId, ModemLimits, ModemStatus, ModemWatch, ProviderInfo, ProviderKind, Registration, Signal, SimPinOutcome, SimPinResult, SimState, WatchKind, modem, modem_admin, modem_data, modem_device, modem_identity, modem_manage, network_link_admin, provider_catalogue};
use rt::*;
use service_logic::modem_commands as mc;
use wire::{Handles, Reader, Sink, Transport, TransportError};

include!(concat!(env!("OUT_DIR"), "/roles_modem_service.rs"));

// THE CONFIGURED MODEMS: an alias a policy row may name, and the provider metadata it binds - the
// publication name the modem's provider chose. The one alias is the development fixture's, and a
// configuration that ships binds no fixture, so there it names nothing and no data, identity or
// management grant can be minted at all until somebody configures a modem.
//
// NOT `cfg`-GATED: this service is built once, into the shared image both configurations stage, and that
// build has no development feature - a gated alias was absent from the development image too.
const ALIASES: &[(&str, &[u8])] = &[("fixture", b"org.libersystem.modem-fixture")];

// WHAT THIS SERVICE ADMITS, and says it admits through `limits`.
const MAX_PROVIDERS: usize = 4;
const MAX_CLIENTS: usize = 32;
const MAX_CONTEXTS: u8 = 1;
// Admin connections minted from the root: PermissionManager's, and a spare across its restart.
const MAX_ADMINS: usize = 4;
// The contract version this service speaks, and how long a provider has to open a session and send
// its first state.
const VERSION: u32 = 1;
const OPEN_TICKS: u64 = 100;
// How long NetworkService has to answer a link-admin call. It bounds this service's waiting; a late
// answer is still handled for what it was.
const LINK_TICKS: u64 = 1000;
// A watch stream's depth: one snapshot being read and one waiting, because the watch coalesces to the
// newest and never holds more.
const WATCH_DEPTH: u64 = 2;
const BUF_BYTES: usize = 8192;

// ------------------------------------------------------------------ what the service holds

struct ModemConn {
	key: u32,
	info: ProviderInfo,
	chan: u64,
	indications: u64,
	receive: u64,
	transmit: u64,
	limits: DeviceLimits,
	device: mc::Device,
	state: DeviceState,
	// Commands sent and not answered: the wire correlation, the transaction and kind, and the SIM and
	// context generations they named - so a refusal on the wire can still be matched to its command.
	sent: Vec<(u32, u64, mc::Kind, u64, u64)>,
	next_corr: u32,
}

// A secret on its way to a provider. ZEROED WHEN IT GOES, however it goes: sent, refused or dropped.
struct Secret(Vec<u8>);

impl Drop for Secret {
	fn drop(&mut self) {
		for byte in self.0.iter_mut() {
			// SAFETY: a valid, aligned byte of this vector.
			unsafe { core::ptr::write_volatile(byte, 0) };
		}
	}
}

// Why a command is in flight, and who is waiting for it.
enum Purpose {
	Attempts { grant: u32, corr: u32 },
	// The read before an attempt: the counters are refreshed, then the attempt is made.
	BeforePin { grant: u32, corr: u32, puk: bool, secret: Secret, new_secret: Secret, acknowledge: bool },
	Pin { grant: u32, corr: u32 },
	Identity { grant: u32, corr: u32 },
	Activate,
	Deactivate { grant: u32, corr: u32 },
	// A deactivation nobody asked for: after an installation failed, or when the context's owner, its
	// grant or its link went away.
	Cleanup,
	// A read-only query that reconciles an unknown outcome.
	Reconcile,
}

struct Call {
	id: u64,
	modem: u32,
	purpose: Purpose,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
	Reserving,
	Activating,
	Installing,
	Active,
	Deactivating,
}

// THE ONE CONTEXT, from the reservation made before the modem is asked to do anything to the
// installation's commit and beyond.
struct Context {
	modem: u32,
	grant: u32,
	// The client call waiting for activation's answer, while the flow runs.
	corr: Option<u32>,
	stage: Stage,
	reservation: u64,
	sim_generation: u64,
	context_generation: u64,
	// This service's end of the packet channel NetworkService holds the other end of.
	packets: u64,
	address: u32,
	prefix: u8,
	mtu: u16,
	to_provider: mc::PacketQueue,
	to_network: mc::PacketQueue,
	transmit_refused: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkCall {
	Reserve,
	Install,
	Release,
}

struct LinkPending {
	corr: u32,
	what: LinkCall,
	deadline: u64,
	// Its waiter is gone: an answer is still handled for what it was, and nobody is told.
	abandoned: bool,
}

struct GrantConn {
	id: u32,
	chan: u64,
	owner: u64,
	kind: GrantKind,
	modem: u32,
	modem_id: ModemId,
	sim_generation: u64,
	policy: DataPolicy,
}

struct Observer {
	chan: u64,
	watch: Option<WatchStream>,
}

struct WatchStream {
	chan: u64,
	seq: u32,
	watch: mc::Watch,
}

struct Service {
	incarnation: u64,
	catalogue: u64,
	link: u64,
	modems: Vec<ModemConn>,
	grants: Vec<GrantConn>,
	observers: Vec<Observer>,
	admins: Vec<u64>,
	calls: Vec<Call>,
	context: Option<Context>,
	links: Vec<LinkPending>,
	next_key: u32,
	next_grant: u32,
	next_call: u64,
	next_link_corr: u32,
}

// ------------------------------------------------------------------ the wire, written by hand

// A generated client's request, captured instead of sent: the encoding is the generator's, the sending
// is this service's own - non-blocking, under a correlation of its choosing.
// The capabilities the request carried are recorded too, and are NOT closed on the way: the capture
// never sends, so they are still this service's to send or to close.
struct Capture {
	bytes: Vec<u8>,
	handles: Vec<u64>,
}

impl Transport for &mut Capture {
	fn call(&mut self, request: &[u8], request_handles: &[u64], _reply_handles: &mut Handles, _deadline: u64) -> Result<Vec<u8>, TransportError> {
		self.bytes = request.to_vec();
		self.handles = request_handles.to_vec();
		Err(TransportError::TimedOut)
	}
	fn discard_handles(&mut self, _handles: &[u64]) {}
}

fn captured_device(encode: impl FnOnce(&mut modem_device::Client<&mut Capture>), corr: u32) -> Option<Vec<u8>> {
	let mut capture = Capture { bytes: Vec::new(), handles: Vec::new() };
	encode(&mut modem_device::Client::new(&mut capture));
	stamp(capture.bytes, corr)
}

fn captured_link(encode: impl FnOnce(&mut network_link_admin::Client<&mut Capture>), corr: u32) -> Option<(Vec<u8>, Vec<u64>)> {
	let mut capture = Capture { bytes: Vec::new(), handles: Vec::new() };
	encode(&mut network_link_admin::Client::new(&mut capture));
	Some((stamp(capture.bytes, corr)?, capture.handles))
}

fn stamp(mut bytes: Vec<u8>, corr: u32) -> Option<Vec<u8>> {
	if bytes.len() < 6 {
		return None;
	}
	bytes[2..6].copy_from_slice(&corr.to_le_bytes());
	Some(bytes)
}

// Zero a buffer that carried a secret.
fn scrub(bytes: &mut [u8]) {
	for byte in bytes.iter_mut() {
		// SAFETY: a valid, aligned byte of this slice.
		unsafe { core::ptr::write_volatile(byte, 0) };
	}
}

// `result<T, error>` under a correlation, sent without waiting.
fn reply<T>(chan: u64, corr: u32, result: Result<T, Error>, write: impl FnOnce(&T, &mut wire::VecWriter) -> Option<()>) {
	let mut writer = wire::VecWriter::new();
	let encoded = (|| {
		writer.u32(corr)?;
		match &result {
			Ok(value) => {
				writer.u8(1)?;
				write(value, &mut writer)
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
		let _ = try_send(chan, &bytes, 0);
	}
}

fn kind_of(kind: mc::Kind) -> CommandKind {
	match kind {
		mc::Kind::Query => CommandKind::Query,
		mc::Kind::EnterPin => CommandKind::EnterPin,
		mc::Kind::EnterPuk => CommandKind::EnterPuk,
		mc::Kind::Activate => CommandKind::Activate,
		mc::Kind::Deactivate => CommandKind::Deactivate,
		mc::Kind::Identity => CommandKind::Identity,
	}
}

fn mc_kind(kind: CommandKind) -> mc::Kind {
	match kind {
		CommandKind::Query => mc::Kind::Query,
		CommandKind::EnterPin => mc::Kind::EnterPin,
		CommandKind::EnterPuk => mc::Kind::EnterPuk,
		CommandKind::Activate => mc::Kind::Activate,
		CommandKind::Deactivate => mc::Kind::Deactivate,
		CommandKind::Identity => mc::Kind::Identity,
	}
}

// Why a command was not sent: the modem is gone, or the command logic would not admit it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Refused {
	Gone,
	Logic(mc::Refusal),
}

impl Refused {
	fn error(self) -> Error {
		match self {
			Refused::Gone => Error::Closed,
			Refused::Logic(refused) => refusal(refused),
		}
	}
}

fn refusal(refusal: mc::Refusal) -> Error {
	match refusal {
		mc::Refusal::Busy | mc::Refusal::Reconcile => Error::Again,
		mc::Refusal::UnknownCount => Error::Invalid,
		mc::Refusal::Stale => Error::Stale,
		mc::Refusal::NoContext => Error::NotFound,
	}
}

// What a command that did not come to `done` tells its caller.
fn failure(outcome: mc::Outcome) -> Error {
	match outcome {
		mc::Outcome::Done => Error::Io,
		mc::Outcome::Rejected => Error::Denied,
		mc::Outcome::Failed => Error::Io,
		mc::Outcome::Stale => Error::Stale,
		// A STATE CHANGE THAT MAY HAVE HAPPENED is neither a success nor a failure, and says so.
		mc::Outcome::OutcomeUnknown => Error::CommitUncertain,
	}
}

fn same(a: &ProviderInfo, b: &ProviderInfo) -> bool {
	a.slot == b.slot && a.provider_generation == b.provider_generation && a.binding_generation == b.binding_generation
}

fn sim_state(sim: DeviceSim) -> SimState {
	match sim {
		DeviceSim::Unknown => SimState::Unknown,
		DeviceSim::Absent => SimState::Absent,
		DeviceSim::LockedPin => SimState::LockedPin,
		DeviceSim::LockedPuk => SimState::LockedPuk,
		DeviceSim::Ready => SimState::Ready,
		DeviceSim::Blocked => SimState::Blocked,
	}
}

fn registration(registration: DeviceRegistration) -> Registration {
	match registration {
		DeviceRegistration::Unknown => Registration::Unknown,
		DeviceRegistration::NotRegistered => Registration::NotRegistered,
		DeviceRegistration::Searching => Registration::Searching,
		DeviceRegistration::Home => Registration::Home,
		DeviceRegistration::Roaming => Registration::Roaming,
		DeviceRegistration::Denied => Registration::Denied,
	}
}

fn count(count: mc::Count) -> AttemptCount {
	match count {
		mc::Count::Known { remaining, sim_generation, observed } => AttemptCount { known: true, remaining, sim_generation, observed_ms: observed * 10 },
		mc::Count::Unknown => AttemptCount { known: false, remaining: 0, sim_generation: 0, observed_ms: 0 },
	}
}

fn context_state(state: mc::ContextState) -> ContextState {
	match state {
		mc::ContextState::Inactive => ContextState::Inactive,
		mc::ContextState::Activating => ContextState::Activating,
		mc::ContextState::Active => ContextState::Active,
		mc::ContextState::Deactivating => ContextState::Deactivating,
		mc::ContextState::Unknown => ContextState::Unknown,
	}
}

fn octets(address: u32) -> WireIp {
	let [a, b, c, d] = address.to_be_bytes();
	WireIp { a, b, c, d }
}

// ------------------------------------------------------------------ what a client sees

impl Service {
	fn modem_id(&self, modem: &ModemConn) -> ModemId {
		ModemId { slot: modem.info.slot, generation: modem.info.provider_generation, binding_generation: modem.info.binding_generation, incarnation: self.incarnation }
	}

	// A modem, observed: NO SUBSCRIBER IDENTIFIER, and every measurement with its validity.
	fn status(&self, modem: &ModemConn) -> ModemStatus {
		let id = self.modem_id(modem);
		let (pin, puk) = modem.device.counters();
		let (state, generation) = modem.device.context();
		let context = match &self.context {
			Some(context) if context.modem == modem.key => Some(ContextStatus { id: ContextId { modem: id.clone(), sim_generation: context.sim_generation, context_generation: context.context_generation }, state: if context.stage == Stage::Active { ContextState::Active } else { context_state(state) }, address: (context.stage == Stage::Active).then_some(context.address), prefix: context.prefix, mtu: context.mtu, transmit_refused: context.transmit_refused, receive_dropped: context.to_network.dropped() }),
			// A CONTEXT THE MODEM MAY HOLD THAT NO FLOW OWNS - an outcome nobody could confirm - is shown
			// as what is known of it, never as nothing.
			_ if state != mc::ContextState::Inactive => Some(ContextStatus { id: ContextId { modem: id.clone(), sim_generation: modem.device.sim_generation(), context_generation: generation }, state: context_state(state), address: None, prefix: 0, mtu: 0, transmit_refused: 0, receive_dropped: 0 }),
			_ => None,
		};
		let state = &modem.state;
		ModemStatus { id, manufacturer: state.manufacturer.clone(), model: state.model.clone(), sim: sim_state(state.sim), sim_generation: modem.device.sim_generation(), registration: registration(state.registration), operator: state.operator.clone(), signal: Signal { valid: state.signal_valid, rssi_dbm: if state.signal_valid { state.rssi_dbm } else { 0 }, quality: if state.signal_valid { state.quality } else { 0 } }, attempts: Attempts { pin: count(pin), puk: count(puk) }, context, revision: modem.device.revision() }
	}

	fn limits(&self) -> ModemLimits {
		ModemLimits { providers: self.modems.len() as u8, providers_max: MAX_PROVIDERS as u8, clients: self.clients() as u8, clients_max: MAX_CLIENTS as u8, contexts: u8::from(self.context.is_some()), contexts_max: MAX_CONTEXTS }
	}

	fn clients(&self) -> usize {
		self.observers.len() + self.grants.len()
	}

	fn changed(&mut self) {
		for observer in &mut self.observers {
			if let Some(watch) = &mut observer.watch {
				watch.watch.changed();
			}
		}
	}

	fn find_grant(&self, id: u32) -> Option<&GrantConn> {
		self.grants.iter().find(|grant| grant.id == id)
	}

	fn answer<T>(&self, grant: u32, corr: u32, result: Result<T, Error>, write: impl FnOnce(&T, &mut wire::VecWriter) -> Option<()>) {
		if let Some(grant) = self.find_grant(grant) {
			reply(grant.chan, corr, result, write);
		}
	}

	fn context_status(&self) -> Option<ContextStatus> {
		let context = self.context.as_ref()?;
		let modem = self.modems.iter().find(|modem| modem.key == context.modem)?;
		self.status(modem).context
	}
}

// ------------------------------------------------------------------ providers

impl Service {
	// A publication: open a session, the three streams, and read the first state - bounded, and a
	// provider that does not manage it in time is passed over.
	fn adopt(&mut self, info: ProviderInfo) {
		if self.modems.iter().any(|modem| same(&modem.info, &info)) {
			return;
		}
		if self.modems.len() >= MAX_PROVIDERS {
			print(b"ModemService: a modem was refused: this service holds four (resource exhausted)\n");
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: self.catalogue }).open(&info) else {
			print(b"ModemService: a published modem could not be opened\n");
			return;
		};
		let mut client = modem_device::Client::with_deadline(ChannelTransport { chan }, clock() + OPEN_TICKS);
		let opened = match client.open(&VERSION) {
			Some(Ok(opened)) if opened.limits.version == VERSION => opened,
			_ => {
				print(b"ModemService: a modem did not open a session at this contract's version\n");
				close(chan);
				return;
			}
		};
		let indications = client.indications().unwrap_or(0);
		let receive = client.receive().unwrap_or(0);
		let transmit = match client.transmit() {
			Some(Ok(transmit)) => transmit,
			_ => 0,
		};
		let close_all = || {
			for handle in [indications, receive, transmit, chan] {
				if handle != 0 {
					close(handle);
				}
			}
		};
		if indications == 0 || receive == 0 || transmit == 0 {
			print(b"ModemService: a modem did not open its streams\n");
			close_all();
			return;
		}
		// THE FIRST STATE, which the indication stream opens with: nothing is decided about a modem
		// before its SIM generation is known.
		let mut buf = alloc::vec![0u8; BUF_BYTES];
		let first = if wait(indications, clock() + OPEN_TICKS) == 0 {
			match try_recv_caps(indications, &mut buf) {
				PolledCaps::Message { len, handles } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
					let mut frame_handles = Handles::new();
					modem_device::indications_read(&buf[..len], &mut frame_handles)
				}
				_ => None,
			}
		} else {
			None
		};
		let Some(first) = first else {
			print(b"ModemService: a modem did not report its state\n");
			close_all();
			return;
		};
		// NEGOTIATED DOWN, never up: a provider asking for more than the contract allows gets the maximum.
		let mut limits = opened.limits;
		limits.pending = limits.pending.clamp(1, mc::MAX_PENDING);
		limits.mtu = limits.mtu.clamp(service_logic::uplink::MIN_MTU, mc::MAX_MTU);
		let mut device = mc::Device::open(opened.connection_generation, limits.pending, first.state.sim_generation);
		device.indication(first.revision, first.state.sim_generation, (first.state.pin_attempts, first.state.puk_attempts), first.state.context_active, clock());
		let key = self.next_key;
		self.next_key = self.next_key.wrapping_add(1).max(1);
		self.modems.push(ModemConn { key, info, chan, indications, receive, transmit, limits, device, state: first.state, sent: Vec::new(), next_corr: 1 });
		print(b"ModemService: a modem was admitted\n");
		self.changed();
	}

	// A modem is gone - withdrawn, or its connection closed. Its context ends, and every grant bound to
	// it: a new publication is a new modem and needs fresh grants.
	fn lose(&mut self, key: u32, why: &[u8]) {
		let Some(at) = self.modems.iter().position(|modem| modem.key == key) else { return };
		// The provider is gone, and so is whatever context it held: nothing to deactivate.
		if self.context.as_ref().is_some_and(|context| context.modem == key) {
			self.end_context(Error::Closed, true);
		}
		let modem = self.modems.remove(at);
		for handle in [modem.indications, modem.receive, modem.transmit, modem.chan] {
			close(handle);
		}
		print(b"ModemService: a modem is gone: ");
		print(why);
		print(b"\n");
		let bound: Vec<u32> = self.grants.iter().filter(|grant| grant.modem == key).map(|grant| grant.id).collect();
		for id in bound {
			self.retire(id);
		}
		self.calls.retain(|call| call.modem != key);
		self.changed();
	}

	// Send one command to a modem, without waiting. NOT SENT IS ANSWERED AT ONCE as a failure: the
	// provider never saw it. A PIN or a PUK rides in `secrets`, and the command, its encoding and the
	// buffers it came in are zeroed once it has gone - or failed to.
	fn submit_with(&mut self, key: u32, kind: mc::Kind, purpose: Purpose, sim_generation: u64, acknowledge: bool, apn: &str, secrets: Option<(Secret, Secret)>) -> Result<(), Refused> {
		let Some(at) = self.modems.iter().position(|modem| modem.key == key) else { return Err(Refused::Gone) };
		let id = self.next_call;
		let modem = &mut self.modems[at];
		let command = modem.device.submit(kind, id, sim_generation, acknowledge, clock()).map_err(Refused::Logic)?;
		self.next_call += 1;
		let corr = modem.next_corr;
		modem.next_corr = modem.next_corr.wrapping_add(1).max(1);
		let (secret, new_secret) = match &secrets {
			Some((secret, new_secret)) => (secret.0.clone(), new_secret.0.clone()),
			None => (Vec::new(), Vec::new()),
		};
		let mut wire = Command { connection_generation: command.connection, transaction: command.transaction, kind: kind_of(kind), sim_generation: command.sim_generation, context_generation: command.context_generation, secret, new_secret, apn: String::from(apn) };
		let mut bytes = captured_device(|client| drop(client.command(&wire)), corr);
		scrub(&mut wire.secret);
		scrub(&mut wire.new_secret);
		drop(secrets);
		let sent = bytes.as_ref().is_some_and(|bytes| try_send(modem.chan, bytes, 0));
		if let Some(bytes) = bytes.as_mut() {
			scrub(bytes);
		}
		// Bounded: a provider that never answers cannot grow this past the commands it could hold.
		if modem.sent.len() >= 32 {
			modem.sent.remove(0);
		}
		modem.sent.push((corr, command.transaction, kind, command.sim_generation, command.context_generation));
		self.calls.push(Call { id, modem: key, purpose });
		if !sent {
			// The provider never saw it: answered as a failure, through the same path a reply takes.
			let failed = mc::Reply { connection: command.connection, transaction: command.transaction, kind, status: mc::Outcome::Failed, sim_generation: command.sim_generation, context_generation: command.context_generation, counters: None, context_active: None };
			if let Some(at) = self.modems.iter().position(|modem| modem.key == key) {
				self.modems[at].sent.retain(|sent| sent.1 != command.transaction);
				if let Some(done) = self.modems[at].device.reply(failed, clock()) {
					self.complete(key, done, None);
				}
			}
		}
		Ok(())
	}

	fn submit(&mut self, key: u32, kind: mc::Kind, purpose: Purpose, sim_generation: u64, acknowledge: bool, apn: &str) -> Result<(), Refused> {
		self.submit_with(key, kind, purpose, sim_generation, acknowledge, apn, None)
	}

	// What a provider said on its command connection.
	fn on_reply(&mut self, key: u32, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		loop {
			let Some(at) = self.modems.iter().position(|modem| modem.key == key) else { return Ok(()) };
			let chan = self.modems[at].chan;
			let (len, handles) = match try_recv_caps(chan, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its connection closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut reader = Reader::new(&buf[..len]);
			let Some(corr) = reader.u32() else { return Err(b"a reply carried no correlation") };
			let decoded: Option<Result<CommandReply, Error>> = (|| if reader.tag()? { Some(Ok(CommandReply::read(&mut reader)?)) } else { Some(Err(Error::read(&mut reader)?)) })();
			let Some(decoded) = decoded else { return Err(b"a reply did not decode") };
			let modem = &mut self.modems[at];
			// A REPLY TO NOTHING OUTSTANDING - late, duplicated, or another connection's - is dropped here
			// or by the command logic, and changes nothing either way.
			let Some(place) = modem.sent.iter().position(|sent| sent.0 == corr) else { continue };
			let (_, transaction, kind, sim_generation, context_generation) = modem.sent.remove(place);
			let (reply, payload) = match decoded {
				Ok(answer) => {
					let status = match answer.status {
						CommandStatus::Done => mc::Outcome::Done,
						CommandStatus::Rejected => mc::Outcome::Rejected,
						CommandStatus::Failed => mc::Outcome::Failed,
						CommandStatus::Stale => mc::Outcome::Stale,
					};
					let counters = answer.state.as_ref().map(|state| (state.pin_attempts, state.puk_attempts));
					let context_active = answer.state.as_ref().map(|state| state.context_active);
					(mc::Reply { connection: answer.connection_generation, transaction: answer.transaction, kind: mc_kind(answer.kind), status, sim_generation: answer.sim_generation, context_generation: answer.context_generation, counters, context_active }, Some(answer))
				}
				// Refused on the wire: a failure of that command, under its own generations.
				Err(_) => (mc::Reply { connection: modem.device.connection(), transaction, kind, status: mc::Outcome::Failed, sim_generation, context_generation, counters: None, context_active: None }, None),
			};
			if let Some(done) = modem.device.reply(reply, clock()) {
				if let Some(state) = payload.as_ref().and_then(|answer| answer.state.clone()) {
					modem.state = state;
				}
				self.complete(key, done, payload);
				self.changed();
			}
		}
	}

	// Unsolicited state, at a revision. A NEW SIM ends everything bound to the old one; the network
	// ending the context ends the link with it.
	fn on_indications(&mut self, key: u32, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		loop {
			let Some(at) = self.modems.iter().position(|modem| modem.key == key) else { return Ok(()) };
			let stream = self.modems[at].indications;
			let (len, handles) = match try_recv_caps(stream, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its indication stream closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = Handles::new();
			let Some(indication) = modem_device::indications_read(&buf[..len], &mut frame_handles) else { return Err(b"an indication did not decode") };
			let modem = &mut self.modems[at];
			let old_sim = modem.device.sim_generation();
			let change = modem.device.indication(indication.revision, indication.state.sim_generation, (indication.state.pin_attempts, indication.state.puk_attempts), indication.state.context_active, clock());
			let Some(change) = change else { continue };
			modem.state = indication.state;
			match change {
				mc::SimChange::Updated => {}
				mc::SimChange::Replaced => {
					print(b"ModemService: the SIM was replaced - its context and every grant bound to it end\n");
					if self.context.as_ref().is_some_and(|context| context.modem == key) {
						self.end_context(Error::Stale, false);
					}
					let bound: Vec<u32> = self.grants.iter().filter(|grant| grant.modem == key && grant.sim_generation == old_sim).map(|grant| grant.id).collect();
					for id in bound {
						self.retire(id);
					}
				}
				mc::SimChange::ContextLost => {
					print(b"ModemService: the network ended the context\n");
					if self.context.as_ref().is_some_and(|context| context.modem == key) {
						self.end_context(Error::LinkChanged, false);
					}
				}
			}
			self.changed();
		}
	}

	// Datagrams from the network, to NetworkService - the active context's only, each within the MTU,
	// and what does not fit counted rather than queued.
	fn on_receive(&mut self, key: u32, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		loop {
			let Some(at) = self.modems.iter().position(|modem| modem.key == key) else { return Ok(()) };
			let stream = self.modems[at].receive;
			let (len, handles) = match try_recv_caps(stream, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its receive stream closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = Handles::new();
			let Some(datagram) = modem_device::receive_read(&buf[..len], &mut frame_handles) else { return Err(b"a datagram did not decode") };
			// ONLY THE ACTIVE CONTEXT'S. A datagram tagged for another generation belongs to a context that
			// is over, and is not delivered into this one.
			if let Some(context) = self.context.as_mut()
				&& context.modem == key
				&& context.stage == Stage::Active
				&& context.context_generation == datagram.context_generation
			{
				context.to_network.receive(&datagram.bytes, context.mtu);
			}
		}
	}

	// ------------------------------------------------------------------ completions

	// A command answered, by its reply or by its deadline.
	fn complete(&mut self, key: u32, done: mc::Completion, payload: Option<CommandReply>) {
		let Some(at) = self.calls.iter().position(|call| call.id == done.caller) else { return };
		let call = self.calls.remove(at);
		match call.purpose {
			Purpose::Attempts { grant, corr } => {
				let result = if done.result == mc::Outcome::Done { self.attempts(key).ok_or(Error::Closed) } else { Err(failure(done.result)) };
				self.answer(grant, corr, result, |attempts, w| attempts.write(w));
			}
			Purpose::BeforePin { grant, corr, puk, secret, new_secret, acknowledge } => {
				// The counters are as fresh as the modem can make them; now the attempt, or the refusal to
				// make one against a count nobody knows.
				let Some(sim_generation) = self.find_grant(grant).map(|grant| grant.sim_generation) else { return };
				let kind = if puk { mc::Kind::EnterPuk } else { mc::Kind::EnterPin };
				match self.submit_with(key, kind, Purpose::Pin { grant, corr }, sim_generation, acknowledge, "", Some((secret, new_secret))) {
					Ok(()) => {}
					// AN UNKNOWN COUNT IS NOT ATTEMPTED AGAINST without the caller's acknowledgement, and the
					// answer says that is why - with whatever is known of the counters.
					Err(Refused::Logic(mc::Refusal::UnknownCount)) => {
						let attempts = self.attempts(key).unwrap_or(Attempts { pin: count(mc::Count::Unknown), puk: count(mc::Count::Unknown) });
						self.answer(grant, corr, Ok(SimPinOutcome { result: SimPinResult::UnknownCount, attempts }), |outcome, w| outcome.write(w));
					}
					Err(refused) => self.answer::<SimPinOutcome>(grant, corr, Err(refused.error()), |outcome, w| outcome.write(w)),
				}
			}
			Purpose::Pin { grant, corr } => {
				let attempts = self.attempts(key).unwrap_or(Attempts { pin: count(mc::Count::Unknown), puk: count(mc::Count::Unknown) });
				let blocked = self.modems.iter().find(|modem| modem.key == key).is_some_and(|modem| matches!(modem.state.sim, DeviceSim::LockedPuk | DeviceSim::Blocked));
				let result = match done.result {
					mc::Outcome::Done => Ok(SimPinResult::Accepted),
					mc::Outcome::Rejected if blocked => Ok(SimPinResult::Blocked),
					mc::Outcome::Rejected => Ok(SimPinResult::Rejected),
					// NOT RETRIED. The counters are read again before anything else is attempted.
					mc::Outcome::OutcomeUnknown => {
						self.reconcile(key);
						Ok(SimPinResult::OutcomeUnknown)
					}
					other => Err(failure(other)),
				};
				self.answer(grant, corr, result.map(|result| SimPinOutcome { result, attempts }), |outcome, w| outcome.write(w));
			}
			Purpose::Identity { grant, corr } => {
				let result = match (done.result, payload.and_then(|answer| answer.identity)) {
					(mc::Outcome::Done, Some(identity)) => Ok(Identity { imsi: identity.imsi, iccid: identity.iccid, msisdn: identity.msisdn }),
					(mc::Outcome::Done, None) => Err(Error::Io),
					(other, _) => Err(failure(other)),
				};
				self.answer(grant, corr, result, |identity, w| identity.write(w));
			}
			Purpose::Activate => self.activated(key, done, payload.and_then(|answer| answer.config)),
			Purpose::Deactivate { grant, corr } => self.deactivated(done, Some((grant, corr))),
			Purpose::Cleanup => {
				if done.result == mc::Outcome::OutcomeUnknown {
					self.reconcile(key);
				}
			}
			Purpose::Reconcile => self.reconciled(key),
		}
	}

	fn attempts(&self, key: u32) -> Option<Attempts> {
		let modem = self.modems.iter().find(|modem| modem.key == key)?;
		let (pin, puk) = modem.device.counters();
		Some(Attempts { pin: count(pin), puk: count(puk) })
	}

	// A fresh read-only query, after an outcome nobody could confirm.
	fn reconcile(&mut self, key: u32) {
		let Some(sim_generation) = self.modems.iter().find(|modem| modem.key == key).map(|modem| modem.device.sim_generation()) else { return };
		let _ = self.submit(key, mc::Kind::Query, Purpose::Reconcile, sim_generation, false, "");
	}

	// Reconciled. A context the modem reports up that no flow owns - an activation whose answer never
	// came - is taken down, bounded: nobody holds it, and nothing may carry traffic over it.
	fn reconciled(&mut self, key: u32) {
		let Some(modem) = self.modems.iter().find(|modem| modem.key == key) else { return };
		let owned = self.context.as_ref().is_some_and(|context| context.modem == key);
		if !owned && modem.device.context().0 == mc::ContextState::Active {
			let sim_generation = modem.device.sim_generation();
			let _ = self.submit(key, mc::Kind::Deactivate, Purpose::Cleanup, sim_generation, false, "");
		}
		self.changed();
	}
}

// ------------------------------------------------------------------ the context and the link

impl Service {
	// One link-admin call, sent without waiting. THE ATTACHMENT'S PACKET CHANNEL TRAVELS WITH AN INSTALL:
	// it is the one capability any of these calls carries, and it is moved with the message or, when the
	// message cannot go, closed here - this service never keeps NetworkService's end.
	fn link_call(&mut self, what: LinkCall, encode: impl FnOnce(&mut network_link_admin::Client<&mut Capture>)) -> bool {
		let corr = self.next_link_corr;
		self.next_link_corr = self.next_link_corr.wrapping_add(1).max(1);
		let Some((bytes, handles)) = captured_link(encode, corr) else { return false };
		let carried: u64 = handles.first().copied().unwrap_or(0);
		let sent = self.link != 0 && try_send(self.link, &bytes, carried);
		if !sent {
			for &handle in &handles {
				close(handle);
			}
			return false;
		}
		for &extra in handles.iter().skip(1) {
			close(extra);
		}
		self.links.push(LinkPending { corr, what, deadline: clock() + LINK_TICKS, abandoned: false });
		true
	}

	// A data client asked for the context: RESERVE BEFORE THE MODEM IS ASKED TO DO ANYTHING.
	fn activate(&mut self, grant: u32, corr: u32, modem_id: ModemId) -> Result<(), Error> {
		let Some(held) = self.find_grant(grant) else { return Err(Error::Closed) };
		if held.modem_id != modem_id {
			return Err(Error::Denied);
		}
		let (key, sim_generation, replace) = (held.modem, held.sim_generation, held.policy.replace_uplink);
		let Some(modem) = self.modems.iter().find(|modem| modem.key == key) else { return Err(Error::Closed) };
		if modem.device.sim_generation() != sim_generation {
			return Err(Error::Stale);
		}
		// ONE CONTEXT, GLOBALLY.
		if self.context.is_some() {
			return Err(Error::Again);
		}
		if self.link == 0 {
			return Err(Error::Io);
		}
		self.context = Some(Context { modem: key, grant, corr: Some(corr), stage: Stage::Reserving, reservation: 0, sim_generation, context_generation: 0, packets: 0, address: 0, prefix: 0, mtu: 0, to_provider: mc::PacketQueue::default(), to_network: mc::PacketQueue::default(), transmit_refused: 0 });
		if !self.link_call(LinkCall::Reserve, |client| {
			let _ = client.reserve(&replace);
		}) {
			self.context = None;
			return Err(Error::Io);
		}
		Ok(())
	}

	// The flow failed before the context was installed: its caller is told, the reservation is given
	// back, and nothing is left behind.
	fn abandon(&mut self, error: Error) {
		let Some(context) = self.context.take() else { return };
		if let Some(corr) = context.corr {
			self.answer::<ContextStatus>(context.grant, corr, Err(error), |status, w| status.write(w));
		}
		if context.packets != 0 {
			close(context.packets);
		}
		if context.reservation != 0 {
			let reservation = context.reservation;
			self.link_call(LinkCall::Release, |client| {
				let _ = client.release(&reservation);
			});
		}
		self.changed();
	}

	// The modem answered the activation.
	fn activated(&mut self, key: u32, done: mc::Completion, config: Option<IpConfig>) {
		let flowing = self.context.as_ref().is_some_and(|context| context.modem == key && context.stage == Stage::Activating);
		if !flowing {
			// NOBODY IS WAITING FOR THIS CONTEXT ANY MORE - its owner, its grant or its link went while the
			// modem worked. What it brought up is taken down again, bounded; what it may have brought up is
			// reconciled first.
			match done.result {
				mc::Outcome::Done => {
					let sim_generation = self.modems.iter().find(|modem| modem.key == key).map_or(0, |modem| modem.device.sim_generation());
					let _ = self.submit(key, mc::Kind::Deactivate, Purpose::Cleanup, sim_generation, false, "");
				}
				mc::Outcome::OutcomeUnknown => self.reconcile(key),
				_ => {}
			}
			return;
		}
		if let Some(context) = self.context.as_mut() {
			context.context_generation = done.context_generation;
		}
		let config = match (done.result, config) {
			(mc::Outcome::Done, Some(config)) => config,
			// Up, and with no configuration to install: taken down again, bounded.
			(mc::Outcome::Done, None) => {
				self.install_failed(Error::Io);
				return;
			}
			(mc::Outcome::OutcomeUnknown, _) => {
				self.abandon(Error::CommitUncertain);
				self.reconcile(key);
				return;
			}
			(other, _) => {
				self.abandon(failure(other));
				return;
			}
		};
		let Some(modem) = self.modems.iter().find(|modem| modem.key == key) else { return };
		let provider = LinkProvider { slot: modem.info.slot, generation: modem.info.provider_generation, binding_generation: modem.info.binding_generation };
		let mtu = config.mtu.min(modem.limits.mtu);
		let Some((mine, theirs)) = channel_with_depth(mc::QUEUE_PACKETS as u64) else {
			self.install_failed(Error::Exhausted);
			return;
		};
		let Some(context) = self.context.as_mut() else {
			close(mine);
			close(theirs);
			return;
		};
		context.packets = mine;
		context.address = config.address;
		context.prefix = config.prefix;
		context.mtu = mtu;
		context.stage = Stage::Installing;
		// NETWORKSERVICE VALIDATES AND OWNS WHAT IT INSTALLS. An IPv6-only configuration goes as what it
		// is, and is refused there as unsupported - which is a failed installation like any other.
		let attachment = LinkAttachment { provider, sim_generation: context.sim_generation, context_generation: context.context_generation, packets: theirs, family: if config.family == IpFamily::Ipv6 { LinkFamily::Ipv6 } else { LinkFamily::Ipv4 }, address: octets(config.address), prefix: config.prefix, gateway: config.gateway.map(octets), dns: config.dns.iter().take(2).map(|&server| octets(server)).collect(), mtu };
		let reservation = context.reservation;
		if !self.link_call(LinkCall::Install, |client| drop(client.install(&reservation, &attachment))) {
			self.install_failed(Error::Io);
		}
	}

	// POST-ACTIVATION INSTALLATION FAILURE: a bounded deactivation, the reservation given back, and the
	// caller told why - the context was never usable.
	fn install_failed(&mut self, error: Error) {
		let Some(context) = self.context.as_ref() else { return };
		let (key, sim_generation) = (context.modem, context.sim_generation);
		self.abandon(error);
		let _ = self.submit(key, mc::Kind::Deactivate, Purpose::Cleanup, sim_generation, false, "");
	}

	// NetworkService committed the installation: now, and only now, the context is active.
	fn installed(&mut self, installed: LinkInstalled) {
		let Some(context) = self.context.as_mut() else { return };
		if context.stage != Stage::Installing {
			return;
		}
		context.stage = Stage::Active;
		context.mtu = installed.mtu;
		let (grant, corr) = (context.grant, context.corr.take());
		print(b"ModemService: the context is installed as the uplink\n");
		if let (Some(corr), Some(status)) = (corr, self.context_status()) {
			self.answer(grant, corr, Ok(status), |status, w| status.write(w));
		}
		self.changed();
	}

	// A data client asked to end its context.
	fn deactivate(&mut self, grant: u32, corr: u32, id: ContextId) -> Result<(), Error> {
		let Some(context) = self.context.as_ref() else { return Err(Error::NotFound) };
		if context.grant != grant {
			return Err(Error::Denied);
		}
		let Some(modem) = self.modems.iter().find(|modem| modem.key == context.modem) else { return Err(Error::Closed) };
		if id.modem != self.modem_id(modem) || id.sim_generation != context.sim_generation || id.context_generation != context.context_generation {
			return Err(Error::Stale);
		}
		if context.stage != Stage::Active {
			return Err(Error::Again);
		}
		let (key, sim_generation) = (context.modem, context.sim_generation);
		self.submit(key, mc::Kind::Deactivate, Purpose::Deactivate { grant, corr }, sim_generation, false, "").map_err(Refused::error)?;
		if let Some(context) = self.context.as_mut() {
			context.stage = Stage::Deactivating;
		}
		self.changed();
		Ok(())
	}

	fn deactivated(&mut self, done: mc::Completion, caller: Option<(u32, u32)>) {
		match done.result {
			mc::Outcome::Done => {
				self.end_context(Error::Closed, true);
				if let Some((grant, corr)) = caller {
					self.answer(grant, corr, Ok(()), |_, _| Some(()));
				}
			}
			// NOT CONFIRMED EITHER WAY: the link cannot be trusted with traffic, so it goes; the modem's
			// context is unknown until a query says otherwise.
			mc::Outcome::OutcomeUnknown => {
				let key = self.context.as_ref().map(|context| context.modem);
				self.end_context(Error::CommitUncertain, true);
				if let Some(key) = key {
					self.reconcile(key);
				}
				if let Some((grant, corr)) = caller {
					self.answer::<()>(grant, corr, Err(Error::CommitUncertain), |_, _| Some(()));
				}
			}
			other => {
				// Refused: the context is still up and still installed.
				if let Some(context) = self.context.as_mut()
					&& context.stage == Stage::Deactivating
				{
					context.stage = Stage::Active;
				}
				if let Some((grant, corr)) = caller {
					self.answer::<()>(grant, corr, Err(failure(other)), |_, _| Some(()));
				}
			}
		}
		self.changed();
	}

	// END THE CONTEXT: the link is released, queued datagrams are dropped, and whoever was waiting is
	// told with a terminal reason. When the modem may still hold it and nobody asked - its owner, grant
	// or link went away - a bounded deactivation follows. A reconnect needs a fresh activation.
	fn end_context(&mut self, reason: Error, deactivated: bool) {
		let Some(context) = self.context.take() else { return };
		if let Some(corr) = context.corr {
			self.answer::<ContextStatus>(context.grant, corr, Err(reason), |status, w| status.write(w));
		}
		if context.packets != 0 {
			close(context.packets);
		}
		if context.reservation != 0 {
			let reservation = context.reservation;
			self.link_call(LinkCall::Release, |client| {
				let _ = client.release(&reservation);
			});
		}
		let modem_up = self.modems.iter().find(|modem| modem.key == context.modem).is_some_and(|modem| modem.device.context().0 == mc::ContextState::Active);
		if !deactivated && modem_up {
			let _ = self.submit(context.modem, mc::Kind::Deactivate, Purpose::Cleanup, context.sim_generation, false, "");
		}
		print(b"ModemService: the context ended\n");
		self.changed();
	}

	// NetworkService answered a link-admin call.
	fn on_link(&mut self, buf: &mut [u8]) {
		loop {
			let (len, handles) = match try_recv_caps(self.link, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					// NETWORKSERVICE IS GONE, and every link it held with it.
					print(b"ModemService: the link administration closed - no context can be installed\n");
					close(self.link);
					self.link = 0;
					self.links.clear();
					self.end_context(Error::LinkChanged, false);
					return;
				}
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut reader = Reader::new(&buf[..len]);
			let Some(corr) = reader.u32() else { continue };
			let Some(at) = self.links.iter().position(|pending| pending.corr == corr) else { continue };
			let pending = self.links.remove(at);
			let Some(ok) = reader.tag() else { continue };
			match pending.what {
				LinkCall::Reserve => {
					let reservation = if ok { reader.u64() } else { None };
					let error = if ok { None } else { Error::read(&mut reader) };
					// A RESERVATION NOBODY IS WAITING FOR ANY MORE is given straight back.
					if pending.abandoned {
						if let Some(reservation) = reservation {
							self.link_call(LinkCall::Release, |client| {
								let _ = client.release(&reservation);
							});
						}
						continue;
					}
					self.reserved(reservation, error.unwrap_or(Error::Io));
				}
				LinkCall::Install => {
					if pending.abandoned {
						continue;
					}
					if ok {
						match LinkInstalled::read(&mut reader) {
							Some(installed) => self.installed(installed),
							None => self.install_failed(Error::Io),
						}
					} else {
						let error = Error::read(&mut reader).unwrap_or(Error::Io);
						self.install_failed(error);
					}
				}
				LinkCall::Release => {}
			}
		}
	}

	// Admission answered: activate now, or tell the caller why not - the current link untouched.
	fn reserved(&mut self, reservation: Option<u64>, error: Error) {
		if !self.context.as_ref().is_some_and(|context| context.stage == Stage::Reserving) {
			// Nobody is waiting for this reservation: it goes straight back.
			if let Some(reservation) = reservation {
				self.link_call(LinkCall::Release, |client| {
					let _ = client.release(&reservation);
				});
			}
			return;
		}
		// `again` is NetworkService's busy: another uplink is selected and this grant may not replace it.
		let Some(reservation) = reservation else {
			self.abandon(error);
			return;
		};
		let Some(context) = self.context.as_mut() else { return };
		context.reservation = reservation;
		context.stage = Stage::Activating;
		let (key, grant, sim_generation) = (context.modem, context.grant, context.sim_generation);
		let apn = self.find_grant(grant).map(|grant| grant.policy.apn.clone()).unwrap_or_default();
		if let Err(refused) = self.submit(key, mc::Kind::Activate, Purpose::Activate, sim_generation, false, &apn) {
			self.abandon(refused.error());
		}
		self.changed();
	}

	// Deadlines on link-admin calls. What was waiting is told; a late answer is handled for what it was.
	fn expire_links(&mut self) {
		let now = clock();
		let expired: Vec<LinkCall> = self
			.links
			.iter_mut()
			.filter(|pending| !pending.abandoned && pending.deadline <= now)
			.map(|pending| {
				pending.abandoned = true;
				pending.what
			})
			.collect();
		for what in expired {
			match what {
				LinkCall::Reserve if self.context.as_ref().is_some_and(|context| context.stage == Stage::Reserving) => self.abandon(Error::TimedOut),
				LinkCall::Install if self.context.as_ref().is_some_and(|context| context.stage == Stage::Installing) => self.install_failed(Error::TimedOut),
				_ => {}
			}
		}
		// Abandoned calls are kept until their answer comes, bounded by the handful a context can make.
		if self.links.len() > 16 {
			self.links.remove(0);
		}
	}

	// Datagrams from NetworkService, toward the provider - bounded, and refused past the bound.
	fn on_packets(&mut self, buf: &mut [u8]) {
		loop {
			let Some(context) = self.context.as_mut() else { return };
			if context.packets == 0 {
				return;
			}
			match try_recv_caps(context.packets, buf) {
				PolledCaps::Message { len, handles } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
					if context.stage == Stage::Active && context.to_provider.push(&buf[..len], context.mtu).is_err() {
						context.transmit_refused += 1;
					}
				}
				PolledCaps::Empty => return,
				// NetworkService let the link go: the context has nowhere to carry traffic, and ends.
				PolledCaps::Closed => {
					self.end_context(Error::LinkChanged, false);
					return;
				}
			}
		}
	}

	// Move what is queued, in both directions, as far as each side takes it now.
	fn drain_packets(&mut self) {
		let Some(context) = self.context.as_mut() else { return };
		if context.stage != Stage::Active {
			return;
		}
		let Some(modem) = self.modems.iter().find(|modem| modem.key == context.modem) else { return };
		let transmit = modem.transmit;
		while let Some(packet) = context.to_provider.front() {
			let Some(frame) = (Datagram { context_generation: context.context_generation, bytes: packet.to_vec() }).encode_vec() else {
				context.to_provider.pop();
				continue;
			};
			match try_send_outcome(transmit, &frame, 0) {
				SendOutcome::Delivered => {
					context.to_provider.pop();
				}
				SendOutcome::Stalled => break,
				SendOutcome::Failed => {
					context.to_provider.clear();
					break;
				}
			}
		}
		let mut closed = false;
		while let Some(packet) = context.to_network.front() {
			match try_send_outcome(context.packets, packet, 0) {
				SendOutcome::Delivered => {
					context.to_network.pop();
				}
				SendOutcome::Stalled => break,
				SendOutcome::Failed => {
					closed = true;
					break;
				}
			}
		}
		if closed {
			self.end_context(Error::LinkChanged, false);
		}
	}

	fn backlog(&self) -> bool {
		self.context.as_ref().is_some_and(|context| !context.to_provider.is_empty() || !context.to_network.is_empty())
	}
}

// ------------------------------------------------------------------ grants

impl Service {
	// A GRANT ENDS: its connection closes, its owner's observer is let go, and the context it owns ends
	// with it - whatever copies of its endpoint live on elsewhere.
	fn retire(&mut self, id: u32) {
		let Some(at) = self.grants.iter().position(|grant| grant.id == id) else { return };
		if self.context.as_ref().is_some_and(|context| context.grant == id) {
			self.end_context(Error::Closed, false);
		}
		let grant = self.grants.remove(at);
		close(grant.chan);
		close(grant.owner);
		self.calls.retain(|call| !matches!(call.purpose, Purpose::Attempts { grant, .. } | Purpose::BeforePin { grant, .. } | Purpose::Pin { grant, .. } | Purpose::Identity { grant, .. } | Purpose::Deactivate { grant, .. } if grant == id));
	}
}

// PermissionManager's minting endpoint.
struct AdminView<'a> {
	service: &'a mut Service,
}

impl modem_admin::Service for AdminView<'_> {
	// THE ALIAS RESOLVES TO EXACTLY ONE CURRENT PUBLICATION OR THE MINT FAILS, and the connection is bound
	// to that modem and the SIM in it now.
	fn mint(&mut self, kind: GrantKind, alias: String, policy: DataPolicy, owner: u64) -> Result<ModemGrant, Error> {
		let refuse = |error: Error| {
			close(owner);
			Err(error)
		};
		let Some(&(_, bound)) = ALIASES.iter().find(|(name, _)| *name == alias) else { return refuse(Error::NotFound) };
		let service = &mut *self.service;
		let matching: Vec<usize> = service.modems.iter().enumerate().filter(|(_, modem)| modem.info.name.as_bytes() == bound).map(|(at, _)| at).collect();
		let at = match matching.as_slice() {
			[at] => *at,
			[] => return refuse(Error::NotFound),
			_ => return refuse(Error::Invalid),
		};
		if service.clients() >= MAX_CLIENTS {
			return refuse(Error::Exhausted);
		}
		// A DATA POLICY IS FOR DATA. The other two authorities carry none.
		if kind != GrantKind::Data && (!policy.apn.is_empty() || policy.replace_uplink) {
			return refuse(Error::Invalid);
		}
		let Some((mine, theirs)) = channel() else { return refuse(Error::Exhausted) };
		let id = service.next_grant;
		service.next_grant = service.next_grant.wrapping_add(1).max(1);
		let modem = &service.modems[at];
		let modem_id = service.modem_id(modem);
		let sim_generation = modem.device.sim_generation();
		let key = modem.key;
		service.grants.push(GrantConn { id, chan: mine, owner, kind, modem: key, modem_id: modem_id.clone(), sim_generation, policy });
		Ok(ModemGrant { connection: theirs, modem: modem_id, sim_generation })
	}
}

// What a grant's connection asked for that is answered once the modem or NetworkService has.
enum Asked {
	Activate(ModemId),
	Deactivate(ContextId),
	Identity,
	Attempts,
	Pin(bool, Secret, Secret, bool),
}

struct GrantView<'a> {
	service: &'a Service,
	grant: usize,
	asked: Option<Asked>,
}

impl GrantView<'_> {
	fn grant(&self) -> &GrantConn {
		&self.service.grants[self.grant]
	}
	fn owns(&self, modem: &ModemId) -> Result<(), Error> {
		if *modem != self.grant().modem_id { Err(Error::Denied) } else { Ok(()) }
	}
}

impl modem_data::Service for GrantView<'_> {
	fn activate(&mut self, modem: ModemId) -> Result<ContextStatus, Error> {
		self.asked = Some(Asked::Activate(modem));
		Err(Error::Again)
	}
	fn deactivate(&mut self, context: ContextId) -> Result<(), Error> {
		self.asked = Some(Asked::Deactivate(context));
		Err(Error::Again)
	}
	fn status(&mut self) -> Result<ModemStatus, Error> {
		let key = self.grant().modem;
		let modem = self.service.modems.iter().find(|modem| modem.key == key).ok_or(Error::Closed)?;
		Ok(self.service.status(modem))
	}
}

impl modem_identity::Service for GrantView<'_> {
	fn identity(&mut self, modem: ModemId) -> Result<Identity, Error> {
		self.owns(&modem)?;
		self.asked = Some(Asked::Identity);
		Err(Error::Again)
	}
}

impl modem_manage::Service for GrantView<'_> {
	fn attempts(&mut self, modem: ModemId) -> Result<Attempts, Error> {
		self.owns(&modem)?;
		self.asked = Some(Asked::Attempts);
		Err(Error::Again)
	}
	fn enter_pin(&mut self, modem: ModemId, pin: String, acknowledge_unknown: bool) -> Result<SimPinOutcome, Error> {
		let secret = Secret(pin.into_bytes());
		self.owns(&modem)?;
		self.asked = Some(Asked::Pin(false, secret, Secret(Vec::new()), acknowledge_unknown));
		Err(Error::Again)
	}
	fn enter_puk(&mut self, modem: ModemId, puk: String, new_pin: String, acknowledge_unknown: bool) -> Result<SimPinOutcome, Error> {
		let (secret, new_secret) = (Secret(puk.into_bytes()), Secret(new_pin.into_bytes()));
		self.owns(&modem)?;
		self.asked = Some(Asked::Pin(true, secret, new_secret, acknowledge_unknown));
		Err(Error::Again)
	}
}

// An observation connection.
struct ObserverView<'a> {
	service: &'a Service,
}

impl modem::Service for ObserverView<'_> {
	fn modems(&mut self) -> Result<Vec<ModemStatus>, Error> {
		Ok(self.service.modems.iter().map(|modem| self.service.status(modem)).collect())
	}
	fn limits(&mut self) -> Result<ModemLimits, Error> {
		Ok(self.service.limits())
	}
	fn watch(&mut self) -> Result<Vec<ModemWatch>, Error> {
		Ok(Vec::new())
	}
}

impl Service {
	// A request on a grant's connection. False when the connection cannot carry it, which closes it.
	fn grant_request(&mut self, at: usize, request: &[u8], handles: &mut Handles, reply_buf: &mut [u8]) -> bool {
		let (chan, id, kind) = (self.grants[at].chan, self.grants[at].id, self.grants[at].kind);
		let mut view = GrantView { service: self, grant: at, asked: None };
		let mut reply_handles = Handles::new();
		let written = match kind {
			GrantKind::Data => modem_data::dispatch(&mut view, request, handles, reply_buf, &mut reply_handles),
			GrantKind::Identity => modem_identity::dispatch(&mut view, request, handles, reply_buf, &mut reply_handles),
			GrantKind::Manage => modem_manage::dispatch(&mut view, request, handles, reply_buf, &mut reply_handles),
		};
		let asked = view.asked.take();
		// The request buffer carried the secret; it is not read again.
		let Some(written) = written else { return false };
		let Some(asked) = asked else {
			if !send_caps_blocking(chan, &reply_buf[..written], reply_handles.as_slice()) {
				for &leftover in reply_handles.as_slice() {
					close(leftover);
				}
			}
			return true;
		};
		scrub(&mut reply_buf[..written]);
		let corr = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
		let key = self.grants[at].modem;
		let sim_generation = self.grants[at].sim_generation;
		let result: Result<(), Error> = match asked {
			Asked::Activate(modem) => self.activate(id, corr, modem),
			Asked::Deactivate(context) => self.deactivate(id, corr, context),
			Asked::Identity => self.submit(key, mc::Kind::Identity, Purpose::Identity { grant: id, corr }, sim_generation, false, "").map_err(Refused::error),
			Asked::Attempts => self.submit(key, mc::Kind::Query, Purpose::Attempts { grant: id, corr }, sim_generation, false, "").map_err(Refused::error),
			// QUERY BEFORE AN ATTEMPT: the counters are read fresh, and the attempt follows the answer.
			Asked::Pin(puk, secret, new_secret, acknowledge) => self.submit(key, mc::Kind::Query, Purpose::BeforePin { grant: id, corr, puk, secret, new_secret, acknowledge }, sim_generation, false, "").map_err(Refused::error),
		};
		if let Err(error) = result {
			reply::<()>(chan, corr, Err(error), |_, _| Some(()));
		}
		true
	}

	// `watch`: the newest snapshot now, and after that the newest again whenever something changed -
	// never a queue of transitions, and marked when transitions were coalesced away.
	fn watch(&mut self, at: usize, request: &[u8], handles: &mut Handles, reply_buf: &mut [u8]) -> bool {
		let chan = self.observers[at].chan;
		let mut view = ObserverView { service: self };
		let Some((corr, _)) = modem::watch_open(&mut view, request, handles) else { return false };
		if self.observers[at].watch.is_some() {
			if let Some(len) = modem::watch_reply_err(corr, &Error::Exhausted, reply_buf) {
				let _ = try_send(chan, &reply_buf[..len], 0);
			}
			return true;
		}
		let Some((producer, consumer)) = channel_with_depth(WATCH_DEPTH) else {
			if let Some(len) = modem::watch_reply_err(corr, &Error::Exhausted, reply_buf) {
				let _ = try_send(chan, &reply_buf[..len], 0);
			}
			return true;
		};
		match modem::watch_reply_ok(corr, reply_buf) {
			Some(len) if send_caps_blocking(chan, &reply_buf[..len], &[consumer]) => {
				let mut watch = mc::Watch::default();
				watch.changed();
				self.observers[at].watch = Some(WatchStream { chan: producer, seq: 0, watch });
			}
			_ => {
				close(producer);
				close(consumer);
			}
		}
		true
	}

	fn drain_watches(&mut self) {
		if !self.watches_pending() {
			return;
		}
		let modems: Vec<ModemStatus> = self.modems.iter().map(|modem| self.status(modem)).collect();
		for observer in &mut self.observers {
			let Some(stream) = observer.watch.as_mut() else { continue };
			let Some(kind) = stream.watch.take() else { continue };
			let item = ModemWatch { kind: if kind == mc::WatchKind::Resync { WatchKind::Resync } else { WatchKind::Snapshot }, modems: modems.clone() };
			let mut frame = alloc::vec![0u8; BUF_BYTES];
			let mut frame_handles = Handles::new();
			let Some(len) = modem::watch_frame(stream.seq, &item, &mut frame, &mut frame_handles) else { continue };
			match try_send_outcome(stream.chan, &frame[..len], 0) {
				SendOutcome::Delivered => stream.seq = stream.seq.wrapping_add(1),
				SendOutcome::Stalled => stream.watch.requeue(kind),
				SendOutcome::Failed => {
					close(stream.chan);
					observer.watch = None;
				}
			}
		}
	}

	fn watches_pending(&self) -> bool {
		// A stalled watch is retried on a short deadline rather than waited for: a watch frame's room
		// is not something `wait_any` can report.
		self.observers.iter().any(|observer| observer.watch.as_ref().is_some_and(|stream| stream.watch.is_pending()))
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// The roles: a catalogue connection minted for `modem` alone, the observation root, the minting root
	// PermissionManager reaches through the broker, and this service's link-admin connection.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (catalogue, serve_root, admin_root, link) = (roles[0], roles[1], roles[2], roles[3]);
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Modem).unwrap_or(0) } else { 0 };
	// THIS INSTANCE'S INCARNATION, in every modem handle it issues: a handle from before a restart names
	// nothing after it.
	let mut drawn = [0u8; 8];
	random_get(&mut drawn);
	let mut service = Service { incarnation: u64::from_le_bytes(drawn) | 1, catalogue, link, modems: Vec::new(), grants: Vec::new(), observers: Vec::new(), admins: Vec::new(), calls: Vec::new(), context: None, links: Vec::new(), next_key: 1, next_grant: 1, next_call: 1, next_link_corr: 1 };
	send_blocking(bootstrap, b"ModemService: online", 0);

	let mut buf = alloc::vec![0u8; BUF_BYTES];
	let mut reply_buf = alloc::vec![0u8; BUF_BYTES];
	let mut subscribed = subscription != 0;
	loop {
		let mut waitset: Vec<u64> = Vec::new();
		for root in [serve_root, admin_root, service.link] {
			if root != 0 {
				waitset.push(root);
			}
		}
		if subscribed {
			waitset.push(subscription);
		}
		waitset.extend(service.admins.iter().copied());
		for modem in &service.modems {
			waitset.extend([modem.chan, modem.indications, modem.receive]);
		}
		for grant in &service.grants {
			waitset.extend([grant.chan, grant.owner]);
		}
		for observer in &service.observers {
			waitset.push(observer.chan);
			if let Some(stream) = &observer.watch {
				waitset.push(stream.chan);
			}
		}
		if let Some(context) = &service.context
			&& context.packets != 0
		{
			waitset.push(context.packets);
		}
		let now = clock();
		let deadlines = [
			service.modems.iter().filter_map(|modem| modem.device.next_deadline()).min(),
			service.links.iter().filter(|pending| !pending.abandoned).map(|pending| pending.deadline).min(),
			(service.backlog() || service.watches_pending()).then_some(now + 1),
		];
		let deadline = deadlines.into_iter().flatten().min().map_or(0, |deadline| deadline.max(now + 1));
		let ready = wait_any(&waitset, deadline);
		// Deadlines first: a command that expired is answered before anything else is decided.
		let now = clock();
		let keys: Vec<u32> = service.modems.iter().map(|modem| modem.key).collect();
		for key in keys {
			let Some(at) = service.modems.iter().position(|modem| modem.key == key) else { continue };
			let expired = service.modems[at].device.tick(now);
			if !expired.is_empty() {
				for done in expired {
					service.complete(key, done, None);
				}
				service.changed();
			}
		}
		service.expire_links();
		if ready >= 0 {
			serve(&mut service, waitset[ready as usize], serve_root, admin_root, subscription, &mut subscribed, &mut buf, &mut reply_buf);
		}
		service.drain_packets();
		service.drain_watches();
	}
}

#[allow(clippy::too_many_arguments)]
fn serve(service: &mut Service, handle: u64, serve_root: u64, admin_root: u64, subscription: u64, subscribed: &mut bool, buf: &mut [u8], reply_buf: &mut [u8]) {
	if let Some(key) = service.modems.iter().find(|modem| modem.chan == handle).map(|modem| modem.key) {
		if let Err(why) = service.on_reply(key, buf) {
			service.lose(key, why);
		}
		return;
	}
	if let Some(key) = service.modems.iter().find(|modem| modem.indications == handle).map(|modem| modem.key) {
		if let Err(why) = service.on_indications(key, buf) {
			service.lose(key, why);
		}
		return;
	}
	if let Some(key) = service.modems.iter().find(|modem| modem.receive == handle).map(|modem| modem.key) {
		if let Err(why) = service.on_receive(key, buf) {
			service.lose(key, why);
		}
		return;
	}
	if handle == service.link && handle != 0 {
		service.on_link(buf);
		return;
	}
	if service.context.as_ref().is_some_and(|context| context.packets == handle) {
		service.on_packets(buf);
		return;
	}
	// A GRANT'S OWNER ENDED: everything it held ends with it, whatever copies of its endpoint live on.
	if let Some(id) = service.grants.iter().find(|grant| grant.owner == handle).map(|grant| grant.id) {
		print(b"ModemService: a grant's owner ended - its grant is retired\n");
		service.retire(id);
		service.changed();
		return;
	}
	if let Some(at) = service.grants.iter().position(|grant| grant.chan == handle) {
		let (len, mut handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				let id = service.grants[at].id;
				service.retire(id);
				service.changed();
				return;
			}
		};
		let understood = len >= 6 && service.grant_request(at, &buf[..len], &mut handles, reply_buf);
		// THE REQUEST MAY HAVE CARRIED A PIN: the buffer it arrived in is zeroed before anything else
		// uses it.
		scrub(&mut buf[..len]);
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		// A REQUEST THIS CONNECTION CANNOT CARRY CLOSES IT, rather than being answered with silence.
		if !understood && let Some(id) = service.grants.get(at).map(|grant| grant.id) {
			service.retire(id);
			service.changed();
		}
		return;
	}
	if let Some(at) = service.observers.iter().position(|observer| observer.watch.as_ref().is_some_and(|stream| stream.chan == handle)) {
		if let PolledCaps::Closed = try_recv_caps(handle, buf) {
			close(handle);
			service.observers[at].watch = None;
		}
		return;
	}
	if let Some(at) = service.observers.iter().position(|observer| observer.chan == handle) {
		let (len, mut handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				let observer = service.observers.remove(at);
				if let Some(stream) = observer.watch {
					close(stream.chan);
				}
				close(observer.chan);
				return;
			}
		};
		// AN OBSERVATION CONNECTION MINTS ANOTHER, as the root does: a resolver keeps the connection the
		// broker minted for it and mints its own from that one. Counted against the same 32 clients.
		if len >= 2 && u16::from_le_bytes([buf[0], buf[1]]) == CONNECT_OP {
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			match channel().filter(|_| service.clients() < MAX_CLIENTS) {
				Some((mine, theirs)) => {
					service.observers.push(Observer { chan: mine, watch: None });
					send_blocking(handle, &[], theirs);
				}
				None => {
					send_blocking(handle, &[], 0);
				}
			}
			return;
		}
		let understood = if len >= 6 && u16::from_le_bytes([buf[0], buf[1]]) == modem::OP_WATCH {
			service.watch(at, &buf[..len], &mut handles, reply_buf)
		} else {
			let mut reply_handles = Handles::new();
			match modem::dispatch(&mut ObserverView { service }, &buf[..len], &mut handles, reply_buf, &mut reply_handles) {
				Some(written) => {
					if !send_caps_blocking(handle, &reply_buf[..written], reply_handles.as_slice()) {
						for &leftover in reply_handles.as_slice() {
							close(leftover);
						}
					}
					true
				}
				None => false,
			}
		};
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		if !understood && let Some(at) = service.observers.iter().position(|observer| observer.chan == handle) {
			let observer = service.observers.remove(at);
			if let Some(stream) = observer.watch {
				close(stream.chan);
			}
			close(observer.chan);
		}
		return;
	}
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
			let mut frame_handles = Handles::new();
			let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else { continue };
			if info.live {
				service.adopt(info);
			} else if let Some(key) = service.modems.iter().find(|modem| same(&modem.info, &info)).map(|modem| modem.key) {
				service.lose(key, b"its publication was withdrawn");
			}
		}
		return;
	}
	let is_serve = handle == serve_root;
	let is_admin_root = handle == admin_root;
	if !is_serve && !is_admin_root && !service.admins.contains(&handle) {
		return;
	}
	let (len, mut handles) = match try_recv_caps(handle, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			if !is_serve && !is_admin_root {
				service.admins.retain(|&admin| admin != handle);
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
		// A CONNECTION PER CALLER. An observer counts against the 32 clients, and past them is refused
		// with no capability rather than a channel nobody serves.
		if op == CONNECT_OP && is_serve {
			if service.clients() >= MAX_CLIENTS {
				send_blocking(handle, &[], 0);
				return;
			}
			match channel() {
				Some((mine, theirs)) => {
					service.observers.push(Observer { chan: mine, watch: None });
					send_blocking(handle, &[], theirs);
				}
				None => {
					send_blocking(handle, &[], 0);
				}
			}
			return;
		}
		// FROM THE ROOT OR FROM A CONNECTION IT MINTED: a resolver keeps the connection the broker minted
		// for it and mints its own from that one, as every root's connections allow - refusing it here
		// refused every grant the resolver was asked for.
		if op == CONNECT_OP && !is_serve {
			if service.admins.len() >= MAX_ADMINS {
				send_blocking(handle, &[], 0);
				return;
			}
			match channel() {
				Some((mine, theirs)) => {
					service.admins.push(mine);
					send_blocking(handle, &[], theirs);
				}
				None => {
					send_blocking(handle, &[], 0);
				}
			}
			return;
		}
	}
	if is_serve || is_admin_root {
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		return;
	}
	let mut reply_handles = Handles::new();
	let written = modem_admin::dispatch(&mut AdminView { service }, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	match written {
		Some(written) => {
			if !send_caps_blocking(handle, &reply_buf[..written], reply_handles.as_slice()) {
				for &leftover in reply_handles.as_slice() {
					close(leftover);
				}
			}
		}
		None => {
			service.admins.retain(|&admin| admin != handle);
			close(handle);
		}
	}
}
