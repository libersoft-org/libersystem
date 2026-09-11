//! Deciding whether an SNTP reply is an answer to a request this host made.
//!
//! A SINGLE FORGED DATAGRAM USED TO MOVE SYSTEM TIME. The client's request was 48 bytes with only
//! the first set, so its transmit timestamp was ZERO - there was no per-request value at all - and
//! the reply was accepted on source port 123 alone, parsed for a transmit timestamp, and handed to
//! TimeService to set the wall clock.
//!
//! THE FIELD THAT MOVES THE CLOCK IS THE ONE THAT MUST BE CHECKED. Validating the ORIGINATE field -
//! the echo of what this client sent - is what correlates the reply; validating the reply's own
//! TRANSMIT field is what stops a server that never set its clock, and a datagram replayed from one
//! that did.

/// Seconds between the NTP epoch (1900) and the Unix epoch (1970): seventy years, seventeen of them
/// leap.
pub const NTP_UNIX_OFFSET: u32 = 2_208_988_800;

/// The fixed size of an SNTP message.
pub const MESSAGE_LEN: usize = 48;

/// Why a reply was not an answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Shorter than a message.
	Short,
	/// Not a server reply, or a version this client did not speak.
	Mode,
	Version,
	/// The leap indicator says the server is not synchronised. RFC 5905 calls it "alarm", and a
	/// server saying so is telling this host not to use its time.
	Alarm,
	/// Stratum outside `1..=15`. 0 is unspecified or a kiss-o'-death, 16 is unsynchronised, and
	/// everything above is RESERVED - so a rule that refused only 0 and 16 accepted 17 through 255,
	/// which is exactly where a forgery would sit.
	Stratum,
	/// The originate timestamp is not the one this host sent: it answers a different request.
	NotOurs,
	/// The server's own transmit timestamp is zero, which names a server that never set it.
	ZeroTransmit,
	/// Byte-identical to the last accepted reply's transmit timestamp: a replay, not a second
	/// reading of the same clock.
	Replayed,
}

/// Build a client request carrying `transmit` as its transmit timestamp.
///
/// A PER-REQUEST VALUE, WHICH THERE WAS NOT ONE OF. The server echoes it in the reply's ORIGINATE
/// field, so it is the only thing that ties a reply to a request - and a constant zero ties it to
/// every request ever made.
pub fn build_request(transmit: u64) -> [u8; MESSAGE_LEN] {
	let mut message = [0u8; MESSAGE_LEN];
	// LI 0, VN 4, Mode 3 (client).
	message[0] = 0x23;
	message[40..48].copy_from_slice(&transmit.to_be_bytes());
	message
}

/// Read a reply, checking everything about it before the wall clock is touched.
///
/// `sent` is the transmit timestamp this host put in its request; `last_accepted` is the transmit
/// timestamp of the previous reply this host acted on, if any.
pub fn parse_reply(message: &[u8], sent: u64, last_accepted: Option<u64>) -> Result<u64, Refusal> {
	if message.len() < MESSAGE_LEN {
		return Err(Refusal::Short);
	}
	let leap: u8 = message[0] >> 6;
	let version: u8 = (message[0] >> 3) & 0x07;
	let mode: u8 = message[0] & 0x07;
	if leap == 3 {
		return Err(Refusal::Alarm);
	}
	if !(3..=4).contains(&version) {
		return Err(Refusal::Version);
	}
	// Mode 4 is a server reply. Anything else is a client, a broadcast or a control message.
	if mode != 4 {
		return Err(Refusal::Mode);
	}
	if !(1..=15).contains(&message[1]) {
		return Err(Refusal::Stratum);
	}
	let originate: u64 = u64::from_be_bytes([message[24], message[25], message[26], message[27], message[28], message[29], message[30], message[31]]);
	if originate != sent {
		return Err(Refusal::NotOurs);
	}
	let transmit: u64 = u64::from_be_bytes([message[40], message[41], message[42], message[43], message[44], message[45], message[46], message[47]]);
	if transmit == 0 {
		return Err(Refusal::ZeroTransmit);
	}
	if last_accepted == Some(transmit) {
		return Err(Refusal::Replayed);
	}
	// The integer-seconds half, converted to the Unix epoch.
	let seconds: u32 = (transmit >> 32) as u32;
	Ok(u64::from(seconds.wrapping_sub(NTP_UNIX_OFFSET)))
}

/// The transmit timestamp of a reply, for a caller that needs to remember what it accepted.
pub fn transmit_of(message: &[u8]) -> Option<u64> {
	if message.len() < MESSAGE_LEN {
		return None;
	}
	Some(u64::from_be_bytes([message[40], message[41], message[42], message[43], message[44], message[45], message[46], message[47]]))
}

#[cfg(test)]
mod tests;
