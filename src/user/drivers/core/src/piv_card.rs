// A PIV CARD, PLAYED BY THE IN-GUEST SMART-CARD FIXTURE: the application's SELECT, its two public data
// objects, GET RESPONSE continuation, and GENERAL AUTHENTICATE with the PIV Authentication key once a
// verification has succeeded.
//
// WHAT IT IS AND IS NOT. A test double for SmartcardService's gate, written from SP 800-73-4 Part 2 and
// sharing no code with the service's command grammar in `service_logic::piv` - the two agreeing is two
// readings of the specification agreeing. It computes no signature: the three it can give were made
// once with OpenSSL for the three challenges it knows (see `data.rs`), and any other challenge is refused
// as a card refuses data it cannot use.

mod data;

use alloc::vec::Vec;

/// The ATR the fixture's cards answer with: 3B 88 80 01, the historical bytes "LIBERPIV", and TCK.
/// T=0 offered first and T=1 as well, so the check byte is present.
pub fn atr() -> Vec<u8> {
	let mut bytes = alloc::vec![0x3b, 0x88, 0x80, 0x01];
	bytes.extend_from_slice(b"LIBERPIV");
	let tck = bytes[1..].iter().fold(0u8, |sum, byte| sum ^ byte);
	bytes.push(tck);
	bytes
}

/// The certificate the card holds for its PIV Authentication key, DER.
pub fn certificate() -> &'static [u8] {
	&data::CERTIFICATE
}

/// The challenges the card can sign, and the signature for each.
pub fn known() -> [(&'static [u8; 32], &'static [u8]); 3] {
	[(&data::CHALLENGE_1, &data::SIGNATURE_1), (&data::CHALLENGE_2, &data::SIGNATURE_2), (&data::CHALLENGE_3, &data::SIGNATURE_3)]
}

const PIV_AID: [u8; 11] = [0xa0, 0x00, 0x00, 0x03, 0x08, 0x00, 0x00, 0x10, 0x00, 0x01, 0x00];

/// What the fixture's pinpad reports for a verification.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pinpad {
	Verified,
	Incorrect,
	Blocked,
}

pub struct Card {
	selected: bool,
	verified: bool,
	retries: u8,
	// The rest of a long answer, for GET RESPONSE.
	pending: Vec<u8>,
}

impl Default for Card {
	fn default() -> Self {
		Self::new()
	}
}

fn status(sw1: u8, sw2: u8) -> Vec<u8> {
	alloc::vec![sw1, sw2]
}

// A BER length, short or long form.
fn length(out: &mut Vec<u8>, len: usize) {
	if len < 0x80 {
		out.push(len as u8);
	} else if len < 0x100 {
		out.extend_from_slice(&[0x81, len as u8]);
	} else {
		out.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
	}
}

impl Card {
	pub fn new() -> Self {
		Self { selected: false, verified: false, retries: 3, pending: Vec::new() }
	}

	/// Power off and on: nothing selected, nothing verified, nothing pending. The retry counter is the
	/// card's own and survives, as a real card's does.
	pub fn reset(&mut self) {
		self.selected = false;
		self.verified = false;
		self.pending.clear();
	}

	pub fn verified(&self) -> bool {
		self.verified
	}

	// Up to 256 bytes of `data` now, the rest behind 61xx.
	fn answer(&mut self, mut data: Vec<u8>) -> Vec<u8> {
		if data.len() <= 256 {
			self.pending.clear();
			data.extend_from_slice(&[0x90, 0x00]);
			return data;
		}
		self.pending = data.split_off(256);
		let remaining = self.pending.len().min(256) as u8;
		data.extend_from_slice(&[0x61, remaining]);
		data
	}

	/// A secure verification's outcome, as the pinpad reports it: the status word the card gives.
	pub fn verify(&mut self, pinpad: Pinpad) -> Vec<u8> {
		if !self.selected {
			return status(0x69, 0x85);
		}
		if self.retries == 0 {
			return status(0x69, 0x83);
		}
		match pinpad {
			Pinpad::Verified => {
				self.verified = true;
				self.retries = 3;
				status(0x90, 0x00)
			}
			Pinpad::Incorrect => {
				self.verified = false;
				self.retries -= 1;
				if self.retries == 0 { status(0x69, 0x83) } else { status(0x63, 0xc0 | self.retries) }
			}
			Pinpad::Blocked => {
				self.verified = false;
				self.retries = 0;
				status(0x69, 0x83)
			}
		}
	}

	/// One command APDU, and the response with its status word - never more than 258 bytes.
	pub fn apdu(&mut self, command: &[u8]) -> Vec<u8> {
		if command.len() < 4 {
			return status(0x67, 0x00);
		}
		let (class, instruction, p1, p2) = (command[0], command[1], command[2], command[3]);
		if class != 0x00 {
			return status(0x6e, 0x00);
		}
		let data: &[u8] = if command.len() > 5 { &command[5..(5 + command[4] as usize).min(command.len())] } else { &[] };
		match instruction {
			0xa4 if p1 == 0x04 && p2 == 0x00 => {
				// The full AID or its right truncation, as SP 800-73-4 Part 1 allows.
				if data == PIV_AID || data == &PIV_AID[..9] {
					self.selected = true;
					self.verified = false;
					// The Application Property Template: the PIX, and the AID of the authority.
					self.answer(alloc::vec![0x61, 0x11, 0x4f, 0x06, 0x00, 0x00, 0x10, 0x00, 0x01, 0x00, 0x79, 0x07, 0x4f, 0x05, 0xa0, 0x00, 0x00, 0x03, 0x08])
				} else {
					self.selected = false;
					status(0x6a, 0x82)
				}
			}
			0xcb if p1 == 0x3f && p2 == 0xff => {
				if !self.selected {
					return status(0x6a, 0x82);
				}
				match data {
					// The Discovery Object: the AID, and a PIN usage policy of the PIV PIN only.
					[0x5c, 0x01, 0x7e] => self.answer(alloc::vec![0x7e, 0x12, 0x4f, 0x0b, 0xa0, 0x00, 0x00, 0x03, 0x08, 0x00, 0x00, 0x10, 0x00, 0x01, 0x00, 0x5f, 0x2f, 0x02, 0x40, 0x00]),
					// The PIV Authentication certificate: 53 { 70 certificate, 71 00, FE (LRC) }.
					[0x5c, 0x03, 0x5f, 0xc1, 0x05] => {
						let certificate = certificate();
						let mut inner = alloc::vec![0x70];
						length(&mut inner, certificate.len());
						inner.extend_from_slice(certificate);
						inner.extend_from_slice(&[0x71, 0x01, 0x00, 0xfe, 0x00]);
						let mut object = alloc::vec![0x53];
						length(&mut object, inner.len());
						object.extend_from_slice(&inner);
						self.answer(object)
					}
					_ => status(0x6a, 0x82),
				}
			}
			0xc0 => {
				if self.pending.is_empty() {
					return status(0x6f, 0x00);
				}
				let wanted = match command.get(4) {
					Some(0) | None => 256,
					Some(&le) => le as usize,
				};
				let take = wanted.min(self.pending.len());
				let rest = self.pending.split_off(take);
				let mut out = core::mem::replace(&mut self.pending, rest);
				if self.pending.is_empty() {
					out.extend_from_slice(&[0x90, 0x00]);
				} else {
					out.extend_from_slice(&[0x61, self.pending.len().min(256) as u8]);
				}
				out
			}
			0x87 => {
				if !self.selected {
					return status(0x6a, 0x82);
				}
				if p1 != 0x11 || p2 != 0x9a {
					return status(0x6a, 0x86);
				}
				if !self.verified {
					return status(0x69, 0x82);
				}
				// 7C 24 { 82 00, 81 20 challenge }: the only template this card answers.
				let Some(challenge) = data.strip_prefix(&[0x7c, 0x24, 0x82, 0x00, 0x81, 0x20][..]).filter(|rest| rest.len() == 32) else { return status(0x6a, 0x80) };
				let Some((_, signature)) = known().into_iter().find(|(known, _)| known.as_slice() == challenge) else { return status(0x6a, 0x80) };
				let mut response = alloc::vec![0x7c];
				length(&mut response, signature.len() + 2);
				response.push(0x82);
				length(&mut response, signature.len());
				response.extend_from_slice(signature);
				self.answer(response)
			}
			_ => status(0x6d, 0x00),
		}
	}
}

#[cfg(test)]
mod tests;
