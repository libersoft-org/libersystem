// modem_fixture - an in-guest modem with a SIM, one data context and the network behind it, published as
// a `modem` provider over the production wire, and a control endpoint for the probe that drives the
// modem gate.
//
// DEVELOPMENT-ONLY. It binds to a QEMU test function at a pinned address only the modem gate adds, and
// its control endpoint is a kind no scope minted for real hardware admits. It is not a claim about USB
// CDC-MBIM transport: no MBIM device exists in QEMU, and none is pretended here.
//
// THE STAGED VALIDATORS CARRY EVERY RECORD. Each command ModemService sends is cut into MBIM control
// fragments and reassembled by `drivers::mbim`'s assembler before the "device" acts on it, and each reply
// crosses back the same way; each datagram, in either direction, is packed into an NCM transfer block
// and taken out of it by the same session/NDP validator the USB class module will use. A record the
// validators refuse never reaches the other side.
//
// WHAT IT PLAYS. A SIM with a PIN, a PUK and their counters; one context, configured with an IPv4
// address, a gateway, a resolver and an MTU - or, when told, with an IPv6-only configuration this system
// does not install; and the network behind it: the gateway answers ICMP echo, and the resolver answers
// `modem.test` with one address and every other name with NXDOMAIN.
//
// WHAT THE PROBE CAN MAKE IT DO. Replace or remove the SIM (a new SIM generation each time), hold the next
// reply to a kind of command, withdraw and republish the modem, end the context from the network side,
// replay the last activation's reply as a stale duplicate, answer activations IPv6-only, and read back
// what it counted - including whether a PIN arrived, never what it was.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use drivers::{common, mbim};
use proto::system::{Command, CommandKind, CommandReply, CommandStatus, Datagram, DeviceIdentity, DeviceLimits, DeviceOpen, DeviceRegistration, DeviceSim, DeviceState, Error, FixtureStats, Indication, IpConfig, IpFamily, modem_device, modem_fixture};
use rt::*;

const NAME: &[u8] = b"org.libersystem.modem-fixture";
const CONTROL_NAME: &[u8] = b"org.libersystem.modem-fixture.control";
const MODEM_TOKEN: u16 = 0;
const CONTROL_TOKEN: u16 = 1;
const VERSION: u32 = 1;
const PIN: &[u8] = b"1234";
const PUK: &[u8] = b"12345678";
const PIN_TRIES: u8 = 3;
const PUK_TRIES: u8 = 10;
// The network behind the modem: a /30 with the gateway as its resolver.
const ADDRESS: u32 = u32::from_be_bytes([10, 64, 0, 2]);
const GATEWAY: u32 = u32::from_be_bytes([10, 64, 0, 1]);
const PREFIX: u8 = 30;
const MTU: u16 = 1400;
const ANSWERED: &[u8] = b"modem.test";
const ANSWER: [u8; 4] = [198, 51, 100, 7];
// SMALL CONTROL TRANSFERS, so every command and every reply crosses the fragment validator in pieces.
const MAX_TRANSFER: usize = 64;
const COMMAND_MSG: u32 = 0x0000_0003;
const COMMAND_DONE: u32 = 0x8000_0003;
const STREAM_DEPTH: u64 = 64;
const NEVER: u64 = u64::MAX;

struct Sim {
	present: bool,
	locked: bool,
	pin: Vec<u8>,
	pin_tries: u8,
	puk_tries: u8,
	generation: u64,
}

impl Sim {
	fn state(&self) -> DeviceSim {
		if !self.present {
			DeviceSim::Absent
		} else if self.puk_tries == 0 {
			DeviceSim::Blocked
		} else if self.pin_tries == 0 {
			DeviceSim::LockedPuk
		} else if self.locked {
			DeviceSim::LockedPin
		} else {
			DeviceSim::Ready
		}
	}
}

// One consumer's session: the connection replies go to, the producer ends of its two streams, and this
// fixture's end of the channel datagrams arrive on.
#[derive(Default)]
struct Session {
	chan: u64,
	indications: u64,
	indication_seq: u32,
	receive: u64,
	receive_seq: u32,
	transmit: u64,
}

struct Held {
	release: u64,
	chan: u64,
	bytes: Vec<u8>,
}

struct Fixture {
	live: bool,
	token: u16,
	next_token: u16,
	sim: Sim,
	revision: u64,
	connection: u64,
	context: Option<u64>,
	ipv6_only: bool,
	delays: Vec<(CommandKind, u32)>,
	held: Vec<Held>,
	// The last activation's reply as it went, for the probe's stale duplicate.
	last_activation: Option<Vec<u8>>,
	session: Session,
	stats: FixtureStats,
	device_side: mbim::Assembler,
	host_side: mbim::Assembler,
	ntb_sequence: u16,
}

fn limits() -> mbim::Limits {
	mbim::Limits::negotiate(16, 16 * 1024, 64 * 1024, 4)
}

fn ticks(ms: u32) -> u64 {
	(ms as u64).div_ceil(10)
}

impl Fixture {
	fn state(&self) -> DeviceState {
		let known = self.sim.present;
		DeviceState { manufacturer: String::from("LiberSystem"), model: String::from("modem fixture"), sim: self.sim.state(), sim_generation: self.sim.generation, registration: if self.sim.state() == DeviceSim::Ready { DeviceRegistration::Home } else { DeviceRegistration::NotRegistered }, operator: (self.sim.state() == DeviceSim::Ready).then(|| String::from("Fixture Network")), signal_valid: known, rssi_dbm: if known { -71 } else { 0 }, quality: if known { 3 } else { 0 }, pin_attempts: known.then_some(self.sim.pin_tries), puk_attempts: known.then_some(self.sim.puk_tries), context_active: self.context.is_some() }
	}

	// Something about the device changed: a new revision, to the indication stream.
	fn indicate(&mut self) {
		self.revision += 1;
		if self.session.indications == 0 {
			return;
		}
		let indication = Indication { revision: self.revision, state: self.state() };
		let mut frame = [0u8; 512];
		let mut handles = wire::Handles::new();
		if let Some(len) = modem_device::indications_frame(self.session.indication_seq, &indication, &mut frame, &mut handles) {
			let _ = try_send(self.session.indications, &frame[..len], 0);
			self.session.indication_seq += 1;
		}
	}

	// A record crossing the control pipe as MBIM: fragmented, reassembled and validated, or refused.
	fn through_mbim(assembler: &mut mbim::Assembler, message_type: u32, transaction: u32, body: &[u8]) -> Option<Vec<u8>> {
		let mut done = None;
		for transfer in mbim::fragments(message_type, transaction, body, MAX_TRANSFER) {
			let fragment = mbim::parse_fragment(&transfer).ok()?;
			if let Some(message) = assembler.feed(&fragment, clock()).ok()? {
				done = Some(message.body);
			}
		}
		done
	}

	// The session ended: whatever it left is dropped, and a new one starts clean.
	fn departed(&mut self) {
		for handle in [self.session.indications, self.session.receive, self.session.transmit] {
			if handle != 0 {
				close(handle);
			}
		}
		self.session = Session::default();
		self.held.clear();
		self.device_side = mbim::Assembler::new(limits());
		self.host_side = mbim::Assembler::new(limits());
	}

	fn release_due(&mut self) {
		let now = clock();
		let (due, kept): (Vec<Held>, Vec<Held>) = core::mem::take(&mut self.held).into_iter().partition(|held| held.release <= now);
		self.held = kept;
		for held in due {
			let _ = try_send(held.chan, &held.bytes, 0);
		}
	}

	fn next_due(&self) -> u64 {
		self.held.iter().map(|held| held.release).filter(|&release| release != NEVER).min().unwrap_or(0)
	}

	// ------------------------------------------------------------------ the network behind the modem

	// One datagram toward the network, out of its transfer block. What the network answers goes back the
	// same way.
	fn to_network(&mut self, datagram: Datagram) {
		if self.context != Some(datagram.context_generation) {
			return;
		}
		// THE SESSION VALIDATOR, on the way out.
		let sequence = self.next_sequence();
		let block = mbim::block(0, &[&datagram.bytes], sequence);
		let Ok(ranges) = mbim::datagrams(&block, 0, MTU) else { return };
		for range in ranges {
			self.stats.datagrams_in += 1;
			if let Some(answer) = self.answer(&block[range]) {
				self.to_service(answer);
			}
		}
	}

	fn to_service(&mut self, packet: Vec<u8>) {
		let Some(context) = self.context else { return };
		// AND ON THE WAY IN.
		let sequence = self.next_sequence();
		let block = mbim::block(0, &[&packet], sequence);
		let Ok(ranges) = mbim::datagrams(&block, 0, MTU) else { return };
		for range in ranges {
			let datagram = Datagram { context_generation: context, bytes: block[range].to_vec() };
			let mut frame = alloc::vec![0u8; datagram.bytes.len() + 64];
			let mut handles = wire::Handles::new();
			if self.session.receive != 0
				&& let Some(len) = modem_device::receive_frame(self.session.receive_seq, &datagram, &mut frame, &mut handles)
				&& try_send(self.session.receive, &frame[..len], 0)
			{
				self.session.receive_seq += 1;
				self.stats.datagrams_out += 1;
			}
		}
	}

	fn next_sequence(&mut self) -> u16 {
		self.ntb_sequence = self.ntb_sequence.wrapping_add(1);
		self.ntb_sequence
	}

	// The gateway: an echo reply to an echo request, an answer to a DNS query, nothing to the rest.
	fn answer(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
		if packet.len() < 20 || packet[0] >> 4 != 4 {
			return None;
		}
		let ihl = usize::from(packet[0] & 15) * 4;
		let total = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
		if ihl < 20 || total < ihl || total > packet.len() {
			return None;
		}
		let (source, destination) = (&packet[12..16], &packet[16..20]);
		let payload = &packet[ihl..total];
		match packet[9] {
			1 if payload.len() >= 8 && payload[0] == 8 => {
				let mut reply = payload.to_vec();
				reply[0] = 0;
				reply[2..4].copy_from_slice(&[0, 0]);
				let sum = checksum(&reply);
				reply[2..4].copy_from_slice(&sum.to_be_bytes());
				self.stats.echo_replies += 1;
				Some(ipv4(1, destination, source, &reply))
			}
			17 if payload.len() >= 8 && u16::from_be_bytes([payload[2], payload[3]]) == 53 => {
				let answer = dns_answer(&payload[8..])?;
				self.stats.dns_answers += 1;
				let mut udp = Vec::with_capacity(8 + answer.len());
				udp.extend_from_slice(&53u16.to_be_bytes());
				udp.extend_from_slice(&payload[0..2]);
				udp.extend_from_slice(&((8 + answer.len()) as u16).to_be_bytes());
				udp.extend_from_slice(&[0, 0]);
				udp.extend_from_slice(&answer);
				Some(ipv4(17, destination, source, &udp))
			}
			_ => None,
		}
	}
}

fn checksum(bytes: &[u8]) -> u16 {
	let mut sum: u32 = 0;
	for pair in bytes.chunks(2) {
		let word = if pair.len() == 2 { u16::from_be_bytes([pair[0], pair[1]]) } else { u16::from_be_bytes([pair[0], 0]) };
		sum += u32::from(word);
	}
	while sum > 0xffff {
		sum = (sum & 0xffff) + (sum >> 16);
	}
	!(sum as u16)
}

fn ipv4(protocol: u8, source: &[u8], destination: &[u8], payload: &[u8]) -> Vec<u8> {
	let total = 20 + payload.len();
	let mut packet = alloc::vec![0u8; 20];
	packet[0] = 0x45;
	packet[2..4].copy_from_slice(&(total as u16).to_be_bytes());
	packet[8] = 64;
	packet[9] = protocol;
	packet[12..16].copy_from_slice(source);
	packet[16..20].copy_from_slice(destination);
	let sum = checksum(&packet);
	packet[10..12].copy_from_slice(&sum.to_be_bytes());
	packet.extend_from_slice(payload);
	packet
}

// The resolver: `modem.test` has one A record and no AAAA record; every other name does not exist.
fn dns_answer(query: &[u8]) -> Option<Vec<u8>> {
	if query.len() < 12 || query[2] & 0x80 != 0 || u16::from_be_bytes([query[4], query[5]]) != 1 {
		return None;
	}
	// The question: labels, then type and class.
	let mut at = 12;
	let mut name: Vec<u8> = Vec::new();
	loop {
		let length = usize::from(*query.get(at)?);
		at += 1;
		if length == 0 {
			break;
		}
		if length > 63 {
			return None;
		}
		if !name.is_empty() {
			name.push(b'.');
		}
		name.extend(query.get(at..at + length)?.iter().map(|byte| byte.to_ascii_lowercase()));
		at += length;
	}
	let qtype = u16::from_be_bytes([*query.get(at)?, *query.get(at + 1)?]);
	let question_end = at + 4;
	if question_end > query.len() {
		return None;
	}
	let known = name.as_slice() == ANSWERED;
	let answers: u16 = u16::from(known && qtype == 1);
	let mut answer = Vec::with_capacity(question_end + 16);
	answer.extend_from_slice(&query[0..2]);
	// A response, recursion desired and available; NXDOMAIN for a name that does not exist.
	answer.extend_from_slice(&[0x81, if known { 0x80 } else { 0x83 }]);
	answer.extend_from_slice(&1u16.to_be_bytes());
	answer.extend_from_slice(&answers.to_be_bytes());
	answer.extend_from_slice(&[0, 0, 0, 0]);
	answer.extend_from_slice(&query[12..question_end]);
	if answers == 1 {
		answer.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
		answer.extend_from_slice(&ANSWER);
	}
	Some(answer)
}

// ------------------------------------------------------------------ the provider

struct ModemView<'a> {
	fixture: &'a mut Fixture,
	// A reply to hold rather than send: until when.
	hold: Option<u64>,
	// A reply that must also be kept for a later stale replay.
	activation: bool,
}

impl modem_device::Service for ModemView<'_> {
	// A NEW SESSION IS A NEW CONNECTION GENERATION, and starts from nothing the last one left.
	fn open(&mut self, version: u32) -> Result<DeviceOpen, Error> {
		if version != VERSION {
			return Err(Error::Unsupported);
		}
		self.fixture.connection += 1;
		let limits = DeviceLimits { version: VERSION, pending: 8, fragments: 16, message_bytes: 16 * 1024, assembly_bytes: 64 * 1024, assemblies: 4, mtu: MTU };
		Ok(DeviceOpen { limits, connection_generation: self.fixture.connection })
	}

	fn command(&mut self, command: Command) -> Result<CommandReply, Error> {
		let fixture = &mut *self.fixture;
		// THE COMMAND CROSSES THE CONTROL PIPE AS MBIM, and what the device acts on is what reassembled.
		let body = command.encode_vec().ok_or(Error::Invalid)?;
		let Some(arrived) = Fixture::through_mbim(&mut fixture.device_side, COMMAND_MSG, command.transaction as u32, &body) else { return Err(Error::Corrupt) };
		let mut command = Command::decode(&arrived).ok_or(Error::Corrupt)?;
		fixture.stats.commands += 1;
		if let Some(at) = fixture.delays.iter().position(|(kind, _)| *kind == command.kind) {
			let (_, delay_ms) = fixture.delays.remove(at);
			self.hold = Some(if delay_ms == 0 { NEVER } else { clock() + ticks(delay_ms) });
		}
		let mut reply = CommandReply { connection_generation: command.connection_generation, transaction: command.transaction, kind: command.kind, status: CommandStatus::Done, sim_generation: fixture.sim.generation, context_generation: command.context_generation, state: None, config: None, identity: None };
		// A COMMAND PREPARED AGAINST ANOTHER SIM is stale, and does nothing.
		if command.sim_generation != fixture.sim.generation || command.connection_generation != fixture.connection {
			reply.status = CommandStatus::Stale;
			reply.state = Some(fixture.state());
			return Ok(reply);
		}
		let mut changed = false;
		match command.kind {
			CommandKind::Query => {}
			CommandKind::EnterPin => {
				fixture.stats.pin_attempts += 1;
				fixture.stats.last_secret_bytes = command.secret.len() as u8;
				reply.status = match fixture.sim.state() {
					DeviceSim::LockedPin if command.secret == fixture.sim.pin => {
						fixture.sim.locked = false;
						fixture.sim.pin_tries = PIN_TRIES;
						CommandStatus::Done
					}
					DeviceSim::LockedPin => {
						fixture.sim.pin_tries -= 1;
						CommandStatus::Rejected
					}
					DeviceSim::Ready => CommandStatus::Done,
					DeviceSim::Absent => CommandStatus::Failed,
					_ => CommandStatus::Rejected,
				};
				changed = true;
			}
			CommandKind::EnterPuk => {
				fixture.stats.pin_attempts += 1;
				fixture.stats.last_secret_bytes = command.secret.len() as u8;
				reply.status = match fixture.sim.state() {
					DeviceSim::LockedPuk if command.secret == PUK && (4..=8).contains(&command.new_secret.len()) => {
						fixture.sim.pin = core::mem::take(&mut command.new_secret);
						fixture.sim.pin_tries = PIN_TRIES;
						fixture.sim.puk_tries = PUK_TRIES;
						fixture.sim.locked = false;
						CommandStatus::Done
					}
					DeviceSim::LockedPuk => {
						fixture.sim.puk_tries -= 1;
						CommandStatus::Rejected
					}
					DeviceSim::Absent => CommandStatus::Failed,
					_ => CommandStatus::Rejected,
				};
				changed = true;
			}
			CommandKind::Activate => {
				fixture.stats.activations += 1;
				if fixture.sim.state() != DeviceSim::Ready {
					reply.status = CommandStatus::Rejected;
				} else if fixture.context.is_some() {
					reply.status = CommandStatus::Failed;
				} else {
					fixture.context = Some(command.context_generation);
					let family = if fixture.ipv6_only { IpFamily::Ipv6 } else { IpFamily::Ipv4 };
					reply.config = Some(IpConfig { family, address: ADDRESS, prefix: PREFIX, gateway: Some(GATEWAY), dns: alloc::vec![GATEWAY], mtu: MTU });
					self.activation = true;
					changed = true;
				}
			}
			CommandKind::Deactivate => {
				if fixture.context == Some(command.context_generation) {
					fixture.context = None;
					changed = true;
				} else {
					reply.status = CommandStatus::Stale;
				}
			}
			CommandKind::Identity => {
				reply.identity = fixture.sim.present.then(|| DeviceIdentity { imsi: Some(String::from("001010000000001")), iccid: Some(String::from("8900100000000000001")), msisdn: Some(String::from("+15550100")) });
				if !fixture.sim.present {
					reply.status = CommandStatus::Failed;
				}
			}
		}
		// ZEROED, whatever the command was: nothing it carried outlives its handling.
		for byte in command.secret.iter_mut().chain(command.new_secret.iter_mut()) {
			// SAFETY: a valid, aligned byte of this vector.
			unsafe { core::ptr::write_volatile(byte, 0) };
		}
		reply.state = Some(fixture.state());
		// AND THE REPLY CROSSES BACK AS MBIM.
		let body = reply.encode_vec().ok_or(Error::Invalid)?;
		let Some(arrived) = Fixture::through_mbim(&mut fixture.host_side, COMMAND_DONE, reply.transaction as u32, &body) else { return Err(Error::Corrupt) };
		let reply = CommandReply::decode(&arrived).ok_or(Error::Corrupt)?;
		if changed {
			fixture.indicate();
		}
		Ok(reply)
	}

	fn indications(&mut self) -> Vec<Indication> {
		Vec::new()
	}

	fn receive(&mut self) -> Vec<Datagram> {
		Vec::new()
	}

	fn transmit(&mut self) -> Result<u64, Error> {
		let (mine, theirs) = channel_with_depth(STREAM_DEPTH).ok_or(Error::Exhausted)?;
		if self.fixture.session.transmit != 0 {
			close(self.fixture.session.transmit);
		}
		self.fixture.session.transmit = mine;
		Ok(theirs)
	}
}

// ------------------------------------------------------------------ the control endpoint

struct ControlView<'a> {
	fixture: &'a mut Fixture,
	serving: &'a mut common::Serving,
	bootstrap: u64,
	bind: &'a common::Bind,
}

impl modem_fixture::Service for ControlView<'_> {
	fn set_sim(&mut self, present: bool, locked: bool) -> Result<(), Error> {
		let fixture = &mut *self.fixture;
		fixture.sim = Sim { present, locked, pin: PIN.to_vec(), pin_tries: PIN_TRIES, puk_tries: PUK_TRIES, generation: fixture.sim.generation + 1 };
		// A SIM CHANGE ENDS THE CONTEXT on the network side too.
		fixture.context = None;
		fixture.indicate();
		Ok(())
	}

	fn delay(&mut self, kind: CommandKind, delay_ms: u32) -> Result<(), Error> {
		self.fixture.delays.retain(|(held, _)| *held != kind);
		self.fixture.delays.push((kind, delay_ms));
		Ok(())
	}

	fn withdraw(&mut self) -> Result<(), Error> {
		let fixture = &mut *self.fixture;
		if !fixture.live {
			return Err(Error::NotFound);
		}
		fixture.live = false;
		fixture.context = None;
		fixture.departed();
		if !common::withdraw(self.bootstrap, self.bind, fixture.token) {
			return Err(Error::Io);
		}
		Ok(())
	}

	fn republish(&mut self) -> Result<(), Error> {
		let fixture = &mut *self.fixture;
		if fixture.live {
			return Err(Error::Invalid);
		}
		let token = fixture.next_token;
		let Some((near, far)) = channel() else { return Err(Error::Exhausted) };
		if !self.serving.publish(token, near) {
			close(near);
			close(far);
			return Err(Error::Exhausted);
		}
		if !common::offer_named(self.bootstrap, self.bind, driver_protocol::provider::MODEM, token, NAME, far) {
			return Err(Error::Io);
		}
		fixture.next_token = token + 1;
		fixture.token = token;
		fixture.live = true;
		Ok(())
	}

	fn stats(&mut self) -> Result<FixtureStats, Error> {
		Ok(self.fixture.stats.clone())
	}

	// THE SAME REPLY AGAIN, to the same session: a duplicate the service already answered.
	fn replay(&mut self) -> Result<(), Error> {
		let fixture = &mut *self.fixture;
		let bytes = fixture.last_activation.clone().ok_or(Error::NotFound)?;
		if fixture.session.chan == 0 || !try_send(fixture.session.chan, &bytes, 0) {
			return Err(Error::Io);
		}
		Ok(())
	}

	fn drop_context(&mut self) -> Result<(), Error> {
		if self.fixture.context.take().is_none() {
			return Err(Error::NotFound);
		}
		self.fixture.indicate();
		Ok(())
	}

	fn ipv6_only(&mut self, on: bool) -> Result<(), Error> {
		self.fixture.ipv6_only = on;
		Ok(())
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
	let mut reply_buf = alloc::vec![0u8; 4096];
	let mut reply_handles = wire::Handles::new();
	if token == CONTROL_TOKEN {
		let mut view = ControlView { fixture: &mut *fixture, serving: &mut *serving, bootstrap, bind };
		if let Some(written) = modem_fixture::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) {
			send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
		}
		return true;
	}
	if token != fixture.token || !fixture.live {
		return true;
	}
	// A NEW CONSUMER CONNECTION IS A NEW SESSION: what the old one held is gone.
	if fixture.session.chan != channel {
		fixture.departed();
		fixture.session.chan = channel;
	}
	let op = u16::from_le_bytes([buf[0], buf[1]]);
	if op == modem_device::OP_INDICATIONS || op == modem_device::OP_RECEIVE {
		let mut view = ModemView { fixture: &mut *fixture, hold: None, activation: false };
		let opened = if op == modem_device::OP_INDICATIONS { modem_device::indications_open(&mut view, &buf[..len], &mut handles).map(|(corr, _)| corr) } else { modem_device::receive_open(&mut view, &buf[..len], &mut handles).map(|(corr, _)| corr) };
		let Some(corr) = opened else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if op == modem_device::OP_INDICATIONS {
			if fixture.session.indications != 0 {
				close(fixture.session.indications);
			}
			fixture.session.indications = producer;
			fixture.session.indication_seq = 0;
		} else {
			if fixture.session.receive != 0 {
				close(fixture.session.receive);
			}
			fixture.session.receive = producer;
			fixture.session.receive_seq = 0;
		}
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		// THE INDICATION STREAM OPENS WITH THE CURRENT STATE.
		if op == modem_device::OP_INDICATIONS {
			fixture.indicate();
		}
		return true;
	}
	let mut view = ModemView { fixture: &mut *fixture, hold: None, activation: false };
	let written = modem_device::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles);
	let (hold, activation) = (view.hold, view.activation);
	// The request may have carried a PIN.
	for byte in buf[..len].iter_mut() {
		// SAFETY: a valid, aligned byte of this buffer.
		unsafe { core::ptr::write_volatile(byte, 0) };
	}
	let Some(written) = written else { return true };
	if activation {
		fixture.last_activation = Some(reply_buf[..written].to_vec());
	}
	match hold {
		Some(release) => fixture.held.push(Held { release, chan: channel, bytes: reply_buf[..written].to_vec() }),
		None => {
			send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
		}
	}
	true
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = common::handshake(bootstrap);
	let (Some((modem, modem_far)), Some((control, control_far))) = (channel(), channel()) else { exit() };
	let timer: u64 = match timer_create() {
		t if t > 0 => t as u64,
		_ => exit(),
	};
	common::online_named(bootstrap, &bind, b"driver.modem-fixture: online (a modem, its SIM and the network behind it, for the modem gate)", &[(driver_protocol::provider::MODEM, modem_far, NAME), (driver_protocol::provider::FIXTURE_CONTROL, control_far, CONTROL_NAME)]);
	let mut serving = common::Serving::from_offers(&[(MODEM_TOKEN, modem), (CONTROL_TOKEN, control)]);
	let mut fixture = Fixture { live: true, token: MODEM_TOKEN, next_token: CONTROL_TOKEN + 1, sim: Sim { present: true, locked: true, pin: PIN.to_vec(), pin_tries: PIN_TRIES, puk_tries: PUK_TRIES, generation: 1 }, revision: 0, connection: 0, context: None, ipv6_only: false, delays: Vec::new(), held: Vec::new(), last_activation: None, session: Session::default(), stats: FixtureStats { commands: 0, pin_attempts: 0, activations: 0, datagrams_in: 0, datagrams_out: 0, echo_replies: 0, dns_answers: 0, last_secret_bytes: 0 }, device_side: mbim::Assembler::new(limits()), host_side: mbim::Assembler::new(limits()), ntb_sequence: 0 };
	let mut buf = alloc::vec![0u8; 8192];
	loop {
		let due = fixture.next_due();
		// ARMED EVERY PASS, and far off when nothing is due. A timer stays expired until it is armed again,
		// so leaving it alone once the last deadline passed made every later wait return at once, and the
		// fixture spun a processor until something was due - long enough to starve the probes it serves.
		timer_set(timer, if due != 0 { due } else { u64::MAX });
		let mut devices: Vec<u64> = alloc::vec![timer];
		if fixture.session.transmit != 0 {
			devices.push(fixture.session.transmit);
		}
		match common::wait_providers_or_answer(bootstrap, &bind, &mut serving, &devices) {
			None => {
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, 0, true);
				}
				exit();
			}
			Some(common::ProviderReady::Connected(_)) => {}
			Some(common::ProviderReady::Device(0)) => fixture.release_due(),
			// Datagrams toward the network, one `datagram` record per message.
			Some(common::ProviderReady::Device(_)) => loop {
				match try_recv_caps(fixture.session.transmit, &mut buf) {
					PolledCaps::Message { len, handles } => {
						for &handle in handles.as_slice() {
							close(handle);
						}
						if let Some(datagram) = Datagram::decode(&buf[..len]) {
							fixture.to_network(datagram);
						}
					}
					PolledCaps::Empty => break,
					PolledCaps::Closed => {
						close(fixture.session.transmit);
						fixture.session.transmit = 0;
						break;
					}
				}
			},
			Some(common::ProviderReady::Consumer(index)) => {
				let token = serving.token_at(index);
				let chan = serving.at(index);
				if !serve(&mut fixture, &mut serving, bootstrap, &bind, token, chan, &mut buf) {
					let token = serving.close_at(index);
					if token == fixture.token && chan == fixture.session.chan {
						// THE CONSUMER LEFT: its context ends on this side, as a restarted service's would.
						fixture.context = None;
						fixture.departed();
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
