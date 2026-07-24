//! `vol://` path resolution and volume routing.

use alloc::string::String;
use alloc::vec::Vec;

fn trim(mut value: &[u8]) -> &[u8] {
	while let [first, rest @ ..] = value {
		if first.is_ascii_whitespace() {
			value = rest;
		} else {
			break;
		}
	}
	while let [rest @ .., last] = value {
		if last.is_ascii_whitespace() {
			value = rest;
		} else {
			break;
		}
	}
	value
}

fn split_vol(uri: &[u8]) -> Option<(&[u8], &[u8])> {
	let rest: &[u8] = uri.strip_prefix(b"vol://")?;
	match rest.iter().position(|&byte: &u8| byte == b'/') {
		Some(slash) => Some((&rest[..slash], &rest[slash + 1..])),
		None => Some((rest, b"")),
	}
}

fn push_segments<'a>(tail: &'a [u8], segments: &mut Vec<&'a [u8]>) {
	for segment in tail.split(|&byte: &u8| byte == b'/') {
		if segment.is_empty() || segment == b"." {
			continue;
		}
		if segment == b".." {
			segments.pop();
			continue;
		}
		segments.push(segment);
	}
}

pub fn resolve(cwd: &str, arg: &[u8]) -> Option<String> {
	let arg: &[u8] = trim(arg);
	let absolute: bool = arg.starts_with(b"vol://");
	let base: &[u8] = if absolute { arg } else { cwd.as_bytes() };
	let (volume, base_tail) = split_vol(base)?;
	if volume.is_empty() {
		return None;
	}
	let mut segments: Vec<&[u8]> = Vec::new();
	push_segments(base_tail, &mut segments);
	if !absolute {
		push_segments(arg, &mut segments);
	}
	let mut out: String = String::from("vol://");
	out.push_str(core::str::from_utf8(volume).ok()?);
	for segment in segments {
		out.push('/');
		out.push_str(core::str::from_utf8(segment).ok()?);
	}
	Some(out)
}

pub fn volume<'a>(cwd: &'a str, arg: &'a [u8]) -> Option<&'a [u8]> {
	let arg: &[u8] = trim(arg);
	let base: &[u8] = if arg.starts_with(b"vol://") { arg } else { cwd.as_bytes() };
	let (volume, _) = split_vol(base)?;
	if volume.is_empty() {
		None
	} else {
		Some(volume)
	}
}

pub fn volume_client(cwd: &str, arg: &[u8], system: u64, media: u64, iso: u64, udf: u64, usb: u64) -> u64 {
	match volume(cwd, arg) {
		Some(b"media") => media,
		Some(b"iso") => iso,
		Some(b"udf") => udf,
		Some(b"usb") => usb,
		_ => system,
	}
}
