// THE EMULATED PEER: an LE boot mouse, as the in-guest Bluetooth fixture plays it.
//
// A TEST FIXTURE, DEVELOPMENT-ONLY, AND ITS CRYPTOGRAPHY IS ITS OWN. The host stack's derivations live
// in `service_logic::smp`, held to the Core specification's sample data. This module does NOT reuse
// them: a peer computing its half of a pairing with the host's own functions would agree with a host
// whose functions were wrong, which is exactly the kind of agreement a fixture exists to refuse. So
// the AES, the CMAC and the three SMP functions here are a separate implementation, held to the SAME
// published vectors by this module's own tests - two implementations written apart, each checked
// against the document, and made to agree with each other in the guest.
//
// WHAT IT PLAYS. The responder side of a Just Works LE Secure Connections pairing; a GATT server
// with the HOGP boot mouse table; and boot mouse reports on a script, once the host has put it in
// boot mode and turned notifications on over an encrypted link.

// ------------------------------------------------------------------ AES-128, forward only

const SBOX: [u8; 256] = {
	// Generated from the field's multiplicative inverse and the affine map, rather than typed in -
	// which is the other way to get the S-box right, and the one that does not share a transcription
	// with the host's table.
	let mut sbox = [0u8; 256];
	let mut p: u8 = 1;
	let mut q: u8 = 1;
	loop {
		// Multiply p by 3.
		p = p ^ (p << 1) ^ if p & 0x80 != 0 { 0x1b } else { 0 };
		// Divide q by 3.
		q ^= q << 1;
		q ^= q << 2;
		q ^= q << 4;
		if q & 0x80 != 0 {
			q ^= 0x09;
		}
		let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
		sbox[p as usize] = x ^ 0x63;
		if p == 1 {
			break;
		}
	}
	sbox[0] = 0x63;
	sbox
};

fn times2(v: u8) -> u8 {
	(v << 1) ^ if v & 0x80 != 0 { 0x1b } else { 0 }
}

// Encrypt one block under a 128-bit key.
pub fn aes128(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
	// The key schedule as forty-four words.
	let mut words = [[0u8; 4]; 44];
	for (i, word) in words.iter_mut().enumerate().take(4) {
		word.copy_from_slice(&key[i * 4..i * 4 + 4]);
	}
	let mut rcon: u8 = 1;
	for i in 4..44 {
		let mut t = words[i - 1];
		if i % 4 == 0 {
			t = [SBOX[t[1] as usize] ^ rcon, SBOX[t[2] as usize], SBOX[t[3] as usize], SBOX[t[0] as usize]];
			rcon = times2(rcon);
		}
		for b in 0..4 {
			words[i][b] = words[i - 4][b] ^ t[b];
		}
	}
	let mut s = *block;
	let add = |s: &mut [u8; 16], round: usize| {
		for c in 0..4 {
			for r in 0..4 {
				s[c * 4 + r] ^= words[round * 4 + c][r];
			}
		}
	};
	add(&mut s, 0);
	for round in 1..=10 {
		// SubBytes and ShiftRows together: row r of column c comes from column c + r.
		let old = s;
		for c in 0..4 {
			for r in 0..4 {
				s[c * 4 + r] = SBOX[old[((c + r) % 4) * 4 + r] as usize];
			}
		}
		if round != 10 {
			for c in 0..4 {
				let a = [s[c * 4], s[c * 4 + 1], s[c * 4 + 2], s[c * 4 + 3]];
				for r in 0..4 {
					s[c * 4 + r] = times2(a[r]) ^ (times2(a[(r + 1) % 4]) ^ a[(r + 1) % 4]) ^ a[(r + 2) % 4] ^ a[(r + 3) % 4];
				}
			}
		}
		add(&mut s, round);
	}
	s
}

// ------------------------------------------------------------------ AES-CMAC

fn double(block: &[u8; 16]) -> [u8; 16] {
	let mut out = [0u8; 16];
	let mut carry = 0u8;
	for i in (0..16).rev() {
		out[i] = (block[i] << 1) | carry;
		carry = block[i] >> 7;
	}
	if carry != 0 {
		out[15] ^= 0x87;
	}
	out
}

pub fn cmac(key: &[u8; 16], message: &[u8]) -> [u8; 16] {
	let k1 = double(&aes128(key, &[0u8; 16]));
	let k2 = double(&k1);
	let n = if message.is_empty() { 1 } else { message.len().div_ceil(16) };
	let complete = !message.is_empty() && message.len() % 16 == 0;
	let mut x = [0u8; 16];
	for i in 0..n {
		let mut block = [0u8; 16];
		let chunk = &message[(i * 16).min(message.len())..((i + 1) * 16).min(message.len())];
		block[..chunk.len()].copy_from_slice(chunk);
		if i == n - 1 {
			if complete {
				for j in 0..16 {
					block[j] ^= k1[j];
				}
			} else {
				block[chunk.len()] ^= 0x80;
				for j in 0..16 {
					block[j] ^= k2[j];
				}
			}
		}
		for j in 0..16 {
			x[j] ^= block[j];
		}
		x = aes128(key, &x);
	}
	x
}

// ------------------------------------------------------------------ the SMP functions, most significant first

pub fn f4(u: &[u8; 32], v: &[u8; 32], x: &[u8; 16], z: u8) -> [u8; 16] {
	let mut m = [0u8; 65];
	m[..32].copy_from_slice(u);
	m[32..64].copy_from_slice(v);
	m[64] = z;
	cmac(x, &m)
}

// The MacKey and the LTK, in that order.
pub fn f5(w: &[u8; 32], n1: &[u8; 16], n2: &[u8; 16], a1: &[u8; 7], a2: &[u8; 7]) -> ([u8; 16], [u8; 16]) {
	const SALT: [u8; 16] = [0x6c, 0x88, 0x83, 0x91, 0xaa, 0xf5, 0xa5, 0x38, 0x60, 0x37, 0x0b, 0xdb, 0x5a, 0x60, 0x83, 0xbe];
	let t = cmac(&SALT, w);
	let mut m = [0u8; 53];
	m[1..5].copy_from_slice(b"btle");
	m[5..21].copy_from_slice(n1);
	m[21..37].copy_from_slice(n2);
	m[37..44].copy_from_slice(a1);
	m[44..51].copy_from_slice(a2);
	m[51] = 0x01;
	m[52] = 0x00;
	let mac_key = cmac(&t, &m);
	m[0] = 1;
	(mac_key, cmac(&t, &m))
}

pub fn f6(w: &[u8; 16], n1: &[u8; 16], n2: &[u8; 16], r: &[u8; 16], io_cap: &[u8; 3], a1: &[u8; 7], a2: &[u8; 7]) -> [u8; 16] {
	let mut m = [0u8; 65];
	m[..16].copy_from_slice(n1);
	m[16..32].copy_from_slice(n2);
	m[32..48].copy_from_slice(r);
	m[48..51].copy_from_slice(io_cap);
	m[51..58].copy_from_slice(a1);
	m[58..65].copy_from_slice(a2);
	cmac(w, &m)
}

// A KEY'S FINGERPRINT, which is what the fixture prints instead of the key.
//
// The gate proves that the key the host presented after a cold reboot is the key the first boot's
// pairing produced, and it needs to compare the two without either one appearing in a log. Four bytes
// of a CMAC under the key itself identify it without revealing it.
pub fn fingerprint(key: &[u8; 16]) -> u32 {
	let tag = cmac(key, b"liber bt fixture fingerprint");
	u32::from_be_bytes([tag[0], tag[1], tag[2], tag[3]])
}

fn reversed<const N: usize>(bytes: &[u8]) -> [u8; N] {
	let mut out = [0u8; N];
	out.copy_from_slice(&bytes[..N]);
	out.reverse();
	out
}

// ------------------------------------------------------------------ the responder

pub const SMP_CID: u16 = 0x0006;
pub const ATT_CID: u16 = 0x0004;

// The fixture's own fixed identity and keys. A real mouse generates its key pair; an emulated one
// has no reason to, and a fixed one keeps every run's transcript the same shape.
pub const PEER_ADDRESS: [u8; 6] = [0xc0, 0xff, 0xee, 0x00, 0x00, 0x01];
// Random static: the top two bits of the most significant byte are set.
pub const PEER_KIND: u8 = 1;
const NB: [u8; 16] = [0x5a; 16];
// The shared secret the emulated controller answers every DHKey request with. Both halves of the
// key agreement are this fixture's to emulate, so they agree by construction; what the exchange
// then proves is everything ABOVE the key agreement.
pub const DHKEY: [u8; 32] = [0x3c; 32];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Smp {
	Idle,
	AwaitPublicKey,
	AwaitRandom,
	AwaitCheck,
	Bonded,
}

pub struct Peer {
	smp: Smp,
	// The host's address, most significant first with its type byte, as the derivations take it.
	host: [u8; 7],
	pka: [u8; 64],
	na: [u8; 16],
	request: [u8; 7],
	mac_key: [u8; 16],
	// The key this peer holds for the host: from a pairing in this process's life, or `None`.
	pub ltk: Option<[u8; 16]>,
	// How many pairing requests this peer has seen, which is how a gate tells a reconnect from a
	// pairing without trusting the host's own account of which it did.
	pub pairings: u32,
	// GATT state.
	pub boot_mode: bool,
	pub notify: bool,
}

// The peer's public key, as the wire carries it. Any 64 bytes the emulated controller will agree on.
pub fn peer_public_key() -> [u8; 64] {
	let mut key = [0u8; 64];
	for (i, byte) in key.iter_mut().enumerate() {
		*byte = 0x40 ^ i as u8;
	}
	key
}

impl Peer {
	pub const fn new() -> Peer {
		Peer { smp: Smp::Idle, host: [0; 7], pka: [0; 64], na: [0; 16], request: [0; 7], mac_key: [0; 16], ltk: None, pairings: 0, boot_mode: false, notify: false }
	}

	fn own(&self) -> [u8; 7] {
		let mut out = [0u8; 7];
		out[0] = PEER_KIND;
		out[1..].copy_from_slice(&PEER_ADDRESS);
		out
	}

	// A new link from this host. `host` is its type byte and address, most significant first.
	pub fn connected(&mut self, host: [u8; 7]) {
		self.host = host;
		self.smp = Smp::Idle;
		self.boot_mode = false;
		self.notify = false;
	}

	// One SMP PDU from the host, answered with the PDUs to send back.
	pub fn smp(&mut self, pdu: &[u8]) -> alloc::vec::Vec<alloc::vec::Vec<u8>> {
		let mut out = alloc::vec::Vec::new();
		match (self.smp, pdu.first().copied()) {
			(_, Some(0x01)) if pdu.len() == 7 => {
				self.pairings += 1;
				self.request.copy_from_slice(pdu);
				// Secure Connections required on this side too: a request without it is refused.
				if pdu[3] & 0x08 == 0 {
					out.push(alloc::vec![0x05, 0x03]);
					self.smp = Smp::Idle;
					return out;
				}
				out.push(alloc::vec![0x02, 0x03, 0x00, 0x09, 16, 0x00, 0x00]);
				self.smp = Smp::AwaitPublicKey;
			}
			(Smp::AwaitPublicKey, Some(0x0c)) if pdu.len() == 65 => {
				self.pka.copy_from_slice(&pdu[1..]);
				let mut key = alloc::vec![0x0c];
				key.extend_from_slice(&peer_public_key());
				out.push(key);
				let pkax: [u8; 32] = reversed(&self.pka[..32]);
				let pkbx: [u8; 32] = reversed(&peer_public_key()[..32]);
				let cb = f4(&pkbx, &pkax, &NB, 0);
				let mut confirm = alloc::vec![0x03];
				confirm.extend_from_slice(&reversed::<16>(&cb));
				out.push(confirm);
				self.smp = Smp::AwaitRandom;
			}
			(Smp::AwaitRandom, Some(0x04)) if pdu.len() == 17 => {
				self.na = reversed(&pdu[1..]);
				let mut random = alloc::vec![0x04];
				random.extend_from_slice(&reversed::<16>(&NB));
				out.push(random);
				let (mac_key, ltk) = f5(&DHKEY, &self.na, &NB, &self.host, &self.own());
				self.mac_key = mac_key;
				self.ltk = Some(ltk);
				self.smp = Smp::AwaitCheck;
			}
			(Smp::AwaitCheck, Some(0x0d)) if pdu.len() == 17 => {
				let io_a = [self.request[3], self.request[2], self.request[1]];
				let expected = f6(&self.mac_key, &self.na, &NB, &[0u8; 16], &io_a, &self.host, &self.own());
				if reversed::<16>(&pdu[1..]) != expected {
					out.push(alloc::vec![0x05, 0x0b]);
					self.ltk = None;
					self.smp = Smp::Idle;
					return out;
				}
				let io_b = [0x09, 0x00, 0x03];
				let eb = f6(&self.mac_key, &NB, &self.na, &[0u8; 16], &io_b, &self.own(), &self.host);
				let mut check = alloc::vec![0x0d];
				check.extend_from_slice(&reversed::<16>(&eb));
				out.push(check);
				self.smp = Smp::Bonded;
			}
			// Anything else is an exchange this peer is not in, answered the way a device answers it.
			_ => out.push(alloc::vec![0x05, 0x08]),
		}
		out
	}
}

impl Default for Peer {
	fn default() -> Self {
		Self::new()
	}
}

// ------------------------------------------------------------------ the GATT server

// The HOGP boot mouse table: GAP, then HID with protocol mode, the boot report and its configuration
// descriptor, then HID information. (handle, attribute type, declaration value handle, characteristic uuid)
pub const PROTOCOL_MODE_VALUE: u16 = 0x0008;
pub const REPORT_VALUE: u16 = 0x000a;
pub const REPORT_CONFIGURATION: u16 = 0x000b;

const SERVICES: [(u16, u16, u16); 2] = [(0x0001, 0x0005, 0x1800), (0x0006, 0x000e, 0x1812)];
const DECLARATIONS: [(u16, u16, u16); 4] = [(0x0002, 0x0003, 0x2a00), (0x0007, 0x0008, 0x2a4e), (0x0009, 0x000a, 0x2a33), (0x000c, 0x000d, 0x2a4a)];
const ATTRIBUTES: [(u16, u16); 12] = [
	(0x0001, 0x2800),
	(0x0002, 0x2803),
	(0x0003, 0x2a00),
	(0x0006, 0x2800),
	(0x0007, 0x2803),
	(0x0008, 0x2a4e),
	(0x0009, 0x2803),
	(0x000a, 0x2a33),
	(0x000b, 0x2902),
	(0x000c, 0x2803),
	(0x000d, 0x2a4a),
	(0x000e, 0x2a4b),
];

fn not_found(request: u8, handle: u16) -> alloc::vec::Vec<u8> {
	let h = handle.to_le_bytes();
	alloc::vec![0x01, request, h[0], h[1], 0x0a]
}

impl Peer {
	// One ATT request, answered. `encrypted` is whether the link is: the report's configuration may
	// only be written on an encrypted one, which is HOGP's own rule and the one that makes a gate's
	// "input arrived" mean "input arrived over an encrypted link".
	pub fn att(&mut self, pdu: &[u8], encrypted: bool) -> Option<alloc::vec::Vec<u8>> {
		const ROOM: usize = 21;
		let range = || (u16::from_le_bytes([pdu[1], pdu[2]]), u16::from_le_bytes([pdu[3], pdu[4]]));
		match pdu.first().copied()? {
			0x10 if pdu.len() == 7 => {
				let (from, to) = range();
				let mut out = alloc::vec![0x11, 6];
				for &(start, end, uuid) in SERVICES.iter().filter(|s| s.0 >= from && s.0 <= to).take(ROOM / 6) {
					out.extend_from_slice(&start.to_le_bytes());
					out.extend_from_slice(&end.to_le_bytes());
					out.extend_from_slice(&uuid.to_le_bytes());
				}
				Some(if out.len() == 2 { not_found(0x10, from) } else { out })
			}
			0x08 if pdu.len() == 7 => {
				let (from, to) = range();
				let mut out = alloc::vec![0x09, 7];
				for &(handle, value, uuid) in DECLARATIONS.iter().filter(|d| d.0 >= from && d.0 <= to).take(ROOM / 7) {
					out.extend_from_slice(&handle.to_le_bytes());
					out.push(0x1a);
					out.extend_from_slice(&value.to_le_bytes());
					out.extend_from_slice(&uuid.to_le_bytes());
				}
				Some(if out.len() == 2 { not_found(0x08, from) } else { out })
			}
			0x04 if pdu.len() == 5 => {
				let (from, to) = range();
				let mut out = alloc::vec![0x05, 1];
				for &(handle, kind) in ATTRIBUTES.iter().filter(|a| a.0 >= from && a.0 <= to).take(ROOM / 4) {
					out.extend_from_slice(&handle.to_le_bytes());
					out.extend_from_slice(&kind.to_le_bytes());
				}
				Some(if out.len() == 2 { not_found(0x04, from) } else { out })
			}
			0x52 if pdu.len() == 4 && u16::from_le_bytes([pdu[1], pdu[2]]) == PROTOCOL_MODE_VALUE => {
				self.boot_mode = pdu[3] == 0;
				None
			}
			0x12 if pdu.len() == 5 && u16::from_le_bytes([pdu[1], pdu[2]]) == REPORT_CONFIGURATION => {
				if !encrypted {
					// Insufficient encryption.
					return Some(alloc::vec![0x01, 0x12, pdu[1], pdu[2], 0x0f]);
				}
				self.notify = pdu[3] & 0x01 != 0;
				Some(alloc::vec![0x13])
			}
			other => Some(alloc::vec![0x01, other, 0, 0, 0x06]),
		}
	}

	// The next scripted report as a notification, once the host has asked for them.
	pub fn report(&self, step: u32) -> Option<alloc::vec::Vec<u8>> {
		if !self.boot_mode || !self.notify {
			return None;
		}
		let (buttons, dx, dy): (u8, i8, i8) = script(step);
		let handle = REPORT_VALUE.to_le_bytes();
		Some(alloc::vec![0x1b, handle[0], handle[1], buttons, dx as u8, dy as u8])
	}
}

// THE MOVEMENT THE GATE LOOKS FOR: right and down, a press of the left button, a release, then left
// and up - so the cursor ends where it began and a consumer sees both directions and both edges of a
// button. One step per tick.
//
// EIGHT FULL-SCALE STEPS A LEG, because a consumer reads a CELL. The pointer a client receives is a
// position on an eighty-by-fifty grid over a range of 32767, so one column is about four hundred
// units and a boot report carries at most 127 a step: a script of small moves would move the pointer
// and move no cell at all, and a gate reading cells would see nothing happen.
pub const SCRIPT_LEN: u32 = 34;

fn script(step: u32) -> (u8, i8, i8) {
	match step % SCRIPT_LEN {
		0..=7 => (0, 127, 0),
		8..=15 => (0, 0, 127),
		16 => (1, 0, 0),
		17 => (0, 0, 0),
		18..=25 => (0, -127, 0),
		_ => (0, 0, -127),
	}
}

#[cfg(test)]
mod tests;
