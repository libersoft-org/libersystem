//! THE INDIC SHAPER'S SYLLABLE PLAN, and the reordering it owes after the basic features.
//!
//! A POSITIONAL FEATURE APPLIED TO THE WHOLE RUN IS NOT POSITIONAL. `half` is the form of a consonant
//! BEFORE the base of its syllable, and `rphf` is a syllable-initial Ra with a base after it. Applied
//! everywhere, `half` draws the dead consonant that ends a word - the `त्` of `जगत्` - as though
//! another consonant followed it, and `rphf` turns a word-final `र्` into a reph floating over
//! nothing. So every syllable is planned before any lookup runs: where its reph is, which consonant is
//! its base, and which glyphs stand before and after that base - and the masks say so, one syllable
//! at a time.
//!
//! THE BASE IS THE FACE'S QUESTION AS MUCH AS THE TEXT'S. In `क्र` the `र` is either a consonant of
//! its own or a form drawn below the `क`, and which one it is depends on whether the face HAS that
//! form. So the plan asks the face's own `blwf`, `vatu`, `pstf` and `pref` lookups, as the script
//! specifications describe, rather than a list of consonants written here.
//!
//! AND THE REPH MOVES. It is written first and drawn last: once the basic features have formed it,
//! the reph glyph is moved to where its script draws it - right after the base in Oriya and
//! Malayalam, after the base and what is drawn below it in Bengali, and at the end of the syllable,
//! before its bindu and visarga, everywhere else. A pre-base-reordering form moves the other way, to
//! just before the base. Both happen BETWEEN the basic features and the presentation features,
//! because the presentation lookups are written against the syllable as it will be drawn.
//!
//! WHAT THIS DELIBERATELY IS NOT: the specification's full initial sort. The pre-base vowel signs are
//! moved to the front of their syllable, after a reph still to form, before any lookup; below-base and
//! post-base forms are not reordered by position because they are drawn where they are written.

use font_parse::{Error, Face};
use unicode_tables::{IndicPositional, IndicSyllabic, Script, indic_positional, indic_syllabic, script};

use crate::buffer::{Buffer, GLOBAL};
use crate::scripts::syllable_starts;
use crate::shape::Probe;

/// The mask bits the per-syllable features apply under. Bit 0 is `GLOBAL`, bits 1 to 4 are the
/// cursive forms and bit 5 is `rtlm`; these follow them.
pub const RPHF: u32 = 1 << 6;
pub const HALF: u32 = 1 << 7;
pub const BLWF: u32 = 1 << 8;
pub const ABVF: u32 = 1 << 9;
pub const PSTF: u32 = 1 << 10;
pub const PREF: u32 = 1 << 11;

/// Where in its syllable a glyph is drawn, in the order a syllable is drawn.
///
/// THE ORDER IS THE POINT. The final reordering asks "the first glyph at or after the base" and "the
/// glyphs that are not yet post-base", and both are comparisons on this order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Position {
	/// Not in a planned syllable: every glyph of every other shaper.
	#[default]
	None,
	/// A syllable-initial Ra that is to become the reph.
	Reph,
	/// The virama after it, which the reph is formed from.
	RephVirama,
	/// A dependent vowel written after its consonant and drawn before it.
	PreMatra,
	/// A consonant before the base, and its virama: a half form.
	PreBase,
	/// The consonant the syllable is built on.
	Base,
	/// What a script draws over the base itself: Oriya's top vowel signs.
	AfterMain,
	/// A consonant the face draws below the base.
	BelowBase,
	/// A dependent vowel drawn above or below.
	AfterSub,
	/// A consonant the face draws after the base.
	PostBase,
	/// A dependent vowel drawn to the right.
	AfterPost,
	/// A bindu, a visarga, a tone or cantillation mark: the end of a syllable.
	Modifier,
}

/// Where a script draws its reph once it has formed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placement {
	/// Right after the base - Oriya and Malayalam.
	AfterMain,
	/// After the base and its below-base forms, before any post-base form, right-side vowel sign or
	/// modifier - Bengali.
	AfterSub,
	/// At the end of the syllable, before its modifiers - every other script.
	End,
}

/// How a script writes the reph.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RephMode {
	/// `Ra, virama` before another consonant.
	Implicit,
	/// `Ra, virama, ZWJ`: Telugu and Sinhala write it explicitly, and `Ra, virama` before a consonant
	/// is something else there.
	Explicit,
	/// Never from `Ra, virama`: Malayalam's dot reph is a character of its own.
	None,
}

/// What one script's Indic shaping needs to know about it.
#[derive(Clone, Copy, Debug)]
pub struct Config {
	/// The Ra the reph is written with. Bengali has two: Assamese writes its own.
	ras: &'static [char],
	/// The virama the face is asked about, to find which consonants have below- and post-base forms.
	virama: char,
	reph: RephMode,
	pub placement: Placement,
	/// Whether `blwf` applies before the base as well as after it.
	below_before_base: bool,
	/// Where a top vowel sign is drawn: over the base itself in Oriya, with the below-base ones
	/// everywhere else.
	top_matra: Position,
}

/// The configuration for the first character of a run whose script is Indic.
pub fn config(characters: &[char]) -> Option<Config> {
	characters.iter().find_map(|character| config_of(script(*character)))
}

fn config_of(found: Script) -> Option<Config> {
	let config = |ras: &'static [char], virama: char, reph: RephMode, placement: Placement, below_before_base: bool, top_matra: Position| Config { ras, virama, reph, placement, below_before_base, top_matra };
	Some(match found {
		Script::Devanagari => config(&['\u{0930}'], '\u{094D}', RephMode::Implicit, Placement::End, true, Position::AfterSub),
		Script::Bengali => config(&['\u{09B0}', '\u{09F0}'], '\u{09CD}', RephMode::Implicit, Placement::AfterSub, true, Position::AfterSub),
		Script::Gurmukhi => config(&['\u{0A30}'], '\u{0A4D}', RephMode::Implicit, Placement::End, true, Position::AfterSub),
		Script::Gujarati => config(&['\u{0AB0}'], '\u{0ACD}', RephMode::Implicit, Placement::End, true, Position::AfterSub),
		Script::Oriya => config(&['\u{0B30}'], '\u{0B4D}', RephMode::Implicit, Placement::AfterMain, true, Position::AfterMain),
		Script::Tamil => config(&['\u{0BB0}'], '\u{0BCD}', RephMode::Implicit, Placement::End, true, Position::AfterSub),
		Script::Telugu => config(&['\u{0C30}'], '\u{0C4D}', RephMode::Explicit, Placement::End, false, Position::AfterSub),
		Script::Kannada => config(&['\u{0CB0}'], '\u{0CCD}', RephMode::Implicit, Placement::End, false, Position::AfterSub),
		Script::Malayalam => config(&['\u{0D30}'], '\u{0D4D}', RephMode::None, Placement::AfterMain, true, Position::AfterSub),
		Script::Sinhala => config(&['\u{0DBB}'], '\u{0DCA}', RephMode::Explicit, Placement::End, true, Position::AfterSub),
		_ => return None,
	})
}

/// The basic features: applied before the final reordering, each only where its mask says.
pub fn basic_features() -> [([u8; 4], u32); 11] {
	[
		(*b"nukt", GLOBAL),
		(*b"akhn", GLOBAL),
		(*b"rphf", RPHF),
		(*b"rkrf", GLOBAL),
		(*b"pref", PREF),
		(*b"blwf", BLWF),
		(*b"abvf", ABVF),
		(*b"half", HALF),
		(*b"pstf", PSTF),
		(*b"vatu", GLOBAL),
		(*b"cjct", GLOBAL),
	]
}

/// The presentation features: applied after the final reordering, to the syllable as it is drawn.
pub fn presentation_features() -> [([u8; 4], u32); 5] {
	[(*b"pres", GLOBAL), (*b"abvs", GLOBAL), (*b"blws", GLOBAL), (*b"psts", GLOBAL), (*b"haln", GLOBAL)]
}

fn is_consonant(character: char) -> bool {
	matches!(indic_syllabic(character), IndicSyllabic::Consonant | IndicSyllabic::ConsonantDead | IndicSyllabic::ConsonantPlaceholder | IndicSyllabic::ConsonantWithStacker | IndicSyllabic::ConsonantMedial | IndicSyllabic::VowelIndependent)
}

fn is_virama(character: char) -> bool {
	indic_syllabic(character) == IndicSyllabic::Virama
}

fn is_joiner(character: char) -> bool {
	matches!(indic_syllabic(character), IndicSyllabic::Joiner | IndicSyllabic::NonJoiner)
}

const ZWJ: char = '\u{200D}';
const ZWNJ: char = '\u{200C}';

/// Where a dependent vowel sign is drawn, from where Unicode says it sits.
fn matra_position(config: &Config, character: char) -> Position {
	match indic_positional(character) {
		IndicPositional::Left => Position::PreMatra,
		IndicPositional::Right | IndicPositional::TopAndRight | IndicPositional::BottomAndRight | IndicPositional::TopAndBottomAndRight | IndicPositional::LeftAndRight | IndicPositional::TopAndLeftAndRight => Position::AfterPost,
		IndicPositional::Top => config.top_matra,
		_ => Position::AfterSub,
	}
}

fn is_modifier(character: char) -> bool {
	matches!(indic_syllabic(character), IndicSyllabic::Bindu | IndicSyllabic::Visarga | IndicSyllabic::ToneMark | IndicSyllabic::CantillationMark | IndicSyllabic::SyllableModifier | IndicSyllabic::GeminationMark)
}

/// Plan every syllable of an Indic run: its masks, its positions and its number, set on the buffer
/// while it still holds one glyph per character. `None` when the run is not in an Indic script.
pub fn plan(face: &Face<'_>, buffer: &mut Buffer, characters: &[char], script_tag: [u8; 4], language: [u8; 4]) -> Result<Option<Config>, Error> {
	let Some(config) = config(characters) else { return Ok(None) };
	// THE VIRAMA'S GLYPH, which the face is asked about. A face with no `cmap` has none to give - the
	// caller's glyphs came from somewhere else - and every consonant is then asked about as a base.
	let virama = match face.table(b"cmap")? {
		Some(_) => face.glyph_for(config.virama)?,
		None => None,
	};
	let mut probe = Probe::new(face, script_tag, language)?;
	let starts = syllable_starts(characters)?;
	let count = characters.len().min(buffer.len());
	for (number, start) in starts.iter().enumerate() {
		let end = starts.get(number + 1).copied().unwrap_or(count).min(count);
		if *start >= end {
			continue;
		}
		// NUMBERED FROM ONE, so zero still means "no syllable", and wrapping rather than saturating so
		// two neighbours never share a number.
		let syllable = (number % usize::from(u16::MAX)) as u16 + 1;
		plan_syllable(&config, &mut probe, virama, characters, buffer, *start, end, syllable)?;
	}
	Ok(Some(config))
}

#[allow(clippy::too_many_arguments)]
fn plan_syllable(config: &Config, probe: &mut Probe<'_>, virama: Option<u16>, characters: &[char], buffer: &mut Buffer, start: usize, end: usize, syllable: u16) -> Result<(), Error> {
	// THE REPH: a syllable-initial Ra and its virama, with a consonant still to come after them - or,
	// where the script writes it explicitly, a ZWJ - and a face whose `rphf` actually forms it. A Ra
	// the face has no reph for is an ordinary consonant, and a `र्` that ends a word is a dead Ra.
	let mut limit = start;
	let mut has_reph = false;
	if end - start >= 3 && config.ras.contains(&characters[start]) && is_virama(characters[start + 1]) {
		let pair = [buffer.infos[start].glyph, buffer.infos[start + 1].glyph];
		let third = characters[start + 2];
		has_reph = match config.reph {
			RephMode::Implicit => !is_joiner(third) && probe.would_substitute(*b"rphf", &pair)?,
			RephMode::Explicit => third == ZWJ && (probe.would_substitute(*b"rphf", &pair)? || probe.would_substitute(*b"rphf", &[pair[0], pair[1], buffer.infos[start + 2].glyph])?),
			RephMode::None => false,
		};
		if has_reph {
			limit = start + 2;
			while limit < end && is_joiner(characters[limit]) {
				limit += 1;
			}
		}
	}

	// THE BASE: from the end of the syllable backwards, the first consonant the face has no below-base
	// or post-base form for - a post-base form counting only until a below-base one has been seen,
	// because post-base forms follow below-base ones. A ZWJ after a virama stops the search: it asks
	// for the half form of what precedes it. With no such consonant, nothing is the base and every
	// consonant is drawn as a pre-base form.
	let mut base = if has_reph { start } else { end };
	let mut seen_below = false;
	let mut at = end;
	while at > limit {
		at -= 1;
		if is_consonant(characters[at]) {
			let form = consonant_form(probe, virama, buffer.infos[at].glyph)?;
			buffer.infos[at].position = form;
			base = at;
			if form != Position::BelowBase && (form != Position::PostBase || seen_below) {
				break;
			}
			if form == Position::BelowBase {
				seen_below = true;
			}
		} else if at > start && characters[at] == ZWJ && is_virama(characters[at - 1]) {
			break;
		}
	}
	// A REPH NEEDS A BASE AFTER IT. With no other consonant the Ra is the base and nothing floats.
	if has_reph && base == start && limit - start <= 2 {
		limit = start;
	}

	// THE POSITIONS. A virama, a nukta and a joiner are drawn with what they follow.
	let mut last = Position::PreBase;
	for at in start..end {
		let character = characters[at];
		let position = if at < limit {
			if at == start {
				Position::Reph
			} else if is_virama(character) {
				Position::RephVirama
			} else {
				Position::PreBase
			}
		} else if is_consonant(character) {
			if at < base {
				Position::PreBase
			} else if at == base {
				Position::Base
			} else {
				// The form the base search asked the face for.
				buffer.infos[at].position
			}
		} else if indic_syllabic(character) == IndicSyllabic::VowelDependent {
			matra_position(config, character)
		} else if is_modifier(character) {
			Position::Modifier
		} else {
			last
		};
		let info = &mut buffer.infos[at];
		info.position = position;
		info.syllable = syllable;
		if position != Position::Modifier {
			last = position;
		}
	}
	// A POST-BASE CONSONANT OWNS THE VIRAMA AND THE JOINERS BEFORE IT, which are what its form is made
	// from: a `pstf` ligature keeps the FIRST component's position, and that component is the virama.
	if base < end {
		let mut owner = base;
		for at in base + 1..end {
			let character = characters[at];
			if is_consonant(character) {
				let position = buffer.infos[at].position;
				for owned in owner + 1..at {
					if buffer.infos[owned].position < Position::Modifier {
						buffer.infos[owned].position = position;
					}
				}
				owner = at;
			} else if indic_syllabic(character) == IndicSyllabic::VowelDependent {
				owner = at;
			}
		}
	}

	// THE MASKS: the reph's characters for `rphf`, everything before the base for `half` (and `blwf`
	// where the script forms below-base consonants before its base too), everything after it for the
	// below-, above- and post-base forms. The base itself takes none of them.
	let before = HALF | if config.below_before_base { BLWF } else { 0 };
	for at in start..end {
		buffer.infos[at].mask |= if at < limit {
			RPHF
		} else if at < base {
			before
		} else if at > base {
			BLWF | ABVF | PSTF
		} else {
			0
		};
	}
	// A PRE-BASE-REORDERING FORM: the first virama and consonant after the base the face's `pref`
	// forms. Only that pair takes the mask, so the feature cannot reach anything else.
	if base < end {
		for at in base + 1..end.saturating_sub(1) {
			if probe.would_substitute(*b"pref", &[buffer.infos[at].glyph, buffer.infos[at + 1].glyph])? {
				buffer.infos[at].mask |= PREF;
				buffer.infos[at + 1].mask |= PREF;
				break;
			}
		}
	}
	// A ZWNJ ASKS FOR THE VISIBLE VIRAMA: no half form for the consonant before it.
	for at in start + 1..end {
		if characters[at] != ZWNJ {
			continue;
		}
		let mut back = at;
		loop {
			back -= 1;
			buffer.infos[back].mask &= !HALF;
			if back <= start || is_consonant(characters[back]) {
				break;
			}
		}
	}
	Ok(())
}

/// Whether the face draws a consonant below the base, after it, or as a base, asked of the face's own
/// lookups in both orders the fonts in use write them.
fn consonant_form(probe: &mut Probe<'_>, virama: Option<u16>, consonant: u16) -> Result<Position, Error> {
	let Some(virama) = virama else { return Ok(Position::Base) };
	for (feature, form) in [(*b"blwf", Position::BelowBase), (*b"vatu", Position::BelowBase), (*b"pstf", Position::PostBase), (*b"pref", Position::PostBase)] {
		if probe.would_substitute(feature, &[virama, consonant])? || probe.would_substitute(feature, &[consonant, virama])? {
			return Ok(form);
		}
	}
	Ok(Position::Base)
}

/// THE FINAL REORDERING, between the basic features and the presentation features: the reph moves to
/// where its script draws it, and a pre-base-reordering form to just before the base.
pub fn final_reorder(buffer: &mut Buffer, config: &Config) -> Result<(), Error> {
	let mut start = 0usize;
	while start < buffer.len() {
		let syllable = buffer.infos[start].syllable;
		let mut end = start + 1;
		while end < buffer.len() && buffer.infos[end].syllable == syllable {
			end += 1;
		}
		if syllable != 0 {
			reorder_syllable(buffer, config, start, end);
		}
		start = end;
	}
	Ok(())
}

fn reorder_syllable(buffer: &mut Buffer, config: &Config, start: usize, end: usize) {
	let position = |buffer: &Buffer, at: usize| buffer.infos[at].position;
	let mut base = (start..end).find(|at| position(buffer, *at) >= Position::Base).unwrap_or(end);

	// THE PRE-BASE-REORDERING FORM, and only one the feature formed: a pair the face left alone is
	// still two glyphs, the second still marked, and it stays where it was written.
	if base < end
		&& let Some(at) = (base + 1..end).find(|at| buffer.infos[*at].mask & PREF != 0)
		&& (at + 1 >= end || buffer.infos[at + 1].mask & PREF == 0)
	{
		move_glyph(buffer, at, base);
		base += 1;
	}

	// THE REPH, and only one the feature formed: the Ra's virama went into it, so no glyph of the
	// syllable still stands where that virama did.
	if position(buffer, start) != Position::Reph || (start + 1..end).any(|at| position(buffer, at) == Position::RephVirama) {
		return;
	}
	let last = end - 1;
	let target = match config.placement {
		Placement::AfterMain => {
			let mut at = base.min(last);
			while at < last && position(buffer, at + 1) <= Position::AfterMain {
				at += 1;
			}
			at
		}
		Placement::AfterSub => {
			let mut at = base.min(last);
			while at < last && !matches!(position(buffer, at + 1), Position::PostBase | Position::AfterPost | Position::Modifier) {
				at += 1;
			}
			at
		}
		Placement::End => {
			let mut at = last;
			while at > start && position(buffer, at) == Position::Modifier {
				at -= 1;
			}
			at
		}
	};
	move_glyph(buffer, start, target);
}

/// Move one glyph to another index, shifting what lies between - in both of the buffer's arrays.
fn move_glyph(buffer: &mut Buffer, from: usize, to: usize) {
	if from < to {
		buffer.infos[from..=to].rotate_left(1);
		buffer.positions[from..=to].rotate_left(1);
	} else if to < from {
		buffer.infos[to..=from].rotate_right(1);
		buffer.positions[to..=from].rotate_right(1);
	}
}
