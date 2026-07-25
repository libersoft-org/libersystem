//! Conservative file-type detection for safe viewer and association defaults.

/// The bounded classification used by the file manager, viewer, and editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileType {
	Directory,
	Text,
	Rust,
	Lsidl,
	Toml,
	Json,
	Markdown,
	Shell,
	Config,
	Image,
	Audio,
	Archive,
	Executable,
	Binary,
}

/// Classify one entry from its immutable name and a bounded prefix sample.
///
/// Magic bytes take precedence over a filename extension. Unknown data defaults to a
/// read-only text view only when it has neither NUL bytes nor non-whitespace controls.
pub fn detect_file_type(name: &[u8], sample: &[u8], directory: bool) -> FileType {
	if directory {
		return FileType::Directory;
	}
	if is_executable(sample) {
		return FileType::Executable;
	}
	if is_image(sample) {
		return FileType::Image;
	}
	if is_audio(sample) {
		return FileType::Audio;
	}
	if is_archive(sample) {
		return FileType::Archive;
	}
	if let Some(extension) = extension(name) {
		if eq_ascii_case(extension, b"rs") {
			return FileType::Rust;
		}
		if eq_ascii_case(extension, b"lsidl") {
			return FileType::Lsidl;
		}
		if eq_ascii_case(extension, b"toml") {
			return FileType::Toml;
		}
		if eq_ascii_case(extension, b"json") {
			return FileType::Json;
		}
		if eq_ascii_case(extension, b"md") || eq_ascii_case(extension, b"markdown") {
			return FileType::Markdown;
		}
		if eq_ascii_case(extension, b"sh") {
			return FileType::Shell;
		}
		if eq_ascii_case(extension, b"conf") || eq_ascii_case(extension, b"cfg") || eq_ascii_case(extension, b"ini") {
			return FileType::Config;
		}
		if matches_extension(extension, &[b"png", b"jpg", b"jpeg", b"gif", b"bmp", b"qoi", b"ico", b"icns", b"pcx", b"tga", b"ppm", b"webp"]) {
			return FileType::Image;
		}
		if matches_extension(extension, &[b"wav", b"wv", b"flac", b"mp3", b"ogg", b"aiff", b"aif"]) {
			return FileType::Audio;
		}
		if matches_extension(extension, &[b"zip", b"tar", b"gz", b"xz", b"pkg"]) {
			return FileType::Archive;
		}
	}
	if is_probably_text(sample) { FileType::Text } else { FileType::Binary }
}

fn is_executable(sample: &[u8]) -> bool {
	sample.starts_with(b"\x7fELF") || sample.starts_with(b"MZ")
}

fn is_image(sample: &[u8]) -> bool {
	sample.starts_with(b"\x89PNG\r\n\x1a\n") || sample.starts_with(b"\xff\xd8\xff") || sample.starts_with(b"GIF87a") || sample.starts_with(b"GIF89a") || sample.starts_with(b"BM") || sample.starts_with(b"qoif")
}

fn is_audio(sample: &[u8]) -> bool {
	(sample.starts_with(b"RIFF") && sample.get(8..12) == Some(b"WAVE")) || sample.starts_with(b"OggS") || sample.starts_with(b"fLaC") || sample.starts_with(b"wvpk") || sample.starts_with(b"ID3")
}

fn is_archive(sample: &[u8]) -> bool {
	sample.starts_with(b"PKGARCH1") || sample.starts_with(b"PK\x03\x04") || sample.starts_with(b"\x1f\x8b")
}

fn is_probably_text(sample: &[u8]) -> bool {
	!sample.iter().any(|&byte| byte == 0 || (byte < 0x20 && !matches!(byte, b'\n' | b'\r' | b'\t' | 0x0c)))
}

fn extension(name: &[u8]) -> Option<&[u8]> {
	let dot = name.iter().rposition(|&byte| byte == b'.')?;
	let extension = name.get(dot + 1..)?;
	if extension.is_empty() { None } else { Some(extension) }
}

fn matches_extension(extension: &[u8], options: &[&[u8]]) -> bool {
	options.iter().any(|option| eq_ascii_case(extension, option))
}

fn eq_ascii_case(left: &[u8], right: &[u8]) -> bool {
	left.len() == right.len() && left.iter().zip(right).all(|(&left, &right)| left.eq_ignore_ascii_case(&right))
}
