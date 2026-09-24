// bt_fixture - the in-guest Bluetooth controller AND the mouse on the other end of its radio.
//
// DEVELOPMENT-ONLY. It is staged into the image a gate builds and into no shipping one, and nothing a
// client does can enable it: it binds to a QEMU test device at a pinned address, which a shipping
// machine does not have.
//
// WHAT IT IS FOR. The Bluetooth host stack has to be proved end to end - scan, pair, bond, encrypt,
// discover a boot mouse, move a cursor, survive a restart and a cold reboot, forget - and there is no
// radio in an emulator. So this driver publishes a `bluetooth-hci` provider exactly as a USB
// controller's class module will, speaks the production transport wire, and plays both the controller
// behind it and an LE boot mouse on the far side of the link.
//
// WHAT IT DOES NOT SHARE WITH THE CODE UNDER TEST. Its pairing responder, its AES, its CMAC and its
// three SMP functions are `drivers::bt_peer`'s own - a separate implementation held to the same
// published vectors - so a host and a fixture that agree are two implementations agreeing, not one
// implementation agreeing with itself.
//
// WHAT IT PRINTS, AND WHY THOSE LINES. The gate proves that a key used after a cold reboot is the key a
// pairing produced before it, and that a reconnect did not pair again. Neither can be taken from the
// host's own account, so the fixture reports them: every completed pairing, every encryption and the
// key it used - as a FINGERPRINT, never the key.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use drivers::bt_peer::{self, Peer};
use drivers::common;
use proto::system::{Error, HciAttachment, HciControlEvent, HciControlKind, HciPacket, HciPacketKind, hci_transport};
use rt::*;

// The one publication this driver makes, and its name.
const HCI_TOKEN: u16 = 0;
const PUBLICATION_NAME: &[u8] = b"org.libersystem.bt-fixture";
// The transport version this fixture speaks.
const VERSION: u32 = 1;
// The emulated controller's own identity address, most significant first.
const CONTROLLER_ADDRESS: [u8; 6] = [0x00, 0x1b, 0xdc, 0x00, 0x00, 0x01];
// The one link handle this controller hands out.
const LINK: u16 = 0x0040;
// One report step every 200 ms: slow enough to read in a log, fast enough that a gate waiting a few
// seconds sees the whole script.
const REPORT_TICKS: u64 = 20;
// ACL buffers this controller advertises, and their size.
const ACL_BUFFERS: u8 = 4;
const ACL_BYTES: u16 = 251;

// The emulated controller's public key, as the wire carries it.
fn controller_public_key() -> [u8; 64] {
	let mut key = [0u8; 64];
	for (i, byte) in key.iter_mut().enumerate() {
		*byte = 0x20 ^ (i as u8).wrapping_mul(3);
	}
	key
}

struct Fixture {
	epoch: u32,
	packets: u64,
	control: u64,
	scanning: bool,
	connected: bool,
	encrypted: bool,
	peer: Peer,
	step: u32,
}

// ------------------------------------------------------------------ what the gate reads

fn hex32(value: u32) -> [u8; 8] {
	let digits = b"0123456789abcdef";
	let mut out = [0u8; 8];
	for (i, slot) in out.iter_mut().enumerate() {
		*slot = digits[((value >> (28 - 4 * i)) & 0xf) as usize];
	}
	out
}

fn report_line(what: &[u8], fingerprint: u32) {
	let mut line = Vec::with_capacity(96);
	line.extend_from_slice(b"bt-fixture: ");
	line.extend_from_slice(what);
	line.extend_from_slice(b" key ");
	line.extend_from_slice(&hex32(fingerprint));
	line.extend_from_slice(b"\n");
	print(&line);
}

// ------------------------------------------------------------------ the controller

impl Fixture {
	fn emit(&self, kind: HciPacketKind, bytes: Vec<u8>) {
		if self.packets == 0 {
			return;
		}
		let packet = HciPacket { kind, epoch: self.epoch, bytes };
		let mut frame = [0u8; 4200];
		let mut handles = wire::Handles::new();
		if let Some(len) = hci_transport::receive_frame(0, &packet, &mut frame, &mut handles) {
			let _ = try_send_outcome(self.packets, &frame[..len], 0);
		}
	}

	fn event(&self, code: u8, params: &[u8]) {
		let mut bytes = alloc::vec![code, params.len() as u8];
		bytes.extend_from_slice(params);
		self.emit(HciPacketKind::Event, bytes);
	}

	fn complete(&self, opcode: u16, params: &[u8]) {
		let mut body = alloc::vec![1];
		body.extend_from_slice(&opcode.to_le_bytes());
		body.extend_from_slice(params);
		self.event(0x0e, &body);
	}

	fn status(&self, opcode: u16, status: u8) {
		let op = opcode.to_le_bytes();
		self.event(0x0f, &[status, 1, op[0], op[1]]);
	}

	fn meta(&self, subevent: u8, params: &[u8]) {
		let mut body = alloc::vec![subevent];
		body.extend_from_slice(params);
		self.event(0x3e, &body);
	}

	// L2CAP data to the host, on the one link.
	fn l2cap(&self, cid: u16, payload: &[u8]) {
		let mut pdu = Vec::with_capacity(4 + payload.len());
		pdu.extend_from_slice(&(payload.len() as u16).to_le_bytes());
		pdu.extend_from_slice(&cid.to_le_bytes());
		pdu.extend_from_slice(payload);
		let mut acl = Vec::with_capacity(4 + pdu.len());
		acl.extend_from_slice(&(LINK | (0b10 << 12)).to_le_bytes());
		acl.extend_from_slice(&(pdu.len() as u16).to_le_bytes());
		acl.extend_from_slice(&pdu);
		self.emit(HciPacketKind::Acl, acl);
	}

	fn command(&mut self, bytes: &[u8]) {
		if bytes.len() < 3 || bytes.len() != 3 + bytes[2] as usize {
			return;
		}
		let opcode = u16::from_le_bytes([bytes[0], bytes[1]]);
		let params = &bytes[3..];
		match opcode {
			// A RESET ENDS WHAT THE LINK LAYER WAS DOING - the connection and the scan, with no disconnection
			// event - which is what a host powering the radio off relies on.
			0x0c03 => {
				self.scanning = false;
				self.connected = false;
				self.encrypted = false;
				self.complete(opcode, &[0]);
			}
			0x0c01 | 0x2001 | 0x200b => self.complete(opcode, &[0]),
			0x1009 => {
				let mut out = alloc::vec![0];
				let mut wire = CONTROLLER_ADDRESS;
				wire.reverse();
				out.extend_from_slice(&wire);
				self.complete(opcode, &out);
			}
			0x1002 => {
				let mut out = alloc::vec![0u8; 65];
				// Octet 34, bits 1 and 2: the two P-256 commands Secure Connections needs.
				out[1 + 34] = 0b0000_0110;
				self.complete(opcode, &out);
			}
			0x2002 => {
				let size = ACL_BYTES.to_le_bytes();
				self.complete(opcode, &[0, size[0], size[1], ACL_BUFFERS]);
			}
			0x200c => {
				self.scanning = params.first() == Some(&1);
				self.complete(opcode, &[0]);
				// EVERY SCAN HEARS THE MOUSE while nothing is connected to it: a peripheral advertises until a
				// central connects, and each scan the host enables is a new listener with empty results.
				if self.scanning && !self.connected {
					self.advertise();
				}
			}
			0x200d if params.len() == 25 => {
				self.status(opcode, 0);
				// The host asked for this fixture's mouse or for nothing this controller can reach.
				let mut wire = [0u8; 6];
				wire.copy_from_slice(&params[6..12]);
				wire.reverse();
				if wire != bt_peer::PEER_ADDRESS || self.connected {
					let mut failed = alloc::vec![0x02, 0, 0, 0, params[5]];
					failed.extend_from_slice(&params[6..12]);
					failed.extend_from_slice(&[0; 7]);
					self.meta(0x01, &failed);
					return;
				}
				self.connected = true;
				self.encrypted = false;
				let mut host = [0u8; 7];
				host[1..].copy_from_slice(&CONTROLLER_ADDRESS);
				self.peer.connected(host);
				let mut done = alloc::vec![0];
				done.extend_from_slice(&LINK.to_le_bytes());
				done.push(0x00);
				done.push(bt_peer::PEER_KIND);
				let mut peer_wire = bt_peer::PEER_ADDRESS;
				peer_wire.reverse();
				done.extend_from_slice(&peer_wire);
				done.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0xa4, 0x01, 0x00]);
				self.meta(0x01, &done);
			}
			0x200e => self.complete(opcode, &[0]),
			0x0406 if params.len() == 3 => {
				self.status(opcode, 0);
				if self.connected {
					self.connected = false;
					self.encrypted = false;
					self.event(0x05, &[0, params[0], params[1], 0x16]);
				}
			}
			0x2025 => {
				self.status(opcode, 0);
				let mut out = alloc::vec![0];
				out.extend_from_slice(&controller_public_key());
				self.meta(0x08, &out);
			}
			0x2026 if params.len() == 64 => {
				self.status(opcode, 0);
				// The peer's key, as the host relayed it, must be the one this fixture's peer sent: a
				// controller refuses a point it was not given - which is how a corrupted key exchange
				// shows up rather than as a pairing that fails at the check value.
				let status = if params == bt_peer::peer_public_key() { 0 } else { 0x12 };
				let mut out = alloc::vec![status];
				let mut wire = bt_peer::DHKEY;
				wire.reverse();
				out.extend_from_slice(&wire);
				self.meta(0x09, &out);
			}
			0x2019 if params.len() == 28 => {
				self.status(opcode, 0);
				let mut key = [0u8; 16];
				key.copy_from_slice(&params[12..28]);
				key.reverse();
				self.encrypt(key);
			}
			// A command this controller does not have.
			_ => self.complete(opcode, &[0x01]),
		}
	}

	fn advertise(&mut self) {
		// Connectable, random, the mouse's address; a name and the human-interface service.
		let mut report = alloc::vec![1, 0x00, bt_peer::PEER_KIND];
		let mut wire = bt_peer::PEER_ADDRESS;
		wire.reverse();
		report.extend_from_slice(&wire);
		let data: &[u8] = &[0x0e, 0x09, b'f', b'i', b'x', b't', b'u', b'r', b'e', b' ', b'm', b'o', b'u', b's', b'e', 0x03, 0x03, 0x12, 0x18];
		report.push(data.len() as u8);
		report.extend_from_slice(data);
		report.push(0xc4);
		self.meta(0x02, &report);
	}

	// ENCRYPTION WITH THE KEY THE HOST PRESENTED, and the one line a gate reads to tell a pairing from a
	// reconnect from a cold reboot. A key this peer holds must match; a peer that holds none - which is
	// what a fresh fixture after a cold reboot is, since an emulated mouse has no flash - accepts the key
	// it is given and says so, so the gate can compare its fingerprint with the one the first boot's
	// pairing printed. That comparison, and not this acceptance, is the proof.
	fn encrypt(&mut self, key: [u8; 16]) {
		let fingerprint = bt_peer::fingerprint(&key);
		let accepted = match self.peer.ltk {
			Some(held) if held == key => {
				report_line(b"encryption with a remembered", fingerprint);
				true
			}
			Some(_) => {
				report_line(b"encryption REFUSED for an unknown", fingerprint);
				false
			}
			None => {
				report_line(b"encryption with a key this boot never paired,", fingerprint);
				self.peer.ltk = Some(key);
				true
			}
		};
		self.encrypted = accepted;
		let handle = LINK.to_le_bytes();
		self.event(0x08, &[if accepted { 0 } else { 0x06 }, handle[0], handle[1], u8::from(accepted)]);
	}

	fn acl(&mut self, bytes: &[u8]) {
		if bytes.len() < 8 {
			return;
		}
		let len = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
		if bytes.len() != 4 + len {
			return;
		}
		// The buffer comes straight back: this controller transmits instantly.
		let handle = LINK.to_le_bytes();
		self.event(0x13, &[1, handle[0], handle[1], 1, 0]);
		let pdu = &bytes[4..];
		let payload_len = u16::from_le_bytes([pdu[0], pdu[1]]) as usize;
		let cid = u16::from_le_bytes([pdu[2], pdu[3]]);
		if pdu.len() != 4 + payload_len {
			return;
		}
		let payload = &pdu[4..];
		match cid {
			bt_peer::SMP_CID => {
				let answers = self.peer.smp(payload);
				// A PAIRING HAS COMPLETED when the host's DHKey check is answered with this peer's own. The
				// key exists from the random exchange on, so its appearance is not the moment; the check is.
				let completed = payload.first() == Some(&0x0d) && answers.iter().any(|answer| answer.first() == Some(&0x0d));
				for answer in answers {
					self.l2cap(bt_peer::SMP_CID, &answer);
				}
				if completed && let Some(ltk) = self.peer.ltk {
					report_line(b"paired;", bt_peer::fingerprint(&ltk));
				}
			}
			bt_peer::ATT_CID => {
				if let Some(answer) = self.peer.att(payload, self.encrypted) {
					self.l2cap(bt_peer::ATT_CID, &answer);
				}
			}
			_ => {}
		}
	}

	fn tick(&mut self) {
		if !self.connected || !self.encrypted {
			return;
		}
		if let Some(notification) = self.peer.report(self.step) {
			self.l2cap(bt_peer::ATT_CID, &notification);
			self.step = self.step.wrapping_add(1);
		}
	}
}

impl hci_transport::Service for Fixture {
	fn attach(&mut self, version: u32) -> Result<HciAttachment, Error> {
		if version != VERSION {
			return Err(Error::Unsupported);
		}
		Ok(HciAttachment { version, iso: false, max_command: 258, max_event: 257, max_acl: ACL_BYTES as u32 + 4, max_iso: 0, command_credits: 1, acl_credits: ACL_BUFFERS as u32, acl_queue: 16, epoch: self.epoch })
	}

	fn send(&mut self, kind: HciPacketKind, bytes: Vec<u8>) -> Result<u32, Error> {
		match kind {
			HciPacketKind::Command => self.command(&bytes),
			HciPacketKind::Acl => self.acl(&bytes),
			HciPacketKind::Event => return Err(Error::Invalid),
			HciPacketKind::Iso => return Err(Error::Unsupported),
		}
		Ok(bytes.len() as u32)
	}

	fn receive(&mut self) -> Vec<HciPacket> {
		Vec::new()
	}

	fn control(&mut self) -> Vec<HciControlEvent> {
		Vec::new()
	}

	fn reset(&mut self) -> Result<u32, Error> {
		let ended = self.epoch;
		self.epoch = self.epoch.wrapping_add(1);
		self.scanning = false;
		self.connected = false;
		self.encrypted = false;
		if self.control != 0 {
			let event = HciControlEvent { kind: HciControlKind::Reset, epoch: ended };
			let mut frame = [0u8; 64];
			let mut handles = wire::Handles::new();
			if let Some(len) = hci_transport::control_frame(0, &event, &mut frame, &mut handles) {
				let _ = try_send_outcome(self.control, &frame[..len], 0);
			}
		}
		Ok(self.epoch)
	}
}

// One request from the consumer. The two streams are opened here, with the producer kept.
fn serve(fixture: &mut Fixture, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	if len < 2 {
		return true;
	}
	let op = u16::from_le_bytes([buf[0], buf[1]]);
	if op == hci_transport::OP_RECEIVE || op == hci_transport::OP_CONTROL {
		let corr = if op == hci_transport::OP_RECEIVE { hci_transport::receive_open(fixture, &buf[..len], &mut handles).map(|(corr, _)| corr) } else { hci_transport::control_open(fixture, &buf[..len], &mut handles).map(|(corr, _)| corr) };
		let Some(corr) = corr else { return true };
		let Some((producer, consumer)) = channel_with_depth(256) else { return true };
		let slot = if op == hci_transport::OP_RECEIVE { &mut fixture.packets } else { &mut fixture.control };
		if *slot != 0 {
			close(*slot);
		}
		*slot = producer;
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	let mut reply = [0u8; 512];
	let mut reply_handles = wire::Handles::new();
	if let Some(written) = hci_transport::dispatch(fixture, &buf[..len], &mut handles, &mut reply, &mut reply_handles) {
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
	}
	true
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = common::handshake(bootstrap);
	let Some((first, first_far)) = channel() else { exit() };
	let timer: u64 = match timer_create() {
		t if t > 0 => t as u64,
		_ => exit(),
	};
	timer_set(timer, clock().saturating_add(REPORT_TICKS));
	common::online_named(bootstrap, &bind, b"driver.bt-fixture: online (an emulated controller and an LE boot mouse)", &[(driver_protocol::provider::BLUETOOTH_HCI, first_far, PUBLICATION_NAME)]);
	let mut serving = common::Serving::from_offers(&[(HCI_TOKEN, first)]);
	let mut fixture = Fixture { epoch: 1, packets: 0, control: 0, scanning: false, connected: false, encrypted: false, peer: Peer::new(), step: 0 };
	let mut buf = alloc::vec![0u8; 4400];
	loop {
		match common::wait_providers_or_answer(bootstrap, &bind, &mut serving, &[timer]) {
			None => {
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, 0, true);
				}
				exit();
			}
			Some(common::ProviderReady::Connected(_)) => {}
			Some(common::ProviderReady::Consumer(index)) => {
				if !serve(&mut fixture, serving.at(index), &mut buf) {
					let token = serving.close_at(index);
					// The consumer is gone: the link and the streams go with it.
					fixture.connected = false;
					fixture.encrypted = false;
					for stream in [&mut fixture.packets, &mut fixture.control] {
						if *stream != 0 {
							close(*stream);
							*stream = 0;
						}
					}
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
				}
			}
			Some(common::ProviderReady::Device(_)) => {
				fixture.tick();
				timer_set(timer, clock().saturating_add(REPORT_TICKS));
			}
		}
	}
}
