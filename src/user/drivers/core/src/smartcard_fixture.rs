// smartcard_fixture - two in-guest smart-card readers and the PIV cards in them, published as
// `smartcard-reader` providers over the production wire, and a control endpoint for the probe that
// drives the smart-card gate.
//
// DEVELOPMENT-ONLY. It binds to a QEMU test function at a pinned address only the smart-card gate adds,
// and its control endpoint is a kind no scope minted for real hardware admits.
//
// WHAT IT PLAYS. Reader A has two slots and a secure-verification pinpad; reader B has one slot and no
// pinpad. Each card is `drivers::piv_card`. The provider side is the contract a USB CCID class module
// will serve: request identities echoed, presence and quiescence as events, a new session answered only
// once whatever the last one left is drained, and aborts that complete when the slot is quiet.
//
// WHAT THE PROBE CAN MAKE IT DO. Insert and remove cards (a new card generation each time), decide what
// the pinpad "saw" for the next verification - an outcome, never a PIN - hold the next exchange's reply
// or the next abort's completion, prove a slot quiescent, withdraw and republish a reader, and read back
// every operation it received, in order, with its own clock. That log is what lets the gate prove an
// ORDER: that no second client's command arrived before an abort and a reset had.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use drivers::common;
use drivers::piv_card::{self, Card, Pinpad};
use proto::system::{CardProtocol, Error, ExchangeLevel, FixtureOperation, FixturePin, PinpadCapabilities, ProviderOutcome, ProviderReply, ReaderDescription, ReaderEvent, ReaderEventKind, RequestHeader, SecureVerifyRequest, SlotReport, smartcard_fixture, smartcard_reader};
use rt::*;

const NAMES: [&[u8]; 2] = [b"org.libersystem.smartcard-fixture.a", b"org.libersystem.smartcard-fixture.b"];
const CONTROL_NAME: &[u8] = b"org.libersystem.smartcard-fixture.control";
const CONTROL_TOKEN: u16 = 2;
const MAX_LOG: usize = 256;
const STREAM_DEPTH: u64 = 64;
// "Never", for a held reply.
const NEVER: u64 = u64::MAX;

struct SlotSim {
	present: bool,
	generation: u64,
	powered: bool,
	card: Card,
}

// A reply the fixture was told to hold, or an operation a departed session left behind.
struct Held {
	release: u64,
	slot: u8,
	bytes: Vec<u8>,
}

struct ReaderSim {
	token: u16,
	live: bool,
	// The consumer connection replies go to, and the producer end of its event stream.
	chan: u64,
	events: u64,
	seq: u32,
	slots: Vec<SlotSim>,
	pinpad: bool,
	delay_next: Option<u64>,
	hold_abort: Option<u64>,
	held: Vec<Held>,
	// Work a departed session left in flight: the slot stays busy until each completes on its own.
	draining: Vec<(u8, u64)>,
	// A new session's reply, held until the drain is complete.
	session: Option<(u64, Vec<u8>)>,
}

struct Fixture {
	readers: [ReaderSim; 2],
	pin: (FixturePin, u64),
	log: Vec<FixtureOperation>,
	next_token: u16,
	next_generation: u64,
}

fn ticks(ms: u32) -> u64 {
	if ms == 0 { NEVER } else { (ms as u64).div_ceil(10) }
}

impl Fixture {
	fn record(&mut self, reader: u8, slot: u8, what: &str, header: &[u8], pin_bytes: bool) {
		if self.log.len() >= MAX_LOG {
			self.log.remove(0);
		}
		self.log.push(FixtureOperation { at: clock(), reader, slot, what: what.into(), header: header.iter().take(4).copied().collect(), pin_bytes });
	}

	fn reader_of(&self, token: u16) -> Option<u8> {
		self.readers.iter().position(|reader| reader.live && reader.token == token).map(|at| at as u8)
	}

	fn report(&self, reader: u8, slot: u8) -> SlotReport {
		let held = &self.readers[reader as usize].slots[slot as usize];
		SlotReport { slot, present: held.present, card_generation: held.generation, atr: if held.present && held.powered { piv_card::atr() } else { Vec::new() }, powered: held.powered }
	}

	fn event(&mut self, reader: u8, event: &ReaderEvent) {
		let sim = &mut self.readers[reader as usize];
		if sim.events == 0 {
			return;
		}
		let mut frame = [0u8; 256];
		let mut handles = wire::Handles::new();
		if let Some(len) = smartcard_reader::events_frame(sim.seq, event, &mut frame, &mut handles)
			&& try_send(sim.events, &frame[..len], 0)
		{
			sim.seq = sim.seq.wrapping_add(1);
		}
	}

	fn presence(&mut self, reader: u8, slot: u8) {
		let report = self.report(reader, slot);
		self.event(reader, &ReaderEvent { kind: ReaderEventKind::Presence, slot, report: Some(report) });
	}

	// What falls due: held replies are sent, drains complete, and a waiting session is answered once
	// nothing is left draining.
	fn release_due(&mut self) {
		let now = clock();
		for reader in 0..2u8 {
			let sim = &mut self.readers[reader as usize];
			let chan = sim.chan;
			let (due, keep): (Vec<Held>, Vec<Held>) = core::mem::take(&mut sim.held).into_iter().partition(|held| held.release <= now);
			sim.held = keep;
			for held in due {
				if chan != 0 {
					let _ = try_send(chan, &held.bytes, 0);
				}
			}
			let drained: Vec<u8> = sim.draining.iter().filter(|(_, until)| *until <= now).map(|(slot, _)| *slot).collect();
			sim.draining.retain(|(_, until)| *until > now);
			for slot in drained {
				self.readers[reader as usize].slots[slot as usize].powered = false;
				self.record(reader, slot, "drained", &[], false);
			}
			let sim = &mut self.readers[reader as usize];
			if sim.draining.is_empty()
				&& let Some((chan, bytes)) = sim.session.take()
			{
				for slot in &mut sim.slots {
					slot.powered = false;
					slot.card.reset();
				}
				let _ = try_send(chan, &bytes, 0);
				self.record(reader, 0, "session", &[], false);
			}
		}
	}

	fn next_due(&self) -> u64 {
		self.readers.iter().flat_map(|sim| sim.held.iter().map(|held| held.release).chain(sim.draining.iter().map(|(_, until)| *until))).filter(|&at| at != NEVER).min().unwrap_or(0)
	}

	// The consumer went: whatever it left in flight drains on its own time, and the slots it powered go
	// off when that is done.
	fn departed(&mut self, reader: u8) {
		let sim = &mut self.readers[reader as usize];
		for held in core::mem::take(&mut sim.held) {
			let until = if held.release == NEVER { clock() } else { held.release };
			sim.draining.push((held.slot, until));
		}
		sim.chan = 0;
		if sim.events != 0 {
			close(sim.events);
			sim.events = 0;
		}
		if sim.draining.is_empty() {
			for slot in &mut sim.slots {
				slot.powered = false;
			}
		}
	}
}

// ------------------------------------------------------------------ the provider wire

struct ReaderView<'a> {
	fixture: &'a mut Fixture,
	reader: u8,
	// A reply to hold rather than send, and until when.
	hold: Option<(u64, u8)>,
	// The session reply is held until the drain is complete.
	defer_session: bool,
}

impl ReaderView<'_> {
	fn sim(&mut self) -> &mut ReaderSim {
		&mut self.fixture.readers[self.reader as usize]
	}

	// The slot a request names, if it holds the card the request was prepared against.
	fn slot(&mut self, header: &RequestHeader) -> Result<(), ProviderOutcome> {
		let Some(slot) = self.sim().slots.get(header.slot as usize) else { return Err(ProviderOutcome::Fault) };
		if !slot.present || slot.generation != header.card_generation {
			return Err(ProviderOutcome::CardRemoved);
		}
		Ok(())
	}
}

fn reply(header: &RequestHeader, outcome: ProviderOutcome, response: Vec<u8>, atr: Vec<u8>) -> ProviderReply {
	ProviderReply { header: header.clone(), outcome, response, atr }
}

impl smartcard_reader::Service for ReaderView<'_> {
	fn describe(&mut self) -> Result<ReaderDescription, Error> {
		let sim = &self.fixture.readers[self.reader as usize];
		let pinpad = if sim.pinpad { PinpadCapabilities { secure_verify: true, ascii: true, max_block: 8, min_digits: 4, max_digits: 8 } } else { PinpadCapabilities { secure_verify: false, ascii: false, max_block: 0, min_digits: 0, max_digits: 0 } };
		Ok(ReaderDescription { name: alloc::string::String::from_utf8_lossy(NAMES[self.reader as usize]).into_owned(), slots: sim.slots.len() as u8, exchange: ExchangeLevel::ShortApdu, protocols: 0b11, pinpad })
	}

	fn open_session(&mut self) -> Result<Vec<SlotReport>, Error> {
		// A NEW SESSION WAITS FOR THE OLD ONE'S WORK TO DRAIN, and every slot goes off first.
		if !self.sim().draining.is_empty() {
			self.defer_session = true;
		} else {
			let reader = self.reader;
			for slot in &mut self.sim().slots {
				slot.powered = false;
				slot.card.reset();
			}
			self.fixture.record(reader, 0, "session", &[], false);
		}
		let reader = self.reader;
		Ok((0..self.fixture.readers[reader as usize].slots.len() as u8)
			.map(|slot| {
				let mut report = self.fixture.report(reader, slot);
				report.powered = false;
				report.atr.clear();
				report
			})
			.collect())
	}

	fn events(&mut self) -> Vec<ReaderEvent> {
		Vec::new()
	}

	fn power(&mut self, header: RequestHeader, on: bool) -> Result<ProviderReply, Error> {
		let reader = self.reader;
		self.fixture.record(reader, header.slot, if on { "power-on" } else { "power-off" }, &[], false);
		if let Err(outcome) = self.slot(&header) {
			return Ok(reply(&header, outcome, Vec::new(), Vec::new()));
		}
		let slot = &mut self.sim().slots[header.slot as usize];
		slot.powered = on;
		slot.card.reset();
		Ok(reply(&header, ProviderOutcome::Done, Vec::new(), if on { piv_card::atr() } else { Vec::new() }))
	}

	fn select_protocol(&mut self, header: RequestHeader, _protocol: CardProtocol) -> Result<ProviderReply, Error> {
		let reader = self.reader;
		self.fixture.record(reader, header.slot, "protocol", &[], false);
		if let Err(outcome) = self.slot(&header) {
			return Ok(reply(&header, outcome, Vec::new(), Vec::new()));
		}
		Ok(reply(&header, ProviderOutcome::Done, Vec::new(), Vec::new()))
	}

	fn exchange(&mut self, header: RequestHeader, apdu: Vec<u8>) -> Result<ProviderReply, Error> {
		let reader = self.reader;
		self.fixture.record(reader, header.slot, "exchange", &apdu, false);
		if let Err(outcome) = self.slot(&header) {
			return Ok(reply(&header, outcome, Vec::new(), Vec::new()));
		}
		let slot = &mut self.sim().slots[header.slot as usize];
		if !slot.powered {
			return Ok(reply(&header, ProviderOutcome::Fault, Vec::new(), Vec::new()));
		}
		let response = slot.card.apdu(&apdu);
		if let Some(delay) = self.sim().delay_next.take() {
			self.hold = Some((if delay == NEVER { NEVER } else { clock() + delay }, header.slot));
		}
		Ok(reply(&header, ProviderOutcome::Done, response, Vec::new()))
	}

	fn secure_verify(&mut self, request: SecureVerifyRequest) -> Result<ProviderReply, Error> {
		let reader = self.reader;
		let header = request.header.clone();
		// THE TEMPLATE MUST CARRY NO PIN: eight placeholder bytes after VERIFY's header, and the log says
		// whether any was anything else.
		let pin_bytes = request.apdu.len() != 13 || request.apdu[5..].iter().any(|&byte| byte != 0xff);
		self.fixture.record(reader, header.slot, "verify", &request.apdu, pin_bytes);
		if let Err(outcome) = self.slot(&header) {
			return Ok(reply(&header, outcome, Vec::new(), Vec::new()));
		}
		if !self.sim().pinpad {
			return Ok(reply(&header, ProviderOutcome::Fault, Vec::new(), Vec::new()));
		}
		let (outcome, delay) = self.fixture.pin;
		let slot = &mut self.fixture.readers[reader as usize].slots[header.slot as usize];
		let answered = match outcome {
			FixturePin::Verified => reply(&header, ProviderOutcome::Done, slot.card.verify(Pinpad::Verified), Vec::new()),
			FixturePin::Incorrect => reply(&header, ProviderOutcome::Done, slot.card.verify(Pinpad::Incorrect), Vec::new()),
			FixturePin::Blocked => reply(&header, ProviderOutcome::Done, slot.card.verify(Pinpad::Blocked), Vec::new()),
			FixturePin::Cancelled => reply(&header, ProviderOutcome::Cancelled, alloc::vec![0x64, 0x01], Vec::new()),
			FixturePin::TimedOut => reply(&header, ProviderOutcome::TimedOut, alloc::vec![0x64, 0x00], Vec::new()),
		};
		if delay != 0 {
			self.hold = Some((clock() + delay, header.slot));
		}
		Ok(answered)
	}

	fn abort(&mut self, header: RequestHeader) -> Result<ProviderReply, Error> {
		let reader = self.reader;
		self.fixture.record(reader, header.slot, "abort", &[], false);
		// A REPLY THE SLOT WAS HOLDING GOES OUT NOW, LATE, ahead of the abort's completion - the race a
		// real transport has between an operation finishing and its abort, which the service must lose
		// gracefully: the late reply names a request that is no longer current, and is dropped.
		let chan = self.sim().chan;
		let (late, keep): (Vec<Held>, Vec<Held>) = core::mem::take(&mut self.sim().held).into_iter().partition(|held| held.slot == header.slot);
		self.sim().held = keep;
		for held in late {
			if chan != 0 {
				let _ = try_send(chan, &held.bytes, 0);
			}
			self.fixture.record(reader, header.slot, "late", &[], false);
		}
		if let Some(delay) = self.sim().hold_abort.take() {
			self.hold = Some((if delay == NEVER { NEVER } else { clock() + delay }, header.slot));
		}
		Ok(reply(&header, ProviderOutcome::Done, Vec::new(), Vec::new()))
	}
}

// ------------------------------------------------------------------ the control endpoint

struct ControlView<'a> {
	fixture: &'a mut Fixture,
	serving: &'a mut common::Serving,
	bootstrap: u64,
	bind: &'a common::Bind,
}

impl ControlView<'_> {
	fn slot_ok(&self, reader: u8, slot: u8) -> bool {
		(reader as usize) < self.fixture.readers.len() && (slot as usize) < self.fixture.readers[reader as usize].slots.len()
	}
}

impl smartcard_fixture::Service for ControlView<'_> {
	fn insert(&mut self, reader: u8, slot: u8) -> Result<(), Error> {
		if !self.slot_ok(reader, slot) {
			return Err(Error::NotFound);
		}
		self.fixture.next_generation += 1;
		let generation = self.fixture.next_generation;
		let held = &mut self.fixture.readers[reader as usize].slots[slot as usize];
		held.present = true;
		held.generation = generation;
		held.powered = false;
		held.card = Card::new();
		self.fixture.record(reader, slot, "inserted", &[], false);
		self.fixture.presence(reader, slot);
		Ok(())
	}

	fn remove(&mut self, reader: u8, slot: u8) -> Result<(), Error> {
		if !self.slot_ok(reader, slot) {
			return Err(Error::NotFound);
		}
		let held = &mut self.fixture.readers[reader as usize].slots[slot as usize];
		held.present = false;
		held.powered = false;
		self.fixture.record(reader, slot, "removed", &[], false);
		self.fixture.presence(reader, slot);
		Ok(())
	}

	fn pin(&mut self, outcome: FixturePin, delay_ms: u32) -> Result<(), Error> {
		self.fixture.pin = (outcome, if delay_ms == 0 { 0 } else { ticks(delay_ms) });
		Ok(())
	}

	fn delay(&mut self, reader: u8, delay_ms: u32) -> Result<(), Error> {
		let sim = self.fixture.readers.get_mut(reader as usize).ok_or(Error::NotFound)?;
		sim.delay_next = Some(ticks(delay_ms));
		Ok(())
	}

	fn hold_abort(&mut self, reader: u8, delay_ms: u32) -> Result<(), Error> {
		let sim = self.fixture.readers.get_mut(reader as usize).ok_or(Error::NotFound)?;
		sim.hold_abort = Some(ticks(delay_ms));
		Ok(())
	}

	fn recover(&mut self, reader: u8, slot: u8) -> Result<(), Error> {
		if !self.slot_ok(reader, slot) {
			return Err(Error::NotFound);
		}
		let sim = &mut self.fixture.readers[reader as usize];
		sim.held.retain(|held| held.slot != slot);
		sim.slots[slot as usize].powered = false;
		self.fixture.record(reader, slot, "quiescent", &[], false);
		self.fixture.event(reader, &ReaderEvent { kind: ReaderEventKind::Quiescent, slot, report: None });
		Ok(())
	}

	fn withdraw(&mut self, reader: u8) -> Result<(), Error> {
		let sim = self.fixture.readers.get_mut(reader as usize).ok_or(Error::NotFound)?;
		if !sim.live {
			return Err(Error::NotFound);
		}
		sim.live = false;
		let token = sim.token;
		self.fixture.departed(reader);
		if !common::withdraw(self.bootstrap, self.bind, token) {
			return Err(Error::Io);
		}
		self.fixture.record(reader, 0, "withdrawn", &[], false);
		Ok(())
	}

	fn republish(&mut self, reader: u8) -> Result<(), Error> {
		let token = self.fixture.next_token;
		let sim = self.fixture.readers.get_mut(reader as usize).ok_or(Error::NotFound)?;
		if sim.live {
			return Err(Error::Invalid);
		}
		let Some((near, far)) = channel() else { return Err(Error::Exhausted) };
		if !self.serving.publish(token, near) {
			close(near);
			close(far);
			return Err(Error::Exhausted);
		}
		if !common::offer_named(self.bootstrap, self.bind, driver_protocol::provider::SMARTCARD_READER, token, NAMES[reader as usize], far) {
			return Err(Error::Io);
		}
		self.fixture.next_token = token + 1;
		let sim = &mut self.fixture.readers[reader as usize];
		sim.token = token;
		sim.live = true;
		self.fixture.record(reader, 0, "republished", &[], false);
		Ok(())
	}

	fn operations(&mut self) -> Result<Vec<FixtureOperation>, Error> {
		Ok(self.fixture.log.clone())
	}

	fn now(&mut self) -> Result<u64, Error> {
		Ok(clock())
	}
}

// One request on one consumer connection. False when the connection is over.
fn serve(fixture: &mut Fixture, serving: &mut common::Serving, bootstrap: u64, bind: &common::Bind, token: u16, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	if len < 2 {
		return true;
	}
	// ROOM FOR THE WHOLE OPERATION LOG, which is the largest answer this program gives: `MAX_LOG` records
	// of about forty bytes each. Four kilobytes held a hundred, and a longer log answered `again`.
	let mut reply_buf = alloc::vec![0u8; 16 * 1024];
	let mut reply_handles = wire::Handles::new();
	if token == CONTROL_TOKEN {
		let mut view = ControlView { fixture: &mut *fixture, serving: &mut *serving, bootstrap, bind };
		if let Some(written) = smartcard_fixture::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) {
			send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
		}
		return true;
	}
	let Some(reader) = fixture.reader_of(token) else { return true };
	fixture.readers[reader as usize].chan = channel;
	let op = u16::from_le_bytes([buf[0], buf[1]]);
	if op == smartcard_reader::OP_EVENTS {
		let mut view = ReaderView { fixture: &mut *fixture, reader, hold: None, defer_session: false };
		let Some((corr, _)) = smartcard_reader::events_open(&mut view, &buf[..len], &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		let sim = &mut fixture.readers[reader as usize];
		if sim.events != 0 {
			close(sim.events);
		}
		sim.events = producer;
		sim.seq = 0;
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	let mut view = ReaderView { fixture: &mut *fixture, reader, hold: None, defer_session: false };
	let written = smartcard_reader::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles);
	let (hold, defer_session) = (view.hold, view.defer_session);
	let Some(written) = written else { return true };
	let sim = &mut fixture.readers[reader as usize];
	if defer_session {
		sim.session = Some((channel, reply_buf[..written].to_vec()));
	} else if let Some((release, slot)) = hold {
		sim.held.push(Held { release, slot, bytes: reply_buf[..written].to_vec() });
	} else {
		send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
	}
	true
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = common::handshake(bootstrap);
	if smartcard_model::atr::parse(&piv_card::atr()).is_err() {
		print(b"driver.smartcard-fixture: the fixture ATR does not parse\n");
		exit();
	}
	let (Some((a, a_far)), Some((b, b_far)), Some((control, control_far))) = (channel(), channel(), channel()) else { exit() };
	let timer: u64 = match timer_create() {
		t if t > 0 => t as u64,
		_ => exit(),
	};
	common::online_named(
		bootstrap,
		&bind,
		b"driver.smartcard-fixture: online (two readers and the PIV cards in them, for the smart-card gate)",
		&[
			(driver_protocol::provider::SMARTCARD_READER, a_far, NAMES[0]),
			(driver_protocol::provider::SMARTCARD_READER, b_far, NAMES[1]),
			(driver_protocol::provider::FIXTURE_CONTROL, control_far, CONTROL_NAME),
		],
	);
	let mut serving = common::Serving::from_offers(&[(0, a), (1, b), (CONTROL_TOKEN, control)]);
	let slot = |present: bool, generation: u64| SlotSim { present, generation, powered: false, card: Card::new() };
	let reader = |token: u16, slots: Vec<SlotSim>, pinpad: bool| ReaderSim { token, live: true, chan: 0, events: 0, seq: 0, slots, pinpad, delay_next: None, hold_abort: None, held: Vec::new(), draining: Vec::new(), session: None };
	let mut fixture = Fixture { readers: [reader(0, alloc::vec![slot(true, 1), slot(false, 0)], true), reader(1, alloc::vec![slot(true, 2)], false)], pin: (FixturePin::Verified, 0), log: Vec::new(), next_token: CONTROL_TOKEN + 1, next_generation: 2 };
	let mut buf = alloc::vec![0u8; 4096];
	loop {
		let due = fixture.next_due();
		// ARMED EVERY PASS, and far off when nothing is due. A timer stays expired until it is armed again,
		// so leaving it alone once the last deadline passed made every later wait return at once, and the
		// fixture spun a processor until something was due - long enough to starve the probes it serves.
		timer_set(timer, if due != 0 { due } else { u64::MAX });
		match common::wait_providers_or_answer(bootstrap, &bind, &mut serving, &[timer]) {
			None => {
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, 0, true);
				}
				exit();
			}
			Some(common::ProviderReady::Connected(_)) => {}
			Some(common::ProviderReady::Device(_)) => fixture.release_due(),
			Some(common::ProviderReady::Consumer(index)) => {
				let token = serving.token_at(index);
				let chan = serving.at(index);
				if !serve(&mut fixture, &mut serving, bootstrap, &bind, token, chan, &mut buf) {
					let token = serving.close_at(index);
					if let Some(reader) = fixture.readers.iter().position(|sim| sim.token == token) {
						fixture.departed(reader as u8);
					}
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
				}
			}
		}
		fixture.release_due();
	}
}
