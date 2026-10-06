// THE EMULATED LE AUDIO WORLD: the LE Audio half of the in-guest Bluetooth fixture's controller - its connected and
// broadcast isochronous streams, its extended and periodic scanning - and the three LE Audio devices on the far side of
// its radio.
//
// A TEST FIXTURE, DEVELOPMENT-ONLY, AND ITS PROTOCOLS ARE ITS OWN. The host's Basic Audio Profile, its stream control,
// its coordinated sets and its call bearer live in `service_logic`; nothing here reads or reuses them. Every HCI layout
// below is Core 5.4 Vol 4 Part E's, the stream endpoints, capability records, set key, volume server and call bearer
// client are written from ASCS, PACS, CSIS, VCS and TBS, the announcements from BAP - and the set's functions are held
// to CSIS's own sample data - so a host and a fixture that agree are two implementations agreeing.
//
// THE DEVICES, numbered for the control endpoint after the LE world's:
//
//   12 earbud L    NoInputNoOutput, Secure Connections    Just Works; a sink and a microphone, front left; set rank 1
//   13 earbud R    NoInputNoOutput, Secure Connections    Just Works; a sink, front right; set rank 2
//   14 broadcast   not connectable                         a Broadcast Source, silent until `broadcast`
//
// THE EARBUDS ARE ONE COORDINATED SET OF TWO. They advertise connectable extended advertising from their identity, with
// a Resolvable Set Identifier hashed from the set's key, so a host that bonded one and read the key finds the other.
// Their pairing is the LE world's responder (`bt_le_world`); their attribute servers and the call bearer client each
// runs against the host's attribute server are `earbud`'s; the broadcast source and the sync this controller keeps with
// it are `broadcast`'s.
//
// TWO KINDS OF CONTROLLER. The fixture starts as a plain LE controller - the LE Audio commands unknown to it, its
// earbuds heard in legacy advertising - so every gate written before LE Audio runs the path it always ran. The gate's
// `le-features` makes it the LE Audio one, or plain again, from the next HCI Reset, as a controller's firmware would
// change under a host that powers the radio off and on.
//
// WHAT THE LE AUDIO CONTROLLER DOES HERE: it answers the LE Audio commands - features, buffers, the extended scan and connection,
// a connected isochronous group and its streams, the ISO data paths, the periodic sync and the broadcast sync - with
// the events the specification lays out, carries ISO data both ways, and keeps the rule real controllers keep: once a
// host has used one extended advertising, scanning or initiating command, the legacy ones are refused until a reset.

use crate::bt_peer::{aes128, cmac};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

// The earbuds' attribute servers - PACS, ASCS, CSIS, VCS - and the call bearer client each runs against the host.
mod earbud;
// The broadcast source, its announcements, and the periodic and broadcast isochronous sync this controller keeps with it.
mod broadcast;

pub use earbud::{Codec, Earbud, SIDES, Side, codec_configuration};

#[cfg(test)]
mod tests;

// The devices' numbers on the control endpoint.
pub const EARBUD_L: u8 = 12;
pub const EARBUD_R: u8 = 13;
pub const BROADCAST_SOURCE: u8 = 14;
// THE SET'S IDENTITY RESOLVING KEY, most significant first: fixed, and shared by both earbuds.
pub const SIRK: [u8; 16] = *b"fixture-set-sirk";
// The broadcast source's static random address, most significant first; its Broadcast_ID, name and Broadcast Code.
pub const BROADCAST_ADDRESS: [u8; 6] = [0xd0, 0x1b, 0xdc, 0x20, 0x00, 0x0e];
pub const BROADCAST_ID: u32 = 0x12_3456;
pub const BROADCAST_NAME: &str = "fixture broadcast";
pub const BROADCAST_CODE: [u8; 16] = *b"fixture-code-016";
// The handles this controller gives a CIS and a BIS: past every ACL handle the fixture hands out.
const FIRST_CIS_HANDLE: u16 = 0x0100;
const FIRST_BIS_HANDLE: u16 = 0x0180;
// SDUs a late stream catches up in one tick before it skips ahead: a few, never a flood.
const CATCH_UP: u32 = 3;
// The LE features this controller has: LE Encryption (0), LE Extended Advertising (12), LE Periodic Advertising (13),
// Connected Isochronous Stream - Central (28) and Synchronized Receiver (31).
pub const FEATURES: u64 = 1 | 1 << 12 | 1 << 13 | 1 << 28 | 1 << 31;
// The LE ACL and ISO buffers LE Read Buffer Size v2 reports.
pub const ACL_BYTES: u16 = 251;
pub const ACL_BUFFERS: u8 = 4;
pub const ISO_BYTES: u16 = 251;
pub const ISO_BUFFERS: u8 = 8;

// ------------------------------------------------------------------ CSIS's security toolbox, most significant first

// `s1`, the salt: AES-CMAC under the all-zero key over M.
pub fn salt(m: &[u8]) -> [u8; 16] {
	cmac(&[0u8; 16], m)
}

// `k1(N, SALT, P)`: T = AES-CMAC under SALT over N, then AES-CMAC under T over P.
pub fn k1(n: &[u8], salt: &[u8; 16], p: &[u8]) -> [u8; 16] {
	let t = cmac(salt, n);
	cmac(&t, p)
}

// `sef(K, SIRK)`: the set key encrypted under K - on LE, the bond's LTK - as the Set Identity Resolving Key
// characteristic exposes it. It is its own inverse, which is what the client's `sdf` is.
pub fn sef(k: &[u8; 16], sirk: &[u8; 16]) -> [u8; 16] {
	let mask = k1(k, &salt(b"SIRKenc"), b"csis");
	core::array::from_fn(|at| mask[at] ^ sirk[at])
}

// `sih(k, r)`: the low twenty-four bits of e(k, r padded with zeros above), most significant first.
pub fn sih(k: &[u8; 16], r: [u8; 3]) -> [u8; 3] {
	let mut block = [0u8; 16];
	block[13..].copy_from_slice(&r);
	let out = aes128(k, &block);
	[out[13], out[14], out[15]]
}

// THE RESOLVABLE SET IDENTIFIER as its AD structure carries it, least significant first: `hash || prand`, the hash in the
// low three octets. `prand` is most significant first, `01` in its top two bits.
pub fn rsi(sirk: &[u8; 16], prand: [u8; 3]) -> [u8; 6] {
	let hash = sih(sirk, prand);
	[hash[2], hash[1], hash[0], prand[2], prand[1], prand[0]]
}

// Whether an RSI (as the AD structure carries it) resolves under a set key.
pub fn resolves(sirk: &[u8; 16], rsi: &[u8; 6]) -> bool {
	let prand = [rsi[5], rsi[4], rsi[3]];
	sih(sirk, prand) == [rsi[2], rsi[1], rsi[0]] && prand[0] >> 6 == 0b01
}

// ------------------------------------------------------------------ the encodings

// LENGTH-TYPE-VALUE STRUCTURES, as codec configurations, capabilities and metadata carry them: a length octet counting
// the type and the value, then the type. `Err` with the type of the structure that overruns, or zero where it has none.
pub fn ltvs(mut bytes: &[u8]) -> Result<Vec<(u8, &[u8])>, u8> {
	let mut out = Vec::new();
	while let Some((&len, rest)) = bytes.split_first() {
		let len = usize::from(len);
		if len == 0 || len > rest.len() {
			return Err(rest.first().copied().unwrap_or(0));
		}
		out.push((rest[0], &rest[1..len]));
		bytes = &rest[len..];
	}
	Ok(out)
}

// A 24-bit little-endian field.
fn le24(value: u32) -> [u8; 3] {
	let bytes = value.to_le_bytes();
	[bytes[0], bytes[1], bytes[2]]
}

fn u24(bytes: &[u8]) -> u32 {
	u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
	bytes.get(at..at + 2).map_or(0, |pair| u16::from_le_bytes([pair[0], pair[1]]))
}

// What this world sends: an HCI event, an ATT PDU an earbud sends on its link (by earbud), and an ISO data packet to the
// host.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AudioOut {
	Event(Vec<u8>),
	Att(usize, Vec<u8>),
	Iso(Vec<u8>),
}

fn event(code: u8, params: &[u8]) -> AudioOut {
	let mut bytes = alloc::vec![code, params.len() as u8];
	bytes.extend_from_slice(params);
	AudioOut::Event(bytes)
}

fn meta(subevent: u8, params: &[u8]) -> AudioOut {
	let mut body = alloc::vec![subevent];
	body.extend_from_slice(params);
	event(0x3e, &body)
}

fn complete(opcode: u16, params: &[u8]) -> AudioOut {
	let mut body = alloc::vec![1];
	body.extend_from_slice(&opcode.to_le_bytes());
	body.extend_from_slice(params);
	event(0x0e, &body)
}

fn status(opcode: u16, code: u8) -> AudioOut {
	let op = opcode.to_le_bytes();
	event(0x0f, &[code, 1, op[0], op[1]])
}

// THE CONTROLLER HAS SENT ONE PACKET'S BUFFER: Number Of Completed Packets for one handle.
fn completed(handle: u16) -> AudioOut {
	let h = handle.to_le_bytes();
	event(0x13, &[1, h[0], h[1], 1, 0])
}

// ------------------------------------------------------------------ what a scan hears

// ONE ADVERTISER AS A SCAN HEARS IT, in either report's form. The event type is the extended report's: connectable,
// scannable, directed, scan response, legacy, and the data status above them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Advert {
	pub event_type: u16,
	pub address_type: u8,
	// Most significant first.
	pub address: [u8; 6],
	// 0 where nothing is sent on a secondary channel - every legacy PDU - and 1 for LE 1M.
	pub secondary_phy: u8,
	// 0xff where the PDU has no ADI - every legacy PDU.
	pub sid: u8,
	pub rssi: i8,
	// In units of 1.25 ms; zero where there is no periodic advertising.
	pub periodic_interval: u16,
	pub data: Vec<u8>,
}

// A legacy ADV_IND as the extended report names it: connectable, scannable, legacy.
pub const LEGACY_CONNECTABLE: u16 = 0x0013;

impl Advert {
	// A CONNECTABLE LEGACY ADVERTISER, as the mouse and the LE world's first devices are.
	pub fn legacy(address_type: u8, address: [u8; 6], rssi: i8, data: &[u8]) -> Advert {
		Advert { event_type: LEGACY_CONNECTABLE, address_type, address, secondary_phy: 0, sid: 0xff, rssi, periodic_interval: 0, data: data.to_vec() }
	}

	pub fn is_legacy(&self) -> bool {
		self.event_type & 0x0010 != 0
	}

	// THE LEGACY REPORT'S PARAMETERS (LE Advertising Report, subevent 0x02): one report, an ADV_IND.
	pub fn legacy_report(&self) -> Vec<u8> {
		let mut report = alloc::vec![1, 0x00, self.address_type];
		let mut wire = self.address;
		wire.reverse();
		report.extend_from_slice(&wire);
		report.push(self.data.len() as u8);
		report.extend_from_slice(&self.data);
		report.push(self.rssi as u8);
		report
	}

	// THE EXTENDED REPORT'S PARAMETERS (LE Extended Advertising Report, subevent 0x0d, Core Vol 4 Part E 7.7.65.13): one
	// report - its event type, the address, the primary PHY (LE 1M), the secondary PHY, the SID, no TX power, the RSSI,
	// the periodic interval, no direct address, and the data.
	pub fn extended_report(&self) -> Vec<u8> {
		let mut report = alloc::vec![1];
		report.extend_from_slice(&self.event_type.to_le_bytes());
		report.push(self.address_type);
		let mut wire = self.address;
		wire.reverse();
		report.extend_from_slice(&wire);
		report.extend_from_slice(&[0x01, self.secondary_phy, self.sid, 0x7f, self.rssi as u8]);
		report.extend_from_slice(&self.periodic_interval.to_le_bytes());
		report.extend_from_slice(&[0x00, 0, 0, 0, 0, 0, 0]);
		report.push(self.data.len() as u8);
		report.extend_from_slice(&self.data);
		report
	}
}

// ------------------------------------------------------------------ ISO data packets (Core Vol 4 Part E 5.4.5)

// ONE ISO DATA PACKET FROM THE HOST: its handle, its packet boundary flag, and - where it starts an SDU - its sequence
// number and the SDU (or the SDU's first fragment).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IsoIn<'a> {
	pub handle: u16,
	pub boundary: u8,
	pub sequence: u16,
	pub sdu: &'a [u8],
}

// THE PACKET READ: handle, PB and TS flags, the 14-bit load length, an optional time stamp, then - for a first fragment
// or a complete SDU - the sequence number and the 12-bit SDU length. `None` where the lengths disagree.
pub fn iso_in(bytes: &[u8]) -> Option<IsoIn<'_>> {
	if bytes.len() < 4 {
		return None;
	}
	let header = u16::from_le_bytes([bytes[0], bytes[1]]);
	let length = usize::from(u16::from_le_bytes([bytes[2], bytes[3]]) & 0x3fff);
	if bytes.len() != 4 + length {
		return None;
	}
	let (handle, boundary, stamped) = (header & 0x0fff, ((header >> 12) & 0b11) as u8, header & 0x4000 != 0);
	let mut load = &bytes[4..];
	if stamped {
		load = load.get(4..)?;
	}
	// A CONTINUATION OR A LAST FRAGMENT carries neither.
	if boundary & 0b01 != 0 {
		return Some(IsoIn { handle, boundary, sequence: 0, sdu: load });
	}
	if load.len() < 4 {
		return None;
	}
	let sequence = u16::from_le_bytes([load[0], load[1]]);
	let sdu_length = usize::from(u16::from_le_bytes([load[2], load[3]]) & 0x0fff);
	let sdu = &load[4..];
	let whole = if boundary == 0b10 { sdu.len() == sdu_length } else { sdu.len() < sdu_length };
	whole.then_some(IsoIn { handle, boundary, sequence, sdu })
}

// ONE COMPLETE SDU TO THE HOST: PB 0b10, no time stamp, the sequence number, the SDU length with its status flag valid.
pub fn iso_packet(handle: u16, sequence: u16, sdu: &[u8]) -> Vec<u8> {
	let mut packet = Vec::with_capacity(8 + sdu.len());
	packet.extend_from_slice(&((handle & 0x0fff) | (0b10 << 12)).to_le_bytes());
	packet.extend_from_slice(&((4 + sdu.len()) as u16).to_le_bytes());
	packet.extend_from_slice(&sequence.to_le_bytes());
	packet.extend_from_slice(&((sdu.len() as u16) & 0x0fff).to_le_bytes());
	packet.extend_from_slice(sdu);
	packet
}

// ONE STREAM'S PACE on the fixture's clock - a hundred ticks a second, one SDU a tick - and its sequence number.
#[derive(Clone, Copy, Default, Debug)]
struct Pace {
	next: u64,
	sequence: u16,
	running: bool,
}

impl Pace {
	// HOW MANY SDUs ARE DUE AT `now`: the one this tick owes, and a few late ones; a stream further behind skips ahead.
	fn due(&mut self, now: u64) -> u32 {
		if !self.running {
			self.running = true;
			self.next = now;
		}
		let mut due = 0;
		while self.next <= now && due < CATCH_UP {
			self.next += 1;
			due += 1;
		}
		if self.next <= now {
			self.next = now + 1;
		}
		due
	}

	fn stop(&mut self) {
		self.running = false;
	}

	fn take(&mut self) -> u16 {
		let sequence = self.sequence;
		self.sequence = self.sequence.wrapping_add(1);
		sequence
	}
}

// ------------------------------------------------------------------ the connected isochronous groups

// ONE CIS AS THE HOST CONFIGURED IT: its parameters each way (central to peripheral first), the link it was created on,
// whether it is established, its two data paths (input from the host, output to it) and its outgoing pace.
struct Cis {
	id: u8,
	handle: u16,
	max_sdu: [u16; 2],
	phy: [u8; 2],
	rtn: [u8; 2],
	acl: Option<u16>,
	established: bool,
	paths: [bool; 2],
	pace: Pace,
}

struct Cig {
	id: u8,
	sdu_interval: [u32; 2],
	framing: u8,
	latency: [u16; 2],
	cises: Vec<Cis>,
}

// WHAT THE LE WORLD TELLS THIS ONE about the links: each earbud's - its handle, whether it is encrypted, the key it
// holds - and every LE link handle there is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EarLink {
	pub handle: u16,
	pub encrypted: bool,
	pub ltk: Option<[u8; 16]>,
}

#[derive(Clone, Default, Debug)]
pub struct Ctx {
	pub links: [Option<EarLink>; 2],
	pub acls: Vec<u16>,
}

impl Ctx {
	fn ear_of(&self, acl: u16) -> Option<usize> {
		self.links.iter().position(|link| link.is_some_and(|link| link.handle == acl))
	}
}

// The extended scan: whether it is on, and when a scan with a duration ends.
#[derive(Clone, Copy, Default, Debug)]
struct Scan {
	on: bool,
	until: Option<u64>,
}

pub struct Audio {
	pub earbuds: [Earbud; 2],
	// THE CONTROLLER'S KIND - LE Audio or plain LE - and the kind the next HCI Reset makes it.
	le_audio: bool,
	next_kind: Option<bool>,
	cigs: Vec<Cig>,
	// THE EXTENDED-MODE RULE'S STATE: an extended advertising, scanning or initiating command since the last reset.
	extended: bool,
	scan: Scan,
	source: broadcast::Source,
	receiver: broadcast::Receiver,
	random: u64,
	now: u64,
	log: Vec<String>,
}

// The PHY a CIS's PHY bitfield names, as the established event reports it: the lowest set.
fn phy_of(bits: u8) -> u8 {
	if bits & 0x01 != 0 {
		1
	} else if bits & 0x02 != 0 {
		2
	} else {
		3
	}
}

impl Audio {
	pub fn new(seed: u64) -> Audio {
		let mut audio = Audio { earbuds: [Earbud::new(&SIDES[0]), Earbud::new(&SIDES[1])], le_audio: false, next_kind: None, cigs: Vec::new(), extended: false, scan: Scan::default(), source: broadcast::Source::default(), receiver: broadcast::Receiver::default(), random: seed | 1, now: 0, log: Vec::new() };
		audio.new_identifiers();
		audio
	}

	fn next(&mut self) -> u64 {
		self.random ^= self.random >> 12;
		self.random ^= self.random << 25;
		self.random ^= self.random >> 27;
		self.random.wrapping_mul(0x2545_F491_4F6C_DD1D)
	}

	pub fn take_log(&mut self) -> Vec<String> {
		core::mem::take(&mut self.log)
	}

	// THE SET IDENTIFIERS THE EARBUDS ADVERTISE, each from a fresh prand: `01` on top, its twenty-two random bits neither
	// all zero nor all one.
	fn new_identifiers(&mut self) {
		for at in 0..self.earbuds.len() {
			let prand = loop {
				let random = self.next();
				let prand = [0x40 | (random as u8 & 0x3f), (random >> 8) as u8, (random >> 16) as u8];
				let bits = u32::from_be_bytes([0, prand[0] & 0x3f, prand[1], prand[2]]);
				if bits != 0 && bits != 0x3f_ffff {
					break prand;
				}
			};
			self.earbuds[at].rsi = rsi(&SIRK, prand);
		}
	}

	// A RESET ENDS WHAT THE LINK LAYER WAS DOING: the groups and their streams, the scans and the syncs - with no event -
	// and the extended-mode rule starts over. The broadcast source is another device and goes on broadcasting.
	pub fn reset(&mut self) {
		self.cigs.clear();
		self.extended = false;
		self.scan = Scan::default();
		self.receiver = broadcast::Receiver::default();
		for earbud in self.earbuds.iter_mut() {
			earbud.link_lost(&mut self.log);
		}
		self.new_identifiers();
	}

	pub fn extended(&self) -> bool {
		self.extended
	}

	pub fn le_audio(&self) -> bool {
		self.le_audio
	}

	// THE GATE CHOOSES THE CONTROLLER'S KIND, for the next HCI Reset to apply.
	pub fn choose(&mut self, le_audio: bool) {
		self.next_kind = Some(le_audio);
		self.log.push(String::from(if le_audio { "controller will be an LE Audio one from its next reset" } else { "controller will be a plain LE one from its next reset" }));
	}

	// AN HCI RESET: the kind the gate chose takes effect, and the link layer starts over.
	pub fn hci_reset(&mut self) {
		if let Some(kind) = self.next_kind.take()
			&& kind != self.le_audio
		{
			self.le_audio = kind;
			self.log.push(String::from(if kind { "controller reset as an LE Audio one" } else { "controller reset as a plain LE one" }));
		}
		self.reset();
	}

	// ------------------------------------------------------------------ commands

	// ONE HCI COMMAND, if it is one this half of the controller answers; `None` for anything else.
	pub fn command(&mut self, opcode: u16, params: &[u8], ctx: &Ctx) -> Option<Vec<AudioOut>> {
		// A PLAIN LE CONTROLLER knows none of them: the fixture answers them as commands it does not have.
		if !self.le_audio {
			return None;
		}
		if (0x2036..=0x204a).contains(&opcode) {
			self.extended = true;
		}
		let mut out = Vec::new();
		match opcode {
			// THE EXTENDED-MODE RULE, as real controllers enforce it: once the host used an extended advertising, scanning
			// or initiating command, the legacy scan and initiation are Command Disallowed until a reset.
			0x200b | 0x200c if self.extended => out.push(complete(opcode, &[0x0c])),
			0x200d if self.extended => out.push(status(opcode, 0x0c)),
			0x2003 => {
				let mut answer = alloc::vec![0];
				answer.extend_from_slice(&FEATURES.to_le_bytes());
				out.push(complete(opcode, &answer));
			}
			0x2060 => {
				let (acl, iso) = (ACL_BYTES.to_le_bytes(), ISO_BYTES.to_le_bytes());
				out.push(complete(opcode, &[0, acl[0], acl[1], ACL_BUFFERS, iso[0], iso[1], ISO_BUFFERS]));
			}
			0x2074 => out.push(complete(opcode, &[0])),
			0x2062 => out.push(self.set_cig(params)),
			0x2064 => out.extend(self.create_cis(params, ctx)),
			0x2065 => out.push(self.remove_cig(params)),
			0x206e => out.push(self.setup_path(params)),
			0x206f => out.push(self.remove_path(params)),
			// A DISCONNECT OF A CIS: its completion, and the earbud's endpoints let go of it.
			0x0406 if params.len() == 3 && self.cis_at(u16_at(params, 0) & 0x0fff).is_some() => out.extend(self.disconnect_cis(u16_at(params, 0) & 0x0fff, ctx)),
			0x2044..=0x204a | 0x2059 | 0x206b | 0x206c => out.extend(self.receiver_command(opcode, params)),
			_ => return None,
		}
		Some(out)
	}

	// LE SET EXTENDED SCAN PARAMETERS: the own address type, the filter policy, the PHYs - LE 1M and LE Coded only - and
	// five octets for each. Not while scanning.
	pub fn extended_scan_parameters(&mut self, params: &[u8]) -> u8 {
		self.extended = true;
		let Some(&phys) = params.get(2) else { return 0x12 };
		if phys == 0 || phys & !0x05 != 0 || params.len() != 3 + 5 * phys.count_ones() as usize || params[0] > 3 {
			return 0x12;
		}
		if self.scan.on {
			return 0x0c;
		}
		0
	}

	// LE SET EXTENDED SCAN ENABLE: on or off, with a duration in units of 10 ms - one tick each - after which the scan
	// ends with an LE Scan Timeout. The status, and whether a scan began, so the fixture reports what it hears.
	pub fn extended_scan_enable(&mut self, params: &[u8], now: u64) -> (u8, bool) {
		self.extended = true;
		if params.len() != 6 || params[0] > 1 || params[1] > 2 {
			return (0x12, false);
		}
		if params[0] == 0 {
			self.scan = Scan::default();
			return (0, false);
		}
		let duration = u16_at(params, 2);
		self.scan = Scan { on: true, until: (duration != 0).then(|| now + u64::from(duration)) };
		(0, true)
	}

	pub fn scanning_extended(&self) -> bool {
		self.scan.on
	}

	// ------------------------------------------------------------------ CIG and CIS (Core 7.8.97 ff.)

	fn cis_at(&self, handle: u16) -> Option<(usize, usize)> {
		self.cigs.iter().enumerate().find_map(|(c, cig)| cig.cises.iter().position(|cis| cis.handle == handle).map(|s| (c, s)))
	}

	fn free_cis_handle(&self) -> u16 {
		(FIRST_CIS_HANDLE..FIRST_BIS_HANDLE).find(|handle| self.cis_at(*handle).is_none()).unwrap_or(FIRST_CIS_HANDLE)
	}

	// LE SET CIG PARAMETERS: a group and its streams configured, or one reconfigured while none of its streams is
	// established; answered with a handle per CIS, in the order the command named them.
	fn set_cig(&mut self, params: &[u8]) -> AudioOut {
		let opcode = 0x2062;
		let id = params.first().copied().unwrap_or(0);
		let refuse = |code: u8| complete(opcode, &[code, id, 0]);
		if params.len() < 15 || params.len() != 15 + 9 * usize::from(params[14]) {
			return refuse(0x12);
		}
		let sdu_interval = [u24(&params[1..4]), u24(&params[4..7])];
		let (packing, framing) = (params[8], params[9]);
		let latency = [u16_at(params, 10), u16_at(params, 12)];
		let count = usize::from(params[14]);
		let entries: Vec<&[u8]> = params[15..].chunks(9).collect();
		let valid_entry = |entry: &&[u8]| entry[0] <= 0xef && u16_at(entry, 1) <= 0x0fff && u16_at(entry, 3) <= 0x0fff && (1..=7).contains(&entry[5]) && (1..=7).contains(&entry[6]) && (u16_at(entry, 1) != 0 || u16_at(entry, 3) != 0);
		let unique = entries.iter().enumerate().all(|(at, entry)| entries[..at].iter().all(|other| other[0] != entry[0]));
		let ranges = sdu_interval.iter().all(|interval| (0xff..=0xf_ffff).contains(interval)) && latency.iter().all(|latency| (5..=0x0fa0).contains(latency));
		if id > 0xef || !(1..=0x1f).contains(&count) || packing > 1 || framing > 1 || !ranges || !unique || !entries.iter().all(valid_entry) {
			return refuse(0x12);
		}
		// A GROUP WITH A STREAM UP is not configurable.
		let existing = self.cigs.iter().position(|cig| cig.id == id);
		if existing.is_some_and(|at| self.cigs[at].cises.iter().any(|cis| cis.established)) {
			return refuse(0x0c);
		}
		let at = match existing {
			Some(at) => at,
			None => {
				self.cigs.push(Cig { id, sdu_interval, framing, latency, cises: Vec::new() });
				self.cigs.len() - 1
			}
		};
		self.cigs[at].sdu_interval = sdu_interval;
		self.cigs[at].framing = framing;
		self.cigs[at].latency = latency;
		let mut answer = alloc::vec![0, id, count as u8];
		for entry in entries {
			let (max_sdu, phy, rtn) = ([u16_at(entry, 1), u16_at(entry, 3)], [entry[5], entry[6]], [entry[7], entry[8]]);
			let handle = match self.cigs[at].cises.iter_mut().find(|cis| cis.id == entry[0]) {
				Some(cis) => {
					cis.max_sdu = max_sdu;
					cis.phy = phy;
					cis.rtn = rtn;
					cis.handle
				}
				None => {
					let handle = self.free_cis_handle();
					self.cigs[at].cises.push(Cis { id: entry[0], handle, max_sdu, phy, rtn, acl: None, established: false, paths: [false; 2], pace: Pace::default() });
					handle
				}
			};
			answer.extend_from_slice(&handle.to_le_bytes());
		}
		complete(opcode, &answer)
	}

	// LE CREATE CIS: pairs of a CIS handle and the ACL it runs beside. The command is refused outright for a handle that
	// names nothing or a CIS already up; otherwise each pair ends in its own LE CIS Established - success where the ACL is
	// an encrypted link to an earbud with an endpoint Enabling for that CIS, the peripheral's refusal otherwise.
	fn create_cis(&mut self, params: &[u8], ctx: &Ctx) -> Vec<AudioOut> {
		let opcode = 0x2064;
		let count = usize::from(params.first().copied().unwrap_or(0));
		if count == 0 || count > 0x1f || params.len() != 1 + 4 * count {
			return alloc::vec![status(opcode, 0x12)];
		}
		let pairs: Vec<(u16, u16)> = params[1..].chunks(4).map(|pair| (u16_at(pair, 0) & 0x0fff, u16_at(pair, 2) & 0x0fff)).collect();
		let mut found = Vec::new();
		for (at, &(cis, acl)) in pairs.iter().enumerate() {
			let Some(position) = self.cis_at(cis) else { return alloc::vec![status(opcode, 0x02)] };
			if !ctx.acls.contains(&acl) {
				return alloc::vec![status(opcode, 0x02)];
			}
			if self.cigs[position.0].cises[position.1].established || pairs[..at].iter().any(|other| other.0 == cis) {
				return alloc::vec![status(opcode, 0x0b)];
			}
			found.push((position, acl));
		}
		let mut out = alloc::vec![status(opcode, 0)];
		for ((c, s), acl) in found {
			out.extend(self.establish(c, s, acl, ctx));
		}
		out
	}

	fn establish(&mut self, c: usize, s: usize, acl: u16, ctx: &Ctx) -> Vec<AudioOut> {
		let (cig_id, cis_id, handle) = (self.cigs[c].id, self.cigs[c].cises[s].id, self.cigs[c].cises[s].handle);
		let ear = ctx.ear_of(acl);
		let accepted = ear.is_some_and(|ear| ctx.links[ear].is_some_and(|link| link.encrypted) && self.earbuds[ear].enabling(cig_id, cis_id));
		// THE PERIPHERAL'S ANSWER: a device that is not an LE Audio one does not have the feature; an earbud with no
		// endpoint waiting for this stream refuses it, as an ASCS server does.
		let code = match ear {
			None => 0x1a,
			Some(_) if accepted => 0,
			Some(_) => 0x0d,
		};
		let mut body = alloc::vec![code];
		body.extend_from_slice(&handle.to_le_bytes());
		if code == 0 {
			body.extend(self.cis_timing(c, s));
		} else {
			body.extend_from_slice(&[0; 25]);
		}
		let mut out = alloc::vec![meta(0x19, &body)];
		let Some(ear) = ear.filter(|_| accepted) else { return out };
		let cis = &mut self.cigs[c].cises[s];
		cis.established = true;
		cis.acl = Some(acl);
		cis.paths = [false; 2];
		cis.pace = Pace::default();
		let link = ctx.links[ear].expect("an earbud's link");
		for pdu in self.earbuds[ear].cis_up(cig_id, cis_id, &link, &mut self.log) {
			out.push(AudioOut::Att(ear, pdu));
		}
		out
	}

	// LE CIS ESTABLISHED'S PARAMETERS after its status and handle (Core 7.7.65.25): the group's and the stream's sync
	// delays, the transport latency each way, the PHYs, NSE, the burst numbers, the flush timeouts, the PDU sizes and the
	// ISO interval - a millisecond of the interval for each stream of the group, one try each way and a retransmission
	// for every RTN.
	fn cis_timing(&self, c: usize, s: usize) -> Vec<u8> {
		let cig = &self.cigs[c];
		let cis = &cig.cises[s];
		let interval = if cis.max_sdu[0] != 0 { cig.sdu_interval[0] } else { cig.sdu_interval[1] };
		let iso_interval = (interval.div_ceil(1250)).clamp(4, 0x0c80) as u16;
		let group_delay = 1000 * cig.cises.len() as u32;
		let stream_delay = 1000 * (cig.cises.len() - s) as u32;
		let latency = |direction: usize| group_delay + u32::from(iso_interval) * 1250 - cig.sdu_interval[direction].min(u32::from(iso_interval) * 1250);
		let nse = (cis.rtn[0].max(cis.rtn[1]).saturating_add(1)).clamp(1, 0x1f);
		let bn = |direction: usize| u8::from(cis.max_sdu[direction] != 0);
		let mut out = Vec::with_capacity(26);
		out.extend_from_slice(&le24(group_delay));
		out.extend_from_slice(&le24(stream_delay));
		out.extend_from_slice(&le24(latency(0)));
		out.extend_from_slice(&le24(latency(1)));
		out.extend_from_slice(&[phy_of(cis.phy[0]), phy_of(cis.phy[1]), nse, bn(0), bn(1), 1, 1]);
		out.extend_from_slice(&cis.max_sdu[0].to_le_bytes());
		out.extend_from_slice(&cis.max_sdu[1].to_le_bytes());
		out.extend_from_slice(&iso_interval.to_le_bytes());
		out
	}

	// LE REMOVE CIG: refused while any of its streams is established.
	fn remove_cig(&mut self, params: &[u8]) -> AudioOut {
		let opcode = 0x2065;
		let Some(&id) = params.first().filter(|_| params.len() == 1) else { return complete(opcode, &[0x12, 0]) };
		let Some(at) = self.cigs.iter().position(|cig| cig.id == id) else { return complete(opcode, &[0x02, id]) };
		if self.cigs[at].cises.iter().any(|cis| cis.established) {
			return complete(opcode, &[0x0c, id]);
		}
		self.cigs.remove(at);
		complete(opcode, &[0, id])
	}

	fn disconnect_cis(&mut self, handle: u16, ctx: &Ctx) -> Vec<AudioOut> {
		let Some((c, s)) = self.cis_at(handle) else { return Vec::new() };
		if !self.cigs[c].cises[s].established {
			return alloc::vec![status(0x0406, 0x02)];
		}
		let h = handle.to_le_bytes();
		let mut out = alloc::vec![status(0x0406, 0), event(0x05, &[0, h[0], h[1], 0x16])];
		let (cig_id, cis_id) = (self.cigs[c].id, self.cigs[c].cises[s].id);
		let acl = self.cigs[c].cises[s].acl;
		self.drop_cis(c, s);
		if let Some(ear) = acl.and_then(|acl| ctx.ear_of(acl))
			&& let Some(link) = ctx.links[ear]
		{
			for pdu in self.earbuds[ear].cis_down(cig_id, cis_id, &link, &mut self.log) {
				out.push(AudioOut::Att(ear, pdu));
			}
		}
		out
	}

	fn drop_cis(&mut self, c: usize, s: usize) {
		let cis = &mut self.cigs[c].cises[s];
		cis.established = false;
		cis.acl = None;
		cis.paths = [false; 2];
		cis.pace = Pace::default();
	}

	// AN EARBUD'S ACL DROPPED, by either side: its streams go first, each with its own Disconnection Complete carrying the
	// link's reason, and its endpoints go idle with the link.
	pub fn acl_lost(&mut self, ear: usize, acl: u16, reason: u8) -> Vec<AudioOut> {
		let mut out = Vec::new();
		for c in 0..self.cigs.len() {
			for s in 0..self.cigs[c].cises.len() {
				if self.cigs[c].cises[s].established && self.cigs[c].cises[s].acl == Some(acl) {
					let h = self.cigs[c].cises[s].handle.to_le_bytes();
					out.push(event(0x05, &[0, h[0], h[1], reason]));
					self.drop_cis(c, s);
					self.earbuds[ear].cis_lost(&mut self.log);
				}
			}
		}
		self.earbuds[ear].link_lost(&mut self.log);
		out
	}

	// THE CISes UP ON AN EARBUD'S LINK, by CIG and CIS id: what its endpoints are coupled to.
	fn up(&self, acl: u16) -> Vec<(u8, u8)> {
		self.cigs.iter().flat_map(|cig| cig.cises.iter().filter(|cis| cis.established && cis.acl == Some(acl)).map(move |cis| (cig.id, cis.id))).collect()
	}

	// ------------------------------------------------------------------ ISO data paths (Core 7.8.109, 7.8.110)

	// LE SETUP ISO DATA PATH over HCI, for an established CIS either way or a synchronized BIS toward the host. The codec
	// is the host's, so the coding format is transparent, as BAP's audio data path setup says.
	fn setup_path(&mut self, params: &[u8]) -> AudioOut {
		let opcode = 0x206e;
		let handle = u16_at(params, 0) & 0x0fff;
		let h = handle.to_le_bytes();
		let answer = |code: u8| complete(opcode, &[code, h[0], h[1]]);
		if params.len() < 13 || params.len() != 13 + usize::from(params[12]) || params[2] > 1 {
			return answer(0x12);
		}
		let (direction, path) = (usize::from(params[2]), params[3]);
		if path != 0x00 {
			return answer(0x12);
		}
		if params[4] != 0x03 {
			return answer(0x11);
		}
		if let Some((c, s)) = self.cis_at(handle) {
			let cis = &mut self.cigs[c].cises[s];
			if !cis.established || cis.paths[direction] {
				return answer(0x0c);
			}
			cis.paths[direction] = true;
			cis.pace = Pace { sequence: cis.pace.sequence, ..Pace::default() };
			return answer(0);
		}
		if let Some(bis) = self.receiver.bis_mut(handle) {
			if direction != 1 || bis.path {
				return answer(0x0c);
			}
			bis.path = true;
			return answer(0);
		}
		answer(0x02)
	}

	// LE REMOVE ISO DATA PATH: the directions in a bitmask, input bit 0 and output bit 1, each one set up.
	fn remove_path(&mut self, params: &[u8]) -> AudioOut {
		let opcode = 0x206f;
		let handle = u16_at(params, 0) & 0x0fff;
		let h = handle.to_le_bytes();
		let answer = |code: u8| complete(opcode, &[code, h[0], h[1]]);
		let Some(&directions) = params.get(2).filter(|_| params.len() == 3) else { return answer(0x12) };
		if directions == 0 || directions > 3 {
			return answer(0x12);
		}
		if let Some((c, s)) = self.cis_at(handle) {
			let cis = &mut self.cigs[c].cises[s];
			if (0..2).any(|at| directions & (1 << at) != 0 && !cis.paths[at]) {
				return answer(0x0c);
			}
			for at in 0..2 {
				if directions & (1 << at) != 0 {
					cis.paths[at] = false;
				}
			}
			return answer(0);
		}
		if let Some(bis) = self.receiver.bis_mut(handle) {
			if directions != 0b10 || !bis.path {
				return answer(0x0c);
			}
			bis.path = false;
			return answer(0);
		}
		answer(0x02)
	}

	// ------------------------------------------------------------------ ISO data both ways

	// ONE ISO DATA PACKET FROM THE HOST: its buffer comes straight back, and an SDU on an established CIS's input path
	// reaches the earbud's sink. A fragment is not what this controller takes: the earbud counts it refused.
	pub fn iso(&mut self, bytes: &[u8], ctx: &Ctx) -> Vec<AudioOut> {
		let Some(packet) = iso_in(bytes) else { return Vec::new() };
		if let Some((c, s)) = self.cis_at(packet.handle) {
			let (cig_id, cis) = (self.cigs[c].id, &self.cigs[c].cises[s]);
			let (cis_id, acl, open) = (cis.id, cis.acl, cis.established && cis.paths[0]);
			if open && let Some(ear) = acl.and_then(|acl| ctx.ear_of(acl)) {
				if packet.boundary == 0b10 {
					self.earbuds[ear].hear(cig_id, cis_id, packet.sdu, &mut self.log);
				} else {
					self.earbuds[ear].refuse(cig_id, cis_id);
				}
			}
			return alloc::vec![completed(packet.handle)];
		}
		if self.receiver.bis_mut(packet.handle).is_some() {
			return alloc::vec![completed(packet.handle)];
		}
		Vec::new()
	}

	// WHAT IS DUE NOW: a scan's end, each microphone's SDUs on its CIS's output path, the broadcast's SDUs on each synced
	// BIS's, and the periodic train's reports.
	pub fn tick(&mut self, now: u64, ctx: &Ctx) -> Vec<AudioOut> {
		self.now = now;
		let mut out = Vec::new();
		if self.scan.until.is_some_and(|until| until <= now) {
			self.scan = Scan::default();
			out.push(meta(0x11, &[]));
		}
		for c in 0..self.cigs.len() {
			let cig_id = self.cigs[c].id;
			for s in 0..self.cigs[c].cises.len() {
				let cis = &self.cigs[c].cises[s];
				if !cis.established || !cis.paths[1] {
					continue;
				}
				let (cis_id, handle) = (cis.id, cis.handle);
				let Some(ear) = cis.acl.and_then(|acl| ctx.ear_of(acl)) else { continue };
				let Some(sdu) = self.earbuds[ear].microphone(cig_id, cis_id, &mut self.log) else {
					self.cigs[c].cises[s].pace.stop();
					continue;
				};
				let pace = &mut self.cigs[c].cises[s].pace;
				for _ in 0..pace.due(now) {
					out.push(AudioOut::Iso(iso_packet(handle, pace.take(), &sdu)));
				}
			}
		}
		out.extend(self.receiver_tick(now));
		out
	}

	// WHETHER ANYTHING HERE RUNS ON THE CLOCK: a scan with an end, a stream toward the host, a periodic train followed.
	pub fn active(&self) -> bool {
		self.scan.until.is_some() || self.cigs.iter().any(|cig| cig.cises.iter().any(|cis| cis.established && cis.paths[1])) || self.receiver.active()
	}

	// ------------------------------------------------------------------ the earbuds, through the LE world

	pub fn connected(&mut self, ear: usize) {
		self.earbuds[ear].connected();
	}

	// ONE ATT PDU FROM THE HOST on an earbud's link.
	pub fn att(&mut self, ear: usize, pdu: &[u8], link: &EarLink) -> Vec<AudioOut> {
		let up = self.up(link.handle);
		self.earbuds[ear].att(pdu, link, &up, &mut self.log).into_iter().map(|pdu| AudioOut::Att(ear, pdu)).collect()
	}

	// AN EARBUD'S LINK IS SECURE - encrypted with its bond's key and the keys distributed: it looks for the host's call
	// bearer.
	pub fn secured(&mut self, ear: usize) -> Vec<AudioOut> {
		self.earbuds[ear].secured().into_iter().map(|pdu| AudioOut::Att(ear, pdu)).collect()
	}

	// THE EARBUD CHANGES ITS OWN VOLUME, and notifies a client that asked.
	pub fn own_volume(&mut self, ear: usize, volume: u8, link: Option<&EarLink>) -> Vec<AudioOut> {
		self.earbuds[ear].own_volume(volume, link, &mut self.log).into_iter().map(|pdu| AudioOut::Att(ear, pdu)).collect()
	}

	// THE EARBUD PRESSES A CALL BUTTON: a write of the host's Call Control Point; `None` where it found no call bearer.
	pub fn call(&mut self, ear: usize, opcode: u8) -> Option<Vec<AudioOut>> {
		Some(self.earbuds[ear].call(opcode, &mut self.log)?.into_iter().map(|pdu| AudioOut::Att(ear, pdu)).collect())
	}

	pub fn forget(&mut self, ear: usize) {
		self.earbuds[ear].forget();
	}

	// A PAIRING BEGINS ON AN EARBUD: a new bond, and none of the old one's client configuration.
	pub fn pairing(&mut self, ear: usize) {
		self.earbuds[ear].forget();
	}

	pub fn advertising_data(&self, ear: usize) -> Vec<u8> {
		self.earbuds[ear].advertising_data()
	}

	pub fn say(&mut self, line: String) {
		self.log.push(line);
	}
}

// WHAT AN EARBUD SAYS IT HEARD, the line a gate reads: the rate its endpoint was configured for, whether every frame of the
// stream so far was whole, and the most frequent loudest spectral line as a frequency - 50 Hz a line.
pub fn heard_line(letter: char, rate: u32, refused: u32, loudest: u16) -> String {
	let whole = if refused == 0 { String::from("every frame whole") } else { format!("{refused} frames refused") };
	format!("earbud {letter} heard 50 frames of LC3 at {rate} Hz, {whole}, the loudest line at {} Hz", u32::from(loudest) * 50)
}
