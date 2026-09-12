//! Tab stops: the one advance that is not a glyph's own.
//!
//! A TAB IS NOT A WIDE SPACE. Its width depends on where the pen already is, which makes it the only
//! thing on a line whose advance is not a property of the font - and a layout that gave it a fixed
//! advance produces columns that do not line up, which is the entire reason anybody typed a tab.
//!
//! MEASURED FROM THE LINE'S OWN START. In a right-to-left paragraph the stops run from the right, so
//! a table of Arabic text columnises the way its reader expects rather than mirrored.

use font_contract::Fixed266;

use crate::Error;

/// Where the tab stops are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TabStops<'a> {
	/// The stated stops, in increasing distance from the line's start.
	explicit: &'a [Fixed266],
	/// What continues past the last stated one. A default interval is not a fallback, it is what a
	/// document with no stop list means.
	interval: Fixed266,
}

impl<'a> TabStops<'a> {
	/// Stops every `interval` from the line's start.
	pub fn every(interval: Fixed266) -> Self {
		Self { explicit: &[], interval }
	}

	/// Stated stops, with an interval continuing past the last of them.
	pub fn stated(explicit: &'a [Fixed266], interval: Fixed266) -> Self {
		Self { explicit, interval }
	}

	/// How far a tab at `from` advances, measured from the line's start.
	///
	/// STRICTLY PAST THE PEN, and that is the rule that matters: a tab that landed ON the current
	/// position would advance by nothing, so two tabs in a row would be one - and a document written
	/// with two tabs would columnise as though it had one.
	///
	/// AN INTERVAL OF ZERO ADVANCES BY NOTHING rather than looping: a zero interval is a document
	/// that states no stops at all, and a layer that searched for the next one would not return.
	pub fn advance_from(&self, from: Fixed266) -> Result<Fixed266, Error> {
		for stop in self.explicit {
			if stop.raw() > from.raw() {
				return Ok(Fixed266::from_raw(stop.raw() - from.raw()));
			}
		}
		if self.interval.raw() <= 0 {
			return Ok(Fixed266::ZERO);
		}
		// Past the stated stops the interval continues, counted from the LINE'S start rather than
		// from the last stop - so a stated stop that is not a multiple of the interval does not shift
		// every column after it.
		let last = self.explicit.last().map(|stop| stop.raw()).unwrap_or(0).max(0);
		let beyond = from.raw().max(last);
		let steps = (beyond - last) / self.interval.raw() + 1;
		let next = last.checked_add(steps.checked_mul(self.interval.raw()).ok_or(Error::Overflow(font_contract::Overflow::Sum))?).ok_or(Error::Overflow(font_contract::Overflow::Sum))?;
		Ok(Fixed266::from_raw(next - from.raw()))
	}
}
