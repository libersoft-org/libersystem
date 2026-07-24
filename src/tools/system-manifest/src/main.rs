use std::env;
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
