// THE EMULATED LE WORLD BEYOND THE MOUSE: the LE peripherals on the far side of the fixture's radio, each pairing in a
// model the mouse does not - and the LE Audio world beside them (`bt_le_audio`).
//
// A TEST FIXTURE, DEVELOPMENT-ONLY, AND ITS PROTOCOLS ARE ITS OWN: the Security Manager's responder half, the key
// distribution, the private address and the attribute server and client here use `bt_peer`'s own AES, CMAC and
// key functions - a separate implementation from the host stack's, held to the same published sample data - so a
// host and a fixture that agree are two implementations agreeing.
//
// THE DEVICES, numbered for the control endpoint after the BR/EDR ones:
//
//    8 tag       DisplayYesNo, Secure Connections       Numeric Comparison; advertises from a resolvable private
//                                                       address and gives its identity and its resolving key;
//                                                       a battery and a custom service on its attribute server
//    9 remote    KeyboardOnly, legacy only              legacy Passkey Entry, this host showing the digits; gives
//                                                       its long-term key with its EDIV and Rand
//   10 display   DisplayOnly, Secure Connections        Passkey Entry, this host typing what it shows
//   11 phone     DisplayYesNo, Secure Connections       THE PHONE'S LE HALF, a dual-mode device: heard from its public
//                                                       address only while it advertises (`le-advertise` on device 2);
//                                                       asks for the BR/EDR key to be derived from its LE pairing,
//                                                       and holds the LE key its BR/EDR half derived
//   12 earbud L    NoInputNoOutput, Secure Connections    Just Works with no prompt on either side; connectable extended
//   13 earbud R                                           advertising from its identity, which it gives with its resolving
//                                                       key; its LE Audio servers and its call bearer client are
//                                                       `bt_le_audio`'s, and so is the broadcast source, device 14
//
// THE KEY AGREEMENT IS THE FIXTURE'S BY CONSTRUCTION, as the mouse's is: each device has a fixed public key, and the
// emulated controller answers a DHKey request against it with that device's fixed shared secret. What the exchange
// then proves is everything above the key agreement.

use crate::bt_le_audio::{self, Advert, Audio, AudioOut, Ctx, EarLink};
use crate::bt_peer::{ah, c1, f4, f5, f6, fingerprint, g2, s1};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub struct LeSpec {
	pub name: &'static str,
	// The identity address, most significant first: static random, its top two bits set.
	pub identity: [u8; 6],
	pub capability: u8,
	pub secure_connections: bool,
	// Whether it advertises from a private address, and so gives its identity and resolving key.
	pub private: bool,
	// Whether it gives a legacy long-term key with its EDIV and Rand.
	pub legacy_key: bool,
	pub services: bool,
	// A DUAL-MODE DEVICE'S LE HALF: its BR/EDR half's number on the control endpoint. Its identity is that half's public
	// address, it advertises only when told to, and it derives keys across transports.
	pub dual: Option<u8>,
	// AN LE AUDIO EARBUD: which of the set's two it is. It gives its identity and resolving key though it advertises from
	// its identity, and its attribute server is `bt_le_audio`'s.
	pub earbud: Option<usize>,
}

pub const LE_DEVICES: [LeSpec; 6] = [
	LeSpec { name: "fixture tag", identity: [0xd0, 0x1b, 0xdc, 0x20, 0x00, 0x08], capability: 0x01, secure_connections: true, private: true, legacy_key: false, services: true, dual: None, earbud: None },
	LeSpec { name: "fixture remote", identity: [0xd0, 0x1b, 0xdc, 0x20, 0x00, 0x09], capability: 0x02, secure_connections: false, private: false, legacy_key: true, services: false, dual: None, earbud: None },
	LeSpec { name: "fixture display", identity: [0xd0, 0x1b, 0xdc, 0x20, 0x00, 0x0a], capability: 0x00, secure_connections: true, private: false, legacy_key: false, services: false, dual: None, earbud: None },
	LeSpec { name: "fixture phone", identity: [0x00, 0x1b, 0xdc, 0x20, 0x00, 0x02], capability: 0x01, secure_connections: true, private: false, legacy_key: false, services: false, dual: Some(2), earbud: None },
	LeSpec { name: "fixture earbud L", identity: [0xd0, 0x1b, 0xdc, 0x20, 0x00, 0x0c], capability: 0x03, secure_connections: true, private: false, legacy_key: false, services: false, dual: None, earbud: Some(0) },
	LeSpec { name: "fixture earbud R", identity: [0xd0, 0x1b, 0xdc, 0x20, 0x00, 0x0d], capability: 0x03, secure_connections: true, private: false, legacy_key: false, services: false, dual: None, earbud: Some(1) },
];

// The first LE world device's number on the control endpoint, and the first link handle.
pub const FIRST_LE_DEVICE: u8 = 8;
const FIRST_HANDLE: u16 = 0x0060;
const SMP_CID: u16 = 0x0006;
const ATT_CID: u16 = 0x0004;

pub enum LeOut {
	Event(Vec<u8>),
	Acl(Vec<u8>),
	// AN ISO DATA PACKET TO THE HOST - an earbud's microphone, a broadcast's stream.
	Iso(Vec<u8>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Smp {
	Idle,
	AwaitPublicKey,
	// Secure Connections: this side's confirm is sent and the host's nonce awaited (Just Works and Numeric), or a
	// passkey round's host confirm awaited.
	AwaitRandom,
	AwaitConfirm,
	AwaitCheck,
	// Legacy: the host's confirm, then its random.
	LegacyConfirm,
	LegacyRandom,
	// The key is agreed; the controller encrypts next, then the keys are distributed.
	Encrypting,
	Distributing,
	Bonded,
}

struct LeLink {
	handle: u16,
	host: [u8; 7],
	encrypted: bool,
	smp: Smp,
	request: [u8; 7],
	response: [u8; 7],
	pka: [u8; 64],
	na: [u8; 16],
	nb: [u8; 16],
	cai: [u8; 16],
	round: u8,
	mac_key: [u8; 16],
	pairing_key: Option<[u8; 16]>,
	tk: [u8; 16],
	mconfirm: Option<[u8; 16]>,
	typed: Option<u32>,
	// Passkey Entry runs: this device shows its digits and the host's person types them.
	passkey_model: bool,
	// A characteristic's notifications are on.
	notify: Vec<u16>,
}

struct LeDevice {
	spec: &'static LeSpec,
	irk: [u8; 16],
	rpa: [u8; 6],
	ltk: Option<[u8; 16]>,
	ediv: u16,
	rand: [u8; 8],
	passkey: u32,
	host_irk: Option<[u8; 16]>,
	reset: bool,
	link: Option<LeLink>,
	custom: Vec<u8>,
	// A dual-mode device's LE half advertises only while told to.
	advertising: bool,
}

pub struct LeWorld {
	devices: Vec<LeDevice>,
	random: u64,
	log: Vec<String>,
	// A BR/EDR LINK KEY a dual-mode device derived from its LE pairing, for its BR/EDR half: its address, the key as HCI
	// carries it, and whether it is authenticated.
	derived: Option<([u8; 6], [u8; 16], bool)>,
	// THE LE AUDIO HALF of the controller, the earbuds' servers and the broadcast source.
	pub audio: Audio,
}

fn reversed<const N: usize>(bytes: &[u8]) -> [u8; N] {
	let mut out = [0u8; N];
	out.copy_from_slice(&bytes[..N]);
	out.reverse();
	out
}

fn meta(subevent: u8, body: &[u8]) -> LeOut {
	let mut bytes = alloc::vec![0x3e, body.len() as u8 + 1, subevent];
	bytes.extend_from_slice(body);
	LeOut::Event(bytes)
}

fn event(code: u8, body: &[u8]) -> LeOut {
	let mut bytes = alloc::vec![code, body.len() as u8];
	bytes.extend_from_slice(body);
	LeOut::Event(bytes)
}

// A device's fixed public key and the secret the controller agrees with it.
pub fn public_key(device: usize) -> [u8; 64] {
	core::array::from_fn(|at| 0x60 ^ (at as u8).wrapping_mul(5) ^ device as u8)
}

pub fn dhkey(device: usize) -> [u8; 32] {
	[0x70 + device as u8; 32]
}

impl LeWorld {
	pub fn new(seed: u64) -> LeWorld {
		let mut world = LeWorld { devices: Vec::new(), random: seed | 1, log: Vec::new(), derived: None, audio: Audio::new(seed.rotate_left(29) ^ 0x6a09_e667_f3bc_c908) };
		for (at, spec) in LE_DEVICES.iter().enumerate() {
			let mut irk = [0u8; 16];
			for chunk in irk.chunks_mut(8) {
				chunk.copy_from_slice(&world.next().to_le_bytes());
			}
			// THE PRIVATE ADDRESS: twenty-two random bits with `01` on top, and their hash under the resolving key.
			let random = world.next();
			let prand = [0x40 | (random as u8 & 0x3f), (random >> 8) as u8, (random >> 16) as u8];
			let hash = ah(&irk, prand);
			let rpa = [prand[0], prand[1], prand[2], hash[0], hash[1], hash[2]];
			let passkey = (world.next() % 1_000_000) as u32;
			world.devices.push(LeDevice { spec, irk, rpa, ltk: None, ediv: 0, rand: [0; 8], passkey, host_irk: None, reset: false, link: None, custom: b"fixture".to_vec(), advertising: spec.dual.is_none() });
			let _ = at;
		}
		world
	}

	fn next(&mut self) -> u64 {
		self.random ^= self.random >> 12;
		self.random ^= self.random << 25;
		self.random ^= self.random >> 27;
		self.random.wrapping_mul(0x2545_F491_4F6C_DD1D)
	}

	fn say(&mut self, line: String) {
		if self.log.len() < 256 {
			self.log.push(line);
		}
	}

	pub fn take_log(&mut self) -> Vec<String> {
		core::mem::take(&mut self.log)
	}

	fn short(&self, at: usize) -> &'static str {
		self.devices[at].spec.name.trim_start_matches("fixture ")
	}

	// The address a device advertises from: its private address, or its identity.
	fn advertised(&self, at: usize) -> [u8; 6] {
		if self.devices[at].spec.private { self.devices[at].rpa } else { self.devices[at].spec.identity }
	}

	pub fn owns(&self, handle: u16) -> bool {
		self.devices.iter().any(|device| device.link.as_ref().is_some_and(|link| link.handle == handle))
	}

	fn by_handle(&self, handle: u16) -> Option<usize> {
		self.devices.iter().position(|device| device.link.as_ref().is_some_and(|link| link.handle == handle))
	}

	pub fn reset(&mut self) {
		for device in self.devices.iter_mut() {
			device.link = None;
		}
		self.audio.reset();
		for line in self.audio.take_log() {
			self.say(line);
		}
	}

	// The address type a device is heard and connected from: public for a dual-mode device's LE half, random otherwise.
	fn address_type(&self, at: usize) -> u8 {
		if self.devices[at].spec.dual.is_some() { 0x00 } else { 0x01 }
	}

	// Whether a device is there to be heard or connected to: not connected, and advertising.
	fn present(&self, at: usize) -> bool {
		self.devices[at].link.is_none() && self.devices[at].advertising
	}

	// WHAT ONE DEVICE ADVERTISES: its name in a connectable legacy PDU - or, for an earbud, connectable extended advertising
	// with its services, its announcement and its set identifier.
	fn advert(&self, at: usize) -> Advert {
		if let Some(ear) = self.devices[at].spec.earbud {
			return Advert { event_type: 0x0001, address_type: 0x01, address: self.devices[at].spec.identity, secondary_phy: 1, sid: 0, rssi: -58 - ear as i8, periodic_interval: 0, data: self.audio.advertising_data(ear) };
		}
		let name = self.devices[at].spec.name.as_bytes();
		let mut data = alloc::vec![name.len() as u8 + 1, 0x09];
		data.extend_from_slice(name);
		Advert::legacy(self.address_type(at), self.advertised(at), -64, &data)
	}

	// EVERY ADVERTISER THERE IS TO HEAR: each device not connected, and the broadcast source while it broadcasts.
	pub fn adverts(&self) -> Vec<Advert> {
		let mut out: Vec<Advert> = (0..self.devices.len()).filter(|&at| self.present(at)).map(|at| self.advert(at)).collect();
		out.extend(self.audio.broadcast_advert());
		out
	}

	// THE ADVERTISING REPORTS a legacy scan hears: the legacy advertisers only, each with its name.
	pub fn advertise(&self) -> Vec<LeOut> {
		self.adverts().iter().filter(|advert| advert.is_legacy()).map(|advert| meta(0x02, &advert.legacy_report())).collect()
	}

	// THE EXTENDED ADVERTISING REPORTS an extended scan hears: every advertiser.
	pub fn extended_reports(&self) -> Vec<LeOut> {
		self.adverts().iter().map(|advert| meta(0x0d, &advert.extended_report())).collect()
	}

	// Which device a host's create connection names, by the address it advertises.
	pub fn named(&self, address: &[u8; 6]) -> Option<usize> {
		(0..self.devices.len()).find(|&at| self.present(at) && self.advertised(at) == *address)
	}

	// THE DEVICES AN ACCEPT LIST HOLDS AND THAT ARE THERE TO CONNECT TO: by identity.
	pub fn listed(&self, accept: &[(u8, [u8; 6])]) -> Option<usize> {
		(0..self.devices.len()).find(|&at| self.present(at) && accept.iter().any(|(_, address)| *address == self.devices[at].spec.identity || *address == self.advertised(at)))
	}

	// A CONNECTION to device `at` from the host at `host`: the controller's completion, its peer named by identity when
	// the resolving list resolves its private address.
	pub fn connect(&mut self, at: usize, host: [u8; 7], resolved: bool) -> Vec<LeOut> {
		let handle = FIRST_HANDLE + at as u16;
		self.devices[at].link = Some(LeLink { handle, host, encrypted: false, smp: Smp::Idle, request: [0; 7], response: [0; 7], pka: [0; 64], na: [0; 16], nb: [0; 16], cai: [0; 16], round: 0, mac_key: [0; 16], pairing_key: None, tk: [0; 16], mconfirm: None, typed: None, passkey_model: false, notify: Vec::new() });
		let private = self.devices[at].spec.private;
		let (kind, address) = if private && !resolved {
			(0x01, self.devices[at].rpa)
		} else if private {
			(0x03, self.devices[at].spec.identity)
		} else {
			(self.address_type(at), self.devices[at].spec.identity)
		};
		let host_kind = if host[0] == 1 { "random" } else { "public" };
		self.say(format!("connected {} - the host from its {host_kind} address", self.short(at)));
		if let Some(ear) = self.devices[at].spec.earbud {
			self.audio.connected(ear);
		}
		// A HOST IT KNOWS BY ITS RESOLVING KEY is recognised behind its private address.
		if host[0] == 1
			&& let Some(irk) = self.devices[at].host_irk
		{
			let prand = [host[1], host[2], host[3]];
			if ah(&irk, prand) == [host[4], host[5], host[6]] {
				self.say(format!("{} resolved the host's private address with the identity it was given", self.short(at)));
			}
		}
		let h = handle.to_le_bytes();
		let mut body = alloc::vec![0, h[0], h[1], 0x00, kind];
		body.extend_from_slice(&reversed::<6>(&address));
		body.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0xa4, 0x01, 0x00]);
		alloc::vec![meta(0x01, &body)]
	}

	// A LINK DROPPED, by the host or by the device. An earbud's streams go with it, each with its own completion before
	// the link's, and it advertises again - heard at once by an extended scan that is on.
	pub fn disconnect(&mut self, handle: u16, by_host: bool) -> Option<Vec<LeOut>> {
		let at = self.by_handle(handle)?;
		let reason = if by_host { 0x16 } else { 0x13 };
		let mut out = Vec::new();
		if let Some(ear) = self.devices[at].spec.earbud {
			let lost = self.audio.acl_lost(ear, handle, reason);
			out.extend(self.from_audio(lost));
		}
		self.devices[at].link = None;
		let who = if by_host { "disconnected by the host" } else { "disconnected" };
		self.say(format!("{who} {}", self.short(at)));
		let h = handle.to_le_bytes();
		out.push(event(0x05, &[0, h[0], h[1], reason]));
		if self.devices[at].spec.earbud.is_some() && self.audio.scanning_extended() {
			out.push(meta(0x0d, &self.advert(at).extended_report()));
		}
		Some(out)
	}

	pub fn dhkey_for(&self, key: &[u8]) -> Option<[u8; 32]> {
		(0..self.devices.len()).find(|&at| public_key(at)[..] == *key).map(dhkey)
	}

	// THE HOST ENCRYPTS: with the key a pairing just agreed, or the one it kept - legacy's with the EDIV and Rand that
	// name it - and a device that holds none after a cold reboot takes the one presented and says so.
	pub fn encrypt(&mut self, handle: u16, rand: [u8; 8], ediv: u16, key: [u8; 16]) -> Option<Vec<LeOut>> {
		let at = self.by_handle(handle)?;
		let name = self.short(at);
		let fingerprint = fingerprint(&key);
		let device = &mut self.devices[at];
		let link = device.link.as_mut()?;
		let accepted = match (link.pairing_key, device.ltk) {
			(Some(agreed), _) if link.smp == Smp::Encrypting => agreed == key,
			(_, Some(held)) => held == key && (!device.spec.legacy_key || (device.ediv == ediv && device.rand == rand)),
			(_, None) if !device.reset => {
				device.ltk = Some(key);
				device.ediv = ediv;
				device.rand = rand;
				true
			}
			_ => false,
		};
		link.encrypted = accepted;
		let distributing = accepted && link.smp == Smp::Encrypting;
		if distributing {
			link.smp = Smp::Distributing;
		}
		// A LINK ENCRYPTED WITH THE KEY OF A BOND IT HOLDS, rather than one a pairing is distributing keys after.
		let bonded = accepted && matches!(link.smp, Smp::Idle | Smp::Bonded);
		let h = handle.to_le_bytes();
		let mut out = alloc::vec![event(0x08, &[if accepted { 0 } else { 0x06 }, h[0], h[1], u8::from(accepted)])];
		let line = if accepted { format!("encrypted {name} with key {:08x}", fingerprint) } else { format!("encryption REFUSED for {name}") };
		self.say(line);
		if distributing {
			out.extend(self.distribute(at));
			out.extend(self.bonded(at, false));
		} else if bonded && let Some(ear) = self.devices[at].spec.earbud {
			let secured = self.audio.secured(ear);
			out.extend(self.from_audio(secured));
		}
		Some(out)
	}

	fn send(&self, at: usize, cid: u16, payload: &[u8]) -> Option<LeOut> {
		let link = self.devices[at].link.as_ref()?;
		let mut pdu = Vec::with_capacity(4 + payload.len());
		pdu.extend_from_slice(&(payload.len() as u16).to_le_bytes());
		pdu.extend_from_slice(&cid.to_le_bytes());
		pdu.extend_from_slice(payload);
		let mut acl = Vec::with_capacity(4 + pdu.len());
		acl.extend_from_slice(&(link.handle | (0b10 << 12)).to_le_bytes());
		acl.extend_from_slice(&(pdu.len() as u16).to_le_bytes());
		acl.extend_from_slice(&pdu);
		Some(LeOut::Acl(acl))
	}

	pub fn acl(&mut self, handle: u16, data: &[u8]) -> Option<Vec<LeOut>> {
		let at = self.by_handle(handle)?;
		let h = handle.to_le_bytes();
		let mut out = alloc::vec![event(0x13, &[1, h[0], h[1], 1, 0])];
		if data.len() < 4 || data.len() != 4 + u16::from_le_bytes([data[0], data[1]]) as usize {
			return Some(out);
		}
		let cid = u16::from_le_bytes([data[2], data[3]]);
		let payload = &data[4..];
		// AN EARBUD'S ATTRIBUTE PROTOCOL is its LE Audio servers' and its call bearer client's.
		if cid == ATT_CID
			&& let Some(ear) = self.devices[at].spec.earbud
		{
			let link = self.ear_link(at)?;
			let answers = self.audio.att(ear, payload, &link);
			out.extend(self.from_audio(answers));
			return Some(out);
		}
		let was_bonded = self.devices[at].link.as_ref().is_some_and(|link| link.smp == Smp::Bonded);
		let answers = match cid {
			SMP_CID => self.smp(at, payload),
			ATT_CID => self.att(at, payload),
			_ => Vec::new(),
		};
		for answer in answers {
			out.extend(self.send(at, cid, &answer));
		}
		out.extend(self.bonded(at, was_bonded));
		Some(out)
	}

	// ------------------------------------------------------------------ the Security Manager's responder half

	fn own(&self, at: usize) -> [u8; 7] {
		let mut out = [0u8; 7];
		out[0] = self.address_type(at);
		out[1..].copy_from_slice(&if self.devices[at].spec.private { self.devices[at].rpa } else { self.devices[at].spec.identity });
		out
	}

	fn passkey_bit(link: &LeLink, passkey: u32) -> u8 {
		0x80 | ((passkey >> link.round) & 1) as u8
	}

	fn nonce(&mut self) -> [u8; 16] {
		let mut out = [0u8; 16];
		for chunk in out.chunks_mut(8) {
			chunk.copy_from_slice(&self.next().to_le_bytes());
		}
		out
	}

	fn smp(&mut self, at: usize, pdu: &[u8]) -> Vec<Vec<u8>> {
		let mut out = Vec::new();
		let Some(&code) = pdu.first() else { return out };
		let own = self.own(at);
		let spec = self.devices[at].spec;
		let name = self.short(at);
		let passkey = self.devices[at].passkey;
		let state = self.devices[at].link.as_ref().map(|link| link.smp);
		let Some(state) = state else { return out };
		match (state, code) {
			(_, 0x01) if pdu.len() == 7 => {
				let host_sc = pdu[3] & 0x08 != 0;
				// A DEVICE WITH NO INPUT AND NO OUTPUT asks for no protection it cannot give: Just Works, on both sides.
				let mitm = if spec.capability == 0x03 { 0 } else { 0x04 };
				let mut response = [0x02, spec.capability, 0x00, 0x01 | mitm, 16, 0, 0];
				if spec.secure_connections {
					response[3] |= 0x08;
				}
				// THE KEYS IT GIVES: its identity where it is private, its legacy key where it has one; it takes the host's
				// identity.
				response[5] = pdu[5] & 0x02;
				response[6] = pdu[6] & (if spec.private || spec.earbud.is_some() { 0x02 } else { 0 } | if spec.legacy_key { 0x01 } else { 0 });
				// A DUAL-MODE DEVICE asks for the BR/EDR key to be derived too, with CT2 where the host has it.
				if spec.dual.is_some() && spec.secure_connections {
					response[3] |= pdu[3] & 0x20;
					response[5] |= pdu[5] & 0x08;
					response[6] |= pdu[6] & 0x08;
				}
				if spec.secure_connections && !host_sc {
					out.push(alloc::vec![0x05, 0x03]);
					return out;
				}
				let link = self.devices[at].link.as_mut().expect("a link");
				link.request.copy_from_slice(pdu);
				link.response = response;
				link.round = 0;
				link.typed = None;
				link.mconfirm = None;
				link.pairing_key = None;
				self.say(format!("pairing {name} from the host's request"));
				if let Some(ear) = spec.earbud {
					self.audio.pairing(ear);
				}
				out.push(response.to_vec());
				let link = self.devices[at].link.as_mut().expect("a link");
				if spec.secure_connections {
					link.smp = Smp::AwaitPublicKey;
				} else {
					// LEGACY: a keyboard waits for its person's digits; with nobody to answer on the host, Just Works.
					link.smp = Smp::LegacyConfirm;
					link.tk = [0; 16];
				}
			}
			(Smp::AwaitPublicKey, 0x0c) if pdu.len() == 65 => {
				let nb = self.nonce();
				let link = self.devices[at].link.as_mut().expect("a link");
				link.pka.copy_from_slice(&pdu[1..]);
				link.nb = nb;
				let mut key = alloc::vec![0x0c];
				key.extend_from_slice(&public_key(at));
				out.push(key);
				let pkax: [u8; 32] = reversed(&link.pka[..32]);
				let pkbx: [u8; 32] = reversed(&public_key(at)[..32]);
				// PASSKEY ENTRY when protection is asked for and the host has a keyboard; Just Works otherwise.
				link.passkey_model = spec.capability == 0x00 && (link.request[3] | link.response[3]) & 0x04 != 0 && matches!(link.request[1], 0x02 | 0x04);
				if link.passkey_model {
					// DISPLAY ONLY: it shows its passkey, and the host's person types it; the host commits first.
					link.smp = Smp::AwaitConfirm;
					self.say(format!("passkey {name} {passkey:06}"));
				} else {
					let cb = f4(&pkbx, &pkax, &nb, 0);
					let mut confirm = alloc::vec![0x03];
					confirm.extend_from_slice(&reversed::<16>(&cb));
					out.push(confirm);
					link.smp = Smp::AwaitRandom;
				}
			}
			// A PASSKEY ROUND: the host's commitment, then this side's.
			(Smp::AwaitConfirm, 0x03) if pdu.len() == 17 => {
				let nbi = self.nonce();
				let link = self.devices[at].link.as_mut().expect("a link");
				link.cai = reversed(&pdu[1..]);
				link.nb = nbi;
				let pkax: [u8; 32] = reversed(&link.pka[..32]);
				let pkbx: [u8; 32] = reversed(&public_key(at)[..32]);
				let cbi = f4(&pkbx, &pkax, &nbi, Self::passkey_bit(link, passkey));
				let mut confirm = alloc::vec![0x03];
				confirm.extend_from_slice(&reversed::<16>(&cbi));
				out.push(confirm);
				link.smp = Smp::AwaitRandom;
			}
			(Smp::AwaitRandom, 0x04) if pdu.len() == 17 => {
				let link = self.devices[at].link.as_mut().expect("a link");
				let na: [u8; 16] = reversed(&pdu[1..]);
				let pkax: [u8; 32] = reversed(&link.pka[..32]);
				let pkbx: [u8; 32] = reversed(&public_key(at)[..32]);
				let passkey_entry = link.passkey_model;
				if passkey_entry && f4(&pkax, &pkbx, &na, Self::passkey_bit(link, passkey)) != link.cai {
					out.push(alloc::vec![0x05, 0x04]);
					link.smp = Smp::Idle;
					self.say(format!("{name} refused the host's commitment"));
					return out;
				}
				link.na = na;
				let mut random = alloc::vec![0x04];
				random.extend_from_slice(&reversed::<16>(&link.nb));
				out.push(random);
				if passkey_entry {
					link.round += 1;
					if link.round < 20 {
						link.smp = Smp::AwaitConfirm;
						return out;
					}
				} else if spec.capability == 0x01 {
					let value = g2(&pkax, &pkbx, &na, &link.nb) % 1_000_000;
					self.say(format!("compare {name} {value:06}"));
				}
				let link = self.devices[at].link.as_mut().expect("a link");
				let (mac_key, ltk) = f5(&dhkey(at), &link.na, &link.nb, &link.host, &own);
				link.mac_key = mac_key;
				link.pairing_key = Some(ltk);
				link.smp = Smp::AwaitCheck;
			}
			(Smp::AwaitCheck, 0x0d) if pdu.len() == 17 => {
				let link = self.devices[at].link.as_mut().expect("a link");
				let r = if link.passkey_model {
					let mut r = [0u8; 16];
					r[12..].copy_from_slice(&passkey.to_be_bytes());
					r
				} else {
					[0u8; 16]
				};
				let io_a = [link.request[3], link.request[2], link.request[1]];
				let expected = f6(&link.mac_key, &link.na, &link.nb, &r, &io_a, &link.host, &own);
				if reversed::<16>(&pdu[1..]) != expected {
					out.push(alloc::vec![0x05, 0x0b]);
					link.pairing_key = None;
					link.smp = Smp::Idle;
					self.say(format!("{name} refused the host's check value"));
					return out;
				}
				let io_b = [link.response[3], link.response[2], link.response[1]];
				let eb = f6(&link.mac_key, &link.nb, &link.na, &r, &io_b, &own, &link.host);
				let mut check = alloc::vec![0x0d];
				check.extend_from_slice(&reversed::<16>(&eb));
				out.push(check);
				link.smp = Smp::Encrypting;
			}
			(Smp::LegacyConfirm, 0x03) if pdu.len() == 17 => {
				let link = self.devices[at].link.as_mut().expect("a link");
				link.mconfirm = Some(reversed(&pdu[1..]));
				// A KEYBOARD PAIRED BY A HOST THAT ASKED FOR PROTECTION waits for its person to type the digits.
				let host_mitm = link.request[3] & 0x04 != 0 && link.request[1] != 0x03;
				if spec.capability == 0x02 && host_mitm && link.typed.is_none() {
					return out;
				}
				out.extend(self.legacy_confirm(at));
			}
			(Smp::LegacyRandom, 0x04) if pdu.len() == 17 => {
				let link = self.devices[at].link.as_mut().expect("a link");
				let mrand: [u8; 16] = reversed(&pdu[1..]);
				let (preq, pres) = (reversed::<7>(&link.request), reversed::<7>(&link.response));
				let ia: [u8; 6] = link.host[1..].try_into().expect("six bytes");
				let ra: [u8; 6] = own[1..].try_into().expect("six bytes");
				if Some(c1(&link.tk, &mrand, &preq, &pres, link.host[0], &ia, own[0], &ra)) != link.mconfirm {
					out.push(alloc::vec![0x05, 0x04]);
					link.smp = Smp::Idle;
					self.say(format!("{name} refused the host's legacy confirm"));
					return out;
				}
				let srand = link.nb;
				let mut random = alloc::vec![0x04];
				random.extend_from_slice(&reversed::<16>(&srand));
				out.push(random);
				link.pairing_key = Some(s1(&link.tk, &srand, &mrand));
				link.smp = Smp::Encrypting;
			}
			// THE HOST'S KEYS, after this side gave its own.
			(Smp::Distributing, 0x08) if pdu.len() == 17 => {
				self.devices[at].host_irk = Some(reversed(&pdu[1..]));
			}
			(Smp::Distributing, 0x09) if pdu.len() == 8 => {
				let address: [u8; 6] = reversed(&pdu[2..]);
				let kind = if pdu[1] == 0 { "public" } else { "random" };
				self.say(format!("{name} learned the host's identity, {kind} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", address[0], address[1], address[2], address[3], address[4], address[5]));
				if let Some(link) = self.devices[at].link.as_mut() {
					link.smp = Smp::Bonded;
				}
			}
			(_, 0x0e) => {}
			_ => out.push(alloc::vec![0x05, 0x08]),
		}
		out
	}

	fn legacy_confirm(&mut self, at: usize) -> Vec<Vec<u8>> {
		let own = self.own(at);
		let srand = self.nonce();
		let link = self.devices[at].link.as_mut().expect("a link");
		link.nb = srand;
		let (preq, pres) = (reversed::<7>(&link.request), reversed::<7>(&link.response));
		let ia: [u8; 6] = link.host[1..].try_into().expect("six bytes");
		let ra: [u8; 6] = own[1..].try_into().expect("six bytes");
		let sconfirm = c1(&link.tk, &srand, &preq, &pres, link.host[0], &ia, own[0], &ra);
		link.smp = Smp::LegacyRandom;
		let mut confirm = alloc::vec![0x03];
		confirm.extend_from_slice(&reversed::<16>(&sconfirm));
		alloc::vec![confirm]
	}

	// THE PERSON AT THE REMOTE TYPES THE DIGITS the host shows: its temporary key, and its confirm goes.
	pub fn type_passkey(&mut self, device: u8, passkey: u32) -> Option<Vec<LeOut>> {
		let at = usize::from(device.checked_sub(FIRST_LE_DEVICE)?);
		let name = self.short(at);
		let link = self.devices.get_mut(at)?.link.as_mut()?;
		link.typed = Some(passkey);
		let mut tk = [0u8; 16];
		tk[12..].copy_from_slice(&passkey.to_be_bytes());
		link.tk = tk;
		self.say(format!("typed the passkey on {name}"));
		let ready = self.devices[at].link.as_ref().is_some_and(|link| link.smp == Smp::LegacyConfirm && link.mconfirm.is_some());
		let answers = if ready { self.legacy_confirm(at) } else { Vec::new() };
		let mut out = Vec::new();
		for answer in answers {
			out.extend(self.send(at, SMP_CID, &answer));
		}
		Some(out)
	}

	// THE KEYS THIS DEVICE GIVES once encrypted: its legacy long-term key with its EDIV and Rand, and its identity
	// with its resolving key.
	fn distribute(&mut self, at: usize) -> Vec<LeOut> {
		let mut pdus = Vec::new();
		let name = self.short(at);
		let spec = self.devices[at].spec;
		let resp = self.devices[at].link.as_ref().map(|link| link.response[6]).unwrap_or(0);
		let secure = self.devices[at].link.as_ref().is_some_and(|link| link.response[3] & 0x08 != 0 && link.request[3] & 0x08 != 0);
		if secure {
			self.devices[at].ltk = self.devices[at].link.as_ref().and_then(|link| link.pairing_key);
			// THE BR/EDR KEY, derived where both asked for it in both fields - for the BR/EDR half to hold.
			let link_key = self.devices[at].link.as_ref().is_some_and(|link| link.request[5] & link.response[5] & link.request[6] & link.response[6] & 0x08 != 0);
			if link_key && let Some(ltk) = self.devices[at].ltk {
				let ct2 = self.devices[at].link.as_ref().is_some_and(|link| link.request[3] & link.response[3] & 0x20 != 0);
				let authenticated = self.devices[at].link.as_ref().is_some_and(|link| link.passkey_model || spec.capability == 0x01 && (link.request[3] | link.response[3]) & 0x04 != 0 && matches!(link.request[1], 0x01 | 0x04));
				let key = crate::bt_peer::link_key_from_ltk(&ltk, ct2);
				self.say(format!("{name} derived its BR/EDR key from the LE pairing, key {:08x}", fingerprint(&key)));
				self.derived = Some((spec.identity, key, authenticated));
			}
		}
		if !secure && resp & 0x01 != 0 && spec.legacy_key {
			let mut ltk = [0u8; 16];
			for chunk in ltk.chunks_mut(8) {
				chunk.copy_from_slice(&self.next().to_le_bytes());
			}
			let ediv = self.next() as u16;
			let rand = self.next().to_le_bytes();
			let device = &mut self.devices[at];
			device.ltk = Some(ltk);
			device.ediv = ediv;
			device.rand = rand;
			let mut info = alloc::vec![0x06];
			info.extend_from_slice(&reversed::<16>(&ltk));
			pdus.push(info);
			let mut master = alloc::vec![0x07];
			master.extend_from_slice(&ediv.to_le_bytes());
			master.extend_from_slice(&rand);
			pdus.push(master);
			self.say(format!("{name} gave its long-term key {:08x} with EDIV {ediv:#06x}", fingerprint(&ltk)));
		}
		if resp & 0x02 != 0 && (spec.private || spec.earbud.is_some()) {
			let irk = self.devices[at].irk;
			let mut info = alloc::vec![0x08];
			info.extend_from_slice(&reversed::<16>(&irk));
			pdus.push(info);
			let mut address = alloc::vec![0x09, 0x01];
			address.extend_from_slice(&reversed::<6>(&spec.identity));
			pdus.push(address);
			self.say(format!("{name} gave its identity and its resolving key"));
		}
		self.devices[at].reset = false;
		let host_gives = self.devices[at].link.as_ref().is_some_and(|link| link.request[5] & link.response[5] & 0x02 != 0);
		if !host_gives && let Some(link) = self.devices[at].link.as_mut() {
			link.smp = Smp::Bonded;
		}
		let mut out = Vec::new();
		for pdu in pdus {
			out.extend(self.send(at, SMP_CID, &pdu));
		}
		out
	}

	// ------------------------------------------------------------------ the attribute server and client

	// The tag's table: (handle, type, value, writable).
	fn table(&self, at: usize) -> Vec<(u16, u16, Vec<u8>, bool)> {
		let name = self.devices[at].spec.name.as_bytes().to_vec();
		let mut table = alloc::vec![(0x0001, 0x2800, 0x1800u16.to_le_bytes().to_vec(), false), (0x0002, 0x2803, alloc::vec![0x02, 0x03, 0x00, 0x00, 0x2a], false), (0x0003, 0x2a00, name, false)];
		if self.devices[at].spec.services {
			table.extend([
				(0x0004, 0x2800, 0x180fu16.to_le_bytes().to_vec(), false),
				(0x0005, 0x2803, alloc::vec![0x12, 0x06, 0x00, 0x19, 0x2a], false),
				(0x0006, 0x2a19, alloc::vec![87], false),
				(0x0007, 0x2902, alloc::vec![0, 0], true),
				(0x0008, 0x2800, 0xfff0u16.to_le_bytes().to_vec(), false),
				(0x0009, 0x2803, alloc::vec![0x1a, 0x0a, 0x00, 0xf1, 0xff], false),
				(0x000a, 0xfff1, self.devices[at].custom.clone(), true),
				(0x000b, 0x2902, alloc::vec![0, 0], true),
			]);
		}
		table
	}

	fn att(&mut self, at: usize, pdu: &[u8]) -> Vec<Vec<u8>> {
		let name = self.short(at);
		let table = self.table(at);
		let Some(&code) = pdu.first() else { return Vec::new() };
		let error = |request: u8, handle: u16, code: u8| {
			let h = handle.to_le_bytes();
			alloc::vec![alloc::vec![0x01, request, h[0], h[1], code]]
		};
		let u16_at = |at: usize| pdu.get(at..at + 2).map_or(0, |bytes| u16::from_le_bytes([bytes[0], bytes[1]]));
		match code {
			0x02 => alloc::vec![alloc::vec![0x03, 23, 0]],
			0x10 => {
				let (from, to, kind) = (u16_at(1), u16_at(3), u16_at(5));
				let services: Vec<&(u16, u16, Vec<u8>, bool)> = table.iter().filter(|entry| entry.1 == 0x2800 && entry.0 >= from && entry.0 <= to).collect();
				if kind != 0x2800 || services.is_empty() {
					return error(0x10, from, 0x0a);
				}
				let mut out = alloc::vec![0x11, 6];
				for service in services.iter().take(3) {
					let end = table.iter().filter(|entry| entry.1 == 0x2800 && entry.0 > service.0).map(|entry| entry.0 - 1).next().unwrap_or(table.last().map_or(service.0, |last| last.0));
					out.extend_from_slice(&service.0.to_le_bytes());
					out.extend_from_slice(&end.to_le_bytes());
					out.extend_from_slice(&service.2);
				}
				alloc::vec![out]
			}
			0x08 => {
				let (from, to, kind) = (u16_at(1), u16_at(3), u16_at(5));
				let found: Vec<&(u16, u16, Vec<u8>, bool)> = table.iter().filter(|entry| entry.1 == kind && entry.0 >= from && entry.0 <= to).collect();
				let Some(first) = found.first() else { return error(0x08, from, 0x0a) };
				let size = first.2.len() + 2;
				let mut out = alloc::vec![0x09, size as u8];
				for entry in found.iter().filter(|entry| entry.2.len() + 2 == size).take(21 / size) {
					out.extend_from_slice(&entry.0.to_le_bytes());
					out.extend_from_slice(&entry.2);
				}
				alloc::vec![out]
			}
			0x04 => {
				let (from, to) = (u16_at(1), u16_at(3));
				let mut out = alloc::vec![0x05, 1];
				for entry in table.iter().filter(|entry| entry.0 >= from && entry.0 <= to).take(5) {
					out.extend_from_slice(&entry.0.to_le_bytes());
					out.extend_from_slice(&entry.1.to_le_bytes());
				}
				if out.len() == 2 { error(0x04, from, 0x0a) } else { alloc::vec![out] }
			}
			0x0a => {
				let handle = u16_at(1);
				match table.iter().find(|entry| entry.0 == handle) {
					Some(entry) => {
						let mut out = alloc::vec![0x0b];
						out.extend_from_slice(&entry.2[..entry.2.len().min(22)]);
						alloc::vec![out]
					}
					None => error(0x0a, handle, 0x01),
				}
			}
			0x12 | 0x52 => {
				let handle = u16_at(1);
				let value = pdu.get(3..).unwrap_or(&[]).to_vec();
				let writable = table.iter().any(|entry| entry.0 == handle && entry.3);
				if !writable {
					return if code == 0x12 { error(0x12, handle, 0x03) } else { Vec::new() };
				}
				let encrypted = self.devices[at].link.as_ref().is_some_and(|link| link.encrypted);
				if !encrypted {
					return if code == 0x12 { error(0x12, handle, 0x0f) } else { Vec::new() };
				}
				let mut notifications = Vec::new();
				match handle {
					0x0007 | 0x000b => {
						let target = handle - 1;
						let on = value.first().is_some_and(|byte| byte & 1 != 0);
						if let Some(link) = self.devices[at].link.as_mut() {
							link.notify.retain(|held| *held != target);
							if on {
								link.notify.push(target);
							}
						}
						self.say(format!("{name} notifications on handle {target:#06x} {}", if on { "on" } else { "off" }));
					}
					0x000a => {
						self.devices[at].custom = value.clone();
						self.say(format!("{name} custom value written {:?}", String::from_utf8_lossy(&value)));
						// A WRITE IS ECHOED BACK as a notification where they are on, so a client sees both directions.
						if self.devices[at].link.as_ref().is_some_and(|link| link.notify.contains(&0x000a)) {
							let mut notification = alloc::vec![0x1b, 0x0a, 0x00];
							notification.extend_from_slice(&value);
							notifications.push(notification);
						}
					}
					_ => {}
				}
				let mut out = if code == 0x12 { alloc::vec![alloc::vec![0x13]] } else { Vec::new() };
				out.extend(notifications);
				out
			}
			// THE HOST'S ANSWER TO THIS DEVICE'S OWN READ of the host's name.
			0x09 => {
				if pdu.len() > 4 {
					let value = &pdu[4..];
					self.say(format!("{name} read the host's name {}", String::from_utf8_lossy(value)));
				}
				Vec::new()
			}
			0x01 => Vec::new(),
			_ => error(code, 0, 0x06),
		}
	}

	// THE DEVICE READS THE HOST'S NAME through the host's attribute server, as any LE client may.
	pub fn read_host_name(&mut self, device: u8) -> Option<Vec<LeOut>> {
		let at = usize::from(device.checked_sub(FIRST_LE_DEVICE)?);
		let request = [0x08, 0x01, 0x00, 0xff, 0xff, 0x00, 0x2a];
		Some(self.send(at, ATT_CID, &request).into_iter().collect())
	}

	// THE BR/EDR KEY a dual-mode device derived, for its BR/EDR half.
	pub fn take_derived(&mut self) -> Option<([u8; 6], [u8; 16], bool)> {
		self.derived.take()
	}

	// THE LE KEY a dual-mode device's BR/EDR half derived: its LE half holds it from now on.
	pub fn set_ltk(&mut self, identity: &[u8; 6], ltk: [u8; 16]) {
		if let Some(device) = self.devices.iter_mut().find(|device| device.spec.dual.is_some() && device.spec.identity == *identity) {
			device.ltk = Some(ltk);
			device.reset = false;
		}
	}

	// A DUAL-MODE DEVICE'S LE HALF, by its BR/EDR half's number.
	fn dual(&self, device: u8) -> Option<usize> {
		self.devices.iter().position(|held| held.spec.dual == Some(device))
	}

	// THE LE HALF ADVERTISES, or stops - and stopping drops its link, as a phone switching LE off does.
	pub fn set_advertising(&mut self, device: u8, on: bool) -> Option<Vec<LeOut>> {
		let at = self.dual(device)?;
		self.devices[at].advertising = on;
		let name = self.short(at);
		self.say(format!("{name} {} advertising on LE", if on { "began" } else { "stopped" }));
		if on {
			return Some(Vec::new());
		}
		let handle = self.devices[at].link.as_ref().map(|link| link.handle);
		Some(handle.and_then(|handle| self.disconnect(handle, false)).unwrap_or_default())
	}

	// A DUAL-MODE DEVICE RESET forgets its LE key too.
	pub fn forget_dual(&mut self, device: u8) {
		if let Some(at) = self.dual(device) {
			self.forget(FIRST_LE_DEVICE + at as u8);
		}
	}

	pub fn forget(&mut self, device: u8) -> bool {
		let Some(at) = device.checked_sub(FIRST_LE_DEVICE).map(usize::from).filter(|at| *at < self.devices.len()) else { return false };
		let name = self.short(at);
		let device = &mut self.devices[at];
		device.ltk = None;
		device.host_irk = None;
		device.reset = true;
		if let Some(ear) = device.spec.earbud {
			self.audio.forget(ear);
		}
		self.say(format!("{name} was reset and forgot its key"));
		true
	}

	pub fn act_disconnect(&mut self, device: u8) -> Option<Vec<LeOut>> {
		let at = usize::from(device.checked_sub(FIRST_LE_DEVICE)?);
		let handle = self.devices.get(at)?.link.as_ref()?.handle;
		self.disconnect(handle, false)
	}

	pub fn is_device(device: u8) -> bool {
		(FIRST_LE_DEVICE..FIRST_LE_DEVICE + LE_DEVICES.len() as u8).contains(&device)
	}

	// ------------------------------------------------------------------ the LE Audio world

	pub fn is_earbud(device: u8) -> bool {
		device == bt_le_audio::EARBUD_L || device == bt_le_audio::EARBUD_R
	}

	// An earbud's link as its servers see it: the handle, whether it is encrypted, the key of its bond.
	fn ear_link(&self, at: usize) -> Option<EarLink> {
		let device = &self.devices[at];
		let link = device.link.as_ref()?;
		Some(EarLink { handle: link.handle, encrypted: link.encrypted, ltk: device.ltk })
	}

	// WHAT THE LE AUDIO HALF IS TOLD of the links: each earbud's, and every LE handle there is.
	fn ctx(&self) -> Ctx {
		let mut ctx = Ctx::default();
		for (at, device) in self.devices.iter().enumerate() {
			if let Some(link) = device.link.as_ref() {
				ctx.acls.push(link.handle);
				if let Some(ear) = device.spec.earbud {
					ctx.links[ear] = self.ear_link(at);
				}
			}
		}
		ctx
	}

	// WHAT THE LE AUDIO HALF SENDS, as this world sends it - an earbud's ATT on its link - and what it said.
	fn from_audio(&mut self, outs: Vec<AudioOut>) -> Vec<LeOut> {
		let mut out = Vec::new();
		for item in outs {
			match item {
				AudioOut::Event(bytes) => out.push(LeOut::Event(bytes)),
				AudioOut::Iso(bytes) => out.push(LeOut::Iso(bytes)),
				AudioOut::Att(ear, pdu) => {
					if let Some(at) = self.devices.iter().position(|device| device.spec.earbud == Some(ear)) {
						out.extend(self.send(at, ATT_CID, &pdu));
					}
				}
			}
		}
		for line in self.audio.take_log() {
			self.say(line);
		}
		out
	}

	// AN EARBUD'S PAIRING HAS ENDED IN A BOND - its keys distributed both ways and kept: it says so, and looks for the
	// host's call bearer on the link the bond now secures.
	fn bonded(&mut self, at: usize, was_bonded: bool) -> Vec<LeOut> {
		let Some(ear) = self.devices[at].spec.earbud else { return Vec::new() };
		let now_bonded = self.devices[at].link.as_ref().is_some_and(|link| link.smp == Smp::Bonded && link.encrypted);
		if was_bonded || !now_bonded || self.devices[at].ltk.is_none() {
			return Vec::new();
		}
		self.say(format!("{} bonded", self.short(at)));
		let secured = self.audio.secured(ear);
		self.from_audio(secured)
	}

	// ONE HCI COMMAND for the LE Audio half of the controller; `None` for anything else.
	pub fn command(&mut self, opcode: u16, params: &[u8]) -> Option<Vec<LeOut>> {
		let ctx = self.ctx();
		let outs = self.audio.command(opcode, params, &ctx)?;
		Some(self.from_audio(outs))
	}

	pub fn extended_scan_parameters(&mut self, params: &[u8]) -> u8 {
		self.audio.extended_scan_parameters(params)
	}

	pub fn extended_scan_enable(&mut self, params: &[u8], now: u64) -> (u8, bool) {
		self.audio.extended_scan_enable(params, now)
	}

	pub fn scanning_extended(&self) -> bool {
		self.audio.scanning_extended()
	}

	// ONE ISO DATA PACKET FROM THE HOST.
	pub fn iso(&mut self, bytes: &[u8]) -> Vec<LeOut> {
		let ctx = self.ctx();
		let outs = self.audio.iso(bytes, &ctx);
		self.from_audio(outs)
	}

	// WHAT THE LE AUDIO HALF OWES AT `now`: streams toward the host, a periodic train's reports, a scan's end.
	pub fn tick(&mut self, now: u64) -> Vec<LeOut> {
		let ctx = self.ctx();
		let outs = self.audio.tick(now, &ctx);
		self.from_audio(outs)
	}

	pub fn audio_active(&self) -> bool {
		self.audio.active()
	}

	// AN EARBUD CHANGES ITS OWN VOLUME; `None` for a device that is not one.
	pub fn earbud_volume(&mut self, device: u8, volume: u8) -> Option<Vec<LeOut>> {
		let at = usize::from(device.checked_sub(FIRST_LE_DEVICE)?);
		let ear = self.devices.get(at)?.spec.earbud?;
		let link = self.ear_link(at);
		let outs = self.audio.own_volume(ear, volume, link.as_ref());
		Some(self.from_audio(outs))
	}

	// AN EARBUD WRITES THE HOST'S CALL CONTROL POINT: whether it could - it found the host's call bearer - and what it
	// sent; `None` for a device that is not an earbud.
	pub fn earbud_call(&mut self, device: u8, opcode: u8) -> Option<(bool, Vec<LeOut>)> {
		let at = usize::from(device.checked_sub(FIRST_LE_DEVICE)?);
		let ear = self.devices.get(at)?.spec.earbud?;
		let outs = self.audio.call(ear, opcode);
		let called = outs.is_some();
		Some((called, self.from_audio(outs.unwrap_or_default())))
	}

	// THE BROADCAST SOURCE starts, changes or stops; `None` for a mode it does not have.
	pub fn broadcast(&mut self, mode: u8) -> Option<Vec<LeOut>> {
		let outs = self.audio.broadcast(mode)?;
		Some(self.from_audio(outs))
	}
}
