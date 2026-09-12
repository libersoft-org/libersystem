//! UAX #14 line break opportunities: where a line may be broken to fit.
//!
//! THE ONLY ONE OF THE THREE ALGORITHMS THAT IS ADVISORY. A grapheme cluster boundary and a word
//! boundary are facts about the text; a line break opportunity is a PERMISSION, and which of them a
//! layout takes depends on the width it has. So this answers where a break is allowed, where one is
//! mandatory, and nowhere decides where a line actually ends - that is line layout's job.
//!
//! IMPLEMENTED AS THE DOCUMENT'S RULES IN THE DOCUMENT'S ORDER, first match wins. That is slower
//! than the pair table most implementations compile the rules into, and it is what makes this one
//! checkable: every rule is one branch, named, in the order UAX #14 numbers them, so a reader with
//! the document open can follow it - and the pair table is an optimisation that can be added later
//! against this as its oracle.
//!
//! NO TAILORING. UAX #14 permits language-specific tailoring and this does none: the default
//! algorithm is what the normative conformance file measures, and a tailoring is a decision with its
//! own gate rather than a difference nobody wrote down.

use unicode_tables::{EastAsianWidth, GeneralCategory, LineBreak, east_asian_width, general_category, is_extended_pictographic, line_break};

/// What a position between two characters permits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineBreakOpportunity {
	/// A line MUST end here - a paragraph separator.
	Mandatory,
	/// A line MAY end here.
	Allowed,
	/// A line may not end here.
	Prohibited,
}

/// The longest run this reads. A paragraph longer than this is bounded by the layer above; this is
/// where THIS function stops reading rather than allocating.
const MAX_ITEMS: usize = 8192;

/// Resolve a character's class as LB1 requires, before any other rule reads it.
///
/// LB1 IS NOT A LOOKUP, IT IS A DECISION. `AI`, `SG` and `XX` become `AL` because the algorithm has
/// no rules for them; `SA` becomes `CM` for a combining mark and `AL` otherwise, which is what makes
/// a South-East Asian syllable behave; and `CJ` becomes `NS`, which is the strict Japanese behaviour
/// the default algorithm takes.
fn resolved(character: char) -> LineBreak {
	match line_break(character) {
		LineBreak::AI | LineBreak::SG | LineBreak::XX => LineBreak::AL,
		LineBreak::SA => match general_category(character) {
			GeneralCategory::Mn | GeneralCategory::Mc => LineBreak::CM,
			_ => LineBreak::AL,
		},
		LineBreak::CJ => LineBreak::NS,
		other => other,
	}
}

/// One character as the rules see it, AFTER LB9 has folded combining marks into what they attach to.
///
/// A FOLDED MARK IS NOT AN ITEM. LB9 says `X (CM | ZWJ)* -> X`, so the marks are removed from the
/// sequence the later rules read rather than carried with a flag every rule has to remember to look
/// at - which is how an implementation ends up applying LB23 to a combining mark.
#[derive(Clone, Copy)]
struct Item {
	class: LineBreak,
	character: char,
	/// LB8a: the run this item stands for ended with a zero-width joiner, so nothing may break after
	/// it. Tracked because the joiner itself was folded away.
	ends_with_zwj: bool,
}

/// Is this character one of the east-asian widths LB19 and LB30 turn on?
fn east_asian(character: char) -> bool {
	matches!(east_asian_width(character), EastAsianWidth::F | EastAsianWidth::W | EastAsianWidth::H)
}

/// Every line break opportunity in `text`, as `(byte offset, opportunity)` pairs.
///
/// The offsets are positions BEFORE which a line may end. The end of text is always mandatory
/// (rule 0.3), and the start of text never breaks (rule 0.2).
pub fn line_break_opportunities(text: &str, into: &mut [(usize, LineBreakOpportunity)]) -> usize {
	// The raw text, then the same text with LB9's marks folded away. `folded[i]` is the index of the
	// item raw character `i` belongs to, and `is_fold[i]` says it was folded INTO that item rather
	// than being its start - which is the same thing as "no break here".
	let mut items: [Item; MAX_ITEMS] = [Item { class: LineBreak::XX, character: '\0', ends_with_zwj: false }; MAX_ITEMS];
	let mut folded: [usize; MAX_ITEMS] = [0; MAX_ITEMS];
	let mut is_fold: [bool; MAX_ITEMS] = [false; MAX_ITEMS];
	let mut raw_count = 0usize;
	let mut item_count = 0usize;
	for character in text.chars() {
		if raw_count == MAX_ITEMS {
			break;
		}
		let class = resolved(character);
		let mark = matches!(class, LineBreak::CM | LineBreak::ZWJ);
		// LB9: a mark attaches to what precedes it, unless that is a break or a space - and LB10
		// then makes an unattached one an ordinary letter.
		let attaches = mark && item_count > 0 && !matches!(items[item_count - 1].class, LineBreak::BK | LineBreak::CR | LineBreak::LF | LineBreak::NL | LineBreak::SP | LineBreak::ZW) && !is_start_of_line(&items, item_count);
		if attaches {
			folded[raw_count] = item_count - 1;
			is_fold[raw_count] = true;
			// RULE 8.1 IS ABOUT THE LAST CHARACTER OF THE RUN, not about whether one anywhere in it
			// was a joiner: `ZWJ CM` ends with the mark, so a break after it is allowed again. This
			// is an assignment rather than a set for exactly that reason.
			items[item_count - 1].ends_with_zwj = class == LineBreak::ZWJ;
		} else {
			// LB10 makes an UNATTACHED mark an ordinary letter - but rule 8.1 is numbered before
			// both, so a zero-width joiner still prohibits a break after itself even when it had
			// nothing to attach to. Losing that is what a leading joiner exposes, and nothing else
			// does: 22 conformance cases, every one of them a text beginning with one.
			let joiner = class == LineBreak::ZWJ;
			let class = if mark { LineBreak::AL } else { class };
			items[item_count] = Item { class, character, ends_with_zwj: joiner };
			folded[raw_count] = item_count;
			is_fold[raw_count] = false;
			item_count += 1;
		}
		raw_count += 1;
	}

	let mut written = 0usize;
	let mut push = |offset: usize, opportunity: LineBreakOpportunity, written: &mut usize| {
		if *written < into.len() {
			into[*written] = (offset, opportunity);
		}
		*written += 1;
	};
	let mut raw_offsets: [usize; MAX_ITEMS] = [0; MAX_ITEMS];
	for (index, (offset, _)) in text.char_indices().enumerate() {
		if index == MAX_ITEMS {
			break;
		}
		raw_offsets[index] = offset;
	}
	for index in 1..raw_count {
		// LB9: a folded mark is inside an item, and an item has no boundary inside it.
		if is_fold[index] {
			continue;
		}
		let at = folded[index];
		if at == 0 {
			continue;
		}
		let opportunity = between(&items[..item_count], at);
		if opportunity != LineBreakOpportunity::Prohibited {
			push(raw_offsets[index], opportunity, &mut written);
		}
	}
	if !text.is_empty() {
		// Rule 0.3: the end of text is a break, and it is mandatory.
		push(text.len(), LineBreakOpportunity::Mandatory, &mut written);
	}
	written
}

/// Rule 9.0's exception: nothing has been emitted yet, so a mark at the very start attaches to
/// nothing and LB10 makes it a letter.
fn is_start_of_line(_items: &[Item; MAX_ITEMS], item_count: usize) -> bool {
	item_count == 0
}

/// The rules, in the order and the numbering UAX #14 gives them, for the boundary before
/// `items[index]`. First match wins, which is what "a rule is invoked only when no lower-numbered
/// rule has applied" means.
fn between(items: &[Item], index: usize) -> LineBreakOpportunity {
	use LineBreak::*;
	use LineBreakOpportunity::*;

	let left = items[index - 1];
	let right = items[index];
	let before = left.class;
	let after = right.class;
	/// The class `distance` items to the left of the boundary, or `None` at the start of text.
	macro_rules! left_of {
		($distance:expr) => {
			if index > $distance { Some(items[index - 1 - $distance].class) } else { None }
		};
	}
	macro_rules! right_of {
		($distance:expr) => {
			items.get(index + $distance).map(|item| item.class)
		};
	}

	// 4.0: BK !
	if before == BK {
		return Mandatory;
	}
	// 5.01: CR x LF. 5.02 to 5.04: a mandatory break after any hard break.
	if before == CR && after == LF {
		return Prohibited;
	}
	if matches!(before, CR | LF | NL) {
		return Mandatory;
	}
	// 6.0: never before a hard break.
	if matches!(after, BK | CR | LF | NL) {
		return Prohibited;
	}
	// 7.01 and 7.02: never before a space or a zero-width space.
	if matches!(after, SP | ZW) {
		return Prohibited;
	}
	// 8.0: ZW SP* รท
	if let Some(start) = run_start_before(items, index, &[SP])
		&& start > 0
		&& items[start - 1].class == ZW
	{
		return Allowed;
	}
	if before == ZW {
		return Allowed;
	}
	// 8.1: ZWJ x - the joiner was folded into the item before, which is why the item remembers it.
	if left.ends_with_zwj {
		return Prohibited;
	}
	// 11.01 and 11.02: the word joiner, on both sides.
	if after == WJ || before == WJ {
		return Prohibited;
	}
	// 12.0: GL x
	if before == GL {
		return Prohibited;
	}
	// 12.1: [^SP BA HY HH] x GL
	if !matches!(before, SP | BA | HY | HH) && after == GL {
		return Prohibited;
	}
	// 13.01 to 13.04: never before a closing bracket, an exclamation or a symbol. `IS` is NOT here -
	// it is rule 15.4, and putting it here would make rule 15.3 unreachable.
	if matches!(after, EX | CL | CP | SY) {
		return Prohibited;
	}
	// 14.0: OP SP* x
	if let Some(start) = run_start_before(items, index, &[SP])
		&& start > 0
		&& items[start - 1].class == OP
	{
		return Prohibited;
	}
	if before == OP {
		return Prohibited;
	}
	// 15.11: an opening quotation after a break or an opener glues forward, across spaces.
	if let Some(start) = run_start_before(items, index, &[SP])
		&& start > 0
		&& is_pi_quotation(items[start - 1])
		&& (start == 1 || matches!(items[start - 2].class, BK | CR | LF | NL | OP | QU | GL | SP | ZW))
	{
		return Prohibited;
	}
	if is_pi_quotation(left) && (index == 1 || matches!(items[index - 2].class, BK | CR | LF | NL | OP | QU | GL | SP | ZW)) {
		return Prohibited;
	}
	// 15.21: a closing quotation glues back when what follows can end a line.
	if is_pf_quotation(right) && right_of!(1).is_none_or(|class| matches!(class, SP | GL | WJ | CL | QU | CP | EX | IS | SY | BK | CR | LF | NL | ZW)) {
		return Prohibited;
	}
	// 15.3: SP รท IS NU
	if before == SP && after == IS && right_of!(1) == Some(NU) {
		return Allowed;
	}
	// 15.4: x IS
	if after == IS {
		return Prohibited;
	}
	// 16.0: (CL | CP) SP* x NS
	if after == NS
		&& let Some(start) = run_start_before(items, index, &[SP])
		&& start > 0
		&& matches!(items[start - 1].class, CL | CP)
	{
		return Prohibited;
	}
	if matches!(before, CL | CP) && after == NS {
		return Prohibited;
	}
	// 17.0: B2 SP* x B2
	if after == B2 {
		if let Some(start) = run_start_before(items, index, &[SP])
			&& start > 0
			&& items[start - 1].class == B2
		{
			return Prohibited;
		}
		if before == B2 {
			return Prohibited;
		}
	}
	// 18.0: SP รท
	if before == SP {
		return Allowed;
	}
	// 19.01: x QU, for a quotation that is not an opening one.
	if after == QU && !is_pi_quotation(right) {
		return Prohibited;
	}
	// 19.02: QU x, for a quotation that is not a closing one.
	if before == QU && !is_pf_quotation(left) {
		return Prohibited;
	}
	// 19.1: [^EastAsian] x QU
	if after == QU && !east_asian(left.character) {
		return Prohibited;
	}
	// 19.11: x QU ( [^EastAsian] | eot )
	if after == QU && items.get(index + 1).is_none_or(|item| !east_asian(item.character)) {
		return Prohibited;
	}
	// 19.12: QU x [^EastAsian]
	if before == QU && !east_asian(right.character) {
		return Prohibited;
	}
	// 19.13: ( [^EastAsian] | sot ) QU x
	if before == QU && (index == 1 || !east_asian(items[index - 2].character)) {
		return Prohibited;
	}
	// 20.01 and 20.02: a contingent break breaks on both sides.
	if after == CB || before == CB {
		return Allowed;
	}
	// 20.1: a hyphen at the start of a line glues to the letter after it.
	if matches!(before, HY | HH) && matches!(after, AL | HL) && (index == 1 || matches!(items[index - 2].class, BK | CR | LF | NL | SP | ZW | CB | GL)) {
		return Prohibited;
	}
	// 21.01 to 21.05.
	if matches!(after, BA | HH | HY | NS) {
		return Prohibited;
	}
	if before == BB {
		return Prohibited;
	}
	// 21.1: HL (HY | HH) x [^HL]
	if index >= 2 && items[index - 2].class == HL && matches!(before, HY | HH) && after != HL {
		return Prohibited;
	}
	// 21.2: SY x HL
	if before == SY && after == HL {
		return Prohibited;
	}
	// 22.0: x IN
	if after == IN {
		return Prohibited;
	}
	// 23.02 and 23.03: letters and numbers.
	if matches!(before, AL | HL) && after == NU {
		return Prohibited;
	}
	if before == NU && matches!(after, AL | HL) {
		return Prohibited;
	}
	// 23.12 and 23.13: prefixes, ideographs and postfixes.
	if before == PR && matches!(after, ID | EB | EM) {
		return Prohibited;
	}
	if matches!(before, ID | EB | EM) && after == PO {
		return Prohibited;
	}
	// 24.02 and 24.03.
	if matches!(before, PR | PO) && matches!(after, AL | HL) {
		return Prohibited;
	}
	if matches!(before, AL | HL) && matches!(after, PR | PO) {
		return Prohibited;
	}
	// 25.01 to 25.15, the number rule, each part as the document writes it.
	if number_rule(items, index) {
		return Prohibited;
	}
	// 26.01 to 26.03: Hangul syllables.
	if before == JL && matches!(after, JL | JV | H2 | H3) {
		return Prohibited;
	}
	if matches!(before, JV | H2) && matches!(after, JV | JT) {
		return Prohibited;
	}
	if matches!(before, JT | H3) && after == JT {
		return Prohibited;
	}
	// 27.01 and 27.02: the affixes beside them.
	if matches!(before, JL | JV | JT | H2 | H3) && after == PO {
		return Prohibited;
	}
	if before == PR && matches!(after, JL | JV | JT | H2 | H3) {
		return Prohibited;
	}
	// 28.0: letters do not break from letters.
	if matches!(before, AL | HL) && matches!(after, AL | HL) {
		return Prohibited;
	}
	// 28.11 to 28.14: the Brahmic orthographic syllable. The dotted circle stands in for a base.
	let brahmic = |item: Item| matches!(item.class, AK | AS) || item.character == '\u{25CC}';
	if before == AP && brahmic(right) {
		return Prohibited;
	}
	if brahmic(left) && matches!(after, VF | VI) {
		return Prohibited;
	}
	if index >= 2 && brahmic(items[index - 2]) && before == VI && (matches!(after, AK) || right.character == '\u{25CC}') {
		return Prohibited;
	}
	if brahmic(left) && brahmic(right) && right_of!(1) == Some(VF) {
		return Prohibited;
	}
	// 29.0: IS x (AL | HL)
	if before == IS && matches!(after, AL | HL) {
		return Prohibited;
	}
	// 30.01 and 30.02: a letter or a number beside a NARROW bracket.
	if matches!(before, AL | HL | NU) && after == OP && !east_asian(right.character) {
		return Prohibited;
	}
	if before == CP && !east_asian(left.character) && matches!(after, AL | HL | NU) {
		return Prohibited;
	}
	// 30.11 to 30.13: regional indicators in pairs, counted from the start of their run.
	if before == RI && after == RI {
		let mut run = 0usize;
		let mut at = index - 1;
		loop {
			if items[at].class != RI {
				break;
			}
			run += 1;
			if at == 0 {
				break;
			}
			at -= 1;
		}
		return if run % 2 == 1 { Prohibited } else { Allowed };
	}
	// 30.21 and 30.22: an emoji base, or an unassigned pictographic, and its modifier.
	if before == EB && after == EM {
		return Prohibited;
	}
	if is_extended_pictographic(left.character) && general_category(left.character) == GeneralCategory::Cn && after == EM {
		return Prohibited;
	}
	// 999.0.
	let _ = left_of!(0);
	Allowed
}

/// A quotation that is an opening one - rule 15.11 and rule 19.01 turn on the distinction.
fn is_pi_quotation(item: Item) -> bool {
	item.class == LineBreak::QU && general_category(item.character) == GeneralCategory::Pi
}

/// A quotation that is a closing one.
fn is_pf_quotation(item: Item) -> bool {
	item.class == LineBreak::QU && general_category(item.character) == GeneralCategory::Pf
}

/// The index of the first item of the run of `classes` immediately before `index`, or `None` when
/// the item before `index` is not one of them.
///
/// THE "SP*" IN HALF THE RULES. `OP SP* x` and `ZW SP* รท` are written over a RUN rather than a pair,
/// and a rule that looked only at the character before the boundary would apply to `OP SP` and not
/// to `OP SP SP`.
fn run_start_before(items: &[Item], index: usize, classes: &[LineBreak]) -> Option<usize> {
	let mut start = index;
	while start > 0 && classes.contains(&items[start - 1].class) {
		start -= 1;
	}
	if start == index { None } else { Some(start) }
}

/// Rule 25, all fifteen parts, each written as the document writes it.
///
/// AS FIFTEEN SEPARATE CONDITIONS RATHER THAN AS ONE CLEVER SCAN. The rule is a set of shapes over a
/// run - a currency prefix, digits with separators inside, a closing bracket, a postfix - and every
/// implementation that collapses it into one loop gets a different subset of them wrong. Fifteen
/// conditions can be read against fifteen lines of the document.
fn number_rule(items: &[Item], index: usize) -> bool {
	use LineBreak::*;
	let before = items[index - 1].class;
	let after = items[index].class;
	let at = |offset: usize| items.get(offset).map(|item| item.class);

	// The start of a `NU (SY | IS)*` run ending just before the boundary, if there is one.
	let digits_run_start = |end: usize| -> Option<usize> {
		let mut start = end;
		while start > 0 && matches!(items[start - 1].class, SY | IS) {
			start -= 1;
		}
		if start > 0 && items[start - 1].class == NU { Some(start - 1) } else { None }
	};

	// 25.01 to 25.04: NU (SY | IS)* (CL | CP) x (PO | PR)
	if matches!(after, PO | PR) && matches!(before, CL | CP) && digits_run_start(index - 1).is_some() {
		return true;
	}
	// 25.05 and 25.06: NU (SY | IS)* x (PO | PR)
	if matches!(after, PO | PR) && digits_run_start(index).is_some() {
		return true;
	}
	// 25.07 and 25.10: (PO | PR) x OP NU
	if matches!(before, PO | PR) && after == OP && at(index + 1) == Some(NU) {
		return true;
	}
	// 25.08 and 25.11: (PO | PR) x OP IS NU
	if matches!(before, PO | PR) && after == OP && at(index + 1) == Some(IS) && at(index + 2) == Some(NU) {
		return true;
	}
	// 25.09 and 25.12: (PO | PR) x NU
	if matches!(before, PO | PR) && after == NU {
		return true;
	}
	// 25.13 and 25.14: (HY | IS) x NU
	if matches!(before, HY | IS) && after == NU {
		return true;
	}
	// 25.15: NU (SY | IS)* x NU
	if after == NU && digits_run_start(index).is_some() {
		return true;
	}
	false
}
