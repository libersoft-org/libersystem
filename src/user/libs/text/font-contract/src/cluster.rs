//! The CLUSTER MAPPING: what a caret, a selection, a hit test and an accessibility tree are built
//! from.
//!
//! NOT A SINGLE INDEX, AND NOT OPTIONAL. One cluster-start integer per glyph cannot express a
//! ligature caret - where the caret goes INSIDE `ffi` - nor RTL affinity, where one byte offset has
//! two screen positions, nor a discontiguous bidi selection, where one contiguous range of text is
//! two runs of pixels. All three are ordinary text, so a mapping that cannot express them is a
//! mapping every consumer works around.
//!
//! IT CARRIES FOUR THINGS: the source RANGE per cluster, the intra-ligature caret positions, caret
//! affinity, and the logical-to-visual mapping selection geometry is derived from.
//!
//! CLUSTERS ARE STORED IN LOGICAL ORDER, the order the text was written. Visual order is derived
//! from the mapping and never stored in its place: storing it would put a bidi decision inside the
//! data every consumer reads, and a hit test would have to undo it to answer a question about a
//! string.

use crate::fixed::Fixed266;

/// A range of the ORIGINAL STRING, as UTF-8 byte offsets.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SourceRange {
	pub start: u32,
	/// Exclusive.
	pub end: u32,
}

/// Which side of a boundary a caret belongs to.
///
/// A BOUNDARY BETWEEN TWO DIRECTIONS HAS TWO PLACES, and the offset alone does not say which. A
/// caret after the last Arabic character and one before the following Latin character are the same
/// byte offset and different pixels; affinity is which of them was meant.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CaretAffinity {
	/// The caret belongs to the text BEFORE the boundary.
	Trailing,
	/// The caret belongs to the text AFTER the boundary.
	Leading,
}

/// A caret position: where in the string, and which side of it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Caret {
	pub offset: u32,
	pub affinity: CaretAffinity,
}

/// One cluster: a range of source bytes, the glyphs it produced, and how many caret stops are inside
/// it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cluster {
	pub source: SourceRange,
	/// The first glyph of this cluster, as an index into the run's glyphs.
	pub first_glyph: u16,
	/// How many glyphs. A cluster with several glyphs is a decomposition; several clusters sharing
	/// one glyph is a ligature, and that is what the caret stops below are for.
	pub glyph_count: u16,
	/// Where this cluster's intra-ligature caret offsets begin, in the map's caret storage.
	pub first_caret: u16,
	/// How many. Zero for a cluster that has no caret position inside it.
	pub caret_count: u16,
}

/// The mapping, borrowed from whatever laid the text out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClusterMap<'a> {
	clusters: &'a [Cluster],
	visual_of_logical: &'a [u16],
	carets: &'a [Fixed266],
	source_len: u32,
}

impl<'a> ClusterMap<'a> {
	/// Build a mapping, CHECKING the invariants a consumer would otherwise have to trust.
	///
	/// WHAT IS CHECKED, and each of these is a way a mapping has actually been wrong: clusters are
	/// in logical order and do not overlap; they stay inside the source; the visual order is a
	/// PERMUTATION rather than any list of numbers; and every cluster's caret slice is inside the
	/// caret storage. A mapping that fails any of them is refused here rather than producing a
	/// selection that is subtly wrong somewhere else.
	pub fn new(clusters: &'a [Cluster], visual_of_logical: &'a [u16], carets: &'a [Fixed266], source_len: u32) -> Option<Self> {
		if clusters.len() != visual_of_logical.len() || clusters.len() > u16::MAX as usize {
			return None;
		}
		let mut previous_end = 0u32;
		for cluster in clusters {
			if cluster.source.start < previous_end || cluster.source.end < cluster.source.start || cluster.source.end > source_len {
				return None;
			}
			previous_end = cluster.source.end;
			let first = cluster.first_caret as usize;
			let count = cluster.caret_count as usize;
			if first + count > carets.len() {
				return None;
			}
		}
		// A PERMUTATION: every visual position used exactly once. Counting is enough and needs no
		// allocation, because the positions are bounded by the cluster count.
		for position in 0..clusters.len() {
			if visual_of_logical.iter().filter(|value| **value as usize == position).count() != 1 {
				return None;
			}
		}
		Some(Self { clusters, visual_of_logical, carets, source_len })
	}

	pub fn clusters(&self) -> &'a [Cluster] {
		self.clusters
	}

	pub fn source_len(&self) -> u32 {
		self.source_len
	}

	/// The cluster containing a byte offset, in LOGICAL order.
	pub fn cluster_at(&self, offset: u32) -> Option<usize> {
		self.clusters.iter().position(|cluster| offset >= cluster.source.start && offset < cluster.source.end)
	}

	/// Where a logical cluster is drawn.
	pub fn visual_of_logical(&self, logical: usize) -> Option<usize> {
		self.visual_of_logical.get(logical).map(|value| *value as usize)
	}

	/// Which cluster is drawn at a visual position - the direction a hit test asks in.
	pub fn logical_of_visual(&self, visual: usize) -> Option<usize> {
		self.visual_of_logical.iter().position(|value| *value as usize == visual)
	}

	/// The caret offsets INSIDE a cluster, relative to the cluster's own origin: the ligature
	/// carets, which is what makes a caret land between `f` and `fi` in `ffi`.
	pub fn intra_ligature_carets(&self, logical: usize) -> &'a [Fixed266] {
		match self.clusters.get(logical) {
			Some(cluster) => {
				let first = cluster.first_caret as usize;
				&self.carets[first..first + cluster.caret_count as usize]
			}
			None => &[],
		}
	}

	/// Is a selection of source bytes CONTIGUOUS on screen?
	///
	/// THE QUESTION A SELECTION HAS TO ASK, and the reason the visual mapping is here: one range of
	/// text in mixed-direction content is drawn as several separate pieces, and a consumer that
	/// assumed otherwise draws a highlight over text the user did not select.
	pub fn selection_is_contiguous(&self, range: SourceRange) -> bool {
		let mut lowest = usize::MAX;
		let mut highest = 0usize;
		let mut count = 0usize;
		for (logical, cluster) in self.clusters.iter().enumerate() {
			if cluster.source.end <= range.start || cluster.source.start >= range.end {
				continue;
			}
			let Some(visual) = self.visual_of_logical(logical) else {
				return false;
			};
			lowest = lowest.min(visual);
			highest = highest.max(visual);
			count += 1;
		}
		count == 0 || highest - lowest + 1 == count
	}
}
