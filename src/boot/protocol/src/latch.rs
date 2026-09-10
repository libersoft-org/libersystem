// One value, latched by the first source that supplies it and required of every later one.
//
// THREE FACTS ARE LATCHED OVER ONE BOOT'S MANIFESTS - the release string, the security generation
// and the purpose - and a set that agrees on one while mixing another is a system nobody built. The
// generation and the purpose share this mechanism so the two cannot drift; the release latch keeps
// a bounded byte copy of its own in the loader, which is what a `Copy` value here cannot hold.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Latch<T: Copy + PartialEq> {
	held: Option<T>,
	offered: usize,
}

// The value already held and the one that disagreed with it, so a refusal can name both.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Conflict<T> {
	pub held: T,
	pub offered: T,
}

impl<T: Copy + PartialEq> Latch<T> {
	pub const fn new() -> Self {
		Latch { held: None, offered: 0 }
	}

	// Offer one source's value: the first is latched, every later one must equal it.
	pub fn record(&mut self, value: T) -> Result<(), Conflict<T>> {
		self.offered += 1;
		match self.held {
			None => {
				self.held = Some(value);
				Ok(())
			}
			Some(held) if held == value => Ok(()),
			Some(held) => Err(Conflict { held, offered: value }),
		}
	}

	// The latched value - `None` until a source has been offered.
	pub fn value(&self) -> Option<T> {
		self.held
	}

	// How many sources were offered, agreeing or not.
	pub fn offered(&self) -> usize {
		self.offered
	}
}

impl<T: Copy + PartialEq> Default for Latch<T> {
	fn default() -> Self {
		Latch::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_first_value_is_latched_and_equal_values_agree() {
		let mut latch: Latch<u64> = Latch::new();
		assert_eq!(latch.value(), None);
		assert_eq!(latch.record(7), Ok(()));
		assert_eq!(latch.record(7), Ok(()));
		assert_eq!(latch.value(), Some(7));
		assert_eq!(latch.offered(), 2);
	}

	#[test]
	fn a_different_value_is_a_conflict_that_names_both_and_leaves_the_first_held() {
		let mut latch: Latch<u32> = Latch::new();
		assert_eq!(latch.record(1), Ok(()));
		assert_eq!(latch.record(2), Err(Conflict { held: 1, offered: 2 }));
		assert_eq!(latch.value(), Some(1), "a conflict does not replace what was latched");
		assert_eq!(latch.offered(), 2, "the disagreeing source still counts as offered");
	}
}
