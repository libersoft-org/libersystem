//! Tagging, group selection and the bounded planning half of the file operations.
//!
//! NOTHING HERE TOUCHES A VOLUME. A plan is a decision - which entries, in which order, to where,
//! and what to do when the destination already exists - and deciding is the half that has the
//! traps in it: a copy of a directory into its own subtree, a move onto itself, a total that
//! overflows. Keeping it apart from the doing is what lets every one of those be a host test
//! rather than a scenario.

extern crate alloc;

use alloc::vec::Vec;

/// The most entries one operation may cover, and the deepest a recursive plan will walk.
///
/// Both are refusals rather than truncations: an operation that quietly covered the first
/// thousand of two thousand tagged files would report success over work it did not do.
pub const MAX_OPERATION_ENTRIES: usize = 4096;
pub const MAX_OPERATION_DEPTH: usize = 64;

/// Whether `name` matches `pattern`, where `*` is any run and `?` is any one byte.
///
/// ITERATIVE WITH ONE BACKTRACK POINT, deliberately. The recursive form is the textbook one and it
/// is a way to crash a program by naming a file: forty asterisks against a four-kilobyte name is
/// the input. This is the same shape `cli`'s matcher takes, written here because the terminal
/// library may not depend on the command library - and a group-select that could be made to hang by
/// what somebody typed is worse than a duplicate of twenty lines.
///
/// Case is folded, because a group-select is somebody typing `*.RS` for the same files as `*.rs`.
pub fn glob_match(pattern: &[u8], name: &[u8]) -> bool {
	let (mut pattern_at, mut name_at) = (0usize, 0usize);
	let (mut star, mut resume) = (usize::MAX, 0usize);
	while name_at < name.len() {
		match pattern.get(pattern_at) {
			Some(b'*') => {
				star = pattern_at;
				resume = name_at;
				pattern_at += 1;
			}
			Some(b'?') => {
				pattern_at += 1;
				name_at += 1;
			}
			Some(byte) if byte.eq_ignore_ascii_case(&name[name_at]) => {
				pattern_at += 1;
				name_at += 1;
			}
			// The one backtrack: return to the last `*` and let it swallow one more byte. Without a
			// star to return to there is nothing left to try, so the answer is no.
			_ => {
				if star == usize::MAX {
					return false;
				}
				pattern_at = star + 1;
				resume += 1;
				name_at = resume;
			}
		}
	}
	while pattern.get(pattern_at) == Some(&b'*') {
		pattern_at += 1;
	}
	pattern_at == pattern.len()
}

/// The tagged set of a panel, as positions in its current view.
///
/// POSITIONS AND NOT NAMES, because a tag is a thing the reader made on the screen in front of
/// them - and it is cleared by anything that changes what the screen shows. A set that survived a
/// re-sort by remembering names would silently carry a selection across a listing the reader
/// re-arranged, which is how the wrong files get deleted.
#[derive(Default)]
pub struct Tags {
	rows: Vec<usize>,
}

impl Tags {
	pub fn new() -> Tags {
		Tags { rows: Vec::new() }
	}

	pub fn clear(&mut self) {
		self.rows.clear();
	}

	pub fn contains(&self, row: usize) -> bool {
		self.rows.binary_search(&row).is_ok()
	}

	pub fn len(&self) -> usize {
		self.rows.len()
	}

	pub fn is_empty(&self) -> bool {
		self.rows.is_empty()
	}

	pub fn rows(&self) -> &[usize] {
		&self.rows
	}

	/// Tag a row. False when the set is full or could not grow, so a tag that did not happen is
	/// visible to the caller rather than being reported as one that did.
	pub fn add(&mut self, row: usize) -> bool {
		match self.rows.binary_search(&row) {
			Ok(_) => true,
			Err(at) => {
				if self.rows.len() == MAX_OPERATION_ENTRIES || self.rows.try_reserve(1).is_err() {
					return false;
				}
				self.rows.insert(at, row);
				true
			}
		}
	}

	pub fn remove(&mut self, row: usize) -> bool {
		match self.rows.binary_search(&row) {
			Ok(at) => {
				self.rows.remove(at);
				true
			}
			Err(_) => false,
		}
	}

	/// Tag if untagged, untag if tagged. What `Insert` does.
	pub fn toggle(&mut self, row: usize) -> bool {
		if self.contains(row) { self.remove(row) } else { self.add(row) }
	}

	/// Tag every row whose name matches `pattern`; answers how many were newly tagged.
	pub fn select<'a>(&mut self, names: impl Iterator<Item = &'a [u8]>, pattern: &[u8]) -> usize {
		let mut added = 0;
		for (row, name) in names.enumerate() {
			if glob_match(pattern, name) && !self.contains(row) && self.add(row) {
				added += 1;
			}
		}
		added
	}

	/// Untag every row whose name matches `pattern`; answers how many were untagged.
	pub fn unselect<'a>(&mut self, names: impl Iterator<Item = &'a [u8]>, pattern: &[u8]) -> usize {
		let mut removed = 0;
		for (row, name) in names.enumerate() {
			if glob_match(pattern, name) && self.remove(row) {
				removed += 1;
			}
		}
		removed
	}

	/// Tag what is untagged and untag what is tagged, over `count` rows.
	pub fn invert(&mut self, count: usize) -> bool {
		let mut inverted: Vec<usize> = Vec::new();
		if inverted.try_reserve(count.saturating_sub(self.rows.len())).is_err() {
			return false;
		}
		for row in 0..count {
			if !self.contains(row) {
				inverted.push(row);
			}
		}
		self.rows = inverted;
		true
	}
}

/// What to do when a destination already exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Overwrite {
	/// Refuse the entry and carry on with the rest. The default, because the alternative silently
	/// destroys something the reader did not name.
	Skip,
	Replace,
	/// Replace only when the source is newer than the destination.
	Newer,
	/// Ask - which the planner records rather than answers, since a plan is made before anybody is
	/// in front of it.
	Ask,
}

/// What one operation is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
	Copy,
	Move,
	Delete,
}

/// Why a plan could not be made. Each is separate because each needs a different sentence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanError {
	/// Nothing was tagged and the cursor is on nothing.
	Empty,
	/// The destination is inside one of the sources - copying a directory into its own subtree
	/// never terminates, and the check is what stops it rather than a depth limit noticing later.
	DestinationInsideSource,
	/// The source and the destination are the same object.
	SameObject,
	/// Past `MAX_OPERATION_ENTRIES` or `MAX_OPERATION_DEPTH`.
	TooMany,
	OutOfMemory,
}

/// One source and where it is going. `destination` is empty for a delete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Step {
	pub source: Vec<u8>,
	pub destination: Vec<u8>,
	pub is_dir: bool,
	pub size: u64,
}

/// A complete, checked operation: what to do, to what, with what policy, and how much of it there
/// is. Nothing has happened yet.
#[derive(Debug, Eq, PartialEq)]
pub struct Plan {
	pub operation: Operation,
	pub overwrite: Overwrite,
	pub steps: Vec<Step>,
	pub total_bytes: u64,
	/// True when the total is a lower bound because a directory was not walked. Stated rather than
	/// hidden: a progress bar over a total nobody computed is a progress bar that lies.
	pub total_is_partial: bool,
}

/// One entry a plan is being made over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Source<'a> {
	pub path: &'a [u8],
	pub name: &'a [u8],
	pub is_dir: bool,
	pub size: u64,
}

/// Make a plan. `destination_dir` is the directory the entries are going into, and is ignored for
/// a delete.
///
/// THE CYCLE CHECK IS WHY THIS EXISTS. `cp -r a a/b` is a request that cannot be satisfied, and a
/// program that starts it discovers that by filling the volume. It is checked here, against every
/// source, before a single byte is read.
pub fn plan(operation: Operation, sources: &[Source], destination_dir: &[u8], overwrite: Overwrite) -> Result<Plan, PlanError> {
	if sources.is_empty() {
		return Err(PlanError::Empty);
	}
	if sources.len() > MAX_OPERATION_ENTRIES {
		return Err(PlanError::TooMany);
	}
	let mut steps: Vec<Step> = Vec::new();
	steps.try_reserve_exact(sources.len()).map_err(|_| PlanError::OutOfMemory)?;
	let mut total: u64 = 0;
	let mut partial = false;
	for source in sources {
		let destination = if operation == Operation::Delete {
			Vec::new()
		} else {
			if is_within(destination_dir, source.path) {
				return Err(PlanError::DestinationInsideSource);
			}
			let joined = join(destination_dir, source.name).ok_or(PlanError::OutOfMemory)?;
			if joined == source.path {
				return Err(PlanError::SameObject);
			}
			joined
		};
		// A DIRECTORY'S SIZE IS NOT ITS CONTENTS. The listing reports the entry's own size, which
		// for a directory says nothing about what is inside it - so the total is marked partial
		// rather than being quietly wrong by however much a subtree holds.
		if source.is_dir {
			partial = true;
		} else {
			total = total.saturating_add(source.size);
		}
		let mut path: Vec<u8> = Vec::new();
		path.try_reserve_exact(source.path.len()).map_err(|_| PlanError::OutOfMemory)?;
		path.extend_from_slice(source.path);
		steps.push(Step { source: path, destination, is_dir: source.is_dir, size: source.size });
	}
	Ok(Plan { operation, overwrite, steps, total_bytes: total, total_is_partial: partial })
}

/// Whether `inner` is `outer` or lies below it.
///
/// COMPARED AT A SEPARATOR, which is the whole of the correctness here: `vol://system/binary` is
/// not inside `vol://system/bin`, and a prefix test that did not require the boundary would say it
/// was - and refuse a copy that is perfectly legal, or worse, allow one that is not.
pub fn is_within(inner: &[u8], outer: &[u8]) -> bool {
	if inner == outer {
		return true;
	}
	let outer = outer.strip_suffix(b"/").unwrap_or(outer);
	inner.len() > outer.len() && inner.starts_with(outer) && inner[outer.len()] == b'/'
}

/// Join a directory and a name with exactly one separator.
pub fn join(directory: &[u8], name: &[u8]) -> Option<Vec<u8>> {
	let trimmed = directory.strip_suffix(b"/").unwrap_or(directory);
	let mut out: Vec<u8> = Vec::new();
	out.try_reserve_exact(trimmed.len() + 1 + name.len()).ok()?;
	out.extend_from_slice(trimmed);
	out.push(b'/');
	out.extend_from_slice(name);
	Some(out)
}

/// Whether an existing destination should be replaced under `policy`.
///
/// `Ask` answers false here and is the caller's business: a planner cannot ask, and answering yes
/// on its behalf would turn "ask me" into "do it".
pub fn should_replace(policy: Overwrite, source_mtime: u64, destination_mtime: u64) -> bool {
	match policy {
		Overwrite::Skip | Overwrite::Ask => false,
		Overwrite::Replace => true,
		Overwrite::Newer => source_mtime > destination_mtime,
	}
}

/// What a search is looking for. Every field is optional, and a criterion nobody set admits
/// everything - so a search with nothing set is a listing of the tree rather than an empty answer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Criteria<'a> {
	/// A glob over the NAME, not the path: a person searching for `*.rs` means the file names.
	pub name: Option<&'a [u8]>,
	pub directories_only: bool,
	pub files_only: bool,
	pub min_size: Option<u64>,
	pub max_size: Option<u64>,
	pub min_modified: Option<u64>,
	pub max_modified: Option<u64>,
	/// How deep below the root to look. `None` is the whole tree, bounded by
	/// `MAX_OPERATION_DEPTH`, which is what stops a link loop or a pathological tree.
	pub max_depth: Option<usize>,
}

impl<'a> Criteria<'a> {
	/// Whether an entry at `depth` is a result.
	///
	/// DEPTH IS CHECKED SEPARATELY FROM WHETHER TO DESCEND, and the difference matters: a directory
	/// past the depth limit is not a result AND is not walked, while a directory that fails the
	/// NAME test is not a result and IS walked - the files somebody is looking for live in
	/// directories whose names do not match.
	pub fn admits(&self, entry: &crate::panel::EntryKey, depth: usize) -> bool {
		if depth > self.max_depth.unwrap_or(MAX_OPERATION_DEPTH) {
			return false;
		}
		if self.directories_only && !entry.is_dir {
			return false;
		}
		if self.files_only && entry.is_dir {
			return false;
		}
		if let Some(pattern) = self.name {
			if !glob_match(pattern, entry.name) {
				return false;
			}
		}
		// SIZE AND TIME DO NOT APPLY TO A DIRECTORY. Its entry size is a filesystem detail rather
		// than an amount of content, so filtering directories by it answers a question about the
		// medium instead of about the tree.
		if !entry.is_dir {
			if self.min_size.is_some_and(|min| entry.size < min) || self.max_size.is_some_and(|max| entry.size > max) {
				return false;
			}
		}
		if self.min_modified.is_some_and(|min| entry.modified < min) || self.max_modified.is_some_and(|max| entry.modified > max) {
			return false;
		}
		true
	}

	/// Whether a directory at `depth` should be walked into.
	pub fn descends(&self, depth: usize) -> bool {
		depth < self.max_depth.unwrap_or(MAX_OPERATION_DEPTH).min(MAX_OPERATION_DEPTH)
	}
}

/// A bounded result set that becomes a temporary panel.
///
/// EACH RESULT KEEPS ITS REAL URI, which is the property the milestone names: viewing, editing,
/// copying or deleting a result acts on the file it came from, through the capability that reached
/// it - a result list of bare names would need the panel to guess which directory each came from.
#[derive(Default)]
pub struct Results {
	entries: Vec<Vec<u8>>,
	truncated: bool,
}

impl Results {
	pub fn new() -> Results {
		Results { entries: Vec::new(), truncated: false }
	}

	/// Record a hit. False when the set is full - and the set REMEMBERS that, because a result list
	/// that silently stopped growing looks exactly like a tree with nothing more in it.
	pub fn push(&mut self, uri: &[u8]) -> bool {
		if self.entries.len() == MAX_OPERATION_ENTRIES {
			self.truncated = true;
			return false;
		}
		let mut owned: Vec<u8> = Vec::new();
		if owned.try_reserve_exact(uri.len()).is_err() || self.entries.try_reserve(1).is_err() {
			self.truncated = true;
			return false;
		}
		owned.extend_from_slice(uri);
		self.entries.push(owned);
		true
	}

	pub fn get(&self, index: usize) -> Option<&[u8]> {
		self.entries.get(index).map(Vec::as_slice)
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	/// Whether the set stopped short of the answer.
	pub fn is_truncated(&self) -> bool {
		self.truncated
	}

	pub fn clear(&mut self) {
		self.entries.clear();
		self.truncated = false;
	}
}

/// The frontier of a lazy tree walk: which directories are still to be opened, and how deep each
/// one is.
///
/// ITERATIVE AND EXPLICIT, because a recursive walk over a tree somebody else made is a stack
/// overflow waiting for a deep enough directory - and the frontier is bounded in both directions,
/// so a tree wide enough to exhaust memory is refused by name rather than by the allocator.
#[derive(Default)]
pub struct Frontier {
	pending: Vec<(Vec<u8>, usize)>,
	refused: bool,
}

impl Frontier {
	pub fn new() -> Frontier {
		Frontier { pending: Vec::new(), refused: false }
	}

	/// Queue a directory to open later. False when the frontier is full, which is recorded.
	pub fn push(&mut self, path: &[u8], depth: usize) -> bool {
		if self.pending.len() == MAX_OPERATION_ENTRIES || depth > MAX_OPERATION_DEPTH {
			self.refused = true;
			return false;
		}
		let mut owned: Vec<u8> = Vec::new();
		if owned.try_reserve_exact(path.len()).is_err() || self.pending.try_reserve(1).is_err() {
			self.refused = true;
			return false;
		}
		owned.extend_from_slice(path);
		self.pending.push((owned, depth));
		true
	}

	/// Take the next directory to open. DEPTH-FIRST, taken from the end: a search reports what it
	/// finds near the first thing it opened rather than a breadth-first sweep that shows the
	/// shallowest matches from everywhere at once - which is what somebody watching results arrive
	/// is expecting to see.
	pub fn pop(&mut self) -> Option<(Vec<u8>, usize)> {
		self.pending.pop()
	}

	pub fn len(&self) -> usize {
		self.pending.len()
	}

	pub fn is_empty(&self) -> bool {
		self.pending.is_empty()
	}

	/// Whether anything was refused, so a walk can say it was incomplete rather than report a
	/// partial tree as the whole one.
	pub fn refused_anything(&self) -> bool {
		self.refused
	}
}
