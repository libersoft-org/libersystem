//! Generate what the graphics profiles are READ from, out of the profiles they ARE - and check what
//! claims to implement or to test them.
//!
//! WHAT IS GENERATED AND WHY EACH. The documentation table is what a person reads; the backend
//! checklist is what an implementer works through; the conformance matrix is what a test suite is
//! measured against; the capability report is what an implementation says about itself. All four are
//! the same list seen from four sides, and four hand-maintained copies of one list is four chances
//! for a feature to exist in three of them.
//!
//! AND A HASH, so a change to a profile is VISIBLE. A closed list that quietly grows is not closed;
//! the hash is what turns "somebody added a feature" from a thing a reviewer might notice into a
//! line in a diff.
//!
//! THE THREE CHECKS NO REVIEWER RELIABLY CATCHES, run by `--check` over the claims in the tree:
//! every `Backend`-owned feature has a handler, every feature has at least one conformance test, and
//! no test claims a feature outside the profile. Where nothing claims anything yet the check is
//! reported as NOT PERFORMED with the count it ranged over, because a check over an empty set
//! passing is not the same as a check passing.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use graphics_profile::capability::{Coverage, Range};
use graphics_profile::{FeatureOwner, ProfileEntry, RENDER2D_CORE_PROFILE_1, RENDER2D_GROUPS, RENDER2D_PROFILE_1_MIN_LIMITS, RENDER3D_CORE_PROFILE_1, RENDER3D_GROUPS};

mod graphics;
mod image;
mod opentype;
mod render2d_spec;
mod render3d_spec;
mod scan;
mod scene3d;
mod scene3d_extended;
mod selftest;
mod shader_ir;
mod thresholds;
mod wsi;

use scan::{Claim, Marker};

/// The generated form of one profile: the directory it is written under, and what it contains.
struct Profile {
	/// The directory under `docs/gen`, which is also what the gate calls this profile.
	slug: &'static str,
	title: &'static str,
	groups: &'static [&'static str],
	entries: Entries,
}

/// The entries, type-erased to what every generated document needs: a name, a group and an owner.
///
/// THE FEATURE ITSELF IS NOT NEEDED HERE. A document is written from names, and erasing the type is
/// what lets one generator write both profiles instead of one generator written twice.
struct Entries(Vec<(&'static str, &'static str, FeatureOwner)>);

impl Entries {
	fn of<F: 'static>(profile: &'static [ProfileEntry<F>]) -> Self {
		Self(profile.iter().map(|entry| (entry.name, entry.group, entry.owner)).collect())
	}
}

fn owner_name(owner: FeatureOwner) -> &'static str {
	match owner {
		FeatureOwner::GraphicsCore => "graphics-core",
		FeatureOwner::Render2D => "render2d",
		FeatureOwner::Render3D => "render3d",
		FeatureOwner::Scene3D => "scene3d",
		FeatureOwner::Backend => "backend",
	}
}

/// The digest as it is written into the document: lower-case hexadecimal, because the profile hash
/// is something a person compares by eye against a diff.
/// What a profile document fails to state about its own floors: the section, and each named limit.
///
/// A FUNCTION RATHER THAN A LOOP INSIDE THE CHECK, so the negative cases above go through exactly
/// the code the positive ones do. A self-test that reimplements the rule tests its own copy.
fn missing_minima(document: &str, names: &[String]) -> Vec<String> {
	let mut missing = Vec::new();
	if !document.contains("Guaranteed minimum") && !document.contains("Guaranteed minima") {
		missing.push(String::from("a guaranteed-minima section at all, so every limit in it is implementation-defined with no floor"));
	}
	for name in names {
		if !document.contains(name.as_str()) {
			missing.push(format!("the guaranteed minimum `{name}`"));
		}
	}
	missing
}

fn hex(digest: &[u8; 32]) -> String {
	let mut out = String::with_capacity(64);
	for byte in digest {
		let _ = write!(out, "{byte:02x}");
	}
	out
}

/// The canonical machine-readable form the hash is taken over and the documents are generated from.
///
/// ONE LINE PER FEATURE, IN PROFILE ORDER, naming the group, the owner and the feature. Order is
/// part of it: a reordering is a different document, and a hash that ignored order would call two
/// different tables the same profile.
fn canonical(profile: &Profile) -> String {
	let mut out = String::new();
	let _ = writeln!(out, "profile={}", profile.slug);
	for (name, group, owner) in &profile.entries.0 {
		let _ = writeln!(out, "feature={name} group={group} owner={}", owner_name(*owner));
	}
	if profile.slug == "render2d" {
		for (name, value) in minima() {
			let _ = writeln!(out, "limit={name} value={value}");
		}
	}
	out
}

/// The 2D guaranteed minima, named and in one order, because they are hashed with the profile: a
/// list that could be lowered without changing the hash is a list that can be lowered quietly.
fn minima() -> Vec<(&'static str, u64)> {
	let limits = RENDER2D_PROFILE_1_MIN_LIMITS;
	vec![
		("max_commands", u64::from(limits.max_commands)),
		("max_resources", u64::from(limits.max_resources)),
		("max_path_verbs", u64::from(limits.max_path_verbs)),
		("max_path_points", u64::from(limits.max_path_points)),
		("max_subpaths", u64::from(limits.max_subpaths)),
		("max_clip_depth", u64::from(limits.max_clip_depth)),
		("max_layer_depth", u64::from(limits.max_layer_depth)),
		("max_filter_nodes", u64::from(limits.max_filter_nodes)),
		("max_filter_radius", u64::from(limits.max_filter_radius)),
		("max_glyphs_per_run", u64::from(limits.max_glyphs_per_run)),
		("max_image_extent", u64::from(limits.max_image_extent)),
		("max_layer_pixels", limits.max_layer_pixels),
		("max_prepared_scratch_bytes", limits.max_prepared_scratch_bytes),
		("max_cache_bytes", limits.max_cache_bytes),
		("max_display_list_bytes", limits.max_display_list_bytes),
	]
}

fn generated_by(what: &str) -> String {
	format!("<!-- @generated by profile-doc from the {what} profile. Do not edit; run `./gen.sh`. -->\n")
}

fn table(profile: &Profile) -> String {
	let mut grouped: BTreeMap<&str, Vec<&(&str, &str, FeatureOwner)>> = BTreeMap::new();
	for entry in &profile.entries.0 {
		grouped.entry(entry.1).or_default().push(entry);
	}
	let mut out = generated_by(profile.slug);
	let _ = writeln!(out, "# {}\n", profile.title);
	let _ = writeln!(out, "Profile hash: `{}`\n", hex(&bootproto::sha256::digest(canonical(profile).as_bytes())));
	let _ = writeln!(out, "A conforming backend implements every entry below. `Unsupported` is reserved for extensions");
	let _ = writeln!(out, "added after Profile 1 and may never be returned for anything in it.\n");
	let _ = writeln!(out, "The OWNER decides which checks apply: `backend` features need a handler in every backend, and");
	let _ = writeln!(out, "the rest are implemented once above a backend and a rasteriser never sees them.\n");
	let _ = writeln!(out, "| feature | group | owner |");
	let _ = writeln!(out, "| --- | --- | --- |");
	for group in profile.groups {
		for (name, group, owner) in grouped.get(group).map(Vec::as_slice).unwrap_or_default() {
			let _ = writeln!(out, "| `{name}` | {group} | {} |", owner_name(*owner));
		}
	}
	let backend = profile.entries.0.iter().filter(|entry| entry.2 == FeatureOwner::Backend).count();
	let _ = writeln!(out, "\n{} features, of which {backend} need a handler in every backend.", profile.entries.0.len());
	if profile.slug == "render2d" {
		let _ = writeln!(out, "\n## Guaranteed minima\n");
		let _ = writeln!(out, "A conforming implementation accepts at least these and may declare more. They are what");
		let _ = writeln!(out, "\"supports Profile 1\" promises, so that it cannot mean \"accepts ten path points\".\n");
		let _ = writeln!(out, "| limit | minimum |");
		let _ = writeln!(out, "| --- | ---: |");
		for (name, value) in minima() {
			let _ = writeln!(out, "| `{name}` | {value} |");
		}
	}
	out
}

fn checklist(profile: &Profile, handled: &[Claim]) -> String {
	let mut out = generated_by(profile.slug);
	let _ = writeln!(out, "# {} - backend checklist\n", profile.title);
	let _ = writeln!(out, "What a backend has to implement, and nothing else: the entries the profile says a backend");
	let _ = writeln!(out, "owns. A backend declares one by writing `@handles: <feature>` in a comment on the code that");
	let _ = writeln!(out, "does it, which is what makes a deleted handler stop claiming coverage.\n");
	let _ = writeln!(out, "| feature | group | handled by |");
	let _ = writeln!(out, "| --- | --- | --- |");
	let mut done = 0;
	let mut total = 0;
	for group in profile.groups {
		for (name, entry_group, owner) in &profile.entries.0 {
			if entry_group != group || *owner != FeatureOwner::Backend {
				continue;
			}
			total += 1;
			let sites: Vec<String> = handled.iter().filter(|claim| claim.feature == *name).map(|claim| format!("`{}:{}`", claim.file, claim.line)).collect();
			if !sites.is_empty() {
				done += 1;
			}
			let _ = writeln!(out, "| `{name}` | {group} | {} |", if sites.is_empty() { "-".to_owned() } else { sites.join(", ") });
		}
	}
	let _ = writeln!(out, "\n{done} of {total} implemented.");
	out
}

fn matrix(profile: &Profile, covered: &[Claim]) -> String {
	let mut out = generated_by(profile.slug);
	let _ = writeln!(out, "# {} - conformance matrix\n", profile.title);
	let _ = writeln!(out, "Every feature and the tests that measure it. A test declares what it measures by writing");
	let _ = writeln!(out, "`@covers: <feature>` in a comment; a name this profile does not have is refused by the gate");
	let _ = writeln!(out, "rather than counted, because a test measuring an extension must not report Profile 1 coverage.\n");
	let _ = writeln!(out, "| feature | group | owner | tests |");
	let _ = writeln!(out, "| --- | --- | --- | --- |");
	let mut done = 0;
	for group in profile.groups {
		for (name, entry_group, owner) in &profile.entries.0 {
			if entry_group != group {
				continue;
			}
			let sites: Vec<String> = covered.iter().filter(|claim| claim.feature == *name).map(|claim| format!("`{}:{}`", claim.file, claim.line)).collect();
			if !sites.is_empty() {
				done += 1;
			}
			let _ = writeln!(out, "| `{name}` | {group} | {} | {} |", owner_name(*owner), if sites.is_empty() { "-".to_owned() } else { sites.join(", ") });
		}
	}
	let _ = writeln!(out, "\n{done} of {} features have at least one conformance test.", profile.entries.0.len());
	out
}

fn capability_report(profile: &Profile, handled: &[Claim]) -> String {
	let mut by_crate: BTreeMap<String, Vec<&Claim>> = BTreeMap::new();
	for claim in handled {
		by_crate.entry(crate_of(&claim.file)).or_default().push(claim);
	}
	let mut out = generated_by(profile.slug);
	let _ = writeln!(out, "# {} - capability report\n", profile.title);
	let _ = writeln!(out, "What each implementation in this tree says about itself, measured against the profile. An");
	let _ = writeln!(out, "implementation conforms when nothing backend-owned is missing: `Unsupported` for a profile");
	let _ = writeln!(out, "feature is not a gap, it is a failure to conform.\n");
	if by_crate.is_empty() {
		let _ = writeln!(out, "No implementation declares a handler yet, so there is nothing to report. This is the state");
		let _ = writeln!(out, "the profile was written in, on purpose: the checklist above is what the first backend works");
		let _ = writeln!(out, "through, and a checklist written after a backend is a description rather than a requirement.");
		return out;
	}
	let backend_total = profile.entries.0.iter().filter(|entry| entry.2 == FeatureOwner::Backend).count();
	for (name, claims) in by_crate {
		let declared: Vec<&str> = claims.iter().map(|claim| claim.feature.as_str()).collect();
		let missing = profile.entries.0.iter().filter(|entry| entry.2 == FeatureOwner::Backend).filter(|entry| !declared.contains(&entry.0)).count();
		let _ = writeln!(out, "## `{name}`\n");
		let _ = writeln!(out, "{} of {backend_total} backend-owned features handled.\n", backend_total - missing);
		if missing > 0 {
			let _ = writeln!(out, "Missing:\n");
			for entry in profile.entries.0.iter().filter(|entry| entry.2 == FeatureOwner::Backend).filter(|entry| !declared.contains(&entry.0)) {
				let _ = writeln!(out, "- `{}` ({})", entry.0, entry.1);
			}
			let _ = writeln!(out);
		}
	}
	out
}

/// The crate a claim was made in: the path up to `/src/`, which is what a reader calls it.
fn crate_of(file: &str) -> String {
	match file.split_once("/src/") {
		Some((crate_path, _)) => crate_path.to_owned(),
		None => file.to_owned(),
	}
}

/// One generated file: where it goes and what is in it.
struct Output {
	path: PathBuf,
	contents: String,
}

fn outputs(root: &Path, profile: &Profile, handled: &[Claim], covered: &[Claim]) -> Vec<Output> {
	let directory = root.join("docs/gen").join(profile.slug);
	vec![
		Output { path: directory.join("profile-1.canonical"), contents: canonical(profile) },
		Output { path: directory.join("profile-1.md"), contents: table(profile) },
		Output { path: directory.join("backend-checklist.md"), contents: checklist(profile, handled) },
		Output { path: directory.join("conformance-matrix.md"), contents: matrix(profile, covered) },
		Output { path: directory.join("capability-report.md"), contents: capability_report(profile, handled) },
	]
}

/// The three checks, reported one line each.
///
/// A CHECK OVER AN EMPTY SET IS NOT A PASS, and this says so in as many words rather than printing
/// "0 missing" and letting a reader take it for one.
fn checks<F: 'static>(slug: &str, entries: &'static [ProfileEntry<F>], handled: &[Claim], covered: &[Claim]) -> bool {
	let mut ok = true;
	let handled_names: Vec<&str> = handled.iter().map(|claim| claim.feature.as_str()).collect();
	let covered_names: Vec<&str> = covered.iter().map(|claim| claim.feature.as_str()).collect();
	let handlers = Coverage::new(entries, &handled_names, Range::OwnedBy(FeatureOwner::Backend));
	let coverage = Coverage::new(entries, &covered_names, Range::EveryFeature);

	// A CLAIMED NAME THE PROFILE DOES NOT HAVE IS ALWAYS AN ERROR, whether anything else is claimed
	// or not: a typo and an extension look the same from here, and both must be refused.
	for name in handlers.outside_profile() {
		let site = handled.iter().find(|claim| claim.feature == name).map(|claim| format!("{}:{}", claim.file, claim.line)).unwrap_or_default();
		eprintln!("{slug}: {site} handles `{name}`, which is not in the profile");
		ok = false;
	}
	for name in coverage.outside_profile() {
		let site = covered.iter().find(|claim| claim.feature == name).map(|claim| format!("{}:{}", claim.file, claim.line)).unwrap_or_default();
		eprintln!("{slug}: {site} covers `{name}`, which is not in the profile");
		ok = false;
	}
	for name in handlers.outside_range() {
		eprintln!("{slug}: `{name}` is handled by a backend, and the profile says no backend owns it");
		ok = false;
	}

	if handled.is_empty() {
		println!("{slug}: backend handlers NOT PERFORMED - nothing declares `@handles:`, {} features await one", handlers.required());
	} else if handlers.complete() {
		println!("{slug}: every one of the {} backend-owned features has a handler", handlers.required());
	} else {
		for entry in handlers.missing() {
			eprintln!("{slug}: `{}` ({}) has no backend handler", entry.name, entry.group);
			ok = false;
		}
	}

	if covered.is_empty() {
		println!("{slug}: conformance coverage NOT PERFORMED - nothing declares `@covers:`, {} features await a test", coverage.required());
	} else if coverage.complete() {
		println!("{slug}: every one of the {} features has at least one conformance test", coverage.required());
	} else {
		for entry in coverage.missing() {
			eprintln!("{slug}: `{}` ({}) has no conformance test", entry.name, entry.group);
			ok = false;
		}
	}
	ok
}

fn main() -> std::process::ExitCode {
	let arguments: Vec<String> = std::env::args().skip(1).collect();
	let check = arguments.iter().any(|argument| argument == "--check");
	let self_test = arguments.iter().any(|argument| argument == "--self-test");
	if let Some(unexpected) = arguments.iter().find(|argument| *argument != "--check" && *argument != "--self-test") {
		eprintln!("profile-doc: unexpected argument '{unexpected}' (usage: profile-doc [--check] [--self-test])");
		return std::process::ExitCode::FAILURE;
	}
	if self_test {
		// A TEMPORARY TREE, REMOVED AFTERWARDS. The self-test's fixtures claim features on purpose,
		// and fixtures left in the tree would be claims the real scan then found.
		let directory = std::env::temp_dir().join(format!("profile-doc-self-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&directory);
		let passed = selftest::run(&directory);
		let _ = std::fs::remove_dir_all(&directory);
		if !passed {
			return std::process::ExitCode::FAILURE;
		}
		if !check {
			return std::process::ExitCode::SUCCESS;
		}
	}
	let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
	let root = root.canonicalize().unwrap_or(root);
	let profiles = [
		Profile { slug: "render2d", title: "Render2D Core Profile 1", groups: RENDER2D_GROUPS, entries: Entries::of(RENDER2D_CORE_PROFILE_1) },
		Profile { slug: "render3d", title: "Render3D Core Profile 1", groups: RENDER3D_GROUPS, entries: Entries::of(RENDER3D_CORE_PROFILE_1) },
	];

	let source = root.join("src");
	let handled = match scan::claims(&source, Marker::Handles) {
		Ok(claims) => claims,
		Err(error) => {
			eprintln!("profile-doc: {error}");
			return std::process::ExitCode::FAILURE;
		}
	};
	let covered = match scan::claims(&source, Marker::Covers) {
		Ok(claims) => claims,
		Err(error) => {
			eprintln!("profile-doc: {error}");
			return std::process::ExitCode::FAILURE;
		}
	};

	let mut ok = true;
	for profile in &profiles {
		// EACH PROFILE SEES ONLY ITS OWN CLAIMS. The two name sets are disjoint - the crate has a
		// fixture for it - so a claim belongs to exactly one profile, and a claim in neither is
		// reported by both as outside the profile, which is what it is.
		let mine = |claims: &[Claim], known: &dyn Fn(&str) -> bool, other: &dyn Fn(&str) -> bool| -> Vec<Claim> { claims.iter().filter(|claim| known(&claim.feature) || !other(&claim.feature)).cloned().collect() };
		let in_2d = |name: &str| graphics_profile::render2d::entry_by_name(name).is_some();
		let in_3d = |name: &str| graphics_profile::render3d::entry_by_name(name).is_some();
		let (known, other): (&dyn Fn(&str) -> bool, &dyn Fn(&str) -> bool) = if profile.slug == "render2d" { (&in_2d, &in_3d) } else { (&in_3d, &in_2d) };
		let handled = mine(&handled, known, other);
		let covered = mine(&covered, known, other);

		for output in outputs(&root, profile, &handled, &covered) {
			if check {
				match std::fs::read_to_string(&output.path) {
					Ok(existing) if existing == output.contents => {}
					Ok(_) => {
						eprintln!("profile-doc: {} differs from the profile", output.path.display());
						ok = false;
					}
					Err(error) => {
						eprintln!("profile-doc: cannot read {}: {error}", output.path.display());
						ok = false;
					}
				}
			} else {
				if let Some(parent) = output.path.parent()
					&& let Err(error) = std::fs::create_dir_all(parent)
				{
					eprintln!("profile-doc: cannot create {}: {error}", parent.display());
					return std::process::ExitCode::FAILURE;
				}
				if let Err(error) = std::fs::write(&output.path, &output.contents) {
					eprintln!("profile-doc: cannot write {}: {error}", output.path.display());
					return std::process::ExitCode::FAILURE;
				}
				println!("profile-doc: wrote {}", output.path.display());
			}
		}
		let passed = if profile.slug == "render2d" { checks(profile.slug, RENDER2D_CORE_PROFILE_1, &handled, &covered) } else { checks(profile.slug, RENDER3D_CORE_PROFILE_1, &handled, &covered) };
		if !passed {
			ok = false;
		}
	}

	// THE OPENTYPE PROFILE, which is a document rather than a checklist: there is no backend handler
	// for `HVAR`, so it has the generation and the hash and none of the coverage checks. Its own
	// fixtures are what hold the list closed, and the gate runs them.
	{
		let canonical = opentype::canonical();
		let hash = hex(&bootproto::sha256::digest(canonical.as_bytes()));
		let directory = root.join("docs/gen/opentype");
		for (path, contents) in [(directory.join("profile-1.canonical"), canonical.clone()), (directory.join("profile-1.md"), opentype::document(&hash))] {
			if check {
				match std::fs::read_to_string(&path) {
					Ok(existing) if existing == contents => {}
					Ok(_) => {
						eprintln!("profile-doc: {} differs from the profile", path.display());
						ok = false;
					}
					Err(error) => {
						eprintln!("profile-doc: cannot read {}: {error}", path.display());
						ok = false;
					}
				}
				continue;
			}
			if let Some(parent) = path.parent()
				&& let Err(error) = std::fs::create_dir_all(parent)
			{
				eprintln!("profile-doc: cannot create {}: {error}", parent.display());
				return std::process::ExitCode::FAILURE;
			}
			if let Err(error) = std::fs::write(&path, &contents) {
				eprintln!("profile-doc: cannot write {}: {error}", path.display());
				return std::process::ExitCode::FAILURE;
			}
			println!("profile-doc: wrote {}", path.display());
		}
		if check {
			println!("opentype: {} tables, {} exclusions, {} scripts and {} languages, hashed", opentype_profile::tables::TABLES.len(), opentype_profile::tables::EXCLUDED.len(), opentype_profile::scripts::SCRIPTS.len(), opentype_profile::scripts::LANGUAGES.len());
		}
	}

	// THE IMAGE AND COLOUR REGISTRY, whose document is normative rather than generated-and-filed: it
	// carries the matrices, the transfer constants and the rounding rules that decide whether two
	// implementations produce the same pixels, so it goes to `docs/graphics/` where other documents
	// cite it - with its generated banner saying where it comes from.
	{
		let canonical = image::canonical();
		let hash = hex(&bootproto::sha256::digest(canonical.as_bytes()));
		for (path, contents) in [(root.join("docs/gen/image-color/profile-1.canonical"), canonical.clone()), (root.join("docs/graphics/IMAGE_COLOR_PROFILE_1.md"), image::document(&hash))] {
			if check {
				match std::fs::read_to_string(&path) {
					Ok(existing) if existing == contents => {}
					Ok(_) => {
						eprintln!("profile-doc: {} differs from the registry", path.display());
						ok = false;
					}
					Err(error) => {
						eprintln!("profile-doc: cannot read {}: {error}", path.display());
						ok = false;
					}
				}
				continue;
			}
			if let Some(parent) = path.parent()
				&& let Err(error) = std::fs::create_dir_all(parent)
			{
				eprintln!("profile-doc: cannot create {}: {error}", parent.display());
				return std::process::ExitCode::FAILURE;
			}
			if let Err(error) = std::fs::write(&path, &contents) {
				eprintln!("profile-doc: cannot write {}: {error}", path.display());
				return std::process::ExitCode::FAILURE;
			}
			println!("profile-doc: wrote {}", path.display());
		}
		if check {
			println!("image-color: {} formats, {} colour spaces, {} YUV layouts and {} matrices, hashed", graphics_profile::image::FORMATS.len(), graphics_profile::image::COLOR_SPACES.len(), graphics_profile::image::YUV_LAYOUTS.len(), graphics_profile::image::YUV_MATRICES.len());
		}
	}

	// THE LAYER REGISTRY, whose document is the naming decision the rest of the stack is written
	// against. `framebuffer` meant five things; each has its own name, and the names are generated so
	// that a sixth meaning cannot appear in prose without appearing in the registry.
	{
		let canonical = graphics::canonical();
		let hash = hex(&bootproto::sha256::digest(canonical.as_bytes()));
		for (path, contents) in [(root.join("docs/gen/graphics-layers/profile-1.canonical"), canonical.clone()), (root.join("docs/GRAPHICS.md"), graphics::document(&hash))] {
			if check {
				match std::fs::read_to_string(&path) {
					Ok(existing) if existing == contents => {}
					Ok(_) => {
						eprintln!("profile-doc: {} differs from the registry", path.display());
						ok = false;
					}
					Err(error) => {
						eprintln!("profile-doc: cannot read {}: {error}", path.display());
						ok = false;
					}
				}
				continue;
			}
			if let Some(parent) = path.parent()
				&& let Err(error) = std::fs::create_dir_all(parent)
			{
				eprintln!("profile-doc: cannot create {}: {error}", parent.display());
				return std::process::ExitCode::FAILURE;
			}
			if let Err(error) = std::fs::write(&path, &contents) {
				eprintln!("profile-doc: cannot write {}: {error}", path.display());
				return std::process::ExitCode::FAILURE;
			}
			println!("profile-doc: wrote {}", path.display());
		}
		if check {
			println!("graphics-layers: {} names, {} ownership edges, {} routes and {} validation boundaries, hashed", graphics_profile::layers::LAYERS.len(), graphics_profile::layers::OWNERSHIP.len(), graphics_profile::layers::ROUTES.len(), graphics_profile::layers::BOUNDARIES.len());
		}
	}

	// THE SCENE LAYER'S CORE PROFILE. Separate from the 3D one because they are conformed to
	// separately: a backend implements `render3d` and a library implements the scene above it.
	{
		let canonical = scene3d::canonical();
		let hash = hex(&bootproto::sha256::digest(canonical.as_bytes()));
		for (path, contents) in [(root.join("docs/gen/scene3d/profile-1.canonical"), canonical.clone()), (root.join("docs/graphics/SCENE3D_PROFILE_1.md"), scene3d::document(&hash))] {
			if check {
				match std::fs::read_to_string(&path) {
					Ok(existing) if existing == contents => {}
					Ok(_) => {
						eprintln!("profile-doc: {} differs from the registry", path.display());
						ok = false;
					}
					Err(error) => {
						eprintln!("profile-doc: cannot read {}: {error}", path.display());
						ok = false;
					}
				}
				continue;
			}
			if let Some(parent) = path.parent()
				&& let Err(error) = std::fs::create_dir_all(parent)
			{
				eprintln!("profile-doc: cannot create {}: {error}", parent.display());
				return std::process::ExitCode::FAILURE;
			}
			if let Err(error) = std::fs::write(&path, &contents) {
				eprintln!("profile-doc: cannot write {}: {error}", path.display());
				return std::process::ExitCode::FAILURE;
			}
			println!("profile-doc: wrote {}", path.display());
		}
		if check {
			println!("scene3d: {} hierarchy rules, {} camera rules, {} queues, {} culling rules, {} materials, {} lighting rules and {} minimum limits, hashed", graphics_profile::scene3d::HIERARCHY_RULES.len(), graphics_profile::scene3d::CAMERA_RULES.len(), graphics_profile::scene3d::QUEUES.len(), graphics_profile::scene3d::CULLING_RULES.len(), graphics_profile::scene3d::MATERIALS.len(), graphics_profile::scene3d::LIGHTING_RULES.len(), graphics_profile::scene3d::SCENE3D_PROFILE_1_MIN_LIMITS.len());
		}
	}

	// EVERY PROFILE DOCUMENT STATES ITS OWN GUARANTEED MINIMA, and this is the check that it does.
	//
	// AN IMPLEMENTATION-DEFINED LIMIT WITHOUT A FLOOR LETS A BACKEND PASS CONFORMANCE WHILE BEING
	// USELESS: a maximum texture extent of 16, a maximum draw count of 1 or a maximum shader length
	// of 8 satisfies every other sentence in these documents. The floors are in the registries; what
	// this refuses is a DOCUMENT that does not carry them, because a floor a reader cannot find is a
	// floor nobody implements against.
	if check {
		let published: &[(&str, Vec<String>)] = &[
			(
				"docs/graphics/RENDER2D_PROFILE_1.md",
				// EVERY FIELD OF `Render2DLimits`, not a sample of them: a check over four of fifteen
				// would approve a document that lost the other eleven.
				[
					"max_commands",
					"max_resources",
					"max_path_verbs",
					"max_path_points",
					"max_subpaths",
					"max_clip_depth",
					"max_layer_depth",
					"max_filter_nodes",
					"max_filter_radius",
					"max_glyphs_per_run",
					"max_image_extent",
					"max_layer_pixels",
					"max_prepared_scratch_bytes",
					"max_cache_bytes",
					"max_display_list_bytes",
				]
				.iter()
				.map(|name| String::from(*name))
				.collect(),
			),
			("docs/graphics/RENDER3D_PROFILE_1.md", graphics_profile::render3d_spec::RENDER3D_PROFILE_1_MIN_LIMITS.iter().map(|limit| String::from(limit.name)).collect()),
			("docs/graphics/SCENE3D_PROFILE_1.md", graphics_profile::scene3d::SCENE3D_PROFILE_1_MIN_LIMITS.iter().map(|limit| String::from(limit.name)).collect()),
			("docs/graphics/SCENE3D_EXTENDED_1.md", graphics_profile::scene3d_extended::SCENE3D_EXTENDED_1_MIN_LIMITS.iter().map(|limit| String::from(limit.name)).collect()),
		];
		// IT PROVES IT REFUSES BEFORE IT IS TRUSTED TO APPROVE, and through the SAME function: the
		// tree is consistent right now, so a run over the tree alone would pass just as well if the
		// check had stopped looking. Two negative cases - a document with no section at all, and one
		// with the section and a floor missing from it.
		let without_section = missing_minima("# X\n\nno floors here\n", &[String::from("max_nodes")]);
		let without_floor = missing_minima("# X\n\n## Guaranteed minimum limits\n\n| `max_nodes` | 1 |\n", &[String::from("max_nodes"), String::from("max_hierarchy_depth")]);
		if without_section.len() != 2 || without_floor != vec![String::from("the guaranteed minimum `max_hierarchy_depth`")] {
			eprintln!("profile-doc: SELF-TEST FAILED - the minima check did not refuse a document that states no floors");
			ok = false;
		}
		for (path, names) in published {
			let document = match std::fs::read_to_string(root.join(path)) {
				Ok(document) => document,
				Err(error) => {
					eprintln!("profile-doc: cannot read {path}: {error}");
					ok = false;
					continue;
				}
			};
			for problem in missing_minima(&document, names) {
				eprintln!("profile-doc: {path} does not state {problem}");
				ok = false;
			}
		}
		if ok {
			println!("minima: {} profile documents state every floor their registries publish, and the check refuses one that does not", published.len());
			// AND EVERY PROFILE THAT IS COMPARED HAS THRESHOLDS. A profile with none is one whose
			// suite either demands bit-exactness or demands nothing, and both are ways of not
			// checking - so the count is reported rather than assumed.
			let mut thresholds_ok = true;
			for profile in ["image-colour", "render2d", "render3d", "scene3d", "scene3d-extended"] {
				let count = thresholds::count(profile);
				if count == 0 {
					eprintln!("profile-doc: `{profile}` publishes no conformance threshold, so nothing bounds a comparison against it");
					thresholds_ok = false;
				}
			}
			ok &= thresholds_ok;
			if thresholds_ok {
				println!("thresholds: image-colour {}, render2d {}, render3d {}, scene3d {}, scene3d-extended {}", thresholds::count("image-colour"), thresholds::count("render2d"), thresholds::count("render3d"), thresholds::count("scene3d"), thresholds::count("scene3d-extended"));
			}
		}
	}

	// THE SCENE LAYER'S EXTENDED PROFILE. Its own hash, because conforming to it is a separate claim.
	{
		let canonical = scene3d_extended::canonical();
		let hash = hex(&bootproto::sha256::digest(canonical.as_bytes()));
		for (path, contents) in [
			(root.join("docs/gen/scene3d-extended/profile-1.canonical"), canonical.clone()),
			(root.join("docs/graphics/SCENE3D_EXTENDED_1.md"), scene3d_extended::document(&hash)),
		] {
			if check {
				match std::fs::read_to_string(&path) {
					Ok(existing) if existing == contents => {}
					Ok(_) => {
						eprintln!("profile-doc: {} differs from the registry", path.display());
						ok = false;
					}
					Err(error) => {
						eprintln!("profile-doc: cannot read {}: {error}", path.display());
						ok = false;
					}
				}
				continue;
			}
			if let Some(parent) = path.parent()
				&& let Err(error) = std::fs::create_dir_all(parent)
			{
				eprintln!("profile-doc: cannot create {}: {error}", parent.display());
				return std::process::ExitCode::FAILURE;
			}
			if let Err(error) = std::fs::write(&path, &contents) {
				eprintln!("profile-doc: cannot write {}: {error}", path.display());
				return std::process::ExitCode::FAILURE;
			}
			println!("profile-doc: wrote {}", path.display());
		}
		if check {
			println!("scene3d-extended: {} PBR terms, {} constants, {} environment rules, {} shadow rules, {} post-process rules, {} animation rules and {} minimum limits, hashed", graphics_profile::scene3d_extended::PBR_TERMS.len(), graphics_profile::scene3d_extended::PBR_CONSTANTS.len(), graphics_profile::scene3d_extended::ENVIRONMENT_RULES.len(), graphics_profile::scene3d_extended::SHADOW_RULES.len(), graphics_profile::scene3d_extended::POSTPROCESS_RULES.len(), graphics_profile::scene3d_extended::ANIMATION_RULES.len(), graphics_profile::scene3d_extended::SCENE3D_EXTENDED_1_MIN_LIMITS.len());
		}
	}

	// THE SHADER IR'S SEMANTICS, frozen with the 3D specification because a position's arithmetic is
	// half in each: the clipping rules are there and the rule that makes the arithmetic reproducible
	// is here.
	{
		let canonical = shader_ir::canonical();
		let hash = hex(&bootproto::sha256::digest(canonical.as_bytes()));
		for (path, contents) in [(root.join("docs/gen/shader-ir/profile-1.canonical"), canonical.clone()), (root.join("docs/graphics/SHADER_IR_1.md"), shader_ir::document(&hash))] {
			if check {
				match std::fs::read_to_string(&path) {
					Ok(existing) if existing == contents => {}
					Ok(_) => {
						eprintln!("profile-doc: {} differs from the registry", path.display());
						ok = false;
					}
					Err(error) => {
						eprintln!("profile-doc: cannot read {}: {error}", path.display());
						ok = false;
					}
				}
				continue;
			}
			if let Some(parent) = path.parent()
				&& let Err(error) = std::fs::create_dir_all(parent)
			{
				eprintln!("profile-doc: cannot create {}: {error}", parent.display());
				return std::process::ExitCode::FAILURE;
			}
			if let Err(error) = std::fs::write(&path, &contents) {
				eprintln!("profile-doc: cannot write {}: {error}", path.display());
				return std::process::ExitCode::FAILURE;
			}
			println!("profile-doc: wrote {}", path.display());
		}
		if check {
			println!("shader-ir: {} numeric answers, {} conversions, {} accuracy bounds, {} layout rules, {} stage rules, {} strict-f32 rules and {} encoding rules, hashed", graphics_profile::shader_ir::NUMERIC_RULES.len(), graphics_profile::shader_ir::CONVERSION_RULES.len(), graphics_profile::shader_ir::TRANSCENDENTAL_ACCURACY.len(), graphics_profile::shader_ir::UNIFORM_LAYOUT.len() + graphics_profile::shader_ir::MATRIX_LAYOUT.len(), graphics_profile::shader_ir::STAGE_RULES.len(), graphics_profile::shader_ir::STRICT_F32_RULES.len(), graphics_profile::shader_ir::ENCODING_RULES.len());
		}
	}

	// THE 3D SPECIFICATION, frozen before `render3d` and `soft3d` exist. The feature list says what a
	// backend must be able to do; this says what the answers are, and two backends that implement
	// every feature can still produce different images if these are left open.
	{
		let canonical = render3d_spec::canonical();
		let hash = hex(&bootproto::sha256::digest(canonical.as_bytes()));
		for (path, contents) in [(root.join("docs/gen/render3d-spec/profile-1.canonical"), canonical.clone()), (root.join("docs/graphics/RENDER3D_PROFILE_1.md"), render3d_spec::document(&hash))] {
			if check {
				match std::fs::read_to_string(&path) {
					Ok(existing) if existing == contents => {}
					Ok(_) => {
						eprintln!("profile-doc: {} differs from the registry", path.display());
						ok = false;
					}
					Err(error) => {
						eprintln!("profile-doc: cannot read {}: {error}", path.display());
						ok = false;
					}
				}
				continue;
			}
			if let Some(parent) = path.parent()
				&& let Err(error) = std::fs::create_dir_all(parent)
			{
				eprintln!("profile-doc: cannot create {}: {error}", parent.display());
				return std::process::ExitCode::FAILURE;
			}
			if let Err(error) = std::fs::write(&path, &contents) {
				eprintln!("profile-doc: cannot write {}: {error}", path.display());
				return std::process::ExitCode::FAILURE;
			}
			println!("profile-doc: wrote {}", path.display());
		}
		if check {
			println!("render3d-spec: {} colour formats, {} vertex formats, {} interpolation qualifiers, {} MSAA answers, {} depth formats, {} sampler answers and {} minimum limits, hashed", graphics_profile::render3d_spec::COLOUR_FORMATS.len(), graphics_profile::render3d_spec::VERTEX_FORMATS.len(), graphics_profile::render3d_spec::QUALIFIERS.len(), graphics_profile::render3d_spec::MSAA_RULES.len(), graphics_profile::render3d_spec::DEPTH_FORMATS.len(), graphics_profile::render3d_spec::SAMPLER_RULES.len(), graphics_profile::render3d_spec::RENDER3D_PROFILE_1_MIN_LIMITS.len());
		}
	}

	// THE WINDOW-SYSTEM PROFILE. Frozen before `a-wsi` implements it, because every number in it is
	// one two sides round differently if it is not stated.
	{
		let canonical = wsi::canonical();
		let hash = hex(&bootproto::sha256::digest(canonical.as_bytes()));
		for (path, contents) in [(root.join("docs/gen/wsi/profile-1.canonical"), canonical.clone()), (root.join("docs/graphics/WSI_PROFILE_1.md"), wsi::document(&hash))] {
			if check {
				match std::fs::read_to_string(&path) {
					Ok(existing) if existing == contents => {}
					Ok(_) => {
						eprintln!("profile-doc: {} differs from the registry", path.display());
						ok = false;
					}
					Err(error) => {
						eprintln!("profile-doc: cannot read {}: {error}", path.display());
						ok = false;
					}
				}
				continue;
			}
			if let Some(parent) = path.parent()
				&& let Err(error) = std::fs::create_dir_all(parent)
			{
				eprintln!("profile-doc: cannot create {}: {error}", parent.display());
				return std::process::ExitCode::FAILURE;
			}
			if let Err(error) = std::fs::write(&path, &contents) {
				eprintln!("profile-doc: cannot write {}: {error}", path.display());
				return std::process::ExitCode::FAILURE;
			}
			println!("profile-doc: wrote {}", path.display());
		}
		if check {
			println!("wsi: {} configuration fields, {} events, {} image transitions, {} present outcomes and {} damage answers, hashed", graphics_profile::wsi::CONFIGURATION.len(), graphics_profile::wsi::EVENTS.len(), graphics_profile::wsi::TRANSITIONS.len(), graphics_profile::wsi::PRESENT_OUTCOMES.len(), graphics_profile::wsi::DAMAGE_RULES.len());
		}
	}

	// THE RENDER2D SPECIFICATION. The feature list says what the profile carries; this says what each
	// entry MEANS numerically, which is the half two implementations can agree on in name and differ
	// on in pixels.
	{
		let canonical = render2d_spec::canonical();
		let hash = hex(&bootproto::sha256::digest(canonical.as_bytes()));
		for (path, contents) in [(root.join("docs/gen/render2d-spec/profile-1.canonical"), canonical.clone()), (root.join("docs/graphics/RENDER2D_PROFILE_1.md"), render2d_spec::document(&hash))] {
			if check {
				match std::fs::read_to_string(&path) {
					Ok(existing) if existing == contents => {}
					Ok(_) => {
						eprintln!("profile-doc: {} differs from the registry", path.display());
						ok = false;
					}
					Err(error) => {
						eprintln!("profile-doc: cannot read {}: {error}", path.display());
						ok = false;
					}
				}
				continue;
			}
			if let Some(parent) = path.parent()
				&& let Err(error) = std::fs::create_dir_all(parent)
			{
				eprintln!("profile-doc: cannot create {}: {error}", parent.display());
				return std::process::ExitCode::FAILURE;
			}
			if let Err(error) = std::fs::write(&path, &contents) {
				eprintln!("profile-doc: cannot write {}: {error}", path.display());
				return std::process::ExitCode::FAILURE;
			}
			println!("profile-doc: wrote {}", path.display());
		}
		if check {
			println!("render2d-spec: {} operators, {} separable and {} non-separable blend modes, {} boolean answers and {} prepared dependencies, hashed", graphics_profile::compositing::OPERATORS.len(), graphics_profile::compositing::BLENDS.len(), graphics_profile::compositing::NON_SEPARABLE_BLENDS.len(), graphics_profile::geometry::BOOLEAN_RULES.len(), graphics_profile::contracts::PREPARED_DEPENDENCIES.len());
		}
	}

	// THE MANIFEST THAT BINDS A DOCUMENT TO ITS PROFILE.
	//
	// The semantic input is the REGISTRY and never the Markdown: hashing the prose would make
	// rewrapping a paragraph a profile change, and would let a corrected number pass unnoticed if the
	// sentence around it was rewritten in the same commit. So the document is bound INSTEAD - it
	// states the hash, the manifest records it, and this recomputes the hash from the registry and
	// refuses any of the three that disagrees.
	{
		let profiles: &[(&str, u32, String, &str, &str)] = &[
			("image-colour", 1, image::canonical(), "docs/gen/image-color/profile-1.canonical", "docs/graphics/IMAGE_COLOR_PROFILE_1.md"),
			("render2d-spec", 1, render2d_spec::canonical(), "docs/gen/render2d-spec/profile-1.canonical", "docs/graphics/RENDER2D_PROFILE_1.md"),
			("render3d-spec", 1, render3d_spec::canonical(), "docs/gen/render3d-spec/profile-1.canonical", "docs/graphics/RENDER3D_PROFILE_1.md"),
			("shader-ir", graphics_profile::shader_ir::SHADER_IR_VERSION, shader_ir::canonical(), "docs/gen/shader-ir/profile-1.canonical", "docs/graphics/SHADER_IR_1.md"),
			("scene3d", 1, scene3d::canonical(), "docs/gen/scene3d/profile-1.canonical", "docs/graphics/SCENE3D_PROFILE_1.md"),
			("scene3d-extended", 1, scene3d_extended::canonical(), "docs/gen/scene3d-extended/profile-1.canonical", "docs/graphics/SCENE3D_EXTENDED_1.md"),
			("wsi", 1, wsi::canonical(), "docs/gen/wsi/profile-1.canonical", "docs/graphics/WSI_PROFILE_1.md"),
		];
		let mut manifest = String::from("# @generated by profile-doc. Do not edit; run `./gen.sh`.\n");
		manifest.push_str("# The semantic input of a profile is its REGISTRY. The document is the normative explanation and\n");
		manifest.push_str("# is bound to the profile here rather than hashed as text - see `graphics_profile::hashing`.\n");
		for (name, version, canonical, canonical_path, document_path) in profiles {
			let hash = hex(&bootproto::sha256::digest(canonical.as_bytes()));
			manifest.push_str(&format!("profile={name} version={version} hash={hash} canonical={canonical_path} document={document_path}\n"));
			if !check {
				continue;
			}
			// THE DOCUMENT STATES ITS OWN HASH, and this is where the statement is checked against
			// the registry that produced it.
			match std::fs::read_to_string(root.join(document_path)) {
				Ok(document) => {
					if !document.contains(&hash) {
						eprintln!("profile-doc: {document_path} does not state the hash `{hash}` its registry produces");
						ok = false;
					}
				}
				Err(error) => {
					eprintln!("profile-doc: cannot read {document_path}: {error}");
					ok = false;
				}
			}
			// AND THE CANONICAL FILE IS THE ONE THE HASH WAS TAKEN OVER, which the per-profile blocks
			// above already compare byte for byte; this is the check that the pair on disk belongs
			// together rather than being two files that happen to exist.
			match std::fs::read_to_string(root.join(canonical_path)) {
				Ok(existing) if hex(&bootproto::sha256::digest(existing.as_bytes())) == hash => {}
				Ok(_) => {
					eprintln!("profile-doc: {canonical_path} does not hash to `{hash}`");
					ok = false;
				}
				Err(error) => {
					eprintln!("profile-doc: cannot read {canonical_path}: {error}");
					ok = false;
				}
			}
		}
		let manifest_path = root.join("docs/gen/profiles.manifest");
		if check {
			match std::fs::read_to_string(&manifest_path) {
				Ok(existing) if existing == manifest => {}
				Ok(_) => {
					eprintln!("profile-doc: docs/gen/profiles.manifest does not match the profiles it names");
					ok = false;
				}
				Err(error) => {
					eprintln!("profile-doc: cannot read docs/gen/profiles.manifest: {error}");
					ok = false;
				}
			}
			if ok {
				println!("manifest: {} profiles bound to their documents by hash and version", profiles.len());
			}
		} else {
			if let Some(parent) = manifest_path.parent()
				&& let Err(error) = std::fs::create_dir_all(parent)
			{
				eprintln!("profile-doc: cannot create {}: {error}", parent.display());
				return std::process::ExitCode::FAILURE;
			}
			if let Err(error) = std::fs::write(&manifest_path, &manifest) {
				eprintln!("profile-doc: cannot write {}: {error}", manifest_path.display());
				return std::process::ExitCode::FAILURE;
			}
			println!("profile-doc: wrote {}", manifest_path.display());
		}
	}

	if !ok {
		if check {
			eprintln!("profile-doc: regenerate with `cargo run --manifest-path src/tools/profile-doc/Cargo.toml`");
		}
		return std::process::ExitCode::FAILURE;
	}
	if check {
		println!("profile-doc: the generated documents match every profile");
	}
	std::process::ExitCode::SUCCESS
}
