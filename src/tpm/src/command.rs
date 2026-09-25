// ONE COMMAND AND ITS RESPONSE: the transport that carries them, the header check every response passes before
// anything reads it, the authorization areas the typed operations use, and the bounded retry of the warnings
// that mean "ask again".

use crate::marshal::{Reader, Writer};
use crate::{Error, MAX_MESSAGE, RC_RETRY, RC_SUCCESS, RC_TESTING, RC_YIELDED, RS_PW, Refused, ST_NO_SESSIONS, ST_SESSIONS, TransportError};
use alloc::vec::Vec;

/// What carries a command to a TPM and its response back: a FIFO or CRB interface over registers, or a socket
/// to a simulator.
pub trait Transport {
	/// Send one command and put its whole response in `response`. `duration_ms` is how long the TPM may take
	/// before the transport cancels it.
	fn execute(&mut self, command: &[u8], response: &mut Vec<u8>, duration_ms: u64) -> Result<(), TransportError>;
}

/// A response whose header was checked.
#[derive(Debug, PartialEq, Eq)]
pub struct Response<'a> {
	pub tag: u16,
	pub code: u32,
	/// Everything after the ten-byte header.
	pub body: &'a [u8],
}

/// A response's header, held against the command's `tag` and against the bytes that arrived.
///
/// A NON-SUCCESS RESPONSE IS TEN BYTES AND NOTHING ELSE, with no sessions whatever the command had - the
/// specification says so, and a longer one is a TPM saying something the caller would read as parameters.
pub fn check_response(bytes: &[u8], tag: u16) -> Result<Response<'_>, Refused> {
	if bytes.len() < 10 {
		return Err(Refused::Short);
	}
	let mut header = Reader::new(bytes);
	let answered = header.u16()?;
	let size = header.u32()? as usize;
	let code = header.u32()?;
	if size != bytes.len() || size > MAX_MESSAGE {
		return Err(Refused::Length);
	}
	if code != RC_SUCCESS {
		if answered != ST_NO_SESSIONS || size != 10 {
			return Err(Refused::Tag);
		}
		return Ok(Response { tag: answered, code, body: &[] });
	}
	if answered != tag {
		return Err(Refused::Tag);
	}
	Ok(Response { tag: answered, code, body: &bytes[10..] })
}

/// The warnings a caller asks again for: the TPM yielded, is testing itself, or wants the command again.
pub fn retryable(code: u32) -> bool {
	matches!(code, RC_RETRY | RC_YIELDED | RC_TESTING)
}

/// How many times a retryable warning is asked again before it is the answer.
pub const RETRIES: u32 = 8;

/// Run one command: sent, its response checked, and a retryable warning asked again a bounded number of
/// times. The response's bytes on success; the TPM's code otherwise.
pub fn run(transport: &mut impl Transport, command: &[u8], duration_ms: u64) -> Result<Vec<u8>, Error> {
	let tag = u16::from_be_bytes([command[0], command[1]]);
	let mut response = Vec::new();
	for _ in 0..RETRIES {
		response.clear();
		transport.execute(command, &mut response, duration_ms)?;
		let checked = check_response(&response, tag)?;
		if checked.code == RC_SUCCESS {
			return Ok(response);
		}
		if !retryable(checked.code) {
			return Err(Error::Tpm(checked.code));
		}
	}
	let last = check_response(&response, tag)?;
	Err(Error::Tpm(last.code))
}

/// THE PASSWORD SESSION with the empty password: the authorization every hierarchy and object here is made with.
/// Its nonce and its HMAC are both empty, as a password session's must be.
pub fn password_authorization(writer: &mut Writer) {
	writer.u32(9);
	writer.u32(RS_PW);
	writer.sized(&[]);
	writer.u8(0);
	writer.sized(&[]);
}

/// A POLICY SESSION as the authorization, with no HMAC: what satisfies the object is the policy the session
/// accumulated, not a secret. `continue_session` keeps the session after the command.
pub fn policy_authorization(writer: &mut Writer, session: u32, nonce: &[u8], continue_session: bool) {
	writer.u32((4 + 2 + nonce.len() + 1 + 2) as u32);
	writer.u32(session);
	writer.sized(nonce);
	writer.u8(u8::from(continue_session));
	writer.sized(&[]);
}

/// A response to a command sent with sessions: its handles, then the parameters by their own size - with the
/// authorization area after them walked and bounded, not trusted.
pub struct SessionResponse<'a> {
	pub handles: Vec<u32>,
	pub parameters: &'a [u8],
}

pub fn session_response(body: &[u8], handles: usize) -> Result<SessionResponse<'_>, Refused> {
	let mut reader = Reader::new(body);
	let mut found = Vec::with_capacity(handles);
	for _ in 0..handles {
		found.push(reader.u32()?);
	}
	let size = reader.u32()? as usize;
	let parameters = reader.take(size)?;
	// EVERY SESSION'S RESPONSE AREA is a nonce, an attribute byte and an HMAC; what follows the parameters is
	// nothing else, and walking it is how a response that ends inside one is refused.
	while reader.remaining() > 0 {
		reader.sized()?;
		reader.u8()?;
		reader.sized()?;
	}
	Ok(SessionResponse { handles: found, parameters })
}

/// A response to a command sent without sessions: its handles, then the parameters, which are the rest.
pub fn plain_response(body: &[u8], handles: usize) -> Result<(Vec<u32>, &[u8]), Refused> {
	let mut reader = Reader::new(body);
	let mut found = Vec::with_capacity(handles);
	for _ in 0..handles {
		found.push(reader.u32()?);
	}
	Ok((found, reader.rest()))
}

/// Which tag a command carries, by whether it has an authorization area.
pub fn tag_for(sessions: bool) -> u16 {
	if sessions { ST_SESSIONS } else { ST_NO_SESSIONS }
}
