extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const SUFFIX: &str = ".lsexe";
pub const MAX_BASENAME_LEN: usize = 64;

fn valid_basename(name: &str) -> bool {
	let mut bytes = name.bytes();
	let Some(first) = bytes.next() else { return false };
	name.len() <= MAX_BASENAME_LEN && (first.is_ascii_alphanumeric() || first == b'_') && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

pub fn logical_name(artifact: &str) -> Option<&str> {
	let stem = artifact.strip_suffix(SUFFIX)?;
	valid_basename(stem).then_some(stem)
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

#[cfg(test)]
mod tests {
	use super::{launch_candidates, logical_name};

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
}
