//! WHAT A PRINTER SAYS ABOUT ITSELF, taken for no more than it says: its IEEE 1284 device ID, and the port
//! status byte of the USB Printer Class (1.1, section 4.2.2).
//!
//! THE DEVICE ID IS LENGTH-CHECKED BEFORE IT IS READ. Its first two bytes are its whole length, big-endian and
//! including themselves; a length that disagrees with what arrived, or one past 4096 bytes, is malformed and
//! nothing of it is believed. The body is `KEY:value;` pairs. The languages are the comma-separated tokens of
//! `CMD` or `COMMAND SET` - exact, case-insensitive `POSTSCRIPT`, `POSTSCRIPT2` or `POSTSCRIPT3` say
//! PostScript; any other token is kept as bounded metadata and is not a language this system sends. A missing
//! command set, a malformed pair, or two command sets that disagree is NO evidence, and the answer to no
//! evidence is `unsupported` - never a guess, never content sniffing.
//!
//! THE PORT BITS ARE THREE OBSERVATIONS. Paper empty, selected and not-error; a port that could not be read is
//! `unavailable` throughout. None of them proves a printer ready: benign bits are what the port reports. Cover
//! open and jam are not in the class's byte at all - they are details a backend with evidence of them may add,
//! and without that evidence they are `unsupported`, never a reassuring `no`.

use alloc::string::String;
use alloc::vec::Vec;

pub const MAX_DEVICE_ID: usize = 4096;
/// Unrecognized command-set tokens kept as metadata, and how long each may be.
pub const MAX_TOKENS: usize = 16;
pub const MAX_TOKEN: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// The length disagrees with what arrived, or is past the bound.
	Length,
	/// A pair without a key, or bytes that are not printable text.
	Malformed,
	/// Two command sets that disagree, or a key given twice.
	Ambiguous,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DeviceId {
	pub manufacturer: Option<String>,
	pub model: Option<String>,
	/// A command set named PostScript.
	pub postscript: bool,
	/// Whether a command set was present at all.
	pub command_set: bool,
	/// Tokens that are not a language this system sends, bounded.
	pub other: Vec<String>,
}

fn is_command_set(key: &str) -> bool {
	key.eq_ignore_ascii_case("CMD") || key.eq_ignore_ascii_case("COMMAND SET")
}

fn is_postscript(token: &str) -> bool {
	["POSTSCRIPT", "POSTSCRIPT2", "POSTSCRIPT3"].iter().any(|name| token.eq_ignore_ascii_case(name))
}

/// Parse a device ID, length prefix included.
pub fn parse(bytes: &[u8]) -> Result<DeviceId, Refusal> {
	if bytes.len() < 2 || bytes.len() > MAX_DEVICE_ID {
		return Err(Refusal::Length);
	}
	let declared = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
	if declared != bytes.len() {
		return Err(Refusal::Length);
	}
	let body = &bytes[2..];
	if body.iter().any(|byte| !(0x20..0x7f).contains(byte) && *byte != b'\t') {
		return Err(Refusal::Malformed);
	}
	let text = core::str::from_utf8(body).map_err(|_| Refusal::Malformed)?;
	let mut out = DeviceId::default();
	let mut seen: Vec<&str> = Vec::new();
	let mut command_tokens: Option<Vec<&str>> = None;
	for pair in text.split(';').map(str::trim).filter(|pair| !pair.is_empty()) {
		let (key, value) = pair.split_once(':').ok_or(Refusal::Malformed)?;
		let (key, value) = (key.trim(), value.trim());
		if key.is_empty() {
			return Err(Refusal::Malformed);
		}
		// A KEY GIVEN TWICE is two answers to one question.
		if seen.iter().any(|held| held.eq_ignore_ascii_case(key)) {
			return Err(Refusal::Ambiguous);
		}
		seen.push(key);
		if is_command_set(key) {
			let tokens: Vec<&str> = value.split(',').map(str::trim).filter(|token| !token.is_empty()).collect();
			// `CMD` and `COMMAND SET` both given must say the same thing.
			if let Some(earlier) = &command_tokens {
				let same = earlier.len() == tokens.len() && earlier.iter().zip(&tokens).all(|(a, b)| a.eq_ignore_ascii_case(b));
				if !same {
					return Err(Refusal::Ambiguous);
				}
			}
			command_tokens = Some(tokens);
		} else if key.eq_ignore_ascii_case("MFG") || key.eq_ignore_ascii_case("MANUFACTURER") {
			out.manufacturer = Some(String::from(&value[..value.len().min(64)]));
		} else if key.eq_ignore_ascii_case("MDL") || key.eq_ignore_ascii_case("MODEL") {
			out.model = Some(String::from(&value[..value.len().min(64)]));
		}
	}
	if let Some(tokens) = command_tokens {
		out.command_set = true;
		for token in tokens {
			if is_postscript(token) {
				out.postscript = true;
			} else if out.other.len() < MAX_TOKENS {
				out.other.push(String::from(&token[..token.len().min(MAX_TOKEN)]));
			}
		}
	}
	Ok(out)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Observation {
	Yes,
	No,
	/// The port could not be read.
	Unavailable,
	/// The backend has no evidence of this at all.
	Unsupported,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Port {
	pub paper_empty: Observation,
	pub selected: Observation,
	pub error: Observation,
	pub cover_open: Observation,
	pub jam: Observation,
}

impl Port {
	pub const UNAVAILABLE: Port = Port { paper_empty: Observation::Unavailable, selected: Observation::Unavailable, error: Observation::Unavailable, cover_open: Observation::Unavailable, jam: Observation::Unavailable };
}

fn observed(bit: bool) -> Observation {
	if bit { Observation::Yes } else { Observation::No }
}

/// The port status byte - bit 5 paper empty, bit 4 selected, bit 3 NOT error - and the details the backend
/// had evidence for.
pub fn port(bits: u8, cover_open: Option<bool>, jam: Option<bool>) -> Port {
	let detail = |evidence: Option<bool>| evidence.map_or(Observation::Unsupported, observed);
	Port { paper_empty: observed(bits & 0x20 != 0), selected: observed(bits & 0x10 != 0), error: observed(bits & 0x08 == 0), cover_open: detail(cover_open), jam: detail(jam) }
}

#[cfg(test)]
mod tests;
