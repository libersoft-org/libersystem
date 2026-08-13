use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const SUFFIX: &str = abi::EXECUTABLE_SUFFIX;
pub const MAX_BASENAME_LEN: usize = 64;
pub const MAX_PATH_LEN: usize = 256;

fn valid_basename(name: &str) -> bool {
	let mut bytes = name.bytes();
	let Some(first) = bytes.next() else { return false };
	name.len() <= MAX_BASENAME_LEN && (first.is_ascii_alphanumeric() || first == b'_') && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

pub fn logical_name(artifact: &str) -> Option<&str> {
	let stem = artifact.strip_suffix(SUFFIX)?;
	valid_basename(stem).then_some(stem)
}

pub fn explicit_path(command: &str) -> Option<(&str, &str)> {
	if command.len() > MAX_PATH_LEN {
		return None;
	}
	let relative = command.strip_prefix("vol://")?;
	let mut segments = relative.split('/');
	let volume = segments.next()?;
	if !valid_basename(volume) {
		return None;
	}
	let mut basename = None;
	for segment in segments {
		if !valid_basename(segment) || segment == "." || segment == ".." {
			return None;
		}
		basename = Some(segment);
	}
	let basename = basename?;
	logical_name(basename)?;
	Some((command, basename))
}

pub fn launch_candidates(command: &str) -> Option<Vec<String>> {
	if !valid_basename(command) {
		return None;
	}
	let mut candidates = Vec::with_capacity(2);
	if logical_name(command).is_some() {
		candidates.push(String::from(command));
	}
	let appended = format!("{command}{SUFFIX}");
	if appended.len() <= MAX_BASENAME_LEN {
		candidates.push(appended);
	}
	(!candidates.is_empty()).then_some(candidates)
}

/// The directories a search path names, in order.
///
/// SEMICOLONS, not colons. A `vol://` URI contains a colon, so a colon-separated PATH cannot
/// express one directory - `vol://system/bin` splits into `vol` and `//system/bin`, and a `which`
/// written against the Unix convention searched exactly those two. The separator is a property of
/// what the entries look like, and here they look like URIs.
///
/// Empty entries are dropped rather than treated as "the current directory": a working directory
/// is not a place to find programs, and the empty-means-cwd convention is how a path picked up a
/// program nobody meant to run.
pub fn path_entries(search: &str) -> impl Iterator<Item = &str> {
	search.split(';').map(str::trim).filter(|entry| !entry.is_empty())
}

pub fn lookup_identity(command: &str) -> Option<&str> {
	if let Some((_, basename)) = explicit_path(command) {
		return logical_name(basename);
	}
	if let Some(stem) = logical_name(command) {
		return Some(stem);
	}
	valid_basename(command).then_some(command)
}

#[cfg(test)]
#[path = "executable/tests.rs"]
mod tests;
