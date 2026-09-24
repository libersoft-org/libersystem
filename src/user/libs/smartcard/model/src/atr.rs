//! THE ANSWER-TO-RESET (ISO/IEC 7816-3), parsed to what a host needs of it: which transmission
//! protocols the card offers, which it offers first, its historical bytes - and whether it is an ATR
//! at all.
//!
//! THE GRAMMAR. TS is the convention (0x3B direct, 0x3F inverse). T0 carries Y1, the presence bits of
//! TA1..TD1, and K, the number of historical bytes. Each TDi present carries the next Y and a protocol
//! T; the chain ends at a TD that is absent. Then K historical bytes, then TCK - which is present
//! exactly when a protocol other than T=0 is offered, and makes the XOR of every byte from T0 through
//! itself zero. At most 33 bytes in all.
//!
//! EVERY LENGTH IS CHECKED AGAINST THE BYTES THAT ARE THERE, never trusted: a chain that promises more
//! interface bytes than follow, historical bytes past the end, or bytes left over after TCK are all a
//! refusal, and nothing is allocated from a declared length.

/// The longest ATR the standard allows.
pub const MAX_ATR: usize = 33;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Shorter than TS and T0, or longer than 33 bytes.
	Length,
	/// TS is neither convention.
	Convention,
	/// The interface bytes or historical bytes run past the end.
	Truncated,
	/// Bytes remain after everything the ATR declared.
	Trailing,
	/// The check byte does not make the XOR zero.
	Check,
	/// A TD names protocol 15 as a transmission protocol, or the chain names more levels than fit.
	Chain,
}

/// What a host needs from an ATR.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Atr {
	/// Bit n set: protocol T=n is offered. T=0 is implicit when no TD names a protocol.
	pub protocols: u16,
	/// The protocol the card offers first, which is the one it uses without negotiation.
	pub first: u8,
	/// TA1, the clock-rate conversion and baud-rate adjustment, when present.
	pub ta1: Option<u8>,
	/// Where the historical bytes start, and how many there are.
	pub historical_at: usize,
	pub historical_len: usize,
	pub inverse: bool,
}

impl Atr {
	pub fn offers(&self, protocol: u8) -> bool {
		protocol < 16 && self.protocols & (1 << protocol) != 0
	}
}

/// Parse and validate one ATR.
pub fn parse(bytes: &[u8]) -> Result<Atr, Refusal> {
	if bytes.len() < 2 || bytes.len() > MAX_ATR {
		return Err(Refusal::Length);
	}
	let inverse = match bytes[0] {
		0x3b => false,
		0x3f => true,
		_ => return Err(Refusal::Convention),
	};
	let t0 = bytes[1];
	let historical_len = (t0 & 0x0f) as usize;
	let mut presence = t0 >> 4;
	let mut at = 2;
	let mut protocols: u16 = 0;
	let mut first: Option<u8> = None;
	let mut ta1 = None;
	let mut level = 1;
	loop {
		// TA, TB, TC, TD in that order, each present when its bit is set.
		for bit in 0..3 {
			if presence & (1 << bit) != 0 {
				let Some(&byte) = bytes.get(at) else { return Err(Refusal::Truncated) };
				if level == 1 && bit == 0 {
					ta1 = Some(byte);
				}
				at += 1;
			}
		}
		if presence & 0b1000 == 0 {
			break;
		}
		let Some(&td) = bytes.get(at) else { return Err(Refusal::Truncated) };
		at += 1;
		let protocol = td & 0x0f;
		// T=15 is a global-parameters indicator, not a protocol a card transmits with.
		if protocol != 15 {
			protocols |= 1 << protocol;
			first.get_or_insert(protocol);
		}
		presence = td >> 4;
		level += 1;
		if level > 8 {
			return Err(Refusal::Chain);
		}
	}
	if protocols == 0 {
		protocols = 1;
	}
	let historical_at = at;
	at += historical_len;
	if at > bytes.len() {
		return Err(Refusal::Truncated);
	}
	// TCK EXACTLY WHEN SOMETHING OTHER THAN T=0 IS OFFERED.
	let needs_check = protocols & !1 != 0;
	if needs_check {
		if at >= bytes.len() {
			return Err(Refusal::Truncated);
		}
		if bytes[1..=at].iter().fold(0u8, |sum, byte| sum ^ byte) != 0 {
			return Err(Refusal::Check);
		}
		at += 1;
	}
	if at != bytes.len() {
		return Err(Refusal::Trailing);
	}
	Ok(Atr { protocols, first: first.unwrap_or(0), ta1, historical_at, historical_len, inverse })
}

/// The check byte that closes an ATR whose other bytes are `body` (TS through the last historical
/// byte): what makes the XOR from T0 onwards zero. For building an ATR, never for accepting one.
pub fn check_byte(body: &[u8]) -> u8 {
	body.iter().skip(1).fold(0u8, |sum, byte| sum ^ byte)
}

#[cfg(test)]
mod tests;
