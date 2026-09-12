//! The per-script shapers: what has to happen to a run BEFORE any lookup is applied.
//!
//! A SHAPER IS NOT A FEATURE LIST. Every script below needs something decided about the run itself -
//! which form each letter takes, which syllable a mark belongs to, how a cluster is ordered - and
//! none of it is expressible as "turn these features on". That is why Latin alone proves nothing:
//! Latin needs no shaper at all, so a stack that renders it beautifully has not exercised the part
//! that makes every other script work.
//!
//! WHAT EACH SHAPER DOES HERE is set the per-glyph MASK, so that the features which are positional
//! apply where they belong and nowhere else. The lookups themselves are the font's.

use alloc::vec::Vec;
use unicode_tables::{IndicSyllabic, JoiningType, Script, indic_syllabic, joining_type, script};

use crate::buffer::{Buffer, GLOBAL};

/// The mask bits the positional forms take. Bit 0 is `GLOBAL`, which every glyph carries.
pub const ISOL: u32 = 1 << 1;
pub const INIT: u32 = 1 << 2;
pub const MEDI: u32 = 1 << 3;
pub const FINA: u32 = 1 << 4;

/// The joining form a letter takes in its context.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
	Isolated,
	Initial,
	Medial,
	Final,
}

impl Form {
	pub const fn mask(self) -> u32 {
		match self {
			Self::Isolated => ISOL,
			Self::Initial => INIT,
			Self::Medial => MEDI,
			Self::Final => FINA,
		}
	}

	pub const fn feature(self) -> [u8; 4] {
		match self {
			Self::Isolated => *b"isol",
			Self::Initial => *b"init",
			Self::Medial => *b"medi",
			Self::Final => *b"fina",
		}
	}
}

/// Which shaper a script runs through, decided from the characters themselves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shaper {
	/// Apply the features in order; no state before them. Latin, Greek, Cyrillic, Hebrew and the
	/// rest of the alphabetic scripts - and the reason Latin alone proves nothing, because this
	/// shaper does nothing.
	Default,
	/// Joining forms computed before any lookup runs.
	Cursive,
	/// Syllables identified and reordered before the lookups.
	Indic,
	/// Khmer's own stacking and reordering.
	Khmer,
	/// Myanmar's own reordering, which is by category rather than by position.
	Myanmar,
	/// Jamo composed into the syllable a font has a glyph for.
	Hangul,
	/// A cluster model without reordering, for the scripts the universal engine was written for.
	Universal,
}

/// The shaper a run of characters needs, from the first character that decides.
pub fn shaper_for(characters: &[char]) -> Shaper {
	for character in characters {
		match script(*character) {
			Script::Arabic | Script::Syriac | Script::Nko | Script::Mandaic | Script::Adlam => return Shaper::Cursive,
			Script::Devanagari | Script::Bengali | Script::Gurmukhi | Script::Gujarati | Script::Oriya | Script::Tamil | Script::Telugu | Script::Kannada | Script::Malayalam | Script::Sinhala => {
				return Shaper::Indic;
			}
			Script::Khmer => return Shaper::Khmer,
			Script::Myanmar => return Shaper::Myanmar,
			Script::Hangul => return Shaper::Hangul,
			Script::Thai | Script::Lao | Script::Tibetan => return Shaper::Universal,
			_ => {}
		}
	}
	Shaper::Default
}

/// THE CURSIVE SHAPER: which form each letter takes, from what it can join to.
///
/// THE RULE IS ABOUT NEIGHBOURS THAT ARE NOT MARKS. A letter joins to the nearest preceding and
/// following letter, and the marks between them are TRANSPARENT - which is the whole reason the
/// joining type has a `T` at all. A shaper that looked at the immediate neighbour would break a
/// join wherever a vowel mark happens to sit, which in Arabic is most places.
pub fn cursive_forms(characters: &[char]) -> Vec<Form> {
	let types: Vec<JoiningType> = characters.iter().map(|character| joining_type(*character)).collect();
	let mut forms = Vec::with_capacity(characters.len());
	for index in 0..characters.len() {
		if types[index] == JoiningType::T {
			// A transparent character has no form of its own; it takes the isolated mask so a font
			// that puts marks in `isol` still reaches it.
			forms.push(Form::Isolated);
			continue;
		}
		let previous = types[..index].iter().rev().find(|kind| **kind != JoiningType::T).copied();
		let next = types[index + 1..].iter().find(|kind| **kind != JoiningType::T).copied();
		// A letter joins to what precedes it when that letter can join FORWARD, and to what follows
		// when that one can join BACKWARD.
		let joins_previous = matches!(previous, Some(JoiningType::D) | Some(JoiningType::L) | Some(JoiningType::C));
		let joins_next = matches!(next, Some(JoiningType::D) | Some(JoiningType::R) | Some(JoiningType::C));
		let kind = types[index];
		let form = match (joins_previous, joins_next) {
			(true, true) if matches!(kind, JoiningType::D | JoiningType::C) => Form::Medial,
			(true, _) if matches!(kind, JoiningType::D | JoiningType::R | JoiningType::C) => Form::Final,
			(_, true) if matches!(kind, JoiningType::D | JoiningType::L | JoiningType::C) => Form::Initial,
			_ => Form::Isolated,
		};
		forms.push(form);
	}
	forms
}

/// Set each glyph's mask from the form its character takes.
///
/// ONE CHARACTER PER GLYPH AT THIS POINT, which is true because the masks are set BEFORE any
/// substitution runs - that is the only moment at which the buffer and the text still correspond.
pub fn apply_cursive_masks(buffer: &mut Buffer, characters: &[char]) {
	let forms = cursive_forms(characters);
	for (index, info) in buffer.infos.iter_mut().enumerate() {
		let Some(form) = forms.get(index) else { continue };
		info.mask = GLOBAL | form.mask();
	}
}

/// The features a cursive run turns on, with the mask each applies under.
pub fn cursive_features() -> [([u8; 4], u32); 8] {
	[
		// The positional forms, each only where its form was decided.
		(*b"isol", ISOL),
		(*b"init", INIT),
		(*b"medi", MEDI),
		(*b"fina", FINA),
		// And the ones that apply across the whole run.
		(*b"rlig", GLOBAL),
		(*b"calt", GLOBAL),
		(*b"liga", GLOBAL),
		(*b"mset", GLOBAL),
	]
}

/// THE INDIC SHAPER'S FIRST HALF: where each syllable begins.
///
/// A SYLLABLE IS THE UNIT EVERYTHING ELSE IS ABOUT. Indic reordering moves a vowel sign from after
/// its consonant to before it, moves a reph from the front of the syllable to the end, and both are
/// defined relative to a syllable rather than to the run - so a shaper that has not found the
/// syllable boundaries cannot do either. The categories come from the generated
/// `Indic_Syllabic_Category` table.
pub fn syllable_starts(characters: &[char]) -> Vec<usize> {
	let mut starts = Vec::new();
	let mut index = 0usize;
	while index < characters.len() {
		starts.push(index);
		index += 1;
		// A syllable continues while what follows attaches to it: a virama binds the consonant after
		// it into the same syllable, and marks, nuktas and vowel signs belong to the one they follow.
		let mut after_virama = false;
		while index < characters.len() {
			let category = indic_syllabic(characters[index]);
			let continues = match category {
				IndicSyllabic::Virama | IndicSyllabic::InvisibleStacker => {
					after_virama = true;
					true
				}
				// A MEDIAL OR SUBJOINED CONSONANT IS PART OF THE CLUSTER IT FOLLOWS, always: that is
				// what makes it medial. Only a full consonant needs a virama before it to belong to
				// the syllable rather than to start the next one - and requiring one for all four
				// splits a Myanmar cluster at its medial `ra`, which then never reaches the front.
				IndicSyllabic::ConsonantMedial | IndicSyllabic::ConsonantSubjoined => true,
				IndicSyllabic::Consonant | IndicSyllabic::ConsonantDead => after_virama,
				IndicSyllabic::VowelDependent | IndicSyllabic::Bindu | IndicSyllabic::Visarga | IndicSyllabic::Nukta | IndicSyllabic::ToneMark | IndicSyllabic::SyllableModifier | IndicSyllabic::CantillationMark => true,
				_ => false,
			};
			if !continues {
				break;
			}
			if !matches!(category, IndicSyllabic::Virama | IndicSyllabic::InvisibleStacker) {
				after_virama = false;
			}
			index += 1;
		}
	}
	starts
}

/// THE INDIC SHAPER'S SECOND HALF: the reordering that makes the script readable.
///
/// A PRE-BASE VOWEL SIGN IS WRITTEN AFTER ITS CONSONANT AND DRAWN BEFORE IT. That is not a font
/// feature - it is the writing system - and a shaper that leaves the order alone renders Devanagari
/// with every `ि` on the wrong side of its letter. The categories say which signs those are;
/// `Indic_Positional_Category` says `Left`, and this moves them to the front of their syllable.
pub fn reorder_indic(characters: &[char], buffer: &mut Buffer) {
	let starts = syllable_starts(characters);
	for (index, start) in starts.iter().enumerate() {
		let end = starts.get(index + 1).copied().unwrap_or(characters.len()).min(buffer.len());
		let start = *start;
		if start >= end {
			continue;
		}
		// Find the pre-base vowel signs of this syllable and move them to its front, keeping their
		// order among themselves.
		let mut at = start + 1;
		let mut insert_at = start;
		while at < end {
			let is_pre_base = unicode_tables::indic_positional(characters[at]) == unicode_tables::IndicPositional::Left;
			if is_pre_base {
				let info = buffer.infos.remove(at);
				let position = buffer.positions.remove(at);
				buffer.infos.insert(insert_at, info);
				buffer.positions.insert(insert_at, position);
				insert_at += 1;
			}
			at += 1;
		}
	}
}

/// The features an Indic run turns on.
pub fn indic_features() -> [([u8; 4], u32); 10] {
	[
		(*b"nukt", GLOBAL),
		(*b"akhn", GLOBAL),
		(*b"rphf", GLOBAL),
		(*b"blwf", GLOBAL),
		(*b"half", GLOBAL),
		(*b"vatu", GLOBAL),
		(*b"pres", GLOBAL),
		(*b"blws", GLOBAL),
		(*b"abvs", GLOBAL),
		(*b"psts", GLOBAL),
	]
}

/// THE HANGUL SHAPER: jamo composed into the syllable a font actually has a glyph for.
///
/// A KOREAN SYLLABLE IS WRITTEN AS TWO OR THREE JAMO AND DRAWN AS ONE SQUARE. Almost every font
/// carries glyphs for the eleven thousand precomposed syllables and NOT for the jamo arranged into
/// them, so a shaper that left a decomposed sequence alone renders three letters in a row where a
/// reader expects one block. The composition is arithmetic rather than a table - which is why this
/// is a shaper rather than a feature.
pub fn compose_hangul(characters: &[char], buffer: &mut Buffer) -> Vec<char> {
	// The blocks, as Unicode defines them for this arithmetic.
	const L_BASE: u32 = 0x1100;
	const V_BASE: u32 = 0x1161;
	const T_BASE: u32 = 0x11A7;
	const S_BASE: u32 = 0xAC00;
	const L_COUNT: u32 = 19;
	const V_COUNT: u32 = 21;
	const T_COUNT: u32 = 28;

	let mut composed: Vec<char> = Vec::with_capacity(characters.len());
	let mut index = 0usize;
	while index < characters.len() {
		let lead = characters[index] as u32;
		let is_lead = (L_BASE..L_BASE + L_COUNT).contains(&lead);
		if !is_lead || index + 1 >= characters.len() {
			composed.push(characters[index]);
			index += 1;
			continue;
		}
		let vowel = characters[index + 1] as u32;
		if !(V_BASE..V_BASE + V_COUNT).contains(&vowel) {
			composed.push(characters[index]);
			index += 1;
			continue;
		}
		// A trailing consonant if there is one; `T_BASE` itself is the "no trailing" slot, which is
		// why the range starts one past it.
		let trailing = characters.get(index + 2).map(|character| *character as u32).filter(|code| (T_BASE + 1..T_BASE + T_COUNT).contains(code));
		let syllable = S_BASE + ((lead - L_BASE) * V_COUNT + (vowel - V_BASE)) * T_COUNT + trailing.map(|code| code - T_BASE).unwrap_or(0);
		let Some(syllable) = char::from_u32(syllable) else {
			composed.push(characters[index]);
			index += 1;
			continue;
		};
		let consumed = if trailing.is_some() { 3 } else { 2 };
		// The buffer follows the characters: the jamo that went into the syllable leave it, and the
		// syllable keeps the cluster of the first of them.
		if index < buffer.len() {
			let end = (index + consumed).min(buffer.len());
			buffer.infos.drain(index + 1..end);
			buffer.positions.drain(index + 1..end);
		}
		composed.push(syllable);
		index += consumed;
	}
	composed
}

/// The features a Hangul run turns on, for the jamo a font DOES carry glyphs for.
pub fn hangul_features() -> [([u8; 4], u32); 3] {
	[(*b"ljmo", GLOBAL), (*b"vjmo", GLOBAL), (*b"tjmo", GLOBAL)]
}

/// THE KHMER SHAPER. Khmer stacks its subjoined consonants under a base with an invisible marker -
/// `coeng` - and writes some of its vowels before the consonant they are pronounced after, like the
/// Indic scripts. What it does NOT share is the Indic syllable model, which is why the profile names
/// it separately: a Khmer cluster can carry two subjoined consonants and a pre-base vowel at once.
pub fn reorder_khmer(characters: &[char], buffer: &mut Buffer) {
	// The syllable model is the same shape - an invisible stacker binds what follows into the
	// cluster - and `syllable_starts` already reads `Invisible_Stacker`, which is what Khmer's coeng
	// is. What differs is only that the pre-base vowels are moved to the front of the WHOLE cluster
	// including its subjoined consonants, which is what this does.
	reorder_indic(characters, buffer);
}

/// The features a Khmer run turns on.
pub fn khmer_features() -> [([u8; 4], u32); 6] {
	[(*b"pref", GLOBAL), (*b"blwf", GLOBAL), (*b"abvf", GLOBAL), (*b"pstf", GLOBAL), (*b"cfar", GLOBAL), (*b"pres", GLOBAL)]
}

/// THE MYANMAR SHAPER. Myanmar writes its medial consonants and vowel signs in an order that is not
/// the order they are drawn in, and unlike Indic the reordering is by CATEGORY rather than by
/// position: the pre-base vowel `e` moves before its consonant, and the medial `ra` moves before
/// everything in its cluster.
pub fn reorder_myanmar(characters: &[char], buffer: &mut Buffer) {
	let starts = syllable_starts(characters);
	for (index, start) in starts.iter().enumerate() {
		let end = starts.get(index + 1).copied().unwrap_or(characters.len()).min(buffer.len());
		let start = *start;
		if start >= end {
			continue;
		}
		let mut insert_at = start;
		let mut at = start + 1;
		while at < end {
			let character = characters[at];
			// MEDIAL RA comes first of all, then the pre-base vowel. Both are written after the
			// consonant and drawn before it.
			let medial_ra = character == '\u{103C}';
			let pre_base_vowel = character == '\u{1031}';
			if medial_ra || pre_base_vowel {
				let info = buffer.infos.remove(at);
				let position = buffer.positions.remove(at);
				buffer.infos.insert(insert_at, info);
				buffer.positions.insert(insert_at, position);
				insert_at += 1;
			}
			at += 1;
		}
	}
}

/// The features a Myanmar run turns on.
pub fn myanmar_features() -> [([u8; 4], u32); 5] {
	[(*b"pref", GLOBAL), (*b"blwf", GLOBAL), (*b"pstf", GLOBAL), (*b"pres", GLOBAL), (*b"blws", GLOBAL)]
}

/// THE UNIVERSAL SHAPER'S CLUSTER MODEL, for the scripts whose rules it was written for - Thai, Lao
/// and Tibetan here.
///
/// WHAT IT IS FOR is the one thing all of those need and Latin does not: a CLUSTER that a mark
/// belongs to, so that a tone mark above a vowel above a consonant stacks rather than being placed
/// three times at the pen. It needs no reordering for these three, which is why it is a cluster
/// model rather than a reordering model - and saying so is better than leaving a reader to wonder
/// which reordering it forgot.
pub fn universal_clusters(characters: &[char]) -> Vec<usize> {
	syllable_starts(characters)
}

/// The features a universal-shaping run turns on.
pub fn universal_features() -> [([u8; 4], u32); 4] {
	[(*b"abvs", GLOBAL), (*b"blws", GLOBAL), (*b"psts", GLOBAL), (*b"pres", GLOBAL)]
}

/// The OpenType script tag a run's characters are indexed by in `GSUB` and `GPOS`.
///
/// NOT THE SAME AS THE UNICODE SCRIPT NAME, and the difference is not cosmetic: OpenType has TWO
/// tags for most Indic scripts - the old `deva` and the version-2 `dev2` - and a font written for one
/// carries no features under the other. The version-2 tag is tried first because it is what every
/// font made this century uses, and the fallback chain in `features_for` reaches the old one and then
/// `DFLT` on its own.
pub fn script_tag(characters: &[char]) -> [u8; 4] {
	for character in characters {
		let tag = match script(*character) {
			Script::Arabic => *b"arab",
			Script::Syriac => *b"syrc",
			Script::Nko => *b"nko ",
			Script::Hebrew => *b"hebr",
			Script::Devanagari => *b"dev2",
			Script::Bengali => *b"bng2",
			Script::Gurmukhi => *b"gur2",
			Script::Gujarati => *b"gjr2",
			Script::Oriya => *b"ory2",
			Script::Tamil => *b"tml2",
			Script::Telugu => *b"tel2",
			Script::Kannada => *b"knd2",
			Script::Malayalam => *b"mlm2",
			Script::Sinhala => *b"sinh",
			Script::Khmer => *b"khmr",
			Script::Myanmar => *b"mym2",
			Script::Thai => *b"thai",
			Script::Lao => *b"lao ",
			Script::Tibetan => *b"tibt",
			Script::Hangul => *b"hang",
			Script::Han => *b"hani",
			Script::Hiragana | Script::Katakana => *b"kana",
			Script::Greek => *b"grek",
			Script::Cyrillic => *b"cyrl",
			Script::Armenian => *b"armn",
			Script::Georgian => *b"geor",
			Script::Ethiopic => *b"ethi",
			Script::Thaana => *b"thaa",
			Script::Latin => *b"latn",
			// A character whose script decides nothing - a space, a digit, a mark - does not pick
			// the tag for the run.
			_ => continue,
		};
		return tag;
	}
	*b"DFLT"
}
