//! X10 and W1 to I2: the isolating run sequences, the weak types, the brackets, the neutrals and the
//! implicit levels.
//!
//! EVERYTHING HERE IS PER ISOLATING RUN SEQUENCE, which is the part an implementation written from
//! the rule names alone gets wrong. The weak and neutral rules do not run over the paragraph; they
//! run over a SEQUENCE of level runs joined across matching isolates, each with its own start and
//! end direction taken from what surrounds it. A rule applied over the paragraph instead gives the
//! right answer for text with no isolates in it and the wrong one for text with any.

use alloc::vec;
use alloc::vec::Vec;
use unicode_tables::{BIDI_BRACKETS, BidiClass};

use crate::{MAX_BRACKET_PAIRS, Working};

/// One isolating run sequence (BD13): the indices it covers, in order, and the directions at its
/// two ends (X10).
struct Sequence {
	indices: Vec<usize>,
	start_of_level: BidiClass,
	end_of_level: BidiClass,
	level: u8,
}

pub(crate) fn resolve_implicit(original: &[BidiClass], state: &mut Working, paragraph_level: u8) {
	for sequence in sequences(original, state, paragraph_level) {
		let mut classes: Vec<BidiClass> = sequence.indices.iter().map(|index| state.classes[*index]).collect();
		weak_rules(&mut classes, &sequence);
		bracket_rule(original, &sequence, &mut classes);
		neutral_rules(&mut classes, &sequence);
		implicit_levels(&classes, &sequence, state);
		for (position, index) in sequence.indices.iter().enumerate() {
			state.classes[*index] = classes[position];
		}
	}
	// X9 removals keep the level of the character before them, which is what rule L1 reads.
	for index in 0..state.levels.len() {
		if state.removed[index] && index > 0 {
			state.levels[index] = state.levels[index - 1];
		}
	}
}

/// BD13 and X10: the level runs, joined across matching isolates, with the surrounding directions.
fn sequences(original: &[BidiClass], state: &Working, paragraph_level: u8) -> Vec<Sequence> {
	let length = original.len();
	// The characters X9 removed take no part in a sequence at all.
	let kept: Vec<usize> = (0..length).filter(|index| !state.removed[*index]).collect();
	let mut used = vec![false; length];
	let mut out = Vec::new();
	for (position, index) in kept.iter().enumerate() {
		if used[*index] {
			continue;
		}
		// A sequence starts at a level run that is not the continuation of an isolate.
		if position > 0 {
			let previous = kept[position - 1];
			let same_run = state.levels[previous] == state.levels[*index];
			let continues_isolate = matches!(original[previous], BidiClass::LRI | BidiClass::RLI | BidiClass::FSI) && state.matching_pdi.get(previous).copied() == Some(*index);
			if same_run && !continues_isolate {
				continue;
			}
			if continues_isolate {
				continue;
			}
		}
		let level = state.levels[*index];
		let mut indices = Vec::new();
		let mut at = position;
		loop {
			// The level run from here.
			while at < kept.len() && state.levels[kept[at]] == level {
				indices.push(kept[at]);
				used[kept[at]] = true;
				at += 1;
			}
			// BD13: if it ends with an isolate initiator that HAS a matching PDI, the sequence
			// continues at that PDI.
			let last = *indices.last().unwrap_or(index);
			let initiator = matches!(original[last], BidiClass::LRI | BidiClass::RLI | BidiClass::FSI);
			let matching = state.matching_pdi.get(last).copied().unwrap_or(length);
			if initiator && matching < length {
				match kept.iter().position(|candidate| *candidate == matching) {
					Some(next) => at = next,
					None => break,
				}
			} else {
				break;
			}
		}
		// X10: the direction at each end is the higher of this sequence's level and its neighbour's,
		// with the paragraph level standing in beyond the ends.
		let first = *indices.first().unwrap_or(index);
		let last = *indices.last().unwrap_or(index);
		let before = kept.iter().rev().find(|candidate| **candidate < first).map(|candidate| state.levels[*candidate]).unwrap_or(paragraph_level);
		let after_level = if matches!(original[last], BidiClass::LRI | BidiClass::RLI | BidiClass::FSI) && state.matching_pdi.get(last).copied().unwrap_or(length) >= length {
			// An isolate initiator with no matching PDI: the end of the sequence is the end of the
			// paragraph, whatever follows it.
			paragraph_level
		} else {
			kept.iter().find(|candidate| **candidate > last).map(|candidate| state.levels[*candidate]).unwrap_or(paragraph_level)
		};
		out.push(Sequence { start_of_level: if level.max(before) % 2 == 1 { BidiClass::R } else { BidiClass::L }, end_of_level: if level.max(after_level) % 2 == 1 { BidiClass::R } else { BidiClass::L }, level, indices });
	}
	out
}

/// W1 to W7.
fn weak_rules(classes: &mut [BidiClass], sequence: &Sequence) {
	use BidiClass::*;
	// W1: a non-spacing mark takes the type of what precedes it, and the start-of-sequence direction
	// when it is first. After an isolate initiator or a PDI it becomes ON, not the isolate's type.
	let mut previous = sequence.start_of_level;
	for class in classes.iter_mut() {
		if *class == NSM {
			*class = match previous {
				LRI | RLI | FSI | PDI => ON,
				other => other,
			};
		}
		previous = *class;
	}
	// W2: a European number becomes an Arabic number when the last strong type was an Arabic letter.
	let mut last_strong = sequence.start_of_level;
	for class in classes.iter_mut() {
		match *class {
			L | R | AL => last_strong = *class,
			EN if last_strong == AL => *class = AN,
			_ => {}
		}
	}
	// W3: an Arabic letter is a right-to-left character from here on.
	for class in classes.iter_mut() {
		if *class == AL {
			*class = R;
		}
	}
	// W4: a single separator between two numbers of the same kind joins them.
	for index in 1..classes.len().saturating_sub(1) {
		let (before, here, after) = (classes[index - 1], classes[index], classes[index + 1]);
		if here == ES && before == EN && after == EN {
			classes[index] = EN;
		}
		if here == CS && before == EN && after == EN {
			classes[index] = EN;
		}
		if here == CS && before == AN && after == AN {
			classes[index] = AN;
		}
	}
	// W5: a run of European terminators beside a European number becomes European numbers.
	let mut index = 0usize;
	while index < classes.len() {
		if classes[index] != ET {
			index += 1;
			continue;
		}
		let start = index;
		while index < classes.len() && classes[index] == ET {
			index += 1;
		}
		let before = if start > 0 { Some(classes[start - 1]) } else { None };
		let after = classes.get(index).copied();
		if before == Some(EN) || after == Some(EN) {
			for class in classes.iter_mut().take(index).skip(start) {
				*class = EN;
			}
		}
	}
	// W6: whatever separators and terminators are left are neutral.
	for class in classes.iter_mut() {
		if matches!(*class, ES | ET | CS) {
			*class = ON;
		}
	}
	// W7: a European number becomes left-to-right when the last strong type was.
	let mut last_strong = sequence.start_of_level;
	for class in classes.iter_mut() {
		match *class {
			L | R => last_strong = *class,
			EN if last_strong == L => *class = L,
			_ => {}
		}
	}
}

/// N0: the paired brackets.
///
/// THE RULE THAT NEEDS THE ORIGINAL CHARACTERS, which is why this one takes them: every other rule
/// here works on classes alone, and a bracket's PAIR is a property of the character. BD16's stack is
/// bounded at 63 pairs by the document itself, which is what this stops at rather than growing.
fn bracket_rule(original: &[BidiClass], sequence: &Sequence, classes: &mut [BidiClass]) {
	let _ = original;
	let characters = sequence.characters();
	let Some(characters) = characters else { return };
	let mut stack: Vec<(u32, usize)> = Vec::new();
	let mut pairs: Vec<(usize, usize)> = Vec::new();
	for (position, character) in characters.iter().enumerate() {
		if classes[position] != BidiClass::ON {
			continue;
		}
		match bracket(*character) {
			Some((pair, true)) => {
				if stack.len() == MAX_BRACKET_PAIRS {
					// BD16: over the limit the rule stops entirely rather than pairing some of them.
					return;
				}
				stack.push((canonical(pair), position));
			}
			Some((_, false)) => {
				let closing = canonical(*character);
				if let Some(found) = stack.iter().rposition(|(expected, _)| *expected == closing) {
					pairs.push((stack[found].1, position));
					stack.truncate(found);
				}
			}
			None => {}
		}
	}
	pairs.sort_by_key(|(opening, _)| *opening);

	let embedding = if sequence.level % 2 == 1 { BidiClass::R } else { BidiClass::L };
	let opposite = if sequence.level % 2 == 1 { BidiClass::L } else { BidiClass::R };
	for (opening, closing) in pairs {
		// N0 b: a strong type matching the embedding direction inside the pair sets both brackets to
		// it.
		let mut found_embedding = false;
		let mut found_opposite = false;
		for class in classes.iter().take(closing).skip(opening + 1) {
			match strong_of(*class) {
				Some(strong) if strong == embedding => found_embedding = true,
				Some(_) => found_opposite = true,
				None => {}
			}
		}
		let resolved = if found_embedding {
			Some(embedding)
		} else if found_opposite {
			// N0 c: an opposite-direction strong type inside, and then the context before the pair
			// decides - c1 takes the opposite direction, c2 takes the embedding one.
			let mut before = sequence.start_of_level;
			for class in classes.iter().take(opening).rev() {
				if let Some(strong) = strong_of(*class) {
					before = strong;
					break;
				}
			}
			Some(if before == opposite { opposite } else { embedding })
		} else {
			// N0 d: nothing strong inside, so the brackets are left to the neutral rules.
			None
		};
		if let Some(resolved) = resolved {
			classes[opening] = resolved;
			classes[closing] = resolved;
			// AND THE NSMs THAT FOLLOW A BRACKET FOLLOW IT, which the rule's own note says and which
			// nothing else would do now that W1 has already run.
			for position in [opening, closing] {
				let mut after = position + 1;
				while after < classes.len() && sequence.original_class(after) == Some(BidiClass::NSM) {
					classes[after] = resolved;
					after += 1;
				}
			}
		}
	}
}

/// The strong direction a resolved class counts as, for N0.
fn strong_of(class: BidiClass) -> Option<BidiClass> {
	match class {
		BidiClass::L => Some(BidiClass::L),
		BidiClass::R | BidiClass::EN | BidiClass::AN => Some(BidiClass::R),
		_ => None,
	}
}

/// N1 and N2: the neutrals.
fn neutral_rules(classes: &mut [BidiClass], sequence: &Sequence) {
	use BidiClass::*;
	let neutral = |class: BidiClass| matches!(class, B | S | WS | ON | FSI | LRI | RLI | PDI);
	let mut index = 0usize;
	while index < classes.len() {
		if !neutral(classes[index]) {
			index += 1;
			continue;
		}
		let start = index;
		while index < classes.len() && neutral(classes[index]) {
			index += 1;
		}
		// The directions on each side, with a number counting as right-to-left.
		let before = if start == 0 { sequence.start_of_level } else { strong_of(classes[start - 1]).unwrap_or(sequence.start_of_level) };
		let after = match classes.get(index) {
			Some(class) => strong_of(*class).unwrap_or(sequence.end_of_level),
			None => sequence.end_of_level,
		};
		// N1: a run of neutrals between two of the same direction takes it. N2: otherwise the
		// embedding direction.
		let resolved = if before == after {
			before
		} else if sequence.level % 2 == 1 {
			R
		} else {
			L
		};
		for class in classes.iter_mut().take(index).skip(start) {
			*class = resolved;
		}
	}
}

/// I1 and I2: the implicit levels.
fn implicit_levels(classes: &[BidiClass], sequence: &Sequence, state: &mut Working) {
	use BidiClass::*;
	for (position, index) in sequence.indices.iter().enumerate() {
		let level = sequence.level;
		state.levels[*index] = if level % 2 == 0 {
			// I1: at an even level, right-to-left goes up one and a number goes up two.
			match classes[position] {
				R => level + 1,
				AN | EN => level + 2,
				_ => level,
			}
		} else {
			// I2: at an odd level, everything that is not right-to-left goes up one.
			match classes[position] {
				L | AN | EN => level + 1,
				_ => level,
			}
		};
	}
}

/// The bracket pair of a character, and whether it opens.
fn bracket(character: char) -> Option<(char, bool)> {
	let code_point = character as u32;
	let found = BIDI_BRACKETS.binary_search_by_key(&code_point, |(candidate, _, _)| *candidate).ok()?;
	let (_, pair, opening) = BIDI_BRACKETS[found];
	Some((char::from_u32(pair)?, opening))
}

/// BD16's canonical equivalence: the two CJK angle brackets pair with their canonical equivalents,
/// which is the only case the rule names and the only one this folds.
fn canonical(character: char) -> u32 {
	match character as u32 {
		0x3008 => 0x2329,
		0x3009 => 0x232A,
		other => other,
	}
}

impl Sequence {
	/// The characters this sequence covers, when the caller gave the algorithm real text. `None`
	/// when it was run over classes alone - the bracket rule is the only one that needs them, and a
	/// conformance file that states classes has no brackets to pair.
	fn characters(&self) -> Option<&Vec<char>> {
		self.characters.as_ref()
	}

	/// The ORIGINAL class of the character at a position in this sequence.
	fn original_class(&self, position: usize) -> Option<BidiClass> {
		self.original.get(position).copied()
	}
}
