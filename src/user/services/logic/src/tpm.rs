// TPMSERVICE'S DECISIONS: which operations a grant carries, which PCRs each operation may name, the bounds, the
// component tag a sealed secret carries, and the queue - one request at the driver, one call per connection,
// sixteen waiting in all - with what the provider's comings and goings do to the calls in it.
//
// THE TPM RUNS ONE COMMAND AT A TIME and an operation is several commands, so the service keeps one operation
// outstanding at the driver and queues the rest; a driver that went away while one was in flight answers it
// `interrupted` and NEVER replays it - an extend may have reached the TPM before the driver died, and a replay
// would extend twice.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// What a minted connection may do, split by what it can hurt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grant {
	/// Observation: info, random, PCR read, quote.
	Tpm,
	/// PCR extend, which changes what every later quote and unseal answers.
	Measure,
	/// Seal and unseal.
	Seal,
}

/// One operation a connection asks for, with what bounds are judged on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
	Info,
	Random { count: u32 },
	PcrRead { pcr: u32 },
	PcrExtend { pcr: u32, digest: Vec<u8> },
	Seal { pcr: u32, secret: Vec<u8> },
	Unseal { sealed: Vec<u8> },
	Quote { pcr: u32, nonce: Vec<u8> },
}

/// A call refused before or instead of the TPM's answer, by the name the contract gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
	NotGranted,
	PcrNotAllowed,
	Bounds,
	Busy,
	PolicyRefused,
	OwnerHierarchyUnavailable,
	OtherComponent,
	Interrupted,
	Unavailable,
}

/// PCRs 0-23 exist.
pub const PCRS: u32 = 24;
/// The PCRs an application may extend: the PC Client profile's debug PCR and its application PCR, both
/// extendable at locality 0. 0-7 are the firmware's, 8-15 this system's own measured boot's, 17-22 the dynamic
/// root of trust's.
pub const EXTENDABLE: [u32; 2] = [16, 23];
/// The bounds the contract states.
pub const MAX_RANDOM: u32 = 1024;
pub const MAX_NONCE: usize = 64;
pub const MAX_SEALED: usize = 1024;
/// The tag every sealed secret begins with, and what is left of the library's 128 bytes for the application.
pub const TAG_LEN: usize = 32;
pub const MAX_LIBRARY_SECRET: usize = 128;
pub const MAX_SECRET: usize = MAX_LIBRARY_SECRET - TAG_LEN;
/// Calls waiting in all, past the one at the driver.
pub const MAX_WAITING: usize = 16;

/// Whether `grant` carries `op`. `info` is every grant's: it is what a connection is about.
pub fn carries(grant: Grant, op: &Op) -> bool {
	match op {
		Op::Info => true,
		Op::Random { .. } | Op::PcrRead { .. } | Op::Quote { .. } => grant == Grant::Tpm,
		Op::PcrExtend { .. } => grant == Grant::Measure,
		Op::Seal { .. } | Op::Unseal { .. } => grant == Grant::Seal,
	}
}

/// Whether `op` needs the owner hierarchy: seal, unseal and quote each begin with a primary key made under it.
pub fn needs_owner(op: &Op) -> bool {
	matches!(op, Op::Seal { .. } | Op::Unseal { .. } | Op::Quote { .. })
}

/// EVERY CHECK A CALL PASSES BEFORE IT IS QUEUED, in the order a caller would want to be told: the grant, the
/// PCR, the bounds, and the owner hierarchy - which is asked last, so a call that could never run on any TPM is
/// told why before it is told about this one.
pub fn admit(grant: Grant, op: &Op, owner_usable: bool) -> Result<(), Refusal> {
	if !carries(grant, op) {
		return Err(Refusal::NotGranted);
	}
	match op {
		Op::Info => {}
		Op::Random { count } => {
			if *count == 0 || *count > MAX_RANDOM {
				return Err(Refusal::Bounds);
			}
		}
		Op::PcrRead { pcr } => pcr_exists(*pcr)?,
		Op::PcrExtend { pcr, digest } => {
			if !EXTENDABLE.contains(pcr) {
				return Err(Refusal::PcrNotAllowed);
			}
			if digest.len() != TAG_LEN {
				return Err(Refusal::Bounds);
			}
		}
		Op::Seal { pcr, secret } => {
			pcr_exists(*pcr)?;
			if secret.is_empty() || secret.len() > MAX_SECRET {
				return Err(Refusal::Bounds);
			}
		}
		Op::Unseal { sealed } => {
			if sealed.is_empty() || sealed.len() > MAX_SEALED {
				return Err(Refusal::Bounds);
			}
		}
		Op::Quote { pcr, nonce } => {
			pcr_exists(*pcr)?;
			if nonce.len() > MAX_NONCE {
				return Err(Refusal::Bounds);
			}
		}
	}
	if needs_owner(op) && !owner_usable {
		return Err(Refusal::OwnerHierarchyUnavailable);
	}
	Ok(())
}

fn pcr_exists(pcr: u32) -> Result<(), Refusal> {
	if pcr < PCRS { Ok(()) } else { Err(Refusal::PcrNotAllowed) }
}

/// The SHA-256 of `bytes` - the digest an application extends a PCR with when it measures a text.
pub fn sha256(bytes: &[u8]) -> [u8; TAG_LEN] {
	crate::sha256::digest(bytes)
}

/// THE COMPONENT TAG: the SHA-256 of the sealing component's name as its grant was minted for it. The TPM binds
/// a sealed object to its owner seed and a PCR's value and nothing more - the storage primary needs no
/// authorization, so nothing in the TPM tells one application from another. This does.
pub fn tag(component: &[u8]) -> [u8; TAG_LEN] {
	crate::sha256::digest(component)
}

/// What the TPM seals for `component`: its tag, then the secret.
pub fn tagged(component: &[u8], secret: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(TAG_LEN + secret.len());
	out.extend_from_slice(&tag(component));
	out.extend_from_slice(secret);
	out
}

/// What an unseal gives `component`: the secret after its tag - and nothing at all when the tag is another
/// component's, or when what the TPM held is too short to carry one.
pub fn untagged(component: &[u8], unsealed: &[u8]) -> Result<Vec<u8>, Refusal> {
	if unsealed.len() < TAG_LEN || unsealed[..TAG_LEN] != tag(component) {
		return Err(Refusal::OtherComponent);
	}
	Ok(unsealed[TAG_LEN..].to_vec())
}

/// A call held by the queue: which connection asked, the caller's own identity for it, and what it asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending<C> {
	pub connection: u32,
	pub call: C,
	pub op: Op,
}

/// What the service does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect<C> {
	/// Send this one to the driver: it is the one outstanding there now.
	Send(Pending<C>),
	/// Answer this call with a refusal - in the record its operation answers with - and nothing reaches the
	/// driver for it.
	Refuse(Pending<C>, Refusal),
}

/// ONE REQUEST AT THE DRIVER, ONE CALL PER CONNECTION, SIXTEEN WAITING IN ALL.
pub struct Queue<C> {
	provider: bool,
	in_flight: Option<Pending<C>>,
	waiting: VecDeque<Pending<C>>,
}

impl<C: Clone> Default for Queue<C> {
	fn default() -> Self {
		Queue { provider: false, in_flight: None, waiting: VecDeque::new() }
	}
}

impl<C: Clone> Queue<C> {
	/// Whether a provider is being served.
	pub fn present(&self) -> bool {
		self.provider
	}

	/// The call outstanding at the driver.
	pub fn in_flight(&self) -> Option<&Pending<C>> {
		self.in_flight.as_ref()
	}

	pub fn waiting(&self) -> usize {
		self.waiting.len()
	}

	/// A call that passed `admit`. `unavailable` with no provider, `busy` when its connection already has one
	/// outstanding or sixteen wait; otherwise queued, and sent at once when the driver is idle.
	pub fn submit(&mut self, connection: u32, call: C, op: Op) -> Vec<Effect<C>> {
		let pending = Pending { connection, call, op };
		if !self.provider {
			return alloc::vec![Effect::Refuse(pending, Refusal::Unavailable)];
		}
		let outstanding = self.in_flight.as_ref().is_some_and(|held| held.connection == connection) || self.waiting.iter().any(|held| held.connection == connection);
		if outstanding || self.waiting.len() >= MAX_WAITING {
			return alloc::vec![Effect::Refuse(pending, Refusal::Busy)];
		}
		self.waiting.push_back(pending);
		self.next()
	}

	/// The driver answered the call outstanding there: it is handed back to be answered, and the next one sent.
	pub fn answered(&mut self) -> (Option<Pending<C>>, Vec<Effect<C>>) {
		let done = self.in_flight.take();
		(done, self.next())
	}

	fn next(&mut self) -> Vec<Effect<C>> {
		if self.in_flight.is_some() || !self.provider {
			return Vec::new();
		}
		match self.waiting.pop_front() {
			Some(pending) => {
				self.in_flight = Some(pending.clone());
				alloc::vec![Effect::Send(pending)]
			}
			None => Vec::new(),
		}
	}

	/// A provider was published.
	pub fn arrived(&mut self) {
		self.provider = true;
	}

	/// THE PROVIDER WENT AWAY: the call at the driver answers `interrupted` and is never replayed, and every call
	/// waiting answers `unavailable`.
	pub fn lost(&mut self) -> Vec<Effect<C>> {
		self.provider = false;
		let mut effects = Vec::new();
		if let Some(pending) = self.in_flight.take() {
			effects.push(Effect::Refuse(pending, Refusal::Interrupted));
		}
		for pending in self.waiting.drain(..) {
			effects.push(Effect::Refuse(pending, Refusal::Unavailable));
		}
		effects
	}

	/// A CONNECTION ENDED: its waiting call is dropped unanswered - there is nobody to answer. One at the driver
	/// stays until the driver answers it, and that answer goes nowhere.
	pub fn closed(&mut self, connection: u32) {
		self.waiting.retain(|pending| pending.connection != connection);
	}
}

#[cfg(test)]
mod tests;
