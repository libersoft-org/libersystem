// TpmService - the machine's one TPM, for components PermissionManager minted a TPM grant for.
//
// WHAT IT HOLDS AND WHAT IT DOES NOT. It consumes the `tpm` publication through a catalogue connection minted for
// that kind alone - the TPM driver owns the transport and runs each operation whole - and this service owns who may
// do what. It serves ONE root, ADMIN, the minting endpoint PermissionManager alone resolves through the broker;
// every application connection is minted there, for one of three grants and the life of one launched component.
// No device claim, no storage.
//
// EVERY DECISION IS IN `service_logic::tpm`: which operations a grant carries, the PCR sets, the bounds, the
// component tag every sealed secret begins with, and the queue - one request at the driver, one call per
// connection, sixteen waiting in all - with what the provider's comings and goings do to the calls in it. What is
// here is the IO around them, and nothing in it waits on anybody but `wait_any` - save the one bounded `describe`
// a new publication is asked before it is used.
//
// WHEN THE DRIVER RESTARTS, application connections STAY: the TPM's state is in the chip. The call in flight
// answers `interrupted` and is never replayed, calls waiting answer `unavailable`, and the republished provider is
// served as soon as it describes itself. WHEN THIS SERVICE RESTARTS, every application connection dies with it,
// and the grants are minted again at each component's next launch.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{BytesAnswer, DeviceDescription, Error, Grant, InfoAnswer, Outcome, ProviderInfo, ProviderKind, QuoteAnswer, Status, TpmInfo, provider_catalogue, tpm, tpm_admin, tpm_device};
use rt::*;
use service_logic::tpm as tl;
use wire::{Handles, Reader, Sink, Transport, TransportError};

include!(concat!(env!("OUT_DIR"), "/roles_tpm_service.rs"));

// Admin connections minted from the root: PermissionManager's, and a spare across its restart.
const MAX_ADMINS: usize = 4;
// Application connections held at once.
const MAX_GRANTS: usize = 64;
// How long a new publication has to describe itself - two GetCapability reads - before it is passed over.
const DESCRIBE_TICKS: u64 = 500;

struct Provider {
	info: ProviderInfo,
	chan: u64,
	description: DeviceDescription,
	// The request outstanding there: its wire correlation, and the call it is for.
	sent: Option<u32>,
	next_corr: u32,
}

struct GrantConn {
	id: u32,
	chan: u64,
	owner: u64,
	grant: tl::Grant,
	// The component's name as its grant was minted: what every secret it seals is tagged with.
	component: Vec<u8>,
}

// Which call a queued operation answers.
#[derive(Clone, Copy)]
struct Call {
	grant: u32,
	corr: u32,
}

struct Service {
	provider: Option<Provider>,
	grants: Vec<GrantConn>,
	admins: Vec<u64>,
	queue: tl::Queue<Call>,
	next_grant: u32,
}

// ------------------------------------------------------------------ the wire, written by hand

// A generated client's request, captured instead of sent: the encoding is the generator's, the sending is this
// service's own - non-blocking, under a correlation of its choosing.
struct Capture {
	bytes: Vec<u8>,
}

impl Transport for &mut Capture {
	fn call(&mut self, request: &[u8], _request_handles: &[u64], _reply_handles: &mut Handles, _deadline: u64) -> Result<Vec<u8>, TransportError> {
		self.bytes = request.to_vec();
		Err(TransportError::TimedOut)
	}
	fn discard_handles(&mut self, _handles: &[u64]) {}
}

fn captured(encode: impl FnOnce(&mut tpm_device::Client<&mut Capture>), corr: u32) -> Option<Vec<u8>> {
	let mut capture = Capture { bytes: Vec::new() };
	encode(&mut tpm_device::Client::new(&mut capture));
	if capture.bytes.len() < 6 {
		return None;
	}
	capture.bytes[2..6].copy_from_slice(&corr.to_le_bytes());
	Some(capture.bytes)
}

// `result<T, error>` under a correlation.
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

fn outcome(refusal: tl::Refusal) -> Outcome {
	match refusal {
		tl::Refusal::NotGranted => Outcome::NotGranted,
		tl::Refusal::PcrNotAllowed => Outcome::PcrNotAllowed,
		tl::Refusal::Bounds => Outcome::Bounds,
		tl::Refusal::Busy => Outcome::Busy,
		tl::Refusal::PolicyRefused => Outcome::PolicyRefused,
		tl::Refusal::OwnerHierarchyUnavailable => Outcome::OwnerHierarchyUnavailable,
		tl::Refusal::OtherComponent => Outcome::OtherComponent,
		tl::Refusal::Interrupted => Outcome::Interrupted,
		tl::Refusal::Unavailable => Outcome::Unavailable,
	}
}

fn grant_of(kind: Grant) -> tl::Grant {
	match kind {
		Grant::Tpm => tl::Grant::Tpm,
		Grant::TpmMeasure => tl::Grant::Measure,
		Grant::TpmSeal => tl::Grant::Seal,
	}
}

// ONE ANSWER, IN THE RECORD THE OPERATION ANSWERS WITH: an info, bytes, a status or a quote.
enum Answer {
	Info(InfoAnswer),
	Bytes(BytesAnswer),
	Status(Status),
	Quote(QuoteAnswer),
}

// The refusal `outcome` for operation `op`, in its own record.
fn refused(op: &tl::Op, outcome: Outcome) -> Answer {
	match op {
		tl::Op::Info => Answer::Info(InfoAnswer { outcome, code: 0, info: None }),
		tl::Op::PcrExtend { .. } => Answer::Status(Status { outcome, code: 0 }),
		tl::Op::Quote { .. } => Answer::Quote(QuoteAnswer { outcome, code: 0, quote: None }),
		_ => Answer::Bytes(BytesAnswer { outcome, code: 0, bytes: Vec::new() }),
	}
}

fn send_answer(chan: u64, corr: u32, answer: Answer) {
	match answer {
		Answer::Info(answer) => reply(chan, corr, Ok(answer), |value, w| value.write(w)),
		Answer::Bytes(answer) => reply(chan, corr, Ok(answer), |value, w| value.write(w)),
		Answer::Status(answer) => reply(chan, corr, Ok(answer), |value, w| value.write(w)),
		Answer::Quote(answer) => reply(chan, corr, Ok(answer), |value, w| value.write(w)),
	}
}

// ------------------------------------------------------------------ the views the generated code calls

// PermissionManager's minting endpoint.
struct AdminView<'a> {
	service: &'a mut Service,
}

impl tpm_admin::Service for AdminView<'_> {
	// ONE GRANT'S OPERATIONS FOR ONE COMPONENT, alive while `owner` is. The component is what a sealed secret is
	// tagged with, so a connection is never minted for no component.
	fn mint(&mut self, kind: Grant, component: String, owner: u64) -> Result<u64, Error> {
		let refuse = |error: Error| {
			close(owner);
			Err(error)
		};
		if component.is_empty() {
			return refuse(Error::Invalid);
		}
		let service = &mut *self.service;
		if service.grants.len() >= MAX_GRANTS {
			return refuse(Error::Exhausted);
		}
		let Some((mine, theirs)) = channel() else { return refuse(Error::Exhausted) };
		let id = service.next_grant;
		service.next_grant = service.next_grant.wrapping_add(1).max(1);
		service.grants.push(GrantConn { id, chan: mine, owner, grant: grant_of(kind), component: component.into_bytes() });
		Ok(theirs)
	}
}

// A grant's connection, decoded by the generated dispatch: every operation is recorded here and answered by hand -
// `info` at once from what the provider described, the rest once the queue has had them run.
struct GrantView {
	asked: Option<tl::Op>,
}

impl tpm::Service for GrantView {
	fn info(&mut self) -> Result<InfoAnswer, Error> {
		self.asked = Some(tl::Op::Info);
		Err(Error::Again)
	}
	fn random(&mut self, count: u32) -> Result<BytesAnswer, Error> {
		self.asked = Some(tl::Op::Random { count });
		Err(Error::Again)
	}
	fn pcr_read(&mut self, pcr: u32) -> Result<BytesAnswer, Error> {
		self.asked = Some(tl::Op::PcrRead { pcr });
		Err(Error::Again)
	}
	fn pcr_extend(&mut self, pcr: u32, digest: Vec<u8>) -> Result<Status, Error> {
		self.asked = Some(tl::Op::PcrExtend { pcr, digest });
		Err(Error::Again)
	}
	fn seal(&mut self, pcr: u32, secret: Vec<u8>) -> Result<BytesAnswer, Error> {
		self.asked = Some(tl::Op::Seal { pcr, secret });
		Err(Error::Again)
	}
	fn unseal(&mut self, sealed: Vec<u8>) -> Result<BytesAnswer, Error> {
		self.asked = Some(tl::Op::Unseal { sealed });
		Err(Error::Again)
	}
	fn quote(&mut self, pcr: u32, nonce: Vec<u8>) -> Result<QuoteAnswer, Error> {
		self.asked = Some(tl::Op::Quote { pcr, nonce });
		Err(Error::Again)
	}
}

impl Service {
	// Whether seal, unseal and quote can run on the TPM being served.
	fn owner_usable(&self) -> bool {
		self.provider.as_ref().is_some_and(|provider| !provider.description.owner_auth_set && provider.description.owner_enabled)
	}

	// ------------------------------------------------------------------ the provider

	// A PUBLICATION: opened, and described within a bound - and then served. A second while one is served is a
	// firmware or reconciliation error, logged and not used.
	fn adopt(&mut self, catalogue: u64, info: ProviderInfo) {
		if self.provider.is_some() {
			print(b"TpmService: a second TPM was published while one is served - it is not used\n");
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) else {
			print(b"TpmService: the published TPM could not be opened\n");
			return;
		};
		let description = match tpm_device::Client::with_deadline(ChannelTransport { chan }, clock() + DESCRIBE_TICKS).describe() {
			Some(Ok(description)) if description.outcome == Outcome::Done => description,
			_ => {
				print(b"TpmService: the published TPM did not describe itself\n");
				close(chan);
				return;
			}
		};
		self.provider = Some(Provider { info, chan, description, sent: None, next_corr: 1 });
		self.queue.arrived();
		print(b"TpmService: a TPM is served\n");
	}

	// THE PROVIDER WENT AWAY: the call at the driver is `interrupted` and never replayed, and those waiting are
	// `unavailable`.
	fn lose(&mut self, why: &[u8]) {
		if let Some(provider) = self.provider.take() {
			close(provider.chan);
			print(b"TpmService: the TPM is gone: ");
			print(why);
			print(b"\n");
		}
		let effects = self.queue.lost();
		self.apply(effects);
	}

	// ------------------------------------------------------------------ effects

	fn apply(&mut self, effects: Vec<tl::Effect<Call>>) {
		for effect in effects {
			match effect {
				// ANSWERED IN THE RECORD ITS OPERATION ANSWERS WITH, to a connection that may already be gone.
				tl::Effect::Refuse(pending, refusal) => {
					if let Some(grant) = self.grants.iter().find(|grant| grant.id == pending.call.grant) {
						send_answer(grant.chan, pending.call.corr, refused(&pending.op, outcome(refusal)));
					}
				}
				tl::Effect::Send(pending) => self.send(pending),
			}
		}
	}

	// THE ONE REQUEST AT THE DRIVER, sent without waiting. A seal's secret goes tagged with its component.
	fn send(&mut self, pending: tl::Pending<Call>) {
		let component = self.grants.iter().find(|grant| grant.id == pending.call.grant).map(|grant| grant.component.clone()).unwrap_or_default();
		let Some(provider) = self.provider.as_mut() else { return };
		let corr = provider.next_corr;
		provider.next_corr = provider.next_corr.wrapping_add(1).max(1);
		let bytes = captured(
			|client| match &pending.op {
				tl::Op::Info => drop(client.describe()),
				tl::Op::Random { count } => drop(client.random(count)),
				tl::Op::PcrRead { pcr } => drop(client.pcr_read(pcr)),
				tl::Op::PcrExtend { pcr, digest } => drop(client.pcr_extend(pcr, digest)),
				tl::Op::Seal { pcr, secret } => drop(client.seal(pcr, &tl::tagged(&component, secret))),
				tl::Op::Unseal { sealed } => drop(client.unseal(sealed)),
				tl::Op::Quote { pcr, nonce } => drop(client.quote(pcr, nonce)),
			},
			corr,
		);
		if bytes.is_some_and(|bytes| try_send(provider.chan, &bytes, 0)) {
			provider.sent = Some(corr);
			return;
		}
		// NOT SENT IS A PROVIDER THAT IS NOT THERE: everything it held is answered as its going would answer it.
		self.lose(b"a request to it could not be sent");
	}

	// ------------------------------------------------------------------ what the provider says

	fn on_reply(&mut self, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		let Some(chan) = self.provider.as_ref().map(|provider| provider.chan) else { return Ok(()) };
		loop {
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
			let Some(provider) = self.provider.as_mut() else { return Ok(()) };
			if provider.sent != Some(corr) {
				// NOT THE ONE OUTSTANDING: a reply to nothing this service asked is the provider's error.
				return Err(b"a reply named a request that was not outstanding");
			}
			provider.sent = None;
			let (done, next) = self.queue.answered();
			let Some(done) = done else { return Err(b"a reply arrived with nothing at the driver") };
			let answer = match decode(&done.op, &mut reader) {
				Some(answer) => answer,
				None => return Err(b"a reply did not decode"),
			};
			if let Some(grant) = self.grants.iter().find(|grant| grant.id == done.call.grant) {
				let answer = match (&done.op, answer) {
					// THE TAG IS CHECKED HERE, where only this service can: another component's secret opens for
					// nobody but it, and nothing of it is returned.
					(tl::Op::Unseal { .. }, Answer::Bytes(unsealed)) if unsealed.outcome == Outcome::Done => match tl::untagged(&grant.component, &unsealed.bytes) {
						Ok(secret) => Answer::Bytes(BytesAnswer { outcome: Outcome::Done, code: 0, bytes: secret }),
						Err(refusal) => refused(&done.op, outcome(refusal)),
					},
					(_, answer) => answer,
				};
				send_answer(grant.chan, done.call.corr, answer);
			}
			self.apply(next);
		}
	}

	// ------------------------------------------------------------------ clients

	fn client(&mut self, at: usize, request: &[u8], handles: &mut Handles, reply_buf: &mut [u8]) -> bool {
		let mut view = GrantView { asked: None };
		let mut reply_handles = Handles::new();
		let written = tpm::dispatch(&mut view, request, handles, reply_buf, &mut reply_handles);
		for &leftover in reply_handles.as_slice() {
			close(leftover);
		}
		let Some(written) = written else { return false };
		let (chan, id, grant) = (self.grants[at].chan, self.grants[at].id, self.grants[at].grant);
		let Some(op) = view.asked else {
			// The protocol's own requests - its version, say - are answered by the dispatch itself.
			let _ = try_send(chan, &reply_buf[..written], 0);
			return true;
		};
		let corr = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
		// `info` IS ANSWERED FROM WHAT THE PROVIDER DESCRIBED, every grant's, and nothing reaches the TPM for it.
		if op == tl::Op::Info {
			let answer = match self.provider.as_ref() {
				Some(provider) => {
					let described = &provider.description;
					InfoAnswer { outcome: Outcome::Done, code: 0, info: Some(TpmInfo { interface: described.interface, manufacturer: described.manufacturer.clone(), vendor: described.vendor.clone(), firmware_1: described.firmware_1, firmware_2: described.firmware_2, sealing: self.owner_usable() }) }
				}
				None => InfoAnswer { outcome: Outcome::Unavailable, code: 0, info: None },
			};
			send_answer(chan, corr, Answer::Info(answer));
			return true;
		}
		if let Err(refusal) = tl::admit(grant, &op, self.owner_usable()) {
			send_answer(chan, corr, refused(&op, outcome(refusal)));
			return true;
		}
		let effects = self.queue.submit(id, Call { grant: id, corr }, op);
		self.apply(effects);
		true
	}

	// A grant's owner ended or its connection closed: it leaves the queue, and an answer the driver still owes it
	// goes to nobody.
	fn drop_grant(&mut self, id: u32) {
		self.queue.closed(id);
		let Some(at) = self.grants.iter().position(|grant| grant.id == id) else { return };
		let grant = self.grants.remove(at);
		close(grant.chan);
		close(grant.owner);
	}
}

// The provider's answer to `op`, as the record the application's call answers with.
fn decode(op: &tl::Op, reader: &mut Reader) -> Option<Answer> {
	if !reader.tag()? {
		// AN ERROR FROM THE PROVIDER'S WIRE is the provider failing the call, which the application reads as a fault.
		let _ = Error::read(reader)?;
		return Some(refused(op, Outcome::Fault));
	}
	Some(match op {
		tl::Op::Info => {
			let described = DeviceDescription::read(reader)?;
			Answer::Info(InfoAnswer { outcome: described.outcome, code: described.code, info: None })
		}
		tl::Op::PcrExtend { .. } => Answer::Status(Status::read(reader)?),
		tl::Op::Quote { .. } => Answer::Quote(QuoteAnswer::read(reader)?),
		_ => Answer::Bytes(BytesAnswer::read(reader)?),
	})
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// The roles: a catalogue connection minted for `tpm` alone, and the minting root PermissionManager reaches
	// through the broker. Nothing else.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (catalogue, mut admin_root) = (roles[0], roles[1]);
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Tpm).unwrap_or(0) } else { 0 };
	let mut service = Service { provider: None, grants: Vec::new(), admins: Vec::new(), queue: tl::Queue::default(), next_grant: 1 };
	send_blocking(bootstrap, b"TpmService: online", 0);

	let mut buf = alloc::vec![0u8; 4096];
	let mut reply_buf = alloc::vec![0u8; 4096];
	let mut subscribed = subscription != 0;
	loop {
		let mut waitset: Vec<u64> = Vec::new();
		if admin_root != 0 {
			waitset.push(admin_root);
		}
		if subscribed {
			waitset.push(subscription);
		}
		waitset.extend(service.admins.iter().copied());
		if let Some(provider) = service.provider.as_ref() {
			waitset.push(provider.chan);
		}
		for grant in &service.grants {
			waitset.push(grant.chan);
			waitset.push(grant.owner);
		}
		let ready = wait_any(&waitset, 0);
		if ready >= 0 {
			serve(&mut service, waitset[ready as usize], catalogue, subscription, &mut subscribed, &mut admin_root, &mut buf, &mut reply_buf);
		}
	}
}

#[allow(clippy::too_many_arguments)]
fn serve(service: &mut Service, handle: u64, catalogue: u64, subscription: u64, subscribed: &mut bool, admin_root: &mut u64, buf: &mut [u8], reply_buf: &mut [u8]) {
	if service.provider.as_ref().is_some_and(|provider| provider.chan == handle) {
		if let Err(why) = service.on_reply(buf) {
			service.lose(why);
		}
		return;
	}
	// A GRANT'S OWNER ENDED: its call leaves the queue with it, whatever copies of its endpoint live on.
	if let Some(id) = service.grants.iter().find(|grant| grant.owner == handle).map(|grant| grant.id) {
		service.drop_grant(id);
		return;
	}
	if let Some(at) = service.grants.iter().position(|grant| grant.chan == handle) {
		let (len, mut handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				let id = service.grants[at].id;
				service.drop_grant(id);
				return;
			}
		};
		let understood = len >= 6 && service.client(at, &buf[..len], &mut handles, reply_buf);
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		// A REQUEST THIS CONNECTION CANNOT CARRY CLOSES IT, rather than being answered with silence.
		if !understood && let Some(id) = service.grants.get(at).map(|grant| grant.id) {
			service.drop_grant(id);
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
			let served = service.provider.as_ref().is_some_and(|provider| provider.info.slot == info.slot && provider.info.provider_generation == info.provider_generation && provider.info.binding_generation == info.binding_generation);
			if info.live {
				service.adopt(catalogue, info);
			} else if served {
				service.lose(b"its publication was withdrawn");
			}
		}
		return;
	}
	let is_root = handle == *admin_root;
	if !is_root && !service.admins.contains(&handle) {
		return;
	}
	let (len, mut handles) = match try_recv_caps(handle, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			// A CLOSED ROOT IS GIVEN UP, not waited on: a closed channel is always ready, and a service that kept it
			// in its wait set would spin on it.
			close(handle);
			if is_root {
				*admin_root = 0;
			} else {
				service.admins.retain(|&admin| admin != handle);
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
		// FROM THE ROOT OR FROM A CONNECTION IT MINTED, as every root's connections allow.
		if op == CONNECT_OP {
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
	if is_root {
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		return;
	}
	let mut reply_handles = Handles::new();
	let written = tpm_admin::dispatch(&mut AdminView { service }, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
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
