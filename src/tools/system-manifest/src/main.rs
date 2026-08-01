use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use system_manifest::Manifest;

fn main() -> ExitCode {
	match run() {
		Ok(()) => ExitCode::SUCCESS,
		Err(error) => {
			eprintln!("{error}");
			ExitCode::FAILURE
		}
	}
}

fn run() -> Result<(), String> {
	let mut arguments = env::args().skip(1);
	let command = arguments.next().unwrap_or_else(|| String::from("check"));
	let workspace = find_workspace(&env::current_dir().map_err(|error| error.to_string())?).ok_or_else(|| String::from("system-manifest: cannot locate src/user/services/manifest.toml"))?;
	let manifest = Manifest::load(&workspace.join("user/services/manifest.toml"), &workspace).map_err(|error| error.to_string())?;
	match command.as_str() {
		"check" => {
			if arguments.next().is_some() {
				return Err(String::from("usage: system-manifest check"));
			}
		}
		"source-path" => {
			let owner = arguments.next().ok_or_else(|| String::from("usage: system-manifest source-path <owner>"))?;
			if arguments.next().is_some() {
				return Err(String::from("usage: system-manifest source-path <owner>"));
			}
			println!("{}", manifest.source_path(&owner).ok_or_else(|| format!("system-manifest: unknown source owner {owner:?}"))?);
		}
		"export-json" => {
			if arguments.next().is_some() {
				return Err(String::from("usage: system-manifest export-json"));
			}
			print!("{}", manifest.canonical_json().map_err(|error| error.to_string())?);
		}
		"programs" => {
			for program in manifest.programs.values() {
				println!("{}", program.name);
			}
		}
		// The boot chain, as `<kind> <name> <destination>` lines. `mkimage.sh` consumes this
		// instead of restating the same paths itself: what lands in an image is decided in the
		// manifest, and a second copy of that list is a second thing to forget to update.
		"boot-artifacts" => {
			for artifact in manifest.boot_artifacts.values() {
				// Spelled as the manifest spells it: `Debug` lowercased would turn InitPackage
				// into `initpackage`, which is not the name anyone writes or greps for.
				let kind = match artifact.kind {
					system_manifest::BootArtifactKind::Kernel => "kernel",
					system_manifest::BootArtifactKind::Loader => "loader",
					system_manifest::BootArtifactKind::InitPackage => "init-package",
					system_manifest::BootArtifactKind::VolumePackage => "volume-package",
				};
				println!("{kind} {} {}", artifact.name, artifact.destination.as_str());
			}
		}
		"libraries" => {
			for library in manifest.libraries.values() {
				println!("{}", library.name);
			}
		}
		"consumers-of" => {
			let provider = arguments.next().ok_or_else(|| String::from("usage: system-manifest consumers-of <library>"))?;
			for program in manifest.programs.values().filter(|program| program.providers.iter().any(|candidate| candidate.as_str() == provider)) {
				println!("{}", program.name);
			}
		}
		"staged-paths" => {
			for destination in manifest.libraries.values().map(|library| library.destination.as_str()).chain(manifest.programs.values().map(|program| program.destination.as_str())) {
				println!("{destination}");
			}
			for destination in manifest.factory_files.values().map(|file| file.destination.as_str()) {
				println!("{destination}");
			}
		}
		"check-volume-package" => {
			let path = arguments.next().ok_or_else(|| String::from("usage: system-manifest check-volume-package <volume.pkg>"))?;
			if arguments.next().is_some() {
				return Err(String::from("usage: system-manifest check-volume-package <volume.pkg>"));
			}
			let bytes = fs::read(&path).map_err(|error| format!("system-manifest: cannot read {path}: {error}"))?;
			let package = abi::Package::parse(&bytes).ok_or_else(|| format!("system-manifest: {path} is not a valid PKGARCH1 archive"))?;
			let mut actual = BTreeSet::new();
			for index in 0..package.len() {
				let name = package.name(index).ok_or_else(|| format!("system-manifest: {path} has an unreadable entry {index}"))?;
				let name = core::str::from_utf8(name).map_err(|_| format!("system-manifest: {path} entry {index} is not UTF-8"))?;
				if !actual.insert(String::from(name)) {
					return Err(format!("system-manifest: {path} has duplicate entry {name}"));
				}
			}
			// A volume package is a shipping one or a development one, and the difference is
			// exactly the development-only programs. Accept whichever it is rather than
			// demanding one, and say which was recognised, so this never becomes a reason to
			// avoid checking the configuration that is actually being built.
			let shipping = manifest.volume_destinations(false);
			let development = manifest.volume_destinations(true);
			if actual != shipping && actual != development {
				let expected = if actual.len() > shipping.len() { &development } else { &shipping };
				let missing = expected.difference(&actual).cloned().collect::<Vec<_>>().join(", ");
				let unexpected = actual.difference(expected).cloned().collect::<Vec<_>>().join(", ");
				return Err(format!("system-manifest: {path} volume entries differ from manifest; missing=[{missing}] unexpected=[{unexpected}]"));
			}
			let configuration = if actual == development && development != shipping { "development" } else { "shipping" };
			println!("system-manifest: volume package entries match manifest ({configuration} configuration)");
		}
		_ => return Err(format!("system-manifest: unknown command {command:?}")),
	}
	Ok(())
}

fn find_workspace(start: &Path) -> Option<PathBuf> {
	for ancestor in start.ancestors() {
		if ancestor.join("user/services/manifest.toml").is_file() {
			return Some(ancestor.to_path_buf());
		}
		let src = ancestor.join("src");
		if src.join("user/services/manifest.toml").is_file() {
			return Some(src);
		}
	}
	None
}
