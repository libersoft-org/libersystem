//! Putting the runs of one line where they go.

use alloc::vec::Vec;
use font_contract::{ClusterMap, Fixed266, GlyphRun};

use crate::{Error, ParagraphDirection};

/// One run of a line, with the mapping that addresses it.
///
/// THE TWO TRAVEL TOGETHER because neither answers anything alone: the run says where the pixels are
/// and the map says which characters they came from, and a caret needs both in the same breath.
#[derive(Clone, Copy)]
pub struct PlacedRun<'a> {
	pub run: GlyphRun<'a>,
	pub map: ClusterMap<'a>,
	/// Where this run's LEFT edge sits along the line, whatever direction the run is.
	///
	/// LEFT AND NOT "START", and that is deliberate: this is the one place in the stack where a
	/// number is about pixels rather than about text, because it is what a renderer is handed.
	pub x: Fixed266,
	/// The run's own extent, measured once here rather than by every consumer that asks.
	pub width: Fixed266,
}

/// A line, ready to be drawn and ready to be asked about.
#[derive(Clone)]
pub struct Line<'a> {
	pub runs: Vec<PlacedRun<'a>>,
	/// Where the line's own box begins, in the consumer's device space.
	pub origin: Fixed266,
	/// The width the line was laid out INTO, which is not the width its runs fill.
	pub box_width: Fixed266,
	/// What the runs actually occupy.
	pub width: Fixed266,
	pub direction: ParagraphDirection,
}

/// Lay the runs of one line out side by side, in the paragraph's own direction.
///
/// THE RUNS ARRIVE IN VISUAL ORDER, which is what the bidi reordering above this produced: rule L2
/// decides which run is drawn first, and this places them in that order. Reordering here as well
/// would apply L2 twice, which is the identity for one level and wrong for three.
///
/// A RIGHT-TO-LEFT PARAGRAPH IS FILLED FROM THE RIGHT. The runs keep the order they were given - that
/// order already IS the visual one - and it is the starting pen that moves to the other end, because
/// the first thing drawn in such a paragraph is the rightmost.
pub fn compose<'a>(runs: &[(GlyphRun<'a>, ClusterMap<'a>)], box_width: Fixed266, direction: ParagraphDirection) -> Result<Line<'a>, Error> {
	let mut widths = Vec::new();
	widths.try_reserve_exact(runs.len()).map_err(|_| Error::Allocation)?;
	let mut total = Fixed266::ZERO;
	for (run, map) in runs {
		// THE MAP MUST DESCRIBE THIS RUN. A cluster naming a glyph the run does not have is a
		// mapping built against something else, and every caret answered from it would be wrong in a
		// way nothing downstream could detect.
		for cluster in map.clusters() {
			let last = (cluster.first_glyph as usize).saturating_add(cluster.glyph_count as usize);
			if cluster.glyph_count > 0 && last > run.glyphs.len() {
				return Err(Error::Mismatched);
			}
		}
		let (width, _) = run.total_advance()?;
		widths.push(width);
		total = total.checked_add(width)?;
	}

	let mut placed = Vec::new();
	placed.try_reserve_exact(runs.len()).map_err(|_| Error::Allocation)?;
	let mut pen = Fixed266::ZERO;
	for ((run, map), width) in runs.iter().zip(widths.iter()) {
		placed.push(PlacedRun { run: *run, map: *map, x: pen, width: *width });
		pen = pen.checked_add(*width)?;
	}
	Ok(Line { runs: placed, origin: Fixed266::ZERO, box_width, width: total, direction })
}

impl Line<'_> {
	/// Move every run by the same amount, which is what an alignment and a justification both do to
	/// a line once they have decided how far.
	pub fn shift(&mut self, by: Fixed266) -> Result<(), Error> {
		for run in self.runs.iter_mut() {
			run.x = run.x.checked_add(by)?;
		}
		Ok(())
	}

	/// The slack: how much of the line's box the runs do not fill.
	///
	/// NEGATIVE WHEN THE LINE IS TOO LONG, and that is a real answer rather than a fault: a line with
	/// no break opportunity in it overflows its box, and a layer that clamped the slack to zero would
	/// centre such a line as though it fitted.
	pub fn slack(&self) -> Fixed266 {
		Fixed266::from_raw(self.box_width.raw().saturating_sub(self.width.raw()))
	}
}
