//! TYPECSERVICE'S OPERATOR REQUESTS: at most one outstanding per connector, each completed exactly once - by the
//! provider's answer, or as INDETERMINATE when the deadline passes or the provider goes first.
//!
//! THE DEADLINE IS FIFTEEN SECONDS: UCSI's ten-second command bound and five for the connector change that reports
//! a swap's result. An indeterminate request is never retried - the platform may have acted - and a late answer to
//! one is dropped, since its operator has had an answer already.
//!
//! Connectors and providers are named as PowerService's registry names its sources and providers, which is what
//! TypeCService holds its connectors in.

use alloc::vec::Vec;

use crate::power_registry::{ProviderId, SourceKey};

/// Fifteen seconds of the system's 100 Hz ticks.
pub const REQUEST_TICKS: u64 = 1500;
/// The most requests outstanding across every connector the service holds.
pub const MAX_OUTSTANDING: usize = 32;

/// Why a request was not started.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// One is already outstanding on this connector.
	Busy,
	/// The service's bound on outstanding requests is reached.
	Exhausted,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Outstanding {
	corr: u32,
	key: SourceKey,
	provider: ProviderId,
	deadline: u64,
}

#[derive(Default)]
pub struct Requests {
	outstanding: Vec<Outstanding>,
	next_corr: u32,
}

impl Requests {
	pub fn new() -> Self {
		Self { outstanding: Vec::new(), next_corr: 1 }
	}

	/// A request for `key` through its provider: the correlation it is sent under.
	pub fn start(&mut self, key: SourceKey, provider: ProviderId, now: u64) -> Result<u32, Refusal> {
		if self.outstanding.iter().any(|held| held.key == key) {
			return Err(Refusal::Busy);
		}
		if self.outstanding.len() >= MAX_OUTSTANDING {
			return Err(Refusal::Exhausted);
		}
		let corr = self.next_corr;
		self.next_corr = self.next_corr.wrapping_add(1).max(1);
		self.outstanding.push(Outstanding { corr, key, provider, deadline: now + REQUEST_TICKS });
		Ok(corr)
	}

	/// The request sent under `corr` was not sent after all: released, and never answered as anything.
	pub fn abandon(&mut self, corr: u32) {
		self.outstanding.retain(|held| held.corr != corr);
	}

	/// The provider answered `corr`. TRUE ONCE, for the request still outstanding; a late or foreign answer is
	/// false, and dropped.
	pub fn answered(&mut self, provider: ProviderId, corr: u32) -> bool {
		match self.outstanding.iter().position(|held| held.corr == corr && held.provider == provider) {
			Some(at) => {
				self.outstanding.remove(at);
				true
			}
			None => false,
		}
	}

	/// The provider went: every request outstanding on it completes as indeterminate.
	pub fn provider_gone(&mut self, provider: ProviderId) -> Vec<u32> {
		let gone: Vec<u32> = self.outstanding.iter().filter(|held| held.provider == provider).map(|held| held.corr).collect();
		self.outstanding.retain(|held| held.provider != provider);
		gone
	}

	/// Time passed: every request past its deadline completes as indeterminate.
	pub fn tick(&mut self, now: u64) -> Vec<u32> {
		let expired: Vec<u32> = self.outstanding.iter().filter(|held| now >= held.deadline).map(|held| held.corr).collect();
		self.outstanding.retain(|held| now < held.deadline);
		expired
	}

	pub fn next_deadline(&self) -> Option<u64> {
		self.outstanding.iter().map(|held| held.deadline).min()
	}
}

#[cfg(test)]
mod tests;
