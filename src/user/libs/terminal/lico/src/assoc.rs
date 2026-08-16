//! File associations, and the settings the suite persists.
//!
//! AN ASSOCIATION IS DATA AND THE TYPE SAYS SO. It maps a validated file type or extension to an
//! ACTION and a CANONICAL EXECUTABLE NAME - three fields, none of which can hold a command line, a
//! path, an argument that reinterprets another file, or a capability. There is nowhere to put one.
//! That is the whole point: an association table that could carry arguments is a place to write
//! `licoview --exec` and a place for somebody else to write something worse.

extern crate alloc;

use alloc::vec::Vec;

use crate::detect::FileType;

/// What opening an entry does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
	/// Open read-only.
	View,
	/// Open for editing - the only action that asks for a write-back grant.
	Edit,
}

/// One rule: a file type, an action, and the canonical name of the program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Association {
	pub kind: FileType,
	pub action: Action,
	pub program: &'static str,
}

/// The shipped defaults, and deliberately only the explicitly safe ones this milestone names.
///
/// AN UNKNOWN FILE DEFAULTS TO THE VIEWER AND NEVER TO EXECUTION, which is why there is no
/// `FileType::Executable` row: `Enter` on a program must not run it. Running a program is what the
/// command bar is for, where somebody typed its name on purpose.
pub const DEFAULT_ASSOCIATIONS: &[Association] = &[
	Association { kind: FileType::Rust, action: Action::Edit, program: "licoedit" },
	Association { kind: FileType::Lsidl, action: Action::Edit, program: "licoedit" },
	Association { kind: FileType::Toml, action: Action::Edit, program: "licoedit" },
	Association { kind: FileType::Json, action: Action::Edit, program: "licoedit" },
	Association { kind: FileType::Markdown, action: Action::Edit, program: "licoedit" },
	Association { kind: FileType::Shell, action: Action::Edit, program: "licoedit" },
	Association { kind: FileType::Config, action: Action::Edit, program: "licoedit" },
	Association { kind: FileType::Text, action: Action::Edit, program: "licoedit" },
	Association { kind: FileType::Image, action: Action::View, program: "imgview" },
	Association { kind: FileType::Audio, action: Action::View, program: "play" },
];

/// The program and action for `kind`, or the viewer.
///
/// The fallback is not a row in the table because it is not a rule - it is what happens when no
/// rule applies, and writing it as a row would let somebody change it into something that executes.
pub fn resolve(table: &[Association], kind: FileType) -> Association {
	match table.iter().find(|entry| entry.kind == kind) {
		Some(entry) => *entry,
		None => Association { kind, action: Action::View, program: "licoview" },
	}
}

/// Whether a program name is one this table is allowed to name.
///
/// A CLOSED SET, checked here rather than trusted from the table, because the table is the thing
/// that would be edited if any of this were ever loaded from a file. The launcher checks the name
/// again; this is the check that keeps a wrong name from getting that far.
pub fn is_associable(program: &str) -> bool {
	matches!(program, "licoview" | "licoedit" | "imgview" | "play")
}

/// The suite's persisted preferences. Ordinary UI state and NOTHING ELSE - no handle, no granted
/// volume list, no selected-file authority, no credential. Every field has a default, and an
/// unreadable or unknown one falls back FIELD BY FIELD rather than discarding the file: a settings
/// file with one bad line is a settings file with one bad line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Settings {
	pub sort_key: crate::panel::SortKey,
	pub reverse: bool,
	pub directories_first: bool,
	pub show_hidden: bool,
	pub line_numbers: bool,
	pub highlight: bool,
	pub wrap: bool,
	/// How many spaces a Tab inserts in the editor. Clamped rather than refused, because a
	/// nonsensical width is a preference nobody can act on and a usable one is always available.
	pub indent_width: u8,
}

impl Default for Settings {
	fn default() -> Settings {
		Settings { sort_key: crate::panel::SortKey::Name, reverse: false, directories_first: true, show_hidden: false, line_numbers: false, highlight: true, wrap: true, indent_width: 4 }
	}
}

/// The version this writer emits. A file naming a version this build does not know is read as
/// DEFAULTS rather than refused: settings are a convenience, and a newer version of the suite
/// having written the file is not a reason for an older one not to start.
pub const SETTINGS_VERSION: u32 = 1;

impl Settings {
	/// Render as the file's bytes: a version line and then `name=value` per field.
	pub fn encode(&self) -> Option<Vec<u8>> {
		let mut out: Vec<u8> = Vec::new();
		out.try_reserve(256).ok()?;
		out.extend_from_slice(b"lico-settings 1\n");
		put(&mut out, b"sort", sort_name(self.sort_key).as_bytes());
		put_bool(&mut out, b"reverse", self.reverse);
		put_bool(&mut out, b"directories-first", self.directories_first);
		put_bool(&mut out, b"show-hidden", self.show_hidden);
		put_bool(&mut out, b"line-numbers", self.line_numbers);
		put_bool(&mut out, b"highlight", self.highlight);
		put_bool(&mut out, b"wrap", self.wrap);
		let mut width = [b'0' + self.indent_width % 10];
		if self.indent_width >= 10 {
			width[0] = b'8';
		}
		put(&mut out, b"indent", &width);
		Some(out)
	}

	/// Read the file back. Never fails: a line that cannot be read leaves its field at the default
	/// and the rest of the file is still read, which is what "falls back field by field" means.
	pub fn decode(bytes: &[u8]) -> Settings {
		let mut settings = Settings::default();
		let mut lines = bytes.split(|&byte| byte == b'\n');
		// The header is checked and its FAILURE IS NOT FATAL either - a file that is not this
		// format yields defaults, which is exactly what no file at all yields.
		let Some(header) = lines.next() else { return settings };
		if !header.starts_with(b"lico-settings ") {
			return settings;
		}
		for line in lines {
			let Some(at) = line.iter().position(|&byte| byte == b'=') else { continue };
			let (name, value) = (&line[..at], &line[at + 1..]);
			match name {
				b"sort" => {
					if let Some(key) = sort_from(value) {
						settings.sort_key = key;
					}
				}
				b"reverse" => settings.reverse = truth(value, settings.reverse),
				b"directories-first" => settings.directories_first = truth(value, settings.directories_first),
				b"show-hidden" => settings.show_hidden = truth(value, settings.show_hidden),
				b"line-numbers" => settings.line_numbers = truth(value, settings.line_numbers),
				b"highlight" => settings.highlight = truth(value, settings.highlight),
				b"wrap" => settings.wrap = truth(value, settings.wrap),
				b"indent" => {
					if let Some(digit) = value.first().and_then(|byte| byte.checked_sub(b'0')).filter(|digit| *digit < 10) {
						settings.indent_width = digit.clamp(1, 8);
					}
				}
				_ => {}
			}
		}
		settings
	}
}

fn put(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
	out.extend_from_slice(name);
	out.push(b'=');
	out.extend_from_slice(value);
	out.push(b'\n');
}

fn put_bool(out: &mut Vec<u8>, name: &[u8], value: bool) {
	put(out, name, if value { b"yes" } else { b"no" });
}

// An unrecognised value keeps the current setting rather than reading as false: `reverse=maybe` is
// a line somebody mistyped, and turning the setting off because of it is a change they did not ask
// for.
fn truth(value: &[u8], current: bool) -> bool {
	match value {
		b"yes" | b"true" | b"1" => true,
		b"no" | b"false" | b"0" => false,
		_ => current,
	}
}

fn sort_name(key: crate::panel::SortKey) -> &'static str {
	match key {
		crate::panel::SortKey::Name => "name",
		crate::panel::SortKey::Extension => "extension",
		crate::panel::SortKey::Size => "size",
		crate::panel::SortKey::Modified => "modified",
		crate::panel::SortKey::Type => "type",
	}
}

fn sort_from(value: &[u8]) -> Option<crate::panel::SortKey> {
	match value {
		b"name" => Some(crate::panel::SortKey::Name),
		b"extension" => Some(crate::panel::SortKey::Extension),
		b"size" => Some(crate::panel::SortKey::Size),
		b"modified" => Some(crate::panel::SortKey::Modified),
		b"type" => Some(crate::panel::SortKey::Type),
		_ => None,
	}
}
