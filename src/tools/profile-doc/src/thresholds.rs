//! The conformance-threshold chapter, rendered into whichever profile document owns each number.
//!
//! ONE REGISTRY AND SEVERAL DOCUMENTS. The thresholds are per profile and they are written in one
//! place, because a number that appeared in two documents would be two numbers as soon as one was
//! corrected - and a conformance suite reading the wrong one passes an implementation the other
//! would refuse.

use std::fmt::Write as _;

use graphics_profile::thresholds::{THRESHOLD_RULES, THRESHOLDS};

/// The canonical lines a profile's own hash covers: its thresholds, and - once, for the profile the
/// rules are documented under - the rules themselves.
pub fn canonical_for(profile: &str, with_rules: bool) -> String {
	let mut out = String::new();
	for threshold in THRESHOLDS.iter().filter(|threshold| threshold.profile == profile) {
		let _ = writeln!(out, "threshold={} tolerance={} why={}", threshold.what, threshold.tolerance, threshold.why);
	}
	if with_rules {
		for rule in THRESHOLD_RULES {
			let _ = writeln!(out, "threshold-rule={} answer={}", rule.question, rule.answer);
		}
	}
	out
}

/// The chapter a person reads.
pub fn chapter(profile: &str, with_rules: bool) -> String {
	let mut out = String::new();
	let _ = writeln!(out, "\n## Conformance thresholds\n");
	let _ = writeln!(out, "A suite without stated tolerances either demands bit-exactness, which no two implementations of a");
	let _ = writeln!(out, "sine achieve, or demands nothing. Both are ways of not checking. No tolerance may be loosened per");
	let _ = writeln!(out, "architecture.\n");
	let _ = writeln!(out, "| what is compared | tolerance | why |");
	let _ = writeln!(out, "| --- | --- | --- |");
	for threshold in THRESHOLDS.iter().filter(|threshold| threshold.profile == profile) {
		let _ = writeln!(out, "| {} | {} | {} |", threshold.what, threshold.tolerance, threshold.why);
	}
	if with_rules {
		let _ = writeln!(out);
		let _ = writeln!(out, "| question | answer |");
		let _ = writeln!(out, "| --- | --- |");
		for rule in THRESHOLD_RULES {
			let _ = writeln!(out, "| {} | {} |", rule.question, rule.answer);
		}
	}
	out
}

/// How many thresholds a profile publishes, for the check's own report.
pub fn count(profile: &str) -> usize {
	THRESHOLDS.iter().filter(|threshold| threshold.profile == profile).count()
}
