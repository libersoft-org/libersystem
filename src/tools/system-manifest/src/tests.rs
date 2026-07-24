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

[[services]]
name = "tool_service"
program = "tool"
restart = "escalate"
dependencies = []
"#
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
