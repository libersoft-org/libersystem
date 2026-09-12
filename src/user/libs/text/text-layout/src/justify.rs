//! Filling a line to its box, and what to do when it cannot be.

use font_contract::Fixed266;

use crate::Error;
use crate::compose::Line;

/// One point on a line that may absorb justification space.
///
/// WHICH POINTS THESE ARE IS A PROPERTY OF THE TEXT and belongs to the layer that has the text: an
/// inter-word space, a Thai word boundary, an Arabic kashida position and a CJK inter-character gap
/// are four different answers in four different scripts, and none of them is visible from a run of
/// glyph advances. HOW MUCH each one gets is a property of the LAYOUT and is decided here. Splitting
/// it the other way is how two callers justify the same paragraph differently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Elastic {
	/// Which run of the line.
	pub run: usize,
	/// Which glyph of that run absorbs the space. The space is added to that glyph's ADVANCE, which
	/// is what moves everything after it along.
	pub glyph: u16,
	/// How willing this point is, relative to the others. A point with weight zero is named and not
	/// used, which is how a caller can list every candidate and let the policy decide.
	pub weight: u16,
}

/// What a justification did, so a caller can tell "filled" from "left alone".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Justification {
	/// The line was stretched to its box.
	Filled,
	/// There was nothing to stretch, so the line was left at its natural width.
	///
	/// NOT LETTER-SPACED. Spreading the slack between every pair of glyphs is what a layout does when
	/// it refuses to admit it cannot justify a line, and it is worse than a ragged edge: it changes
	/// the rhythm of the word itself, and on a line with one long word it is unreadable.
	NoOpportunity,
	/// The line is already at or past its box, so there was no slack to distribute.
	AlreadyFull,
}

/// Distribute a line's slack over its elastic points.
///
/// THE LAST LINE OF A PARAGRAPH IS NOT JUSTIFIED, which is why this takes the decision as an argument
/// rather than inferring it: a layer that stretched every line would spread the four words of a final
/// line across the whole measure, and that is the single most recognisable sign of a layout engine
/// that was never read by a typographer. The caller knows which line is last; this one does not.
///
/// THE REMAINDER GOES TO THE FIRST POINTS, one 26.6 unit each, rather than being dropped. Dropping it
/// leaves the line a few sixty-fourths short of its box, which shows as a ragged right edge on
/// justified text - the exact thing justification was asked for.
pub fn justify(line: &mut Line<'_>, elastic: &[Elastic], is_last_line: bool) -> Result<Justification, Error> {
	if is_last_line {
		return Ok(Justification::NoOpportunity);
	}
	let slack = line.slack();
	if slack.raw() <= 0 {
		return Ok(Justification::AlreadyFull);
	}
	let mut total_weight: u64 = 0;
	for point in elastic {
		total_weight += point.weight as u64;
	}
	if total_weight == 0 {
		return Ok(Justification::NoOpportunity);
	}
	// Every point's share, and then the remainder one unit at a time so the line reaches its box
	// exactly rather than nearly.
	let slack_units = slack.raw() as i64;
	let mut given: i64 = 0;
	let mut shares = alloc::vec::Vec::new();
	shares.try_reserve_exact(elastic.len()).map_err(|_| Error::Allocation)?;
	for point in elastic {
		let share = slack_units * point.weight as i64 / total_weight as i64;
		given += share;
		shares.push(share);
	}
	let mut remainder = slack_units - given;
	for (index, point) in elastic.iter().enumerate() {
		if remainder == 0 {
			break;
		}
		if point.weight == 0 {
			continue;
		}
		shares[index] += 1;
		remainder -= 1;
	}

	// APPLIED TO THE ADVANCE, NOT TO A POSITION. A justification that moved the glyphs after each
	// space would have to move them again for every later space, and the two would disagree by a
	// rounding unit; widening the space's own advance is one change that everything after it
	// inherits.
	let mut growth = Fixed266::ZERO;
	for (point, share) in elastic.iter().zip(shares.iter()) {
		let Some(run) = line.runs.get_mut(point.run) else { return Err(Error::Mismatched) };
		let Some(glyph) = run.run.glyphs.get(point.glyph as usize) else { return Err(Error::Mismatched) };
		let _ = glyph;
		run.width = run.width.checked_add(Fixed266::from_raw(*share as i32))?;
		growth = growth.checked_add(Fixed266::from_raw(*share as i32))?;
	}
	// The runs after a widened one start further along, which is the whole effect of the widening.
	let mut pen = line.runs.first().map(|run| run.x).unwrap_or(Fixed266::ZERO);
	for run in line.runs.iter_mut() {
		run.x = pen;
		pen = pen.checked_add(run.width)?;
	}
	line.width = line.width.checked_add(growth)?;
	Ok(Justification::Filled)
}
