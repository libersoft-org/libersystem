// SmartcardService - smart-card readers, exclusive expiring transactions over their slots, and a
// bounded set of PIV operations, for components PermissionManager minted a reader-scoped connection for.
//
// WHAT IT HOLDS AND WHAT IT DOES NOT. It consumes every `smartcard-reader` publication through a
// catalogue connection minted for that kind alone; a provider - the USB CCID class module, the in-guest
// fixture - owns the transport, and this service owns who may do what. It has no device claim, no input
// capability and no storage: a PIN never reaches it, because the reader's own keypad collects it, and
// there is no operation through which an application could supply one.
//
// EVERY DECISION IS IN `service_logic`: the command allowlist and the PIV grammar in `piv`, and the
// slots, transactions, leases, aborts, resets and removals in `card_slots`, all host-tested. What is
// here is the IO around them - minting, decoding, sending, answering - and the rule that nothing in it
// waits on anybody but `wait_any`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{ApduResponse, AuthenticateResult, CardEvent, CardOutcome, CardProtocol, Error, EventKind, ExchangeLevel, ExchangeResult, Minted, Operations, PinOutcome, PinResult, Presence, ProviderInfo, ProviderKind, ProviderOutcome, ProviderReply, ReaderDescription, ReaderEventKind, ReaderId, ReaderInfo, RequestHeader, SecureVerifyRequest, SlotReport, SlotState, Transaction, provider_catalogue, smartcard, smartcard_admin, smartcard_reader};
use rt::*;
use service_logic::card_slots as cs;
use service_logic::piv;
use wire::{Handles, Reader, Sink, Transport, TransportError};

include!(concat!(env!("OUT_DIR"), "/roles_smartcard_service.rs"));

// THE CONFIGURED READERS: an alias a policy row may name, and the provider metadata it binds - the
// publication name the reader's provider chose. A shipping build configures none, which is the default
// the milestone asks for: no reader grant at all until somebody configures one. A development build
// configures the two fixture readers its gate uses.
#[cfg(feature = "development")]
const ALIASES: &[(&str, &[u8])] = &[("fixture-a", b"org.libersystem.smartcard-fixture.a"), ("fixture-b", b"org.libersystem.smartcard-fixture.b")];
#[cfg(not(feature = "development"))]
const ALIASES: &[(&str, &[u8])] = &[];

// Admin connections minted from the root: PermissionManager's, and a spare across its restart.
const MAX_ADMINS: usize = 4;
// Event subscriptions one grant may hold at once.
const MAX_STREAMS: usize = 2;
// How long a provider has to describe itself and open its event stream, and how long a new session's
// quiescence may take before the reader is passed over.
const DESCRIBE_TICKS: u64 = 100;
const SESSION_TICKS: u64 = 1000;
// A pinpad's own entry timeout, which the service's deadline cuts short if it must.
const PINPAD_SECONDS: u8 = 30;
const FRAME_BYTES: usize = 1024;

struct ReaderConn {
	key: u32,
	info: ProviderInfo,
	chan: u64,
	events: u64,
	description: ReaderDescription,
	pinpad: bool,
	// The open-session request, until its answer admits the reader; and when it was sent.
	opening: Option<(u32, u64)>,
	// What was asked and not answered: the wire correlation, the service's request, the slot and the
	// card generation it named.
	sent: Vec<(u32, u64, u8, u64)>,
	next_corr: u32,
}

struct Stream {
	chan: u64,
	seq: u32,
	queue: cs::EventQueue<Vec<u8>>,
}

struct GrantConn {
	id: u32,
	chan: u64,
	owner: u64,
	reader: u32,
	alias: String,
	operations: cs::Operations,
	streams: Vec<Stream>,
}

struct Service {
	cards: Cards,
	readers: Vec<ReaderConn>,
	grants: Vec<GrantConn>,
	admins: Vec<u64>,
	next_reader: u32,
	next_grant: u32,
	sequence: u64,
}

type Cards = cs::Cards;

// ------------------------------------------------------------------ the wire, written by hand

// A generated client's request, captured instead of sent: the encoding is the generator's, the sending
// is this service's own - non-blocking, under a correlation of its choosing.
struct Capture {
	bytes: Vec<u8>,
}

impl Transport for Capture {
	fn call(&mut self, request: &[u8], _request_handles: &[u64], _reply_handles: &mut Handles, _deadline: u64) -> Result<Vec<u8>, TransportError> {
		self.bytes = request.to_vec();
		Err(TransportError::TimedOut)
	}
	fn discard_handles(&mut self, _handles: &[u64]) {}
}

fn captured(encode: impl FnOnce(&mut smartcard_reader::Client<&mut Capture>), corr: u32) -> Option<Vec<u8>> {
	let mut capture = Capture { bytes: Vec::new() };
	encode(&mut smartcard_reader::Client::new(&mut capture));
	if capture.bytes.len() < 6 {
		return None;
	}
	capture.bytes[2..6].copy_from_slice(&corr.to_le_bytes());
	Some(capture.bytes)
}

impl Transport for &mut Capture {
	fn call(&mut self, request: &[u8], request_handles: &[u64], reply_handles: &mut Handles, deadline: u64) -> Result<Vec<u8>, TransportError> {
		(**self).call(request, request_handles, reply_handles, deadline)
	}
	fn discard_handles(&mut self, handles: &[u64]) {
		(**self).discard_handles(handles)
	}
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

fn refusal(refusal: cs::Refusal) -> Error {
	match refusal {
		cs::Refusal::Denied => Error::Denied,
		cs::Refusal::Unsupported => Error::Unsupported,
		cs::Refusal::Invalid => Error::Invalid,
		cs::Refusal::NotFound => Error::NotFound,
		cs::Refusal::Stale => Error::Stale,
		cs::Refusal::Busy => Error::Again,
		cs::Refusal::Exhausted => Error::Exhausted,
		cs::Refusal::TimedOut => Error::TimedOut,
		cs::Refusal::Unavailable => Error::Io,
		cs::Refusal::Closed => Error::Closed,
	}
}

fn outcome(outcome: cs::Outcome) -> CardOutcome {
	match outcome {
		cs::Outcome::Done => CardOutcome::Done,
		cs::Outcome::CardRemoved => CardOutcome::CardRemoved,
		cs::Outcome::Cancelled => CardOutcome::Cancelled,
		cs::Outcome::TimedOut => CardOutcome::TimedOut,
		cs::Outcome::ProviderError => CardOutcome::ProviderError,
	}
}

fn slot_state(view: &cs::SlotView) -> SlotState {
	SlotState { slot: view.slot, presence: if view.present { Presence::Present } else { Presence::Absent }, card_generation: view.generation, atr: view.atr.clone(), available: view.available }
}

fn reader_id(info: &ProviderInfo) -> ReaderId {
	ReaderId { slot: info.slot, generation: info.provider_generation, binding_generation: info.binding_generation }
}

fn same(a: &ProviderInfo, b: &ProviderInfo) -> bool {
	a.slot == b.slot && a.provider_generation == b.provider_generation && a.binding_generation == b.binding_generation
}

// ------------------------------------------------------------------ the views the generated code calls

// PermissionManager's minting endpoint.
struct AdminView<'a> {
	service: &'a mut Service,
}

impl smartcard_admin::Service for AdminView<'_> {
	// THE ALIAS RESOLVES TO EXACTLY ONE CURRENT PUBLICATION OR THE MINT FAILS. Never the first reader
	// available, never an identity the caller names: the alias is configuration, and the publication is
	// DeviceManager's.
	fn mint(&mut self, alias: String, operations: Operations, owner: u64) -> Result<Minted, Error> {
		let refuse = |error: Error| {
			close(owner);
			Err(error)
		};
		let Some(&(_, bound)) = ALIASES.iter().find(|(name, _)| *name == alias) else { return refuse(Error::NotFound) };
		let service = &mut *self.service;
		let matching: Vec<usize> = service.readers.iter().enumerate().filter(|(_, reader)| reader.opening.is_none() && reader.info.name.as_bytes() == bound).map(|(at, _)| at).collect();
		let at = match matching.as_slice() {
			[at] => *at,
			[] => return refuse(Error::NotFound),
			_ => return refuse(Error::Invalid),
		};
		let key = service.readers[at].key;
		let id = service.next_grant;
		let granted = cs::Operations { read: operations.read, transact: operations.transact, authenticate: operations.authenticate };
		if !service.cards.add_grant(id, key, granted) {
			return refuse(Error::Exhausted);
		}
		let Some((mine, theirs)) = channel() else {
			service.cards.drop_grant(id, clock());
			return refuse(Error::Exhausted);
		};
		service.next_grant = service.next_grant.wrapping_add(1).max(1);
		service.grants.push(GrantConn { id, chan: mine, owner, reader: key, alias, operations: granted, streams: Vec::new() });
		let info = &service.readers[at].info;
		Ok(Minted { connection: theirs, reader: reader_id(info), name: String::from_utf8_lossy(info.name.as_bytes()).into_owned() })
	}
}

// A grant's connection, decoded by the generated dispatch. The synchronous reads are answered through
// it; the rest are recorded here and answered by hand once the card has.
enum Asked {
	Acquire(u8, u32, u32),
	Exchange(u64, Vec<u8>),
	Verify(u64, u32),
	Authenticate(u64, Vec<u8>),
	Finish(u64),
}

struct GrantView<'a> {
	service: &'a Service,
	grant: usize,
	asked: Option<Asked>,
}

impl smartcard::Service for GrantView<'_> {
	fn reader(&mut self) -> Result<ReaderInfo, Error> {
		let grant = &self.service.grants[self.grant];
		if !grant.operations.read {
			return Err(Error::Denied);
		}
		let reader = self.service.readers.iter().find(|reader| reader.key == grant.reader).ok_or(Error::Closed)?;
		Ok(ReaderInfo { id: reader_id(&reader.info), name: reader.description.name.clone(), alias: grant.alias.clone(), slots: reader.description.slots, exchange: reader.description.exchange, pinpad: reader.pinpad })
	}
	fn slots(&mut self) -> Result<Vec<SlotState>, Error> {
		let grant = &self.service.grants[self.grant];
		if !grant.operations.read {
			return Err(Error::Denied);
		}
		Ok(self.service.cards.slots(grant.reader).iter().map(slot_state).collect())
	}
	fn events(&mut self) -> Result<Vec<CardEvent>, Error> {
		Ok(Vec::new())
	}
	fn acquire(&mut self, slot: u8, wait_ms: u32, lease_ms: u32) -> Result<Transaction, Error> {
		self.asked = Some(Asked::Acquire(slot, wait_ms, lease_ms));
		Err(Error::Again)
	}
	fn exchange(&mut self, transaction: u64, apdu: Vec<u8>) -> Result<ExchangeResult, Error> {
		self.asked = Some(Asked::Exchange(transaction, apdu));
		Err(Error::Again)
	}
	fn verify_piv_pin(&mut self, transaction: u64, timeout_ms: u32) -> Result<PinOutcome, Error> {
		self.asked = Some(Asked::Verify(transaction, timeout_ms));
		Err(Error::Again)
	}
	fn authenticate_piv(&mut self, transaction: u64, challenge: Vec<u8>) -> Result<AuthenticateResult, Error> {
		self.asked = Some(Asked::Authenticate(transaction, challenge));
		Err(Error::Again)
	}
	fn cancel(&mut self, transaction: u64) -> Result<(), Error> {
		self.asked = Some(Asked::Finish(transaction));
		Err(Error::Again)
	}
	fn release(&mut self, transaction: u64) -> Result<(), Error> {
		self.asked = Some(Asked::Finish(transaction));
		Err(Error::Again)
	}
}

fn ticks(ms: u32) -> u64 {
	(ms as u64).div_ceil(10)
}

impl Service {
	// ------------------------------------------------------------------ providers

	// A publication: describe it, open its events, and open a session - whose answer, which comes only
	// once the provider has drained anything a previous session left, is what admits it.
	fn adopt(&mut self, catalogue: u64, info: ProviderInfo) {
		if self.readers.iter().any(|reader| same(&reader.info, &info)) {
			return;
		}
		if self.readers.len() >= cs::MAX_READERS {
			print(b"SmartcardService: a reader was refused: this service holds eight (resource exhausted)\n");
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) else {
			print(b"SmartcardService: a published reader could not be opened\n");
			return;
		};
		let mut client = smartcard_reader::Client::with_deadline(ChannelTransport { chan }, clock() + DESCRIBE_TICKS);
		let description = match client.describe() {
			Some(Ok(description)) => description,
			_ => {
				print(b"SmartcardService: a reader did not describe itself\n");
				close(chan);
				return;
			}
		};
		// A LARGER ADVERTISEMENT IS REFUSED, never truncated.
		if description.slots == 0 || description.slots > cs::MAX_SLOTS {
			print(b"SmartcardService: a reader advertising more than four slots was refused\n");
			close(chan);
			return;
		}
		let events = client.events().unwrap_or(0);
		if events == 0 {
			close(chan);
			return;
		}
		let pinpad = piv::pinpad_fits(description.pinpad.secure_verify, description.pinpad.ascii, description.pinpad.max_block, description.pinpad.min_digits, description.pinpad.max_digits);
		let key = self.next_reader;
		self.next_reader = self.next_reader.wrapping_add(1);
		let mut reader = ReaderConn { key, info, chan, events, description, pinpad, opening: None, sent: Vec::new(), next_corr: 1 };
		let corr = reader.next_corr;
		reader.next_corr += 1;
		match captured(|client| drop(client.open_session()), corr) {
			Some(bytes) if try_send(chan, &bytes, 0) => reader.opening = Some((corr, clock())),
			_ => {
				close(events);
				close(chan);
				return;
			}
		}
		self.readers.push(reader);
	}

	fn lose(&mut self, key: u32, why: &[u8]) {
		if let Some(at) = self.readers.iter().position(|reader| reader.key == key) {
			let reader = self.readers.remove(at);
			close(reader.events);
			close(reader.chan);
			print(b"SmartcardService: a reader is gone: ");
			print(why);
			print(b"\n");
		}
		let effects = self.cards.remove_reader(key);
		self.apply(effects);
	}

	// ------------------------------------------------------------------ effects

	fn apply(&mut self, effects: Vec<cs::Effect>) {
		for effect in effects {
			match effect {
				cs::Effect::Send { reader, request, slot, generation, what } => self.send(reader, request, slot, generation, what),
				cs::Effect::Acquired { call, result } => self.answer(call, result.map_err(refusal), |acquired, w| {
					let lease_ms = (acquired.lease_ticks * 10).min(u32::MAX as u64) as u32;
					Transaction { id: acquired.transaction, slot: acquired.slot, card_generation: acquired.generation, lease_ms }.write(w)
				}),
				cs::Effect::Exchanged { call, result } => self.answer(call, result.map_err(refusal), |exchanged, w| {
					let response = exchanged.sw.map(|(sw1, sw2)| ApduResponse { data: exchanged.data.clone(), sw1, sw2 });
					ExchangeResult { outcome: outcome(exchanged.outcome), response }.write(w)
				}),
				cs::Effect::Verified { call, result } => self.answer(call, result.map_err(refusal), |verified, w| {
					let (result, retries) = match verified {
						cs::Verified::Verified => (PinResult::Verified, None),
						cs::Verified::Incorrect(retries) => (PinResult::Incorrect, Some(*retries)),
						cs::Verified::Blocked => (PinResult::Blocked, None),
						cs::Verified::Cancelled => (PinResult::Cancelled, None),
						cs::Verified::TimedOut => (PinResult::TimedOut, None),
						cs::Verified::CardRemoved => (PinResult::CardRemoved, None),
						cs::Verified::ProviderError => (PinResult::ProviderError, None),
						cs::Verified::TrustedInputUnavailable => (PinResult::TrustedInputUnavailable, None),
					};
					PinOutcome { result, retries }.write(w)
				}),
				cs::Effect::Authenticated { call, result } => self.answer(call, result.map_err(refusal), |authenticated, w| AuthenticateResult { outcome: outcome(authenticated.outcome), signature: authenticated.signature.clone() }.write(w)),
				cs::Effect::Event { reader, kind, slot } => self.event(reader, kind, slot),
				cs::Effect::CloseGrant(id) => self.close_grant(id),
			}
		}
	}

	fn answer<T>(&self, call: cs::Call, result: Result<T, Error>, write: impl FnOnce(&T, &mut wire::VecWriter) -> Option<()>) {
		if let Some(grant) = self.grants.iter().find(|grant| grant.id == call.grant) {
			reply(grant.chan, call.corr, result, write);
		}
	}

	// A request to a provider, sent without waiting. NOT SENT IS ANSWERED AS A FAULT at once: the
	// provider never saw it, and the decisions go on from there.
	fn send(&mut self, key: u32, request: u64, slot: u8, generation: u64, what: cs::Request) {
		let Some(at) = self.readers.iter().position(|reader| reader.key == key) else { return };
		let reader = &mut self.readers[at];
		let corr = reader.next_corr;
		reader.next_corr = reader.next_corr.wrapping_add(1).max(1);
		let header = RequestHeader { request, slot, card_generation: generation };
		let bytes = captured(
			|client| match what {
				cs::Request::PowerOff => drop(client.power(&header, &false)),
				cs::Request::PowerOn => drop(client.power(&header, &true)),
				cs::Request::Protocol(protocol) => drop(client.select_protocol(&header, &if protocol == 1 { CardProtocol::T1 } else { CardProtocol::T0 })),
				cs::Request::Exchange(apdu) => drop(client.exchange(&header, &apdu)),
				cs::Request::Verify => drop(client.secure_verify(&SecureVerifyRequest { header: header.clone(), block: piv::PIN_BLOCK, min_digits: piv::PIN_MIN_DIGITS, max_digits: piv::PIN_MAX_DIGITS, timeout_seconds: PINPAD_SECONDS, apdu: piv::verify_template().to_vec() })),
				cs::Request::Abort => drop(client.abort(&header)),
			},
			corr,
		);
		let sent = bytes.is_some_and(|bytes| try_send(reader.chan, &bytes, 0));
		if sent {
			if reader.sent.len() >= 32 {
				reader.sent.remove(0);
			}
			reader.sent.push((corr, request, slot, generation));
			return;
		}
		let fault = cs::Answer { request, slot, generation, outcome: cs::ProviderOutcome::Fault, response: Vec::new(), atr: Vec::new(), offer: None };
		let effects = self.cards.answered(key, fault, clock());
		self.apply(effects);
	}

	// A presence or availability change, to every event stream of every grant to that reader.
	fn event(&mut self, key: u32, kind: cs::EventKind, slot: u8) {
		let Some(view) = self.cards.slots(key).into_iter().find(|view| view.slot == slot) else { return };
		self.sequence += 1;
		let event = CardEvent {
			sequence: self.sequence,
			kind: match kind {
				cs::EventKind::Inserted => EventKind::Inserted,
				cs::EventKind::Removed => EventKind::Removed,
				cs::EventKind::Unavailable => EventKind::Unavailable,
				cs::EventKind::Available => EventKind::Available,
			},
			slot: Some(slot_state(&view)),
		};
		for grant in self.grants.iter_mut().filter(|grant| grant.reader == key) {
			for stream in &mut grant.streams {
				let mut frame = [0u8; FRAME_BYTES];
				let mut handles = Handles::new();
				if let Some(len) = smartcard::events_frame(0, &event, &mut frame, &mut handles) {
					// A SLOW READER'S OVERFLOW CLOSES ITS OWN STREAM, and nothing else waits on it.
					let _ = stream.queue.push(frame[..len].to_vec());
				}
			}
		}
	}

	fn drain_streams(&mut self) {
		for grant in &mut self.grants {
			grant.streams.retain_mut(|stream| {
				if stream.queue.overflowed() {
					print(b"SmartcardService: an event stream fell sixteen behind and is closed\n");
					close(stream.chan);
					return false;
				}
				while let Some(frame) = stream.queue.front() {
					let mut numbered = frame.clone();
					numbered[..4].copy_from_slice(&stream.seq.to_le_bytes());
					match try_send_outcome(stream.chan, &numbered, 0) {
						SendOutcome::Delivered => {
							stream.seq = stream.seq.wrapping_add(1);
							stream.queue.pop();
						}
						SendOutcome::Stalled => break,
						SendOutcome::Failed => {
							close(stream.chan);
							return false;
						}
					}
				}
				true
			});
		}
	}

	fn close_grant(&mut self, id: u32) {
		let Some(at) = self.grants.iter().position(|grant| grant.id == id) else { return };
		let grant = self.grants.remove(at);
		for stream in grant.streams {
			close(stream.chan);
		}
		close(grant.chan);
		close(grant.owner);
	}

	// A grant's owner died or its connection closed: its transactions end, whatever copies of its
	// endpoint exist elsewhere.
	fn drop_grant(&mut self, id: u32) {
		let effects = self.cards.drop_grant(id, clock());
		self.close_grant(id);
		self.apply(effects);
	}

	// ------------------------------------------------------------------ what providers say

	fn on_reply(&mut self, at: usize, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		let key = self.readers[at].key;
		let chan = self.readers[at].chan;
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
			let Some(at) = self.readers.iter().position(|held| held.key == key) else { return Ok(()) };
			if self.readers[at].opening.is_some_and(|(opening, _)| opening == corr) {
				let decoded = (|| {
					if !reader.tag()? {
						return Some(None);
					}
					let count = reader.u16()? as usize;
					let mut reports = Vec::new();
					for _ in 0..count.min(cs::MAX_SLOTS as usize + 1) {
						reports.push(SlotReport::read(&mut reader)?);
					}
					Some(Some(reports))
				})();
				let Some(Some(reports)) = decoded else { return Err(b"it refused a new session") };
				self.readers[at].opening = None;
				let description = &self.readers[at].description;
				let spec = cs::ReaderSpec { slots: description.slots, short_apdu: description.exchange == ExchangeLevel::ShortApdu, pinpad: self.readers[at].pinpad, protocols: description.protocols };
				let reports: Vec<cs::SlotReport> = reports.iter().map(|report| cs::SlotReport { slot: report.slot, present: report.present, generation: report.card_generation, atr: report.atr.clone() }).collect();
				match self.cards.add_reader(key, spec, &reports, clock()) {
					Ok(effects) => self.apply(effects),
					Err(_) => return Err(b"it could not be admitted"),
				}
				continue;
			}
			let Some(place) = self.readers[at].sent.iter().position(|(sent, ..)| *sent == corr) else { continue };
			let (_, request, slot, generation) = self.readers[at].sent.remove(place);
			let decoded = (|| {
				if reader.tag()? { Some(Ok(ProviderReply::read(&mut reader)?)) } else { Some(Err(Error::read(&mut reader)?)) }
			})();
			let answer = match decoded {
				Some(Ok(reply)) => {
					// THE ECHO MUST BE WHAT WAS ASKED: another request's reply is not this one's.
					if reply.header.request != request || reply.header.slot != slot || reply.header.card_generation != generation || reply.response.len() > piv::MAX_RESPONSE || reply.atr.len() > cs::MAX_ATR {
						return Err(b"a reply did not echo its request");
					}
					let outcome = match reply.outcome {
						ProviderOutcome::Done => cs::ProviderOutcome::Done,
						ProviderOutcome::CardRemoved => cs::ProviderOutcome::CardRemoved,
						ProviderOutcome::Aborted => cs::ProviderOutcome::Aborted,
						ProviderOutcome::TimedOut => cs::ProviderOutcome::TimedOut,
						ProviderOutcome::Cancelled => cs::ProviderOutcome::Cancelled,
						ProviderOutcome::Fault => cs::ProviderOutcome::Fault,
					};
					// THE ATR IS READ HERE, by the one parser every provider uses, and the decisions get what it offers.
					let offer = smartcard_model::atr::parse(&reply.atr).ok().map(|atr| cs::Offer { protocols: atr.protocols, first: atr.first });
					cs::Answer { request, slot, generation, outcome, response: reply.response, atr: reply.atr, offer }
				}
				Some(Err(_)) => cs::Answer { request, slot, generation, outcome: cs::ProviderOutcome::Fault, response: Vec::new(), atr: Vec::new(), offer: None },
				None => return Err(b"a reply did not decode"),
			};
			let effects = self.cards.answered(key, answer, clock());
			self.apply(effects);
		}
	}

	fn on_events(&mut self, at: usize, buf: &mut [u8]) -> Result<(), &'static [u8]> {
		let key = self.readers[at].key;
		let stream = self.readers[at].events;
		loop {
			let (len, handles) = match try_recv_caps(stream, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return Ok(()),
				PolledCaps::Closed => return Err(b"its event stream closed"),
			};
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = Handles::new();
			let Some(event) = smartcard_reader::events_read(&buf[..len], &mut frame_handles) else { return Err(b"an event did not decode") };
			let now = clock();
			let effects = match event.kind {
				ReaderEventKind::Presence => match event.report {
					Some(report) if report.atr.len() <= cs::MAX_ATR => self.cards.presence(key, cs::SlotReport { slot: report.slot, present: report.present, generation: report.card_generation, atr: report.atr }, now),
					_ => return Err(b"a presence event carried no report"),
				},
				ReaderEventKind::Fault => self.cards.fault(key, event.slot),
				ReaderEventKind::Quiescent => self.cards.quiescent(key, event.slot, now),
			};
			self.apply(effects);
		}
	}

	// ------------------------------------------------------------------ clients

	fn client(&mut self, at: usize, request: &[u8], handles: &mut Handles, reply_buf: &mut [u8]) -> bool {
		let chan = self.grants[at].chan;
		let id = self.grants[at].id;
		if request.len() >= 2 && u16::from_le_bytes([request[0], request[1]]) == smartcard::OP_EVENTS {
			return self.subscribe(at, request, handles, reply_buf);
		}
		let mut view = GrantView { service: self, grant: at, asked: None };
		let mut reply_handles = Handles::new();
		let written = smartcard::dispatch(&mut view, request, handles, reply_buf, &mut reply_handles);
		let asked = view.asked.take();
		let Some(written) = written else { return false };
		let Some(asked) = asked else {
			if !send_caps_blocking(chan, &reply_buf[..written], reply_handles.as_slice()) {
				for &leftover in reply_handles.as_slice() {
					close(leftover);
				}
			}
			return true;
		};
		let corr = u32::from_le_bytes([request[2], request[3], request[4], request[5]]);
		let call = cs::Call { grant: id, corr };
		let now = clock();
		let effects = match asked {
			Asked::Acquire(slot, wait_ms, lease_ms) => self.cards.acquire(call, slot, ticks(wait_ms), ticks(lease_ms), now),
			Asked::Exchange(transaction, apdu) => self.cards.exchange(call, transaction, &apdu, now),
			Asked::Verify(transaction, timeout_ms) => self.cards.verify(call, transaction, ticks(timeout_ms), now),
			Asked::Authenticate(transaction, challenge) => self.cards.authenticate(call, transaction, &challenge, now),
			Asked::Finish(transaction) => match self.cards.finish(call, transaction, now) {
				Ok(effects) => {
					reply(chan, corr, Ok(()), |_, _| Some(()));
					effects
				}
				Err(refused) => {
					reply::<()>(chan, corr, Err(refusal(refused)), |_, _| Some(()));
					Vec::new()
				}
			},
		};
		self.apply(effects);
		true
	}

	// `events`: the reader's slots as an atomic snapshot, then ordered changes. Charged before it is
	// admitted - every frame encoded and a channel allocated deep enough for them and the live queue.
	fn subscribe(&mut self, at: usize, request: &[u8], handles: &mut Handles, reply_buf: &mut [u8]) -> bool {
		let chan = self.grants[at].chan;
		let mut view = GrantView { service: self, grant: at, asked: None };
		let Some((corr, _)) = smartcard::events_open(&mut view, request, handles) else { return false };
		let refuse = |error: Error, reply_buf: &mut [u8]| {
			if let Some(len) = smartcard::events_reply_err(corr, &error, reply_buf) {
				let _ = try_send(chan, &reply_buf[..len], 0);
			}
		};
		if !self.grants[at].operations.read {
			refuse(Error::Denied, reply_buf);
			return true;
		}
		if self.grants[at].streams.len() >= MAX_STREAMS {
			refuse(Error::Exhausted, reply_buf);
			return true;
		}
		let slots = self.cards.slots(self.grants[at].reader);
		let items = slots.iter().map(|view| CardEvent { sequence: self.sequence, kind: EventKind::Snapshot, slot: Some(slot_state(view)) }).chain(core::iter::once(CardEvent { sequence: self.sequence, kind: EventKind::SnapshotEnd, slot: None }));
		let mut frames: Vec<Vec<u8>> = Vec::new();
		for (seq, item) in items.enumerate() {
			let mut frame = [0u8; FRAME_BYTES];
			let mut frame_handles = Handles::new();
			let Some(len) = smartcard::events_frame(seq as u32, &item, &mut frame, &mut frame_handles) else {
				refuse(Error::Exhausted, reply_buf);
				return true;
			};
			frames.push(frame[..len].to_vec());
		}
		let Some((producer, consumer)) = channel_with_depth(frames.len() as u64 + cs::MAX_EVENTS as u64) else {
			refuse(Error::Exhausted, reply_buf);
			return true;
		};
		for frame in &frames {
			if !try_send(producer, frame, 0) {
				close(producer);
				close(consumer);
				refuse(Error::Exhausted, reply_buf);
				return true;
			}
		}
		match smartcard::events_reply_ok(corr, reply_buf) {
			Some(len) if send_caps_blocking(chan, &reply_buf[..len], &[consumer]) => {
				self.grants[at].streams.push(Stream { chan: producer, seq: frames.len() as u32, queue: cs::EventQueue::default() });
			}
			_ => {
				close(producer);
				close(consumer);
			}
		}
		true
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// The roles: a catalogue connection minted for `smartcard-reader` alone, and the minting root
	// PermissionManager reaches through the broker. Nothing else.
	let mut roles: [u64; BOOTSTRAP_ROLES.len()] = [0; BOOTSTRAP_ROLES.len()];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES, &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (catalogue, admin_root) = (roles[0], roles[1]);
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::SmartcardReader).unwrap_or(0) } else { 0 };
	let mut service = Service { cards: Cards::new(), readers: Vec::new(), grants: Vec::new(), admins: Vec::new(), next_reader: 1, next_grant: 1, sequence: 0 };
	send_blocking(bootstrap, b"SmartcardService: online", 0);

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
		for reader in &service.readers {
			waitset.push(reader.chan);
			waitset.push(reader.events);
		}
		for grant in &service.grants {
			waitset.push(grant.chan);
			waitset.push(grant.owner);
			waitset.extend(grant.streams.iter().map(|stream| stream.chan));
		}
		let now = clock();
		// A session that never opens is a reader passed over.
		if let Some(key) = service.readers.iter().find(|reader| reader.opening.is_some_and(|(_, since)| now.saturating_sub(since) >= SESSION_TICKS)).map(|reader| reader.key) {
			service.lose(key, b"its new session was never proven quiescent");
			continue;
		}
		let backlog = service.grants.iter().any(|grant| grant.streams.iter().any(|stream| !stream.queue.is_empty()));
		let deadlines = [
			service.cards.next_deadline(),
			backlog.then_some(now + 10),
			service.readers.iter().filter_map(|reader| reader.opening.map(|(_, since)| since + SESSION_TICKS)).min(),
		];
		let deadline = deadlines.into_iter().flatten().min().map_or(0, |deadline| deadline.max(now + 1));
		let ready = wait_any(&waitset, deadline);
		let effects = service.cards.tick(clock());
		service.apply(effects);
		if ready >= 0 {
			serve(&mut service, waitset[ready as usize], catalogue, subscription, &mut subscribed, admin_root, &mut buf, &mut reply_buf);
		}
		service.drain_streams();
	}
}

fn serve(service: &mut Service, handle: u64, catalogue: u64, subscription: u64, subscribed: &mut bool, admin_root: u64, buf: &mut [u8], reply_buf: &mut [u8]) {
	if let Some(at) = service.readers.iter().position(|reader| reader.chan == handle) {
		if let Err(why) = service.on_reply(at, buf) {
			let key = service.readers[at].key;
			service.lose(key, why);
		}
		return;
	}
	if let Some(at) = service.readers.iter().position(|reader| reader.events == handle) {
		if let Err(why) = service.on_events(at, buf) {
			let key = service.readers[at].key;
			service.lose(key, why);
		}
		return;
	}
	// A GRANT'S OWNER ENDED: its transactions end with it, whatever copies of its endpoint live on.
	if let Some(id) = service.grants.iter().find(|grant| grant.owner == handle).map(|grant| grant.id) {
		service.drop_grant(id);
		return;
	}
	if let Some(at) = service.grants.iter().position(|grant| grant.streams.iter().any(|stream| stream.chan == handle)) {
		if let PolledCaps::Closed = try_recv_caps(handle, buf) {
			service.grants[at].streams.retain(|stream| {
				if stream.chan == handle {
					close(stream.chan);
					false
				} else {
					true
				}
			});
		}
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
			if info.live {
				service.adopt(catalogue, info);
			} else if let Some(key) = service.readers.iter().find(|reader| same(&reader.info, &info)).map(|reader| reader.key) {
				service.lose(key, b"its publication was withdrawn");
			}
		}
		return;
	}
	let is_root = handle == admin_root;
	if !is_root && !service.admins.contains(&handle) {
		return;
	}
	let (len, mut handles) = match try_recv_caps(handle, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			if !is_root {
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
		// FROM THE ROOT OR FROM A CONNECTION IT MINTED: a resolver keeps the connection the broker minted
		// for it and mints its own from that one, as every root's connections allow - refusing it here
		// refused every grant the resolver was asked for.
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
	let written = smartcard_admin::dispatch(&mut AdminView { service }, &buf[..len], &mut handles, reply_buf, &mut reply_handles);
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
