//! A published provider whose first request went unanswered, asked again later rather than never.
//!
//! A SERVICE THAT ADOPTS PROVIDERS opens each one's update stream with one bounded request when the catalogue
//! announces it. A provider that is busy at that moment - a driver still answering its controller at bind, on a
//! machine slow enough that one second of wall clock is little work - did not answer inside the bound, and the
//! service let it go for good: the provider stayed published and the service never held it. On riscv64 under TCG
//! TypeCService held no connector for a whole boot that way. So the failed ones are kept here and asked again, at
//! one, two, four ... seconds, a bounded number of times, and dropped at once when the catalogue withdraws them.

use alloc::vec::Vec;

/// How many times a provider is asked again after its first failure - at the base delay doubled each time, so the
/// last is asked about two minutes after the first (one, two, four ... sixty-four seconds at a one-second base).
pub const RETRIES: u32 = 7;

/// What a failure answers: when the provider will be asked again, or that it will not be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Next {
	/// Asked again at this tick.
	At(u64),
	/// Every retry is spent.
	GivenUp,
}

struct Waiting<T> {
	item: T,
	at: u64,
	attempts: u32,
}

/// The providers waiting to be asked again.
pub struct ProviderRetry<T> {
	waiting: Vec<Waiting<T>>,
	base: u64,
}

impl<T> ProviderRetry<T> {
	/// `base` is the first delay, in ticks; each later one doubles it.
	pub const fn new(base: u64) -> Self {
		ProviderRetry { waiting: Vec::new(), base }
	}

	/// `item` did not answer its opening request at `now`, after `attempts` earlier retries (0 for the first time).
	pub fn failed(&mut self, item: T, attempts: u32, now: u64) -> Next {
		if attempts >= RETRIES {
			return Next::GivenUp;
		}
		let at = now.saturating_add(self.base << attempts);
		self.waiting.push(Waiting { item, at, attempts: attempts + 1 });
		Next::At(at)
	}

	/// The earliest tick a provider is due, if any is waiting.
	pub fn next_deadline(&self) -> Option<u64> {
		self.waiting.iter().map(|waiting| waiting.at).min()
	}

	/// The providers due at `now`, each with how many retries it has had, taken out of the list.
	pub fn due(&mut self, now: u64) -> Vec<(T, u32)> {
		let mut out = Vec::new();
		let mut at = 0;
		while at < self.waiting.len() {
			if self.waiting[at].at <= now {
				let waiting = self.waiting.remove(at);
				out.push((waiting.item, waiting.attempts));
			} else {
				at += 1;
			}
		}
		out
	}

	/// Take only as much opening work as the service has concurrent slots for; unread retries keep
	/// their attempt count and original due time while another kind shares those slots.
	pub fn due_one(&mut self, now: u64) -> Option<(T, u32)> {
		let at = self.waiting.iter().position(|waiting| waiting.at <= now)?;
		let waiting = self.waiting.remove(at);
		Some((waiting.item, waiting.attempts))
	}

	/// Drop every waiting provider `gone` names - one the catalogue withdrew.
	pub fn withdraw(&mut self, gone: impl Fn(&T) -> bool) {
		self.waiting.retain(|waiting| !gone(&waiting.item));
	}

	pub fn len(&self) -> usize {
		self.waiting.len()
	}

	pub fn contains(&self, found: impl Fn(&T) -> bool) -> bool {
		self.waiting.iter().any(|waiting| found(&waiting.item))
	}

	pub fn is_empty(&self) -> bool {
		self.waiting.is_empty()
	}
}

#[cfg(test)]
mod tests;
