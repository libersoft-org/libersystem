// THE EMULATED BR/EDR WORLD: the classic half of the in-guest Bluetooth fixture's controller, and five
// classic devices on the far side of its radio.
//
// A TEST FIXTURE, DEVELOPMENT-ONLY, AND ITS PROTOCOLS ARE ITS OWN. The host stack's L2CAP signalling,
// SDP, RFCOMM and pairing rules live in `service_logic`; nothing here reuses them. The far side's L2CAP,
// its SDP records and searches, its RFCOMM framing with its CRC and its credits are written here from the
// specification, so a host and a fixture that agree are two implementations agreeing. Where the in-guest
// fixture is the oracle - the milestone says so, in the place it applies - this is the half it is made of.
//
// WHAT THE CONTROLLER DOES HERE AND WHAT IT DOES NOT. A real controller runs Secure Simple Pairing's key
// agreement over LMP; this one plays the host-visible half of it - the IO capability exchange, the six
// digits, the passkey a keyboard types, the link key it hands back and the key type that says how it was
// made - and makes the key from the system's randomness. The host never sees the agreement itself, so
// nothing a host can check is missing; what this fixture reports is what the far side saw.
//
// THE DEVICES, numbered for the control endpoint from 2 (1 is the LE mouse):
//
//   2 phone      DisplayYesNo, Secure Connections        Numeric Comparison
//   3 keyboard   KeyboardOnly, Secure Connections        Passkey Entry, this host showing the digits
//   4 headset    NoInputNoOutput, Secure Connections     Just Works; an incoming one asks consent; an A2DP sink
//                                                        and an HFP hands-free unit (`av`, `hf`)
//   5 serial     NoInputNoOutput, P-192 only             Just Works; a serial port on RFCOMM channel 3 that
//                                                        echoes what it receives, and Object Push on channel 4
//
// THE PHONE is a network access point on BNEP's PSM, leasing the host an address on a network of its own (`nap`); and
// it takes an object pushed on its Object Push record - RFCOMM channel 12, or L2CAP's GOEP PSM 0x1021 in
// enhanced retransmission mode - and pushes one to the host on the gate's word (`opp`).
//   6 legacy     no Secure Simple Pairing                a PIN, 0000
//   7 gamepad    NoInputNoOutput, Secure Connections     Just Works; a HID gamepad of the harness's shape
//
// THE KEYBOARD IS A HID DEVICE: its SDP record carries its report descriptor - a keyboard report (id 1) and a
// consumer control report (id 2) - it answers a protocol change on its control channel, opens its control and
// interrupt channels itself when told to reconnect, and types a script on its interrupt channel at the time the
// gate asks for, `{cad}` and `{power}` in the script being Ctrl+Alt+Delete and its Power key.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

// A2DP and AVRCP: the headset's sink and remote control, the phone's stream and its remote control.
mod av;

// HFP: the headset's hands-free unit and its voice link.
mod hf;

// OBEX Object Push: the phone's and the serial device's servers, and the phone's push.
mod opp;

// PAN: the phone's network access point, and the little network behind it.
mod nap;

// ------------------------------------------------------------------ the devices

pub struct Spec {
	pub name: &'static str,
	// Most significant byte first, as the gate and `btctl` print it.
	pub address: [u8; 6],
	pub class: u32,
	// The IO capability, and the authentication requirements it states.
	pub capability: u8,
	pub authentication: u8,
	pub ssp: bool,
	pub secure_connections: bool,
	pub uuids: &'static [u16],
	pub pin: &'static [u8],
	pub serial_channel: Option<u8>,
	// A HID device's report descriptor, which its record carries.
	pub hid: Option<&'static [u8]>,
	// A DUAL-MODE DEVICE: it has the BR/EDR Security Manager, derives an LE key from a Secure Connections link key, and
	// has an LE half in `bt_le_world` under the same public address.
	pub dual: bool,
	// AN OBJECT PUSH SERVER: its RFCOMM channel, and its GOEP L2CAP PSM where its record offers one.
	pub opp: Option<(u8, Option<u16>)>,
}

pub const DEVICES: [Spec; 6] = [
	Spec { name: "fixture phone", address: [0x00, 0x1b, 0xdc, 0x20, 0x00, 0x02], class: 0x5a020c, capability: 1, authentication: 0x05, ssp: true, secure_connections: true, uuids: &[0x1105, 0x110a, 0x110c, 0x1116, 0x111f], pin: b"", serial_channel: None, hid: None, dual: true, opp: Some((12, Some(0x1021))) },
	Spec { name: "fixture keyboard", address: [0x00, 0x1b, 0xdc, 0x20, 0x00, 0x03], class: 0x002540, capability: 2, authentication: 0x05, ssp: true, secure_connections: true, uuids: &[0x1124], pin: b"", serial_channel: None, hid: Some(&KEYBOARD_DESCRIPTOR), dual: false, opp: None },
	Spec { name: "fixture headset", address: [0x00, 0x1b, 0xdc, 0x20, 0x00, 0x04], class: 0x240404, capability: 3, authentication: 0x04, ssp: true, secure_connections: true, uuids: &[0x111e, 0x1108, 0x110b, 0x110e], pin: b"", serial_channel: None, hid: None, dual: false, opp: None },
	Spec { name: "fixture serial", address: [0x00, 0x1b, 0xdc, 0x20, 0x00, 0x05], class: 0x001f00, capability: 3, authentication: 0x04, ssp: true, secure_connections: false, uuids: &[0x1101, 0x1105], pin: b"", serial_channel: Some(3), hid: None, dual: false, opp: Some((4, None)) },
	Spec { name: "fixture legacy", address: [0x00, 0x1b, 0xdc, 0x20, 0x00, 0x06], class: 0x001f00, capability: 3, authentication: 0x00, ssp: false, secure_connections: false, uuids: &[0x1101], pin: b"0000", serial_channel: Some(1), hid: None, dual: false, opp: None },
	Spec { name: "fixture gamepad", address: [0x00, 0x1b, 0xdc, 0x20, 0x00, 0x07], class: 0x000508, capability: 3, authentication: 0x04, ssp: true, secure_connections: true, uuids: &[0x1124], pin: b"", serial_channel: None, hid: Some(&GAMEPAD_DESCRIPTOR), dual: false, opp: None },
];

// The first device's number on the control endpoint.
pub const FIRST_DEVICE: u8 = 2;
// The first handle a classic link gets: after the LE mouse's 0x0040.
const FIRST_HANDLE: u16 = 0x0041;
// The ACL buffers this controller advertises for BR/EDR, and their size.
pub const ACL_BYTES: u16 = 1021;
pub const ACL_BUFFERS: u16 = 8;
// What the log holds before the oldest line is dropped.
const LOG_LINES: usize = 256;
// A received DLC's credits: given at negotiation, and topped up when half are used.
const CREDITS: u8 = 7;
// Frames a device holds unsent before it stops giving the host credits.
const BACKLOG_FRAMES: usize = 8;

// What the fixture sends the host.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Out {
	Event(Vec<u8>),
	Acl(Vec<u8>),
	Sco(Vec<u8>),
}

// ------------------------------------------------------------------ state

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Model {
	Numeric,
	Passkey(u32),
	JustWorks,
	Pin,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Purpose {
	// The host opened it.
	Host,
	// The phone's AVDTP signalling channel, and its media channel.
	Avdtp,
	AvdtpMedia,
	Sdp(u16),
	Rfcomm(u8),
	Psm,
}

struct Chan {
	local: u16,
	remote: u16,
	psm: u16,
	purpose: Purpose,
	ours_done: bool,
	theirs_done: bool,
	open: bool,
	// Enhanced retransmission mode, where the channel runs in it.
	ertm: Option<opp::Ertm>,
}

struct Dlc {
	dlci: u8,
	open: bool,
	// Frames this side may send, and frames it gave the host and the host has not used.
	tx_credits: u16,
	rx_credits: u16,
	queued: VecDeque<Vec<u8>>,
	received: usize,
	// This side's modem status has gone.
	msc_sent: bool,
	// What arrived on it, for the profile above to read; dropped where none does.
	inbox: Vec<u8>,
}

struct Rfcomm {
	cid: u16,
	// Whether this side opened the multiplexer.
	initiator: bool,
	mux: bool,
	// The DLC this side is opening, waiting on its parameters and then its acknowledgement.
	opening: Option<u8>,
	dlcs: Vec<Dlc>,
}

struct Link {
	handle: u16,
	authenticated: bool,
	encrypted: bool,
	// A pairing under way: its model, and whether this side started it.
	model: Option<Model>,
	peer_started: bool,
	host_capability: u8,
	host_authentication: u8,
	// Bytes of an L2CAP PDU arriving in fragments.
	assembling: Vec<u8>,
	want: usize,
	chans: Vec<Chan>,
	next_cid: u16,
	next_identifier: u8,
	rfcomm: Option<Rfcomm>,
	av: av::Av,
	hf: hf::Hf,
	opp: opp::Opp,
	// The host's address on the PAN link, as its frames gave it: what BNEP's compressed forms leave out.
	nap_host: [u8; 6],
}

struct Device {
	spec: &'static Spec,
	key: Option<[u8; 16]>,
	// The type of the key it holds: a Secure Connections key (0x07, 0x08) is the only source of a derived LE key.
	key_type: u8,
	// RESET TO FACTORY SETTINGS on the gate's word: it holds no key and knows it, so a key the host presents is
	// refused - unlike a fixture that simply forgot everything in a cold reboot.
	reset: bool,
	next_type: Option<u8>,
	link: Option<Link>,
	// The host was paged and has not answered.
	paging: bool,
	// The gate asked the headset to connect to the voice gateway before it had a link: it does once encrypted.
	hf_wanted: bool,
	// A push the gate asked for: its size, and when it starts.
	opp_wanted: Option<(u32, u64)>,
}

pub struct World {
	devices: Vec<Device>,
	scan: u8,
	log: VecDeque<String>,
	random: u64,
	transaction: u16,
	// What the keyboard types, and when: a tick and the script.
	typing: Vec<(u64, u8, String)>,
	// The clock as the last tick gave it: what an action that starts something on the fixture's clock starts from.
	now: u64,
	// AN LE KEY A DUAL-MODE DEVICE DERIVED over the BR/EDR Security Manager, for its LE half: its address and the key.
	derived: Option<([u8; 6], [u8; 16])>,
}

// THE KEYBOARD'S REPORT DESCRIPTOR: report 1 a boot-shaped keyboard (modifiers, a reserved byte, six keys), report 2
// one consumer control usage.
#[rustfmt::skip]
pub const KEYBOARD_DESCRIPTOR: [u8; 74] = [
	0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x01,
	0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
	0x95, 0x01, 0x75, 0x08, 0x81, 0x01,
	0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x26, 0xff, 0x00, 0x05, 0x07, 0x19, 0x00, 0x2a, 0xff, 0x00, 0x81, 0x00,
	0xc0,
	0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x02,
	0x15, 0x00, 0x26, 0xff, 0x03, 0x19, 0x00, 0x2a, 0xff, 0x03, 0x75, 0x10, 0x95, 0x01, 0x81, 0x00,
	0xc0,
];

// THE GAMEPAD'S REPORT DESCRIPTOR, the harness's shape: X, Y, Z and Rz over 0..255, sixteen buttons and one hat
// whose value past its range is centred - report 1.
#[rustfmt::skip]
pub const GAMEPAD_DESCRIPTOR: [u8; 64] = [
	0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01,
	0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x04, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35, 0x81, 0x02,
	0x05, 0x09, 0x19, 0x01, 0x29, 0x10, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x10, 0x81, 0x02,
	0x05, 0x01, 0x09, 0x39, 0x15, 0x00, 0x25, 0x07, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42,
	0x75, 0x04, 0x95, 0x01, 0x81, 0x03,
	0xc0,
];

// The gamepad's number on the control endpoint.
pub const GAMEPAD: u8 = 7;

// The keyboard's number on the control endpoint.
pub const KEYBOARD: u8 = 3;
// The BR/EDR Security Manager's fixed channel.
const SECURITY_MANAGER_CID: u16 = 0x0007;
// The HID PSMs, and the protocol's input data header.
const HID_CONTROL: u16 = 0x0011;
const HID_INTERRUPT: u16 = 0x0013;
const HIDP_DATA_INPUT: u8 = 0xa1;

// A character's keyboard usage and whether it needs Shift, on a US layout: what the script can type.
fn usage_of(character: char) -> Option<(u8, bool)> {
	match character {
		'a'..='z' => Some((0x04 + (character as u8 - b'a'), false)),
		'A'..='Z' => Some((0x04 + (character as u8 - b'A'), true)),
		'1'..='9' => Some((0x1e + (character as u8 - b'1'), false)),
		'0' => Some((0x27, false)),
		'\n' => Some((0x28, false)),
		' ' => Some((0x2c, false)),
		'-' => Some((0x2d, false)),
		'.' => Some((0x37, false)),
		_ => None,
	}
}

// ------------------------------------------------------------------ encodings, written here

fn event(code: u8, params: &[u8]) -> Out {
	let mut bytes = alloc::vec![code, params.len() as u8];
	bytes.extend_from_slice(params);
	Out::Event(bytes)
}

fn complete(opcode: u16, params: &[u8]) -> Out {
	let mut body = alloc::vec![1];
	body.extend_from_slice(&opcode.to_le_bytes());
	body.extend_from_slice(params);
	event(0x0e, &body)
}

fn status(opcode: u16, code: u8) -> Out {
	let op = opcode.to_le_bytes();
	event(0x0f, &[code, 1, op[0], op[1]])
}

// The device address as the wire carries it: least significant byte first.
fn wire(address: &[u8; 6]) -> [u8; 6] {
	let mut out = *address;
	out.reverse();
	out
}

fn with_address(address: &[u8; 6], rest: &[u8]) -> Vec<u8> {
	let mut out = wire(address).to_vec();
	out.extend_from_slice(rest);
	out
}

pub fn hex32(value: u32) -> String {
	format!("{value:08x}")
}

// THE RFCOMM FRAME CHECK: CRC-8 with the reversed polynomial 0xE0 (x^8 + x^2 + x + 1), from 0xFF, complemented.
pub fn crc8(bytes: &[u8]) -> u8 {
	let mut crc: u8 = 0xFF;
	for &byte in bytes {
		crc ^= byte;
		for _ in 0..8 {
			crc = if crc & 1 != 0 { (crc >> 1) ^ 0xE0 } else { crc >> 1 };
		}
	}
	0xFF - crc
}

// One RFCOMM frame: address, control, length, the credit byte where there is one, the information, the check.
pub fn rfcomm_frame(dlci: u8, cr: bool, control: u8, pf: bool, credits: Option<u8>, info: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![(dlci << 2) | (u8::from(cr) << 1) | 1, control | if pf { 0x10 } else { 0 }];
	if info.len() <= 127 {
		out.push(((info.len() as u8) << 1) | 1);
	} else {
		out.push((info.len() as u8) << 1);
		out.push((info.len() >> 7) as u8);
	}
	// UIH frames are checked over address and control only; every other frame over its length too.
	let check = if control == 0xEF { crc8(&out[..2]) } else { crc8(&out) };
	if let Some(credits) = credits {
		out.push(credits);
	}
	out.extend_from_slice(info);
	out.push(check);
	out
}

const SABM: u8 = 0x2F;
const UA: u8 = 0x63;
const DM: u8 = 0x0F;
const DISC: u8 = 0x43;
const UIH: u8 = 0xEF;

// SDP data elements, the few this fixture writes: unsigned integers, 16-bit UUIDs, text and sequences.
fn de_u8(value: u8) -> Vec<u8> {
	alloc::vec![0x08, value]
}

fn de_u16(value: u16) -> Vec<u8> {
	let bytes = value.to_be_bytes();
	alloc::vec![0x09, bytes[0], bytes[1]]
}

fn de_u32(value: u32) -> Vec<u8> {
	let mut out = alloc::vec![0x0A];
	out.extend_from_slice(&value.to_be_bytes());
	out
}

fn de_uuid(value: u16) -> Vec<u8> {
	let bytes = value.to_be_bytes();
	alloc::vec![0x19, bytes[0], bytes[1]]
}

fn de_text(text: &str) -> Vec<u8> {
	let mut out = alloc::vec![0x25, text.len() as u8];
	out.extend_from_slice(text.as_bytes());
	out
}

// Bytes as a text element: what a HID descriptor list carries a report descriptor as.
fn de_bytes(bytes: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![0x25, bytes.len() as u8];
	out.extend_from_slice(bytes);
	out
}

fn de_sequence(parts: &[Vec<u8>]) -> Vec<u8> {
	let body: Vec<u8> = parts.concat();
	let mut out = if body.len() < 256 { alloc::vec![0x35, body.len() as u8] } else { alloc::vec![0x36, (body.len() >> 8) as u8, body.len() as u8] };
	out.extend_from_slice(&body);
	out
}

// The 16-bit UUIDs anywhere in a data element sequence: what a search pattern names.
fn uuids_in(mut bytes: &[u8], out: &mut Vec<u16>) {
	while let Some((&header, rest)) = bytes.split_first() {
		let kind = header >> 3;
		let size = header & 7;
		let (len, rest) = match size {
			0..=4 if kind == 0 => (0, rest),
			0 => (1, rest),
			1 => (2, rest),
			2 => (4, rest),
			3 => (8, rest),
			4 => (16, rest),
			5 => match rest.split_first() {
				Some((&len, rest)) => (len as usize, rest),
				None => return,
			},
			6 if rest.len() >= 2 => (u16::from_be_bytes([rest[0], rest[1]]) as usize, &rest[2..]),
			_ => return,
		};
		if len > rest.len() {
			return;
		}
		match kind {
			3 if len == 2 => out.push(u16::from_be_bytes([rest[0], rest[1]])),
			6 | 7 => uuids_in(&rest[..len], out),
			_ => {}
		}
		bytes = &rest[len..];
	}
}

// ------------------------------------------------------------------ the world

impl Default for World {
	fn default() -> Self {
		World::new(1)
	}
}

impl World {
	pub fn new(seed: u64) -> World {
		World { devices: DEVICES.iter().map(|spec| Device { spec, key: None, key_type: 0, reset: false, next_type: None, link: None, paging: false, hf_wanted: false, opp_wanted: None }).collect(), scan: 0, log: VecDeque::new(), random: seed | 1, transaction: 1, typing: Vec::new(), now: 0, derived: None }
	}

	fn say(&mut self, line: String) {
		if self.log.len() >= LOG_LINES {
			self.log.pop_front();
		}
		self.log.push_back(line);
	}

	// What the far side saw since the last call.
	pub fn take_log(&mut self) -> Vec<String> {
		self.log.drain(..).collect()
	}

	// The lines not yet taken, for the fixture to print as they happen.
	pub fn log_len(&self) -> usize {
		self.log.len()
	}

	pub fn log_line(&self, at: usize) -> Option<&str> {
		self.log.get(at).map(String::as_str)
	}

	fn next_random(&mut self) -> u64 {
		// xorshift64*: the fixture's numbers need not be secret, only different.
		self.random ^= self.random >> 12;
		self.random ^= self.random << 25;
		self.random ^= self.random >> 27;
		self.random.wrapping_mul(0x2545_F491_4F6C_DD1D)
	}

	pub fn mix(&mut self, entropy: &[u8]) {
		for (at, byte) in entropy.iter().enumerate() {
			self.random ^= u64::from(*byte) << ((at % 8) * 8);
		}
		self.random |= 1;
	}

	// A RESET ENDS WHAT THE LINK LAYER WAS DOING, with no event: the links and the scan mode.
	pub fn reset(&mut self) {
		for device in self.devices.iter_mut() {
			device.link = None;
			device.paging = false;
		}
		self.scan = 0;
	}

	pub fn owns(&self, handle: u16) -> bool {
		self.devices.iter().any(|device| device.link.as_ref().is_some_and(|link| link.handle == handle))
	}

	fn by_address(&self, wire_address: &[u8]) -> Option<usize> {
		if wire_address.len() < 6 {
			return None;
		}
		let mut address = [0u8; 6];
		address.copy_from_slice(&wire_address[..6]);
		address.reverse();
		self.devices.iter().position(|device| device.spec.address == address)
	}

	fn by_handle(&self, handle: u16) -> Option<usize> {
		self.devices.iter().position(|device| device.link.as_ref().is_some_and(|link| link.handle == handle))
	}

	fn short(&self, at: usize) -> &'static str {
		self.devices[at].spec.name.trim_start_matches("fixture ")
	}

	fn new_link(&mut self, at: usize) -> u16 {
		let handle = FIRST_HANDLE + at as u16;
		self.devices[at].link = Some(Link { handle, authenticated: false, encrypted: false, model: None, peer_started: false, host_capability: 3, host_authentication: 0, assembling: Vec::new(), want: 0, chans: Vec::new(), next_cid: 0x0040, next_identifier: 1, rfcomm: None, av: av::Av { volume: 0x40, ..av::Av::default() }, hf: hf::Hf::default(), opp: opp::Opp::default(), nap_host: [0; 6] });
		handle
	}

	// ------------------------------------------------------------------ commands

	// ONE HCI COMMAND, if it is a BR/EDR one this world answers; `None` for anything else.
	pub fn command(&mut self, opcode: u16, params: &[u8]) -> Option<Vec<Out>> {
		let mut out = Vec::new();
		match opcode {
			// LMP features page 0: role switch and sniff (byte 0), EV3 (byte 3), LE on the controller and BR/EDR
			// supported (byte 4), Secure Simple Pairing (byte 6).
			0x1003 => out.push(complete(opcode, &[0, 0xA0, 0, 0, 0x80, 0x40, 0, 0x08, 0])),
			0x1005 => {
				let size = ACL_BYTES.to_le_bytes();
				let count = ACL_BUFFERS.to_le_bytes();
				out.push(complete(opcode, &[0, size[0], size[1], 64, count[0], count[1], 8, 0]));
			}
			0x0C56 | 0x0C7A | 0x0C24 | 0x0C13 | 0x0C45 | 0x080F | 0x0C18 | 0x0C6D | 0x0C52 => out.push(complete(opcode, &[0])),
			0x0C1A => {
				let mode = params.first().copied().unwrap_or(0);
				if mode != self.scan {
					self.scan = mode;
					self.say(format!("scan {mode}"));
				}
				out.push(complete(opcode, &[0]));
			}
			0x0401 => {
				out.push(status(opcode, 0));
				// EVERY DEVICE ANSWERS INQUIRY with an extended inquiry response: its name and its service classes.
				for at in 0..self.devices.len() {
					out.push(self.inquiry_result(at));
				}
				out.push(event(0x01, &[0]));
			}
			0x0402 => out.push(complete(opcode, &[0])),
			0x0405 => {
				out.push(status(opcode, 0));
				let Some(at) = self.by_address(params) else {
					let mut failed = alloc::vec![0x04, 0, 0];
					failed.extend_from_slice(&params[..params.len().min(6)]);
					failed.extend_from_slice(&[1, 0]);
					out.push(event(0x03, &failed));
					return Some(out);
				};
				if self.devices[at].link.is_some() {
					out.push(self.connection_complete(at, 0x0B, 0));
					return Some(out);
				}
				let handle = self.new_link(at);
				self.say(format!("connected {} - this host paged it", self.short(at)));
				out.push(self.connection_complete(at, 0, handle));
			}
			0x0408 => {
				let mut body = alloc::vec![0];
				body.extend_from_slice(&params[..params.len().min(6)]);
				out.push(complete(opcode, &body));
			}
			0x0409 => {
				out.push(status(opcode, 0));
				let Some(at) = self.by_address(params).filter(|&at| self.devices[at].paging) else { return Some(out) };
				self.devices[at].paging = false;
				let handle = self.new_link(at);
				self.say(format!("accepted {} - this host took its page", self.short(at)));
				out.push(self.connection_complete(at, 0, handle));
				out.extend(self.hf_linked(at));
			}
			0x040A => {
				out.push(status(opcode, 0));
				let Some(at) = self.by_address(params).filter(|&at| self.devices[at].paging) else { return Some(out) };
				self.devices[at].paging = false;
				let reason = params.get(6).copied().unwrap_or(0x0F);
				self.say(format!("rejected {} reason {reason:#04x}", self.short(at)));
				out.push(self.connection_complete(at, reason, 0));
			}
			0x080D => out.push(complete(opcode, &[0, params.first().copied().unwrap_or(0), params.get(1).copied().unwrap_or(0)])),
			0x041B => {
				out.push(status(opcode, 0));
				let handle = handle_of(params);
				if self.by_handle(handle).is_some() {
					let h = handle.to_le_bytes();
					out.push(event(0x0B, &[0, h[0], h[1], 0xA0, 0, 0, 0x80, 0, 0, 0x08, 0]));
				}
			}
			0x0419 => {
				out.push(status(opcode, 0));
				if let Some(at) = self.by_address(params) {
					let mut body = alloc::vec![0];
					body.extend_from_slice(&wire(&self.devices[at].spec.address));
					let mut name = [0u8; 248];
					let text = self.devices[at].spec.name.as_bytes();
					name[..text.len()].copy_from_slice(text);
					body.extend_from_slice(&name);
					out.push(event(0x07, &body));
				}
			}
			0x0411 => {
				out.push(status(opcode, 0));
				let Some(at) = self.by_handle(handle_of(params)) else { return Some(out) };
				let address = self.devices[at].spec.address;
				if let Some(link) = self.devices[at].link.as_mut() {
					link.peer_started = false;
				}
				// The controller asks its host for the key it holds for this device.
				out.push(event(0x17, &wire(&address)));
			}
			0x040B => {
				let Some(at) = self.by_address(params) else {
					out.push(complete(opcode, &[0x02]));
					return Some(out);
				};
				let mut reply = alloc::vec![0];
				reply.extend_from_slice(&params[..6]);
				out.push(complete(opcode, &reply));
				let mut key = [0u8; 16];
				if params.len() >= 22 {
					key.copy_from_slice(&params[6..22]);
				}
				out.extend(self.authenticate_with(at, key));
			}
			0x040C => {
				let Some(at) = self.by_address(params) else { return Some(out) };
				let mut reply = alloc::vec![0];
				reply.extend_from_slice(&params[..6]);
				out.push(complete(opcode, &reply));
				// NO KEY: the two sides pair. A device with Secure Simple Pairing asks this host its IO capability;
				// one without asks for a PIN.
				let address = wire(&self.devices[at].spec.address);
				if self.devices[at].spec.ssp {
					out.push(event(0x31, &address));
				} else {
					if let Some(link) = self.devices[at].link.as_mut() {
						link.model = Some(Model::Pin);
					}
					out.push(event(0x16, &address));
				}
			}
			0x042B => {
				let Some(at) = self.by_address(params) else { return Some(out) };
				let mut reply = alloc::vec![0];
				reply.extend_from_slice(&params[..6]);
				out.push(complete(opcode, &reply));
				let capability = params.get(6).copied().unwrap_or(3);
				let authentication = params.get(8).copied().unwrap_or(0);
				self.say(format!("io-capability {} host {capability} auth {authentication:#04x}", self.short(at)));
				let spec = self.devices[at].spec;
				let Some(link) = self.devices[at].link.as_mut() else { return Some(out) };
				link.host_capability = capability;
				link.host_authentication = authentication;
				// When the host started it, the device's capability comes now; when the device did, it came first.
				if !link.peer_started {
					out.push(event(0x32, &with_address(&spec.address, &[spec.capability, 0, spec.authentication])));
				}
				out.extend(self.association(at));
			}
			0x0434 => {
				let Some(at) = self.by_address(params) else { return Some(out) };
				let mut reply = alloc::vec![0];
				reply.extend_from_slice(&params[..6]);
				out.push(complete(opcode, &reply));
				let reason = params.get(6).copied().unwrap_or(0x18);
				self.say(format!("pairing refused by the host {} reason {reason:#04x}", self.short(at)));
				out.extend(self.pairing_failed(at, reason));
			}
			0x042C => {
				let Some(at) = self.by_address(params) else { return Some(out) };
				let mut reply = alloc::vec![0];
				reply.extend_from_slice(&params[..6]);
				out.push(complete(opcode, &reply));
				self.say(format!("confirmed by the host {}", self.short(at)));
				out.extend(self.pairing_done(at));
			}
			0x042D => {
				let Some(at) = self.by_address(params) else { return Some(out) };
				let mut reply = alloc::vec![0];
				reply.extend_from_slice(&params[..6]);
				out.push(complete(opcode, &reply));
				self.say(format!("confirmation refused by the host {}", self.short(at)));
				out.extend(self.pairing_failed(at, 0x05));
			}
			0x042E | 0x042F => {
				let mut reply = alloc::vec![0];
				reply.extend_from_slice(&params[..params.len().min(6)]);
				out.push(complete(opcode, &reply));
			}
			0x040D => {
				let Some(at) = self.by_address(params) else { return Some(out) };
				let mut reply = alloc::vec![0];
				reply.extend_from_slice(&params[..6]);
				out.push(complete(opcode, &reply));
				let len = params.get(6).copied().unwrap_or(0) as usize;
				let pin = params.get(7..7 + len.min(16)).unwrap_or(&[]);
				if pin == self.devices[at].spec.pin {
					self.say(format!("pin accepted {}", self.short(at)));
					out.extend(self.pairing_done(at));
				} else {
					self.say(format!("pin wrong {}", self.short(at)));
					out.extend(self.pairing_failed(at, 0x05));
				}
			}
			0x040E => {
				let Some(at) = self.by_address(params) else { return Some(out) };
				let mut reply = alloc::vec![0];
				reply.extend_from_slice(&params[..6]);
				out.push(complete(opcode, &reply));
				self.say(format!("pin refused by the host {}", self.short(at)));
				out.extend(self.pairing_failed(at, 0x06));
			}
			0x0413 => {
				out.push(status(opcode, 0));
				let handle = handle_of(params);
				let Some(at) = self.by_handle(handle) else { return Some(out) };
				let h = handle.to_le_bytes();
				let link = self.devices[at].link.as_mut()?;
				let authenticated = link.authenticated;
				link.encrypted = authenticated;
				if authenticated {
					self.say(format!("encrypted {}", self.short(at)));
					out.push(event(0x08, &[0, h[0], h[1], 1]));
				} else {
					out.push(event(0x08, &[0x05, h[0], h[1], 0]));
				}
			}
			0x0803 => {
				out.push(status(opcode, 0));
				let handle = handle_of(params);
				if let Some(at) = self.by_handle(handle) {
					self.say(format!("sniff {}", self.short(at)));
					let h = handle.to_le_bytes();
					let interval = params.get(2..4).map_or([0, 8], |bytes| [bytes[0], bytes[1]]);
					out.push(event(0x14, &[0, h[0], h[1], 2, interval[0], interval[1]]));
				}
			}
			0x0804 => {
				out.push(status(opcode, 0));
				let h = handle_of(params).to_le_bytes();
				out.push(event(0x14, &[0, h[0], h[1], 0, 0, 0]));
			}
			0x080B | 0x042A | 0x0429 => out.push(status(opcode, 0)),
			// THE HOST SETS A VOICE LINK UP to a headset: transparent data - the voice setting's air coding bits 11 - is
			// mSBC.
			0x0428 => {
				out.push(status(opcode, 0));
				let setting = params.get(12..14).map_or(0, |bytes| u16::from_le_bytes([bytes[0], bytes[1]]));
				out.extend(self.sco_setup(handle_of(params), setting & 0x0003 == 0x0003));
			}
			0x0406 if self.owns_sco(handle_of(params)).is_some() => {
				out.push(status(opcode, 0));
				out.extend(self.sco_down(handle_of(params)));
			}
			0x0406 => {
				let handle = handle_of(params);
				let at = self.by_handle(handle)?;
				out.push(status(opcode, 0));
				self.say(format!("disconnected by the host {}", self.short(at)));
				self.devices[at].link = None;
				let h = handle.to_le_bytes();
				out.push(event(0x05, &[0, h[0], h[1], 0x16]));
			}
			_ => return None,
		}
		Some(out)
	}

	fn inquiry_result(&mut self, at: usize) -> Out {
		let spec = self.devices[at].spec;
		let mut body = alloc::vec![1];
		body.extend_from_slice(&wire(&spec.address));
		// Page scan repetition R1, reserved, the class, a clock offset and the signal strength.
		body.extend_from_slice(&[1, 0]);
		body.extend_from_slice(&spec.class.to_le_bytes()[..3]);
		body.extend_from_slice(&[0x34, 0x12]);
		body.push((-50i8 - at as i8) as u8);
		let mut eir = Vec::new();
		eir.push(spec.name.len() as u8 + 1);
		eir.push(0x09);
		eir.extend_from_slice(spec.name.as_bytes());
		eir.push(spec.uuids.len() as u8 * 2 + 1);
		eir.push(0x03);
		for uuid in spec.uuids {
			eir.extend_from_slice(&uuid.to_le_bytes());
		}
		eir.resize(240, 0);
		body.extend_from_slice(&eir);
		event(0x2F, &body)
	}

	fn connection_complete(&self, at: usize, code: u8, handle: u16) -> Out {
		let h = handle.to_le_bytes();
		let mut body = alloc::vec![code, h[0], h[1]];
		body.extend_from_slice(&wire(&self.devices[at].spec.address));
		body.extend_from_slice(&[1, 0]);
		event(0x03, &body)
	}

	// THE HOST PRESENTED A KEY. One this device holds must match; a device that holds none - a fresh fixture
	// after a cold reboot - takes it and says so, so the gate can compare its fingerprint with the pairing's.
	fn authenticate_with(&mut self, at: usize, key: [u8; 16]) -> Vec<Out> {
		let mut out = Vec::new();
		let fingerprint = crate::bt_peer::fingerprint(&key);
		let name = self.short(at);
		let accepted = match self.devices[at].key {
			Some(held) if held == key => {
				self.say(format!("authenticated {name} with a remembered key {}", hex32(fingerprint)));
				true
			}
			Some(_) => {
				self.say(format!("authentication REFUSED for {name}: an unknown key {}", hex32(fingerprint)));
				false
			}
			None if self.devices[at].reset => {
				self.say(format!("authentication REFUSED for {name}: it was reset and holds no key"));
				false
			}
			None => {
				self.say(format!("authenticated {name} with a key this boot never paired, key {}", hex32(fingerprint)));
				self.devices[at].key = Some(key);
				true
			}
		};
		let Some(link) = self.devices[at].link.as_mut() else { return out };
		let h = link.handle.to_le_bytes();
		link.authenticated = accepted;
		if link.peer_started {
			// THE DEVICE AUTHENTICATED, so it turns encryption on itself.
			if accepted {
				link.encrypted = true;
				out.push(event(0x08, &[0, h[0], h[1], 1]));
			}
		} else {
			out.push(event(0x06, &[if accepted { 0 } else { 0x06 }, h[0], h[1]]));
		}
		out
	}

	// THE ASSOCIATION MODEL both IO capabilities give, and its first question to the host.
	fn association(&mut self, at: usize) -> Vec<Out> {
		let mut out = Vec::new();
		let spec = self.devices[at].spec;
		let Some(host) = self.devices[at].link.as_ref().map(|link| link.host_capability) else { return out };
		let model = match (host, spec.capability) {
			(1, 1) => Model::Numeric,
			(0 | 1, 2) => Model::Passkey((self.next_random() % 1_000_000) as u32),
			_ => Model::JustWorks,
		};
		if let Some(link) = self.devices[at].link.as_mut() {
			link.model = Some(model);
		}
		match model {
			Model::Passkey(passkey) => {
				self.say(format!("passkey {} {passkey:06}", self.short(at)));
				out.push(event(0x3B, &with_address(&spec.address, &passkey.to_le_bytes())));
			}
			_ => {
				let value = (self.next_random() % 1_000_000) as u32;
				let what = if model == Model::Numeric { "compare" } else { "just-works" };
				self.say(format!("{what} {} {value:06}", self.short(at)));
				out.push(event(0x33, &with_address(&spec.address, &value.to_le_bytes())));
			}
		}
		out
	}

	// THE PAIRING SUCCEEDED: a new key of the type the model and the device's support make, kept by the device and
	// handed to the host.
	fn pairing_done(&mut self, at: usize) -> Vec<Out> {
		let mut out = Vec::new();
		let spec = self.devices[at].spec;
		let Some(model) = self.devices[at].link.as_ref().and_then(|link| link.model) else { return out };
		let authenticated = !matches!(model, Model::JustWorks);
		let made = match model {
			Model::Pin => 0x00,
			_ if spec.secure_connections => {
				if authenticated {
					0x08
				} else {
					0x07
				}
			}
			_ if authenticated => 0x05,
			_ => 0x04,
		};
		let kind = self.devices[at].next_type.take().unwrap_or(made);
		let mut key = [0u8; 16];
		for chunk in key.chunks_mut(8) {
			chunk.copy_from_slice(&self.next_random().to_le_bytes());
		}
		self.devices[at].key = Some(key);
		self.devices[at].key_type = kind;
		self.devices[at].reset = false;
		self.say(format!("paired {} type {kind:#04x} key {}", self.short(at), hex32(crate::bt_peer::fingerprint(&key))));
		if spec.ssp {
			let mut body = alloc::vec![0];
			body.extend_from_slice(&wire(&spec.address));
			out.push(event(0x36, &body));
		}
		let mut notification = wire(&spec.address).to_vec();
		notification.extend_from_slice(&key);
		notification.push(kind);
		out.push(event(0x18, &notification));
		let Some(link) = self.devices[at].link.as_mut() else { return out };
		link.model = None;
		link.authenticated = true;
		let h = link.handle.to_le_bytes();
		if link.peer_started {
			link.encrypted = true;
			out.push(event(0x08, &[0, h[0], h[1], 1]));
		} else {
			out.push(event(0x06, &[0, h[0], h[1]]));
		}
		out
	}

	fn pairing_failed(&mut self, at: usize, reason: u8) -> Vec<Out> {
		let mut out = Vec::new();
		let spec = self.devices[at].spec;
		let Some(link) = self.devices[at].link.as_mut() else { return out };
		link.model = None;
		let h = link.handle.to_le_bytes();
		let peer_started = link.peer_started;
		if spec.ssp {
			let mut body = alloc::vec![reason];
			body.extend_from_slice(&wire(&spec.address));
			out.push(event(0x36, &body));
		}
		if !peer_started {
			out.push(event(0x06, &[reason, h[0], h[1]]));
		}
		out
	}

	// ------------------------------------------------------------------ the control endpoint

	// ONE DEVICE ACTS. The answer is the action's immediate result; what follows arrives as the host answers.
	pub fn act(&mut self, device: u8, action: u32, argument: u32) -> Result<(u32, Vec<Out>), &'static str> {
		let at = usize::from(device.checked_sub(FIRST_DEVICE).ok_or("no such device")?);
		if at >= self.devices.len() {
			return Err("no such device");
		}
		let mut out = Vec::new();
		match action {
			// Page the host.
			1 => {
				if self.devices[at].link.is_some() {
					return Ok((0x0B, out));
				}
				if self.scan & 2 == 0 {
					self.say(format!("page {}: this host is not connectable", self.short(at)));
					return Ok((0x04, out));
				}
				self.devices[at].paging = true;
				let spec = self.devices[at].spec;
				let mut body = wire(&spec.address).to_vec();
				body.extend_from_slice(&spec.class.to_le_bytes()[..3]);
				body.push(1);
				out.push(event(0x04, &body));
			}
			// Open an L2CAP channel to a PSM.
			2 => out.extend(self.open_channel(at, argument as u16, Purpose::Psm).ok_or("no link")?),
			// Pair from the device's side: with its key where it has one, or anew.
			3 => {
				let spec = self.devices[at].spec;
				let has_key = self.devices[at].key.is_some();
				let link = self.devices[at].link.as_mut().ok_or("no link")?;
				link.peer_started = true;
				if has_key {
					out.push(event(0x17, &wire(&spec.address)));
				} else if spec.ssp {
					out.push(event(0x32, &with_address(&spec.address, &[spec.capability, 0, spec.authentication])));
					out.push(event(0x31, &wire(&spec.address)));
				} else {
					link.model = Some(Model::Pin);
					out.push(event(0x16, &wire(&spec.address)));
				}
			}
			4 => self.devices[at].next_type = Some(argument as u8),
			// Type the passkey: the digits arrive as keypresses, then the pairing finishes or fails.
			5 => {
				let spec = self.devices[at].spec;
				let Some(Model::Passkey(shown)) = self.devices[at].link.as_ref().and_then(|link| link.model) else { return Err("no passkey is being typed") };
				for kind in [0u8, 1, 1, 1, 1, 1, 1, 4] {
					out.push(event(0x3C, &with_address(&spec.address, &[kind])));
				}
				if shown == argument {
					self.say(format!("typed the passkey on {}", self.short(at)));
					out.extend(self.pairing_done(at));
				} else {
					self.say(format!("typed a wrong passkey on {}", self.short(at)));
					out.extend(self.pairing_failed(at, 0x05));
				}
			}
			6 => out.extend(self.open_channel(at, 0x0001, Purpose::Sdp(argument as u16)).ok_or("no link")?),
			7 => out.extend(self.open_channel(at, 0x0003, Purpose::Rfcomm(argument as u8)).ok_or("no link")?),
			8 => {
				let Some(link) = self.devices[at].link.take() else { return Ok((0x02, out)) };
				self.say(format!("{} disconnected", self.short(at)));
				let h = link.handle.to_le_bytes();
				out.push(event(0x05, &[0, h[0], h[1], 0x13]));
			}
			9 => {
				self.devices[at].key = None;
				self.devices[at].reset = true;
				self.say(format!("{} was reset and forgot its key", self.short(at)));
			}
			10 => out.extend(self.rfcomm_send(at, argument as usize)),
			11 => out.extend(self.press(at, &[(0x05, 0x4c)])),
			12 => out.extend(self.consumer(at, 0x0030)),
			// THE KEYBOARD RECONNECTS ITS INPUT: control first, then interrupt once control is up.
			13 => out.extend(self.open_channel(at, HID_CONTROL, Purpose::Psm).ok_or("no link")?),
			// THE GAMEPAD REPORTS: buttons in the low sixteen bits, the hat in the next four, X in the top byte.
			// THE PHONE STREAMS for `argument` seconds, or stops; the headset changes its level; the headset presses play.
			15 => out.extend(self.phone_stream(at, argument)?),
			16 => out.extend(self.headset_volume(at, argument as u8)?),
			17 => out.extend(self.headset_play(at)?),
			// THE HEADSET'S HANDS-FREE UNIT: it connects to the voice gateway, answers, hangs up, asks for audio.
			18 => out.extend(self.hf_connect(at)?),
			19 => out.extend(self.hf_press(at, "ATA")?),
			20 => out.extend(self.hf_press(at, "AT+CHUP")?),
			21 => out.extend(self.hf_press(at, "AT+BCC")?),
			// THE PHONE PUSHES AN OBJECT of `argument` bytes, five seconds from now.
			22 => out.extend(self.opp_push(at, argument)?),
			14 => {
				let remote = self.interrupt(at).ok_or("the gamepad's input is not connected")?;
				let buttons = (argument as u16).to_le_bytes();
				let hat = ((argument >> 16) & 0x0f) as u8;
				let x = (argument >> 24) as u8;
				out.extend(self.send_pdu(at, remote, &[HIDP_DATA_INPUT, 1, x, 127, 127, 127, buttons[0], buttons[1], hat]));
				self.say(format!("{} reported buttons {:#06x} hat {hat} x {x}", self.short(at), argument as u16));
			}
			_ => return Err("no such action"),
		}
		Ok((0, out))
	}

	// ------------------------------------------------------------------ ACL and L2CAP

	// ONE ACL PACKET from the host, if it is for a link of this world; the buffer comes straight back.
	pub fn acl(&mut self, handle: u16, boundary: u8, data: &[u8]) -> Option<Vec<Out>> {
		let at = self.by_handle(handle)?;
		let h = handle.to_le_bytes();
		let mut out = alloc::vec![event(0x13, &[1, h[0], h[1], 1, 0])];
		let link = self.devices[at].link.as_mut()?;
		if boundary == 0b01 {
			link.assembling.extend_from_slice(data);
		} else {
			link.assembling.clear();
			link.assembling.extend_from_slice(data);
			link.want = if data.len() >= 2 { u16::from_le_bytes([data[0], data[1]]) as usize + 4 } else { usize::MAX };
		}
		if link.assembling.len() < link.want {
			return Some(out);
		}
		let pdu = core::mem::take(&mut link.assembling);
		if pdu.len() != link.want {
			return Some(out);
		}
		let cid = u16::from_le_bytes([pdu[2], pdu[3]]);
		let payload = &pdu[4..];
		if cid == 0x0001 {
			out.extend(self.signalling(at, payload));
		} else if cid == SECURITY_MANAGER_CID {
			out.extend(self.security_manager(at, payload));
		} else {
			out.extend(self.channel_data(at, cid, payload));
		}
		Some(out)
	}

	// THE BR/EDR SECURITY MANAGER of a dual-mode device, the responder: the host - the link's central - asks for the LE
	// key to be derived from the link key, and this side, holding a Secure Connections key on an encrypted link,
	// agrees, derives it with its own functions, takes the host's identity and hands the key to its LE half. It gives no
	// identity of its own: its LE half is heard from its public address.
	fn security_manager(&mut self, at: usize, payload: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		let name = self.short(at);
		let spec = self.devices[at].spec;
		let (key, key_type) = (self.devices[at].key, self.devices[at].key_type);
		let encrypted = self.devices[at].link.as_ref().is_some_and(|link| link.encrypted);
		let Some(&code) = payload.first() else { return out };
		match code {
			0x01 if payload.len() == 7 => {
				let refuse = !spec.dual || !encrypted || !matches!(key_type, 0x07 | 0x08) || payload[4] != 16 || payload[5] & payload[6] & 0x01 == 0;
				let Some(key) = key.filter(|_| !refuse) else {
					self.say(format!("{name} refused a key derivation over BR/EDR"));
					out.extend(self.send_pdu(at, SECURITY_MANAGER_CID, &[0x05, 0x05]));
					return out;
				};
				let ct2 = payload[3] & 0x20 != 0;
				let response = [0x02, 0, 0, payload[3] & 0x20, 16, payload[5] & 0x03, payload[6] & 0x01];
				out.extend(self.send_pdu(at, SECURITY_MANAGER_CID, &response));
				let ltk = crate::bt_peer::ltk_from_link_key(&key, ct2);
				self.derived = Some((spec.address, ltk));
				self.say(format!("{name} derived an LE key over BR/EDR, key {}", hex32(crate::bt_peer::fingerprint(&ltk))));
			}
			0x08 => {}
			0x09 if payload.len() == 8 => self.say(format!("{name} learned the host's identity over BR/EDR")),
			_ => out.extend(self.send_pdu(at, SECURITY_MANAGER_CID, &[0x05, 0x08])),
		}
		out
	}

	// THE LE KEY A DUAL-MODE DEVICE DERIVED over BR/EDR, for its LE half.
	pub fn take_derived(&mut self) -> Option<([u8; 6], [u8; 16])> {
		self.derived.take()
	}

	// THE LINK KEY A DUAL-MODE DEVICE'S LE HALF DERIVED from its LE pairing: the BR/EDR half holds it from now on, as an
	// authenticated or unauthenticated P-256 key.
	pub fn set_key(&mut self, address: &[u8; 6], key: [u8; 16], authenticated: bool) {
		let Some(at) = self.devices.iter().position(|device| device.spec.address == *address && device.spec.dual) else { return };
		self.devices[at].key = Some(key);
		self.devices[at].key_type = if authenticated { 0x08 } else { 0x07 };
		self.devices[at].reset = false;
	}

	// A DUAL-MODE DEVICE RESET on both radios: the BR/EDR half forgets here, the LE half in its own world.
	pub fn is_dual(&self, device: u8) -> Option<[u8; 6]> {
		let at = usize::from(device.checked_sub(FIRST_DEVICE)?);
		self.devices.get(at).filter(|device| device.spec.dual).map(|device| device.spec.address)
	}

	fn send_pdu(&self, at: usize, cid: u16, payload: &[u8]) -> Option<Out> {
		let link = self.devices[at].link.as_ref()?;
		let mut pdu = Vec::with_capacity(4 + payload.len());
		pdu.extend_from_slice(&(payload.len() as u16).to_le_bytes());
		pdu.extend_from_slice(&cid.to_le_bytes());
		pdu.extend_from_slice(payload);
		let mut acl = Vec::with_capacity(4 + pdu.len());
		acl.extend_from_slice(&(link.handle | (0b10 << 12)).to_le_bytes());
		acl.extend_from_slice(&(pdu.len() as u16).to_le_bytes());
		acl.extend_from_slice(&pdu);
		Some(Out::Acl(acl))
	}

	fn signal(&mut self, at: usize, code: u8, identifier: Option<u8>, data: &[u8]) -> Option<Out> {
		let link = self.devices[at].link.as_mut()?;
		let identifier = identifier.unwrap_or_else(|| {
			let id = link.next_identifier;
			link.next_identifier = if id == 0xFF { 1 } else { id + 1 };
			id
		});
		let mut body = alloc::vec![code, identifier];
		body.extend_from_slice(&(data.len() as u16).to_le_bytes());
		body.extend_from_slice(data);
		self.send_pdu(at, 0x0001, &body)
	}

	fn open_channel(&mut self, at: usize, psm: u16, purpose: Purpose) -> Option<Vec<Out>> {
		let link = self.devices[at].link.as_mut()?;
		let local = link.next_cid;
		link.next_cid += 1;
		link.chans.push(Chan { local, remote: 0, psm, purpose, ours_done: false, theirs_done: false, open: false, ertm: None });
		let mut data = psm.to_le_bytes().to_vec();
		data.extend_from_slice(&local.to_le_bytes());
		Some(self.signal(at, 0x02, None, &data).into_iter().collect())
	}

	fn configure(&mut self, at: usize, remote: u16, ertm: bool, mtu: u16) -> Option<Out> {
		// The default MTU of 672: what a small device takes, and a test that the host fragments to it. BNEP's channel
		// takes a whole Ethernet frame.
		let mut data = remote.to_le_bytes().to_vec();
		data.extend_from_slice(&[0, 0, 0x01, 0x02]);
		data.extend_from_slice(&mtu.to_le_bytes());
		if ertm {
			// ENHANCED RETRANSMISSION: a window of ten, three transmissions, two seconds and twelve, segments of 670.
			data.extend_from_slice(&[0x04, 0x09, 0x03, 10, 3]);
			data.extend_from_slice(&2000u16.to_le_bytes());
			data.extend_from_slice(&12000u16.to_le_bytes());
			data.extend_from_slice(&670u16.to_le_bytes());
		}
		self.signal(at, 0x04, None, &data)
	}

	fn signalling(&mut self, at: usize, mut bytes: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		while bytes.len() >= 4 {
			let (code, identifier) = (bytes[0], bytes[1]);
			let len = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
			if bytes.len() < 4 + len {
				break;
			}
			let data = bytes[4..4 + len].to_vec();
			bytes = &bytes[4 + len..];
			let u16_at = |at: usize| data.get(at..at + 2).map_or(0, |pair| u16::from_le_bytes([pair[0], pair[1]]));
			match code {
				// THE HOST OPENS A CHANNEL: accepted, and this side's configuration follows.
				0x02 => {
					let (psm, scid) = (u16_at(0), u16_at(2));
					// THE DEVICE'S OBJECT PUSH ON L2CAP runs in enhanced retransmission mode, as GOEP requires.
					let ertm = self.devices[at].spec.opp.and_then(|(_, goep)| goep) == Some(psm);
					let Some(link) = self.devices[at].link.as_mut() else { break };
					let local = link.next_cid;
					link.next_cid += 1;
					link.chans.push(Chan { local, remote: scid, psm, purpose: Purpose::Host, ours_done: false, theirs_done: false, open: false, ertm: ertm.then(opp::Ertm::default) });
					let mut response = local.to_le_bytes().to_vec();
					response.extend_from_slice(&scid.to_le_bytes());
					response.extend_from_slice(&[0, 0, 0, 0]);
					out.extend(self.signal(at, 0x03, Some(identifier), &response));
					out.extend(self.configure(at, scid, ertm, if psm == nap::BNEP_PSM { nap::BNEP_MTU } else { 672 }));
				}
				0x03 => {
					let (dcid, scid, result) = (u16_at(0), u16_at(2), u16_at(4));
					let name = self.short(at);
					let Some(link) = self.devices[at].link.as_mut() else { break };
					let Some(position) = link.chans.iter().position(|chan| chan.local == scid) else { continue };
					if result == 1 {
						continue;
					}
					let psm = link.chans[position].psm;
					self.log.push_back(format!("l2cap {name} psm {psm:#06x} result {result}"));
					if result != 0 {
						let purpose = link.chans.remove(position).purpose;
						// THE HOST REFUSED THE SESSION A PUSH NEEDS.
						if purpose == Purpose::Rfcomm(opp::HOST_CHANNEL) {
							self.opp_refused(at);
						}
						continue;
					}
					link.chans[position].remote = dcid;
					out.extend(self.configure(at, dcid, false, 672));
				}
				0x04 => {
					let dcid = u16_at(0);
					let Some(link) = self.devices[at].link.as_mut() else { break };
					let Some(chan) = link.chans.iter_mut().find(|chan| chan.local == dcid) else { continue };
					chan.theirs_done = true;
					let remote = chan.remote;
					let mut response = remote.to_le_bytes().to_vec();
					response.extend_from_slice(&[0, 0, 0, 0]);
					out.extend(self.signal(at, 0x05, Some(identifier), &response));
					out.extend(self.maybe_open(at, dcid));
				}
				0x05 => {
					let scid = u16_at(0);
					let Some(link) = self.devices[at].link.as_mut() else { break };
					let Some(chan) = link.chans.iter_mut().find(|chan| chan.local == scid) else { continue };
					chan.ours_done = true;
					out.extend(self.maybe_open(at, scid));
				}
				0x06 => {
					let (dcid, scid) = (u16_at(0), u16_at(2));
					let Some(link) = self.devices[at].link.as_mut() else { break };
					link.chans.retain(|chan| chan.local != dcid);
					if link.rfcomm.as_ref().is_some_and(|rfcomm| rfcomm.cid == dcid) {
						link.rfcomm = None;
					}
					let mut response = dcid.to_le_bytes().to_vec();
					response.extend_from_slice(&scid.to_le_bytes());
					out.extend(self.signal(at, 0x07, Some(identifier), &response));
				}
				0x07 => {
					let scid = u16_at(2);
					if let Some(link) = self.devices[at].link.as_mut() {
						link.chans.retain(|chan| chan.local != scid);
					}
				}
				0x08 => out.extend(self.signal(at, 0x09, Some(identifier), &data)),
				// THE FIXED CHANNELS a dual-mode device has - the signalling channel and the BR/EDR Security Manager;
				// every other question is answered not supported.
				0x0A => {
					let info_type = u16_at(0);
					let mut response = info_type.to_le_bytes().to_vec();
					if info_type == 0x0003 && self.devices[at].spec.dual {
						response.extend_from_slice(&0u16.to_le_bytes());
						response.extend_from_slice(&((1u64 << 1) | (1 << 7)).to_le_bytes());
					} else {
						response.extend_from_slice(&1u16.to_le_bytes());
					}
					out.extend(self.signal(at, 0x0B, Some(identifier), &response));
				}
				_ => {}
			}
		}
		out
	}

	// A channel configured both ways is open, and what it was opened for begins.
	fn maybe_open(&mut self, at: usize, local: u16) -> Vec<Out> {
		let mut out = Vec::new();
		let Some(link) = self.devices[at].link.as_mut() else { return out };
		let Some(chan) = link.chans.iter_mut().find(|chan| chan.local == local) else { return out };
		if chan.open || !chan.ours_done || !chan.theirs_done {
			return out;
		}
		chan.open = true;
		let (remote, purpose) = (chan.remote, chan.purpose);
		match purpose {
			Purpose::Sdp(uuid) => {
				let transaction = self.transaction;
				self.transaction = self.transaction.wrapping_add(1);
				// A ServiceSearchRequest for the one class: the pattern, at most sixteen handles, no continuation.
				let mut parameters = de_sequence(&[de_uuid(uuid)]);
				parameters.extend_from_slice(&16u16.to_be_bytes());
				parameters.push(0);
				let mut pdu = alloc::vec![0x02];
				pdu.extend_from_slice(&transaction.to_be_bytes());
				pdu.extend_from_slice(&(parameters.len() as u16).to_be_bytes());
				pdu.extend_from_slice(&parameters);
				out.extend(self.send_pdu(at, remote, &pdu));
			}
			Purpose::Rfcomm(channel) => {
				let Some(link) = self.devices[at].link.as_mut() else { return out };
				link.rfcomm = Some(Rfcomm { cid: local, initiator: true, mux: false, opening: Some(channel), dlcs: Vec::new() });
				out.extend(self.send_pdu(at, remote, &rfcomm_frame(0, Self::cr(true, true), SABM, true, None, &[])));
			}
			Purpose::Psm => {
				let name = self.short(at);
				self.say(format!("l2cap {name} channel {local:#06x} open"));
				// THE KEYBOARD'S CONTROL CHANNEL IS UP: its interrupt channel follows, as a HID device opens them.
				let psm = self.devices[at].link.as_ref().and_then(|link| link.chans.iter().find(|chan| chan.local == local)).map(|chan| chan.psm);
				if psm == Some(HID_CONTROL) {
					out.extend(self.open_channel(at, HID_INTERRUPT, Purpose::Psm).unwrap_or_default());
				}
			}
			Purpose::Avdtp | Purpose::AvdtpMedia => {
				let psm = self.devices[at].link.as_ref().and_then(|link| link.chans.iter().find(|chan| chan.local == local)).map_or(0, |chan| chan.psm);
				out.extend(self.av_opened(at, local, psm, purpose));
			}
			Purpose::Host => {
				let psm = self.devices[at].link.as_ref().and_then(|link| link.chans.iter().find(|chan| chan.local == local)).map_or(0, |chan| chan.psm);
				if psm == av::AVDTP || psm == av::AVCTP {
					out.extend(self.av_opened(at, local, psm, purpose));
				}
			}
		}
		out
	}

	fn remote_of(&self, at: usize, local: u16) -> Option<u16> {
		self.devices[at].link.as_ref()?.chans.iter().find(|chan| chan.local == local).map(|chan| chan.remote)
	}

	fn channel_data(&mut self, at: usize, cid: u16, payload: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		let Some((psm, purpose, remote, ertm)) = self.devices[at].link.as_ref().and_then(|link| link.chans.iter().find(|chan| chan.local == cid && chan.open)).map(|chan| (chan.psm, chan.purpose, chan.remote, chan.ertm.is_some())) else { return out };
		if ertm {
			return self.opp_l2cap(at, cid, remote, payload);
		}
		match (psm, purpose) {
			(0x0001, Purpose::Sdp(uuid)) => {
				// THE HOST'S ANSWER TO THIS DEVICE'S SEARCH: how many records matched.
				let name = self.short(at);
				if payload.first() == Some(&0x03) && payload.len() >= 9 {
					let total = u16::from_be_bytes([payload[5], payload[6]]);
					self.say(format!("sdp {name} found {total} records for {uuid:#06x}"));
				} else {
					self.say(format!("sdp {name} got an error for {uuid:#06x}"));
				}
				if let Some(link) = self.devices[at].link.as_mut() {
					link.chans.retain(|chan| chan.local != cid);
				}
				let mut data = remote.to_le_bytes().to_vec();
				data.extend_from_slice(&cid.to_le_bytes());
				out.extend(self.signal(at, 0x06, None, &data));
			}
			(0x0001, _) => out.extend(self.sdp_server(at, remote, payload)),
			(0x0003, _) => out.extend(self.rfcomm_receive(at, cid, remote, payload)),
			(nap::BNEP_PSM, _) if self.devices[at].spec.uuids.contains(&0x1116) => out.extend(self.nap_receive(at, remote, payload)),
			(av::AVDTP | av::AVCTP, _) => out.extend(self.av_data(at, cid, psm, payload)),
			// A PROTOCOL CHANGE on the HID control channel is answered with a successful handshake.
			(HID_CONTROL, _) => {
				let name = self.short(at);
				match payload.first() {
					Some(0x70) => self.say(format!("hid {name} set to boot protocol")),
					Some(0x71) => self.say(format!("hid {name} set to report protocol")),
					_ => {}
				}
				out.extend(self.send_pdu(at, remote, &[0x00]));
			}
			_ => {}
		}
		out
	}

	// THIS DEVICE'S SDP SERVER: one record where it has a serial port, answered to a service search attribute
	// request for the serial port class.
	fn sdp_server(&mut self, at: usize, remote: u16, request: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		if request.len() < 5 {
			return out;
		}
		let transaction = [request[1], request[2]];
		let spec = self.devices[at].spec;
		let mut asked = Vec::new();
		uuids_in(&request[5..], &mut asked);
		let mut lists = Vec::new();
		// A HID DEVICE'S RECORD: its two PSMs and, in its descriptor list, its report descriptor.
		if let Some(descriptor) = spec.hid
			&& asked.contains(&0x1124)
		{
			let descriptor = de_bytes(descriptor);
			lists.push(de_sequence(&[
				de_u16(0x0000),
				de_u32(0x0001_0001),
				de_u16(0x0001),
				de_sequence(&[de_uuid(0x1124)]),
				de_u16(0x0004),
				de_sequence(&[de_sequence(&[de_uuid(0x0100), de_u16(HID_CONTROL)]), de_sequence(&[de_uuid(0x0011)])]),
				de_u16(0x000d),
				de_sequence(&[de_sequence(&[de_sequence(&[de_uuid(0x0100), de_u16(HID_INTERRUPT)]), de_sequence(&[de_uuid(0x0011)])])]),
				de_u16(0x0206),
				de_sequence(&[de_sequence(&[de_u8(0x22), descriptor])]),
			]));
		}
		if let Some(channel) = spec.serial_channel
			&& asked.contains(&0x1101)
		{
			lists.push(de_sequence(&[
				de_u16(0x0000),
				de_u32(0x0001_0000),
				de_u16(0x0001),
				de_sequence(&[de_uuid(0x1101)]),
				de_u16(0x0004),
				de_sequence(&[de_sequence(&[de_uuid(0x0100)]), de_sequence(&[de_uuid(0x0003), de_u8(channel)])]),
				de_u16(0x0100),
				de_text("fixture serial port"),
			]));
		}
		// A NETWORK ACCESS POINT'S RECORD: BNEP on its PSM, version 1.0.
		if spec.uuids.contains(&0x1116) && asked.contains(&0x1116) {
			lists.push(de_sequence(&[
				de_u16(0x0000),
				de_u32(0x0001_0030),
				de_u16(0x0001),
				de_sequence(&[de_uuid(0x1116)]),
				de_u16(0x0004),
				de_sequence(&[de_sequence(&[de_uuid(0x0100), de_u16(nap::BNEP_PSM)]), de_sequence(&[de_uuid(0x000f), de_u16(0x0100), de_sequence(&[de_u16(0x0800), de_u16(0x0806)])])]),
				de_u16(0x0009),
				de_sequence(&[de_sequence(&[de_uuid(0x1116), de_u16(0x0100)])]),
			]));
		}
		// AN OBJECT PUSH SERVER'S RECORD: its RFCOMM channel, OBEX over it, and its GOEP L2CAP PSM where it has one.
		if let Some((channel, goep)) = spec.opp
			&& asked.contains(&0x1105)
		{
			let mut record = alloc::vec![
				de_u16(0x0000),
				de_u32(0x0001_0020),
				de_u16(0x0001),
				de_sequence(&[de_uuid(0x1105)]),
				de_u16(0x0004),
				de_sequence(&[de_sequence(&[de_uuid(0x0100)]), de_sequence(&[de_uuid(0x0003), de_u8(channel)]), de_sequence(&[de_uuid(0x0008)])]),
				de_u16(0x0009),
				de_sequence(&[de_sequence(&[de_uuid(0x1105), de_u16(0x0102)])]),
			];
			if let Some(psm) = goep {
				record.push(de_u16(0x0200));
				record.push(de_u16(psm));
			}
			record.push(de_u16(0x0303));
			record.push(de_sequence(&[de_u8(0xff)]));
			lists.push(de_sequence(&record));
		}
		// AN A2DP SINK'S RECORD - the headset's - or a source's - the phone's - on AVDTP's PSM, version 1.3.
		for class in [0x110bu16, 0x110a] {
			if spec.uuids.contains(&class) && asked.contains(&class) {
				lists.push(de_sequence(&[
					de_u16(0x0000),
					de_u32(0x0001_0010 + u32::from(class & 1)),
					de_u16(0x0001),
					de_sequence(&[de_uuid(class)]),
					de_u16(0x0004),
					de_sequence(&[de_sequence(&[de_uuid(0x0100), de_u16(av::AVDTP)]), de_sequence(&[de_uuid(0x0019), de_u16(0x0103)])]),
					de_u16(0x0009),
					de_sequence(&[de_sequence(&[de_uuid(0x110d), de_u16(0x0103)])]),
				]));
			}
		}
		let body = de_sequence(&lists);
		let name = self.short(at);
		self.say(format!("sdp {name} answered a search with {} records", lists.len()));
		let pdu_id = if request[0] == 0x06 { 0x07 } else { 0x01 };
		let mut parameters = Vec::new();
		if pdu_id == 0x07 {
			parameters.extend_from_slice(&(body.len() as u16).to_be_bytes());
			parameters.extend_from_slice(&body);
			parameters.push(0);
		} else {
			// Anything but the one request this device answers: invalid request syntax.
			parameters.extend_from_slice(&3u16.to_be_bytes());
		}
		let mut pdu = alloc::vec![pdu_id, transaction[0], transaction[1]];
		pdu.extend_from_slice(&(parameters.len() as u16).to_be_bytes());
		pdu.extend_from_slice(&parameters);
		out.extend(self.send_pdu(at, remote, &pdu));
		out
	}

	// ------------------------------------------------------------------ RFCOMM

	// Commands from this side carry C/R 1 when it opened the multiplexer and 0 when the host did; responses the
	// opposite.
	fn cr(initiator: bool, command: bool) -> bool {
		command == initiator
	}

	// A multiplexer command or response: a UIH command frame on DLCI 0, whose type byte says which it is.
	fn mcc(initiator: bool, kind: u8, command: bool, body: &[u8]) -> Vec<u8> {
		let mut info = alloc::vec![(kind << 2) | (u8::from(command) << 1) | 1, ((body.len() as u8) << 1) | 1];
		info.extend_from_slice(body);
		rfcomm_frame(0, Self::cr(initiator, true), UIH, false, None, &info)
	}

	fn rfcomm_receive(&mut self, at: usize, cid: u16, remote: u16, frame: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		if frame.len() < 4 {
			return out;
		}
		let name = self.short(at);
		let serial_channel = self.devices[at].spec.serial_channel;
		let opp_channel = self.devices[at].spec.opp.map(|(channel, _)| channel);
		let Some(link) = self.devices[at].link.as_mut() else { return out };
		let rfcomm = link.rfcomm.get_or_insert(Rfcomm { cid, initiator: false, mux: false, opening: None, dlcs: Vec::new() });
		let initiator = rfcomm.initiator;
		let dlci = frame[0] >> 2;
		let control = frame[1] & !0x10;
		let pf = frame[1] & 0x10 != 0;
		let (len, header) = if frame[2] & 1 == 1 { ((frame[2] >> 1) as usize, 3) } else { ((frame[2] >> 1) as usize | (frame[3] as usize) << 7, 4) };
		let check = if control == UIH { crc8(&frame[..2]) } else { crc8(&frame[..header]) };
		let mut sends: Vec<Vec<u8>> = Vec::new();
		let mut lines: Vec<String> = Vec::new();
		let gateway = hf::GATEWAY_CHANNEL << 1;
		let was_open = rfcomm.dlcs.iter().any(|dlc| dlc.dlci == gateway && dlc.open);
		let host_opp = opp::HOST_CHANNEL << 1;
		let push_was_open = rfcomm.dlcs.iter().any(|dlc| dlc.dlci == host_opp && dlc.open);
		let push_opening = rfcomm.opening == Some(opp::HOST_CHANNEL);
		if frame[frame.len() - 1] != check {
			lines.push(format!("rfcomm {name} dropped a frame with a bad check"));
		} else {
			let credit_dlc = rfcomm.dlcs.iter().any(|dlc| dlc.dlci == dlci && dlc.open);
			let has_credits = control == UIH && pf && dlci != 0 && credit_dlc;
			let body_start = header + usize::from(has_credits);
			if body_start + len + 1 == frame.len() {
				let info = frame[body_start..body_start + len].to_vec();
				let credits = has_credits.then(|| frame[header]);
				Self::rfcomm_frame_in(rfcomm, initiator, &[serial_channel, opp_channel], name, dlci, control, credits, &info, &mut sends, &mut lines);
			}
		}
		// WHAT ARRIVED ON THE VOICE GATEWAY'S CHANNEL is the hands-free unit's to read; on the serial port's, it is echoed
		// back as its credits allow; on any other, nobody's.
		let mut delivered = Vec::new();
		let mut opened = false;
		let mut push_opened = false;
		let mut push_answer = Vec::new();
		let mut served_bytes = Vec::new();
		let push_refused = push_opening && rfcomm.opening.is_none() && !rfcomm.dlcs.iter().any(|dlc| dlc.dlci == host_opp);
		for dlc in rfcomm.dlcs.iter_mut() {
			// THE GATEWAY IS A CHANNEL THE HEADSET OPENED: a session this side began.
			if dlc.dlci == gateway && initiator {
				opened = dlc.open && !was_open;
				delivered = core::mem::take(&mut dlc.inbox);
			} else if dlc.dlci == host_opp && initiator {
				// THE HOST'S OBJECT PUSH CHANNEL, which the phone's push opened.
				push_opened = dlc.open && !push_was_open;
				push_answer = core::mem::take(&mut dlc.inbox);
			} else if !initiator && opp_channel == Some(dlc.dlci >> 1) {
				served_bytes = core::mem::take(&mut dlc.inbox);
			} else if dlc.open && serial_channel == Some(dlc.dlci >> 1) && !dlc.inbox.is_empty() {
				let echo = core::mem::take(&mut dlc.inbox);
				dlc.queued.push_back(echo);
				while dlc.tx_credits > 0 {
					let Some(data) = dlc.queued.pop_front() else { break };
					dlc.tx_credits -= 1;
					sends.push(rfcomm_frame(dlc.dlci, Self::cr(initiator, true), UIH, false, None, &data));
				}
			} else {
				dlc.inbox.clear();
			}
		}
		for line in lines {
			self.say(line);
		}
		for send in sends {
			out.extend(self.send_pdu(at, remote, &send));
		}
		if opened {
			out.extend(self.hf_opened(at));
		}
		if !delivered.is_empty() {
			out.extend(self.hf_receive(at, &delivered));
		}
		if push_refused {
			self.opp_refused(at);
		}
		if push_opened {
			out.extend(self.opp_opened(at));
		}
		if !push_answer.is_empty() {
			out.extend(self.opp_answer(at, &push_answer));
		}
		if !served_bytes.is_empty()
			&& let Some(channel) = opp_channel
		{
			for reply in self.opp_serve(at, &served_bytes, "RFCOMM") {
				out.extend(self.rfcomm_write(at, channel, &reply));
			}
		}
		out
	}

	// One well-formed frame, on the session's own state: what to send back and what to say.
	#[allow(clippy::too_many_arguments)]
	fn rfcomm_frame_in(rfcomm: &mut Rfcomm, initiator: bool, served: &[Option<u8>], name: &str, dlci: u8, control: u8, credits: Option<u8>, info: &[u8], sends: &mut Vec<Vec<u8>>, lines: &mut Vec<String>) {
		match (dlci, control) {
			(0, SABM) => {
				rfcomm.mux = true;
				sends.push(rfcomm_frame(0, Self::cr(initiator, false), UA, true, None, &[]));
			}
			(0, UA) => {
				rfcomm.mux = true;
				// THE MULTIPLEXER IS OPEN: the parameters for the DLC this side is opening, credits asked for.
				if let Some(channel) = rfcomm.opening {
					let dlci = channel << 1;
					rfcomm.dlcs.push(Dlc { dlci, open: false, tx_credits: 0, rx_credits: u16::from(CREDITS), queued: VecDeque::new(), received: 0, msc_sent: false, inbox: Vec::new() });
					let frame_size = 127u16.to_le_bytes();
					sends.push(Self::mcc(initiator, 0x20, true, &[dlci, 0xF0, 0, 0, frame_size[0], frame_size[1], 0, CREDITS]));
				}
			}
			(0, DM) => lines.push(format!("rfcomm {name} the host refused the session")),
			(0, DISC) => {
				sends.push(rfcomm_frame(0, Self::cr(initiator, false), UA, true, None, &[]));
				rfcomm.dlcs.clear();
				rfcomm.mux = false;
			}
			(0, UIH) if info.len() >= 2 => {
				let kind = info[0] >> 2;
				let command = info[0] & 2 != 0;
				let body_len = (info[1] >> 1) as usize;
				let body = info.get(2..2 + body_len).unwrap_or(&[]).to_vec();
				match (kind, command) {
					// THE HOST NEGOTIATES A DLC on this device's server channel: credits accepted.
					(0x20, true) if body.len() >= 8 => {
						let dlci = body[0] & 0x3F;
						if !served.contains(&Some(dlci >> 1)) {
							sends.push(rfcomm_frame(dlci, Self::cr(initiator, false), DM, true, None, &[]));
						} else {
							rfcomm.dlcs.retain(|dlc| dlc.dlci != dlci);
							rfcomm.dlcs.push(Dlc { dlci, open: false, tx_credits: u16::from(body[7] & 7), rx_credits: u16::from(CREDITS), queued: VecDeque::new(), received: 0, msc_sent: false, inbox: Vec::new() });
							let mut answer = body.clone();
							answer[1] = 0xE0;
							answer[7] = CREDITS;
							sends.push(Self::mcc(initiator, 0x20, false, &answer));
						}
					}
					// THE HOST'S ANSWER TO THIS SIDE'S NEGOTIATION: then the SABM.
					(0x20, false) if body.len() >= 8 => {
						let dlci = body[0] & 0x3F;
						if let Some(dlc) = rfcomm.dlcs.iter_mut().find(|dlc| dlc.dlci == dlci) {
							dlc.tx_credits = u16::from(body[7] & 7);
							sends.push(rfcomm_frame(dlci, Self::cr(initiator, true), SABM, true, None, &[]));
						}
					}
					(0x38, true) if !body.is_empty() => {
						let dlci = body[0] >> 2;
						let mut answer = body.clone();
						answer.truncate(2);
						sends.push(Self::mcc(initiator, 0x38, false, &answer));
						if let Some(dlc) = rfcomm.dlcs.iter_mut().find(|dlc| dlc.dlci == dlci)
							&& !dlc.open
						{
							dlc.open = true;
							lines.push(format!("rfcomm {name} channel {} open, credits {}", dlci >> 1, dlc.tx_credits));
							// This side's own modem status, unless it went with its acknowledgement already.
							if !dlc.msc_sent {
								dlc.msc_sent = true;
								sends.push(Self::mcc(initiator, 0x38, true, &[(dlci << 2) | 0b11, 0x8D]));
							}
						}
					}
					(_, true) => sends.push(Self::mcc(initiator, kind, false, &body)),
					_ => {}
				}
			}
			(dlci, SABM) => {
				if let Some(dlc) = rfcomm.dlcs.iter_mut().find(|dlc| dlc.dlci == dlci) {
					dlc.open = false;
					sends.push(rfcomm_frame(dlci, Self::cr(initiator, false), UA, true, None, &[]));
				} else {
					sends.push(rfcomm_frame(dlci, Self::cr(initiator, false), DM, true, None, &[]));
				}
			}
			(dlci, UA) => {
				if let Some(dlc) = rfcomm.dlcs.iter_mut().find(|dlc| dlc.dlci == dlci) {
					// ACCEPTED: this side's modem status goes now, and the line once the host's arrives.
					dlc.msc_sent = true;
					rfcomm.opening = None;
					sends.push(Self::mcc(initiator, 0x38, true, &[(dlci << 2) | 0b11, 0x8D]));
					lines.push(format!("rfcomm {name} channel {} accepted by the host", dlci >> 1));
				}
			}
			(dlci, DM) => {
				rfcomm.dlcs.retain(|dlc| dlc.dlci != dlci);
				rfcomm.opening = None;
				lines.push(format!("rfcomm {name} channel {} refused by the host", dlci >> 1));
			}
			(dlci, DISC) => {
				rfcomm.dlcs.retain(|dlc| dlc.dlci != dlci);
				sends.push(rfcomm_frame(dlci, Self::cr(initiator, false), UA, true, None, &[]));
				lines.push(format!("rfcomm {name} channel {} closed by the host", dlci >> 1));
			}
			(dlci, UIH) => {
				let Some(dlc) = rfcomm.dlcs.iter_mut().find(|dlc| dlc.dlci == dlci) else { return };
				if let Some(granted) = credits {
					dlc.tx_credits += u16::from(granted);
					lines.push(format!("rfcomm {name} the host granted {granted} credits"));
				}
				if !info.is_empty() {
					dlc.received += info.len();
					dlc.inbox.extend_from_slice(info);
					dlc.rx_credits = dlc.rx_credits.saturating_sub(1);
				}
				// What waited for credits goes now.
				while dlc.tx_credits > 0 {
					let Some(data) = dlc.queued.pop_front() else { break };
					dlc.tx_credits -= 1;
					sends.push(rfcomm_frame(dlci, Self::cr(initiator, true), UIH, false, None, &data));
				}
				// CREDITS BACK when half are used, on an empty frame - unless the device holds a backlog it could not
				// send, as a device with a small buffer does: then the host waits, and its writer with it.
				if dlc.rx_credits <= u16::from(CREDITS) / 2 && dlc.queued.len() < BACKLOG_FRAMES {
					let grant = u16::from(CREDITS) - dlc.rx_credits;
					dlc.rx_credits += grant;
					sends.push(rfcomm_frame(dlci, Self::cr(initiator, true), UIH, true, Some(grant as u8), &[]));
				}
			}
			_ => {}
		}
	}

	// ------------------------------------------------------------------ the keyboard

	// THE KEYBOARD'S INTERRUPT CHANNEL, open: where its reports go, by the host's channel id.
	fn interrupt(&self, at: usize) -> Option<u16> {
		self.devices[at].link.as_ref()?.chans.iter().find(|chan| chan.psm == HID_INTERRUPT && chan.open).map(|chan| chan.remote)
	}

	fn keyboard_report(&self, at: usize, modifiers: u8, usage: u8) -> Option<Out> {
		let remote = self.interrupt(at)?;
		self.send_pdu(at, remote, &[HIDP_DATA_INPUT, 1, modifiers, 0, usage, 0, 0, 0, 0, 0])
	}

	// Each chord pressed and let go: its modifiers and key down, then everything up.
	fn press(&mut self, at: usize, chords: &[(u8, u8)]) -> Vec<Out> {
		let mut out = Vec::new();
		for &(modifiers, usage) in chords {
			out.extend(self.keyboard_report(at, modifiers, usage));
			out.extend(self.keyboard_report(at, 0, 0));
		}
		out
	}

	// One consumer control pressed and let go.
	fn consumer(&mut self, at: usize, usage: u16) -> Vec<Out> {
		let mut out = Vec::new();
		let Some(remote) = self.interrupt(at) else { return out };
		let bytes = usage.to_le_bytes();
		out.extend(self.send_pdu(at, remote, &[HIDP_DATA_INPUT, 2, bytes[0], bytes[1]]));
		out.extend(self.send_pdu(at, remote, &[HIDP_DATA_INPUT, 2, 0, 0]));
		out
	}

	// TYPE `text` AT `due`: what a person at the keyboard would, at the gate's time.
	pub fn type_text(&mut self, device: u8, text: &str, due: u64) -> Result<(), &'static str> {
		if device != KEYBOARD {
			return Err("only the keyboard types");
		}
		if self.typing.len() >= 8 {
			return Err("too much is already waiting to be typed");
		}
		self.typing.push((due, device, String::from(text)));
		Ok(())
	}

	// WHAT IS DUE NOW is typed, chord by chord, `{cad}` and `{power}` among the characters.
	pub fn tick(&mut self, now: u64) -> Vec<Out> {
		let mut out = Vec::new();
		while let Some(position) = self.typing.iter().position(|(due, _, _)| *due <= now) {
			let (_, device, text) = self.typing.remove(position);
			let at = usize::from(device - FIRST_DEVICE);
			if self.interrupt(at).is_none() {
				self.say(format!("{} could not type: its input is not connected", self.short(at)));
				continue;
			}
			let mut rest = text.as_str();
			while !rest.is_empty() {
				if let Some(after) = rest.strip_prefix("{cad}") {
					out.extend(self.press(at, &[(0x05, 0x4c)]));
					self.say(format!("{} pressed Ctrl+Alt+Delete", self.short(at)));
					rest = after;
					continue;
				}
				if let Some(after) = rest.strip_prefix("{power}") {
					out.extend(self.consumer(at, 0x0030));
					self.say(format!("{} pressed its Power key", self.short(at)));
					rest = after;
					continue;
				}
				let mut characters = rest.chars();
				let character = characters.next().unwrap_or(' ');
				rest = characters.as_str();
				if let Some((usage, shift)) = usage_of(character) {
					out.extend(self.press(at, &[(if shift { 0x02 } else { 0 }, usage)]));
				}
			}
			self.say(format!("{} typed {:?}", self.short(at), text));
		}
		self.now = now;
		out.extend(self.phone_tick(now));
		out.extend(self.sco_tick(now));
		out.extend(self.opp_tick(now));
		out
	}

	// BYTES ON ONE OPEN DLC to the host, as its credits allow; the rest wait for the host's.
	fn rfcomm_write(&mut self, at: usize, channel: u8, bytes: &[u8]) -> Vec<Out> {
		let mut out = Vec::new();
		let Some(link) = self.devices[at].link.as_mut() else { return out };
		let Some(rfcomm) = link.rfcomm.as_mut() else { return out };
		let cid = rfcomm.cid;
		let cr = Self::cr(rfcomm.initiator, true);
		let Some(dlc) = rfcomm.dlcs.iter_mut().find(|dlc| dlc.dlci >> 1 == channel && dlc.open) else { return out };
		let dlci = dlc.dlci;
		// FRAMES OF AT MOST 127 BYTES: what this side asks for when it opens a DLC, and less than the host takes.
		for piece in bytes.chunks(127) {
			dlc.queued.push_back(piece.to_vec());
		}
		let mut sends = Vec::new();
		while dlc.tx_credits > 0 {
			let Some(data) = dlc.queued.pop_front() else { break };
			dlc.tx_credits -= 1;
			sends.push(rfcomm_frame(dlci, cr, UIH, false, None, &data));
		}
		let Some(remote) = self.remote_of(at, cid) else { return out };
		for send in sends {
			out.extend(self.send_pdu(at, remote, &send));
		}
		out
	}

	// SEND `frames` FRAMES of 16 bytes on the device's open DLC, as its credits allow; the rest wait for the host's.
	fn rfcomm_send(&mut self, at: usize, frames: usize) -> Vec<Out> {
		let mut out = Vec::new();
		let name = self.short(at);
		let Some(link) = self.devices[at].link.as_mut() else { return out };
		let Some(rfcomm) = link.rfcomm.as_mut() else { return out };
		let cid = rfcomm.cid;
		let cr = Self::cr(rfcomm.initiator, true);
		let Some(dlc) = rfcomm.dlcs.iter_mut().find(|dlc| dlc.open) else { return out };
		let dlci = dlc.dlci;
		for index in 0..frames {
			dlc.queued.push_back(alloc::vec![b'a' + (index % 26) as u8; 16]);
		}
		let mut sends = Vec::new();
		while dlc.tx_credits > 0 {
			let Some(data) = dlc.queued.pop_front() else { break };
			dlc.tx_credits -= 1;
			sends.push(rfcomm_frame(dlci, cr, UIH, false, None, &data));
		}
		let waiting = dlc.queued.len();
		self.say(format!("rfcomm {name} sent {} frames, {waiting} wait for credits", sends.len()));
		let Some(remote) = self.remote_of(at, cid) else { return out };
		for send in sends {
			out.extend(self.send_pdu(at, remote, &send));
		}
		out
	}
}

fn handle_of(params: &[u8]) -> u16 {
	params.get(..2).map_or(0, |bytes| u16::from_le_bytes([bytes[0], bytes[1]]) & 0x0FFF)
}
