// The component graph: three sources unioned, every edge typed.
//
// `manifest.toml` is the DYNAMIC graph - which .lslib a program loads at run time. It is not the
// dependency graph, and a selector built on it alone would be a false-negative machine: 27 of the
// 108 crates here are named nowhere in it, including all seven of `src/fs`, while the kernel
// statically links `liberfs` and `partition` and dev-links eleven userspace codecs.
//
// Direction is always dependent -> dependency ("A uses B"). Selection walks it backwards: a change
// to B affects everything that can reach B.

use crate::crates::Crate;
use crate::registry::Registry;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Edge {
	pub from: String,
	pub to: String,
	pub kind: String,
	pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
	pub components: BTreeSet<String>,
	pub edges: Vec<Edge>,
	dependents: BTreeMap<String, BTreeSet<String>>,
}

impl Graph {
	pub fn build(crates: &[Crate], manifest: &system_manifest::Manifest, registry: &Registry) -> Self {
		let mut graph = Graph::default();
		for entry in crates {
			graph.components.insert(entry.name.clone());
			for binary in &entry.binaries {
				graph.components.insert(binary_component(&binary.name));
			}
		}
		// A declared component is a component even with no edges. `harness.scripts` depends on
		// nothing and nothing depends on it - it selects everything by decree rather than by
		// closure - and leaving it out of the node set would make every reference to it look like
		// a dangling name.
		for rule in &registry.ownership {
			graph.components.insert(rule.component.clone());
		}

		// Source one: the Cargo manifests, all three dependency kinds kept apart.
		for entry in crates {
			for dependency in &entry.dependencies {
				let reason = match (dependency.kind, dependency.optional) {
					(_, true) => format!("{} Cargo.toml, optional (feature-gated, taken anyway)", entry.dir),
					_ => format!("{} Cargo.toml", entry.dir),
				};
				graph.push(&entry.name, &dependency.name, dependency.kind.edge_kind(), &reason);
			}
			// A program is built from its crate's shared source, so changing that source reaches
			// every program in it - while changing one program's own file reaches only that one.
			for binary in &entry.binaries {
				graph.push(&binary_component(&binary.name), &entry.name, "link.static", &format!("{} [[bin]] {}", entry.dir, binary.name));
			}
		}

		// Source two: the run-time provider graph. A program or library names the providers it
		// loads, which is an edge no Cargo manifest carries - `audioconv` does not `path`-depend on
		// `flac`, it dlopens it.
		//
		// Manifest names are LOGICAL and need resolving: the library `lsrt` is the crate `rt`, the
		// source `core` is the crate `services`, and `audioconv` is both a program and a library -
		// which is exactly why programs live in their own `bin.` namespace here.
		let names = Names::new(crates, manifest);
		for program in manifest.programs.values() {
			let Some(from) = names.program(program) else { continue };
			for provider in &program.providers {
				let Some(to) = names.library(provider.as_str()) else { continue };
				graph.push(&from, &to, "link.dynamic", "services/manifest.toml providers");
			}
		}
		for library in manifest.libraries.values() {
			let Some(from) = names.library(library.name.as_str()) else { continue };
			for provider in &library.providers {
				let Some(to) = names.library(provider.as_str()) else { continue };
				graph.push(&from, &to, "link.dynamic", "services/manifest.toml providers");
			}
		}

		// A declared subtree of a crate is part of what that crate is BUILT FROM, and the edge has to
		// say so or the subtree floats free. Found by the regression corpus: a change under
		// `src/kernel/arch/riscv64` planned a riscv64 boot and no build at all, because
		// `kernel.arch.riscv64` had no edge to `kernel` and `build.kernel` covers the crate.
		//
		// The direction is crate -> subtree ("the kernel is built from its arch tree"), so the reverse
		// closure from a change in the subtree arrives at the crate, which is what selects the build.
		let mut crate_dirs: Vec<(&str, &str)> = crates.iter().map(|entry| (entry.dir.as_str(), entry.name.as_str())).collect();
		crate_dirs.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.len()));
		for rule in &registry.ownership {
			let Some((dir, owner)) = crate_dirs.iter().find(|(dir, _)| crate::registry::prefix_match(dir, &rule.path).is_some()) else { continue };
			if *owner == rule.component {
				continue;
			}
			graph.push(owner, &rule.component, "link.static", &format!("{} is a declared subtree of the {owner} crate at {dir}", rule.path));
		}

		// Source three: what only a person can state.
		for edge in &registry.edges {
			graph.push(&edge.from, &edge.to, &edge.kind, &edge.reason);
		}

		graph.edges.sort();
		graph.edges.dedup();
		graph.reindex();
		graph
	}

	fn push(&mut self, from: &str, to: &str, kind: &str, reason: &str) {
		// A self-edge says nothing and would make every closure look one step deeper than it is.
		if from == to {
			return;
		}
		self.components.insert(from.to_string());
		self.components.insert(to.to_string());
		self.edges.push(Edge { from: from.to_string(), to: to.to_string(), kind: kind.to_string(), reason: reason.to_string() });
	}

	fn reindex(&mut self) {
		self.dependents.clear();
		for edge in &self.edges {
			self.dependents.entry(edge.to.clone()).or_default().insert(edge.from.clone());
		}
	}

	// Everything reachable BACKWARDS from the given components, the seeds included.
	//
	// Breadth-first with a visited set, so a cycle terminates rather than recursing - and cycles do
	// occur here: a dev-dependency can point the opposite way to a normal one, which is exactly
	// what the kernel's dependency on the audio codecs does.
	pub fn affected(&self, seeds: &BTreeSet<String>) -> BTreeSet<String> {
		let mut seen: BTreeSet<String> = seeds.clone();
		let mut queue: VecDeque<String> = seeds.iter().cloned().collect();
		while let Some(component) = queue.pop_front() {
			let Some(dependents) = self.dependents.get(&component) else { continue };
			for dependent in dependents {
				if seen.insert(dependent.clone()) {
					queue.push_back(dependent.clone());
				}
			}
		}
		seen
	}

	// The same walk, but recording ONE shortest path per component, so `--explain` can say
	// "audioconv, because audioconv -link.dynamic-> flac" rather than only naming the answer.
	pub fn affected_with_reasons(&self, seeds: &BTreeSet<String>) -> BTreeMap<String, Vec<Edge>> {
		let mut paths: BTreeMap<String, Vec<Edge>> = seeds.iter().map(|seed| (seed.clone(), Vec::new())).collect();
		let mut queue: VecDeque<String> = seeds.iter().cloned().collect();
		while let Some(component) = queue.pop_front() {
			let Some(dependents) = self.dependents.get(&component) else { continue };
			let so_far = paths.get(&component).cloned().unwrap_or_default();
			for dependent in dependents {
				if paths.contains_key(dependent) {
					continue;
				}
				let edge = self.edges.iter().find(|edge| &edge.from == dependent && edge.to == component).cloned().expect("the reverse index was built from these edges");
				let mut path = so_far.clone();
				path.push(edge);
				paths.insert(dependent.clone(), path);
				queue.push_back(dependent.clone());
			}
		}
		paths
	}

	// Forward reachability over chosen edge kinds: what this component pulls IN, rather than what
	// pulls it in. Used to answer whether a build configuration drags in the freestanding runtime.
	pub fn reaches(&self, from: &str, kinds: &[&str]) -> BTreeSet<String> {
		let mut seen: BTreeSet<String> = [from.to_string()].into_iter().collect();
		let mut queue: VecDeque<String> = [from.to_string()].into_iter().collect();
		while let Some(component) = queue.pop_front() {
			for edge in self.edges.iter().filter(|edge| edge.from == component && kinds.contains(&edge.kind.as_str())) {
				if seen.insert(edge.to.clone()) {
					queue.push_back(edge.to.clone());
				}
			}
		}
		seen
	}

	pub fn edges_from(&self, component: &str) -> Vec<&Edge> {
		self.edges.iter().filter(|edge| edge.from == component).collect()
	}

	pub fn contains(&self, component: &str) -> bool {
		self.components.contains(component)
	}

	// Every component named by an edge must be a component the model knows about.
	//
	// The failure this catches is a rename: a crate becomes `audio-conv`, the registry still says
	// `audioconv`, the declared edge silently points at nothing, and the closure quietly stops
	// reaching what it used to reach. Nothing else in the pipeline would notice - a missing edge
	// makes the plan SMALLER, which is the direction no error message ever arrives from.
	pub fn validate(&self, crates: &[Crate], registry: &Registry) -> Result<(), String> {
		let mut known: BTreeSet<String> = crates.iter().map(|entry| entry.name.clone()).collect();
		for entry in crates {
			for binary in &entry.binaries {
				known.insert(binary_component(&binary.name));
			}
		}
		let declared: BTreeSet<&str> = registry.ownership.iter().map(|rule| rule.component.as_str()).collect();
		let mut errors = Vec::new();
		for edge in &self.edges {
			for endpoint in [&edge.from, &edge.to] {
				if !known.contains(endpoint.as_str()) && !declared.contains(endpoint.as_str()) {
					errors.push(format!("edge {} -{}-> {} names '{endpoint}', which is neither a crate nor a declared component", edge.from, edge.kind, edge.to));
				}
			}
		}
		errors.sort();
		errors.dedup();
		if errors.is_empty() { Ok(()) } else { Err(errors.join("\n")) }
	}
}

// Programs live in their own namespace because a manifest name can mean two different things:
// `audioconv` is a program under `bin/audioconv.lsexe` AND a library under
// `lib/audio/audioconv.lslib`, owned by different crates, with different providers. Flattening
// them would merge a tool with the library it loads.
pub fn binary_component(name: &str) -> String {
	format!("bin.{name}")
}

// Manifest logical names resolved to components, once, so no caller has to know the rules.
struct Names {
	// Manifest source owner -> crate package name. The two differ: the source `core` is the crate
	// `services`, and the source `rt` backs the library `lsrt`.
	sources: BTreeMap<String, String>,
	libraries: BTreeMap<String, String>,
	binaries: BTreeSet<String>,
}

impl Names {
	fn new(crates: &[Crate], manifest: &system_manifest::Manifest) -> Self {
		let by_dir: BTreeMap<&str, &str> = crates.iter().map(|entry| (entry.dir.as_str(), entry.name.as_str())).collect();
		let mut sources = BTreeMap::new();
		for source in manifest.sources.values() {
			// Manifest paths are relative to `src/`; crate directories here are relative to the
			// repository root.
			let dir = format!("src/{}", source.path.as_str());
			if let Some(name) = by_dir.get(dir.as_str()) {
				sources.insert(source.owner.as_str().to_string(), (*name).to_string());
			}
		}
		let binaries: BTreeSet<String> = crates.iter().flat_map(|entry| entry.binaries.iter().map(|binary| binary.name.clone())).collect();
		let mut libraries = BTreeMap::new();
		for library in manifest.libraries.values() {
			// A library IS a crate, resolved through its owning source. Falling back to the
			// library's own name covers the common case where the two already agree.
			let component = sources.get(library.owner.as_str()).cloned().unwrap_or_else(|| library.name.as_str().to_string());
			libraries.insert(library.name.as_str().to_string(), component);
		}
		Names { sources, libraries, binaries }
	}

	fn program(&self, program: &system_manifest::Program) -> Option<String> {
		if self.binaries.contains(program.name.as_str()) {
			return Some(binary_component(program.name.as_str()));
		}
		self.sources.get(program.owner.as_str()).cloned()
	}

	fn library(&self, name: &str) -> Option<String> {
		self.libraries.get(name).cloned()
	}
}
