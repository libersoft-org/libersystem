//! DISPLAYSERVICE'S PROTECTED SESSION: which surface may be visible while it is held, and which comes back
//! when it ends.
//!
//! WHILE LOCKED, THE LOCK'S SURFACE IS THE ONLY ONE THAT MAY BECOME VISIBLE. A new surface is created hidden,
//! a request to make another visible is refused, and focus follows visibility - so nothing an ordinary client
//! does can replace or overpaint the protected screen. Hidden surfaces keep what they had.
//!
//! ON RELEASE THE SURFACE THAT WAS LIVE BEFORE COMES BACK, if it still exists; otherwise the console, if there
//! is one; otherwise nothing. A lock is one epoch: a release naming another epoch releases nothing.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Held {
	pub epoch: u64,
	/// The protected surface's channel.
	pub surface: u64,
	/// The surface that was live when the lock was taken.
	pub prior: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// A lock is already held.
	Held,
	/// Not the lock that is held.
	Epoch,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Lock {
	held: Option<Held>,
}

impl Lock {
	pub fn new() -> Lock {
		Lock { held: None }
	}

	pub fn held(&self) -> Option<Held> {
		self.held
	}

	pub fn take(&mut self, epoch: u64, surface: u64, active: u64) -> Result<(), Refusal> {
		if self.held.is_some() {
			return Err(Refusal::Held);
		}
		self.held = Some(Held { epoch, surface, prior: active });
		Ok(())
	}

	/// Whether `surface` may become the visible one now.
	pub fn admits(&self, surface: u64) -> bool {
		self.held.is_none_or(|held| held.surface == surface)
	}

	/// A surface went away. If it was the one to restore, there is none to restore any more.
	pub fn forget(&mut self, surface: u64) {
		if let Some(held) = self.held.as_mut()
			&& held.prior == surface
		{
			held.prior = 0;
		}
	}

	/// End the session: the surface to make visible again - the prior one, the console, or none.
	pub fn release(&mut self, epoch: u64, console: u64) -> Result<u64, Refusal> {
		let held = self.held.ok_or(Refusal::Epoch)?;
		if held.epoch != epoch {
			return Err(Refusal::Epoch);
		}
		Ok(self.end(console))
	}

	/// The session ends whatever its epoch: the display was reset, the scanout lost, or the holder went away.
	pub fn end(&mut self, console: u64) -> u64 {
		let Some(held) = self.held.take() else { return 0 };
		if held.prior != 0 { held.prior } else { console }
	}
}

#[cfg(test)]
mod tests;
