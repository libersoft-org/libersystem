//! Bounded per-panel presentation state: ordering, filtering, quick search, history, bookmarks.
//!
//! NONE OF IT IS AUTHORITY, which is the reason it can live here at all. A sort key, a filter, a
//! visited directory and a bookmark are all ways of LOOKING at what a capability already reaches -
//! so this module holds no client, opens nothing, and a saved path is a suggestion the next launch
//! re-checks rather than a right the panel carries forward.

extern crate alloc;

use alloc::vec::Vec;

/// The most directories one panel remembers going back through, and the most bookmarks it keeps.
/// Small on purpose: a history is a way back to where you just were, and one that remembers a
/// thousand places is one nobody navigates.
pub const MAX_HISTORY: usize = 64;
pub const MAX_BOOKMARKS: usize = 32;

/// What a listing is ordered by.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortKey {
	Name,
	/// The bytes after the LAST dot, with a name that has none sorting before every name that has
	/// one - so the extensionless files group together instead of being scattered by their names.
	Extension,
	Size,
	Modified,
	Type,
}

/// One entry as the ordering and the filter see it. A view rather than the listing's own type, so
/// this module needs no storage vocabulary and can be tested without one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryKey<'a> {
	pub name: &'a [u8],
	pub size: u64,
	pub modified: u64,
	pub is_dir: bool,
}

impl<'a> EntryKey<'a> {
	/// The extension, or an empty slice. A LEADING DOT IS NOT AN EXTENSION SEPARATOR: `.config` is
	/// a hidden file named `config`, not a file with a `config` extension, and sorting it under one
	/// puts it somewhere nobody looks for it.
	pub fn extension(&self) -> &'a [u8] {
		match self.name.iter().rposition(|&byte| byte == b'.') {
			Some(0) | None => &[],
			Some(at) => &self.name[at + 1..],
		}
	}

	/// Whether the entry is hidden, by the one convention this system uses for it.
	pub fn is_hidden(&self) -> bool {
		self.name.first() == Some(&b'.')
	}
}

/// How two entries compare under `key`, before `reverse` and before directories-first.
///
/// NAME IS ALWAYS THE TIE-BREAK, which is what makes every ordering STABLE in the sense a reader
/// cares about: two files of the same size do not swap places between refreshes, so the entry under
/// the cursor is still under the cursor after one.
pub fn compare(key: SortKey, left: &EntryKey, right: &EntryKey) -> core::cmp::Ordering {
	use core::cmp::Ordering;
	let primary = match key {
		SortKey::Name => Ordering::Equal,
		SortKey::Extension => left.extension().cmp(right.extension()),
		SortKey::Size => left.size.cmp(&right.size),
		SortKey::Modified => left.modified.cmp(&right.modified),
		SortKey::Type => left.is_dir.cmp(&right.is_dir).reverse(),
	};
	primary.then_with(|| left.name.cmp(right.name))
}

/// The complete ordering: the key, the direction, and whether directories lead.
///
/// DIRECTORIES-FIRST IS APPLIED OUTSIDE `reverse`, deliberately. Reversing a listing that groups
/// directories first should reverse the FILES and the DIRECTORIES, not move the directories to the
/// bottom: a reader who reverses by size is asking for the biggest first, not for the folders to
/// move somewhere else.
pub fn order(spec: SortSpec, left: &EntryKey, right: &EntryKey) -> core::cmp::Ordering {
	use core::cmp::Ordering;
	if spec.directories_first && left.is_dir != right.is_dir {
		return if left.is_dir { Ordering::Less } else { Ordering::Greater };
	}
	let ordering = compare(spec.key, left, right);
	if spec.reverse { ordering.reverse() } else { ordering }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SortSpec {
	pub key: SortKey,
	pub reverse: bool,
	pub directories_first: bool,
	pub show_hidden: bool,
}

impl Default for SortSpec {
	fn default() -> SortSpec {
		SortSpec { key: SortKey::Name, reverse: false, directories_first: true, show_hidden: false }
	}
}

impl SortSpec {
	/// Whether an entry is shown at all. `..` is never hidden by the hidden-file rule even though it
	/// begins with a dot - a panel that could hide the way out of a directory would be a trap.
	pub fn admits(&self, entry: &EntryKey) -> bool {
		if entry.name == b".." {
			return true;
		}
		self.show_hidden || !entry.is_hidden()
	}
}

/// A bounded back/forward history of visited directories.
///
/// The shape is a browser's, and the important half is what happens on a NEW navigation: everything
/// ahead is discarded, because a forward list kept across a divergence offers to go somewhere the
/// reader did not come from.
pub struct History {
	entries: Vec<Vec<u8>>,
	at: usize,
}

impl Default for History {
	fn default() -> History {
		History::new()
	}
}

impl History {
	pub fn new() -> History {
		History { entries: Vec::new(), at: 0 }
	}

	/// Record arriving at `path`. False when it could not be recorded, which loses the history and
	/// never the navigation - a panel that refused to move because it could not remember doing so
	/// would be worse than one with a short memory.
	pub fn visit(&mut self, path: &[u8]) -> bool {
		if self.entries.get(self.at.wrapping_sub(1)).is_some_and(|last| last == path) {
			return true;
		}
		self.entries.truncate(self.at);
		let mut owned: Vec<u8> = Vec::new();
		if owned.try_reserve_exact(path.len()).is_err() || self.entries.try_reserve(1).is_err() {
			return false;
		}
		owned.extend_from_slice(path);
		self.entries.push(owned);
		if self.entries.len() > MAX_HISTORY {
			self.entries.remove(0);
		}
		self.at = self.entries.len();
		true
	}

	/// The previous directory, or None at the beginning.
	pub fn back(&mut self) -> Option<&[u8]> {
		if self.at <= 1 {
			return None;
		}
		self.at -= 1;
		self.entries.get(self.at - 1).map(Vec::as_slice)
	}

	/// The next directory, or None when nothing was gone back from.
	pub fn forward(&mut self) -> Option<&[u8]> {
		if self.at >= self.entries.len() {
			return None;
		}
		self.at += 1;
		self.entries.get(self.at - 1).map(Vec::as_slice)
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}
}

/// A bounded named list of places, which is all a bookmark is: `add` refuses past the cap rather
/// than dropping the oldest, because a bookmark somebody made on purpose disappearing to make room
/// for a newer one is the opposite of what they asked for.
pub struct Bookmarks {
	entries: Vec<Vec<u8>>,
}

impl Default for Bookmarks {
	fn default() -> Bookmarks {
		Bookmarks::new()
	}
}

impl Bookmarks {
	pub fn new() -> Bookmarks {
		Bookmarks { entries: Vec::new() }
	}

	pub fn add(&mut self, path: &[u8]) -> bool {
		if self.entries.iter().any(|entry| entry == path) {
			return true;
		}
		if self.entries.len() == MAX_BOOKMARKS {
			return false;
		}
		let mut owned: Vec<u8> = Vec::new();
		if owned.try_reserve_exact(path.len()).is_err() || self.entries.try_reserve(1).is_err() {
			return false;
		}
		owned.extend_from_slice(path);
		self.entries.push(owned);
		true
	}

	pub fn remove(&mut self, path: &[u8]) -> bool {
		match self.entries.iter().position(|entry| entry == path) {
			Some(at) => {
				self.entries.remove(at);
				true
			}
			None => false,
		}
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
}

/// Where a quick search should put the cursor: the first entry whose name starts with `typed`, at
/// or after `from`, wrapping once.
///
/// PREFIX AND NOT SUBSTRING, because that is what typing a name is: a reader typing `co` means the
/// file that begins with it, and matching `.config` on the same keystroke would move the cursor
/// somewhere they were not heading. Case is folded, since typing a capital to reach a file is a
/// requirement nobody wants.
///
/// WRAPPING ONCE is what lets a search started half way down the listing reach a name above it
/// without walking past the end - and once rather than forever, so a pattern nothing matches
/// terminates.
pub fn quick_search<'a>(names: impl Iterator<Item = &'a [u8]> + Clone, typed: &[u8], from: usize) -> Option<usize> {
	if typed.is_empty() {
		return None;
	}
	let matches = |name: &[u8]| name.len() >= typed.len() && name[..typed.len()].eq_ignore_ascii_case(typed);
	let tail = names.clone().enumerate().skip(from).find(|(_, name)| matches(name));
	if let Some((index, _)) = tail {
		return Some(index);
	}
	names.enumerate().take(from).find(|(_, name)| matches(name)).map(|(index, _)| index)
}
