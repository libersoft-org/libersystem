//! Where a caret lands, and which pixels a selection covers.
//!
//! THIS IS WHAT THE CLUSTER MAPPING WAS CARRIED FOR. Everything above it - the source range per
//! cluster, the intra-ligature carets, the affinity, the logical-to-visual order - exists so that
//! these two questions can be answered about a STRING rather than about a picture.
//!
//! ONE CONTIGUOUS RANGE OF TEXT IS NOT ONE RECTANGLE. In mixed-direction content a selection from the
//! middle of an Arabic word to the middle of the Latin one after it is drawn as two separate pieces
//! with a gap between them, and a layer that assumed one rectangle draws a highlight over text the
//! user did not select. That is not an edge case; it is what selecting across a direction boundary
//! always looks like.
//!
//! A CARET AT A DIRECTION BOUNDARY HAS TWO PLACES AND ONE OFFSET, which is what affinity is. The
//! caret after the last Arabic character and the caret before the following Latin one are the same
//! byte and different pixels, and the offset alone cannot say which was meant.

use alloc::vec::Vec;
use font_contract::cluster::{Caret, CaretAffinity, Cluster, SourceRange};
use font_contract::{Direction, Fixed266};

use crate::Error;
use crate::compose::{Line, PlacedRun};

/// One piece of a selection: a span of pixels along the line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
	pub left: Fixed266,
	pub right: Fixed266,
}

/// Where a caret stands on this line.
///
/// `None` when the offset is not on this line at all, and when AFFINITY says the caret belongs to the
/// other side of a line's own edge - which is how a caller laying out a paragraph finds the one line
/// that owns a caret rather than getting an answer from every line that touches its offset.
pub fn caret_position(line: &Line<'_>, caret: Caret) -> Result<Option<Fixed266>, Error> {
	let Some((first, last)) = line_extent(line) else { return Ok(None) };
	if caret.offset < first || caret.offset > last {
		return Ok(None);
	}
	// AT THE LINE'S OWN EDGES THE AFFINITY DECIDES WHETHER THIS LINE OWNS THE CARET. A caret before
	// the line's first character belongs to what came before it; one after its last belongs to what
	// comes after. Answering from both lines is how a caret is drawn twice.
	if caret.offset == first && caret.affinity == CaretAffinity::Trailing && first != 0 {
		return Ok(None);
	}
	if caret.offset == last && caret.affinity == CaretAffinity::Leading {
		return Ok(None);
	}

	for placed in &line.runs {
		let Some(position) = caret_in_run(placed, caret)? else { continue };
		return Ok(Some(line.origin.checked_add(position)?));
	}
	Ok(None)
}

/// The pieces of pixels a range of source bytes covers on this line.
///
/// SEVERAL, IN VISUAL ORDER, MERGED WHERE THEY TOUCH. Merging matters as much as splitting: a
/// selection over ten Latin clusters is ONE rectangle, and drawing ten adjacent ones leaves a seam
/// at every boundary on a surface that blends.
pub fn selection_pieces(line: &Line<'_>, range: SourceRange) -> Result<Vec<Selection>, Error> {
	let mut pieces: Vec<Selection> = Vec::new();
	for placed in &line.runs {
		for (index, cluster) in placed.map.clusters().iter().enumerate() {
			if cluster.source.end <= range.start || cluster.source.start >= range.end {
				continue;
			}
			let (low, high) = cluster_extent(placed, index)?;
			let piece = Selection { left: line.origin.checked_add(low)?, right: line.origin.checked_add(high)? };
			if piece.left == piece.right {
				// A CLUSTER THAT OCCUPIES NO PIXELS IS NOT A PIECE. An absorbed ligature component
				// has its own range and no width of its own; the ligature that swallowed it is
				// already covering those pixels, and adding a zero-width piece would leave a seam.
				continue;
			}
			pieces.push(piece);
		}
	}
	pieces.sort_by_key(|piece| piece.left.raw());
	let mut merged: Vec<Selection> = Vec::new();
	merged.try_reserve_exact(pieces.len()).map_err(|_| Error::Allocation)?;
	for piece in pieces {
		match merged.last_mut() {
			Some(last) if last.right.raw() >= piece.left.raw() => {
				if piece.right.raw() > last.right.raw() {
					last.right = piece.right;
				}
			}
			_ => merged.push(piece),
		}
	}
	Ok(merged)
}

/// The first and last source offset this line covers.
fn line_extent(line: &Line<'_>) -> Option<(u32, u32)> {
	let mut extent: Option<(u32, u32)> = None;
	for placed in &line.runs {
		for cluster in placed.map.clusters() {
			extent = Some(match extent {
				Some((first, last)) => (first.min(cluster.source.start), last.max(cluster.source.end)),
				None => (cluster.source.start, cluster.source.end),
			});
		}
	}
	extent
}

/// Where a caret stands within one run, or `None` when the run does not contain it.
fn caret_in_run(placed: &PlacedRun<'_>, caret: Caret) -> Result<Option<Fixed266>, Error> {
	let clusters = placed.map.clusters();
	let right_to_left = matches!(placed.run.direction, Direction::RightToLeft | Direction::BottomToTop);
	for (index, cluster) in clusters.iter().enumerate() {
		if caret.offset < cluster.source.start || caret.offset > cluster.source.end {
			continue;
		}
		// AT A CLUSTER'S TRAILING EDGE THE NEXT CLUSTER'S LEADING EDGE IS THE SAME PLACE, so only one
		// of the two answers - the leading one - unless this is the run's last cluster.
		if caret.offset == cluster.source.end && index + 1 < clusters.len() {
			continue;
		}
		let (low, high) = cluster_extent(placed, index)?;
		// A CLUSTER THAT OWNS NO GLYPH WAS SWALLOWED BY A LIGATURE, and the pixels it is measured
		// within belong to the glyph that swallowed it. Its own extent is a zero-width point at that
		// glyph's left edge, so every answer here is taken from the OWNER.
		if cluster.glyph_count == 0 {
			let owner = owner_of(placed, index);
			let (owner_low, owner_high) = cluster_extent(placed, owner)?;
			if caret.offset < cluster.source.end {
				// Where this component BEGINS inside the ligature: the face's own divider, or the
				// ligature's leading edge when the face declared none.
				return Ok(Some(ligature_divider(placed, index)?.unwrap_or(if right_to_left { owner_high } else { owner_low })));
			}
			// The trailing edge of the LAST component is the whole ligature's trailing edge; an
			// earlier component's trailing edge is the next one's divider, and the loop above has
			// already moved on to that cluster.
			return Ok(Some(if right_to_left { owner_low } else { owner_high }));
		}
		// INSIDE AN ORDINARY CLUSTER THERE IS NO CARET STOP - a caret does not stand between a letter
		// and the accent on it - so the cluster's own leading edge is the honest answer.
		if caret.offset > cluster.source.start && caret.offset < cluster.source.end {
			return Ok(Some(if right_to_left { high } else { low }));
		}
		let leading = caret.offset == cluster.source.start;
		return Ok(Some(match (leading, right_to_left) {
			(true, false) | (false, true) => low,
			(true, true) | (false, false) => high,
		}));
	}
	Ok(None)
}

/// Which cluster owns the glyph a cluster sits inside: itself, or the ligature that swallowed it.
fn owner_of(placed: &PlacedRun<'_>, index: usize) -> usize {
	let clusters = placed.map.clusters();
	let Some(cluster) = clusters.get(index) else { return index };
	let mut owner = index;
	while owner > 0 {
		let previous: &Cluster = &clusters[owner - 1];
		if previous.first_glyph != cluster.first_glyph {
			break;
		}
		owner -= 1;
	}
	owner
}

/// The left and right edge of one cluster's own pixels, within its line.
fn cluster_extent(placed: &PlacedRun<'_>, index: usize) -> Result<(Fixed266, Fixed266), Error> {
	let Some(cluster) = placed.map.clusters().get(index) else { return Err(Error::Mismatched) };
	let mut before = Fixed266::ZERO;
	for glyph in placed.run.glyphs.iter().take(cluster.first_glyph as usize) {
		before = before.checked_add(glyph.x_advance)?;
	}
	let mut width = Fixed266::ZERO;
	for offset in 0..cluster.glyph_count as usize {
		let at = (cluster.first_glyph as usize).checked_add(offset).ok_or(Error::Mismatched)?;
		let Some(glyph) = placed.run.glyphs.get(at) else { return Err(Error::Mismatched) };
		width = width.checked_add(glyph.x_advance)?;
	}
	let low = placed.x.checked_add(before)?;
	Ok((low, low.checked_add(width)?))
}

/// Where the divider before an ABSORBED cluster sits, out of the owning ligature's caret list.
///
/// A LIGATURE IS ONE GLYPH FOR SEVERAL CHARACTERS, and a caret that could only stand at its two ends
/// makes the middle of a word unreachable: press the arrow key and the caret jumps two characters.
/// The dividing positions are the face's own, because `ffi` is not three equal thirds.
fn ligature_divider(placed: &PlacedRun<'_>, index: usize) -> Result<Option<Fixed266>, Error> {
	let owner = owner_of(placed, index);
	let component = index - owner;
	if component == 0 {
		return Ok(None);
	}
	let dividers = placed.map.intra_ligature_carets(owner);
	let Some(divider) = dividers.get(component - 1) else { return Ok(None) };
	// THE DIVIDER IS AN X WITHIN THE GLYPH, measured from its own left edge, so it is added to that
	// edge in either direction - which is why this does not branch on the run's direction.
	let (low, _) = cluster_extent(placed, owner)?;
	Ok(Some(low.checked_add(*divider)?))
}
