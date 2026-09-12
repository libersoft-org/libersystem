//! The buffer a shaper works in: the glyphs so far, and where each one sits.
//!
//! ONE BUFFER, REWRITTEN IN PLACE BY EACH LOOKUP, which is what makes a chain of lookups a chain:
//! the output of one is the input of the next, and a substitution that produced a new buffer would
//! lose the cluster mapping every later rule and every caret position is built from.
//!
//! EVERY GLYPH REMEMBERS WHICH CHARACTERS IT CAME FROM. A ligature is one glyph for three
//! characters and a decomposition is three glyphs for one; without the cluster the caret cannot be
//! put between them and a hit test cannot answer which character was clicked.

use alloc::vec::Vec;

/// The mask bit every glyph carries: the features that apply everywhere.
pub const GLOBAL: u32 = 1;

/// Where a glyph sits, in font units, before scaling.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Position {
	pub x_advance: i32,
	pub y_advance: i32,
	pub x_offset: i32,
	pub y_offset: i32,
}

/// One glyph in the buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GlyphInfo {
	pub glyph: u16,
	/// The cluster this glyph belongs to: the byte offset of the first character it came from.
	pub cluster: u32,
	/// The `GDEF` class - base, ligature, mark or component - which the mark rules and the lookup
	/// flags both turn on. Zero when the font declares none.
	pub class: u16,
	/// For a mark attached to something, which buffer position it attached to. `None` while nothing
	/// has attached it.
	pub attached_to: Option<u16>,
	/// WHICH FEATURES MAY APPLY HERE, as a bitmask.
	///
	/// A FEATURE IS NOT ALWAYS GLOBAL, and that is the whole of complex-script shaping. `fina`
	/// selects the final form of a letter and must apply to the LAST letter of a word and to no
	/// other; a shaper that applied it everywhere would render every Arabic letter in its final
	/// form, which is a word made of endings. Bit 0 is the mask every glyph carries, for the
	/// features that really are global.
	pub mask: u32,
}

impl GlyphInfo {
	pub const fn new(glyph: u16, cluster: u32) -> Self {
		Self { glyph, cluster, class: 0, attached_to: None, mask: GLOBAL }
	}
}

/// The glyphs and their positions, in visual order for the run being shaped.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Buffer {
	pub infos: Vec<GlyphInfo>,
	pub positions: Vec<Position>,
}

impl Buffer {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn push(&mut self, glyph: u16, cluster: u32) {
		self.infos.push(GlyphInfo::new(glyph, cluster));
		self.positions.push(Position::default());
	}

	pub fn len(&self) -> usize {
		self.infos.len()
	}

	pub fn is_empty(&self) -> bool {
		self.infos.is_empty()
	}

	/// Replace one glyph with another, keeping its cluster - which is what a single substitution is.
	pub fn substitute(&mut self, at: usize, glyph: u16) {
		if let Some(info) = self.infos.get_mut(at) {
			info.glyph = glyph;
		}
	}

	/// Replace a run of glyphs with one, as a ligature does.
	///
	/// THE CLUSTER OF THE RESULT IS THE FIRST ONE'S, and the ones that went away are gone from the
	/// buffer but not from the mapping: a caret between `f` and `i` inside `fi` is what the cluster
	/// range is for, and it is the layout above that keeps it.
	pub fn ligate(&mut self, at: usize, count: usize, glyph: u16) {
		if count == 0 || at + count > self.infos.len() {
			return;
		}
		self.infos[at].glyph = glyph;
		self.infos.drain(at + 1..at + count);
		self.positions.drain(at + 1..at + count);
	}

	/// Replace one glyph with several, as a decomposition does. All of them share its cluster.
	pub fn decompose(&mut self, at: usize, glyphs: &[u16]) {
		if glyphs.is_empty() || at >= self.infos.len() {
			return;
		}
		let cluster = self.infos[at].cluster;
		self.infos[at].glyph = glyphs[0];
		let mask = self.infos[at].mask;
		for (offset, glyph) in glyphs[1..].iter().enumerate() {
			let mut info = GlyphInfo::new(*glyph, cluster);
			// A decomposed glyph inherits what its source was allowed, or a per-position feature
			// would stop applying to the very glyphs a decomposition just produced.
			info.mask = mask;
			self.infos.insert(at + 1 + offset, info);
			self.positions.insert(at + 1 + offset, Position::default());
		}
	}
}
