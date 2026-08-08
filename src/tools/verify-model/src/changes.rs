// What changed, parsed once, in the model.
//
// It used to be two `sed` expressions in `verify.sh` and a different `git diff --name-only` in the
// regression corpus - two parsers for one question, which is why the corpus never caught the defect
// in the other one: `git status --porcelain` reports a rename as `old -> new`, the shell kept only
// the new path, and the old one was discarded. Moving a file OUT of a component therefore did not
// select that component, and a rename into `docs/` could look like nothing had changed at all.
//
// **Both sides of a rename are changes.** The new path is where the code is now; the old path is a
// place that used to have code and no longer does, which is a change to whatever owned it. The same
// argument makes a deletion the OLD path - the only path it has.
//
// Machine-readable formats throughout (`--porcelain=v2 -z`, `--name-status -z`), because the
// human-readable ones quote and escape paths with spaces and the shell versions got that wrong too.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
	Added,
	Modified,
	Deleted,
	Renamed,
	Copied,
	Untracked,
	// A conflicted file. Both sides changed it, so it is a change by any reading.
	Unmerged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
	pub kind: Kind,
	pub path: String,
	// The path a rename or copy came FROM. It is a changed path in its own right and is returned by
	// `paths()` alongside the destination.
	pub origin: Option<String>,
}

impl Change {
	fn simple(kind: Kind, path: &str) -> Self {
		Change { kind, path: path.to_string(), origin: None }
	}
}

// Every path a change set touches, deduplicated and sorted - both sides of every rename.
pub fn paths(changes: &[Change]) -> Vec<String> {
	let mut all: BTreeSet<String> = BTreeSet::new();
	for change in changes {
		all.insert(change.path.clone());
		if let Some(origin) = &change.origin {
			all.insert(origin.clone());
		}
	}
	all.into_iter().collect()
}

pub fn working_tree(repo_root: &Path) -> Result<Vec<Change>, String> {
	let output = git(repo_root, &["status", "--porcelain=v2", "-z", "--untracked-files=all"])?;
	parse_status_v2(&output)
}

pub fn range(repo_root: &Path, range: &str) -> Result<Vec<Change>, String> {
	let output = git(repo_root, &["diff", "--name-status", "-z", "--find-renames", range])?;
	parse_name_status(&output)
}

fn git(repo_root: &Path, arguments: &[&str]) -> Result<String, String> {
	let output = Command::new("git").arg("-C").arg(repo_root).args(arguments).output().map_err(|error| format!("git {}: {error}", arguments.join(" ")))?;
	if !output.status.success() {
		return Err(format!("git {} failed: {}", arguments.join(" "), String::from_utf8_lossy(&output.stderr).trim()));
	}
	Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// `git status --porcelain=v2 -z`.
//
// Records are NUL-terminated and the leading character says which shape follows:
//
//   1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>              ordinary change
//   2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <Xscore> <path>     rename or copy, and the ORIGIN
//                                                             follows as its own NUL-terminated field
//   u <XY> ...  <path>                                        unmerged
//   ? <path>                                                  untracked
//   ! <path>                                                  ignored
pub fn parse_status_v2(output: &str) -> Result<Vec<Change>, String> {
	let mut fields = output.split('\0').peekable();
	let mut changes = Vec::new();
	while let Some(record) = fields.next() {
		if record.is_empty() {
			continue;
		}
		let mut parts = record.splitn(2, ' ');
		let marker = parts.next().unwrap_or("");
		let rest = parts.next().unwrap_or("");
		match marker {
			"1" => {
				// `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>` - seven metadata fields.
				let (status, path) = split_status_and_path(rest, 7).ok_or_else(|| format!("unparseable porcelain record: {record}"))?;
				changes.push(Change::simple(kind_from_xy(status), path));
			}
			"2" => {
				// Eight metadata fields for a rename record, then the destination path; the origin
				// is the NEXT NUL-terminated field. Consuming it here is what keeps the two in step.
				let (status, path) = split_status_and_path(rest, 8).ok_or_else(|| format!("unparseable porcelain rename record: {record}"))?;
				// Empty is not absent to `split`, and a truncated record ends with an empty field -
				// so `next()` alone would accept a rename whose origin was never written.
				let origin = fields.next().filter(|origin| !origin.is_empty()).ok_or_else(|| format!("porcelain rename record with no origin: {record}"))?;
				let kind = if status.starts_with('C') || status.contains('C') { Kind::Copied } else { Kind::Renamed };
				changes.push(Change { kind, path: path.to_string(), origin: Some(origin.to_string()) });
			}
			"u" => {
				// `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>` - nine metadata fields,
				// three stage hashes rather than the two an ordinary record carries.
				let (_, path) = split_status_and_path(rest, 9).ok_or_else(|| format!("unparseable porcelain unmerged record: {record}"))?;
				changes.push(Change::simple(Kind::Unmerged, path));
			}
			"?" => changes.push(Change::simple(Kind::Untracked, rest)),
			// Ignored files are not changes; headers (`#`) are not either.
			"!" | "#" => {}
			_ => return Err(format!("unknown porcelain record marker '{marker}' in: {record}")),
		}
	}
	Ok(changes)
}

// Skip `count` whitespace-separated metadata fields and return the status field plus the remainder,
// which is the path. Split by field count rather than by pattern because a path may contain spaces.
fn split_status_and_path(rest: &str, count: usize) -> Option<(&str, &str)> {
	let mut cursor = rest;
	let mut status = None;
	for index in 0..count {
		let space = cursor.find(' ')?;
		if index == 0 {
			status = Some(&cursor[..space]);
		}
		cursor = &cursor[space + 1..];
	}
	Some((status?, cursor))
}

fn kind_from_xy(status: &str) -> Kind {
	// Two columns, staged and unstaged. Either one carrying a letter makes it that kind; `A` wins
	// over `M` because a file that was added and then edited is still an addition.
	if status.contains('A') {
		Kind::Added
	} else if status.contains('D') {
		Kind::Deleted
	} else {
		Kind::Modified
	}
}

// `git diff --name-status -z`: `<status>\0<path>\0`, and for a rename or copy
// `<status><score>\0<origin>\0<destination>\0` - origin FIRST, unlike porcelain v2.
pub fn parse_name_status(output: &str) -> Result<Vec<Change>, String> {
	let mut fields = output.split('\0');
	let mut changes = Vec::new();
	while let Some(status) = fields.next() {
		if status.is_empty() {
			continue;
		}
		let letter = status.chars().next().unwrap_or('?');
		match letter {
			'R' | 'C' => {
				let origin = fields.next().ok_or_else(|| format!("rename status '{status}' with no origin"))?;
				let destination = fields.next().ok_or_else(|| format!("rename status '{status}' with no destination"))?;
				changes.push(Change { kind: if letter == 'C' { Kind::Copied } else { Kind::Renamed }, path: destination.to_string(), origin: Some(origin.to_string()) });
			}
			_ => {
				let path = fields.next().ok_or_else(|| format!("status '{status}' with no path"))?;
				let kind = match letter {
					'A' => Kind::Added,
					'D' => Kind::Deleted,
					'U' => Kind::Unmerged,
					_ => Kind::Modified,
				};
				changes.push(Change::simple(kind, path));
			}
		}
	}
	Ok(changes)
}
