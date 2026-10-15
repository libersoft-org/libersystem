//! The canonical-equivalence policy: PRESERVATION.
//!
//! THE BUFFER IS NEVER REWRITTEN. No NFC, no NFD, no NFKC. Every offset this pipeline reports -
//! cluster starts, caret positions, selection boundaries - is a byte offset into the ORIGINAL UTF-8
//! the caller passed. That is the load-bearing half of the rule: normalising the buffer and then
//! reporting offsets into the normalised copy is how a text engine returns caret positions that do
//! not exist in the caller's string, and no mapping back survives a decomposition that changes
//! length.
//!
//! AND EQUIVALENCE IS RESOLVED BEFORE FALLBACK, NOT INSIDE SHAPING. Atomicity - a composition and
//! its decomposition being one grapheme cluster - keeps a sequence together; it does not make two
//! spellings COVER the same. A face may have a glyph for precomposed U+00E9 and no combining acute,
//! or the reverse, so fallback would already have chosen different faces for the two spellings by
//! the time shaping could have resolved anything.
//!
//! SO COVERAGE IS ASKED IN A CANONICAL VIEW: a face covers a cluster when it covers the cluster's
//! canonical COMPOSITION or its full DECOMPOSITION. The view is a lookup over the cluster, not a
//! rewrite of the buffer.

use unicode_tables::{canonical_composition, canonical_decomposition, combining_class};

/// How many characters one cluster's canonical view will hold. A cluster whose decomposition is
/// longer than this is asked about AS IT STANDS rather than expanded - a coverage question answered
/// about the caller's own characters, never about a truncated spelling that would leave some of them
/// unasked.
pub const MAX_VIEW: usize = 64;

/// One spelling of a cluster, held inline: asking about a cluster allocates nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Spelling {
	characters: [char; MAX_VIEW],
	len: usize,
}

impl Spelling {
	const EMPTY: Self = Self { characters: ['\0'; MAX_VIEW], len: 0 };

	fn as_slice(&self) -> &[char] {
		&self.characters[..self.len]
	}

	/// Append one character, or say the spelling is full.
	fn push(&mut self, character: char) -> bool {
		if self.len == MAX_VIEW {
			return false;
		}
		self.characters[self.len] = character;
		self.len += 1;
		true
	}
}

/// The two spellings of one cluster, without touching the buffer either came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CanonicalView {
	composed: Spelling,
	decomposed: Spelling,
}

impl CanonicalView {
	/// Both spellings of a cluster, or `None` for one whose decomposition is longer than `MAX_VIEW`.
	pub fn of(cluster: &[char]) -> Option<Self> {
		let decomposed = decompose(cluster)?;
		let composed = compose(&decomposed);
		Some(Self { composed, decomposed })
	}

	/// The fully composed spelling.
	pub fn composed(&self) -> &[char] {
		self.composed.as_slice()
	}

	/// The fully decomposed spelling, with its marks in canonical order.
	pub fn decomposed(&self) -> &[char] {
		self.decomposed.as_slice()
	}
}

/// The full canonical decomposition, with the marks put in canonical order.
///
/// THE ORDERING IS NOT DECORATION. Two marks on one base may be written in either order and are the
/// same string; a view that did not sort them would answer "these two clusters differ" for two
/// spellings Unicode calls identical, and the fallback decision would then differ too.
fn decompose(cluster: &[char]) -> Option<Spelling> {
	let mut out = Spelling::EMPTY;
	for character in cluster {
		if !expand(*character, &mut out, 0) {
			return None;
		}
	}
	// The canonical ordering: a stable sort of each run of non-starters by combining class.
	let characters = &mut out.characters[..out.len];
	let mut start = 0usize;
	while start < characters.len() {
		if combining_class(characters[start]) == 0 {
			start += 1;
			continue;
		}
		let mut end = start;
		while end < characters.len() && combining_class(characters[end]) != 0 {
			end += 1;
		}
		characters[start..end].sort_by_key(|character| combining_class(*character));
		start = end;
	}
	Some(out)
}

/// One character's decomposition, recursively - the recursion the table deliberately does not do.
/// `false` when the spelling would not fit.
fn expand(character: char, out: &mut Spelling, depth: u8) -> bool {
	// The standard's own decompositions nest at most a few deep; the bound is here because this
	// reads a generated table and a table is a thing that can be regenerated wrongly.
	if depth > 8 {
		return out.push(character);
	}
	match canonical_decomposition(character) {
		Some((first, second)) => expand(first, out, depth + 1) && second.is_none_or(|second| expand(second, out, depth + 1)),
		None => out.push(character),
	}
}

/// The composed spelling of an already-decomposed cluster.
///
/// THE RULE IS THE STANDARD'S: a starter composes with a following mark when nothing between them
/// blocks it - a mark of the same or a higher combining class does. Composing past a blocker is how
/// an implementation produces a spelling that is not canonically equivalent to what it was given.
fn compose(decomposed: &Spelling) -> Spelling {
	let mut out = Spelling::EMPTY;
	let mut starter: Option<usize> = None;
	let mut last_class: u8 = 0;
	for character in decomposed.as_slice() {
		let class = combining_class(*character);
		if let Some(at) = starter
			&& (last_class < class || last_class == 0 && class == 0)
			&& let Some(composed) = canonical_composition(out.characters[at], *character)
		{
			out.characters[at] = composed;
			// The class does not advance: what just composed is gone, and the next mark is measured
			// against what was before it.
			continue;
		}
		if class == 0 {
			starter = Some(out.len);
			last_class = 0;
		} else {
			last_class = class;
		}
		// A composition is never longer than the decomposition it came from, so this always fits.
		out.push(*character);
	}
	out
}

/// What a set of faces answers about a cluster.
///
/// A TRAIT RATHER THAN A TYPE, because the pipeline does not own the faces: the catalogue does, and
/// which faces a run may use is a policy above this layer. What the pipeline owns is the QUESTION -
/// and that it is asked in the canonical view rather than of the raw bytes.
///
/// IT DOES NOT SAY WHICH FACES TO TRY, OR IN WHICH ORDER. That is the fallback policy's, per run
/// script and language; an order the coverage provider enumerated would be the accident of
/// enumeration the policy exists to replace.
pub trait Coverage {
	/// Does this face cover every character of the spelling given?
	fn covers(&self, face: u16, spelling: &[char]) -> bool;
}

/// The first face of `order` that covers a cluster, asked in the canonical view.
///
/// BOTH SPELLINGS, AND A FACE THAT COVERS EITHER COVERS THE CLUSTER. This is the whole of the policy
/// in one function: ask about the composition, ask about the decomposition, and take the first face
/// that answers to either - so the two spellings of one string take the SAME fallback decision. A
/// cluster too long for the view is asked about as it stands.
pub fn face_for(coverage: &impl Coverage, order: impl IntoIterator<Item = u16>, cluster: &[char]) -> Option<u16> {
	let view = CanonicalView::of(cluster);
	order.into_iter().find(|face| match &view {
		Some(view) => coverage.covers(*face, view.composed()) || coverage.covers(*face, view.decomposed()),
		None => coverage.covers(*face, cluster),
	})
}
