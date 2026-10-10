//! FrameLoop's private timestamp ownership, kept syscall-free for adapter regressions.
//! This records actual frame starts; it neither supplies nor estimates display timing.

use crate::Step;
use alloc::collections::TryReserveError;
use alloc::vec::Vec;

#[derive(Clone, Copy)]
struct AcquiredStart {
	addr: u64,
	started: u64,
}

pub(crate) struct FrameStarts {
	pending: Option<u64>,
	generation: u64,
	images: Vec<Option<AcquiredStart>>,
}

impl FrameStarts {
	pub(crate) fn new(images: usize, generation: u64) -> Result<Self, TryReserveError> {
		let mut starts = Self { pending: None, generation, images: Vec::new() };
		starts.reset(images, generation)?;
		Ok(starts)
	}

	pub(crate) fn reset(&mut self, images: usize, generation: u64) -> Result<(), TryReserveError> {
		self.invalidate();
		self.images.clear();
		self.images.try_reserve_exact(images)?;
		self.images.resize(images, None);
		self.generation = generation;
		Ok(())
	}

	pub(crate) fn invalidate(&mut self) {
		self.cancel_pending();
		self.images.fill(None);
	}

	pub(crate) fn cancel_pending(&mut self) {
		self.pending = None;
	}

	pub(crate) fn step(&mut self, step: Step, now: u64) {
		if step == Step::Draw {
			// Re-reading the decision before acquire does not restart work already begun.
			self.pending.get_or_insert(now);
		} else {
			self.cancel_pending();
		}
	}

	pub(crate) fn begin_acquire(&mut self) -> Option<u64> {
		// Refusal consumes this attempt too. Its work cannot migrate to a later image.
		self.pending.take()
	}

	pub(crate) fn acquired(&mut self, index: u32, addr: u64, started: Option<u64>) {
		if let Some(slot) = self.images.get_mut(index as usize) {
			*slot = started.map(|started| AcquiredStart { addr, started });
		}
	}

	pub(crate) fn take(&mut self, index: u32, addr: u64, generation: u64) -> Option<u64> {
		if generation != self.generation {
			return None;
		}
		let slot = self.images.get_mut(index as usize)?;
		if slot.as_ref()?.addr != addr {
			return None;
		}
		// Only this mapping's present/abandon consumes its timestamp. A different acquired
		// image and a not-yet-acquired Draw retain their own starts.
		slot.take().map(|entry| entry.started)
	}
}
