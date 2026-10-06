// bt_fixture - the in-guest Bluetooth controller AND the devices on the other end of its radio: an LE
// boot mouse, five BR/EDR devices (`drivers::bt_world`), the LE world's peripherals (`drivers::bt_le_world`)
// and its LE Audio devices - two earbuds of one coordinated set and a broadcast source (`drivers::bt_le_audio`).
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
//
// THE CONTROL ENDPOINT. A gate's probe reaches the fixture through a `fixture-control` publication, which only
// a development probe's policy row grants: it makes a device act - page this host, pair from its side, open a
// channel, type a passkey - and reads what the far side saw, line by line, as `bt-fixture:` prints it.
//
// AN LE AUDIO CONTROLLER. It has extended advertising, periodic advertising, the CIS central role and the
// synchronized receiver role, and keeps the rule real ones keep: a host that used an extended command is refused
// the legacy scan and initiation until it resets. Its ISO data comes in as `iso` packets and goes out the way a USB
// controller's bulk IN pipe delivers it - as `acl` packets carrying the ISO header, which the host tells apart by
// handle.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use drivers::bt_le_audio::{self, Advert};
use drivers::bt_le_world::{self, LeOut, LeWorld};
use drivers::bt_peer::{self, Peer};
use drivers::bt_world::{self, World};
use drivers::common;
use proto::system::{Error, FixtureAction, HciAttachment, HciControlEvent, HciControlKind, HciPacket, HciPacketKind, bluetooth_fixture, hci_transport};
use rt::*;

// The two publications this driver makes, and their names.
const HCI_TOKEN: u16 = 0;
const CONTROL_TOKEN: u16 = 1;
const PUBLICATION_NAME: &[u8] = b"org.libersystem.bt-fixture";
const CONTROL_NAME: &[u8] = b"org.libersystem.bt-fixture.control";
// The LE mouse's number on the control endpoint; the BR/EDR devices follow it.
const MOUSE: u8 = 1;
// Lines the control endpoint holds for a probe that has not read them.
const EVENT_LINES: usize = 256;
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
	world: World,
	le: LeWorld,
	// The host's private address, the controller's filter accept list and resolving list, and a connection attempt
	// through the list waiting for a device on it to be there - with the host's own address type.
	host_random: Option<[u8; 6]>,
	accept: Vec<(u8, [u8; 6])>,
	resolving: Vec<[u8; 6]>,
	resolution: bool,
	accepting: Option<u8>,
	// What the far side saw and the probe has not read yet.
	events: VecDeque<String>,
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

	// What the BR/EDR world sends, and what it saw - printed, and kept for the control endpoint.
	fn world_out(&mut self, outs: Vec<bt_world::Out>) {
		for out in outs {
			match out {
				bt_world::Out::Event(bytes) => self.emit(HciPacketKind::Event, bytes),
				bt_world::Out::Acl(bytes) => self.emit(HciPacketKind::Acl, bytes),
				bt_world::Out::Sco(bytes) => self.emit(HciPacketKind::Sco, bytes),
			}
		}
		for line in self.world.take_log() {
			self.note(line);
		}
		// A DUAL-MODE DEVICE'S HALVES SHARE WHAT ONE DERIVES: the LE key its BR/EDR half made, to its LE half.
		if let Some((address, ltk)) = self.world.take_derived() {
			self.le.set_ltk(&address, ltk);
		}
	}

	fn le_out(&mut self, outs: Vec<LeOut>) {
		for out in outs {
			match out {
				LeOut::Event(bytes) => self.emit(HciPacketKind::Event, bytes),
				LeOut::Acl(bytes) => self.emit(HciPacketKind::Acl, bytes),
				// ISO DATA TO THE HOST travels as a USB controller's bulk IN pipe carries it: beside ACL, told apart by handle.
				LeOut::Iso(bytes) => self.emit(HciPacketKind::Acl, bytes),
			}
		}
		for line in self.le.take_log() {
			self.note(line);
		}
		// And the BR/EDR key its LE half made, to its BR/EDR half.
		if let Some((address, key, authenticated)) = self.le.take_derived() {
			self.world.set_key(&address, key, authenticated);
		}
	}

	// The host's address on a new LE link, as the derivations take it: its private one when it connects from it.
	fn host_address(&self, own: u8) -> [u8; 7] {
		let mut host = [0u8; 7];
		match (own, self.host_random) {
			(1, Some(random)) => {
				host[0] = 1;
				host[1..].copy_from_slice(&random);
			}
			_ => host[1..].copy_from_slice(&CONTROLLER_ADDRESS),
		}
		host
	}

	fn connect_mouse(&mut self, host: [u8; 7]) {
		self.connected = true;
		self.encrypted = false;
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

	// A CONNECTION THROUGH THE ACCEPT LIST, if a device on it is there: the mouse, or one of the LE world's - named by
	// its identity where the resolving list resolves its private address.
	fn try_accept(&mut self) {
		let Some(own) = self.accepting else { return };
		let host = self.host_address(own);
		if !self.connected && self.accept.iter().any(|(_, address)| *address == bt_peer::PEER_ADDRESS) {
			self.accepting = None;
			self.connect_mouse(host);
			return;
		}
		if let Some(at) = self.le.listed(&self.accept) {
			self.accepting = None;
			let identity = bt_le_world::LE_DEVICES[at].identity;
			let resolved = self.resolution && self.resolving.contains(&identity);
			let outs = self.le.connect(at, host, resolved);
			self.le_out(outs);
		}
	}

	fn note(&mut self, line: String) {
		let mut printed = Vec::with_capacity(line.len() + 13);
		printed.extend_from_slice(b"bt-fixture: ");
		printed.extend_from_slice(line.as_bytes());
		printed.push(b'\n');
		print(&printed);
		if self.events.len() >= EVENT_LINES {
			self.events.pop_front();
		}
		self.events.push_back(line);
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
		// A DISCONNECT OF A BR/EDR LINK, and every other BR/EDR command, is the world's.
		let le_disconnect = opcode == 0x0406 && params.len() == 3 && u16::from_le_bytes([params[0], params[1]]) & 0x0fff == LINK;
		if !le_disconnect && let Some(outs) = self.world.command(opcode, params) {
			self.world_out(outs);
			return;
		}
		// THE LE AUDIO HALF: its features and buffers, its groups, streams, data paths and syncs - and the extended-mode
		// rule, which refuses the legacy scan and initiation here once the host has gone extended.
		if let Some(outs) = self.le.command(opcode, params) {
			self.le_out(outs);
			return;
		}
		match opcode {
			// A RESET ENDS WHAT THE LINK LAYER WAS DOING - the connections and the scans, with no disconnection
			// event - which is what a host powering the radio off relies on.
			0x0c03 => {
				self.scanning = false;
				self.connected = false;
				self.encrypted = false;
				self.world.reset();
				// THE CONTROLLER'S KIND the gate chose - LE Audio or plain LE - takes effect here, at the host's reset, and the
				// fixture says so.
				self.le.hci_reset();
				self.le_out(Vec::new());
				self.accepting = None;
				self.resolution = false;
				self.complete(opcode, &[0]);
			}
			0x0c01 | 0x2001 | 0x200b => self.complete(opcode, &[0]),
			// THE HOST'S PRIVATE ADDRESS, and the two lists a bonded peripheral reconnects through.
			0x2005 if params.len() == 6 => {
				let mut address = [0u8; 6];
				address.copy_from_slice(params);
				address.reverse();
				self.host_random = Some(address);
				self.note(alloc::format!("the host's private address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", address[0], address[1], address[2], address[3], address[4], address[5]));
				self.complete(opcode, &[0]);
			}
			0x2010 => {
				self.accept.clear();
				self.complete(opcode, &[0]);
			}
			0x2011 if params.len() == 7 => {
				let mut address = [0u8; 6];
				address.copy_from_slice(&params[1..]);
				address.reverse();
				self.accept.push((params[0], address));
				self.complete(opcode, &[0]);
			}
			0x2029 => {
				self.resolving.clear();
				self.complete(opcode, &[0]);
			}
			0x2027 if params.len() == 39 => {
				let mut address = [0u8; 6];
				address.copy_from_slice(&params[1..7]);
				address.reverse();
				self.resolving.push(address);
				self.complete(opcode, &[0]);
			}
			0x202d => {
				self.resolution = params.first() == Some(&1);
				self.complete(opcode, &[0]);
			}
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
				if self.scanning {
					let reports = self.le.advertise();
					self.le_out(reports);
				}
			}
			0x200d if params.len() == 25 => {
				self.status(opcode, 0);
				self.initiate(params[4] == 0x01, params[12], params[5], &params[6..12]);
			}
			// THE EXTENDED SCAN: its parameters, and each enable a new listener that hears every advertiser there is - the
			// mouse while nothing is connected to it.
			0x2041 if self.le.le_audio() => {
				let status = self.le.extended_scan_parameters(params);
				self.complete(opcode, &[status]);
			}
			0x2042 if self.le.le_audio() => {
				let (status, began) = self.le.extended_scan_enable(params, clock());
				self.complete(opcode, &[status]);
				if began {
					if !self.connected {
						self.meta(0x0d, &mouse_advert().extended_report());
					}
					let reports = self.le.extended_reports();
					self.le_out(reports);
				}
			}
			// LE EXTENDED CREATE CONNECTION, version 1: the filter policy, the own and peer address, the initiating PHYs and
			// sixteen octets for each - on the air what the legacy one does.
			0x2043 if self.le.le_audio() => {
				let phys = params.get(9).copied().unwrap_or(0);
				if params.len() < 10 || phys == 0 || phys & !0x07 != 0 || params.len() != 10 + 16 * phys.count_ones() as usize {
					self.status(opcode, 0x12);
					return;
				}
				self.status(opcode, 0);
				self.initiate(params[0] == 0x01, params[1], params[2], &params[3..9]);
			}
			0x200e => {
				self.complete(opcode, &[0]);
				// A WAITING ATTEMPT CANCELLED ends with its completion, unknown connection.
				if self.accepting.take().is_some() {
					self.meta(0x01, &[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
				}
			}
			0x0406 if params.len() == 3 => {
				self.status(opcode, 0);
				let handle = u16::from_le_bytes([params[0], params[1]]) & 0x0fff;
				if self.connected && handle == LINK {
					self.connected = false;
					self.encrypted = false;
					self.event(0x05, &[0, params[0], params[1], 0x16]);
				}
				if let Some(outs) = self.le.disconnect(handle, true) {
					self.le_out(outs);
				}
				self.try_accept();
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
				let (status, mut wire) = if params == bt_peer::peer_public_key() {
					(0, bt_peer::DHKEY)
				} else {
					match self.le.dhkey_for(params) {
						Some(dhkey) => (0, dhkey),
						None => (0x12, [0; 32]),
					}
				};
				let mut out = alloc::vec![status];
				wire.reverse();
				out.extend_from_slice(&wire);
				self.meta(0x09, &out);
			}
			0x2019 if params.len() == 28 => {
				self.status(opcode, 0);
				let mut key = [0u8; 16];
				key.copy_from_slice(&params[12..28]);
				key.reverse();
				let handle = u16::from_le_bytes([params[0], params[1]]) & 0x0fff;
				if handle == LINK {
					self.encrypt(key);
				} else {
					let mut rand = [0u8; 8];
					rand.copy_from_slice(&params[2..10]);
					let ediv = u16::from_le_bytes([params[10], params[11]]);
					if let Some(outs) = self.le.encrypt(handle, rand, ediv, key) {
						self.le_out(outs);
					}
				}
			}
			// A command this controller does not have.
			_ => self.complete(opcode, &[0x01]),
		}
	}

	fn advertise(&mut self) {
		self.meta(0x02, &mouse_advert().legacy_report());
	}

	// A CONNECTION THE HOST INITIATES - legacy or extended, the same on the air: through the accept list, whichever
	// device on it is there or the first that comes back; or to the device it names by the address it advertises.
	fn initiate(&mut self, accept_list: bool, own: u8, peer_type: u8, peer: &[u8]) {
		if accept_list {
			self.accepting = Some(own);
			self.try_accept();
			return;
		}
		// The host asked for a device by the address it advertises, or for nothing this controller can reach.
		let mut wire = [0u8; 6];
		wire.copy_from_slice(peer);
		wire.reverse();
		let host = self.host_address(own);
		if wire == bt_peer::PEER_ADDRESS && !self.connected {
			self.connect_mouse(host);
			return;
		}
		if let Some(at) = self.le.named(&wire) {
			let outs = self.le.connect(at, host, false);
			self.le_out(outs);
			return;
		}
		let mut failed = alloc::vec![0x02, 0, 0, 0, peer_type];
		failed.extend_from_slice(peer);
		failed.extend_from_slice(&[0; 7]);
		self.meta(0x01, &failed);
	}

	// ONE ISO DATA PACKET FROM THE HOST: an SDU for an earbud's sink, its buffer straight back.
	fn iso(&mut self, bytes: &[u8]) {
		let outs = self.le.iso(bytes);
		self.le_out(outs);
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
		if bytes.len() < 5 {
			return;
		}
		let len = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
		if bytes.len() != 4 + len {
			return;
		}
		let header = u16::from_le_bytes([bytes[0], bytes[1]]);
		if self.le.owns(header & 0x0fff) {
			if let Some(outs) = self.le.acl(header & 0x0fff, &bytes[4..]) {
				self.le_out(outs);
			}
			return;
		}
		if header & 0x0fff != LINK {
			if let Some(outs) = self.world.acl(header & 0x0fff, ((header >> 12) & 0b11) as u8, &bytes[4..]) {
				self.world_out(outs);
			}
			return;
		}
		if bytes.len() < 8 {
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
		// THE KEYBOARD'S SCRIPT, at its time, whatever the mouse is doing.
		let typed = self.world.tick(clock());
		self.world_out(typed);
		// THE LE AUDIO STREAMS toward the host, the periodic train's reports, an extended scan's end.
		let audio = self.le.tick(clock());
		self.le_out(audio);
		if !self.connected || !self.encrypted {
			return;
		}
		if let Some(notification) = self.peer.report(self.step) {
			self.l2cap(bt_peer::ATT_CID, &notification);
			self.step = self.step.wrapping_add(1);
		}
	}
}

// THE MOUSE AS A SCAN HEARS IT: connectable, random, its address; a name and the human-interface service.
fn mouse_advert() -> Advert {
	let data: &[u8] = &[0x0e, 0x09, b'f', b'i', b'x', b't', b'u', b'r', b'e', b' ', b'm', b'o', b'u', b's', b'e', 0x03, 0x03, 0x12, 0x18];
	Advert::legacy(bt_peer::PEER_KIND, bt_peer::PEER_ADDRESS, -60, data)
}

impl hci_transport::Service for Fixture {
	fn attach(&mut self, version: u32) -> Result<HciAttachment, Error> {
		if version != VERSION {
			return Err(Error::Unsupported);
		}
		// THE LARGER OF THE TWO RADIOS' BUFFERS bounds a packet on the transport: BR/EDR's. An ISO packet holds its header,
		// a time stamp, the SDU header and the largest SDU this controller takes.
		Ok(HciAttachment { version, iso: true, max_command: 258, max_event: 257, max_acl: bt_world::ACL_BYTES as u32 + 4, max_iso: 4 + 4 + 4 + bt_le_audio::ISO_BYTES as u32, command_credits: 1, acl_credits: ACL_BUFFERS as u32, acl_queue: 16, epoch: self.epoch, sco: true, max_sco: 64 })
	}

	fn send(&mut self, kind: HciPacketKind, bytes: Vec<u8>) -> Result<u32, Error> {
		match kind {
			HciPacketKind::Command => self.command(&bytes),
			HciPacketKind::Acl => self.acl(&bytes),
			HciPacketKind::Event => return Err(Error::Invalid),
			HciPacketKind::Sco => self.world.sco_in(&bytes),
			HciPacketKind::Iso => self.iso(&bytes),
		}
		Ok(bytes.len() as u32)
	}

	fn receive(&mut self) -> Vec<HciPacket> {
		Vec::new()
	}

	fn control(&mut self) -> Vec<HciControlEvent> {
		Vec::new()
	}

	// VOICE: the headset's synchronous link carries what the world makes of it; the setting is the host's, and the
	// world's packets are whole either way.
	fn voice(&mut self, _channels: u8, _bits: u8, _wideband: bool) -> Result<u8, Error> {
		Ok(1)
	}

	fn reset(&mut self) -> Result<u32, Error> {
		let ended = self.epoch;
		self.epoch = self.epoch.wrapping_add(1);
		self.scanning = false;
		self.connected = false;
		self.encrypted = false;
		self.world.reset();
		self.le.reset();
		self.accepting = None;
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

// ------------------------------------------------------------------ the control endpoint

struct ControlView<'a> {
	fixture: &'a mut Fixture,
}

impl bluetooth_fixture::Service for ControlView<'_> {
	fn act(&mut self, peer: u8, action: FixtureAction, argument: u32) -> Result<u32, Error> {
		let fixture = &mut *self.fixture;
		// THE CONTROLLER ITSELF, peer 0: the kind it is from its next reset - an LE Audio one (1) or a plain LE one (0).
		if peer == 0 {
			if action != FixtureAction::LeFeatures || argument > 1 {
				return Err(if action == FixtureAction::LeFeatures { Error::Invalid } else { Error::Unsupported });
			}
			fixture.le.choose_kind(argument == 1);
			for line in fixture.le.take_log() {
				fixture.note(line);
			}
			return Ok(0);
		}
		if peer == MOUSE {
			// THE MOUSE does two things on the gate's word: forget its key, and drop its link.
			match action {
				FixtureAction::Forget => {
					fixture.peer.ltk = None;
					fixture.note(String::from("mouse forgot its key"));
				}
				FixtureAction::Disconnect if fixture.connected => {
					fixture.connected = false;
					fixture.encrypted = false;
					let handle = LINK.to_le_bytes();
					fixture.event(0x05, &[0, handle[0], handle[1], 0x13]);
				}
				_ => return Err(Error::Unsupported),
			}
			return Ok(0);
		}
		// THE LE WORLD'S DEVICES: a passkey typed, a link dropped, a key forgotten, the host's name read - and an earbud's
		// own volume and its call buttons.
		if bt_le_world::LeWorld::is_device(peer) {
			let earbud = bt_le_world::LeWorld::is_earbud(peer);
			let outs = match action {
				FixtureAction::TypePasskey => fixture.le.type_passkey(peer, argument),
				FixtureAction::Disconnect => fixture.le.act_disconnect(peer),
				FixtureAction::Forget => fixture.le.forget(peer).then(Vec::new),
				FixtureAction::ReadHostName if !earbud => fixture.le.read_host_name(peer),
				FixtureAction::LeVolume if earbud => fixture.le.earbud_volume(peer, u8::try_from(argument).map_err(|_| Error::Invalid)?),
				FixtureAction::LeCall if earbud => {
					let Some((called, outs)) = fixture.le.earbud_call(peer, u8::try_from(argument).map_err(|_| Error::Invalid)?) else { return Err(Error::Unsupported) };
					fixture.le_out(outs);
					return Ok(if called { 0 } else { 0x02 });
				}
				_ => return Err(Error::Unsupported),
			};
			let Some(outs) = outs else { return Ok(0x02) };
			fixture.le_out(outs);
			fixture.try_accept();
			return Ok(0);
		}
		// THE BROADCAST SOURCE starts in the clear or encrypted, or stops.
		if peer == bt_le_audio::BROADCAST_SOURCE {
			if action != FixtureAction::Broadcast {
				return Err(Error::Unsupported);
			}
			let outs = u8::try_from(argument).ok().and_then(|mode| fixture.le.broadcast(mode)).ok_or(Error::Invalid)?;
			fixture.le_out(outs);
			return Ok(0);
		}
		// A DUAL-MODE DEVICE'S LE HALF advertises or stops; and a reset forgets on both radios.
		if action == FixtureAction::LeAdvertise {
			let outs = fixture.le.set_advertising(peer, argument != 0).ok_or(Error::Unsupported)?;
			fixture.le_out(outs);
			if argument != 0 && fixture.scanning {
				let reports = fixture.le.advertise();
				fixture.le_out(reports);
			}
			if argument != 0 && fixture.le.scanning_extended() {
				let reports = fixture.le.extended_reports();
				fixture.le_out(reports);
			}
			fixture.try_accept();
			return Ok(0);
		}
		if action == FixtureAction::Forget && fixture.world.is_dual(peer).is_some() {
			fixture.le.forget_dual(peer);
		}
		let code = match action {
			FixtureAction::Page => 1,
			FixtureAction::OpenPsm => 2,
			FixtureAction::Pair => 3,
			FixtureAction::KeyType => 4,
			FixtureAction::TypePasskey => 5,
			FixtureAction::SdpSearch => 6,
			FixtureAction::RfcommOpen => 7,
			FixtureAction::Disconnect => 8,
			FixtureAction::Forget => 9,
			FixtureAction::RfcommSend => 10,
			FixtureAction::CtrlAltDelete => 11,
			FixtureAction::PowerKey => 12,
			FixtureAction::HidConnect => 13,
			FixtureAction::GamepadReport => 14,
			FixtureAction::Stream => 15,
			FixtureAction::Volume => 16,
			FixtureAction::PressPlay => 17,
			FixtureAction::HfpConnect => 18,
			FixtureAction::Answer => 19,
			FixtureAction::HangUp => 20,
			FixtureAction::AudioRequest => 21,
			FixtureAction::PushObject => 22,
			FixtureAction::ReadHostName | FixtureAction::LeAdvertise | FixtureAction::Broadcast | FixtureAction::LeVolume | FixtureAction::LeCall | FixtureAction::LeFeatures => return Err(Error::Unsupported),
		};
		match fixture.world.act(peer, code, argument) {
			Ok((result, outs)) => {
				fixture.world_out(outs);
				Ok(result)
			}
			Err(why) => {
				fixture.note(alloc::format!("refused an action: {why}"));
				Err(Error::Invalid)
			}
		}
	}

	fn events(&mut self) -> Result<Vec<String>, Error> {
		Ok(self.fixture.events.drain(..).collect())
	}

	fn type_text(&mut self, peer: u8, text: String, delay_ms: u32) -> Result<(), Error> {
		let due = clock().saturating_add(u64::from(delay_ms) / 10);
		match self.fixture.world.type_text(peer, &text, due) {
			Ok(()) => Ok(()),
			Err(why) => {
				self.fixture.note(alloc::format!("refused to type: {why}"));
				Err(Error::Invalid)
			}
		}
	}
}

// Serve one control request; false when the probe's connection closed.
fn serve_control(fixture: &mut Fixture, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	let mut reply = alloc::vec![0u8; 16 * 1024];
	let mut reply_handles = wire::Handles::new();
	let mut view = ControlView { fixture };
	if let Some(written) = bluetooth_fixture::dispatch(&mut view, &buf[..len], &mut handles, &mut reply, &mut reply_handles) {
		send_caps_blocking(channel, &reply[..written], reply_handles.as_slice());
	}
	for &leftover in handles.as_slice() {
		close(leftover);
	}
	true
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
	let (Some((first, first_far)), Some((control, control_far))) = (channel(), channel()) else { exit() };
	let timer: u64 = match timer_create() {
		t if t > 0 => t as u64,
		_ => exit(),
	};
	timer_set(timer, clock().saturating_add(REPORT_TICKS));
	common::online_named(bootstrap, &bind, b"driver.bt-fixture: online (an emulated dual-mode LE Audio controller, an LE boot mouse, five BR/EDR devices, LE peripherals, two earbuds and a broadcast source)", &[(driver_protocol::provider::BLUETOOTH_HCI, first_far, PUBLICATION_NAME), (driver_protocol::provider::FIXTURE_CONTROL, control_far, CONTROL_NAME)]);
	let mut serving = common::Serving::from_offers(&[(HCI_TOKEN, first), (CONTROL_TOKEN, control)]);
	let mut seed = [0u8; 8];
	let _ = random_get(&mut seed);
	let mut world = World::new(u64::from_le_bytes(seed));
	world.mix(&clock().to_le_bytes());
	let le = LeWorld::new(u64::from_le_bytes(seed) ^ 0x5bd1_e995);
	let mut fixture = Fixture { epoch: 1, packets: 0, control: 0, scanning: false, connected: false, encrypted: false, peer: Peer::new(), step: 0, world, le, host_random: None, accept: Vec::new(), resolving: Vec::new(), resolution: false, accepting: None, events: VecDeque::new() };
	let mut buf = alloc::vec![0u8; 4400];
	loop {
		match common::wait_providers_or_answer(bootstrap, &bind, &mut serving, &[timer]) {
			None => {
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, 0, true);
				}
				// A DRIVER THAT LEAVES SAYS WHY, so a log tells this exit from a panic - which rt ends with no word.
				print(b"bt-fixture: exiting - the manager's channel ended, or a wait on the fixture's channels failed\n");
				exit();
			}
			Some(common::ProviderReady::Connected(_)) => {}
			Some(common::ProviderReady::Consumer(index)) if serving.token_at(index) == CONTROL_TOKEN => {
				if !serve_control(&mut fixture, serving.at(index), &mut buf) {
					let token = serving.close_at(index);
					if !common::disconnected(bootstrap, &bind, token) {
						print(b"bt-fixture: exiting - the manager did not take word that the control endpoint's probe left\n");
						exit();
					}
				}
			}
			Some(common::ProviderReady::Consumer(index)) => {
				if !serve(&mut fixture, serving.at(index), &mut buf) {
					let token = serving.close_at(index);
					// The consumer is gone: the links and the streams go with it.
					fixture.connected = false;
					fixture.encrypted = false;
					fixture.world.reset();
					fixture.le.reset();
					fixture.accepting = None;
					for stream in [&mut fixture.packets, &mut fixture.control] {
						if *stream != 0 {
							close(*stream);
							*stream = 0;
						}
					}
					if !common::disconnected(bootstrap, &bind, token) {
						print(b"bt-fixture: exiting - the manager did not take word that the host left\n");
						exit();
					}
				}
			}
			Some(common::ProviderReady::Device(_)) => {
				fixture.tick();
				// A STREAM ON THE PHONE'S CLOCK, a voice link's microphone, and every LE Audio stream and train are fed every
				// tick; otherwise the reports' pace is enough.
				let next = if fixture.world.streaming() || fixture.world.voice_active() || fixture.le.audio_active() { 1 } else { REPORT_TICKS };
				timer_set(timer, clock().saturating_add(next));
			}
		}
	}
}
