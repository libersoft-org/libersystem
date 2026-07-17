use super::executable_aliases_ambiguous;

#[test]
fn executable_alias_collision_is_exactly_one_suffix_level() {
	assert!(executable_aliases_ambiguous(b"bin/ping.lsexe", b"bin/ping.lsexe.lsexe"));
	assert!(!executable_aliases_ambiguous(b"bin/ping.lsexe", b"bin/ping.lsexe.lsexe.lsexe"));
	assert!(!executable_aliases_ambiguous(b"bin/ping.lsexe", b"drivers/ping.lsexe.lsexe"));
}
