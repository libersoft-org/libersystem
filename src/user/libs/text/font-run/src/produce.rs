//! Turning a shaped buffer into the run and the mapping the seam defines.

use alloc::vec::Vec;
use font_contract::cluster::{Cluster, ClusterMap, SourceRange};
use font_contract::{Direction, FaceRef, Fixed266, GlyphKind, GlyphRun, KindSelection, PositionedGlyph, RasterisationMode, ScriptTag, VariationCoordinates};
use font_parse::tables::Face;
use font_shape::Buffer;

use crate::Error;
use crate::carets::{Carets, MAX_CARETS};
use crate::kind::Forms;

/// Everything about a run that is not the glyphs: what the shaper was given, and where the result is
/// to be drawn.
///
/// ONE FACE, ONE SCRIPT, ONE DIRECTION, which is the seam's own requirement rather than this
/// producer's convenience. A run that mixed any of them would put a face switch inside the structure
/// whose whole purpose is to be painted in one operation.
pub struct Request<'a> {
	/// The parsed face, for its design grid, its glyph forms and its ligature carets.
	pub face: &'a Face<'a>,
	/// Who that face IS, as the catalogue issued it - which is what a cache key is built on.
	pub identity: FaceRef,
	/// The em size, in 26.6.
	pub size: Fixed266,
	/// The instance the run was shaped for.
	pub variation: VariationCoordinates,
	pub script: ScriptTag,
	pub direction: Direction,
	pub mode: RasterisationMode,
	/// The baseline origin in the consumer's device space.
	pub origin: (Fixed266, Fixed266),
	/// The WHOLE original string. Cluster ranges are offsets into this and not into the run's slice,
	/// because a hit test asks about the string a person typed.
	pub text: &'a str,
	/// Where this run begins in that string.
	pub start: usize,
	/// Where it ends, exclusive.
	pub end: usize,
}

/// A produced run: the storage the seam's borrowed views are taken from.
///
/// OWNED HERE AND BORROWED THERE. `GlyphRun` and `ClusterMap` are both borrowed types - the layout
/// that produced them holds the storage - so something has to be the layout, and this is it.
pub struct Run {
	face: FaceRef,
	size: Fixed266,
	variation: VariationCoordinates,
	script: ScriptTag,
	direction: Direction,
	mode: RasterisationMode,
	origin: (Fixed266, Fixed266),
	glyphs: Vec<PositionedGlyph>,
	clusters: Vec<Cluster>,
	visual_of_logical: Vec<u16>,
	carets: Vec<Fixed266>,
	source_len: u32,
}

impl Run {
	/// The run, as the renderer consumes it.
	pub fn run(&self) -> GlyphRun<'_> {
		GlyphRun { face: self.face, size: self.size, variation: self.variation, script: self.script, direction: self.direction, mode: self.mode, origin_x: self.origin.0, origin_y: self.origin.1, glyphs: &self.glyphs }
	}

	/// The mapping a caret, a selection, a hit test and an accessibility tree are built from.
	///
	/// IT GOES THROUGH THE SEAM'S OWN CONSTRUCTOR, which checks the invariants rather than trusting
	/// them - including when the thing being checked is what this crate just produced.
	pub fn cluster_map(&self) -> Option<ClusterMap<'_>> {
		ClusterMap::new(&self.clusters, &self.visual_of_logical, &self.carets, self.source_len)
	}

	/// The clusters, in logical order - the order the text was written.
	pub fn clusters(&self) -> &[Cluster] {
		&self.clusters
	}
}

/// Produce the run and its mapping from a buffer a shaper has finished with.
///
/// THE BUFFER IS IN LOGICAL ORDER AND THE RUN IS IN VISUAL ORDER. Shaping never reverses: every rule
/// in `GSUB` and `GPOS` is written about the order the text was typed, and a shaper that reversed
/// early would apply `fina` to the first letter of an Arabic word. The reversal belongs HERE, at the
/// one point where the result stops being about the string and starts being about the picture.
pub fn produce(request: &Request<'_>, buffer: &Buffer) -> Result<Run, Error> {
	if request.end < request.start || request.end > request.text.len() {
		return Err(Error::ClusterOutOfRange);
	}
	// THE PROFILE'S OUTPUT CEILING FIRST, because it is the smaller one and it is the one that says
	// WHY: a run past it is a document asking for work rather than a caller that merely wrote a long
	// line. The seam's own `u16` indices are checked below, where the clusters are counted.
	if buffer.len() > opentype_profile::limits::RUN_OUTPUT as usize {
		return Err(Error::Exceeded { limit: "run output", ceiling: opentype_profile::limits::RUN_OUTPUT, asked: buffer.len() as u64 });
	}
	let units_per_em = request.face.header.units_per_em;
	let size_pixels = u16::try_from(request.size.floor_pixels().max(0)).unwrap_or(u16::MAX);
	let forms = Forms::of(request.face)?;
	let carets = Carets::of(request.face)?;
	let reversed = matches!(request.direction, Direction::RightToLeft | Direction::BottomToTop);

	// THE GLYPHS, IN VISUAL ORDER.
	// FALLIBLE, because the size comes from input. Infallible allocation in userspace ends the
	// process when it fails, which would take a caller's whole program down over one paragraph.
	let mut glyphs = Vec::new();
	glyphs.try_reserve_exact(buffer.len()).map_err(|_| Error::Allocation)?;
	for visual in 0..buffer.len() {
		let logical = if reversed { buffer.len() - 1 - visual } else { visual };
		let info = buffer.infos[logical];
		let position = buffer.positions[logical];
		let (kind, selection) = forms.kind_of(info.glyph, size_pixels)?;
		glyphs.push(positioned(info.glyph, position, units_per_em, request.size, kind, selection)?);
	}

	// THE CLUSTERS, IN LOGICAL ORDER, over GRAPHEME boundaries.
	let boundaries = grapheme_boundaries_of(request.text, request.start, request.end);
	let mut clusters = Vec::new();
	clusters.try_reserve_exact(boundaries.len().saturating_sub(1)).map_err(|_| Error::Allocation)?;
	let mut caret_storage: Vec<Fixed266> = Vec::new();
	for pair in boundaries.windows(2) {
		let (start, end) = (pair[0], pair[1]);
		let source = SourceRange { start: u32::try_from(start).map_err(|_| Error::TooMany)?, end: u32::try_from(end).map_err(|_| Error::TooMany)? };
		// Which glyphs came from this cluster, in the buffer's LOGICAL order - which is the order the
		// visual indices below are derived from, not the order they are drawn in.
		let mut first_logical = None;
		let mut count = 0usize;
		for (logical, info) in buffer.infos.iter().enumerate() {
			let offset = info.cluster as usize;
			if offset < start || offset >= end {
				continue;
			}
			if first_logical.is_none() {
				first_logical = Some(logical);
			}
			count += 1;
		}
		let (first_glyph, glyph_count, first_caret, caret_count) = match first_logical {
			Some(logical) => {
				let visual = if reversed { buffer.len() - 1 - (logical + count - 1) } else { logical };
				// A cluster that owns a glyph may own a LIGATURE, and a ligature's dividing carets are
				// what makes the middle of a word reachable.
				let mut values = [0i32; MAX_CARETS];
				let written = carets.of_glyph(request.face, buffer.infos[logical].glyph, &mut values)?;
				let first_caret = u16::try_from(caret_storage.len()).map_err(|_| Error::TooMany)?;
				for value in values.iter().take(written) {
					caret_storage.push(Fixed266::from_font_units(*value, units_per_em, request.size)?);
				}
				(u16::try_from(visual).map_err(|_| Error::TooMany)?, u16::try_from(count).map_err(|_| Error::TooMany)?, first_caret, u16::try_from(written).map_err(|_| Error::TooMany)?)
			}
			None => {
				// A CLUSTER A LIGATURE ABSORBED. It produced no glyph of its own, and saying so with a
				// glyph count of zero is what lets a caret land inside the ligature that swallowed it:
				// the dividing positions are on the cluster that OWNS the glyph, and this one points
				// at that glyph so a hit test knows which pixels to measure within.
				let owner = clusters.last().map(|cluster: &Cluster| cluster.first_glyph).unwrap_or(0);
				(owner, 0, 0, 0)
			}
		};
		clusters.push(Cluster { source, first_glyph, glyph_count, first_caret, caret_count });
	}
	if clusters.len() > u16::MAX as usize {
		return Err(Error::TooMany);
	}

	// THE VISUAL ORDER OF THE CLUSTERS, which is the run's direction and nothing else: a run is
	// direction-homogeneous by the seam's own definition, so within one run the order reverses
	// wholesale or not at all.
	let mut visual_of_logical = Vec::new();
	visual_of_logical.try_reserve_exact(clusters.len()).map_err(|_| Error::Allocation)?;
	for logical in 0..clusters.len() {
		let visual = if reversed { clusters.len() - 1 - logical } else { logical };
		visual_of_logical.push(u16::try_from(visual).map_err(|_| Error::TooMany)?);
	}

	let run = Run { face: request.identity, size: request.size, variation: request.variation, script: request.script, direction: request.direction, mode: request.mode, origin: request.origin, glyphs, clusters, visual_of_logical, carets: caret_storage, source_len: u32::try_from(request.text.len()).map_err(|_| Error::TooMany)? };
	// REFUSED RATHER THAN HANDED ON. If what this produced does not satisfy the seam's own invariants
	// then the defect is HERE, and passing it to a consumer would move the symptom somewhere the cause
	// cannot be seen.
	if run.cluster_map().is_none() {
		return Err(Error::InconsistentMapping);
	}
	Ok(run)
}

/// One glyph, converted across the one line between font units and the seam.
///
/// THE Y AXIS TURNS OVER HERE. A font measures upward from the baseline; the seam's device space
/// measures downward. Passing font units straight through would put every mark on the wrong side of
/// its base - which looks like a broken font rather than a sign nobody wrote down.
fn positioned(glyph: u16, position: font_shape::Position, units_per_em: u16, size: Fixed266, kind: GlyphKind, selection: KindSelection) -> Result<PositionedGlyph, Error> {
	Ok(PositionedGlyph { glyph: glyph as u32, x_offset: Fixed266::from_font_units(position.x_offset, units_per_em, size)?, y_offset: Fixed266::from_font_units(-position.y_offset, units_per_em, size)?, x_advance: Fixed266::from_font_units(position.x_advance, units_per_em, size)?, y_advance: Fixed266::from_font_units(-position.y_advance, units_per_em, size)?, kind, selection })
}

/// The grapheme boundaries inside one run's source, as absolute offsets into the whole string.
///
/// ABSOLUTE, because a cluster range is a range of the ORIGINAL string; and computed over the run's
/// SLICE, because a boundary is a property of the text around it and the run is what was shaped.
fn grapheme_boundaries_of(text: &str, start: usize, end: usize) -> Vec<usize> {
	let slice = &text[start..end];
	let mut offsets = Vec::new();
	if offsets.try_reserve_exact(slice.len() + 1).is_err() {
		return Vec::new();
	}
	offsets.resize(slice.len() + 1, 0usize);
	let count = unicode_segmentation::grapheme::grapheme_boundaries(slice, &mut offsets);
	offsets.truncate(count.min(offsets.len()));
	for offset in offsets.iter_mut() {
		*offset += start;
	}
	offsets
}
