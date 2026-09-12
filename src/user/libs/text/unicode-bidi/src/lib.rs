//! UAX #9, the Unicode bidirectional algorithm.
//!
//! HALF OF THIS IS WORSE THAN NONE, which is why it is implemented whole rather than to the depth a
//! first document needs. An implementation with paragraph levels and without the weak-type rules
//! renders Arabic with its numbers in the wrong order; one without the isolate rules renders a
//! quoted Hebrew phrase inside an English sentence backwards. Both look plausible and are wrong, and
//! a reader who cannot read the script cannot tell.
//!
//! THE RULES ARE NUMBERED AS THE DOCUMENT NUMBERS THEM - P2, X1 to X10, W1 to W7, N0 to N2, I1, I2,
//! L1 and L2 - and each is named at the code that implements it. What decides whether they are right
//! is `BidiTest` and `BidiCharacterTest`, run in full by the gate: between them they are every
//! ordering the algorithm must produce, including the ones no implementer would think to write.
//!
//! WHAT THIS PRODUCES IS LEVELS, and one reordering helper over them. A level is even for
//! left-to-right and odd for right-to-left; the shaping stack reads them per run and the layout
//! reverses by level. Mirroring is a property lookup the renderer applies to the character it draws,
//! which is why it is answered here and not done here.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use unicode_tables::{BidiClass, bidi_class};

/// The longest paragraph the algorithm is run over in one call.
///
/// A BOUND RATHER THAN A GROWING BUFFER, because the algorithm is quadratic in its worst case over
/// bracket pairs and this is a text stack reading untrusted input. The layer above bounds a
/// paragraph too; this is where THIS function stops.
pub const MAX_PARAGRAPH: usize = 65536;

/// How many bracket pairs rule N0 will consider. UAX #9 names 63 as the stack limit and says an
/// implementation may stop there, which is what this does rather than growing.
const MAX_BRACKET_PAIRS: usize = 63;

/// The explicit level limit UAX #9 states.
const MAX_DEPTH: u8 = 125;

/// Which way a paragraph runs when the text does not say.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParagraphDirection {
	/// Rule P2 and P3: take the direction from the first strong character, and left-to-right when
	/// there is none.
	Auto,
	LeftToRight,
	RightToLeft,
}

/// The result: one embedding level per character, and the paragraph's own level.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Levels {
	pub paragraph_level: u8,
	/// One level per CHARACTER of the input, in logical order.
	pub levels: Vec<u8>,
	/// The class each character was resolved to, which a caller needs for rule L1 and for deciding
	/// what a run is made of.
	pub classes: Vec<BidiClass>,
}

/// The paragraph level rule P2/P3: the first strong character decides, skipping isolate runs.
pub fn paragraph_level(text: &str, direction: ParagraphDirection) -> u8 {
	match direction {
		ParagraphDirection::LeftToRight => return 0,
		ParagraphDirection::RightToLeft => return 1,
		ParagraphDirection::Auto => {}
	}
	let mut isolate_depth = 0usize;
	for character in text.chars() {
		match bidi_class(character) {
			// P2 skips the contents of an isolate: a right-to-left phrase inside an isolate does not
			// decide the direction of the paragraph that contains it.
			BidiClass::LRI | BidiClass::RLI | BidiClass::FSI => isolate_depth += 1,
			BidiClass::PDI => isolate_depth = isolate_depth.saturating_sub(1),
			BidiClass::L if isolate_depth == 0 => return 0,
			BidiClass::R | BidiClass::AL if isolate_depth == 0 => return 1,
			_ => {}
		}
	}
	// P3: no strong character, so left to right.
	0
}

/// The embedding levels of a paragraph.
pub fn levels(text: &str, direction: ParagraphDirection) -> Levels {
	let characters: Vec<char> = text.chars().take(MAX_PARAGRAPH).collect();
	let classes: Vec<BidiClass> = characters.iter().map(|character| bidi_class(*character)).collect();
	let paragraph = paragraph_level(text, direction);
	resolve(&classes, Some(&characters), paragraph)
}

/// The same, for a caller that already has the classes - which the conformance file does, and which
/// is what lets it be tested without inventing characters for a class.
pub fn levels_of_classes(classes: &[BidiClass], direction: ParagraphDirection) -> Levels {
	let paragraph = match direction {
		ParagraphDirection::LeftToRight => 0,
		ParagraphDirection::RightToLeft => 1,
		ParagraphDirection::Auto => auto_level(classes),
	};
	resolve(classes, None, paragraph)
}

/// P2 and P3 over a class sequence.
fn auto_level(classes: &[BidiClass]) -> u8 {
	let mut isolate_depth = 0usize;
	for class in classes {
		match class {
			BidiClass::LRI | BidiClass::RLI | BidiClass::FSI => isolate_depth += 1,
			BidiClass::PDI => isolate_depth = isolate_depth.saturating_sub(1),
			BidiClass::L if isolate_depth == 0 => return 0,
			BidiClass::R | BidiClass::AL if isolate_depth == 0 => return 1,
			_ => {}
		}
	}
	0
}

mod explicit;
mod implicit;
mod reorder;

pub use reorder::{mirrored, reorder_visual};

fn resolve(classes: &[BidiClass], text: Option<&[char]>, paragraph_level: u8) -> Levels {
	let mut state = explicit::resolve_explicit(classes, paragraph_level);
	implicit::resolve_implicit(classes, text, &mut state, paragraph_level);
	Levels { paragraph_level, levels: state.levels, classes: state.classes }
}

/// What the explicit pass produced and the implicit pass refines.
pub(crate) struct Working {
	pub levels: Vec<u8>,
	pub classes: Vec<BidiClass>,
	/// The characters X9 removes from consideration: the embedding and override controls and the
	/// pops. They keep a level for rule L1 and take no part in anything else.
	pub removed: Vec<bool>,
	/// For each isolate initiator, the index of its matching PDI - BD9, which X10's run sequences
	/// are built out of.
	pub matching_pdi: Vec<usize>,
}

#[cfg(test)]
mod tests;
