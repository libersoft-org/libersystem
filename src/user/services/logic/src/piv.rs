//! THE PIV CARD COMMANDS THIS SYSTEM WILL SEND, AND NO OTHERS (NIST SP 800-73-4 Part 2).
//!
//! AN ALLOWLIST OF EXACT COMMANDS, NOT A FILTER OF BAD ONES. A client may send the canonical SELECT of
//! the PIV application and the canonical GET DATA for two public objects - the Discovery Object and the
//! PIV Authentication certificate - and every other byte string is refused before a reader sees it.
//! The commands that would touch a secret or change the card - VERIFY in any form, CHANGE REFERENCE
//! DATA, RESET RETRY COUNTER, PUT DATA, key generation and import, GENERAL AUTHENTICATE - are refused
//! by name, so the refusal says so; everything else is simply unsupported. Chaining, logical channels
//! other than the basic one, secure messaging and proprietary classes are all a class byte other than
//! 0x00, and refused as such.
//!
//! THE SERVICE BUILDS WHAT IT SENDS ON A CLIENT'S BEHALF: the secure PIN-verification template, whose
//! eight PIN bytes are placeholders the reader overwrites with digits it collected itself; the GENERAL
//! AUTHENTICATE for a P-256 signature over a 32-byte challenge; and the GET RESPONSE that continues a
//! long answer, bounded in count and total bytes so a client cannot use continuation to reach anything.

use alloc::vec::Vec;

/// The PIV application identifier, with its version, as SP 800-73-4 Part 1 fixes it.
pub const PIV_AID: [u8; 11] = [0xa0, 0x00, 0x00, 0x03, 0x08, 0x00, 0x00, 0x10, 0x00, 0x01, 0x00];

/// The canonical SELECT: CLA 00, INS A4, P1 04 (by name), P2 00, the full AID, Le 00.
pub const SELECT_PIV: [u8; 17] = [0x00, 0xa4, 0x04, 0x00, 0x0b, 0xa0, 0x00, 0x00, 0x03, 0x08, 0x00, 0x00, 0x10, 0x00, 0x01, 0x00, 0x00];
/// GET DATA for the Discovery Object (tag 7E).
pub const GET_DISCOVERY: [u8; 9] = [0x00, 0xcb, 0x3f, 0xff, 0x03, 0x5c, 0x01, 0x7e, 0x00];
/// GET DATA for the X.509 certificate for PIV Authentication (tag 5FC105).
pub const GET_AUTH_CERTIFICATE: [u8; 11] = [0x00, 0xcb, 0x3f, 0xff, 0x05, 0x5c, 0x03, 0x5f, 0xc1, 0x05, 0x00];

/// The PIV Card Application PIN's key reference.
pub const PIN_REFERENCE: u8 = 0x80;
/// The PIV Authentication key's reference, and the P-256 algorithm identifier.
pub const AUTHENTICATION_KEY: u8 = 0x9a;
pub const ALGORITHM_P256: u8 = 0x11;
/// The challenge a P-256 authentication signs: a SHA-256-sized digest.
pub const CHALLENGE_BYTES: usize = 32;

/// The longest short command APDU, and the longest short response with its status word.
pub const MAX_COMMAND: usize = 261;
pub const MAX_RESPONSE: usize = 258;
/// What a continued answer may add up to, and how many GET RESPONSEs it may take.
pub const MAX_CONTINUED: usize = 16 * 1024;
pub const MAX_CONTINUATIONS: usize = 64;

/// What the allowlist let through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Allowed {
	SelectPiv,
	GetDiscovery,
	GetAuthCertificate,
}

/// Why a command was refused before any reader saw it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
	/// Not a short APDU: its lengths do not add up.
	Malformed,
	/// A command that touches a secret or changes the card, named so the refusal says so.
	Forbidden,
	/// Anything else the allowlist does not name: other applications, other objects, other classes.
	Unsupported,
}

// The short-APDU cases (ISO/IEC 7816-3 12.1): header only; header and Le; header, Lc and data; and
// header, Lc, data and Le. Lc is never zero in a short APDU.
fn well_formed(apdu: &[u8]) -> bool {
	match apdu.len() {
		0..=3 => false,
		4 | 5 => true,
		len => {
			let lc = apdu[4] as usize;
			lc != 0 && (len == 5 + lc || len == 6 + lc)
		}
	}
}

/// Classify one command a client asked to send.
pub fn classify(apdu: &[u8]) -> Result<Allowed, Refused> {
	if apdu.len() > MAX_COMMAND || !well_formed(apdu) {
		return Err(Refused::Malformed);
	}
	// The instruction is judged first, so a forbidden command is refused as forbidden whatever class
	// byte it wears: a VERIFY behind a chaining bit is still a VERIFY.
	match apdu[1] {
		// VERIFY, CHANGE REFERENCE DATA, RESET RETRY COUNTER, PUT DATA, GENERATE ASYMMETRIC KEY PAIR,
		// GENERAL AUTHENTICATE (typed only), and the key-management instructions of the PIV
		// extensions: import and attestation.
		0x20 | 0x24 | 0x2c | 0xdb | 0x47 | 0x87 | 0xfe | 0xf9 | 0xfa => return Err(Refused::Forbidden),
		// GET RESPONSE is the service's to send; a client's would continue something it did not ask.
		0xc0 => return Err(Refused::Unsupported),
		_ => {}
	}
	if apdu == SELECT_PIV {
		return Ok(Allowed::SelectPiv);
	}
	if apdu == GET_DISCOVERY {
		return Ok(Allowed::GetDiscovery);
	}
	if apdu == GET_AUTH_CERTIFICATE {
		return Ok(Allowed::GetAuthCertificate);
	}
	Err(Refused::Unsupported)
}

/// The secure PIN-verification template: VERIFY for the PIV application PIN, with eight 0xFF bytes the
/// reader overwrites with the ASCII digits it collects and pads. No PIN is anywhere in it.
pub fn verify_template() -> [u8; 13] {
	[0x00, 0x20, 0x00, PIN_REFERENCE, 0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
}

/// The PIN block the template carries, and the digit range the PIV PIN allows.
pub const PIN_BLOCK: u8 = 8;
pub const PIN_MIN_DIGITS: u8 = 6;
pub const PIN_MAX_DIGITS: u8 = 8;

/// Whether a pinpad's advertised format can fill the template: ASCII digits, an eight-byte block, and a
/// digit range that covers six to eight.
pub fn pinpad_fits(secure_verify: bool, ascii: bool, max_block: u8, min_digits: u8, max_digits: u8) -> bool {
	secure_verify && ascii && max_block >= PIN_BLOCK && min_digits <= PIN_MIN_DIGITS && max_digits >= PIN_MAX_DIGITS
}

/// GENERAL AUTHENTICATE with the PIV Authentication key: a Dynamic Authentication Template asking for a
/// response (82, empty) to a challenge (81, 32 bytes), Le 00.
pub fn general_authenticate(challenge: &[u8; CHALLENGE_BYTES]) -> [u8; 44] {
	let mut apdu = [0u8; 44];
	apdu[..11].copy_from_slice(&[0x00, 0x87, ALGORITHM_P256, AUTHENTICATION_KEY, 0x26, 0x7c, 0x24, 0x82, 0x00, 0x81, 0x20]);
	apdu[11..43].copy_from_slice(challenge);
	apdu[43] = 0x00;
	apdu
}

/// GET RESPONSE for what SW2 of a 61xx said remains: 00 means 256.
pub fn get_response(remaining: u8) -> [u8; 5] {
	[0x00, 0xc0, 0x00, 0x00, remaining]
}

/// One BER-TLV length, and how many bytes it took. Short form, or long form with one or two bytes.
fn ber_length(bytes: &[u8]) -> Option<(usize, usize)> {
	match *bytes.first()? {
		short @ 0..=0x7f => Some((short as usize, 1)),
		0x81 => Some((*bytes.get(1)? as usize, 2)),
		0x82 => Some((((*bytes.get(1)? as usize) << 8) | *bytes.get(2)? as usize, 3)),
		_ => None,
	}
}

/// Why an authentication response was not a signature.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Malformed {
	Template,
	Signature,
}

/// The signature in a GENERAL AUTHENTICATE response: `7C L { 82 L signature }` and nothing else, with
/// the signature itself a DER ECDSA value - a SEQUENCE of two positive INTEGERs of at most 33 bytes.
pub fn authentication_signature(response: &[u8]) -> Result<Vec<u8>, Malformed> {
	let template = || -> Option<&[u8]> {
		if *response.first()? != 0x7c {
			return None;
		}
		let (outer, used) = ber_length(&response[1..])?;
		let body = response.get(1 + used..1 + used + outer)?;
		if 1 + used + outer != response.len() || *body.first()? != 0x82 {
			return None;
		}
		let (inner, used) = ber_length(&body[1..])?;
		let signature = body.get(1 + used..1 + used + inner)?;
		if 1 + used + inner != body.len() {
			return None;
		}
		Some(signature)
	};
	let signature = template().ok_or(Malformed::Template)?;
	if !der_ecdsa(signature) {
		return Err(Malformed::Signature);
	}
	Ok(signature.to_vec())
}

fn der_ecdsa(bytes: &[u8]) -> bool {
	let integer = |at: usize| -> Option<usize> {
		if *bytes.get(at)? != 0x02 {
			return None;
		}
		let len = *bytes.get(at + 1)? as usize;
		let value = bytes.get(at + 2..at + 2 + len)?;
		// Positive, minimally encoded, and no longer than a P-256 scalar with its sign byte.
		let minimal = len == 1 || !(value[0] == 0x00 && value[1] & 0x80 == 0);
		if len == 0 || len > 33 || value[0] & 0x80 != 0 || !minimal {
			return None;
		}
		Some(at + 2 + len)
	};
	let parsed = || -> Option<()> {
		if *bytes.first()? != 0x30 || *bytes.get(1)? as usize != bytes.len() - 2 {
			return None;
		}
		let after_r = integer(2)?;
		let after_s = integer(after_r)?;
		(after_s == bytes.len()).then_some(())
	};
	parsed().is_some()
}

/// What a secure verification's status word says, never retried.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PinStatus {
	Verified,
	Incorrect {
		retries: u8,
	},
	Blocked,
	/// The pinpad's own timeout (SW 6400).
	TimedOut,
	/// Cancelled on the pinpad (SW 6401).
	Cancelled,
	/// Anything else: the card or the reader failed.
	Error,
}

pub fn pin_status(sw1: u8, sw2: u8) -> PinStatus {
	match (sw1, sw2) {
		(0x90, 0x00) => PinStatus::Verified,
		(0x63, retries) if retries & 0xf0 == 0xc0 => PinStatus::Incorrect { retries: retries & 0x0f },
		(0x69, 0x83) => PinStatus::Blocked,
		(0x64, 0x00) => PinStatus::TimedOut,
		(0x64, 0x01) => PinStatus::Cancelled,
		_ => PinStatus::Error,
	}
}

/// The status word ending a response: the last two bytes, and the data before them.
pub fn split_status(response: &[u8]) -> Option<(&[u8], u8, u8)> {
	let (data, sw) = response.split_at(response.len().checked_sub(2)?);
	Some((data, sw[0], sw[1]))
}

/// An answer continued with GET RESPONSE, bounded: at most `MAX_CONTINUATIONS` pieces and
/// `MAX_CONTINUED` bytes in all.
#[derive(Default)]
pub struct Continued {
	data: Vec<u8>,
	pieces: usize,
}

/// What to do with one more piece of an answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Next {
	/// Send this GET RESPONSE and feed its answer here.
	More([u8; 5]),
	/// The answer is complete, with this final status word.
	Complete(u8, u8),
	/// Past the bounds, or not a response at all.
	Refused,
}

impl Continued {
	pub fn new() -> Self {
		Self::default()
	}

	/// One response, status word included.
	pub fn feed(&mut self, response: &[u8]) -> Next {
		let Some((data, sw1, sw2)) = split_status(response) else { return Next::Refused };
		self.pieces += 1;
		if self.pieces > MAX_CONTINUATIONS || self.data.len() + data.len() > MAX_CONTINUED {
			return Next::Refused;
		}
		self.data.extend_from_slice(data);
		if sw1 == 0x61 {
			return Next::More(get_response(sw2));
		}
		Next::Complete(sw1, sw2)
	}

	pub fn data(&self) -> &[u8] {
		&self.data
	}

	pub fn into_data(self) -> Vec<u8> {
		self.data
	}
}

#[cfg(test)]
mod tests;
