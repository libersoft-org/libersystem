//! A bounded set of provider openings, advanced by replies rather than by blocking the service loop.
//!
//! The catalogue hands back a request channel, then the provider hands back its update stream. Each
//! stage gets its own deadline. Correlations are never reused: a late catalogue reply cannot become a
//! new publication's channel. Retiring an opening returns it to the caller, which closes any channel
//! it already owns and decides whether the existing provider retry policy should ask again.

use alloc::vec::Vec;

pub struct Pending<T> {
	pub item: T,
	pub attempts: u32,
	pub corr: u32,
	pub deadline: u64,
	pub timeout: u64,
	channel: Option<u64>,
}

impl<T> Pending<T> {
	pub fn channel(&self) -> Option<u64> {
		self.channel
	}
}

pub struct Opens<T> {
	pending: Vec<Pending<T>>,
	limit: usize,
	next_corr: u32,
}

impl<T> Opens<T> {
	pub const fn new(limit: usize) -> Self {
		Self { pending: Vec::new(), limit, next_corr: 1 }
	}

	/// Reserve before sending to the catalogue. None means capacity or correlation space exhausted.
	pub fn begin(&mut self, item: T, attempts: u32, now: u64, timeout: u64) -> Option<u32> {
		if self.pending.len() >= self.limit || self.next_corr == 0 {
			return None;
		}
		let corr = self.next_corr;
		self.next_corr = corr.checked_add(1).unwrap_or(0);
		self.pending.push(Pending { item, attempts, corr, deadline: now.saturating_add(timeout), timeout, channel: None });
		Some(corr)
	}

	pub fn take_catalogue(&mut self, corr: u32) -> Option<Pending<T>> {
		let at = self.pending.iter().position(|pending| pending.channel.is_none() && pending.corr == corr)?;
		Some(self.pending.remove(at))
	}

	pub fn take_stream(&mut self, channel: u64, corr: u32) -> Option<Pending<T>> {
		let at = self.pending.iter().position(|pending| pending.channel == Some(channel) && pending.corr == corr)?;
		Some(self.pending.remove(at))
	}

	/// The private provider channel closed before answering, so there is no reply correlation.
	pub fn take_channel(&mut self, channel: u64) -> Option<Pending<T>> {
		let at = self.pending.iter().position(|pending| pending.channel == Some(channel))?;
		Some(self.pending.remove(at))
	}

	/// Reinsert the catalogue opening just taken, now waiting on its own provider channel. No other
	/// admission intervenes in the serialized caller; the slot removed above is still reserved.
	pub fn await_stream(&mut self, mut pending: Pending<T>, channel: u64, now: u64) {
		assert!(self.pending.len() < self.limit);
		pending.channel = Some(channel);
		pending.deadline = now.saturating_add(pending.timeout);
		self.pending.push(pending);
	}

	pub fn channels(&self) -> impl Iterator<Item = u64> + '_ {
		self.pending.iter().filter_map(Pending::channel)
	}

	pub fn items(&self) -> impl Iterator<Item = &T> + '_ {
		self.pending.iter().map(|pending| &pending.item)
	}

	pub fn contains(&self, found: impl Fn(&T) -> bool) -> bool {
		self.pending.iter().any(|pending| found(&pending.item))
	}

	pub fn len(&self) -> usize {
		self.pending.len()
	}

	pub fn next_deadline(&self) -> Option<u64> {
		self.pending.iter().map(|pending| pending.deadline).min()
	}

	pub fn expire(&mut self, now: u64) -> Vec<Pending<T>> {
		self.remove_where(|pending| now >= pending.deadline)
	}

	pub fn cancel(&mut self, withdrawn: impl Fn(&T) -> bool) -> Vec<Pending<T>> {
		self.remove_where(|pending| withdrawn(&pending.item))
	}

	fn remove_where(&mut self, remove: impl Fn(&Pending<T>) -> bool) -> Vec<Pending<T>> {
		let mut taken = Vec::new();
		let mut at = 0;
		while at < self.pending.len() {
			if remove(&self.pending[at]) {
				taken.push(self.pending.remove(at));
			} else {
				at += 1;
			}
		}
		taken
	}
}

#[cfg(test)]
mod tests;
