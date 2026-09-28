//! WHAT A DRIVER MAY EVALUATE ON ITS NODE CHANNEL: a named method or object of the node it was handed - or the node
//! itself - with bounded arguments, and the few methods its class lets it reach on the parent node (the first is
//! `_DOS` on a video output's adapter). PLATFORM METHODS ARE REFUSED: `_INI`, `_REG`, `_OSC`, `_PTS`, `_WAK`, the
//! sleep objects, and the methods only the service runs; and so is every other node - a path with a separator, a
//! root or a parent prefix.

use crate::node::VIDEO_OUTPUT;

/// What an admitted name reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
	/// The node itself - an empty name.
	Node,
	/// An object of the node, by its four-character name.
	Own([u8; 4]),
	/// An object of the parent node a class row names.
	Parent([u8; 4]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
	/// A platform method: the service's alone.
	Platform,
	/// Another node - a path, a root or a parent prefix not in the class's row.
	OtherNode,
	/// Not a name at all.
	Malformed,
}

/// The platform methods no driver evaluates.
pub const PLATFORM_METHODS: &[&[u8; 4]] = &[b"_INI", b"_REG", b"_OSC", b"_PTS", b"_WAK", b"_S0_", b"_S1_", b"_S2_", b"_S3_", b"_S4_", b"_S5_", b"_SST", b"_TTS", b"_PDC", b"_OSI", b"_GTS", b"_BFS", b"_SWS"];

/// THE CLASS ROWS' PARENT METHODS: a video output's driver may evaluate `_DOS` on its adapter.
pub fn parent_methods(class: Option<&str>) -> &'static [&'static [u8; 4]] {
	match class {
		Some(VIDEO_OUTPUT) => &[b"_DOS"],
		_ => &[],
	}
}

fn segment(text: &str) -> Option<[u8; 4]> {
	let bytes = text.as_bytes();
	if bytes.is_empty() || bytes.len() > 4 {
		return None;
	}
	if !bytes.iter().enumerate().all(|(at, byte)| byte.is_ascii_uppercase() || *byte == b'_' || (at != 0 && byte.is_ascii_digit())) {
		return None;
	}
	let mut out = [b'_'; 4];
	out[..bytes.len()].copy_from_slice(bytes);
	Some(out)
}

/// ADMIT `name` on a node of `class`.
pub fn admit(name: &str, class: Option<&str>) -> Result<Target, Refusal> {
	if name.is_empty() {
		return Ok(Target::Node);
	}
	if let Some(rest) = name.strip_prefix('^') {
		let seg = segment(rest).ok_or(Refusal::OtherNode)?;
		return if parent_methods(class).iter().any(|allowed| **allowed == seg) { Ok(Target::Parent(seg)) } else { Err(Refusal::OtherNode) };
	}
	if name.contains('.') || name.starts_with('\\') {
		return Err(Refusal::OtherNode);
	}
	let seg = segment(name).ok_or(Refusal::Malformed)?;
	if PLATFORM_METHODS.iter().any(|platform| **platform == seg) {
		return Err(Refusal::Platform);
	}
	Ok(Target::Own(seg))
}
