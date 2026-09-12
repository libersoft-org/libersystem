//! Where a line sits in its box.

use font_contract::Fixed266;

use crate::compose::Line;
use crate::{Error, ParagraphDirection};

/// Where the line goes in the width it was laid out into.
///
/// START AND END RATHER THAN LEFT AND RIGHT. "Start" is the left in a left-to-right paragraph and the
/// RIGHT in a right-to-left one; a layout spelled left and right makes every right-to-left document
/// ragged on the wrong side - a defect a reader of that script sees immediately and a developer of it
/// never does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alignment {
	/// Against the edge the text begins at.
	Start,
	/// Against the edge it ends at.
	End,
	/// Centred, with any odd half-unit of slack going to the START side.
	Centre,
	/// Filled to the box, by the justification policy.
	Justify,
	/// Against the LEFT, whatever the direction. For a column of numbers, where the glyphs and not
	/// the language decide.
	Left,
	/// Against the RIGHT, for the same reason.
	Right,
}

impl Alignment {
	/// Which physical side this is, once the paragraph's direction is known. `None` for `Justify`,
	/// which is not a side.
	fn side(self, direction: ParagraphDirection) -> Option<Side> {
		Some(match (self, direction.is_right_to_left()) {
			(Alignment::Start, false) | (Alignment::End, true) | (Alignment::Left, _) => Side::Left,
			(Alignment::Start, true) | (Alignment::End, false) | (Alignment::Right, _) => Side::Right,
			(Alignment::Centre, _) => Side::Centre,
			(Alignment::Justify, _) => return None,
		})
	}
}

enum Side {
	Left,
	Right,
	Centre,
}

/// Place the line in its box.
///
/// `Justify` ALIGNS TO THE START HERE AND NOTHING ELSE, because stretching is a separate decision
/// with its own opportunities and its own refusals - see [`crate::justify`]. A line that could not be
/// stretched still has to sit somewhere, and the start is where an unstretched line of justified text
/// belongs: aligning it to the end instead would make the last line of every paragraph jump to the
/// other margin.
pub fn align(line: &mut Line<'_>, alignment: Alignment) -> Result<(), Error> {
	let slack = line.slack();
	let side = alignment.side(line.direction).unwrap_or(match line.direction.is_right_to_left() {
		true => Side::Right,
		false => Side::Left,
	});
	let shift = match side {
		Side::Left => Fixed266::ZERO,
		Side::Right => slack,
		// THE ODD HALF-UNIT GOES TO THE START SIDE. Rounding toward zero would move a centred line
		// left in a right-to-left paragraph, which is the one direction it must not drift.
		Side::Centre => {
			let half = slack.raw() / 2;
			let remainder = slack.raw() - half * 2;
			Fixed266::from_raw(if line.direction.is_right_to_left() { half + remainder } else { half })
		}
	};
	line.shift(shift)
}
