// cardcheck - the in-guest scenario driver for the smart-card gate. DEVELOPMENT-ONLY.
//
// It holds a grant on fixture reader A with every operation, the smart-card fixture's control endpoint
// and the supervisor's admin channel, which the restart phase makes its stop and start through. It drives SmartcardService against the fixture's readers and prints one verdict line per
// phase for the gate to read. What it proves is what a client can observe, and - through the fixture's
// operation log - what reached a card and in what order. The service's handle baseline is not this
// probe's to read: the gate reads it from the system graph, which holds the service's process.
//
//   cardcheck insert      wait for an insertion, read the exact ATR, acquire, exchange the allowlist
//   cardcheck pin         verify on the pinpad with no PIN anywhere, then authenticate; print what the
//                         gate verifies with OpenSSL
//   cardcheck refuse      a PIN-bearing VERIFY, another application, chained and proprietary commands and
//                         a raw GENERAL AUTHENTICATE fail before the reader sees them
//   cardcheck isolation   events and slots are reader A's alone
//   cardcheck queue       a queued second client gets the slot only after a full reset, and inherits no
//                         verification
//   cardcheck inherit     a transferred endpoint does not keep its dead owner's transaction
//   cardcheck expiry      a lease ending mid-exchange times it out, and the late reply is dropped
//   cardcheck removal     removal mid-exchange is terminal; a rapid reinsertion waits for the drain
//   cardcheck stuck       an abort never proven makes the slot unavailable until the provider recovers
//   cardcheck overflow    an event stream sixteen behind is closed; a new one is a fresh snapshot
//   cardcheck prime-slow  make the next exchange on reader A take four seconds
//   cardcheck inflight    wait until that exchange is at the reader
//   cardcheck restart     stop and start the service through the supervisor with that exchange in
//                         flight, and wait until the fixture answers the replacement's session
//   cardcheck session     after a restart, the new session opened only once the old work drained
//   cardcheck republish   a withdrawn reader's grant fails; republication is another reader

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{CardEvent, CardOutcome, Error, EventKind, FixtureOperation, LaunchContext, PinResult, Presence, smartcard, smartcard_fixture};
use rt::*;
use wire::Sink;

const TICKS: u64 = 100;
const A: u8 = 0;
const B: u8 = 1;
// The first challenge the fixture card knows: SHA-256 of "liber smartcard fixture challenge 1".
const CHALLENGE: [u8; 32] = [
	0xfa,
	0x17,
	0xde,
	0xed,
	0x9d,
	0x48,
	0x1c,
	0x4c,
	0x20,
	0xa1,
	0x14,
	0x94,
	0x1f,
	0x45,
	0x3b,
	0xa7,
	0xf5,
	0xec,
	0x2d,
	0x92,
	0xb0,
	0xd9,
	0xbc,
	0x4a,
	0x6c,
	0xd1,
	0xc2,
	0xa7,
	0x85,
	0x3d,
	0xbb,
	0x1b,
];
const SELECT: [u8; 17] = [0x00, 0xa4, 0x04, 0x00, 0x0b, 0xa0, 0x00, 0x00, 0x03, 0x08, 0x00, 0x00, 0x10, 0x00, 0x01, 0x00, 0x00];
const GET_DISCOVERY: [u8; 9] = [0x00, 0xcb, 0x3f, 0xff, 0x03, 0x5c, 0x01, 0x7e, 0x00];
const GET_CERTIFICATE: [u8; 11] = [0x00, 0xcb, 0x3f, 0xff, 0x05, 0x5c, 0x03, 0x5f, 0xc1, 0x05, 0x00];

fn say(line: &[u8]) {
	print(b"cardcheck: ");
	print(line);
	print(b"\n");
}

fn fail(line: &[u8]) -> ! {
	print(b"cardcheck: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

fn hex(out: &mut Vec<u8>, bytes: &[u8]) {
	for byte in bytes {
		out.extend_from_slice(&[b"0123456789abcdef"[(byte >> 4) as usize], b"0123456789abcdef"[(byte & 15) as usize]]);
	}
}

fn number(out: &mut Vec<u8>, value: u64) {
	let mut digits = [0u8; 20];
	let mut at = digits.len();
	let mut n = value;
	loop {
		at -= 1;
		digits[at] = b'0' + (n % 10) as u8;
		n /= 10;
		if n == 0 {
			break;
		}
	}
	out.extend_from_slice(&digits[at..]);
}

struct Probe {
	card: u64,
	fixture: u64,
}

impl Probe {
	fn card(&self) -> smartcard::Client<ChannelTransport> {
		smartcard::Client::with_deadline(ChannelTransport { chan: self.card }, clock() + 40 * TICKS)
	}
	fn fixture(&self) -> smartcard_fixture::Client<ChannelTransport> {
		smartcard_fixture::Client::new(ChannelTransport { chan: self.fixture })
	}
	fn now(&self) -> u64 {
		match self.fixture().now() {
			Some(Ok(now)) => now,
			_ => fail(b"the fixture's clock could not be read"),
		}
	}
	fn operations(&self, since: u64) -> Vec<FixtureOperation> {
		match self.fixture().operations() {
			Some(Ok(log)) => log.into_iter().filter(|operation| operation.at >= since).collect(),
			_ => fail(b"the fixture's operation log could not be read"),
		}
	}
	fn acquire(&self, slot: u8, wait_ms: u32, lease_ms: u32) -> u64 {
		match self.card().acquire(&slot, &wait_ms, &lease_ms) {
			Some(Ok(transaction)) => transaction.id,
			_ => fail(b"a transaction could not be acquired"),
		}
	}
	fn exchange(&self, transaction: u64, apdu: &[u8]) -> Result<proto::system::ExchangeResult, Error> {
		match self.card().exchange(&transaction, &apdu.to_vec()) {
			Some(result) => result,
			None => fail(b"an exchange did not complete"),
		}
	}
	fn release(&self, transaction: u64) {
		let _ = self.card().release(&transaction);
	}
	fn fixture_ok(&self, done: Option<Result<(), Error>>, what: &[u8]) {
		if !matches!(done, Some(Ok(()))) {
			fail(what);
		}
	}
	fn events(&self) -> u64 {
		match self.card().events() {
			Some(Ok(stream)) => stream,
			_ => fail(b"an event subscription was refused"),
		}
	}
}

enum Read {
	Event(CardEvent),
	Closed,
	Quiet,
}

fn read(stream: u64, deadline: u64) -> Read {
	let mut buf = [0u8; 1024];
	loop {
		match try_recv_caps(stream, &mut buf) {
			PolledCaps::Message { len, mut handles } => {
				return match smartcard::events_read(&buf[..len], &mut handles) {
					Some(event) => Read::Event(event),
					None => fail(b"an event did not decode"),
				};
			}
			PolledCaps::Closed => return Read::Closed,
			PolledCaps::Empty => {
				if clock() >= deadline {
					return Read::Quiet;
				}
				let _ = wait_any(&[stream], deadline);
			}
		}
	}
}

fn snapshot(stream: u64) -> Vec<CardEvent> {
	let mut held = Vec::new();
	loop {
		match read(stream, clock() + 5 * TICKS) {
			Read::Event(event) if event.kind == EventKind::SnapshotEnd => return held,
			Read::Event(event) => held.push(event),
			_ => fail(b"an event stream ended before its snapshot did"),
		}
	}
}

fn fixture_atr() -> Vec<u8> {
	let mut bytes = alloc::vec![0x3b, 0x88, 0x80, 0x01];
	bytes.extend_from_slice(b"LIBERPIV");
	let tck = bytes[1..].iter().fold(0u8, |sum, byte| sum ^ byte);
	bytes.push(tck);
	bytes
}

// ------------------------------------------------------------------ the phases

fn insert(probe: &Probe) {
	let stream = probe.events();
	snapshot(stream);
	probe.fixture_ok(probe.fixture().insert(&A, &1), b"insert: the fixture could not insert a card");
	loop {
		match read(stream, clock() + 5 * TICKS) {
			Read::Event(event) if event.kind == EventKind::Inserted && event.slot.as_ref().is_some_and(|slot| slot.slot == 1) => break,
			Read::Event(_) => {}
			_ => fail(b"insert: the insertion was never announced"),
		}
	}
	close(stream);
	// The reset that follows an insertion powers the card and reads its ATR.
	let deadline = clock() + 5 * TICKS;
	let atr = loop {
		if let Some(Ok(slots)) = probe.card().slots()
			&& let Some(slot) = slots.iter().find(|slot| slot.slot == 1 && slot.presence == Presence::Present && !slot.atr.is_empty())
		{
			break slot.atr.clone();
		}
		if clock() >= deadline {
			fail(b"insert: the inserted card's ATR never appeared");
		}
		sleep_until(clock() + TICKS / 10);
	};
	if atr != fixture_atr() {
		fail(b"insert: the ATR is not the card's");
	}
	let transaction = probe.acquire(1, 5000, 0);
	for (apdu, what) in [(&SELECT[..], &b"SELECT"[..]), (&GET_DISCOVERY[..], &b"the Discovery Object"[..]), (&GET_CERTIFICATE[..], &b"the certificate"[..])] {
		let Ok(result) = probe.exchange(transaction, apdu) else { fail(what) };
		let Some(response) = result.response.filter(|response| result.outcome == CardOutcome::Done && response.sw1 == 0x90 && response.sw2 == 0x00) else { fail(what) };
		if apdu == GET_CERTIFICATE {
			// 53 82 LL LL 70 82 LL LL certificate ... - past 256 bytes, so the service continued it.
			if response.data.len() <= 256 || response.data[0] != 0x53 || response.data[4] != 0x70 {
				fail(b"insert: the certificate object is not whole");
			}
			let len = ((response.data[6] as usize) << 8) | response.data[7] as usize;
			let mut line = b"certificate ".to_vec();
			hex(&mut line, &response.data[8..8 + len]);
			say(&line);
		}
	}
	probe.release(transaction);
	let mut line = b"PASS insert: an insertion was announced, the ATR is exactly ".to_vec();
	hex(&mut line, &atr);
	line.extend_from_slice(b", and the allowlisted SELECT and GET DATA answered, the certificate continued past one response");
	say(&line);
}

fn pin(probe: &Probe) {
	let since = probe.now();
	let transaction = probe.acquire(0, 5000, 0);
	match probe.card().verify_piv_pin(&transaction, &10000) {
		Some(Ok(outcome)) if outcome.result == PinResult::Verified => {}
		_ => fail(b"pin: the pinpad verification did not succeed"),
	}
	let signature = match probe.card().authenticate_piv(&transaction, &CHALLENGE.to_vec()) {
		Some(Ok(result)) if result.outcome == CardOutcome::Done => match result.signature {
			Some(signature) => signature,
			None => fail(b"pin: an authentication gave no signature"),
		},
		_ => fail(b"pin: authentication did not complete"),
	};
	probe.release(transaction);
	let verifies: Vec<FixtureOperation> = probe.operations(since).into_iter().filter(|operation| operation.what == "verify").collect();
	if verifies.is_empty() || verifies.iter().any(|operation| operation.pin_bytes) {
		fail(b"pin: the verification the reader received carried something other than placeholders");
	}
	let mut line = b"challenge ".to_vec();
	hex(&mut line, &CHALLENGE);
	say(&line);
	let mut line = b"signature ".to_vec();
	hex(&mut line, &signature);
	say(&line);
	say(b"PASS pin: the pinpad verified with no PIN in any message, and authentication returned a signature");
}

fn refuse(probe: &Probe) {
	let since = probe.now();
	let transaction = probe.acquire(0, 5000, 0);
	let cases: [(&[u8], Error, &[u8]); 5] = [
		(&[0x00, 0x20, 0x00, 0x80, 0x08, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0xff, 0xff], Error::Denied, b"a PIN-bearing VERIFY"),
		(&[0x00, 0xa4, 0x04, 0x00, 0x07, 0xa0, 0x00, 0x00, 0x00, 0x03, 0x10, 0x10, 0x00], Error::Unsupported, b"another application"),
		(&[0x10, 0xcb, 0x3f, 0xff, 0x03, 0x5c, 0x01, 0x7e, 0x00], Error::Unsupported, b"a chained command"),
		(&[0x80, 0xca, 0x00, 0x00, 0x00], Error::Unsupported, b"a proprietary command"),
		(&[0x00, 0x87, 0x11, 0x9a, 0x04, 0x7c, 0x02, 0x82, 0x00, 0x00], Error::Denied, b"a raw GENERAL AUTHENTICATE"),
	];
	for (apdu, expected, what) in cases {
		if probe.exchange(transaction, apdu).err() != Some(expected) {
			let mut line = b"refuse: ".to_vec();
			line.extend_from_slice(what);
			line.extend_from_slice(b" was not refused as it must be");
			fail(&line);
		}
	}
	probe.release(transaction);
	// NONE OF THEM REACHED A READER.
	let reached = probe.operations(since).into_iter().any(|operation| operation.what == "exchange" && operation.header.first().is_some_and(|&class| class != 0x00) || operation.what == "exchange" && operation.header.get(1).is_some_and(|&instruction| instruction == 0x20 || instruction == 0x87) || operation.what == "verify" && operation.reader == B);
	if reached {
		fail(b"refuse: a refused command reached the reader");
	}
	say(b"PASS refuse: a PIN-bearing VERIFY, another application, chained and proprietary commands and a raw GENERAL AUTHENTICATE failed before any reader saw them");
}

fn isolation(probe: &Probe) {
	let name = match probe.card().reader() {
		Some(Ok(reader)) => reader.name,
		_ => fail(b"isolation: the reader could not be described"),
	};
	if !name.as_bytes().ends_with(b"reader A") && name.as_bytes() != b"org.libersystem.smartcard-fixture.a" {
		fail(b"isolation: this grant's reader is not reader A");
	}
	let stream = probe.events();
	snapshot(stream);
	probe.fixture_ok(probe.fixture().remove(&B, &0), b"isolation: the fixture could not remove B's card");
	probe.fixture_ok(probe.fixture().insert(&B, &0), b"isolation: the fixture could not insert B's card");
	if let Read::Event(_) = read(stream, clock() + TICKS) {
		fail(b"isolation: reader B's events reached a grant for reader A");
	}
	close(stream);
	say(b"PASS isolation: a grant for reader A sees reader A and nothing of reader B");
}

fn queue(probe: &Probe) {
	// The holder was started first and has the slot: this one queues behind it.
	sleep_until(clock() + TICKS);
	let since = probe.now();
	let transaction = probe.acquire(0, 15000, 0);
	let Ok(result) = probe.exchange(transaction, &GET_DISCOVERY) else { fail(b"queue: the queued client could not exchange") };
	if result.outcome != CardOutcome::Done {
		fail(b"queue: the queued client's exchange did not complete");
	}
	if !matches!(probe.card().authenticate_piv(&transaction, &CHALLENGE.to_vec()), Some(Err(Error::Denied))) {
		fail(b"queue: the queued client inherited the holder's verification");
	}
	probe.release(transaction);
	// THE ORDER, FROM THE READER'S OWN LOG: the holder's verification, then a reset - power off, power
	// on, the protocol, SELECT - and only then this client's first command. Read back from that command
	// to the verification before it, and not forward from when this probe began: the holder was started
	// first, and its verification can be older than anything this probe saw.
	let log = probe.operations(0);
	let on_a = |operation: &FixtureOperation, what: &str, instruction: Option<u8>| operation.what == what && operation.reader == A && operation.slot == 0 && instruction.is_none_or(|ins| operation.header.get(1) == Some(&ins));
	let ordered = (|| {
		let ours = log.iter().position(|operation| operation.at >= since && on_a(operation, "exchange", Some(0xcb)))?;
		let verified = log[..ours].iter().rposition(|operation| on_a(operation, "verify", None))?;
		let after = |from: usize, what: &str, instruction: Option<u8>| (from..ours).find(|&at| on_a(&log[at], what, instruction));
		let off = after(verified, "power-off", None)?;
		let on = after(off, "power-on", None)?;
		let protocol = after(on, "protocol", None)?;
		after(protocol, "exchange", Some(0xa4))
	})();
	if ordered.is_none() {
		fail(b"queue: the queued client's command was not after the holder's reset");
	}
	say(b"PASS queue: the second client got the slot only after the holder's release was followed by a full reset, and inherited no verification");
}

fn inherit(probe: &Probe) {
	// The transaction and the endpoint the holder sent down the pipe before it exited.
	let mut buf = [0u8; 64];
	let (len, handles) = loop {
		match try_recv_caps(stdin(), &mut buf) {
			PolledCaps::Message { len, handles } if !handles.as_slice().is_empty() => break (len, handles),
			PolledCaps::Message { .. } => {}
			// NOT YET: the holder acquires the slot before it sends anything.
			PolledCaps::Empty => sleep_until(clock() + TICKS / 10),
			PolledCaps::Closed => fail(b"inherit: the holder sent no endpoint"),
		}
	};
	if len < 8 {
		fail(b"inherit: the holder's message named no transaction");
	}
	let transaction = u64::from_le_bytes([buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7]]);
	let inherited = handles.first();
	// The holder has exited by now; its grant went with it.
	sleep_until(clock() + TICKS);
	let answer = smartcard::Client::with_deadline(ChannelTransport { chan: inherited }, clock() + 5 * TICKS).exchange(&transaction, &GET_DISCOVERY.to_vec());
	if matches!(answer, Some(Ok(_))) {
		fail(b"inherit: a transferred endpoint kept its dead owner's transaction");
	}
	close(inherited);
	// And the slot came back, reset, for this client.
	let mine = probe.acquire(0, 10000, 0);
	probe.release(mine);
	say(b"PASS inherit: a transferred endpoint did not keep its dead owner's transaction, and the slot was reset for the next");
}

fn expiry(probe: &Probe) {
	let since = probe.now();
	let transaction = probe.acquire(0, 5000, 2000);
	probe.fixture_ok(probe.fixture().delay(&A, &5000), b"expiry: the fixture could not delay");
	let started = clock();
	let Ok(result) = probe.exchange(transaction, &GET_DISCOVERY) else { fail(b"expiry: the exchange was refused") };
	let took = clock() - started;
	if result.outcome != CardOutcome::TimedOut || took > 3 * TICKS {
		fail(b"expiry: an exchange past the lease did not time out at the lease");
	}
	// THE LATE REPLY went out ahead of the abort's completion, and was nobody's answer.
	let log = probe.operations(since);
	if !log.iter().any(|operation| operation.what == "late") || !log.iter().any(|operation| operation.what == "abort") {
		fail(b"expiry: the fixture never sent the late reply or never saw the abort");
	}
	let next = probe.acquire(0, 10000, 0);
	let Ok(result) = probe.exchange(next, &GET_DISCOVERY) else { fail(b"expiry: the slot did not come back") };
	if result.outcome != CardOutcome::Done {
		fail(b"expiry: the slot came back answering something other than its own command");
	}
	probe.release(next);
	say(b"PASS expiry: a lease ending mid-exchange timed it out once, the late reply was dropped, and the reset slot answered the next client");
}

// An exchange sent without waiting for its answer, so the card can be pulled while it is in flight.
fn send_exchange(chan: u64, corr: u32, transaction: u64, apdu: &[u8]) {
	let mut writer = wire::VecWriter::new();
	let written = (|| {
		writer.u16(smartcard::OP_EXCHANGE)?;
		writer.u32(corr)?;
		writer.u64(transaction)?;
		writer.u16(apdu.len() as u16)?;
		writer.raw(apdu)
	})();
	match written.and_then(|()| writer.into_inner()) {
		Some(bytes) if send_blocking(chan, &bytes, 0) => {}
		_ => fail(b"an exchange could not be sent"),
	}
}

fn removal(probe: &Probe) {
	let since = probe.now();
	let transaction = probe.acquire(0, 5000, 0);
	probe.fixture_ok(probe.fixture().delay(&A, &3000), b"removal: the fixture could not delay");
	send_exchange(probe.card, 77, transaction, &GET_DISCOVERY);
	sleep_until(clock() + TICKS / 2);
	probe.fixture_ok(probe.fixture().remove(&A, &0), b"removal: the fixture could not remove the card");
	probe.fixture_ok(probe.fixture().insert(&A, &0), b"removal: the fixture could not reinsert the card");
	let mut handles = wire::Handles::new();
	let bytes = match recv_vec_caps_deadline(probe.card, &mut handles, clock() + 5 * TICKS) {
		ReceivedVecCaps::Message { bytes } => bytes,
		_ => fail(b"removal: the exchange in flight was never answered"),
	};
	let mut reader = wire::Reader::new(&bytes);
	let removed = (|| {
		if reader.u32()? != 77 || !reader.tag()? {
			return None;
		}
		Some(proto::system::ExchangeResult::read(&mut reader)?.outcome == CardOutcome::CardRemoved)
	})();
	if removed != Some(true) {
		fail(b"removal: the removed card's exchange was not answered card-removed");
	}
	if probe.exchange(transaction, &GET_DISCOVERY).is_ok() {
		fail(b"removal: the removed card's transaction still worked");
	}
	let next = probe.acquire(0, 10000, 0);
	probe.release(next);
	// THE NEW CARD WAS ADMITTED ONLY AFTER THE OLD ABORT: the abort, then the reset, in the log.
	let log = probe.operations(since);
	let reinserted = log.iter().position(|operation| operation.what == "inserted").unwrap_or(usize::MAX);
	let abort = log.iter().position(|operation| operation.what == "abort").unwrap_or(usize::MAX);
	let power_on = log.iter().rposition(|operation| operation.what == "power-on").unwrap_or(0);
	if abort == usize::MAX || reinserted == usize::MAX || power_on < abort {
		fail(b"removal: the reinserted card was reset before the old abort was complete");
	}
	say(b"PASS removal: removal mid-exchange was a distinct terminal result, and the reinserted card waited for the abort and a reset");
}

fn stuck(probe: &Probe) {
	let transaction = probe.acquire(0, 5000, 2000);
	probe.fixture_ok(probe.fixture().delay(&A, &0), b"stuck: the fixture could not hold the exchange");
	probe.fixture_ok(probe.fixture().hold_abort(&A, &0), b"stuck: the fixture could not hold the abort");
	let Ok(result) = probe.exchange(transaction, &GET_DISCOVERY) else { fail(b"stuck: the exchange was refused") };
	if result.outcome != CardOutcome::TimedOut {
		fail(b"stuck: the held exchange did not time out");
	}
	// Five seconds for the abort, and a little over.
	sleep_until(clock() + 6 * TICKS);
	let unavailable = matches!(probe.card().slots(), Some(Ok(slots)) if slots.iter().any(|slot| slot.slot == 0 && !slot.available));
	if !unavailable {
		fail(b"stuck: an abort never proven did not make the slot unavailable");
	}
	if !matches!(probe.card().acquire(&0, &0, &0), Some(Err(Error::Io))) {
		fail(b"stuck: an unavailable slot was acquirable");
	}
	probe.fixture_ok(probe.fixture().recover(&A, &0), b"stuck: the fixture could not recover the slot");
	let deadline = clock() + 5 * TICKS;
	let transaction = loop {
		if let Some(Ok(transaction)) = probe.card().acquire(&0, &1000, &0) {
			break transaction.id;
		}
		if clock() >= deadline {
			fail(b"stuck: the recovered slot never came back");
		}
		sleep_until(clock() + TICKS / 5);
	};
	probe.release(transaction);
	say(b"PASS stuck: an abort that was never proven left the slot unavailable, its provider's recovery brought it back after a reset");
}

fn overflow(probe: &Probe) {
	let stream = probe.events();
	// NOTHING IS READ while forty presence changes happen.
	for _ in 0..20 {
		probe.fixture_ok(probe.fixture().insert(&A, &1), b"overflow: the fixture could not insert");
		probe.fixture_ok(probe.fixture().remove(&A, &1), b"overflow: the fixture could not remove");
	}
	sleep_until(clock() + TICKS / 2);
	let mut frames = 0;
	loop {
		match read(stream, clock() + 2 * TICKS) {
			Read::Event(_) => frames += 1,
			Read::Closed => break,
			Read::Quiet => fail(b"overflow: a stream forty events behind was not closed"),
		}
	}
	close(stream);
	let fresh = probe.events();
	let current = snapshot(fresh);
	close(fresh);
	if !current.iter().any(|event| event.slot.as_ref().is_some_and(|slot| slot.slot == 1 && slot.presence == Presence::Absent)) {
		fail(b"overflow: the new snapshot is not the current state");
	}
	let mut line = b"PASS overflow: a stream sixteen events behind was closed after ".to_vec();
	number(&mut line, frames);
	line.extend_from_slice(b" frames, and a new subscription's snapshot is current");
	say(&line);
}

// Wait until the exchange the background holder started is in flight at the reader, so a restart lands
// on it and not before it.
fn inflight(probe: &Probe) {
	let deadline = clock() + 5 * TICKS;
	loop {
		let log = probe.operations(0);
		if log.last().is_some_and(|operation| operation.what == "exchange" && operation.reader == A && operation.header.get(1) == Some(&0xcb)) {
			say(b"an exchange the fixture holds is in flight");
			return;
		}
		if clock() >= deadline {
			fail(b"inflight: the holder's exchange never reached the reader");
		}
		sleep_until(clock() + TICKS / 10);
	}
}

// THE RESTART LANDS ON THE EXCHANGE IN FLIGHT, made through the supervisor's admin channel as `stop` and
// `start` make it. A probe typed after `start` could not look: its launch needs a grant minted for reader
// A, and the replacement admits reader A only once the provider has answered its session - which is the
// ordering under test, so the launch was refused while the old exchange drained. This one was launched
// before the stop, and waits for that answer on the fixture's log alone.
fn restart(probe: &Probe, supervisor: u64) {
	let since = probe.now();
	let mut reply = [0u8; 512];
	if !send_blocking(supervisor, b"smartcard_service", 0) {
		fail(b"restart: the supervisor could not be asked to stop the service");
	}
	match recv_blocking(supervisor, &mut reply) {
		Received::Message { len, .. } if reply[..len].starts_with(b"STOPPED\n") => {}
		_ => fail(b"restart: the supervisor did not stop the service"),
	}
	if !send_blocking(supervisor, b"+smartcard_service", 0) {
		fail(b"restart: the supervisor could not be asked to start the service");
	}
	match recv_blocking(supervisor, &mut reply) {
		Received::Message { len, .. } if reply[..len].starts_with(b"STARTED\n") => {}
		_ => fail(b"restart: the supervisor did not start the service again"),
	}
	let deadline = clock() + 20 * TICKS;
	while !probe.operations(since).iter().any(|operation| operation.what == "session" && operation.reader == A) {
		if clock() >= deadline {
			fail(b"restart: the replacement's session was never answered");
		}
		sleep_until(clock() + TICKS / 10);
	}
	say(b"PASS restart: the service was stopped and started with the exchange in flight, and the fixture answered the replacement's session");
}

fn session(probe: &Probe) {
	// THE NEW SESSION WAITED FOR THE DRAIN: the fixture drained the old session's held exchange, then
	// answered the new one.
	let log = probe.operations(0);
	let drained = log.iter().rposition(|operation| operation.what == "drained" && operation.reader == A);
	let session = log.iter().rposition(|operation| operation.what == "session" && operation.reader == A);
	match (drained, session) {
		(Some(drained), Some(session)) if session > drained && log[session].at >= log[drained].at => {}
		_ => fail(b"session: the restarted service's session did not wait for the old work to drain"),
	}
	let transaction = probe.acquire(0, 10000, 0);
	probe.release(transaction);
	say(b"PASS session: the restarted service's session opened only once the old session's exchange had drained, and the slot serves again");
}

fn republish(probe: &Probe) {
	if !matches!(probe.card().reader(), Some(Ok(_))) {
		fail(b"republish: the grant did not work before the withdrawal");
	}
	probe.fixture_ok(probe.fixture().withdraw(&A), b"republish: the fixture could not withdraw reader A");
	sleep_until(clock() + TICKS / 2);
	if matches!(smartcard::Client::with_deadline(ChannelTransport { chan: probe.card }, clock() + 2 * TICKS).reader(), Some(Ok(_))) {
		fail(b"republish: a withdrawn reader's grant still answered");
	}
	probe.fixture_ok(probe.fixture().republish(&A), b"republish: the fixture could not republish reader A");
	say(b"PASS republish: a withdrawn reader's grant failed, and the reader was published again as another");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let args: Vec<u8> = context.arguments.clone().into_bytes();
	let supervisor = recv_tagged(bootstrap, &mut buf, b"SUPERVISOR").unwrap_or(0);
	let fixture = recv_tagged(bootstrap, &mut buf, b"FIXTURE").unwrap_or(0);
	let card = recv_tagged(bootstrap, &mut buf, b"SMARTCARD").unwrap_or(0);
	if supervisor == 0 || fixture == 0 || card == 0 {
		fail(b"a grant this probe needs was not delivered");
	}
	let probe = Probe { card, fixture };
	match args.split(|&b| b == b' ').next().unwrap_or(&[]) {
		b"insert" => insert(&probe),
		b"pin" => pin(&probe),
		b"refuse" => refuse(&probe),
		b"isolation" => isolation(&probe),
		b"queue" => queue(&probe),
		b"inherit" => inherit(&probe),
		b"expiry" => expiry(&probe),
		b"removal" => removal(&probe),
		b"stuck" => stuck(&probe),
		b"overflow" => overflow(&probe),
		b"prime-slow" => probe.fixture_ok(probe.fixture().delay(&A, &4000), b"prime-slow: the fixture could not delay"),
		b"inflight" => inflight(&probe),
		b"restart" => restart(&probe, supervisor),
		b"session" => session(&probe),
		b"republish" => republish(&probe),
		_ => fail(b"usage: cardcheck insert | pin | refuse | isolation | queue | inherit | expiry | removal | stuck | overflow | prime-slow | inflight | restart | session | republish"),
	}
	exit();
}
