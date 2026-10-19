//! The buffer a shaper works in: the glyphs so far, and where each one sits.
//!
//! ONE BUFFER, REWRITTEN IN PLACE BY EACH LOOKUP, which is what makes a chain of lookups a chain:
//! the output of one is the input of the next, and a substitution that produced a new buffer would
//! lose the cluster mapping every later rule and every caret position is built from.
//!
//! EVERY GLYPH REMEMBERS WHICH CHARACTERS IT CAME FROM. A ligature is one glyph for three
//! characters and a decomposition is three glyphs for one; without the cluster the caret cannot be
//! put between them and a hit test cannot answer which character was clicked.
//!
//! IT GROWS FALLIBLY. Its size comes from the text and from the font, and infallible allocation in
//! userspace ends the calling process when it fails - which would take a caller's whole program down
//! over one paragraph instead of answering the typed refusal this stack promises.

use alloc::vec::Vec;
use font_parse::Error;

use crate::indic::Position as SyllablePosition;

/// The mask bit every glyph carries: the features that apply everywhere.
pub const GLOBAL: u32 = 1;

/// The mask bit for `rtlm`, the face's own mirrored forms.
///
/// SET BY THE CALLER, on the glyphs of a right-to-left run whose character was NOT replaced by its
/// mirror - because only the caller mapped the characters and knows which ones it mirrored. A glyph
/// drawn from the mirror character already faces the right way, and applying `rtlm` to it as well
/// would turn it back.
pub const RTLM: u32 = 1 << 5;

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
	/// A mark's `GDEF` mark attachment class, which a lookup's `markAttachmentType` selects marks by.
	/// Zero where the face puts the glyph in none.
	pub mark_class: u16,
	/// For a mark attached to something, which buffer position it attached to. `None` while nothing
	/// has attached it.
	pub attached_to: Option<u16>,
	/// THE LIGATURE A GLYPH BELONGS TO, numbered by the substitution that formed it: a ligature carries
	/// its own number, and a mark that sat between the components it was formed from carries the same
	/// one. Zero for every other glyph.
	///
	/// A MARK INSIDE A LIGATURE BELONGS TO ONE COMPONENT OF IT, and nothing else can say which: once
	/// the components are one glyph, a lam-alef whose fatha was written after the lam has to put that
	/// fatha over the lam half, which only the mark's own record of where it sat can tell.
	pub ligature: u16,
	/// For a mark that sat between a ligature's components, which one it followed, counted from one.
	/// Zero for every other glyph.
	pub component: u16,
	/// For a glyph a cursive attachment hangs from another, that other's position: what its offset
	/// across the line is measured from once positioning is done. `None` for every other glyph.
	pub cursive: Option<u16>,
	/// WHICH FEATURES MAY APPLY HERE, as a bitmask.
	///
	/// A FEATURE IS NOT ALWAYS GLOBAL, and that is the whole of complex-script shaping. `fina`
	/// selects the final form of a letter and must apply to the LAST letter of a word and to no
	/// other; a shaper that applied it everywhere would render every Arabic letter in its final
	/// form, which is a word made of endings. Bit 0 is the mask every glyph carries, for the
	/// features that really are global.
	pub mask: u32,
	/// The syllable this glyph belongs to, numbered by the shaper that found the syllables; zero
	/// where no shaper did.
	///
	/// CARRIED ON THE GLYPH, because a substitution changes how many glyphs a syllable has: once a
	/// ligature has formed, the glyphs themselves are the only thing left that knows where one
	/// syllable ends and the next begins.
	pub syllable: u16,
	/// Where in its syllable the Indic shaper draws this glyph. `None` for every other shaper.
	pub position: SyllablePosition,
}

impl GlyphInfo {
	pub const fn new(glyph: u16, cluster: u32) -> Self {
		Self { glyph, cluster, class: 0, mark_class: 0, attached_to: None, ligature: 0, component: 0, cursive: None, mask: GLOBAL, syllable: 0, position: SyllablePosition::None }
	}
}

/// The glyphs and their positions, in LOGICAL order - the order the text was written - for the run
/// being shaped.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Buffer {
	pub infos: Vec<GlyphInfo>,
	pub positions: Vec<Position>,
	/// WHETHER THE RUN IS DRAWN RIGHT TO LEFT. Every rule is applied in logical order whatever the
	/// direction, and one is not direction-free: a cursive join moves the glyph its pen meets first,
	/// which in a right-to-left run is the LATER one in the text. Set by whoever knows the run's level.
	pub right_to_left: bool,
	/// The number the last ligature formed here was given; see `GlyphInfo::ligature`.
	pub ligatures: u16,
}

impl Buffer {
	pub fn new() -> Self {
		Self::default()
	}

	/// Make room for this many more glyphs, or refuse.
	pub fn reserve(&mut self, additional: usize) -> Result<(), Error> {
		self.infos.try_reserve(additional).map_err(|_| Error::Allocation)?;
		self.positions.try_reserve(additional).map_err(|_| Error::Allocation)
	}

	/// Append one glyph, or refuse.
	pub fn push(&mut self, glyph: u16, cluster: u32) -> Result<(), Error> {
		self.reserve(1)?;
		self.infos.push(GlyphInfo::new(glyph, cluster));
		self.positions.push(Position::default());
		Ok(())
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

	/// Replace the glyphs at `components` - ascending, the first the ligature's own place - with one,
	/// KEEPING whatever the lookup skipped between them.
	///
	/// A LOOKUP THAT IGNORES MARKS FORMS ITS LIGATURE ACROSS THEM, and the marks are not components:
	/// a lam, a fatha and an alef become a lam-alef and the fatha, still there to be attached. Each
	/// kept mark records which component it followed, under the ligature's number, so the mark
	/// positioning can put it on that component's anchor rather than on whichever one a guess picks.
	pub fn ligate_components(&mut self, components: &[usize], glyph: u16) {
		let (Some(&first), Some(&last)) = (components.first(), components.last()) else { return };
		if last >= self.infos.len() || components.windows(2).any(|pair| pair[0] >= pair[1]) {
			return;
		}
		self.ligatures = self.ligatures.wrapping_add(1).max(1);
		let number = self.ligatures;
		self.infos[first].glyph = glyph;
		self.infos[first].ligature = number;
		self.infos[first].component = 0;
		for (followed, pair) in components.windows(2).enumerate() {
			for kept in pair[0] + 1..pair[1] {
				self.infos[kept].ligature = number;
				self.infos[kept].component = followed as u16 + 1;
			}
		}
		for at in components[1..].iter().rev() {
			self.infos.remove(*at);
			self.positions.remove(*at);
		}
	}

	/// Replace one glyph with several, as a decomposition does. All of them share its cluster.
	///
	/// THE ONLY PLACE A RUN GROWS, so it is where the run's absolute output ceiling is held as it
	/// grows rather than only after: a face that multiplies every glyph would otherwise be handed
	/// the memory for the whole expansion before the count was ever looked at.
	pub fn decompose(&mut self, at: usize, glyphs: &[u16]) -> Result<(), Error> {
		if glyphs.is_empty() || at >= self.infos.len() {
			return Ok(());
		}
		let grown = self.infos.len() + glyphs.len() - 1;
		if grown > opentype_profile::limits::RUN_OUTPUT as usize {
			return Err(Error::Unsupported(opentype_profile::Unsupported::Exceeded { limit: "run output", ceiling: opentype_profile::limits::RUN_OUTPUT, asked: grown as u64 }));
		}
		self.reserve(glyphs.len() - 1)?;
		let source = self.infos[at];
		self.infos[at].glyph = glyphs[0];
		for (offset, glyph) in glyphs[1..].iter().enumerate() {
			// A decomposed glyph inherits what its source was allowed and where its source stood, or
			// a per-position feature would stop applying to the very glyphs a decomposition just
			// produced and a syllable would lose track of its own pieces.
			let mut info = source;
			info.glyph = *glyph;
			info.attached_to = None;
			info.cursive = None;
			self.infos.insert(at + 1 + offset, info);
			self.positions.insert(at + 1 + offset, Position::default());
		}
		Ok(())
	}
}
