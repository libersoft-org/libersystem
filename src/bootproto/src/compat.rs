// Provider compatibility: whether a candidate library may replace an installed provider
// in a running system, or whether the change needs a cold restart.
//
// The point of writing this down is that hot replacement must never be a judgement call.
// A running process has already resolved the installed provider's symbols; swapping an
// image underneath it is safe only when every resolution that has already happened would
// still resolve the same way, and when nothing about how the image was produced has moved.
// Both halves are decidable from the two images alone, so they are decided here rather
// than argued about per publication.
//
// A candidate is compatible when all of the following hold:
//
//   1. Both images declare the same machine, both parse, and both carry a well-formed
//      identity record. The machine is read before parsing, so a perfectly valid image for
//      another architecture is refused as a machine mismatch rather than as an unreadable
//      file - which also lets this rule be applied on any host to images for any target,
//      since it is a statement about two images and not about the machine running it.
//   2. Every identity field except the content digest is identical. That covers the target,
//      the build profile, the codegen flags, the feature set and the toolchain: a change in
//      any of them produces different code under the same name, which is exactly what a
//      running process cannot be handed.
//   3. The declared provider list is identical in content AND in order. The list is the
//      resolved dependency closure in canonical order, so an added, removed or reordered
//      entry means the candidate was linked against a different graph.
//   4. The image's own DT_NEEDED entries are identical in content and order, which is the
//      cross-check that the ELF matches what its identity record claims.
//   5. The candidate's exported dynamic symbols are a superset of the installed provider's,
//      and no symbol carried over changes kind, binding, size or visibility. New exports are
//      admissible - nothing has resolved against them yet. A removed or narrowed one is not:
//      something already resolved against it.
//
// Anything else is incompatible and requires the cold path. Every rejection names the field
// that decided it, and for a symbol rejection the symbol, so a refusal is explainable rather
// than merely final.
//
// The content digest is the one identity field allowed to differ, because it is the whole
// point: a candidate with the same digest is the same image and there is nothing to publish.

use crate::elf::{Elf, Symbol};

// The identity record's own name for the digest of the sources the image was built from.
// It is the only field a candidate may change.
pub const CONTENT_DIGEST_FIELD: &str = "source-sha256";

// Every identity field that must match, in the order they are checked. `format` is first so
// a record this rule does not understand is rejected as a field mismatch rather than
// silently compared field by field against a layout that may not mean the same thing.
pub const REQUIRED_FIELDS: [&str; 9] = ["format", "kind", "artifact", "package", "rustc-commit", "target", "profile", "rustflags", "features"];

// The identity record layout this rule understands.
pub const IDENTITY_FORMAT: &str = "liber-image-identity-v1";

// How many candidate symbols the export comparison may visit in total before giving up.
//
// THE BOUND THE WALK DID NOT HAVE. Matching each installed export against the candidate's is a
// forward walk that restarts and rescans the whole table whenever the orders diverge. A rebuild
// emits its symbols in the same order with new ones appended, so in practice the walk is one pass -
// but ELF does not require that order, and a candidate whose exports are REVERSED reaches the
// rescan for every symbol. That is quadratic in a table the format allows to hold 65,536 entries,
// and `compat::decide` runs synchronously inside a core service while a development artifact of up
// to 32 MB is being published, so the cost lands on the control plane.
//
// A budget rather than an index, because this crate is `no_std` with no allocator at all: there is
// nowhere to build the sorted table the obvious fix wants. The number is chosen so both legitimate
// shapes stay exact - the largest library in this tree has 672 exports, whose worst case is about
// 451,000 visits, and a 65,536-symbol table in ordinary order costs one pass - while the
// adversarial combination of the two is refused rather than run.
//
// Measured, in the guest, on that 672-export library: the ordinary walk is ~8 ms and rescanning per
// symbol is ~810 ms. The budget's job is the case neither of those numbers covers.
const MAX_SYMBOL_VISITS: u64 = 1 << 22;

// ELF symbol binding and visibility values this rule reasons about.
const BINDING_LOCAL: u8 = 0;
const VISIBILITY_HIDDEN: u8 = 2;
const VISIBILITY_INTERNAL: u8 = 1;

// Why a candidate was refused. Every variant carries what decided it; the borrowed strings
// point into the images, so naming a symbol or a field costs no allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason<'a> {
	// One of the two images is not a readable ELF.
	NotAnElf { installed: bool },
	// The two images are built for different machines, so nothing below is worth comparing.
	MachineMismatch { installed: u16, candidate: u16 },
	// One of the two images carries no identity record, or one this rule cannot read.
	MissingIdentity { installed: bool },
	// The record is not the layout this rule understands.
	UnknownIdentityFormat { format: &'a str },
	// A required identity field is absent from one of the records.
	MissingField { field: &'static str, installed: bool },
	// A required identity field differs.
	IdentityField { field: &'static str, installed: &'a str, candidate: &'a str },
	// The declared provider closure differs at this position; `None` means the list ended.
	ProviderList { position: usize, installed: Option<&'a str>, candidate: Option<&'a str> },
	// The image's own DT_NEEDED entries differ at this position.
	NeededList { position: usize, installed: Option<&'a str>, candidate: Option<&'a str> },
	// The image's dynamic table is unreadable, so its exports cannot be compared.
	UnreadableDynamic { installed: bool },
	// An export the installed provider had is gone.
	ExportRemoved { symbol: &'a str },
	// An export survived but changed in a way an already-resolved reference would notice.
	ExportChanged { symbol: &'a str, field: &'static str },
	// Deciding the export comparison would cost more work than this rule is willing to spend.
	//
	// FAILS CLOSED, which is what makes a budget safe to have: the answer is "use the cold path",
	// never "assume compatible". See `MAX_SYMBOL_VISITS`.
	TooComplex { visits: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict<'a> {
	// The candidate may replace the installed provider without a restart.
	Compatible,
	// It may not, for this reason.
	Incompatible(Reason<'a>),
}

impl Verdict<'_> {
	pub fn is_compatible(&self) -> bool {
		matches!(self, Verdict::Compatible)
	}
}

// One parsed identity record. It is kept as borrowed text and read field by field rather
// than destructured up front: the record is a small, line-oriented format, and scanning it
// on demand costs nothing while keeping this type free of a fixed field list that would
// have to grow every time the build system records one more thing.
#[derive(Clone, Copy, Debug)]
pub struct Identity<'a> {
	text: &'a str,
}

impl<'a> Identity<'a> {
	// Read the identity record out of an image. Returns None when the image carries none, or
	// one that is not valid UTF-8.
	pub fn read(image: &Elf<'a>) -> Option<Identity<'a>> {
		let record = image.liber_identity_note()?;
		Some(Identity { text: core::str::from_utf8(record).ok()? })
	}

	pub fn as_str(&self) -> &'a str {
		self.text
	}

	// The value of the first line with this key, or None when the record has no such line.
	pub fn field(&self, key: &str) -> Option<&'a str> {
		self.text.lines().find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
	}

	// The declared provider entries, in the record's order, WITH their identity digests. The order
	// is the canonical provider order, so it is part of what is compared and not incidental.
	//
	// THE DIGESTS USED TO BE CUT OFF HERE, and that made the comparison in `decide` unsound. A
	// record writes each provider as `provider=<name>:<identity-digest>`; this returned only the
	// name. Two images built against DIFFERENT versions of the same provider - which is exactly what
	// a digest exists to distinguish - therefore compared equal and were reported safe to
	// hot-replace, against a dependency closure that is not the installed one. The process loader
	// has always parsed and compared these digests, so the two components implemented different
	// identity languages, and a publication this rule called compatible could then fail to launch.
	//
	// Keeping the whole entry rather than splitting into a `(name, digest)` pair is deliberate: the
	// comparison is an equality over an ordered list, and `Reason::ProviderList` carries the entry
	// as text for the diagnostic, so splitting would only mean rejoining it there.
	pub fn providers(&self) -> impl Iterator<Item = &'a str> {
		self.text.lines().filter_map(|line| line.strip_prefix("provider="))
	}

	// The provider NAMES alone, for a caller that wants the identity of a dependency rather than
	// the identity of the build of it. Not used by the compatibility rule, which needs both halves.
	pub fn provider_names(&self) -> impl Iterator<Item = &'a str> {
		self.providers().map(|entry| match entry.find(':') {
			Some(at) => &entry[..at],
			None => entry,
		})
	}
}

// Decide whether `candidate` may hot-replace `installed`. Both are complete image bytes.
pub fn decide<'a>(installed: &'a [u8], candidate: &'a [u8]) -> Verdict<'a> {
	let (installed_machine, candidate_machine) = match (declared_machine(installed), declared_machine(candidate)) {
		(Some(l), Some(r)) => (l, r),
		(None, _) => return Verdict::Incompatible(Reason::NotAnElf { installed: true }),
		(_, None) => return Verdict::Incompatible(Reason::NotAnElf { installed: false }),
	};
	if installed_machine != candidate_machine {
		return Verdict::Incompatible(Reason::MachineMismatch { installed: installed_machine, candidate: candidate_machine });
	}
	let installed_elf = match Elf::parse_for_machine(installed, installed_machine) {
		Some(elf) => elf,
		None => return Verdict::Incompatible(Reason::NotAnElf { installed: true }),
	};
	let candidate_elf = match Elf::parse_for_machine(candidate, candidate_machine) {
		Some(elf) => elf,
		None => return Verdict::Incompatible(Reason::NotAnElf { installed: false }),
	};
	let installed_id = match Identity::read(&installed_elf) {
		Some(id) => id,
		None => return Verdict::Incompatible(Reason::MissingIdentity { installed: true }),
	};
	let candidate_id = match Identity::read(&candidate_elf) {
		Some(id) => id,
		None => return Verdict::Incompatible(Reason::MissingIdentity { installed: false }),
	};
	if let Verdict::Incompatible(reason) = compare_identity(&installed_id, &candidate_id) {
		return Verdict::Incompatible(reason);
	}
	if let Verdict::Incompatible(reason) = compare_lists(installed_id.providers(), candidate_id.providers(), true) {
		return Verdict::Incompatible(reason);
	}
	compare_exports(&installed_elf, &candidate_elf)
}

// The machine an image declares, read straight from the header so it can be compared before
// either image is parsed. None when the bytes are not a 64-bit little-endian ELF at all.
// Public because a publication needs the same distinction this rule needs: an image for
// another target is a wrong-target rejection, not an unreadable file.
pub fn declared_machine(bytes: &[u8]) -> Option<u16> {
	// A PRELIMINARY CLASSIFIER, NOT A VALIDATOR - it exists so a wrong-target image can be named as
	// such before either side is parsed, and `Elf::parse_for_machine` is what establishes validity.
	// It checks the identification version anyway, because reading `e_machine` out of a file that
	// never declared this layout is reading a field at an offset the file did not promise.
	if bytes.len() < 20 || bytes[..4] != [0x7f, b'E', b'L', b'F'] || bytes[4] != 2 || bytes[5] != 1 || bytes[6] != 1 {
		return None;
	}
	Some(u16::from_le_bytes([bytes[18], bytes[19]]))
}

// Rule 2: every required identity field matches, and the record is the layout this rule
// understands. The content digest is skipped by construction - it is not in REQUIRED_FIELDS.
fn compare_identity<'a>(installed: &Identity<'a>, candidate: &Identity<'a>) -> Verdict<'a> {
	for field in REQUIRED_FIELDS {
		let (left, right) = match (installed.field(field), candidate.field(field)) {
			(Some(l), Some(r)) => (l, r),
			(None, _) => return Verdict::Incompatible(Reason::MissingField { field, installed: true }),
			(_, None) => return Verdict::Incompatible(Reason::MissingField { field, installed: false }),
		};
		if left != right {
			return Verdict::Incompatible(Reason::IdentityField { field, installed: left, candidate: right });
		}
		if field == "format" && left != IDENTITY_FORMAT {
			return Verdict::Incompatible(Reason::UnknownIdentityFormat { format: left });
		}
	}
	Verdict::Compatible
}

// Rules 3 and 4: two ordered name lists must be identical, and the first position that is
// not identical is what is reported.
fn compare_lists<'a>(installed: impl Iterator<Item = &'a str>, candidate: impl Iterator<Item = &'a str>, providers: bool) -> Verdict<'a> {
	let mut left = installed;
	let mut right = candidate;
	let mut position: usize = 0;
	loop {
		let (l, r) = (left.next(), right.next());
		if l == r {
			if l.is_none() {
				return Verdict::Compatible;
			}
			position += 1;
			continue;
		}
		return Verdict::Incompatible(if providers { Reason::ProviderList { position, installed: l, candidate: r } } else { Reason::NeededList { position, installed: l, candidate: r } });
	}
}

// Rules 4 and 5: the candidate's DT_NEEDED list matches, and its exports are a superset of
// the installed provider's with every retained export unchanged.
fn compare_exports<'a>(installed: &Elf<'a>, candidate: &Elf<'a>) -> Verdict<'a> {
	let installed_info = match installed.dynamic_info() {
		Some(Some(info)) => info,
		_ => return Verdict::Incompatible(Reason::UnreadableDynamic { installed: true }),
	};
	let candidate_info = match candidate.dynamic_info() {
		Some(Some(info)) => info,
		_ => return Verdict::Incompatible(Reason::UnreadableDynamic { installed: false }),
	};
	let (installed_needed, candidate_needed) = match (installed.needed_names(&installed_info), candidate.needed_names(&candidate_info)) {
		(Some(l), Some(r)) => (l, r),
		(None, _) => return Verdict::Incompatible(Reason::UnreadableDynamic { installed: true }),
		(_, None) => return Verdict::Incompatible(Reason::UnreadableDynamic { installed: false }),
	};
	if let Verdict::Incompatible(reason) = compare_lists(installed_needed, candidate_needed, false) {
		return Verdict::Incompatible(reason);
	}
	let installed_symbols = match installed.symbols(&installed_info) {
		Some(symbols) => symbols,
		None => return Verdict::Incompatible(Reason::UnreadableDynamic { installed: true }),
	};
	// Walk the candidate's exports alongside the installed provider's rather than rescanning
	// them per symbol. A rebuild emits its dynamic symbols in the same order as the build
	// before it, with new exports appended, so the match is nearly always the next one the
	// walk yields and the whole comparison costs one pass. When the orders do diverge the
	// walk is restarted and the table scanned in full for that symbol, which keeps the answer
	// exact - the cost of a reordered table, not of every table. Measured in the guest on the
	// largest library in the tree (672 exports), rescanning per symbol cost ~810 ms and this
	// costs ~8 ms.
	let mut walk = match candidate.symbols(&candidate_info) {
		Some(symbols) => symbols,
		None => return Verdict::Incompatible(Reason::UnreadableDynamic { installed: false }),
	};
	// Every candidate symbol the walk looks at, across the whole comparison. See `MAX_SYMBOL_VISITS`:
	// the walk is one pass when the orders agree and a full rescan per symbol when they do not, and
	// nothing bounded the second case.
	let mut visits: u64 = 0;
	let mut budget = |seen: usize, visits: &mut u64| -> bool {
		*visits = visits.saturating_add(seen as u64);
		*visits <= MAX_SYMBOL_VISITS
	};
	for (symbol, name) in installed_symbols {
		if !is_export(&symbol, name) {
			continue;
		}
		let mut seen = 0usize;
		let mut found = walk.by_ref().inspect(|_| seen += 1).find(|(s, n)| is_export(s, n) && *n == name);
		if !budget(seen, &mut visits) {
			return Verdict::Incompatible(Reason::TooComplex { visits });
		}
		if found.is_none() {
			// The walk ran out before this name: either the orders diverged or the export is
			// gone. Restart it and scan the whole table before concluding the latter.
			walk = match candidate.symbols(&candidate_info) {
				Some(symbols) => symbols,
				None => return Verdict::Incompatible(Reason::UnreadableDynamic { installed: false }),
			};
			let mut rescan = 0usize;
			found = walk.by_ref().inspect(|_| rescan += 1).find(|(s, n)| is_export(s, n) && *n == name);
			if !budget(rescan, &mut visits) {
				return Verdict::Incompatible(Reason::TooComplex { visits });
			}
		}
		let (other, _) = match found {
			Some(pair) => pair,
			None => return Verdict::Incompatible(Reason::ExportRemoved { symbol: name }),
		};
		// Each of these would be noticed by a reference that has already resolved: a call
		// through a symbol whose type changed, a weak binding that hardened, an object whose
		// size no longer covers what a copy relocation copied, an export that narrowed.
		if let Some(field) = changed_field(&symbol, &other) {
			return Verdict::Incompatible(Reason::ExportChanged { symbol: name, field });
		}
	}
	Verdict::Compatible
}

// Which attribute of a retained export moved, or None when none did.
fn changed_field(installed: &Symbol, candidate: &Symbol) -> Option<&'static str> {
	if installed.symbol_type() != candidate.symbol_type() {
		return Some("kind");
	}
	if installed.binding() != candidate.binding() {
		return Some("binding");
	}
	if installed.size != candidate.size {
		return Some("size");
	}
	if installed.visibility() != candidate.visibility() {
		return Some("visibility");
	}
	None
}

// Whether a dynamic symbol is something another image could have resolved against: defined
// here, not local, and not hidden or internal. Undefined entries are this image's own
// imports and say nothing about what it offers.
fn is_export(symbol: &Symbol, name: &str) -> bool {
	!name.is_empty() && symbol.is_defined() && symbol.binding() != BINDING_LOCAL && symbol.visibility() != VISIBILITY_HIDDEN && symbol.visibility() != VISIBILITY_INTERNAL
}

#[cfg(test)]
#[path = "compat/tests.rs"]
mod tests;
