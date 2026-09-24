// AdminService - one person's confirmation, on a protected screen, of one bound high-risk operation, and at
// most one attempt at it.
//
// WHAT IT HOLDS. A request factory PermissionManager mints request connections from, one per authorized launch
// and bound to the task that launch prepared; a catalogue connection minted for `admin-executor` alone, which
// is how registered executors - drivers DeviceManager bound - reach it and nothing else does; the protected
// session roots DisplayService and InputService serve for it alone; a volume client scoped to its journal
// directory; and TimeService, for a wall-clock reading a record may carry. It holds no device and no payload
// beyond handing the requester's to the executor that prepares it.
//
// EVERY DECISION IS IN `service_logic`: the state machine in `admin_broker`, the descriptor checks and the
// prompt's template in `admin_descriptor`, the journal's geometry in `admin_journal` and the decision keys in
// `trusted_keys`, all host-tested. What is here is the IO around them, and the rule that nothing in it waits
// on anybody but `wait_any`: a preparation, a revalidation, a display or keyboard acknowledgment and every
// journal write is a request sent without waiting and answered through the loop, with five seconds to answer.
//
// THE OWNER IS CHECKED BEFORE ANYTHING ELSE THAT IS READY. Every iteration asks each launching task, with an
// already-reached deadline, whether it has ended - before a reply, a key or a redemption is looked at - so a
// termination that is ready beside other work always wins, whatever order a wait happens to report.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{AdminAction, AdminAnswer, AdminDescriptor, AdminEvent, AdminGrant, AdminJournalFault, AdminJournalPage, AdminPrepared, AdminRecord, AdminRequestArgs, AdminResult, AdminScope, Error, OpenOpts, ProviderInfo, ProviderKind, Timestamp, TrustedInput, TrustedInputKind, TrustedScreen, WriterMode, admin_authority, admin_executor, admin_factory, admin_journal, admin_request, admin_test, display_trusted, input_trusted, provider_catalogue, time, volume, writer};
use rt::*;
use service_logic::admin_broker::{self as ab, Effect, Event, Outcome};
use service_logic::admin_descriptor::{self as ad, Action, Asked, Descriptor};
use service_logic::admin_journal as aj;
use service_logic::trusted_keys::{Decision, Verdict};
use wire::{Handles, Reader, Sink, Transport, TransportError, VecWriter};

include!(concat!(env!("OUT_DIR"), "/roles_admin_service.rs"));

// THE JOURNAL'S DIRECTORY, which the supervisor made and scoped this service's volume client to. Absolute,
// because a scoped client checks the path it is given against its scope rather than resolving a relative one.
const JOURNAL_DIR: &str = "vol://system/admin-audit";
const WRITE_CHUNK: usize = 4096;
// A write storage refused is tried again after this long, so a failing volume does not spin the loop.
const RETRY_TICKS: u64 = 100;
// Connections minted from each root: PermissionManager's, and a spare across its restart.
const MAX_FACTORIES: usize = 4;
const MAX_READERS: usize = 4;
// And from the development image's test controls, which each probe holds for its whole run: PermissionManager's
// resolved connection, and a requester and its helper running at once. Two refused the helper's launch while the
// requester it contends with was holding its own.
const MAX_TESTERS: usize = 4;
// Records one journal page carries.
const PAGE: usize = 16;
// How often the wall-clock reading a record carries is refreshed.
const TIME_REFRESH_TICKS: u64 = 6000;
// Margin past an executor's own deadline before its silence is an unknown outcome.
const EXECUTE_MARGIN_TICKS: u64 = 100;

// THE REGISTERED EXECUTORS, by the action each carries out: its publication name. The firmware adapter's
// slot is named and nothing publishes under it until the USB driver set's DFU class module does, so until
// then that action is declined. The probe's exists in a development image alone - the executor that serves
// it is a development fixture - and a shipping build does not name it at all.
const DFU_EXECUTOR: &[u8] = b"org.libersystem.admin-dfu";
#[cfg(feature = "development")]
const PROBE_EXECUTOR: &[u8] = b"org.libersystem.admin-probe";

fn action_of(name: &[u8]) -> Option<Action> {
	#[cfg(feature = "development")]
	if name == PROBE_EXECUTOR {
		return Some(Action::ProbeWrite);
	}
	if name == DFU_EXECUTOR { Some(Action::FirmwareDownload) } else { None }
}

// ------------------------------------------------------------------ the wire, sent without waiting

// A generated client's request, captured instead of sent: the encoding is the generator's, the sending is
// this service's own - non-blocking, under a correlation of its choosing, with the capabilities it carries.
struct Capture {
	bytes: Vec<u8>,
	handles: Vec<u64>,
}

impl Transport for Capture {
	fn call(&mut self, request: &[u8], request_handles: &[u64], _reply_handles: &mut Handles, _deadline: u64) -> Result<Vec<u8>, TransportError> {
		self.bytes = request.to_vec();
		self.handles = request_handles.to_vec();
		Err(TransportError::TimedOut)
	}
	fn discard_handles(&mut self, _handles: &[u64]) {}
}

impl Transport for &mut Capture {
	fn call(&mut self, request: &[u8], request_handles: &[u64], reply_handles: &mut Handles, deadline: u64) -> Result<Vec<u8>, TransportError> {
		(**self).call(request, request_handles, reply_handles, deadline)
	}
	fn discard_handles(&mut self, handles: &[u64]) {
		(**self).discard_handles(handles)
	}
}

// Send one request under `corr`, answering whether it left. NOT SENT is the caller's to answer at once: the
// peer never saw it. The capabilities it carried are closed when it did not leave.
fn post(chan: u64, corr: u32, encode: impl FnOnce(&mut Capture)) -> bool {
	let mut capture = Capture { bytes: Vec::new(), handles: Vec::new() };
	encode(&mut capture);
	let sent = capture.bytes.len() >= 6 && {
		capture.bytes[2..6].copy_from_slice(&corr.to_le_bytes());
		try_send_caps(chan, &capture.bytes, &capture.handles)
	};
	if !sent {
		for &handle in &capture.handles {
			close(handle);
		}
	}
	sent
}

// `result<T, error>` under a correlation, sent without waiting, with whatever capabilities `T` carries.
fn answer<T>(chan: u64, corr: u32, result: Result<T, Error>, write: impl FnOnce(&T, &mut VecWriter) -> Option<()>) -> bool {
	let mut writer = VecWriter::new();
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
	let (bytes, handles) = writer.into_message();
	if encoded.is_some() && try_send_caps(chan, &bytes, handles.as_slice()) {
		return true;
	}
	for &handle in handles.as_slice() {
		close(handle);
	}
	false
}

// A reply's correlation and its `result` tag, with the reader left at the value.
fn reply_head<'a>(bytes: &'a [u8], handles: &'a Handles) -> Option<(u32, bool, Reader<'a>)> {
	let mut reader = Reader::with_handle_list(bytes, handles);
	let corr = reader.u32()?;
	let ok = reader.tag()?;
	Some((corr, ok, reader))
}

// Close what a reply carried and its decoded value did not adopt.
fn close_except(handles: &Handles, kept: &[u64]) {
	for &handle in handles.as_slice() {
		if !kept.contains(&handle) {
			close(handle);
		}
	}
}

fn next(counter: &mut u32) -> u32 {
	let corr = *counter;
	*counter = counter.wrapping_add(1).max(1);
	corr
}

// ------------------------------------------------------------------ the vocabulary, both ways

fn action_wire(action: Action) -> AdminAction {
	match action {
		Action::FirmwareDownload => AdminAction::FirmwareDownload,
		Action::ProbeWrite => AdminAction::ProbeWrite,
	}
}

fn action_from(action: AdminAction) -> Action {
	match action {
		AdminAction::FirmwareDownload => Action::FirmwareDownload,
		AdminAction::ProbeWrite => Action::ProbeWrite,
	}
}

fn descriptor_wire(descriptor: &Descriptor) -> AdminDescriptor {
	AdminDescriptor { version: descriptor.version, action: action_wire(descriptor.action), executor: descriptor.executor.clone(), executor_epoch: descriptor.executor_epoch, target: descriptor.target.clone(), target_generation: descriptor.target_generation, parameters: descriptor.parameters.clone(), payload_length: descriptor.payload_length, payload_digest: descriptor.payload_digest.clone() }
}

fn descriptor_from(descriptor: &AdminDescriptor) -> Descriptor {
	Descriptor { version: descriptor.version, action: action_from(descriptor.action), executor: descriptor.executor.clone(), executor_epoch: descriptor.executor_epoch, target: descriptor.target.clone(), target_generation: descriptor.target_generation, parameters: descriptor.parameters.clone(), payload_length: descriptor.payload_length, payload_digest: descriptor.payload_digest.clone() }
}

fn event_wire(event: Event) -> AdminEvent {
	match event {
		Event::Requested => AdminEvent::Requested,
		Event::Granted => AdminEvent::Granted,
		Event::Declined => AdminEvent::Declined,
		Event::Consumed => AdminEvent::Consumed,
		Event::Completed => AdminEvent::Completed,
		Event::Failed => AdminEvent::Failed,
		Event::OutcomeUnknown => AdminEvent::OutcomeUnknown,
	}
}

fn event_name(event: Event) -> &'static str {
	match event {
		Event::Requested => "requested",
		Event::Granted => "granted",
		Event::Declined => "declined",
		Event::Consumed => "consumed",
		Event::Completed => "completed",
		Event::Failed => "failed",
		Event::OutcomeUnknown => "outcome unknown",
	}
}

fn bounded(text: &str, most: usize) -> String {
	let mut cut = text.len().min(most);
	while !text.is_char_boundary(cut) {
		cut -= 1;
	}
	String::from(&text[..cut])
}

fn ticks(ms: u32) -> u64 {
	(ms as u64).div_ceil(10)
}

fn segment_path(number: u64) -> String {
	format!("{JOURNAL_DIR}/segment-{number:016x}")
}

// A DECISION CAN BE TAKEN, said once, by whichever of the two confirmations came second. The keyboard arms
// when the chord's keys are up and the screen is presented when the display has flushed it, in either
// order - so neither line alone says the session takes keys, and a person or a scenario waiting to press
// one waits for this.
fn say_ready(was: bool, decision: &Decision) {
	if !was && decision.ready() {
		print(b"AdminService: the session is ready for a decision\n");
	}
}

// ------------------------------------------------------------------ what the service holds

struct Executor {
	key: u32,
	info: ProviderInfo,
	chan: u64,
	action: Action,
	next_corr: u32,
	sent: Vec<(u32, Sent)>,
}

#[derive(Clone, Copy)]
enum Sent {
	Prepare(ab::RequestId),
	Revalidate(ab::RequestId),
	Execute(ab::RequestId),
	Release,
}

// A request connection: the channel, the launching task it is bound to, and the call awaiting its answer.
struct Conn {
	id: ab::ConnectionId,
	chan: u64,
	owner: u64,
	call: Option<u32>,
}

// A grant's channel, and the redemption awaiting its answer.
struct GrantConn {
	request: ab::RequestId,
	chan: u64,
	call: Option<u32>,
}

// The protected session: its epoch, what it shows, and what the display and the keyboard have acknowledged.
struct Session {
	epoch: u64,
	request: Option<ab::RequestId>,
	decision: Decision,
	// The display's session channel, once the lock answered; its closing ends the session.
	display: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DisplayCall {
	Lock(u64),
	Present(u64),
	Release,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputCall {
	Arm(u64),
	Disarm,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
	Opening,
	Writing,
	Committing,
}

// The one journal record being written: the head of the ring.
struct Writing {
	sequence: u64,
	request: u64,
	placement: aj::Placement,
	bytes: Vec<u8>,
	header: usize,
	frame: usize,
	step: Step,
	session: u64,
	corr: u32,
	deadline: u64,
	at: usize,
	start: Option<u64>,
}

// An operator's page, waiting on the volume read that fills it.
struct Read {
	chan: u64,
	corr: u32,
	located: Vec<aj::Located>,
	vol_corr: u32,
	deadline: u64,
}

#[derive(Clone, Copy)]
enum VolCall {
	OpenWriter,
	Remove,
	Read,
}

struct Service {
	broker: ab::Broker,
	journal: aj::Journal,
	executors: Vec<Executor>,
	conns: Vec<Conn>,
	grants: Vec<GrantConn>,
	// A request's payload, until its preparation carries it to the executor.
	payloads: Vec<(ab::RequestId, u64)>,
	// An executor's own operation deadline, from its preparation; and a dispatch's, once sent.
	operation_ms: Vec<(ab::RequestId, u32)>,
	dispatched: Vec<(ab::RequestId, u64)>,
	// Journal records whose first attempt the broker is waiting on.
	owed: Vec<(u64, ab::RequestId, Event)>,
	writing: Option<Writing>,
	retry_at: u64,
	fault: AdminJournalFault,
	held: Option<bool>,
	reads: Vec<Read>,
	vol_calls: Vec<(u32, VolCall)>,
	session: Option<Session>,
	sessions: u32,
	display_calls: Vec<(u32, DisplayCall, u64)>,
	input_calls: Vec<(u32, InputCall, u64)>,
	// The roots and connections.
	volume: u64,
	display: u64,
	input: u64,
	events: u64,
	time: u64,
	catalogue: u64,
	vol_corr: u32,
	display_corr: u32,
	input_corr: u32,
	time_corr: u32,
	time_asked: Option<(u32, u64)>,
	time_at: u64,
	// The wall clock as TimeService last reported it, against the monotonic clock then.
	utc: Option<(u64, u64)>,
	next_executor: u32,
}

// ------------------------------------------------------------------ the views the generated code calls

struct FactoryView<'a> {
	service: &'a mut Service,
}

impl admin_factory::Service for FactoryView<'_> {
	// ONE REQUEST CONNECTION PER AUTHORIZED LAUNCH, bound to the task that launch prepared. The observer is
	// checked here - alive, and observable - with an already-reached deadline, which makes the wait a check:
	// a task that ended already, or a handle nothing can wait on, is refused, and the observer is closed.
	fn mint(&mut self, component: String, scope: AdminScope, launch: u64, owner: u64) -> Result<u64, Error> {
		let refuse = |error: Error| {
			if owner != 0 {
				close(owner);
			}
			Err(error)
		};
		if owner == 0 || component.is_empty() {
			return refuse(Error::Invalid);
		}
		let observed = wait(owner, clock().max(1));
		if observed == 0 {
			return refuse(Error::Closed);
		}
		if observed != ERR_TIMED_OUT {
			return refuse(Error::Invalid);
		}
		let scope = ab::Scope { actions: scope.actions.into_iter().map(action_from).collect(), target_prefix: scope.target_prefix };
		let service = &mut *self.service;
		let Ok(id) = service.broker.open(component, scope, launch) else { return refuse(Error::Exhausted) };
		let Some((mine, theirs)) = channel() else {
			service.broker.closed(id, clock(), &mut Vec::new());
			service.broker.settle();
			return refuse(Error::Exhausted);
		};
		service.conns.push(Conn { id, chan: mine, owner, call: None });
		Ok(theirs)
	}
}

// A request connection's calls, decoded by the generated dispatch and answered here by hand once decided.
enum Asked2 {
	Request(AdminRequestArgs, u64),
	Cancel,
}

struct RequestView {
	asked: Option<Asked2>,
}

impl admin_request::Service for RequestView {
	fn request(&mut self, args: AdminRequestArgs, payload: u64) -> Result<AdminAnswer, Error> {
		self.asked = Some(Asked2::Request(args, payload));
		Err(Error::Again)
	}
	fn cancel(&mut self) -> Result<(), Error> {
		self.asked = Some(Asked2::Cancel);
		Err(Error::Again)
	}
}

struct GrantView {
	asked: bool,
}

impl admin_authority::Service for GrantView {
	fn execute(&mut self) -> Result<AdminResult, Error> {
		self.asked = true;
		Err(Error::Again)
	}
}

struct JournalView {
	asked: Option<u64>,
}

impl admin_journal::Service for JournalView {
	fn read(&mut self, from: u64) -> Result<AdminJournalPage, Error> {
		self.asked = Some(from);
		Err(Error::Again)
	}
}

struct TestView<'a> {
	service: &'a mut Service,
}

impl admin_test::Service for TestView<'_> {
	fn journal(&mut self, fault: AdminJournalFault) -> Result<(), Error> {
		self.service.fault = fault;
		Ok(())
	}

	fn path(&mut self, present: bool) -> Result<(), Error> {
		let service = &mut *self.service;
		let present = present && service.display != 0 && service.events != 0;
		service.step(|broker, now, effects| broker.set_path(present, now, effects));
		Ok(())
	}

	fn held(&mut self) -> Result<u32, Error> {
		Ok(u32::from(self.service.held.is_some()))
	}
}

impl Service {
	// ------------------------------------------------------------------ effects

	fn apply(&mut self, effects: Vec<Effect>) {
		for effect in effects {
			let now = clock();
			match effect {
				Effect::Prepare { request } => self.prepare(request, now),
				Effect::Revalidate { request, executor, operation } => self.to_executor(request, executor, Sent::Revalidate(request), operation, now),
				Effect::Journal { request, event, reason } => self.record(request, event, reason),
				Effect::Show { request, session } => self.show(request, session),
				Effect::Hide => self.hide(),
				Effect::Answer { connection, request, granted } => self.answer_request(connection, request, granted),
				Effect::Dispatch { request, executor, operation } => self.dispatch(request, executor, operation, now),
				Effect::Release { executor, operation, .. } => self.release(executor, operation),
				Effect::CloseGrant { request } => self.close_grant(request),
				Effect::Redeemed { request, outcome } => self.redeemed(request, outcome),
			}
		}
	}

	fn step(&mut self, act: impl FnOnce(&mut ab::Broker, u64, &mut Vec<Effect>)) {
		let mut effects = Vec::new();
		act(&mut self.broker, clock(), &mut effects);
		self.apply(effects);
	}

	fn executor_for(&self, action: Action) -> Option<usize> {
		self.executors.iter().position(|executor| executor.action == action)
	}

	// PREPARATION: the journal must be able to promise the request's records, an executor must serve its
	// action, and the payload travels to that executor with the request - this service keeps no copy.
	fn prepare(&mut self, request: ab::RequestId, now: u64) {
		let payload = self.payloads.iter().position(|(held, _)| *held == request).map(|at| self.payloads.remove(at).1).unwrap_or(0);
		let refuse = |service: &mut Service, payload: u64, reason: &'static str| {
			if payload != 0 {
				close(payload);
			}
			service.step(|broker, now, effects| broker.prepared(request, Err(reason), now, effects));
		};
		let Some(asked) = self.broker.request(request).map(|held| held.asked.clone()) else {
			if payload != 0 {
				close(payload);
			}
			return;
		};
		if self.journal.reserve(request).is_err() {
			return refuse(self, payload, "the journal cannot promise this request's records");
		}
		let Some(at) = self.executor_for(asked.action) else { return refuse(self, payload, "no registered executor serves this action") };
		if payload == 0 {
			return refuse(self, payload, "the request carried no payload");
		}
		let key = self.executors[at].key;
		self.broker.bind_executor(request, key);
		let executor = &mut self.executors[at];
		let corr = next(&mut executor.next_corr);
		let sent = post(executor.chan, corr, |capture| {
			let _ = admin_executor::Client::new(capture).prepare(&action_wire(asked.action), &asked.target, &asked.parameters, &asked.payload_length, &payload);
		});
		if !sent {
			return self.step(|broker, now, effects| broker.prepared(request, Err("the executor could not be reached"), now, effects));
		}
		executor.sent.push((corr, Sent::Prepare(request)));
		let _ = now;
	}

	fn to_executor(&mut self, request: ab::RequestId, key: u32, what: Sent, operation: u64, _now: u64) {
		let Some(at) = self.executors.iter().position(|executor| executor.key == key) else {
			if let Sent::Revalidate(request) = what {
				self.step(|broker, now, effects| broker.revalidated(request, false, now, effects));
			}
			return;
		};
		let executor = &mut self.executors[at];
		let corr = next(&mut executor.next_corr);
		let sent = post(executor.chan, corr, |capture| {
			let _ = admin_executor::Client::new(capture).revalidate(&operation);
		});
		if sent {
			executor.sent.push((corr, what));
		} else {
			self.step(|broker, now, effects| broker.revalidated(request, false, now, effects));
		}
	}

	// THE ONE DISPATCH: the executor's own prepared object and the epoch it froze. Its deadline, not this
	// service's exchange deadline, governs what follows - and silence past it is an unknown outcome.
	fn dispatch(&mut self, request: ab::RequestId, key: u32, operation: u64, now: u64) {
		let epoch = self.broker.request(request).and_then(|held| held.descriptor.as_ref()).map_or(0, |descriptor| descriptor.executor_epoch);
		let Some(at) = self.executors.iter().position(|executor| executor.key == key) else {
			// NEVER SENT, so nothing was attempted: a failure, with certainty.
			return self.step(|broker, now, effects| broker.executed(request, Outcome::Failed, now, effects));
		};
		let executor = &mut self.executors[at];
		let corr = next(&mut executor.next_corr);
		if !post(executor.chan, corr, |capture| {
			let _ = admin_executor::Client::new(capture).execute(&operation, &epoch);
		}) {
			return self.step(|broker, now, effects| broker.executed(request, Outcome::Failed, now, effects));
		}
		executor.sent.push((corr, Sent::Execute(request)));
		let budget = self.operation_ms.iter().find(|(held, _)| *held == request).map_or(0, |(_, ms)| ticks(*ms));
		self.dispatched.push((request, now + budget + EXECUTE_MARGIN_TICKS));
	}

	fn release(&mut self, key: u32, operation: u64) {
		let Some(executor) = self.executors.iter_mut().find(|executor| executor.key == key) else { return };
		let corr = next(&mut executor.next_corr);
		if post(executor.chan, corr, |capture| {
			let _ = admin_executor::Client::new(capture).cancel(&operation);
		}) {
			executor.sent.push((corr, Sent::Release));
		}
	}

	fn answer_request(&mut self, connection: ab::ConnectionId, request: ab::RequestId, granted: bool) {
		let Some(at) = self.conns.iter().position(|conn| conn.id == connection) else { return };
		let Some(corr) = self.conns[at].call.take() else { return };
		let chan = self.conns[at].chan;
		if !granted {
			answer(chan, corr, Ok(AdminAnswer::Declined), |value, w| value.write(w));
			return;
		}
		let Some(descriptor) = self.broker.request(request).and_then(|held| held.descriptor.as_ref()).map(descriptor_wire) else {
			answer(chan, corr, Ok(AdminAnswer::Declined), |value, w| value.write(w));
			return;
		};
		// A FRESH CHANNEL SERVED BY THE GRANT'S RECORD, carrying what a holder needs to call it and pass it on,
		// and not `duplicate`.
		// NO CHANNEL FOR IT: the requester is told `declined`, and the grant nobody can reach expires unused.
		let Some((mine, theirs)) = channel() else {
			answer(chan, corr, Ok(AdminAnswer::Declined), |value, w| value.write(w));
			return;
		};
		let narrowed = duplicate(theirs, RIGHT_SEND | RIGHT_RECEIVE | RIGHT_WAIT | RIGHT_TRANSFER);
		close(theirs);
		if narrowed <= 0 {
			close(mine);
			answer(chan, corr, Ok(AdminAnswer::Declined), |value, w| value.write(w));
			return;
		}
		self.grants.push(GrantConn { request, chan: mine, call: None });
		answer(chan, corr, Ok(AdminAnswer::Granted(AdminGrant { grant: narrowed as u64, descriptor })), |value, w| value.write(w));
	}

	fn close_grant(&mut self, request: ab::RequestId) {
		let Some(at) = self.grants.iter().position(|grant| grant.request == request) else { return };
		let grant = self.grants.remove(at);
		// A REDEMPTION STILL WAITING is answered before its channel goes.
		if let Some(corr) = grant.call {
			answer::<AdminResult>(grant.chan, corr, Err(Error::Denied), |_, _| Some(()));
		}
		if grant.chan != 0 {
			close(grant.chan);
		}
	}

	fn redeemed(&mut self, request: ab::RequestId, outcome: Option<Outcome>) {
		self.dispatched.retain(|(held, _)| *held != request);
		let Some(grant) = self.grants.iter_mut().find(|grant| grant.request == request) else { return };
		let Some(corr) = grant.call.take() else { return };
		let result = match outcome {
			Some(Outcome::Completed) => Ok(AdminResult::Completed),
			Some(Outcome::Failed) => Ok(AdminResult::Failed),
			Some(Outcome::Unknown) => Ok(AdminResult::OutcomeUnknown),
			None => Err(Error::Denied),
		};
		answer(grant.chan, corr, result, |value, w| value.write(w));
	}

	// ------------------------------------------------------------------ the journal

	// ONE RECORD, built from what the broker knows of the request, pushed behind whatever is waiting. The
	// wall clock goes with it only as TimeService last reported it, and says so.
	fn record(&mut self, request: ab::RequestId, event: Event, reason: &'static str) {
		// EVERY DECISION IS ALSO SAID, as it is recorded: the line is a diagnostic and the journal is the record.
		let mut line = format!("AdminService: request {request} {}", event_name(event));
		if !reason.is_empty() {
			line.push_str(": ");
			line.push_str(reason);
		}
		line.push('\n');
		print(line.as_bytes());
		let sequence = self.journal.sequence();
		let held = self.broker.request(request);
		let (launch, requester) = held.and_then(|held| self.broker.connection(held.connection)).map_or((0, String::new()), |connection| (connection.launch, bounded(&connection.component, ad::MAX_NAME)));
		let action = held.map_or(0, |held| held.asked.action.wire());
		let digest: Vec<u8> = held.and_then(|held| held.descriptor.as_ref()).and_then(|descriptor| descriptor_wire(descriptor).encode_vec()).map_or(Vec::new(), |encoded| bootproto::sha256::digest(&encoded).to_vec());
		let (utc_seconds, utc_provenance) = match self.utc {
			Some((unix, at)) => (Some(unix + clock().saturating_sub(at) / 100), String::from("time-service")),
			None => (None, String::new()),
		};
		let mut record = AdminRecord { sequence, broker_epoch: self.broker.epoch, request, launch, requester, action, digest, event: event_wire(event), reason: bounded(reason, 64), monotonic_ns: clock_ns(), utc_seconds, utc_provenance };
		let bytes = match record.encode_vec().and_then(|encoded| aj::frame(&encoded).ok()) {
			Some(bytes) => bytes,
			// A RECORD PAST ITS BOUND carries a bounded refusal instead of what would not fit.
			None => {
				record.requester = String::new();
				record.digest = Vec::new();
				record.reason = String::from("record past its bound");
				match record.encode_vec().and_then(|encoded| aj::frame(&encoded).ok()) {
					Some(bytes) => bytes,
					None => return,
				}
			}
		};
		let evicted = self.journal.evicted;
		self.journal.push(aj::Entry { sequence, request, bytes, first_attempt: true });
		// THE RING IS FULL AND STORAGE IS NOT TAKING ANYTHING: what it dropped is counted, and said, with the
		// oldest record it still holds.
		if self.journal.evicted != evicted {
			let oldest = self.journal.head().map_or(sequence, |entry| entry.sequence);
			print(format!("AdminService: the emergency ring dropped a record; {} dropped so far, the oldest held is {oldest}\n", self.journal.evicted).as_bytes());
		}
		self.owed.push((sequence, request, event));
		// WHAT THE RING DROPPED is owed to nobody any more; the broker's own deadline answers for it.
		let journal = &self.journal;
		self.owed.retain(|(sequence, ..)| journal.holds(*sequence));
	}

	// Start the next write, if nothing is being written and something is waiting.
	fn pump(&mut self) {
		let now = clock();
		if self.writing.is_some() || self.held.is_some() || now < self.retry_at {
			return;
		}
		let Some(head) = self.journal.head() else { return };
		let (sequence, request, frame) = (head.sequence, head.request, head.bytes.clone());
		if self.fault == AdminJournalFault::Fail || self.volume == 0 {
			return self.finish(false);
		}
		let Ok(placement) = self.journal.place(frame.len()) else { return self.finish(false) };
		let mut bytes = Vec::new();
		if placement.new_segment {
			bytes.extend_from_slice(&aj::header(placement.segment, sequence));
		}
		let header = bytes.len();
		bytes.extend_from_slice(&frame);
		let path = segment_path(placement.segment);
		let mode = if placement.new_segment { WriterMode::Replace } else { WriterMode::Append };
		let corr = next(&mut self.vol_corr);
		if !post(self.volume, corr, |capture| {
			let _ = volume::Client::new(capture).open_writer(&path, &mode);
		}) {
			return self.finish(false);
		}
		self.vol_calls.push((corr, VolCall::OpenWriter));
		self.writing = Some(Writing { sequence, request, placement, bytes, header, frame: frame.len(), step: Step::Opening, session: 0, corr, deadline: now + ab::EXCHANGE_TICKS, at: 0, start: None });
	}

	// The next chunk, or the commit once every byte is staged.
	fn write_on(&mut self) {
		let Some(writing) = self.writing.as_mut() else { return };
		let corr = next(&mut self.vol_corr);
		let sent = if writing.at < writing.bytes.len() {
			let end = (writing.at + WRITE_CHUNK).min(writing.bytes.len());
			// THE HEADER IS ITS OWN WRITE, so the first reply after it says exactly where the record begins.
			let end = if writing.at < writing.header { writing.header } else { end };
			let chunk = writing.bytes[writing.at..end].to_vec();
			writing.step = Step::Writing;
			post(writing.session, corr, |capture| {
				let _ = writer::Client::new(capture).write(&chunk);
			})
		} else {
			writing.step = Step::Committing;
			post(writing.session, corr, |capture| {
				let _ = writer::Client::new(capture).commit();
			})
		};
		writing.corr = corr;
		writing.deadline = clock() + ab::EXCHANGE_TICKS;
		if !sent {
			self.abandon(false);
		}
	}

	// The writer answered. A write reports the staged length, which is where this record really began; the
	// commit reports the file's length, which is where the next one will.
	fn on_writer(&mut self, buf: &mut [u8]) {
		let Some(session) = self.writing.as_ref().map(|writing| writing.session) else { return };
		loop {
			let (len, handles) = match try_recv_caps(session, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => return self.abandon(false),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let Some(writing) = self.writing.as_mut() else { return };
			let Some((corr, ok, mut reader)) = reply_head(&buf[..len], &handles) else { return self.abandon(false) };
			if corr != writing.corr {
				continue;
			}
			if !ok {
				return self.abandon(false);
			}
			let Some(staged) = reader.u64() else { return self.abandon(false) };
			match writing.step {
				Step::Writing => {
					let end = if writing.at < writing.header { writing.header } else { (writing.at + WRITE_CHUNK).min(writing.bytes.len()) };
					let written = end - writing.at;
					writing.at = end;
					if writing.start.is_none() {
						writing.start = Some(staged.saturating_sub(written as u64));
					}
					self.write_on();
				}
				Step::Committing => {
					let writing = self.writing.take().expect("a write in progress");
					close(writing.session);
					let offset = writing.start.unwrap_or(0) + writing.header as u64;
					self.journal.committed(writing.placement, writing.sequence, writing.request, writing.frame - aj::FRAME, offset, staged);
					// A COMPLETE SEGMENT RETIRED BY THIS RECORD'S ROTATION goes from the volume too.
					if let Some(retired) = writing.placement.retire {
						let corr = next(&mut self.vol_corr);
						let path = segment_path(retired);
						if post(self.volume, corr, |capture| {
							let _ = volume::Client::new(capture).remove(&path);
						}) {
							self.vol_calls.push((corr, VolCall::Remove));
						}
					}
					self.finish(true);
					return;
				}
				Step::Opening => return self.abandon(false),
			}
		}
	}

	// A write that will not complete: its session is closed, which aborts anything staged.
	fn abandon(&mut self, uncertain: bool) {
		let Some(writing) = self.writing.take() else { return };
		if writing.session != 0 {
			close(writing.session);
		}
		// A COMMIT WHOSE ANSWER NEVER CAME may have landed. It is not replayed - a second copy is not a repair
		// - and it is not a commit either: it is dropped, and the broker hears it failed.
		if uncertain {
			print(b"AdminService: a journal commit went unanswered; the record is not written again\n");
			let _ = self.journal.discard();
			self.owe(writing.sequence, false);
			return;
		}
		self.finish(false);
	}

	// The head's attempt is over. The first attempt of a record is what the broker waits on; a retry answers
	// nobody. A success while an outcome was unrecorded, with nothing left waiting, is storage recovered.
	fn finish(&mut self, ok: bool) {
		if self.fault == AdminJournalFault::Hold {
			self.held = Some(ok);
			return;
		}
		self.writing = None;
		let Some(sequence) = self.journal.head().map(|entry| entry.sequence) else { return };
		let _ = self.journal.written(ok);
		if !ok {
			self.retry_at = clock() + RETRY_TICKS;
		}
		self.owe(sequence, ok);
		if ok && self.broker.blocked && self.journal.waiting() == 0 {
			self.broker.recovered();
			print(b"AdminService: the journal has recovered; approvals are admitted again\n");
		}
	}

	fn owe(&mut self, sequence: u64, ok: bool) {
		let Some(at) = self.owed.iter().position(|(owed, ..)| *owed == sequence) else { return };
		let (_, request, event) = self.owed.remove(at);
		self.step(|broker, now, effects| broker.journaled(request, event, ok, now, effects));
	}

	// The volume answered: a writer session, a removal, or an operator's read.
	fn on_volume(&mut self, buf: &mut [u8]) {
		loop {
			let (len, handles) = match try_recv_caps(self.volume, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					print(b"AdminService: the journal's volume went away; every record now waits in memory\n");
					close(self.volume);
					self.volume = 0;
					self.abandon(false);
					return;
				}
			};
			let Some((corr, ok, mut reader)) = reply_head(&buf[..len], &handles) else {
				for &handle in handles.as_slice() {
					close(handle);
				}
				continue;
			};
			let Some(at) = self.vol_calls.iter().position(|(sent, _)| *sent == corr) else {
				for &handle in handles.as_slice() {
					close(handle);
				}
				continue;
			};
			let (_, call) = self.vol_calls.remove(at);
			match call {
				VolCall::OpenWriter => {
					let session = if ok {
						let _ = reader.u32();
						reader.take_handle()
					} else {
						None
					};
					close_except(&handles, session.as_slice());
					match (session, self.writing.as_mut()) {
						(Some(session), Some(writing)) if writing.corr == corr && writing.step == Step::Opening => {
							writing.session = session;
							self.write_on();
						}
						(Some(session), _) => close(session),
						(None, Some(writing)) if writing.corr == corr => self.abandon(false),
						(None, _) => {}
					}
				}
				VolCall::Remove => close_except(&handles, &[]),
				VolCall::Read => {
					let buffer = if ok { reader.u64().zip(reader.take_handle()) } else { None };
					close_except(&handles, buffer.map(|(_, handle)| handle).as_slice());
					if let Some(at) = self.reads.iter().position(|read| read.vol_corr == corr) {
						let read = self.reads.remove(at);
						self.page(read, buffer.map(|(length, handle)| (handle, length)));
					} else if let Some((_, handle)) = buffer {
						close(handle);
					}
				}
			}
		}
	}

	// An operator's page: up to sixteen retained records from `from`, from one segment at a time, read back
	// from the volume - never from memory, so what it shows is what is durable.
	fn start_read(&mut self, chan: u64, corr: u32, from: u64) {
		let from = from.max(self.journal.oldest());
		let mut located = self.journal.find(from, PAGE);
		if let Some(first) = located.first().copied() {
			located.retain(|entry| entry.segment == first.segment);
		}
		let Some(first) = located.first().copied() else {
			let page = AdminJournalPage { records: Vec::new(), oldest: self.journal.oldest(), next: from, evicted: self.journal.evicted };
			answer(chan, corr, Ok(page), |value, w| value.write(w));
			return;
		};
		let end = located.iter().map(|entry| entry.offset + aj::FRAME as u64 + entry.length as u64).max().unwrap_or(first.offset);
		let length = (end - first.offset).min(u32::MAX as u64) as u32;
		let vol_corr = next(&mut self.vol_corr);
		let path = segment_path(first.segment);
		if self.volume == 0
			|| !post(self.volume, vol_corr, |capture| {
				let _ = volume::Client::new(capture).read(&path, &first.offset, &length);
			}) {
			answer::<AdminJournalPage>(chan, corr, Err(Error::Io), |_, _| Some(()));
			return;
		}
		self.vol_calls.push((vol_corr, VolCall::Read));
		self.reads.push(Read { chan, corr, located, vol_corr, deadline: clock() + ab::EXCHANGE_TICKS });
	}

	fn page(&mut self, read: Read, buffer: Option<(u64, u64)>) {
		let Some((handle, length)) = buffer else {
			answer::<AdminJournalPage>(read.chan, read.corr, Err(Error::Io), |_, _| Some(()));
			return;
		};
		let base = read.located.first().map_or(0, |first| first.offset);
		let mut records = Vec::new();
		if let Some(addr) = unsafe { map_object(handle) } {
			let bytes: &[u8] = unsafe { core::slice::from_raw_parts(addr as *const u8, length as usize) };
			for entry in &read.located {
				let at = (entry.offset - base) as usize + aj::FRAME;
				let end = at + entry.length as usize;
				if end > bytes.len() {
					break;
				}
				match AdminRecord::decode(&bytes[at..end]) {
					Some(record) if record.sequence == entry.sequence => records.push(record),
					_ => break,
				}
			}
			unmap_object(handle);
		}
		close(handle);
		let next = records.last().map_or(read.located.first().map_or(0, |first| first.sequence), |record| record.sequence + 1);
		let page = AdminJournalPage { records, oldest: self.journal.oldest(), next, evicted: self.journal.evicted };
		answer(read.chan, read.corr, Ok(page), |value, w| value.write(w));
	}

	// ------------------------------------------------------------------ the protected session

	// THE PROTECTED SCREEN: the display is locked to this epoch and the keyboard armed for it, and the prompt
	// is drawn once the lock has answered. Nothing a key does counts until both have acknowledged this epoch.
	fn show(&mut self, request: Option<ab::RequestId>, epoch: u64) {
		self.session = Some(Session { epoch, request, decision: Decision::new(epoch), display: 0 });
		let now = clock();
		let corr = next(&mut self.display_corr);
		if post(self.display, corr, |capture| {
			let _ = display_trusted::Client::new(capture).lock(&epoch);
		}) {
			self.display_calls.push((corr, DisplayCall::Lock(epoch), now + ab::EXCHANGE_TICKS));
		} else {
			return self.step(|broker, now, effects| broker.refuse(epoch, "the protected display could not be reached", now, effects));
		}
		let corr = next(&mut self.input_corr);
		if post(self.input, corr, |capture| {
			let _ = input_trusted::Client::new(capture).arm(&epoch);
		}) {
			self.input_calls.push((corr, InputCall::Arm(epoch), now + ab::EXCHANGE_TICKS));
		} else {
			self.step(|broker, now, effects| broker.refuse(epoch, "the trusted keyboard could not be reached", now, effects));
		}
	}

	// The session ends: the keyboard goes back to the ordinary path and the display to the surface that was
	// live before. Neither is waited for.
	fn hide(&mut self) {
		let Some(session) = self.session.take() else { return };
		print(b"AdminService: the protected screen is closed\n");
		let now = clock();
		let corr = next(&mut self.input_corr);
		if post(self.input, corr, |capture| {
			let _ = input_trusted::Client::new(capture).disarm(&session.epoch);
		}) {
			self.input_calls.push((corr, InputCall::Disarm, now + ab::EXCHANGE_TICKS));
		}
		let corr = next(&mut self.display_corr);
		if post(self.display, corr, |capture| {
			let _ = display_trusted::Client::new(capture).release(&session.epoch);
		}) {
			self.display_calls.push((corr, DisplayCall::Release, now + ab::EXCHANGE_TICKS));
		}
		if session.display != 0 {
			close(session.display);
		}
	}

	// The prompt, in the service's own template: the canonical operation first, the requester's words last
	// and escaped. Drawn into memory this service owns and handed to the display to show whole.
	fn draw(&self, screen: &TrustedScreen, request: Option<ab::RequestId>) -> Option<u64> {
		let lines: Vec<String> = match request.and_then(|request| self.broker.request(request)) {
			Some(held) => match held.descriptor.as_ref() {
				Some(descriptor) => {
					let requester = self.broker.connection(held.connection).map_or(String::from("(ended)"), |connection| format!("{}, launch {}", connection.component, connection.launch));
					ad::prompt(descriptor, &requester, &held.asked.label)
				}
				None => ad::idle_prompt(),
			},
			None => ad::idle_prompt(),
		};
		render(screen, &lines)
	}

	// A trusted keyboard event.
	fn on_trusted(&mut self, event: TrustedInput) {
		match event.kind {
			// SECURE ATTENTION: the protected screen, always - with the request waiting, or with nothing. The
			// chord while a session is up changes nothing; and with no display to show it on, the keyboard is
			// handed straight back.
			TrustedInputKind::Attention => {
				if self.session.is_some() {
					return;
				}
				if !self.broker.path {
					let corr = next(&mut self.input_corr);
					if post(self.input, corr, |capture| {
						let _ = input_trusted::Client::new(capture).disarm(&0);
					}) {
						self.input_calls.push((corr, InputCall::Disarm, clock() + ab::EXCHANGE_TICKS));
					}
					return;
				}
				self.sessions = self.sessions.wrapping_add(1);
				let epoch = (self.broker.epoch << 32) | u64::from(self.sessions);
				self.step(|broker, now, effects| broker.attention(epoch, now, effects));
			}
			TrustedInputKind::Armed => {
				if let Some(session) = self.session.as_mut()
					&& session.epoch == event.epoch
				{
					let was = session.decision.ready();
					session.decision.armed(event.epoch);
					print(b"AdminService: the trusted keyboard is idle and the session is armed\n");
					say_ready(was, &session.decision);
				}
			}
			TrustedInputKind::Key => {
				let Some(session) = self.session.as_mut() else { return };
				let epoch = session.epoch;
				match session.decision.key(event.epoch, event.usage, event.down) {
					Some(Verdict::Approve) => {
						// NOTHING WAITING, NOTHING TO APPROVE.
						if session.request.is_none() {
							return;
						}
						// THE SCREEN THE PERSON LOOKED AT MUST STILL BE THERE.
						if session.display == 0 || matches!(try_recv(session.display, &mut [0u8; 8]), Polled::Closed) {
							return self.step(|broker, now, effects| broker.refuse(epoch, "the protected display was lost", now, effects));
						}
						self.step(|broker, now, effects| broker.approve(epoch, now, effects));
					}
					Some(Verdict::Decline) => self.step(|broker, now, effects| broker.refuse(epoch, "declined by the person", now, effects)),
					None => {}
				}
			}
			TrustedInputKind::Lost => {
				print(b"AdminService: the trusted keyboard was lost; requests are declined until it returns\n");
				self.step(|broker, now, effects| broker.set_path(false, now, effects));
			}
		}
	}

	fn on_display(&mut self, buf: &mut [u8]) {
		loop {
			let (len, handles) = match try_recv_caps(self.display, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					print(b"AdminService: the protected display went away; requests are declined\n");
					close(self.display);
					self.display = 0;
					self.step(|broker, now, effects| broker.set_path(false, now, effects));
					return;
				}
			};
			let Some((corr, ok, mut reader)) = reply_head(&buf[..len], &handles) else {
				for &handle in handles.as_slice() {
					close(handle);
				}
				continue;
			};
			let Some(at) = self.display_calls.iter().position(|(sent, ..)| *sent == corr) else {
				for &handle in handles.as_slice() {
					close(handle);
				}
				continue;
			};
			let (_, call, _) = self.display_calls.remove(at);
			let current = self.session.as_ref().map(|session| session.epoch);
			match call {
				DisplayCall::Lock(epoch) => {
					let screen = if ok { TrustedScreen::read(&mut reader) } else { None };
					close_except(&handles, screen.as_ref().map(|screen| screen.session).as_slice());
					match screen {
						Some(screen) if current == Some(epoch) => {
							let request = self.session.as_ref().and_then(|session| session.request);
							if let Some(session) = self.session.as_mut() {
								session.display = screen.session;
							}
							let pixels = self.draw(&screen, request);
							let corr = next(&mut self.display_corr);
							let shown = pixels.is_some_and(|pixels| {
								post(self.display, corr, |capture| {
									let _ = display_trusted::Client::new(capture).present(&epoch, &pixels);
								})
							});
							if shown {
								self.display_calls.push((corr, DisplayCall::Present(epoch), clock() + ab::EXCHANGE_TICKS));
							} else {
								self.step(|broker, now, effects| broker.refuse(epoch, "the protected screen could not be drawn", now, effects));
							}
						}
						// A LOCK FOR A SESSION THAT HAS ENDED is given straight back.
						Some(screen) => {
							close(screen.session);
							let corr = next(&mut self.display_corr);
							if post(self.display, corr, |capture| {
								let _ = display_trusted::Client::new(capture).release(&epoch);
							}) {
								self.display_calls.push((corr, DisplayCall::Release, clock() + ab::EXCHANGE_TICKS));
							}
						}
						None if current == Some(epoch) => self.step(|broker, now, effects| broker.refuse(epoch, "the protected display was unavailable", now, effects)),
						None => {}
					}
				}
				DisplayCall::Present(epoch) => {
					close_except(&handles, &[]);
					if current != Some(epoch) {
						continue;
					}
					if ok {
						if let Some(session) = self.session.as_mut() {
							let was = session.decision.ready();
							session.decision.presented(epoch);
							print(if session.request.is_some() { b"AdminService: the protected screen is presented, and a request is waiting\n".as_slice() } else { b"AdminService: the protected screen is presented, and nothing is waiting\n".as_slice() });
							say_ready(was, &session.decision);
						}
					} else {
						self.step(|broker, now, effects| broker.refuse(epoch, "the protected screen was not presented", now, effects));
					}
				}
				DisplayCall::Release => close_except(&handles, &[]),
			}
		}
	}

	fn on_input(&mut self, buf: &mut [u8]) {
		loop {
			let (len, handles) = match try_recv_caps(self.input, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					close(self.input);
					self.input = 0;
					self.step(|broker, now, effects| broker.set_path(false, now, effects));
					return;
				}
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let Some((corr, ok, _)) = reply_head(&buf[..len], &handles) else { continue };
			let Some(at) = self.input_calls.iter().position(|(sent, ..)| *sent == corr) else { continue };
			let (_, call, _) = self.input_calls.remove(at);
			if let InputCall::Arm(epoch) = call
				&& !ok && self.session.as_ref().is_some_and(|session| session.epoch == epoch)
			{
				self.step(|broker, now, effects| broker.refuse(epoch, "the trusted keyboard refused the session", now, effects));
			}
		}
	}

	fn on_events(&mut self, buf: &mut [u8]) {
		loop {
			let (len, handles) = match try_recv_caps(self.events, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					print(b"AdminService: the trusted keyboard's stream closed; requests are declined\n");
					close(self.events);
					self.events = 0;
					self.step(|broker, now, effects| broker.set_path(false, now, effects));
					return;
				}
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = Handles::new();
			if let Some(event) = input_trusted::events_read(&buf[..len], &mut frame_handles) {
				self.on_trusted(event);
			}
		}
	}

	// ------------------------------------------------------------------ executors

	fn adopt(&mut self, info: ProviderInfo) {
		let Some(action) = action_of(info.name.as_bytes()) else {
			print(b"AdminService: a publication under a name no action is registered to was not adopted\n");
			return;
		};
		if self.executor_for(action).is_some() {
			print(b"AdminService: a second executor for one action was refused\n");
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::with_deadline(ChannelTransport { chan: self.catalogue }, clock() + ab::EXCHANGE_TICKS).open(&info) else {
			print(b"AdminService: a published executor could not be opened\n");
			return;
		};
		let key = self.next_executor;
		self.next_executor = self.next_executor.wrapping_add(1).max(1);
		self.executors.push(Executor { key, info, chan, action, next_corr: 1, sent: Vec::new() });
		print(b"AdminService: an executor was registered for ");
		print(action.name().as_bytes());
		print(b"\n");
	}

	fn lose_executor(&mut self, key: u32) {
		if let Some(at) = self.executors.iter().position(|executor| executor.key == key) {
			let executor = self.executors.remove(at);
			close(executor.chan);
			print(b"AdminService: an executor went away; its preparations and unused grants are void\n");
		}
		self.step(|broker, now, effects| broker.executor_lost(key, now, effects));
	}

	fn on_executor(&mut self, at: usize, buf: &mut [u8]) -> bool {
		let key = self.executors[at].key;
		let chan = self.executors[at].chan;
		loop {
			let (len, handles) = match try_recv_caps(chan, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return true,
				PolledCaps::Closed => return false,
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let Some(at) = self.executors.iter().position(|executor| executor.key == key) else { return true };
			let Some((corr, ok, mut reader)) = reply_head(&buf[..len], &handles) else { return false };
			let Some(place) = self.executors[at].sent.iter().position(|(sent, _)| *sent == corr) else { continue };
			let (_, sent) = self.executors[at].sent.remove(place);
			match sent {
				Sent::Prepare(request) => {
					let prepared = if ok { AdminPrepared::read(&mut reader) } else { None };
					let name = self.executors[at].info.name.clone();
					match prepared {
						// THE EXECUTOR NAMES ITSELF AS PUBLISHED, or what it prepared is not believed.
						Some(prepared) if prepared.descriptor.executor.as_bytes() == name.as_bytes() => {
							let descriptor = descriptor_from(&prepared.descriptor);
							let encoded = prepared.descriptor.encode_vec().map_or(usize::MAX, |bytes| bytes.len());
							self.operation_ms.push((request, prepared.deadline_ms));
							let operation = prepared.operation;
							self.step(|broker, now, effects| broker.prepared(request, Ok((descriptor, encoded, operation)), now, effects));
						}
						Some(prepared) => {
							self.release(key, prepared.operation);
							self.step(|broker, now, effects| broker.prepared(request, Err("the executor named another executor"), now, effects));
						}
						None => self.step(|broker, now, effects| broker.prepared(request, Err("the executor refused to prepare it"), now, effects)),
					}
				}
				Sent::Revalidate(request) => self.step(|broker, now, effects| broker.revalidated(request, ok, now, effects)),
				Sent::Execute(request) => {
					let outcome = if ok {
						match AdminResult::read(&mut reader) {
							Some(AdminResult::Completed) => Outcome::Completed,
							Some(AdminResult::Failed) => Outcome::Failed,
							_ => Outcome::Unknown,
						}
					} else {
						// REFUSED UNDER ITS START GUARD: nothing was attempted.
						Outcome::Failed
					};
					self.step(|broker, now, effects| broker.executed(request, outcome, now, effects));
				}
				Sent::Release => {}
			}
		}
	}

	// ------------------------------------------------------------------ deadlines

	fn deadlines(&mut self) {
		let now = clock();
		self.step(|broker, now, effects| broker.tick(now, effects));
		// A DISPATCH PAST THE EXECUTOR'S OWN DEADLINE is an outcome nobody observed - never retried.
		let late: Vec<ab::RequestId> = self.dispatched.iter().filter(|(_, by)| now >= *by).map(|(request, _)| *request).collect();
		for request in late {
			self.dispatched.retain(|(held, _)| *held != request);
			self.step(|broker, now, effects| broker.executed(request, Outcome::Unknown, now, effects));
		}
		// A DISPLAY OR KEYBOARD THAT DID NOT ACKNOWLEDGE ends the session it was asked about.
		let current = self.session.as_ref().map(|session| session.epoch);
		let silent = self.display_calls.iter().any(|(_, call, by)| now >= *by && matches!(call, DisplayCall::Lock(epoch) | DisplayCall::Present(epoch) if Some(*epoch) == current)) || self.input_calls.iter().any(|(_, call, by)| now >= *by && matches!(call, InputCall::Arm(epoch) if Some(*epoch) == current));
		self.display_calls.retain(|(_, _, by)| now < *by);
		self.input_calls.retain(|(_, _, by)| now < *by);
		if silent && let Some(epoch) = current {
			self.step(|broker, now, effects| broker.refuse(epoch, "the protected display or keyboard did not answer", now, effects));
		}
		// A JOURNAL WRITE THAT DID NOT ANSWER. Before its commit it is an abort and is tried again; at its
		// commit, it is uncertain.
		if let Some(step) = self.writing.as_ref().filter(|writing| now >= writing.deadline).map(|writing| writing.step) {
			self.abandon(step == Step::Committing);
		}
		let late: Vec<usize> = self.reads.iter().enumerate().filter(|(_, read)| now >= read.deadline).map(|(at, _)| at).collect();
		for at in late.into_iter().rev() {
			let read = self.reads.remove(at);
			self.vol_calls.retain(|(corr, _)| *corr != read.vol_corr);
			answer::<AdminJournalPage>(read.chan, read.corr, Err(Error::TimedOut), |_, _| Some(()));
		}
		if self.time_asked.is_some_and(|(_, by)| now >= by) {
			self.time_asked = None;
		}
	}

	fn next_deadline(&self) -> Option<u64> {
		let mut deadlines: Vec<u64> = Vec::new();
		deadlines.extend(self.broker.next_deadline());
		deadlines.extend(self.dispatched.iter().map(|(_, by)| *by));
		deadlines.extend(self.display_calls.iter().map(|(_, _, by)| *by));
		deadlines.extend(self.input_calls.iter().map(|(_, _, by)| *by));
		deadlines.extend(self.writing.as_ref().map(|writing| writing.deadline));
		deadlines.extend(self.reads.iter().map(|read| read.deadline));
		if self.writing.is_none() && self.held.is_none() && self.journal.waiting() > 0 {
			deadlines.push(self.retry_at);
		}
		if self.time != 0 {
			deadlines.push(self.time_asked.map_or(self.time_at + TIME_REFRESH_TICKS, |(_, by)| by));
		}
		deadlines.into_iter().min()
	}

	// ------------------------------------------------------------------ the wall clock

	fn ask_time(&mut self) {
		let now = clock();
		if self.time == 0 || self.time_asked.is_some() || (self.time_at != 0 && now < self.time_at + TIME_REFRESH_TICKS) {
			return;
		}
		let corr = next(&mut self.time_corr);
		if post(self.time, corr, |capture| {
			let _ = time::Client::new(capture).now();
		}) {
			self.time_asked = Some((corr, now + ab::EXCHANGE_TICKS));
		}
		self.time_at = now;
	}

	fn on_time(&mut self, buf: &mut [u8]) {
		loop {
			let (len, handles) = match try_recv_caps(self.time, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					close(self.time);
					self.time = 0;
					self.utc = None;
					return;
				}
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let Some((corr, ok, mut reader)) = reply_head(&buf[..len], &handles) else { continue };
			if self.time_asked.is_none_or(|(asked, _)| asked != corr) {
				continue;
			}
			self.time_asked = None;
			if ok && let Some(stamp) = Timestamp::read(&mut reader) {
				self.utc = Some((stamp.unix_secs, clock()));
			}
		}
	}

	// ------------------------------------------------------------------ requesters

	// A request connection spoke: a request, a withdrawal, or something it cannot carry.
	fn on_conn(&mut self, at: usize, buf: &mut [u8], reply_buf: &mut [u8]) {
		let chan = self.conns[at].chan;
		let id = self.conns[at].id;
		loop {
			let (len, mut handles) = match try_recv_caps(chan, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				// THE CONNECTION CLOSED - whoever held it - and that is requester loss on its own.
				PolledCaps::Closed => {
					self.step(|broker, now, effects| broker.closed(id, now, effects));
					if let Some(conn) = self.conns.iter_mut().find(|conn| conn.id == id) {
						close(conn.chan);
						conn.chan = 0;
						conn.call = None;
					}
					return;
				}
			};
			let mut view = RequestView { asked: None };
			let mut reply_handles = Handles::new();
			let written = admin_request::dispatch(&mut view, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			for &leftover in reply_handles.as_slice() {
				close(leftover);
			}
			let corr = if len >= 6 { u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]) } else { 0 };
			match view.asked {
				Some(Asked2::Request(args, payload)) => {
					let busy = self.conns.iter().any(|conn| conn.id == id && conn.call.is_some());
					let asked = Asked { action: action_from(args.action), target: args.target, parameters: args.parameters, payload_length: args.payload_length, label: args.label };
					let mut effects = Vec::new();
					let request = if busy { None } else { self.broker.ask(id, asked, clock(), &mut effects) };
					match request {
						Some(request) => {
							if let Some(conn) = self.conns.iter_mut().find(|conn| conn.id == id) {
								conn.call = Some(corr);
							}
							self.payloads.push((request, payload));
							self.apply(effects);
						}
						// ONE REQUEST AT A TIME ON A CONNECTION.
						None => {
							close(payload);
							answer::<AdminAnswer>(chan, corr, Err(Error::Again), |_, _| Some(()));
						}
					}
				}
				Some(Asked2::Cancel) => {
					answer(chan, corr, Ok(()), |_, _| Some(()));
					self.step(|broker, now, effects| broker.cancel(id, now, effects));
				}
				None => {
					// Answered by the generated code - the protocol's own questions - or refused as malformed.
					match written {
						Some(written) => {
							let _ = try_send(chan, &reply_buf[..written], 0);
						}
						None if len >= 6 => {
							answer::<()>(chan, corr, Err(Error::Invalid), |_, _| Some(()));
						}
						None => {}
					}
				}
			}
		}
	}

	fn on_grant(&mut self, at: usize, buf: &mut [u8], reply_buf: &mut [u8]) {
		let chan = self.grants[at].chan;
		let request = self.grants[at].request;
		loop {
			let (len, mut handles) = match try_recv_caps(chan, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				// A GRANT NOBODY HOLDS any more is a grant that will not be redeemed; it expires as it would, and its
				// channel leaves the wait set - a closed peer is always ready.
				PolledCaps::Closed => {
					if let Some(grant) = self.grants.iter_mut().find(|grant| grant.request == request) {
						grant.call = None;
						close(grant.chan);
						grant.chan = 0;
					}
					return;
				}
			};
			let mut view = GrantView { asked: false };
			let mut reply_handles = Handles::new();
			let written = admin_authority::dispatch(&mut view, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			for &leftover in reply_handles.as_slice() {
				close(leftover);
			}
			let corr = if len >= 6 { u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]) } else { 0 };
			if !view.asked {
				if let Some(written) = written {
					let _ = try_send(chan, &reply_buf[..written], 0);
				}
				continue;
			}
			// EXECUTE. A second call while the first waits, a spent grant, an expired one, or one whose owner has
			// ended: refused, and nothing reaches an executor.
			let waiting = self.grants.iter().any(|grant| grant.request == request && grant.call.is_some());
			let mut effects = Vec::new();
			let admitted = !waiting && self.broker.redeem(request, clock(), &mut effects);
			if admitted {
				if let Some(grant) = self.grants.iter_mut().find(|grant| grant.request == request) {
					grant.call = Some(corr);
				}
			} else {
				answer::<AdminResult>(chan, corr, Err(Error::Denied), |_, _| Some(()));
			}
			self.apply(effects);
			if self.grants.iter().all(|grant| grant.request != request) {
				return;
			}
		}
	}

	// ------------------------------------------------------------------ the owners

	// EVERY LAUNCHING TASK, ASKED EXPLICITLY, before anything else that is ready is looked at: an
	// already-reached deadline makes each wait a check. Ended is requester loss; a wait that fails is an
	// observer this service cannot trust, and is the same loss.
	fn check_owners(&mut self) {
		let reached = clock().max(1);
		let mut lost: Vec<(ab::ConnectionId, bool)> = Vec::new();
		for conn in &mut self.conns {
			if conn.owner == 0 {
				continue;
			}
			let observed = wait(conn.owner, reached);
			if observed == ERR_TIMED_OUT {
				continue;
			}
			close(conn.owner);
			conn.owner = 0;
			lost.push((conn.id, observed == 0));
		}
		for (connection, ended) in lost {
			if ended {
				self.step(|broker, now, effects| broker.owner_died(connection, now, effects));
			} else {
				self.step(|broker, now, effects| broker.observer_failed(connection, now, effects));
			}
		}
	}

	// A request finished and was answered: its connection may ask again, its reservation is given back, and a
	// connection nobody holds and nothing refers to is gone.
	fn settle(&mut self) {
		let before: Vec<ab::RequestId> = self.broker.requests().iter().map(|request| request.id).collect();
		self.broker.settle();
		for request in before {
			if self.broker.request(request).is_none() {
				self.journal.unpin(request);
				self.operation_ms.retain(|(held, _)| *held != request);
				if let Some(at) = self.payloads.iter().position(|(held, _)| *held == request) {
					close(self.payloads.remove(at).1);
				}
			}
		}
		let broker = &self.broker;
		self.conns.retain(|conn| {
			if broker.connection(conn.id).is_some() {
				return true;
			}
			if conn.chan != 0 {
				close(conn.chan);
			}
			if conn.owner != 0 {
				close(conn.owner);
			}
			false
		});
	}
}

// ------------------------------------------------------------------ drawing

// Colours, blue-green-red-unused in memory: a deep blue field, an amber band, white text. The field is what
// a captured frame is checked for.
const FIELD: [u8; 4] = [0x50, 0x20, 0x10, 0x00];
const BAND: [u8; 4] = [0x00, 0xb0, 0xff, 0x00];
const TEXT: [u8; 4] = [0xff, 0xff, 0xff, 0x00];

fn render(screen: &TrustedScreen, lines: &[String]) -> Option<u64> {
	let (width, height, pitch) = (screen.width as usize, screen.height as usize, screen.pitch as usize);
	let length = pitch.checked_mul(height)?;
	let handle = memory_object_create(length as u64);
	if handle < 0 {
		return None;
	}
	let handle = handle as u64;
	let Some(addr) = (unsafe { map_object(handle) }) else {
		close(handle);
		return None;
	};
	let pixels: &mut [u8] = unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, length) };
	let scale: usize = if width >= 1024 { 2 } else { 1 };
	let band: usize = 8 * scale;
	for row in 0..height {
		let colour = if row < band || row + band >= height { BAND } else { FIELD };
		for column in 0..width {
			let at = row * pitch + column * 4;
			pixels[at..at + 4].copy_from_slice(&colour);
		}
	}
	let (cell_w, cell_h) = (8 * scale, 16 * scale);
	let left = (4 * cell_w).min(width / 8);
	let mut top = band + 2 * cell_h;
	for line in lines {
		let mut x = left;
		for character in line.chars() {
			if x + cell_w > width || top + cell_h > height {
				break;
			}
			let glyph = term::glyph(character);
			for (glyph_row, bits) in glyph.iter().enumerate() {
				for bit in 0..8 {
					if bits & (0x80 >> bit) == 0 {
						continue;
					}
					for dy in 0..scale {
						for dx in 0..scale {
							let at = (top + glyph_row * scale + dy) * pitch + (x + bit * scale + dx) * 4;
							pixels[at..at + 4].copy_from_slice(&TEXT);
						}
					}
				}
			}
			x += cell_w;
		}
		top += cell_h + cell_h / 2;
	}
	unmap_object(handle);
	let shared = duplicate(handle, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
	close(handle);
	if shared > 0 { Some(shared as u64) } else { None }
}

// ------------------------------------------------------------------ recovery

// Read back what the journal holds, before anything is served: every segment's frames, in order, and the
// highest broker epoch recorded. FOR INVESTIGATION AND NUMBERING ONLY: nothing read here becomes authority.
// Each exchange has five seconds; a journal that cannot be read starts empty, numbered from one.
fn recover(volume: u64) -> (Vec<aj::Segment>, Vec<aj::Located>, u64) {
	let mut segments: Vec<aj::Segment> = Vec::new();
	let mut index: Vec<aj::Located> = Vec::new();
	let mut epoch: u64 = 0;
	if volume == 0 {
		return (segments, index, epoch);
	}
	let deadline = || clock() + ab::EXCHANGE_TICKS;
	let names: Vec<u64> = match volume::Client::with_deadline(ChannelTransport { chan: volume }, deadline()).list(JOURNAL_DIR) {
		Some(Ok(consumer)) => drain_names(consumer),
		_ => Vec::new(),
	};
	let mut numbers = names;
	numbers.sort_unstable();
	// ONLY THE NEWEST FOUR ARE KEPT: anything older is what a rotation that crashed before its removal left.
	while numbers.len() > aj::SEGMENTS {
		let stale = numbers.remove(0);
		let _ = volume::Client::with_deadline(ChannelTransport { chan: volume }, deadline()).remove(&segment_path(stale));
	}
	for number in numbers {
		let opened = volume::Client::with_deadline(ChannelTransport { chan: volume }, deadline()).open(&OpenOpts { path: segment_path(number), write: false, create: false });
		let Some(Ok(opened)) = opened else { continue };
		let Some(addr) = (unsafe { map_object(opened.file) }) else {
			close(opened.file);
			continue;
		};
		let bytes: &[u8] = unsafe { core::slice::from_raw_parts(addr as *const u8, opened.size as usize) };
		if let Some((found, first, frames)) = aj::scan(bytes) {
			let mut end = aj::HEADER as u64;
			let mut records = 0u32;
			for (offset, frame) in &frames {
				let Some(record) = AdminRecord::decode(frame) else { break };
				epoch = epoch.max(record.broker_epoch);
				index.push(aj::Located { sequence: record.sequence, segment: found, offset: *offset, length: frame.len() as u32 });
				records += 1;
				end = offset + aj::FRAME as u64 + frame.len() as u64;
			}
			// A TORN TAIL IS NEVER APPENDED TO: anything after it would be unreadable, so the segment counts as
			// complete and the next record starts a new one.
			let torn = end != opened.size;
			segments.push(aj::Segment { number: found, first, records: if torn { aj::RECORDS } else { records }, bytes: opened.size });
		}
		unmap_object(opened.file);
		close(opened.file);
	}
	(segments, index, epoch)
}

// The segment numbers a listing names, read with a bound: a volume that stops answering ends the listing.
fn drain_names(consumer: u64) -> Vec<u64> {
	let mut numbers = Vec::new();
	let mut buf = alloc::vec![0u8; 1024];
	let until = clock() + ab::EXCHANGE_TICKS;
	loop {
		if wait(consumer, until) < 0 {
			break;
		}
		match try_recv_caps(consumer, &mut buf) {
			PolledCaps::Message { len: 0, handles } => {
				for &handle in handles.as_slice() {
					close(handle);
				}
				break;
			}
			PolledCaps::Message { len, handles } => {
				let mut frame = Handles::try_from_slice(handles.as_slice()).unwrap_or_default();
				if let Some(entry) = volume::list_read(&buf[..len], &mut frame)
					&& let Some(hex) = entry.name.strip_prefix("segment-")
					&& let Ok(number) = u64::from_str_radix(hex, 16)
				{
					numbers.push(number);
				}
				for &handle in frame.as_slice() {
					close(handle);
				}
			}
			PolledCaps::Empty => continue,
			PolledCaps::Closed => break,
		}
	}
	close(consumer);
	numbers
}

// ------------------------------------------------------------------ the loop

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let [journal_root, time_root, display_root, input_root, catalogue, factory_root, audit_root, test_root] = roles;
	// THE TEST CONTROLS EXIST IN A DEVELOPMENT IMAGE ALONE. A shipping build closes the root it was handed, so
	// no connection to it can ever be made.
	#[cfg(not(feature = "development"))]
	let test_root = {
		if test_root != 0 {
			close(test_root);
		}
		0u64
	};
	let (segments, index, recovered_epoch) = recover(journal_root);
	let mut journal = aj::Journal::new();
	let recovered = index.len();
	journal.recover(segments, index);
	// A FRESH EPOCH, past every one the journal recorded, so nothing this instance issues can be confused with
	// what an earlier one did.
	let mut broker = ab::Broker::new(recovered_epoch + 1);
	let events: u64 = if input_root != 0 { input_trusted::Client::with_deadline(ChannelTransport { chan: input_root }, clock() + ab::EXCHANGE_TICKS).events().unwrap_or(0) } else { 0 };
	// A PATH TO A PERSON, OR NONE: without both a trusted display and a trusted keyboard every request is
	// declined.
	broker.path = display_root != 0 && events != 0;
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::AdminExecutor).unwrap_or(0) } else { 0 };
	let mut service = Service { broker, journal, executors: Vec::new(), conns: Vec::new(), grants: Vec::new(), payloads: Vec::new(), operation_ms: Vec::new(), dispatched: Vec::new(), owed: Vec::new(), writing: None, retry_at: 0, fault: AdminJournalFault::None, held: None, reads: Vec::new(), vol_calls: Vec::new(), session: None, sessions: 0, display_calls: Vec::new(), input_calls: Vec::new(), volume: journal_root, display: display_root, input: input_root, events, time: time_root, catalogue, vol_corr: 1, display_corr: 1, input_corr: 1, time_corr: 1, time_asked: None, time_at: 0, utc: None, next_executor: 1 };
	{
		let mut line = format!("AdminService: online, epoch {}, {} journal records recovered", service.broker.epoch, recovered);
		if !service.broker.path {
			line.push_str(" - no trusted display and keyboard, so every request is declined");
		}
		line.push('\n');
		print(line.as_bytes());
	}
	send_blocking(bootstrap, b"AdminService: online", 0);

	let mut factories: Vec<u64> = Vec::new();
	let mut readers: Vec<u64> = Vec::new();
	let mut testers: Vec<u64> = Vec::new();
	let mut subscribed = subscription != 0;
	let mut buf = alloc::vec![0u8; 8192];
	let mut reply_buf = alloc::vec![0u8; 8192];
	loop {
		service.ask_time();
		service.pump();
		let mut waitset: Vec<u64> = Vec::new();
		for root in [factory_root, audit_root, test_root, service.volume, service.display, service.input, service.events, service.time] {
			if root != 0 {
				waitset.push(root);
			}
		}
		if subscribed {
			waitset.push(subscription);
		}
		if let Some(writing) = service.writing.as_ref()
			&& writing.session != 0
		{
			waitset.push(writing.session);
		}
		if let Some(session) = service.session.as_ref()
			&& session.display != 0
		{
			waitset.push(session.display);
		}
		waitset.extend(factories.iter().copied());
		waitset.extend(readers.iter().copied());
		waitset.extend(testers.iter().copied());
		waitset.extend(service.executors.iter().map(|executor| executor.chan));
		for conn in &service.conns {
			if conn.chan != 0 {
				waitset.push(conn.chan);
			}
			if conn.owner != 0 {
				waitset.push(conn.owner);
			}
		}
		waitset.extend(service.grants.iter().filter(|grant| grant.chan != 0).map(|grant| grant.chan));
		let now = clock();
		let deadline = service.next_deadline().map_or(0, |deadline| deadline.max(now + 1));
		let ready = wait_any(&waitset, deadline);
		// OWNERS FIRST, whatever woke this loop.
		service.check_owners();
		service.deadlines();
		if ready >= 0 {
			let handle = waitset[ready as usize];
			serve(&mut service, handle, [factory_root, audit_root, test_root], &mut factories, &mut readers, &mut testers, subscription, &mut subscribed, &mut buf, &mut reply_buf);
		}
		service.settle();
	}
}

#[allow(clippy::too_many_arguments)]
fn serve(service: &mut Service, handle: u64, [factory_root, audit_root, test_root]: [u64; 3], factories: &mut Vec<u64>, readers: &mut Vec<u64>, testers: &mut Vec<u64>, subscription: u64, subscribed: &mut bool, buf: &mut [u8], reply_buf: &mut [u8]) {
	if handle == service.volume {
		return service.on_volume(buf);
	}
	if service.writing.as_ref().is_some_and(|writing| writing.session == handle) {
		return service.on_writer(buf);
	}
	if handle == service.display {
		return service.on_display(buf);
	}
	if handle == service.input {
		return service.on_input(buf);
	}
	if handle == service.events {
		return service.on_events(buf);
	}
	if handle == service.time {
		return service.on_time(buf);
	}
	// THE PROTECTED DISPLAY'S SESSION ENDED - a reset, a lost scanout - and the request on it with it.
	if let Some(epoch) = service.session.as_ref().filter(|session| session.display == handle).map(|session| session.epoch) {
		if let PolledCaps::Closed = try_recv_caps(handle, buf) {
			if let Some(session) = service.session.as_mut() {
				close(session.display);
				session.display = 0;
			}
			service.step(|broker, now, effects| broker.refuse(epoch, "the protected display was lost", now, effects));
			service.hide();
		}
		return;
	}
	if let Some(at) = service.executors.iter().position(|executor| executor.chan == handle) {
		if !service.on_executor(at, buf) {
			let key = service.executors[at].key;
			service.lose_executor(key);
		}
		return;
	}
	if let Some(at) = service.conns.iter().position(|conn| conn.chan == handle) {
		return service.on_conn(at, buf, reply_buf);
	}
	// AN OWNER BECAME READY: `check_owners` has already acted on it.
	if service.conns.iter().any(|conn| conn.owner == handle) {
		return;
	}
	if let Some(at) = service.grants.iter().position(|grant| grant.chan == handle) {
		return service.on_grant(at, buf, reply_buf);
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
			let mut frame = Handles::try_from_slice(handles.as_slice()).unwrap_or_default();
			let info = provider_catalogue::subscribe_read(&buf[..len], &mut frame);
			for &leftover in frame.as_slice() {
				close(leftover);
			}
			let Some(info) = info else { continue };
			if info.live {
				service.adopt(info);
			} else if let Some(key) = service.executors.iter().find(|executor| executor.info.slot == info.slot && executor.info.provider_generation == info.provider_generation).map(|executor| executor.key) {
				service.lose_executor(key);
			}
		}
		return;
	}
	serve_root(service, handle, [factory_root, audit_root, test_root], factories, readers, testers, buf, reply_buf);
}

// The three roots and the connections minted from them: PermissionManager's factory, the operator's journal
// view, and - in a development image - the test controls.
#[allow(clippy::too_many_arguments)]
fn serve_root(service: &mut Service, handle: u64, [factory_root, audit_root, test_root]: [u64; 3], factories: &mut Vec<u64>, readers: &mut Vec<u64>, testers: &mut Vec<u64>, buf: &mut [u8], reply_buf: &mut [u8]) {
	let (root, minted, most): (u64, &mut Vec<u64>, usize) = if handle == factory_root || factories.contains(&handle) {
		(factory_root, factories, MAX_FACTORIES)
	} else if handle == audit_root || readers.contains(&handle) {
		(audit_root, readers, MAX_READERS)
	} else if test_root != 0 && (handle == test_root || testers.contains(&handle)) {
		(test_root, testers, MAX_TESTERS)
	} else {
		return;
	};
	let is_root = handle == root;
	let (len, mut handles) = match try_recv_caps(handle, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			if !is_root {
				minted.retain(|&held| held != handle);
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
		// FROM THE ROOT OR FROM A CONNECTION IT MINTED: a resolver keeps the connection the broker minted
		// for it and mints its own from that one, as every root's connections allow - refusing it here
		// refused every grant the resolver was asked for.
		if op == CONNECT_OP {
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			match (minted.len() < most).then(channel).flatten() {
				Some((mine, theirs)) => {
					minted.push(mine);
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
	if root == factory_root {
		let written = admin_factory::dispatch(&mut FactoryView { service: &mut *service }, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
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
				minted.retain(|&held| held != handle);
				close(handle);
			}
		}
		return;
	}
	if root == audit_root {
		let mut view = JournalView { asked: None };
		let written = admin_journal::dispatch(&mut view, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		for &leftover in reply_handles.as_slice() {
			close(leftover);
		}
		let corr = if len >= 6 { u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]) } else { 0 };
		match (view.asked, written) {
			(Some(from), _) => service.start_read(handle, corr, from),
			(None, Some(written)) => {
				let _ = try_send(handle, &reply_buf[..written], 0);
			}
			(None, None) => {
				minted.retain(|&held| held != handle);
				close(handle);
			}
		}
		return;
	}
	let written = admin_test::dispatch(&mut TestView { service: &mut *service }, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	if let Some(written) = written {
		let _ = try_send(handle, &reply_buf[..written], 0);
	}
	// A HELD ACKNOWLEDGMENT IS RELEASED when the fault is lifted, and only then.
	if service.fault != AdminJournalFault::Hold
		&& let Some(ok) = service.held.take()
	{
		service.finish(ok);
	}
}
