// Identity: what a verification run actually ran over - ONE source identity, and the closed set of
// inputs that can influence what a run does.
//
// TWO ANSWERS USED TO EXIST. `lib.sh`'s `source_digest` hashes selected directories and extensions
// (fast, used by every build stamp), and this crate's change model hashed HEAD plus the changed
// paths. Neither covered the firmware, the QEMU binary, the feature flags or the environment
// overrides that select them. The identity here is the one both places consume from now on: for a
// clean checkout the Git tree identity, labelled as such; otherwise a NUL-safe digest over every
// tracked path with its mode and its working-tree bytes, plus the lockfiles and the declared
// generated inputs. A tracked file that is missing from the working tree is part of the identity
// too, as its absence.
//
// THE INFLUENCING-INPUT MANIFEST is closed: the resolved path, digest and version of every tool a
// run invokes, the firmware images by digest (not by package name), the cargo configurations and
// toolchain pins, and exactly the environment variables permitted to influence a run. An override
// outside that list is REFUSED rather than recorded, because a manifest that records whatever was
// set has described the run without bounding it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::evidence::{hex, sha256_file};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
	// `git-tree` for a clean checkout (the tree object id), `working-tree` otherwise.
	pub kind: String,
	pub value: String,
	pub head: String,
	pub tracked_files: usize,
	pub changed_paths: usize,
	pub lockfiles: BTreeMap<String, String>,
	pub generated_inputs: BTreeMap<String, String>,
	pub submodules: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
	pub name: String,
	pub path: String,
	pub sha256: String,
	pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
	pub schema: String,
	pub source: SourceIdentity,
	pub tools: Vec<Tool>,
	pub firmware: BTreeMap<String, String>,
	pub configurations: BTreeMap<String, String>,
	pub environment: BTreeMap<String, String>,
}

pub const IDENTITY_SCHEMA: &str = "libersystem-identity/1";

// The tools a run invokes. Missing ones are recorded as absent rather than skipped, so a run on a
// machine without one says so.
pub const TOOLS: [&str; 16] = [
	"cargo",
	"rustc",
	"qemu-system-x86_64",
	"qemu-system-aarch64",
	"qemu-system-riscv64",
	"qemu-img",
	"xorriso",
	"mformat",
	"mcopy",
	"mkfs.fat",
	"sbsign",
	"sbverify",
	"virt-fw-vars",
	"python3",
	"objcopy",
	"llvm-strip",
];

// The firmware images the runners open, by the environment variable that selects each and its
// default path.
pub const FIRMWARE: [(&str, &str); 6] = [
	("OVMF_CODE", "/usr/share/OVMF/OVMF_CODE_4M.fd"),
	("OVMF_VARS_SRC", "/usr/share/OVMF/OVMF_VARS_4M.fd"),
	("OVMF_SECBOOT", "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd"),
	("AAVMF_CODE", "/usr/share/AAVMF/AAVMF_CODE.fd"),
	("AAVMF_VARS", "/usr/share/AAVMF/AAVMF_VARS.fd"),
	("UBOOT", "/usr/lib/u-boot/qemu-riscv64_smode/u-boot.bin"),
];

// The configuration files whose bytes decide how the compilers are invoked.
pub const CONFIGURATIONS: [&str; 9] = [
	"src/kernel/.cargo/config.toml",
	"src/kernel/rust-toolchain.toml",
	"src/user/.cargo/config.toml",
	"src/user/rust-toolchain.toml",
	"src/user/x86_64-unknown-none.json",
	"src/boot/loader/.cargo/config.toml",
	"src/sdk/.cargo/config.toml",
	"src/sdk/rust-toolchain.toml",
	"src/.cargo/config.toml",
];

// The environment variables permitted to influence a run, and the prefixes under which any other
// variable is an UNDECLARED OVERRIDE that refuses the run.
pub const PERMITTED_ENVIRONMENT: [&str; 24] = [
	"LIBER_RUN_MODE",
	"LIBER_DMA_MODE",
	"LIBER_DEVELOPMENT",
	"LIBER_SECURITY_GENERATION",
	"LIBER_MANIFEST_PURPOSE",
	"LIBER_TRUST_PROFILE",
	"LIBER_TRUST_KEY",
	"LIBER_TRUST_KEY_ID",
	"LIBER_VERIFY_RUN",
	"LIBER_PUBLISH_LOCK",
	"LIBER_GATE_KEY",
	"LIBER_CONCURRENT_GUESTS",
	"LIBER_KERNEL_STRIP",
	"LIBER_NO_DT_PROFILE",
	"OVMF_CODE",
	"OVMF_VARS_SRC",
	"OVMF_SECBOOT",
	"AAVMF_CODE",
	"AAVMF_VARS",
	"UBOOT",
	"QEMU_EXTRA",
	"SOURCE_DATE_EPOCH",
	"TEST_TAGS",
	"TEST_SELECTION",
];
pub const GUARDED_PREFIXES: [&str; 5] = ["LIBER_", "OVMF_", "AAVMF_", "QEMU_", "TEST_"];

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
	let output = Command::new("git").args(args).current_dir(repo).output().map_err(|error| format!("git {}: {error}", args.join(" ")))?;
	if !output.status.success() {
		return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim()));
	}
	Ok(output.stdout)
}

// The source identity of `repo`. NUL-safe: paths come from `git ls-files -z --stage`, so a
// newline in a name cannot split an entry.
pub fn source_identity(repo: &Path) -> Result<SourceIdentity, String> {
	let head = String::from_utf8_lossy(&git(repo, &["rev-parse", "HEAD"])?).trim().to_string();
	let status = git(repo, &["status", "--porcelain", "-z", "--untracked-files=no"])?;
	let changed_paths = status.split(|b| *b == 0).filter(|entry| !entry.is_empty()).count();
	let staged = git(repo, &["ls-files", "-z", "--stage"])?;
	let mut hasher = Sha256::new();
	let mut tracked = 0usize;
	for entry in staged.split(|b| *b == 0).filter(|e| !e.is_empty()) {
		// `<mode> <object> <stage>\t<path>`
		let text = String::from_utf8_lossy(entry).to_string();
		let (meta, path) = text.split_once('\t').ok_or_else(|| format!("unparsable ls-files entry: {text}"))?;
		let mode = meta.split(' ').next().unwrap_or("");
		tracked += 1;
		hasher.update(mode.as_bytes());
		hasher.update(b"\0");
		hasher.update(path.as_bytes());
		hasher.update(b"\0");
		match std::fs::read(repo.join(path)) {
			Ok(bytes) => {
				hasher.update((bytes.len() as u64).to_le_bytes());
				hasher.update(&bytes);
			}
			Err(_) => hasher.update(b"\0absent\0"),
		}
		hasher.update(b"\0");
	}
	let submodules: Vec<String> = String::from_utf8_lossy(&git(repo, &["submodule", "status"]).unwrap_or_default()).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
	for submodule in &submodules {
		hasher.update(submodule.as_bytes());
		hasher.update(b"\0");
	}
	let mut lockfiles = BTreeMap::new();
	let listing = git(repo, &["ls-files", "-z", "--", "*Cargo.lock", "toolchain.lock"])?;
	for entry in listing.split(|b| *b == 0).filter(|e| !e.is_empty()) {
		let path = String::from_utf8_lossy(entry).to_string();
		if let Ok(digest) = sha256_file(&repo.join(&path)) {
			lockfiles.insert(path, digest);
		}
	}
	for (path, digest) in &lockfiles {
		hasher.update(path.as_bytes());
		hasher.update(digest.as_bytes());
	}
	// Declared generated inputs: what a build script derives from and which is not itself tracked
	// is covered by its tracked source; the one generated input read at RUN time is the staged
	// image inventory the harness writes, which is a run product and not an identity input.
	let generated_inputs: BTreeMap<String, String> = BTreeMap::new();
	let working_tree = hex(&hasher.finalize());
	if changed_paths == 0 {
		let tree = String::from_utf8_lossy(&git(repo, &["rev-parse", "HEAD^{tree}"])?).trim().to_string();
		Ok(SourceIdentity { kind: String::from("git-tree"), value: tree, head, tracked_files: tracked, changed_paths, lockfiles, generated_inputs, submodules })
	} else {
		Ok(SourceIdentity { kind: String::from("working-tree"), value: working_tree, head, tracked_files: tracked, changed_paths, lockfiles, generated_inputs, submodules })
	}
}

fn resolve_tool(repo: &Path, name: &str) -> Option<PathBuf> {
	// `cargo` and `rustc` on PATH are rustup's proxies - one binary under every name, which
	// canonicalises to `rustup` itself and answers `--version` as rustup. The bytes a build runs are
	// the toolchain's, selected by the tree's `rust-toolchain.toml`, and `rustup which` names them.
	if matches!(name, "cargo" | "rustc")
		&& let Ok(output) = Command::new("rustup").arg("which").arg(name).current_dir(repo).output()
		&& output.status.success()
	{
		let resolved = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
		if resolved.is_file() {
			return Some(resolved);
		}
	}
	let path = std::env::var_os("PATH")?;
	std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| candidate.is_file())
}

fn tool_version(path: &Path) -> String {
	Command::new(path).arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string()).unwrap_or_else(|_| String::from("unavailable"))
}

// Every override present in the environment that is not permitted, under the guarded prefixes.
pub fn undeclared_overrides(environment: &[(String, String)]) -> Vec<String> {
	environment.iter().filter(|(name, _)| GUARDED_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) && !PERMITTED_ENVIRONMENT.contains(&name.as_str())).map(|(name, _)| name.clone()).collect()
}

pub fn collect(repo: &Path) -> Result<Identity, String> {
	let environment_all: Vec<(String, String)> = std::env::vars().collect();
	let undeclared = undeclared_overrides(&environment_all);
	if !undeclared.is_empty() {
		return Err(format!("undeclared environment override(s) would influence this run and are refused: {} - add each to the permitted list or unset it", undeclared.join(", ")));
	}
	let source = source_identity(repo)?;
	let mut tools = Vec::new();
	for name in TOOLS {
		match resolve_tool(repo, name) {
			Some(path) => {
				let resolved = std::fs::canonicalize(&path).unwrap_or(path.clone());
				tools.push(Tool { name: name.to_string(), path: resolved.to_string_lossy().to_string(), sha256: sha256_file(&resolved).unwrap_or_else(|_| String::from("unreadable")), version: tool_version(&resolved) });
			}
			None => tools.push(Tool { name: name.to_string(), path: String::from("absent"), sha256: String::from("absent"), version: String::from("absent") }),
		}
	}
	let mut firmware = BTreeMap::new();
	for (variable, default) in FIRMWARE {
		let path = std::env::var(variable).unwrap_or_else(|_| default.to_string());
		let digest = sha256_file(Path::new(&path)).unwrap_or_else(|_| String::from("absent"));
		firmware.insert(format!("{variable}={path}"), digest);
	}
	let mut configurations = BTreeMap::new();
	for path in CONFIGURATIONS {
		configurations.insert(path.to_string(), sha256_file(&repo.join(path)).unwrap_or_else(|_| String::from("absent")));
	}
	let mut environment = BTreeMap::new();
	for name in PERMITTED_ENVIRONMENT {
		if let Ok(value) = std::env::var(name) {
			environment.insert(name.to_string(), value);
		}
	}
	Ok(Identity { schema: String::from(IDENTITY_SCHEMA), source, tools, firmware, configurations, environment })
}
