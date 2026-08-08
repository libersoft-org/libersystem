// Which components can behave differently on different targets, found by looking.
//
// The architecture policy in `registry.toml` is a hand-written path table, and a hand-written table
// only knows what somebody remembered to put in it. Measured: **thirteen files under `src/user`
// contain `global_asm!`**, and `volume-client-provider` has a separate branch for each of the three
// targets - `jmp`, `b`, `tail` - while falling under the ordinary-userspace rule, which cross-builds
// everywhere and boots x86_64 only. Cross-building catches a branch that stops compiling. It does
// not catch one that compiles and jumps the wrong way.
//
// This is a CONSERVATIVE RISK CLASSIFIER and never a proof of neutrality, which is the distinction
// the design insists on:
//
//   a marker is present  -> the component is DEFINITELY architecture-sensitive
//   no marker is present -> the component is a CANDIDATE for neutral, and nothing more
//
// `usize`, `repr(C)` layout, alignment, atomics and a dependency's internals all differ between
// targets without any of these appearing. So a positive result widens the boot set and a negative
// one changes nothing - it never narrows what the policy table already said.

use crate::ownership::{Owner, Ownership};
use crate::registry::ARCHITECTURES;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Risk {
	// Targets the source names explicitly. `cfg(target_arch = "riscv64")` puts riscv64 here.
	pub targets: BTreeSet<String>,
	// A marker that proves target-dependence without naming a target: a bare `asm!`, a
	// `cfg(target_pointer_width)`, a `build.rs` that reads `TARGET`. Every target is at risk.
	pub any_target: bool,
	// Where it was seen, so `--explain` can name a file rather than assert a verdict.
	pub evidence: Vec<String>,
}

impl Risk {
	// The targets that must be booted for a change to this component. Empty means the classifier
	// found nothing and the policy table's answer stands unmodified.
	pub fn boot_targets(&self) -> BTreeSet<String> {
		if self.any_target {
			return ARCHITECTURES.iter().map(|architecture| (*architecture).to_string()).collect();
		}
		self.targets.clone()
	}

	fn record(&mut self, evidence: String) {
		// One line per component is enough to act on; the rest would bury it.
		if self.evidence.len() < 4 && !self.evidence.contains(&evidence) {
			self.evidence.push(evidence);
		}
	}
}

pub fn scan(repo_root: &Path, ownership: &Ownership) -> Result<BTreeMap<String, Risk>, String> {
	let mut risks: BTreeMap<String, Risk> = BTreeMap::new();
	walk(repo_root, &repo_root.join("src"), ownership, &mut risks)?;
	Ok(risks)
}

fn walk(repo_root: &Path, dir: &Path, ownership: &Ownership, risks: &mut BTreeMap<String, Risk>) -> Result<(), String> {
	let Ok(entries) = fs::read_dir(dir) else { return Ok(()) };
	for entry in entries.flatten() {
		let path = entry.path();
		let name = entry.file_name().to_string_lossy().into_owned();
		if path.is_dir() {
			if name == "target" || name.starts_with('.') {
				continue;
			}
			walk(repo_root, &path, ownership, risks)?;
			continue;
		}
		// Rust sources and Cargo manifests. A `.ld` script or a target JSON is per-target by
		// construction and is already handled by the policy table's own rows.
		let is_rust = name.ends_with(".rs");
		if !is_rust && name != "Cargo.toml" {
			continue;
		}
		let relative = crate::crates::relative(repo_root, &path);
		let Owner::Component { component, .. } = ownership.owner(&relative) else { continue };
		let Ok(text) = fs::read_to_string(&path) else { continue };
		let risk = risks.entry(component).or_default();
		if is_rust {
			classify_rust(&text, &relative, risk);
		} else {
			classify_manifest(&text, &relative, risk);
		}
	}
	Ok(())
}

// Exposed for the tests, which pin the marker set the design names rather than the tree's current
// contents - a tree that stopped containing `global_asm!` would otherwise make those tests vacuous.
#[cfg(test)]
pub fn classify_rust_for_test(text: &str, path: &str, risk: &mut Risk) {
	classify_rust(text, path, risk)
}

fn classify_rust(text: &str, path: &str, risk: &mut Risk) {
	for architecture in ARCHITECTURES {
		// Both spellings Rust accepts, and the `any(...)`/`not(...)` forms contain the same
		// substring, so this catches them without parsing cfg expressions.
		if text.contains(&format!("target_arch = \"{architecture}\"")) || text.contains(&format!("target_arch=\"{architecture}\"")) {
			risk.targets.insert(architecture.to_string());
			risk.record(format!("{path} names target_arch {architecture}"));
		}
	}
	// Inline assembly is target-specific by definition. `global_asm!` especially: it emits a whole
	// function body, and a per-target branch of it can be valid and wrong at the same time.
	for marker in ["global_asm!", "asm!(", "core::arch::asm", "naked_asm!"] {
		if text.contains(marker) {
			risk.any_target = true;
			risk.record(format!("{path} contains {marker}"));
		}
	}
	// A pointer-width branch names no target and differs on all of them.
	if text.contains("target_pointer_width") || text.contains("target_endian") || text.contains("target_feature") {
		risk.any_target = true;
		risk.record(format!("{path} branches on a target property other than the architecture"));
	}
	// A build script that reads the target decides something per target, and what it decides is
	// invisible in the crate's own source - `src/user/build.rs` picks one of three linker scripts
	// this way.
	if path.ends_with("build.rs") && (text.contains("CARGO_CFG_TARGET_ARCH") || text.contains("\"TARGET\"")) {
		risk.any_target = true;
		risk.record(format!("{path} reads the target and decides per target"));
	}
}

fn classify_manifest(text: &str, path: &str, risk: &mut Risk) {
	// `[target.'cfg(target_arch = "riscv64")'.dependencies]` and the plain-triple form
	// `[target.riscv64gc-unknown-none-elf.dependencies]`.
	for line in text.lines() {
		let line = line.trim();
		if !line.starts_with("[target.") {
			continue;
		}
		let mut named = false;
		for architecture in ARCHITECTURES {
			if line.contains(architecture) {
				risk.targets.insert(architecture.to_string());
				risk.record(format!("{path} has a {architecture}-specific dependency table"));
				named = true;
			}
		}
		if !named {
			risk.any_target = true;
			risk.record(format!("{path} has a target-specific dependency table"));
		}
	}
}
