//! `vol://` path resolution shared by the shell and the sandboxed tools.
//!
//! A relative path argument is resolved against an inherited working directory the same
//! way everywhere, so a tool launched with a raw argument and its caller's cwd reaches the
//! same file the shell would. The hand-written helpers here operate on the system's
//! `vol://<volume>[/<seg>...]` addressing vocabulary; they are pure (no syscalls), only
//! allocating the resolved `String`.

use alloc::string::String;
use alloc::vec::Vec;

// Trim ASCII whitespace from both ends of a byte slice.
fn trim(mut s: &[u8]) -> &[u8] {
	while let [first, rest @ ..] = s {
		if first.is_ascii_whitespace() {
			s = rest;
		} else {
			break;
		}
	}
	while let [rest @ .., last] = s {
		if last.is_ascii_whitespace() {
			s = rest;
		} else {
			break;
		}
	}
	s
}

// Split a "vol://<volume>[/<tail>]" URI into its volume name and the remaining path
// (without the leading slash). Returns None if the "vol://" scheme is missing.
fn split_vol(uri: &[u8]) -> Option<(&[u8], &[u8])> {
	let rest: &[u8] = uri.strip_prefix(b"vol://")?;
	match rest.iter().position(|&b: &u8| b == b'/') {
		Some(slash) => Some((&rest[..slash], &rest[slash + 1..])),
		None => Some((rest, b"")),
	}
}

// Normalize a '/'-separated path onto `segs`: empty and "." segments are dropped and
// ".." pops the last segment (a no-op at the root), so the result stays clean.
fn push_segments<'a>(tail: &'a [u8], segs: &mut Vec<&'a [u8]>) {
	for seg in tail.split(|&b: &u8| b == b'/') {
		if seg.is_empty() || seg == b"." {
			continue;
		}
		if seg == b".." {
			segs.pop();
			continue;
		}
		segs.push(seg);
	}
}

// Resolve a user-supplied path against the current working directory. An argument that
// starts with "vol://" is absolute and starts fresh at its own volume; anything else is
// relative and extends the cwd. The result is always a clean, normalized
// "vol://<volume>[/<seg>...]" URI; returns None if the path is malformed (missing scheme
// or volume, or non-UTF-8) so the caller can report it.
pub fn resolve(cwd: &str, arg: &[u8]) -> Option<String> {
	let arg: &[u8] = trim(arg);
	let absolute: bool = arg.starts_with(b"vol://");
	let base: &[u8] = if absolute { arg } else { cwd.as_bytes() };
	let (volume, base_tail) = split_vol(base)?;
	if volume.is_empty() {
		return None;
	}
	let mut segs: Vec<&[u8]> = Vec::new();
	push_segments(base_tail, &mut segs);
	if !absolute {
		push_segments(arg, &mut segs);
	}
	let mut out: String = String::from("vol://");
	out.push_str(core::str::from_utf8(volume).ok()?);
	for seg in segs {
		out.push('/');
		out.push_str(core::str::from_utf8(seg).ok()?);
	}
	Some(out)
}

// The volume a path resolves onto, for routing - the volume of an absolute "vol://"
// argument, otherwise the cwd's volume - without normalizing the rest of the path.
// Returns None if neither names a volume (so the caller can report a malformed path).
pub fn volume<'a>(cwd: &'a str, arg: &'a [u8]) -> Option<&'a [u8]> {
	let arg: &[u8] = trim(arg);
	let base: &[u8] = if arg.starts_with(b"vol://") { arg } else { cwd.as_bytes() };
	let (volume, _tail) = split_vol(base)?;
	if volume.is_empty() { None } else { Some(volume) }
}

// Route a path to the StorageService client for its volume, from the four clients a tool
// holds under the `volumes` capability. The volume is that of an absolute `vol://` argument,
// otherwise the cwd's; `system` (the writable LiberFS) is the fallback for the system volume
// or an unrecognized / malformed path, so a tool always has a client to try.
pub fn volume_client(cwd: &str, arg: &[u8], system: u64, media: u64, iso: u64, udf: u64) -> u64 {
	match volume(cwd, arg) {
		Some(b"media") => media,
		Some(b"iso") => iso,
		Some(b"udf") => udf,
		_ => system,
	}
}
