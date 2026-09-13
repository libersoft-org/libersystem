//! THE ENTROPY POOL, AND THE ONE THING IT REFUSES TO CONCLUDE.
//!
//! A virtual machine's entropy device is a real seed source and it is NOT a proof of cryptographic
//! health. The host may be handing the guest a recording; the device may be backed by a file; a
//! guest resumed from a snapshot is handed a device that is about to produce, again, exactly the
//! bytes it produced before the snapshot was taken. None of that is detectable from inside the
//! guest, and none of it makes the bytes worthless either - they are a seed, and a seed is worth
//! having. What must not happen is the step from "a device gave me bytes" to "this machine has
//! cryptographic-quality randomness", because that step is the one nothing downstream can undo.
//!
//! So three rules, and they are the whole of this crate's opinion:
//!
//!   - BYTES ARE CREDITED AT A FRACTION OF THEIR LENGTH. The submitter does not say what its bytes
//!     are worth; the pool decides, from the KIND of source, at a rate below one bit per bit.
//!   - THE POOL REFUSES UNTIL IT HAS ENOUGH. Below the seed threshold `draw` answers nothing at all
//!     rather than answering weakly, because a caller asking for key material has no way to act on
//!     "this is probably fine".
//!   - WHAT IT REPORTS IS CREDIT AND SOURCES, NEVER HEALTH. `health` says how many bits are credited
//!     and where they came from. Whether that is enough for a given purpose is the caller's
//!     question, and a pool that answered it would be answering for every future caller too.
//!
//! It lives in a crate of its own, outside the kernel, for the reason `driver-binding` does: the
//! kernel is not host-testable and these are decisions, so the decisions go where a test can watch
//! them fail.

#![no_std]

use bootproto::sha256;

// WHAT THE POOL NEEDS BEFORE IT WILL ANSWER AT ALL. A 256-bit key is what the draw is keyed on, so
// crediting less than that and answering anyway would be handing out a key with fewer bits behind it
// than it appears to have.
pub const SEED_BITS: u32 = 256;

// THE MOST ONE SUBMISSION MAY EVER BE WORTH. A source that hands over a megabyte in one call has not
// thereby proved a megabyte of unpredictability; it has proved that it can produce bytes quickly.
pub const MAX_SUBMISSION_BITS: u32 = 512;

// THE MOST THE POOL WILL EVER HOLD. Credit is not a currency to accumulate - past this point more
// bytes change nothing, and a number that only grows would eventually report a machine as arbitrarily
// well seeded because its device had been running for a month.
pub const CREDIT_CEILING_BITS: u32 = 4096;

/// Where a submission came from, which is what decides the rate it is credited at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
	/// A paravirtual device: virtio-rng. Credited at a QUARTER of its length, because everything
	/// behind it is the host's choice and the guest cannot see any of it.
	Paravirtual,
	/// A CPU instruction: RDRAND and its equivalents. Credited at a HALF, because it is at least
	/// this machine's own silicon - and still not at full rate, because a caller cannot audit it
	/// either.
	Hardware,
}

impl Source {
	// The divisor applied to a submission's bit count.
	const fn rate(self) -> u32 {
		match self {
			Source::Paravirtual => 4,
			Source::Hardware => 2,
		}
	}

	const fn tag(self) -> u8 {
		match self {
			Source::Paravirtual => 1,
			Source::Hardware => 2,
		}
	}
}

/// What `len` bytes from `source` are worth, in bits.
///
/// A separate function because it is the number an auditor argues with, and it should be readable
/// without reading a pool.
pub fn credit_for(source: Source, len: usize) -> u32 {
	let bits: u64 = (len as u64).saturating_mul(8) / source.rate() as u64;
	if bits > MAX_SUBMISSION_BITS as u64 { MAX_SUBMISSION_BITS } else { bits as u32 }
}

/// What the pool will say about itself. Credit and provenance; no verdict.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Health {
	pub credited_bits: u32,
	pub submissions: u32,
	pub paravirtual_submissions: u32,
	pub hardware_submissions: u32,
	pub draws: u64,
	pub seeded: bool,
}

// The domain separators. Absorbing, drawing and rekeying are three different uses of one key, and a
// construction that did not separate them would let an attacker who can influence one predict
// another.
const DOMAIN_ABSORB: u8 = 0x01;
const DOMAIN_DRAW: u8 = 0x02;
const DOMAIN_REKEY: u8 = 0x03;

// How much of a submission goes into one compression step. The pool chains rather than concatenating
// so that absorbing a large buffer costs a fixed amount of stack - a kernel has no room for a
// four-kilobyte scratch on the path a device interrupt runs on.
const ABSORB_CHUNK: usize = 32;

// key || counter || tag || chunk, which is what one absorb step hashes.
const ABSORB_INPUT: usize = 1 + 32 + 8 + 1 + ABSORB_CHUNK;

pub struct Pool {
	// The pool's whole state. Zero at boot, which is why `seeded` is false there: a key of zeros is
	// a key everybody has.
	key: [u8; 32],
	// Never rewinds, and that is a property rather than a detail: it is what makes the output of a
	// pool that absorbed the same bytes twice - a driver restarted with a device that repeats -
	// different both times.
	counter: u64,
	credited_bits: u32,
	submissions: u32,
	paravirtual: u32,
	hardware: u32,
	draws: u64,
}

impl Default for Pool {
	fn default() -> Self {
		Self::new()
	}
}

impl Pool {
	pub const fn new() -> Pool {
		Pool { key: [0u8; 32], counter: 0, credited_bits: 0, submissions: 0, paravirtual: 0, hardware: 0, draws: 0 }
	}

	pub fn seeded(&self) -> bool {
		self.credited_bits >= SEED_BITS
	}

	pub fn health(&self) -> Health {
		Health { credited_bits: self.credited_bits, submissions: self.submissions, paravirtual_submissions: self.paravirtual, hardware_submissions: self.hardware, draws: self.draws, seeded: self.seeded() }
	}

	/// Take a submission in, and answer what it was credited.
	///
	/// AN EMPTY SUBMISSION IS NOT AN ERROR AND IS NOT WORTH ANYTHING. A device that answered a
	/// request with nothing has told the pool exactly that, and the state still moves - the counter
	/// records that a submission happened, which is what keeps a failing device from producing a
	/// repeatable pool.
	pub fn absorb(&mut self, bytes: &[u8], source: Source) -> u32 {
		let mut input: [u8; ABSORB_INPUT] = [0u8; ABSORB_INPUT];
		let mut offset: usize = 0;
		loop {
			let end: usize = (offset + ABSORB_CHUNK).min(bytes.len());
			let chunk: &[u8] = &bytes[offset..end];
			input[0] = DOMAIN_ABSORB;
			input[1..33].copy_from_slice(&self.key);
			input[33..41].copy_from_slice(&self.counter.to_le_bytes());
			input[41] = source.tag();
			input[42..42 + chunk.len()].copy_from_slice(chunk);
			self.key = sha256::digest(&input[..42 + chunk.len()]);
			self.counter = self.counter.wrapping_add(1);
			offset = end;
			if offset >= bytes.len() {
				break;
			}
		}
		let credit: u32 = credit_for(source, bytes.len());
		self.credited_bits = (self.credited_bits.saturating_add(credit)).min(CREDIT_CEILING_BITS);
		self.submissions = self.submissions.saturating_add(1);
		match source {
			Source::Paravirtual => self.paravirtual = self.paravirtual.saturating_add(1),
			Source::Hardware => self.hardware = self.hardware.saturating_add(1),
		}
		credit
	}

	/// Fill `out`, or refuse.
	///
	/// FALSE IS THE ANSWER, NOT WEAK BYTES. A caller that asked for key material and got a buffer it
	/// cannot tell from good key material has no way to act on the difference, so the difference has
	/// to be in the return rather than in the bytes.
	pub fn draw(&mut self, out: &mut [u8]) -> bool {
		if !self.seeded() {
			return false;
		}
		let mut written: usize = 0;
		let mut block_input: [u8; 41] = [0u8; 41];
		while written < out.len() {
			self.counter = self.counter.wrapping_add(1);
			block_input[0] = DOMAIN_DRAW;
			block_input[1..33].copy_from_slice(&self.key);
			block_input[33..41].copy_from_slice(&self.counter.to_le_bytes());
			let block: [u8; 32] = sha256::digest(&block_input);
			let take: usize = (out.len() - written).min(32);
			out[written..written + take].copy_from_slice(&block[..take]);
			written += take;
		}
		// BACKTRACKING RESISTANCE. The key that produced what was just handed out is replaced, so a
		// reader of this structure a moment later cannot recompute the bytes somebody else is using
		// as a key.
		self.counter = self.counter.wrapping_add(1);
		block_input[0] = DOMAIN_REKEY;
		block_input[1..33].copy_from_slice(&self.key);
		block_input[33..41].copy_from_slice(&self.counter.to_le_bytes());
		self.key = sha256::digest(&block_input);
		self.draws = self.draws.saturating_add(1);
		true
	}
}

#[cfg(test)]
#[path = "pool/tests.rs"]
mod tests;
