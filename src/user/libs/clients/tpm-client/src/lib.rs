//! The TPM client a program links: the machine's one TPM, through whichever of the three grants the program was
//! given.
//!
//! THREE GRANTS, ONE CLIENT. PermissionManager mints each TPM grant separately - `tpm` (info, random, PCR read,
//! quote), `tpm-measure` (PCR extend) and `tpm-seal` (seal, unseal) - as a connection of its own. This client holds
//! whichever the program received and sends each call on the connection whose grant carries it; a call no held grant
//! carries is answered `not-granted` HERE, in the program's own process, without a message. That answer is a
//! convenience and not the boundary: TpmService answers `not-granted` itself to an operation sent on a connection
//! whose grant does not carry it, which is what a program that does not link this crate meets.
//!
//! WHY A CRATE AND NOT `Client::new(ChannelTransport { .. })`, for the reason `font-client` gives: the generated
//! client is generic over its transport, so instantiating it in a consumer copies the codec into that consumer. The
//! copy is made once, inside `tpm-proto`, and reaches a consumer through the trampolines `tpm-client-provider`
//! declares.
//!
//! WHAT A CALL COSTS: `seal`, `unseal` and `quote` each generate a primary key in the TPM; the others are one short
//! command. One call is outstanding per connection.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use tpm_proto::generated::liber::tpm::v1::{BytesAnswer, InfoAnswer, Outcome, QuoteAnswer, Status};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_tpm_tpm_info"]
	fn tpm_info(chan: u64) -> Option<Result<InfoAnswer, Error>>;
	#[link_name = "liber_channel_liber_tpm_tpm_random"]
	fn tpm_random(chan: u64, count: &u32) -> Option<Result<BytesAnswer, Error>>;
	#[link_name = "liber_channel_liber_tpm_tpm_pcr_read"]
	fn tpm_pcr_read(chan: u64, pcr: &u32) -> Option<Result<BytesAnswer, Error>>;
	#[link_name = "liber_channel_liber_tpm_tpm_pcr_extend"]
	fn tpm_pcr_extend(chan: u64, pcr: &u32, digest: &[u8]) -> Option<Result<Status, Error>>;
	#[link_name = "liber_channel_liber_tpm_tpm_seal"]
	fn tpm_seal(chan: u64, pcr: &u32, secret: &[u8]) -> Option<Result<BytesAnswer, Error>>;
	#[link_name = "liber_channel_liber_tpm_tpm_unseal"]
	fn tpm_unseal(chan: u64, sealed: &[u8]) -> Option<Result<BytesAnswer, Error>>;
	#[link_name = "liber_channel_liber_tpm_tpm_quote"]
	fn tpm_quote(chan: u64, pcr: &u32, nonce: &[u8]) -> Option<Result<QuoteAnswer, Error>>;
}

/// The TPM, through the grants this program holds. A zero connection is a grant it was not given.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct TpmClient {
	tpm: u64,
	measure: u64,
	seal: u64,
}

#[inline(always)]
fn not_granted_bytes() -> Option<Result<BytesAnswer, Error>> {
	Some(Ok(BytesAnswer { outcome: Outcome::NotGranted, code: 0, bytes: Vec::new() }))
}

impl TpmClient {
	/// The connections the program received under `TPM`, `TPMMEASURE` and `TPMSEAL`, each zero if it was not given.
	#[inline(always)]
	pub const fn new(tpm: u64, measure: u64, seal: u64) -> Self {
		Self { tpm, measure, seal }
	}

	/// The interface, the TPM's identity and whether sealing works - on any held connection, since every grant
	/// carries it.
	#[inline(always)]
	pub fn info(&mut self) -> Option<Result<InfoAnswer, Error>> {
		let chan = [self.tpm, self.measure, self.seal].into_iter().find(|chan| *chan != 0);
		match chan {
			Some(chan) => unsafe { tpm_info(chan) },
			None => Some(Ok(InfoAnswer { outcome: Outcome::NotGranted, code: 0, info: None })),
		}
	}

	/// At most 1024 random bytes. The `tpm` grant.
	#[inline(always)]
	pub fn random(&mut self, count: u32) -> Option<Result<BytesAnswer, Error>> {
		if self.tpm == 0 {
			return not_granted_bytes();
		}
		unsafe { tpm_random(self.tpm, &count) }
	}

	/// PCR 0-23 of the SHA-256 bank. The `tpm` grant.
	#[inline(always)]
	pub fn pcr_read(&mut self, pcr: u32) -> Option<Result<BytesAnswer, Error>> {
		if self.tpm == 0 {
			return not_granted_bytes();
		}
		unsafe { tpm_pcr_read(self.tpm, &pcr) }
	}

	/// Extend PCR 16 or 23 with a SHA-256 digest. The `tpm-measure` grant.
	#[inline(always)]
	pub fn pcr_extend(&mut self, pcr: u32, digest: &[u8]) -> Option<Result<Status, Error>> {
		if self.measure == 0 {
			return Some(Ok(Status { outcome: Outcome::NotGranted, code: 0 }));
		}
		unsafe { tpm_pcr_extend(self.measure, &pcr, digest) }
	}

	/// Seal at most 96 bytes to a PCR's present value. The `tpm-seal` grant.
	#[inline(always)]
	pub fn seal(&mut self, pcr: u32, secret: &[u8]) -> Option<Result<BytesAnswer, Error>> {
		if self.seal == 0 {
			return not_granted_bytes();
		}
		unsafe { tpm_seal(self.seal, &pcr, secret) }
	}

	/// The secret a sealed object holds. The `tpm-seal` grant.
	#[inline(always)]
	pub fn unseal(&mut self, sealed: &[u8]) -> Option<Result<BytesAnswer, Error>> {
		if self.seal == 0 {
			return not_granted_bytes();
		}
		unsafe { tpm_unseal(self.seal, sealed) }
	}

	/// A quote of PCR 0-23 over a nonce of at most 64 bytes. The `tpm` grant.
	#[inline(always)]
	pub fn quote(&mut self, pcr: u32, nonce: &[u8]) -> Option<Result<QuoteAnswer, Error>> {
		if self.tpm == 0 {
			return Some(Ok(QuoteAnswer { outcome: Outcome::NotGranted, code: 0, quote: None }));
		}
		unsafe { tpm_quote(self.tpm, &pcr, nonce) }
	}
}
