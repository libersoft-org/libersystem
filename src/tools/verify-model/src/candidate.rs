// A NARROWING IS EARNED, AND A CANDIDATE IS HOW THE EVIDENCE IS GATHERED WITHOUT SKIPPING ANYTHING.
//
// The ordering problem this solves: `--shadow` compares FULL against FULL while the kernel is one
// component, so it proves nothing about a narrower model - and splitting the kernel changes the
// registry and the catalog and therefore `model_hash`, which correctly discards every certificate
// bound to the old one. Evidence first, split second, and the split destroys the evidence.
//
// A candidate breaks that circle. It is a FROZEN, VERSIONED input that computes the narrower
// selection; the authoritative run stays FULL, so nothing is skipped while evidence accumulates; the
// comparison is recorded against the CANDIDATE's hash; and activation checks that hash.
//
// IT IS A COMPLETE OVERLAY OF BOTH NARROWING INPUTS, and this is the part that decides whether
// activation can ever match. `model_hash` is taken over the registry text, the configuration
// catalog, the graph, every feature definition, the arch-risk table AND the catalog itself, check by
// check, each with its `covers`. Narrowing changes TWO things: ownership in `registry.toml`, and the
// `covers` written in the kernel tests' own sources, which reach the hash through the catalog. A
// candidate carrying one of them cannot hash identically at activation, which would make the
// strictest step of the contract unsatisfiable rather than strict.
//
// AND IT CARRIES THE PREIMAGE. Comparing only the RESULT says nothing about the base: an activation
// that overwrites a `registry.toml` somebody edited in the meantime still produces the hash the
// candidate predicts, because the overlay decides it - so the check would pass while destroying an
// unrelated change and certifying a model nobody reviewed. The digests of the canonical inputs the
// candidate was frozen against are verified BEFORE the first write, and a base that has moved is a
// refusal with nothing written.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Candidate {
	// What this candidate is for, in a sentence a person reads before activating it.
	pub reason: String,
	// THE MODEL HASH THE EVIDENCE WAS GATHERED UNDER, and the whole point of the check at activation.
	//
	// Without it "the same candidate, activated" is a claim about a human's intention. The model that
	// becomes active is re-read from the canonical files with no overlay in the path and must hash
	// to this; anything else is refused and rolled back.
	pub expected_hash: String,
	// The canonical inputs this was frozen against, by path and content digest.
	pub base: BTreeMap<String, String>,
	// The complete replacement text for `model/registry.toml`. Bytes rather than a patch: the hash
	// is taken over the registry's TEXT, so what is frozen has to be the exact bytes that will be
	// there - which makes materialising it a copy rather than a regeneration that has to agree.
	pub registry: String,
	// The `covers` each named test id gets under this candidate. Applied to the sources that declare
	// it - which is why `Declaration` carries its paths.
	#[serde(default)]
	pub covers: BTreeMap<String, Vec<String>>,
}

pub fn digest_of(bytes: &[u8]) -> String {
	let mut hasher = Sha256::new();
	hasher.update(bytes);
	format!("{:x}", hasher.finalize())
}

impl Candidate {
	pub fn load(path: &Path) -> Result<Candidate, String> {
		let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
		toml::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
	}

	// Every canonical input still holds the bytes this candidate was frozen against.
	//
	// Answered before anything is written, and answered for ALL of them rather than stopping at the
	// first mismatch: an operator deciding whether to refreeze wants the whole list, and reporting
	// one file at a time turns one decision into as many rounds as there are files.
	pub fn base_is_unmoved(&self, repo_root: &Path, sources: &BTreeMap<String, Vec<String>>) -> Result<(), String> {
		// EVERY FILE THIS WILL OVERWRITE HAS TO BE IN THE BASE. The check ranged over the entries the
		// candidate CHOSE to list, so a candidate that simply omitted `registry.toml` - or the source
		// declaring a test whose `covers` it rewrites - passed a check named "the base is unmoved"
		// while overwriting a file nobody had compared against anything.
		let mut will_write: Vec<String> = vec![String::from("src/tools/verify-model/model/registry.toml")];
		for id in self.covers.keys() {
			if let Some(paths) = sources.get(id) {
				will_write.extend(paths.iter().cloned());
			}
		}
		let unlisted: Vec<&String> = will_write.iter().filter(|path| !self.base.contains_key(*path)).collect();
		if !unlisted.is_empty() {
			return Err(format!("this candidate would overwrite files it does not record a base digest for, so nothing could tell whether they had moved:\n    {}\n  Refreeze the candidate so its `base` covers every file it writes; nothing was written.", unlisted.iter().map(|path| path.as_str()).collect::<Vec<_>>().join("\n    ")));
		}
		let mut moved = Vec::new();
		for (relative, expected) in &self.base {
			let path = repo_root.join(relative);
			let Ok(bytes) = std::fs::read(&path) else {
				moved.push(format!("{relative} is gone"));
				continue;
			};
			let actual = digest_of(&bytes);
			if &actual != expected {
				moved.push(format!("{relative} has changed since this candidate was frozen"));
			}
		}
		if moved.is_empty() {
			return Ok(());
		}
		Err(format!("the base this candidate was frozen against has moved, so activating it would overwrite work nobody reviewed:\n    {}\n  Refreeze the candidate against the tree as it is now; nothing was written.", moved.join("\n    ")))
	}

	// Write the overlay into the canonical files. Called only after `base_is_unmoved` has passed.
	//
	// Returns what was there before, so a refused activation can put it back byte for byte - which
	// is the difference between a check that refuses and one that refuses after the damage.
	pub fn materialise(&self, repo_root: &Path, sources: &BTreeMap<String, Vec<String>>) -> Result<BTreeMap<String, Vec<u8>>, String> {
		// EVERY ID RESOLVED BEFORE THE FIRST WRITE. The registry was written first and the ids were
		// resolved inside the loop after it, so a candidate naming a test the tree does not have was
		// refused with the canonical registry already replaced - and the caller's `?` returned before
		// it had the `previous` map to put it back with. A refusal that leaves the damage is the one
		// thing this function's own comment says it is not.
		for id in self.covers.keys() {
			if !sources.contains_key(id) {
				return Err(format!("the candidate gives `{id}` a narrower `covers` and no source declares that id - a candidate naming a test the tree does not have cannot be activated; nothing was written"));
			}
		}
		// AND AN IO ERROR PART-WAY THROUGH UNDOES WHAT THIS CALL WROTE. The ids above are resolved
		// first, so what is left to fail here is a read or a write - and failing on the third file of
		// four used to leave two of them replaced with the caller holding no `previous` to undo them.
		let mut previous: BTreeMap<String, Vec<u8>> = BTreeMap::new();
		match self.write_overlay(repo_root, sources, &mut previous) {
			Ok(()) => Ok(previous),
			Err(error) => match Candidate::roll_back(repo_root, &previous) {
				Ok(()) => Err(format!("{error}; nothing was kept")),
				Err(second) => Err(format!("{error}; and putting the files back failed too, so the tree is now part-written and needs a look: {second}")),
			},
		}
	}

	// The writes themselves. Separated so the caller above can undo exactly what this managed to do.
	fn write_overlay(&self, repo_root: &Path, sources: &BTreeMap<String, Vec<String>>, previous: &mut BTreeMap<String, Vec<u8>>) -> Result<(), String> {
		let registry = repo_root.join("src/tools/verify-model/model/registry.toml");
		previous.insert(String::from("src/tools/verify-model/model/registry.toml"), std::fs::read(&registry).map_err(|error| format!("{}: {error}", registry.display()))?);
		std::fs::write(&registry, self.registry.as_bytes()).map_err(|error| format!("{}: {error}", registry.display()))?;

		// THE OTHER HALF OF THE OVERLAY. `covers` lives in the kernel tests' own sources and reaches
		// `model_hash` through the catalog, so a candidate that changed only the registry could never
		// hash to what its evidence was gathered under.
		//
		// Rewritten where the test is DECLARED, which is why the declaration carries its paths -
		// plural, because an arch-gated test is declared once per target under one id and editing
		// only the last file read would leave the other two saying something else.
		for (id, covers) in &self.covers {
			let Some(paths) = sources.get(id) else { unreachable!("every id was resolved before the first write") };
			let replacement = format!("id = \"{id}\", covers = [{}]", covers.iter().map(|component| format!("\"{component}\"")).collect::<Vec<_>>().join(", "));
			for relative in paths {
				let path = repo_root.join(relative);
				let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
				previous.entry(relative.clone()).or_insert_with(|| text.clone().into_bytes());
				let Some(start) = text.find(&format!("id = \"{id}\"")) else { continue };
				let Some(end) = text[start..].find(']').map(|at| start + at + 1) else { continue };
				let mut rewritten = String::with_capacity(text.len());
				rewritten.push_str(&text[..start]);
				rewritten.push_str(&replacement);
				rewritten.push_str(&text[end..]);
				std::fs::write(&path, rewritten.as_bytes()).map_err(|error| format!("{}: {error}", path.display()))?;
			}
		}
		Ok(())
	}

	pub fn roll_back(repo_root: &Path, previous: &BTreeMap<String, Vec<u8>>) -> Result<(), String> {
		for (relative, bytes) in previous {
			let path = repo_root.join(relative);
			std::fs::write(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
		}
		Ok(())
	}
}
