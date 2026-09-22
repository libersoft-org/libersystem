//! ACL FRAGMENTS BACK INTO L2CAP PDUs, which is the first place a controller's own numbers decide
//! how much this service reads.
//!
//! An ACL packet carries a connection handle, a two-bit boundary flag and a length; the L2CAP PDU
//! inside it carries its OWN length and a channel id, and the two are independent claims about the
//! same bytes. A PDU larger than one ACL buffer arrives as a first fragment and continuations, so
//! reassembly is not optional - and reassembly is state kept on a stranger's word.
//!
//! THREE WAYS IT IS NOT A PDU, and each is a different mistake:
//!
//!   A CONTINUATION WITH NOTHING TO CONTINUE. Bytes appended to a PDU that was never started are
//!     bytes appended to whatever the buffer held, which on a reused buffer is the previous PDU.
//!   A LENGTH PAST WHAT THIS SERVICE WILL HOLD. The declared length is the device's claim and the
//!     buffer is this process's; a host that allocated to the claim would let a controller choose
//!     how much memory this service uses.
//!   MORE BYTES THAN THE PDU DECLARED. A fragment that overruns its own PDU is not a long PDU, it
//!     is a framing disagreement, and continuing past it reads the next PDU's header as payload.
//!
//! AND ONE THAT IS NOT A MISTAKE BUT A DEADLINE. A first fragment whose continuations never arrive
//! holds a buffer for ever, and a controller that sends one per connection holds all of them. An
//! incomplete PDU has a deadline and is dropped at it, which costs one PDU and not the link.

/// The L2CAP header: a two-byte length and a two-byte channel id.
pub const HEADER: usize = 4;

/// The largest SDU this profile carries. The boot mouse subset runs on the default ATT MTU of 23,
/// so this is headroom by a factor of twenty and not a number anything here needs.
pub const MAX_SDU: usize = 512;

/// The largest PDU, which is an SDU and the header in front of it.
pub const MAX_PDU: usize = MAX_SDU + HEADER;

/// How long an incomplete PDU may wait for its continuations, in the tick every deadline here is in.
/// A hundred ticks is one second on this timer.
pub const ASSEMBLY_TICKS: u64 = 100;

/// The boundary flag an ACL packet carries, as the two bits the controller sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Boundary {
	/// The start of a PDU. `01` is a continuation and `10` is a first non-flushable packet, which is
	/// what a host-to-controller start is; both `10` and `00` are read as starts here because a
	/// controller may use either and neither means "continue".
	Start,
	/// More of the PDU already begun.
	Continuation,
}

impl Boundary {
	/// What the two-bit flag names, or `None` for a value this profile does not accept. `11` is a
	/// complete automatically-flushable PDU, which no controller in this milestone's scope sends and
	/// which is refused rather than guessed at.
	pub const fn from_bits(bits: u8) -> Option<Boundary> {
		match bits & 0b11 {
			0b00 | 0b10 => Some(Boundary::Start),
			0b01 => Some(Boundary::Continuation),
			_ => None,
		}
	}
}

/// Why a fragment was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// A boundary flag this profile does not accept.
	UnknownBoundary(u8),
	/// A start fragment too short to hold the L2CAP header it must begin with.
	ShortHeader { len: usize },
	/// The declared PDU length is past what this service will hold.
	TooLong { declared: usize, bound: usize },
	/// Bytes appended to a PDU that was never started.
	NoPduInProgress,
	/// A start arrived while one was already in progress. ONE INCOMPLETE PDU PER LINK is the rule,
	/// and a second start is a controller that abandoned the first without saying so.
	AlreadyInProgress,
	/// A fragment carrying more bytes than the PDU it belongs to has left.
	Overrun { have: usize, want: usize },
	/// A fragment with no bytes at all, which advances nothing and would let a controller hold a
	/// deadline open for ever by sending them.
	Empty,
}

/// What a fragment did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fed {
	/// The PDU is still incomplete; this many bytes are still owed.
	More { owed: usize },
	/// The PDU is complete. `cid` is its channel and `len` is the payload's length, which is the
	/// declared length - the header is not part of it.
	Complete { cid: u16, len: usize },
}

/// One link's reassembly: at most one incomplete PDU, and the deadline it is held under.
#[derive(Clone, Copy)]
pub struct Reassembly {
	buffer: [u8; MAX_PDU],
	/// How much of the PDU has arrived, header included. Zero means nothing is in progress.
	have: usize,
	/// The whole PDU's length, header included, from its own declared length.
	want: usize,
	cid: u16,
	/// When the incomplete PDU must be given up, meaningful only while one is in progress.
	deadline: u64,
}

impl Default for Reassembly {
	fn default() -> Self {
		Self::new()
	}
}

impl Reassembly {
	pub const fn new() -> Reassembly {
		Reassembly { buffer: [0; MAX_PDU], have: 0, want: 0, cid: 0, deadline: 0 }
	}

	/// Whether a PDU is part-way through.
	pub const fn in_progress(&self) -> bool {
		self.have > 0
	}

	/// The completed PDU's payload, valid only after `Fed::Complete` and before the next fragment.
	pub fn payload(&self) -> &[u8] {
		&self.buffer[HEADER..self.want]
	}

	/// Give up whatever is in progress.
	pub fn clear(&mut self) {
		self.have = 0;
		self.want = 0;
		self.cid = 0;
		self.deadline = 0;
	}

	/// Drop an incomplete PDU whose continuations never came. Answers whether one was dropped, so a
	/// caller can say so once rather than discovering it as a PDU that never arrives.
	///
	/// AT THE DEADLINE AND NOT PAST IT: `now >= deadline` rather than `>`, because a deadline that
	/// has arrived has arrived, and the one-tick difference is the difference between a bound and a
	/// bound plus whatever the loop's period happens to be.
	pub fn expire(&mut self, now: u64) -> bool {
		if self.in_progress() && now >= self.deadline {
			self.clear();
			return true;
		}
		false
	}

	/// Feed one ACL fragment's L2CAP bytes.
	///
	/// `now` is the clock the deadline is taken from, read once by the caller so that every fragment
	/// in one pass is judged against one moment.
	pub fn feed(&mut self, boundary: u8, bytes: &[u8], now: u64) -> Result<Fed, Refusal> {
		let Some(boundary) = Boundary::from_bits(boundary) else { return Err(Refusal::UnknownBoundary(boundary)) };
		if bytes.is_empty() {
			return Err(Refusal::Empty);
		}
		match boundary {
			Boundary::Start => {
				if self.in_progress() {
					return Err(Refusal::AlreadyInProgress);
				}
				if bytes.len() < HEADER {
					return Err(Refusal::ShortHeader { len: bytes.len() });
				}
				let declared = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
				// THE DECLARED LENGTH IS CHECKED BEFORE ANY BYTE IS COPIED. A claim past the buffer
				// is refused on its own terms rather than becoming a copy that has to be bounded.
				if declared > MAX_SDU {
					return Err(Refusal::TooLong { declared, bound: MAX_SDU });
				}
				let want = declared + HEADER;
				if bytes.len() > want {
					return Err(Refusal::Overrun { have: bytes.len(), want });
				}
				self.cid = u16::from_le_bytes([bytes[2], bytes[3]]);
				self.want = want;
				self.buffer[..bytes.len()].copy_from_slice(bytes);
				self.have = bytes.len();
				self.deadline = now.saturating_add(ASSEMBLY_TICKS);
			}
			Boundary::Continuation => {
				if !self.in_progress() {
					return Err(Refusal::NoPduInProgress);
				}
				let room = self.want - self.have;
				if bytes.len() > room {
					// THE PDU IS GIVEN UP RATHER THAN TRUNCATED. A fragment that overruns its own
					// PDU is a framing disagreement, and keeping what arrived before it would hand
					// the layer above a PDU assembled out of two different framings.
					self.clear();
					return Err(Refusal::Overrun { have: bytes.len(), want: room });
				}
				self.buffer[self.have..self.have + bytes.len()].copy_from_slice(bytes);
				self.have += bytes.len();
			}
		}
		if self.have == self.want {
			let (cid, len) = (self.cid, self.want - HEADER);
			// `have` goes to zero so the next fragment must be a start, but `want` and the buffer
			// stay: `payload` is read after this returns.
			self.have = 0;
			self.deadline = 0;
			return Ok(Fed::Complete { cid, len });
		}
		Ok(Fed::More { owed: self.want - self.have })
	}
}

#[cfg(test)]
mod tests;
