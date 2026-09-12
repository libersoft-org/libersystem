//! Ellipsis and truncation: what a line says when it does not fit.

use font_contract::Fixed266;
use font_contract::cluster::SourceRange;

use crate::compose::Line;
use crate::{Error, ParagraphDirection};

/// What a truncation did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Truncation {
	/// The source range that is still drawn. The rest is replaced by the ellipsis.
	pub kept: SourceRange,
	/// Where the ellipsis goes, along the line.
	pub ellipsis_at: Fixed266,
	/// Whether anything was removed at all.
	pub truncated: bool,
}

/// Trim a line to its box and put an ellipsis at the end of it.
///
/// THE ELLIPSIS GOES AT THE PARAGRAPH'S END, which in a right-to-left paragraph is the LEFT. A layout
/// that always put it on the right would truncate an Arabic line at its beginning and mark it at its
/// end, which is a sentence with its first word missing and a mark saying the last one is.
///
/// TRUNCATION STOPS AT A CLUSTER BOUNDARY AND NEVER INSIDE ONE. Cutting inside a cluster drops half a
/// combining sequence or half a ligature, which is not a shorter string - it is a different one.
///
/// A LINE THAT FITS IS NOT TOUCHED, and neither is one with no cluster that can be removed: an
/// ellipsis alone, on a line whose single cluster was too wide for the box, says nothing that the
/// clipped glyph does not say better.
pub fn truncate(line: &Line<'_>, ellipsis_width: Fixed266) -> Result<Truncation, Error> {
	let full = full_extent(line);
	let Some(full) = full else {
		return Ok(Truncation { kept: SourceRange { start: 0, end: 0 }, ellipsis_at: line.origin, truncated: false });
	};
	if line.width.raw() <= line.box_width.raw() {
		return Ok(Truncation { kept: full, ellipsis_at: end_edge(line), truncated: false });
	}
	// How much of the box the kept text may occupy, which is the box less the ellipsis.
	let allowance = line.box_width.raw().saturating_sub(ellipsis_width.raw());
	if allowance <= 0 {
		return Ok(Truncation { kept: SourceRange { start: full.start, end: full.start }, ellipsis_at: start_edge(line), truncated: true });
	}

	// Walk the clusters in LOGICAL order, accumulating what they occupy, and stop at the last one
	// that still fits. Walking them visually would keep whichever clusters happen to be drawn first,
	// which in mixed-direction text is not the beginning of the sentence.
	let mut kept_end = full.start;
	let mut used: i32 = 0;
	let mut any = false;
	for placed in &line.runs {
		for (index, cluster) in placed.map.clusters().iter().enumerate() {
			let width = cluster_width(placed, index)?;
			let next = used.saturating_add(width.raw());
			if next > allowance {
				return Ok(Truncation { kept: SourceRange { start: full.start, end: kept_end }, ellipsis_at: ellipsis_edge(line, Fixed266::from_raw(used), ellipsis_width)?, truncated: true });
			}
			used = next;
			kept_end = cluster.source.end;
			any = true;
		}
	}
	let _ = any;
	Ok(Truncation { kept: SourceRange { start: full.start, end: kept_end }, ellipsis_at: ellipsis_edge(line, Fixed266::from_raw(used), ellipsis_width)?, truncated: true })
}

/// Where the ellipsis sits once the kept text's width is known.
fn ellipsis_edge(line: &Line<'_>, kept_width: Fixed266, ellipsis_width: Fixed266) -> Result<Fixed266, Error> {
	Ok(match line.direction {
		ParagraphDirection::LeftToRight => line.origin.checked_add(kept_width)?,
		// The kept text is at the RIGHT of a right-to-left line, so the ellipsis is to the left of it.
		ParagraphDirection::RightToLeft => {
			let right = line.origin.checked_add(line.box_width)?;
			Fixed266::from_raw(right.raw().saturating_sub(kept_width.raw()).saturating_sub(ellipsis_width.raw()))
		}
	})
}

fn start_edge(line: &Line<'_>) -> Fixed266 {
	match line.direction {
		ParagraphDirection::LeftToRight => line.origin,
		ParagraphDirection::RightToLeft => Fixed266::from_raw(line.origin.raw().saturating_add(line.box_width.raw())),
	}
}

fn end_edge(line: &Line<'_>) -> Fixed266 {
	match line.direction {
		ParagraphDirection::LeftToRight => Fixed266::from_raw(line.origin.raw().saturating_add(line.width.raw())),
		ParagraphDirection::RightToLeft => line.origin,
	}
}

/// One cluster's own width.
fn cluster_width(placed: &crate::compose::PlacedRun<'_>, index: usize) -> Result<Fixed266, Error> {
	let Some(cluster) = placed.map.clusters().get(index) else { return Err(Error::Mismatched) };
	let mut width = Fixed266::ZERO;
	for offset in 0..cluster.glyph_count as usize {
		let at = (cluster.first_glyph as usize).checked_add(offset).ok_or(Error::Mismatched)?;
		let Some(glyph) = placed.run.glyphs.get(at) else { return Err(Error::Mismatched) };
		width = width.checked_add(glyph.x_advance)?;
	}
	Ok(width)
}

/// The whole source range this line covers.
fn full_extent(line: &Line<'_>) -> Option<SourceRange> {
	let mut extent: Option<SourceRange> = None;
	for placed in &line.runs {
		for cluster in placed.map.clusters() {
			extent = Some(match extent {
				Some(range) => SourceRange { start: range.start.min(cluster.source.start), end: range.end.max(cluster.source.end) },
				None => cluster.source,
			});
		}
	}
	extent
}
