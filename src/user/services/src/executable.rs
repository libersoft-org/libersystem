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
mod tests {
	use super::{explicit_path, launch_candidates, logical_name, lookup_identity};

	fn names(command: &str) -> alloc::vec::Vec<alloc::string::String> {
		launch_candidates(command).unwrap()
	}

	#[test]
	fn bare_name_has_only_the_suffixed_candidate() {
		assert_eq!(names("ping"), ["ping.lsexe"]);
	}

	#[test]
	fn explicit_name_prefers_exact_then_one_suffix_appended() {
		assert_eq!(names("ping.lsexe"), ["ping.lsexe", "ping.lsexe.lsexe"]);
	}

	#[test]
	fn repeated_suffix_is_ordinary_stem_text() {
		assert_eq!(logical_name("ping.lsexe.lsexe"), Some("ping.lsexe"));
		assert_eq!(names("ping.lsexe.lsexe"), ["ping.lsexe.lsexe", "ping.lsexe.lsexe.lsexe"]);
	}

	#[test]
	fn paths_and_malformed_names_are_rejected() {
		for name in ["", ".lsexe", "../ping", "bin/ping", "vol://system/bin/ping.lsexe", "ping name"] {
			assert!(launch_candidates(name).is_none(), "accepted {name}");
		}
	}

	#[test]
	fn explicit_volume_paths_require_an_exact_executable_artifact() {
		assert_eq!(explicit_path("vol://system/bin/ping.lsexe"), Some(("vol://system/bin/ping.lsexe", "ping.lsexe")));
		assert_eq!(explicit_path("vol://system/bin/ping.lsexe.lsexe"), Some(("vol://system/bin/ping.lsexe.lsexe", "ping.lsexe.lsexe")));
		for path in ["vol://system/bin/ping", "vol://system/bin/../ping.lsexe", "vol://system//ping.lsexe", "vol://system"] {
			assert!(explicit_path(path).is_none(), "accepted {path}");
		}
	}

	#[test]
	fn manifest_lookup_accepts_short_full_and_explicit_spellings() {
		assert_eq!(lookup_identity("cat"), Some("cat"));
		assert_eq!(lookup_identity("cat.lsexe"), Some("cat"));
		assert_eq!(lookup_identity("vol://system/bin/cat.lsexe"), Some("cat"));
	}
}
