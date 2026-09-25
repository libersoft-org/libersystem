// THE TYPED OPERATIONS, AND ONLY THESE: random bytes, a PCR read and extended, a secret sealed to a PCR's value
// and unsealed only while it holds, and a quote of a PCR signed by a key only this TPM has. Each builds its own
// commands and reads its own responses; nothing here sends bytes a caller chose.
//
// EVERYTHING LOADED IS FLUSHED, on the failure paths too. A TPM holds a handful of objects and sessions at once,
// and an operation that leaked one a call would stop working after a few - on a machine that was never rebooted
// in between.

use crate::command::{self, Transport, password_authorization, plain_response, policy_authorization, session_response};
use crate::marshal::{Reader, Writer};
use crate::*;
use alloc::vec::Vec;

/// How long a command may take: one that reads or checks something, and one that generates a key.
pub const SHORT_MS: u64 = 2_000;
pub const KEY_MS: u64 = 120_000;
/// The most random bytes one call returns, and the largest secret or nonce this seals or quotes with.
pub const MAX_RANDOM: usize = 1024;
pub const MAX_SECRET: usize = 128;
pub const MAX_NONCE: usize = 64;
/// PCRs 0-23 exist; 17 to 22 belong to the dynamic root of trust and only localities 3 and 4 extend them.
pub const PCRS: u32 = 24;

/// Whether locality 0 may extend `pcr`.
pub fn extendable(pcr: u32) -> bool {
	pcr <= 16 || pcr == 23
}

/// One PCR in the SHA-256 bank, as a `TPML_PCR_SELECTION`.
pub fn select(writer: &mut Writer, pcr: u32) {
	let mut bitmap = [0u8; 3];
	bitmap[(pcr / 8) as usize] |= 1 << (pcr % 8);
	writer.u32(1).u16(ALG_SHA256).u8(3).bytes(&bitmap);
}

/// A `TPML_PCR_SELECTION` read back, which must be exactly the one PCR that was asked for.
pub fn selected(reader: &mut Reader<'_>, pcr: u32) -> Result<(), Refused> {
	let mut bitmap = [0u8; 3];
	bitmap[(pcr / 8) as usize] |= 1 << (pcr % 8);
	if reader.u32()? != 1 || reader.u16()? != ALG_SHA256 {
		return Err(Refused::Value);
	}
	let size = reader.u8()? as usize;
	if reader.take(size)? != bitmap {
		return Err(Refused::Value);
	}
	Ok(())
}

/// The storage key secrets are sealed under: an ECC P-256 restricted decryption key in the owner hierarchy. A
/// primary key is derived from the hierarchy's seed and its template, so the same template makes the same key
/// every time - which is what lets an unseal load what an earlier seal made.
pub fn storage_template() -> Vec<u8> {
	let mut writer = Writer::empty();
	writer.u16(ALG_ECC).u16(ALG_SHA256).u32(OA_FIXED_TPM | OA_FIXED_PARENT | OA_SENSITIVE_DATA_ORIGIN | OA_USER_WITH_AUTH | OA_NO_DA | OA_RESTRICTED | OA_DECRYPT);
	writer.sized(&[]);
	writer.u16(ALG_AES).u16(128).u16(ALG_CFB);
	writer.u16(ALG_NULL);
	writer.u16(ECC_NIST_P256);
	writer.u16(ALG_NULL);
	writer.sized(&[]).sized(&[]);
	writer.into_bytes()
}

/// The key a quote is signed with: an ECC P-256 RESTRICTED signing key, ECDSA over SHA-256. Restricted, because a
/// restricted key signs only what the TPM itself produced - so a quote cannot be forged by asking it to sign bytes
/// that look like one.
pub fn signing_template() -> Vec<u8> {
	let mut writer = Writer::empty();
	writer.u16(ALG_ECC).u16(ALG_SHA256).u32(OA_FIXED_TPM | OA_FIXED_PARENT | OA_SENSITIVE_DATA_ORIGIN | OA_USER_WITH_AUTH | OA_NO_DA | OA_RESTRICTED | OA_SIGN);
	writer.sized(&[]);
	writer.u16(ALG_NULL);
	writer.u16(ALG_ECDSA).u16(ALG_SHA256);
	writer.u16(ECC_NIST_P256);
	writer.u16(ALG_NULL);
	writer.sized(&[]).sized(&[]);
	writer.into_bytes()
}

/// A sealed data object whose only authorization is `policy`: no password opens it, only a policy session
/// that reached this digest does.
pub fn sealed_template(policy: &[u8]) -> Vec<u8> {
	let mut writer = Writer::empty();
	writer.u16(ALG_KEYEDHASH).u16(ALG_SHA256).u32(OA_FIXED_TPM | OA_FIXED_PARENT | OA_NO_DA);
	writer.sized(policy);
	writer.u16(ALG_NULL);
	writer.sized(&[]);
	writer.into_bytes()
}

/// The public point of an ECC key's `TPMT_PUBLIC`, each coordinate 32 bytes.
pub fn ecc_point(public: &[u8]) -> Result<(Vec<u8>, Vec<u8>), Refused> {
	let mut reader = Reader::new(public);
	if reader.u16()? != ALG_ECC {
		return Err(Refused::Value);
	}
	reader.u16()?;
	reader.u32()?;
	reader.sized()?;
	if reader.u16()? != ALG_NULL {
		reader.u16()?;
		reader.u16()?;
	}
	if reader.u16()? != ALG_NULL {
		reader.u16()?;
	}
	if reader.u16()? != ECC_NIST_P256 {
		return Err(Refused::Value);
	}
	if reader.u16()? != ALG_NULL {
		reader.u16()?;
	}
	let x = reader.sized()?.to_vec();
	let y = reader.sized()?.to_vec();
	if x.len() != 32 || y.len() != 32 || reader.remaining() != 0 {
		return Err(Refused::Value);
	}
	Ok((x, y))
}

/// What a quote attests, read out of its `TPMS_ATTEST` and held against what was asked: the TPM's magic, the
/// quote type, the caller's nonce and the one PCR. The PCR digest it carries.
pub fn attested(attest: &[u8], nonce: &[u8], pcr: u32) -> Result<Vec<u8>, Refused> {
	let mut reader = Reader::new(attest);
	if reader.u32()? != GENERATED_VALUE || reader.u16()? != ST_ATTEST_QUOTE {
		return Err(Refused::Value);
	}
	reader.sized()?;
	if reader.sized()? != nonce {
		return Err(Refused::Value);
	}
	// The clock, its reset and restart counts, and whether it is safe; then the firmware version.
	reader.u64()?;
	reader.u32()?;
	reader.u32()?;
	reader.u8()?;
	reader.u64()?;
	selected(&mut reader, pcr)?;
	let digest = reader.sized()?;
	if digest.len() != SHA256_LEN || reader.remaining() != 0 {
		return Err(Refused::Value);
	}
	Ok(digest.to_vec())
}

/// A secret sealed to one PCR's value: the object's two halves, which only this TPM can load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sealed {
	pub pcr: u32,
	pub public: Vec<u8>,
	pub private: Vec<u8>,
}

/// A quote: the attestation, its ECDSA signature, the public point of the key that made it, and the PCR digest
/// the attestation carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quote {
	pub attest: Vec<u8>,
	pub r: Vec<u8>,
	pub s: Vec<u8>,
	pub x: Vec<u8>,
	pub y: Vec<u8>,
	pub pcr_digest: Vec<u8>,
}

pub struct Tpm<T: Transport> {
	pub transport: T,
}

impl<T: Transport> Tpm<T> {
	pub fn new(transport: T) -> Tpm<T> {
		Tpm { transport }
	}

	fn run(&mut self, writer: Writer, duration_ms: u64) -> Result<Vec<u8>, Error> {
		let command = writer.finish().ok_or(Error::Bounds)?;
		command::run(&mut self.transport, &command, duration_ms)
	}

	/// Startup(CLEAR). A TPM that has already started says so, and that is what the caller wanted.
	pub fn startup(&mut self) -> Result<(), Error> {
		let mut writer = Writer::command(ST_NO_SESSIONS, CC_STARTUP);
		writer.u16(0x0000);
		match self.run(writer, SHORT_MS) {
			Ok(_) | Err(Error::Tpm(RC_INITIALIZE)) => Ok(()),
			Err(error) => Err(error),
		}
	}

	/// SelfTest(full).
	pub fn self_test(&mut self) -> Result<(), Error> {
		let mut writer = Writer::command(ST_NO_SESSIONS, CC_SELF_TEST);
		writer.u8(1);
		self.run(writer, KEY_MS).map(|_| ())
	}

	/// `count` bytes from the TPM's generator, asked for in pieces no longer than a digest.
	pub fn random(&mut self, count: usize) -> Result<Vec<u8>, Error> {
		if count > MAX_RANDOM {
			return Err(Error::Bounds);
		}
		let mut out = Vec::with_capacity(count);
		while out.len() < count {
			let ask = (count - out.len()).min(SHA256_LEN) as u16;
			let mut writer = Writer::command(ST_NO_SESSIONS, CC_GET_RANDOM);
			writer.u16(ask);
			let response = self.run(writer, SHORT_MS)?;
			let mut reader = Reader::new(&response[10..]);
			let bytes = reader.sized()?;
			if bytes.is_empty() || bytes.len() > ask as usize || reader.remaining() != 0 {
				return Err(Error::Malformed(Refused::Value));
			}
			out.extend_from_slice(bytes);
		}
		Ok(out)
	}

	/// PCR `pcr` in the SHA-256 bank.
	pub fn pcr_read(&mut self, pcr: u32) -> Result<[u8; SHA256_LEN], Error> {
		if pcr >= PCRS {
			return Err(Error::Locality);
		}
		let mut writer = Writer::command(ST_NO_SESSIONS, CC_PCR_READ);
		select(&mut writer, pcr);
		let response = self.run(writer, SHORT_MS)?;
		let mut reader = Reader::new(&response[10..]);
		reader.u32()?;
		selected(&mut reader, pcr)?;
		if reader.u32()? != 1 {
			return Err(Error::Malformed(Refused::Value));
		}
		let digest = reader.sized()?;
		if digest.len() != SHA256_LEN || reader.remaining() != 0 {
			return Err(Error::Malformed(Refused::Value));
		}
		let mut out = [0u8; SHA256_LEN];
		out.copy_from_slice(digest);
		Ok(out)
	}

	/// Extend PCR `pcr`'s SHA-256 bank with `digest`: the PCR becomes SHA-256(its value || digest).
	pub fn pcr_extend(&mut self, pcr: u32, digest: &[u8; SHA256_LEN]) -> Result<(), Error> {
		if !extendable(pcr) {
			return Err(Error::Locality);
		}
		let mut writer = Writer::command(ST_SESSIONS, CC_PCR_EXTEND);
		writer.u32(pcr);
		password_authorization(&mut writer);
		writer.u32(1).u16(ALG_SHA256).bytes(digest);
		let response = self.run(writer, SHORT_MS)?;
		let parsed = session_response(&response[10..], 0)?;
		if !parsed.parameters.is_empty() {
			return Err(Error::Malformed(Refused::Length));
		}
		Ok(())
	}

	fn create_primary(&mut self, template: &[u8]) -> Result<(u32, Vec<u8>), Error> {
		let mut writer = Writer::command(ST_SESSIONS, CC_CREATE_PRIMARY);
		writer.u32(RH_OWNER);
		password_authorization(&mut writer);
		writer.u16(4).u16(0).u16(0);
		writer.sized(template);
		writer.sized(&[]);
		writer.u32(0);
		let response = self.run(writer, KEY_MS)?;
		let parsed = session_response(&response[10..], 1)?;
		let mut reader = Reader::new(parsed.parameters);
		let public = reader.sized()?.to_vec();
		Ok((parsed.handles[0], public))
	}

	fn flush(&mut self, handle: u32) -> Result<(), Error> {
		let mut writer = Writer::command(ST_NO_SESSIONS, CC_FLUSH_CONTEXT);
		writer.u32(handle);
		self.run(writer, SHORT_MS).map(|_| ())
	}

	fn start_session(&mut self, kind: u8) -> Result<u32, Error> {
		let nonce = self.random(SHA256_LEN)?;
		let mut writer = Writer::command(ST_NO_SESSIONS, CC_START_AUTH_SESSION);
		writer.u32(RH_NULL).u32(RH_NULL);
		writer.sized(&nonce);
		writer.sized(&[]);
		writer.u8(kind);
		writer.u16(ALG_NULL);
		writer.u16(ALG_SHA256);
		let response = self.run(writer, SHORT_MS)?;
		let (handles, rest) = plain_response(&response[10..], 1)?;
		let mut reader = Reader::new(rest);
		if reader.sized()?.len() != SHA256_LEN || reader.remaining() != 0 {
			return Err(Error::Malformed(Refused::Value));
		}
		Ok(handles[0])
	}

	fn policy_pcr(&mut self, session: u32, pcr: u32) -> Result<(), Error> {
		let mut writer = Writer::command(ST_NO_SESSIONS, CC_POLICY_PCR);
		writer.u32(session);
		// AN EMPTY DIGEST: the TPM takes the PCR's value as it is now, which is the value being sealed to - or,
		// in the unseal's session, the value that has to match it.
		writer.sized(&[]);
		select(&mut writer, pcr);
		self.run(writer, SHORT_MS).map(|_| ())
	}

	fn policy_digest(&mut self, session: u32) -> Result<Vec<u8>, Error> {
		let mut writer = Writer::command(ST_NO_SESSIONS, CC_POLICY_GET_DIGEST);
		writer.u32(session);
		let response = self.run(writer, SHORT_MS)?;
		let mut reader = Reader::new(&response[10..]);
		let digest = reader.sized()?;
		if digest.len() != SHA256_LEN || reader.remaining() != 0 {
			return Err(Error::Malformed(Refused::Value));
		}
		Ok(digest.to_vec())
	}

	// The digest a policy session reaches when PCR `pcr` holds its present value, from a trial session.
	fn trial_digest(&mut self, pcr: u32) -> Result<Vec<u8>, Error> {
		let trial = self.start_session(SE_TRIAL)?;
		let digest = match self.policy_pcr(trial, pcr) {
			Ok(()) => self.policy_digest(trial),
			Err(error) => Err(error),
		};
		let _ = self.flush(trial);
		digest
	}

	fn create_sealed(&mut self, parent: u32, secret: &[u8], policy: &[u8]) -> Result<(Vec<u8>, Vec<u8>), Error> {
		let mut writer = Writer::command(ST_SESSIONS, CC_CREATE);
		writer.u32(parent);
		password_authorization(&mut writer);
		writer.u16((2 + 2 + secret.len()) as u16).u16(0).sized(secret);
		writer.sized(&sealed_template(policy));
		writer.sized(&[]);
		writer.u32(0);
		let response = self.run(writer, KEY_MS)?;
		let parsed = session_response(&response[10..], 0)?;
		let mut reader = Reader::new(parsed.parameters);
		let private = reader.sized()?.to_vec();
		let public = reader.sized()?.to_vec();
		Ok((public, private))
	}

	/// Seal `secret` to PCR `pcr`'s present value.
	pub fn seal(&mut self, secret: &[u8], pcr: u32) -> Result<Sealed, Error> {
		if secret.is_empty() || secret.len() > MAX_SECRET {
			return Err(Error::Bounds);
		}
		if pcr >= PCRS {
			return Err(Error::Locality);
		}
		let (primary, _) = self.create_primary(&storage_template())?;
		let sealed = match self.trial_digest(pcr) {
			Ok(policy) => self.create_sealed(primary, secret, &policy),
			Err(error) => Err(error),
		};
		let _ = self.flush(primary);
		let (public, private) = sealed?;
		Ok(Sealed { pcr, public, private })
	}

	fn load(&mut self, parent: u32, sealed: &Sealed) -> Result<u32, Error> {
		let mut writer = Writer::command(ST_SESSIONS, CC_LOAD);
		writer.u32(parent);
		password_authorization(&mut writer);
		writer.sized(&sealed.private);
		writer.sized(&sealed.public);
		let response = self.run(writer, SHORT_MS)?;
		let parsed = session_response(&response[10..], 1)?;
		Ok(parsed.handles[0])
	}

	fn unseal_under(&mut self, object: u32, pcr: u32) -> Result<Vec<u8>, Error> {
		let session = self.start_session(SE_POLICY)?;
		if let Err(error) = self.policy_pcr(session, pcr) {
			let _ = self.flush(session);
			return Err(error);
		}
		let nonce = match self.random(SHA256_LEN) {
			Ok(nonce) => nonce,
			Err(error) => {
				let _ = self.flush(session);
				return Err(error);
			}
		};
		let mut writer = Writer::command(ST_SESSIONS, CC_UNSEAL);
		writer.u32(object);
		// NOT CONTINUED: the TPM ends the session with the command, whichever way the command went.
		policy_authorization(&mut writer, session, &nonce, false);
		match self.run(writer, SHORT_MS) {
			Ok(response) => {
				let parsed = session_response(&response[10..], 0)?;
				let mut reader = Reader::new(parsed.parameters);
				Ok(reader.sized()?.to_vec())
			}
			// A POLICY THAT DOES NOT HOLD is the answer this operation exists to give, and it is named as one.
			Err(Error::Tpm(code)) if format_one_is(code, RC_POLICY_FAIL) || code == RC_PCR_CHANGED => {
				let _ = self.flush(session);
				Err(Error::PolicyRefused)
			}
			Err(error) => {
				let _ = self.flush(session);
				Err(error)
			}
		}
	}

	/// The secret `sealed` holds, if its PCR still has the value it was sealed to - `PolicyRefused` if not.
	pub fn unseal(&mut self, sealed: &Sealed) -> Result<Vec<u8>, Error> {
		if sealed.pcr >= PCRS {
			return Err(Error::Locality);
		}
		let (primary, _) = self.create_primary(&storage_template())?;
		let secret = match self.load(primary, sealed) {
			Ok(object) => {
				let secret = self.unseal_under(object, sealed.pcr);
				let _ = self.flush(object);
				secret
			}
			Err(error) => Err(error),
		};
		let _ = self.flush(primary);
		secret
	}

	fn quote_with(&mut self, key: u32, public: &[u8], nonce: &[u8], pcr: u32) -> Result<Quote, Error> {
		let mut writer = Writer::command(ST_SESSIONS, CC_QUOTE);
		writer.u32(key);
		password_authorization(&mut writer);
		writer.sized(nonce);
		writer.u16(ALG_NULL);
		select(&mut writer, pcr);
		let response = self.run(writer, KEY_MS)?;
		let parsed = session_response(&response[10..], 0)?;
		let mut reader = Reader::new(parsed.parameters);
		let attest = reader.sized()?.to_vec();
		if reader.u16()? != ALG_ECDSA || reader.u16()? != ALG_SHA256 {
			return Err(Error::Malformed(Refused::Value));
		}
		let r = reader.sized()?.to_vec();
		let s = reader.sized()?.to_vec();
		if reader.remaining() != 0 {
			return Err(Error::Malformed(Refused::Length));
		}
		let pcr_digest = attested(&attest, nonce, pcr)?;
		let (x, y) = ecc_point(public)?;
		Ok(Quote { attest, r, s, x, y, pcr_digest })
	}

	/// A quote of PCR `pcr` over `nonce`, signed by a restricted key this TPM made for it.
	pub fn quote(&mut self, nonce: &[u8], pcr: u32) -> Result<Quote, Error> {
		if nonce.len() > MAX_NONCE {
			return Err(Error::Bounds);
		}
		if pcr >= PCRS {
			return Err(Error::Locality);
		}
		let (key, public) = self.create_primary(&signing_template())?;
		let quote = self.quote_with(key, &public, nonce, pcr);
		let _ = self.flush(key);
		quote
	}
}
