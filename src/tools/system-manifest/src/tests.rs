use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture_workspace() -> PathBuf {
	let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
	let root = std::env::temp_dir().join(format!("liber-system-manifest-{}-{nonce}", std::process::id()));
	fs::create_dir_all(root.join("user/libs/audio/pcm")).unwrap();
	fs::create_dir_all(root.join("user/apps/tool")).unwrap();
	fs::write(root.join("user/libs/audio/pcm/Cargo.toml"), "[package]\nname='pcm'\nversion='0.0.0'\n").unwrap();
	fs::write(root.join("user/apps/tool/Cargo.toml"), "[package]\nname='tool'\nversion='0.0.0'\n").unwrap();
	root
}

fn valid_fixture() -> &'static str {
	r#"
schema = 1

[[sources]]
owner = "pcm"
path = "user/libs/audio/pcm"

[[sources]]
owner = "tool"
path = "user/apps/tool"

[[libraries]]
name = "pcm"
owner = "pcm"
destination = "lib/audio/pcm.lslib"

[[programs]]
name = "tool"
owner = "tool"
role = "tool"
linkage = "dynamic"
stage = "volume"
destination = "bin/tool.lsexe"
providers = ["pcm"]

[[factory_files]]
name = "liber-component"
kind = "sdk-component"
owner = "liber_component"
destination = "components/liber_component/app.wasm"

[[services]]
name = "tool_service"
program = "tool"
restart = "escalate"
dependencies = []

# An image without a kernel or a loader does not boot, so `Manifest::parse` requires all four
# kinds. This fixture predated that rule and had stopped parsing at all - which nothing noticed,
# because no gate ran this crate's suite. That is the inventory defect P02M0118 was written about,
# found by the gate that now runs all fifty-eight of them.
#
# One owner for all four: the rule under test is that each KIND is provided exactly once, and
# spreading them over four sources would also change what `sources.len()` asserts below.
[[boot_artifacts]]
name = "kernel"
owner = "tool"
kind = "kernel"
destination = "kernel"

[[boot_artifacts]]
name = "loader"
owner = "tool"
kind = "loader"
destination = "EFI/BOOT/BOOTX64.EFI"

[[boot_artifacts]]
name = "init-package"
owner = "tool"
kind = "init-package"
destination = "init.pkg"

[[boot_artifacts]]
name = "volume-package"
owner = "tool"
kind = "volume-package"
destination = "volume.pkg"
"#
}

#[test]
fn every_boot_artifact_kind_is_required() {
	// The rule the fixture above had silently stopped satisfying. Asserted directly so the next
	// person to touch the fixture learns which four kinds are mandatory from a failure that says
	// so, rather than from four unrelated tests going red at once.
	let root = fixture_workspace();
	for (name, kind, destination) in [
		("kernel", "kernel", "kernel"),
		("loader", "loader", "EFI/BOOT/BOOTX64.EFI"),
		("init-package", "init-package", "init.pkg"),
		("volume-package", "volume-package", "volume.pkg"),
	] {
		let stanza = format!("[[boot_artifacts]]\nname = \"{name}\"\nowner = \"tool\"\nkind = \"{kind}\"\ndestination = \"{destination}\"\n");
		let without = valid_fixture().replace(&stanza, "");
		assert_ne!(without, valid_fixture(), "the {kind} stanza must actually be removed, or this proves nothing");
		let error = Manifest::parse(&without, &root).unwrap_err().to_string();
		assert!(error.contains("no artifact provides"), "an image with no {kind} must be refused, got: {error}");
	}
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn canonical_model_sorts_entities_and_edges() {
	let root = fixture_workspace();
	let manifest = Manifest::parse(valid_fixture(), &root).unwrap();
	assert_eq!(manifest.sources.len(), 2);
	assert_eq!(manifest.programs.len(), 1);
	assert_eq!(manifest.services.len(), 1);
	assert_eq!(manifest.libraries.len(), 1);
	assert_eq!(manifest.source_path("pcm"), Some("user/libs/audio/pcm"));
	let canonical = manifest.canonical_json().unwrap();
	assert!(canonical.contains("\"tool_service\""));
	let first = "[[sources]]\nowner = \"pcm\"\npath = \"user/libs/audio/pcm\"\n\n[[sources]]\nowner = \"tool\"\npath = \"user/apps/tool\"";
	let second = "[[sources]]\nowner = \"tool\"\npath = \"user/apps/tool\"\n\n[[sources]]\nowner = \"pcm\"\npath = \"user/libs/audio/pcm\"";
	let reordered = valid_fixture().replace(first, second);
	assert_eq!(Manifest::parse(&reordered, &root).unwrap().canonical_json().unwrap(), canonical);
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn validation_aggregates_schema_reference_path_and_cycle_errors() {
	let root = fixture_workspace();
	let invalid = valid_fixture().replace("schema = 1", "schema = 9").replace("providers = [\"pcm\"]", "providers = [\"missing\"]").replace("dependencies = []", "dependencies = [\"tool_service\"]").replace("destination = \"bin/tool.lsexe\"", "destination = \"../tool\"");
	let error = Manifest::parse(&invalid, &root).unwrap_err().to_string();
	assert!(error.contains("unsupported version 9"));
	assert!(error.contains("unknown library missing"));
	assert!(error.contains("invalid normalized relative path"));
	assert!(error.contains("self dependency edge"));
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn unknown_fields_fail_during_deserialization() {
	let root = fixture_workspace();
	let error = Manifest::parse(&valid_fixture().replace("schema = 1", "schema = 1\nextra = true"), &root).unwrap_err().to_string();
	assert!(error.contains("unknown field"));
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_built_payload_must_name_what_builds_it() {
	// The SDK payload is the one staged artifact with no source path, and its provenance is what
	// puts `src/sdk` in the system volume's freshness model. Without an owner the volume's staleness
	// digest could not see an SDK edit at all: change the SDK, rebuild nothing, and a guest ran the
	// previous `app.wasm` against a digest that agreed the volume was current.
	let root = fixture_workspace();
	let without_owner = valid_fixture().replace("owner = \"liber_component\"\n", "");
	let error = Manifest::parse(&without_owner, &root).expect_err("an sdk-component with no owner must be refused");
	assert!(format!("{error:?}").contains("require the component that builds them"), "got {error:?}");

	// And the converse: a checked-in source file takes its provenance from its path, so naming an
	// owner there would be a second answer to a question that already has one.
	fs::create_dir_all(root.join("volume")).unwrap();
	fs::write(root.join("volume/hello.txt"), "hello").unwrap();
	let source_with_owner = format!("{}\n[[factory_files]]\nname = \"hello\"\nkind = \"source\"\nsource = \"volume/hello.txt\"\nowner = \"liber_component\"\ndestination = \"hello.txt\"\n", valid_fixture());
	let error = Manifest::parse(&source_with_owner, &root).expect_err("a source factory file with an owner must be refused");
	assert!(format!("{error:?}").contains("take their provenance from their source path"), "got {error:?}");
}

#[test]
fn factory_files_cover_the_source_tree_and_enforce_the_layout() {
	let root = fixture_workspace();
	fs::create_dir_all(root.join("volume/audio")).unwrap();
	fs::write(root.join("volume/hello.txt"), "hello").unwrap();
	fs::write(root.join("volume/audio/test.mp3"), "mp3").unwrap();
	let fixture = format!("{}\n[[factory_files]]\nname = \"hello\"\nkind = \"source\"\nsource = \"volume/hello.txt\"\ndestination = \"hello.txt\"\n\n[[factory_files]]\nname = \"audio-demo\"\nkind = \"source\"\nsource = \"volume/audio/test.mp3\"\ndestination = \"audio/test.mp3\"\n", valid_fixture());
	let manifest = Manifest::parse(&fixture, &root).unwrap();
	assert_eq!(manifest.factory_files.len(), 3);
	let invalid = fixture.replace("destination = \"audio/test.mp3\"", "destination = \"share/test.mp3\"");
	let error = Manifest::parse(&invalid, &root).unwrap_err().to_string();
	assert!(error.contains("expected audio/test.mp3"));
	let without_component = fixture.replace("\n[[factory_files]]\nname = \"liber-component\"\nkind = \"sdk-component\"\nowner = \"liber_component\"\ndestination = \"components/liber_component/app.wasm\"\n", "");
	let error = Manifest::parse(&without_component, &root).unwrap_err().to_string();
	assert!(error.contains("no SDK component payload is declared"));
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn volume_program_roles_have_exact_destinations() {
	let root = fixture_workspace();
	let helper = valid_fixture().replace("role = \"tool\"", "role = \"helper\"").replace("destination = \"bin/tool.lsexe\"", "destination = \"libexec/tool.lsexe\"");
	Manifest::parse(&helper, &root).unwrap();
	let nested = helper.replace("destination = \"libexec/tool.lsexe\"", "destination = \"libexec/tool/private/tool.lsexe\"");
	let error = Manifest::parse(&nested, &root).unwrap_err().to_string();
	assert!(error.contains("expected libexec/tool.lsexe"));
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn runtime_paths_require_their_declared_owner_and_destination() {
	let root = fixture_workspace();
	fs::create_dir_all(root.join("user/services/core")).unwrap();
	fs::write(root.join("user/services/core/Cargo.toml"), "[package]\nname='services'\nversion='0.0.0'\n").unwrap();
	let fixture = format!("{}\n[[sources]]\nowner = \"services\"\npath = \"user/services/core\"\n\n[[programs]]\nname = \"config_service\"\nowner = \"services\"\nrole = \"service\"\nlinkage = \"dynamic\"\nstage = \"volume\"\ndestination = \"libexec/config_service/config_service.lsexe\"\n\n[[runtime_paths]]\nname = \"config-tree\"\nowner = \"config_service\"\ndestination = \"libexec/config_service/config.tree\"\n", valid_fixture());
	Manifest::parse(&fixture, &root).unwrap();
	let invalid = fixture.replace("destination = \"libexec/config_service/config.tree\"", "destination = \"config.tree\"");
	let error = Manifest::parse(&invalid, &root).unwrap_err().to_string();
	assert!(error.contains("expected libexec/config_service/config.tree"));
	fs::remove_dir_all(root).unwrap();
}
