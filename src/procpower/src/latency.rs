//! THE LATENCY BOUND: the live `LatencyRequest`s, of which the smallest is the bound every core's idle governor keeps -
//! no state whose exit latency exceeds it is entered. A request lives as long as a handle to it does, so a holder that
//! dies releases it. Bounded: four live requests per process and sixty-four in the system. The smallest bound possible,
//! zero, means the shallowest state - the halt - and never a core that spins.

use alloc::vec::Vec;

/// At most this many live requests per process.
pub const PER_PROCESS: usize = 4;
/// At most this many in the system.
pub const IN_ALL: usize = 64;

/// Why a request was not made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Full {
	/// The process holds `PER_PROCESS` already.
	Process,
	/// The system holds `IN_ALL` already.
	System,
	/// The heap could not hold one more.
	NoMemory,
}

/// Every live request: its owner, its identity and its bound in microseconds.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Requests {
	live: Vec<(u64, u64, u32)>,
}

impl Requests {
	pub const fn new() -> Requests {
		Requests { live: Vec::new() }
	}

	/// A NEW REQUEST of `bound_us` by process `owner`, under identity `id` - refused when either bound is reached.
	pub fn add(&mut self, owner: u64, id: u64, bound_us: u32) -> Result<(), Full> {
		if self.live.len() >= IN_ALL {
			return Err(Full::System);
		}
		if self.live.iter().filter(|(held, _, _)| *held == owner).count() >= PER_PROCESS {
			return Err(Full::Process);
		}
		if self.live.try_reserve(1).is_err() {
			return Err(Full::NoMemory);
		}
		self.live.push((owner, id, bound_us));
		Ok(())
	}

	/// The request `id` ended - its last handle closed. Answers whether it was live.
	pub fn remove(&mut self, id: u64) -> bool {
		match self.live.iter().position(|(_, held, _)| *held == id) {
			Some(at) => {
				self.live.swap_remove(at);
				true
			}
			None => false,
		}
	}

	/// THE BOUND: the smallest live request's, or None.
	pub fn bound(&self) -> Option<u32> {
		self.live.iter().map(|(_, _, bound)| *bound).min()
	}

	/// How many are live.
	pub fn len(&self) -> usize {
		self.live.len()
	}

	pub fn is_empty(&self) -> bool {
		self.live.is_empty()
	}
}

#[cfg(test)]
mod tests;
