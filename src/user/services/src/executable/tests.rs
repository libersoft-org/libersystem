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
