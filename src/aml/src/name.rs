//! NAMES: the four-character segment, and a path as AML encodes it - absolute from the root, or relative with a
//! number of parent prefixes - with the search rule for a bare single segment.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// One four-character name segment: a lead character `A-Z` or `_`, then three of `A-Z`, `0-9` or `_`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seg(pub [u8; 4]);

impl Seg {
	/// A segment from its four bytes, `None` when they are not a name.
	pub fn new(bytes: [u8; 4]) -> Option<Seg> {
		let lead = bytes[0] == b'_' || bytes[0].is_ascii_uppercase();
		let rest = bytes[1..].iter().all(|&byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit());
		(lead && rest).then_some(Seg(bytes))
	}

	/// A segment from text, padded with `_` as ASL pads a short name: `"_SB"` is `_SB_`.
	pub fn from_str(text: &str) -> Option<Seg> {
		let bytes = text.as_bytes();
		if bytes.is_empty() || bytes.len() > 4 {
			return None;
		}
		let mut seg = [b'_'; 4];
		seg[..bytes.len()].copy_from_slice(bytes);
		Seg::new(seg)
	}

	pub fn as_str(&self) -> &str {
		core::str::from_utf8(&self.0).unwrap_or("????")
	}
}

impl fmt::Debug for Seg {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl fmt::Display for Seg {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

/// A NAME STRING as AML encodes it: from the root, or relative to the current scope after `parents` parent
/// prefixes; `segs` empty is the null name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NameString {
	pub root: bool,
	pub parents: u8,
	pub segs: Vec<Seg>,
}

impl NameString {
	pub fn null() -> NameString {
		NameString { root: false, parents: 0, segs: Vec::new() }
	}

	/// Whether the search rule applies: a bare single segment, with no root and no parent prefix, is looked for in
	/// the current scope and then in each enclosing one.
	pub fn searches(&self) -> bool {
		!self.root && self.parents == 0 && self.segs.len() == 1
	}

	/// Parse `\\_SB.PCI0.S08`, `^^FOO`, `FOO.BAR` or `_STA` - the ASL spelling, for callers and tests.
	pub fn parse(text: &str) -> Option<NameString> {
		let mut rest = text;
		let root = rest.starts_with('\\');
		if root {
			rest = &rest[1..];
		}
		let mut parents = 0u8;
		while let Some(after) = rest.strip_prefix('^') {
			parents = parents.checked_add(1)?;
			rest = after;
		}
		if root && parents != 0 {
			return None;
		}
		let segs = if rest.is_empty() { Vec::new() } else { rest.split('.').map(Seg::from_str).collect::<Option<Vec<Seg>>>()? };
		Some(NameString { root, parents, segs })
	}

	/// Decode a name string at the start of `bytes`, answering it and the bytes it took.
	pub fn decode(bytes: &[u8]) -> Option<(NameString, usize)> {
		let mut at = 0usize;
		let mut root = false;
		let mut parents = 0u8;
		match bytes.first()? {
			b'\\' => {
				root = true;
				at = 1;
			}
			b'^' => {
				while bytes.get(at) == Some(&b'^') {
					parents = parents.checked_add(1)?;
					at += 1;
				}
			}
			_ => {}
		}
		let segs_at = |at: usize, count: usize| -> Option<Vec<Seg>> { (0..count).map(|i| Seg::new(bytes.get(at + 4 * i..at + 4 * i + 4)?.try_into().ok()?)).collect() };
		let (segs, taken) = match *bytes.get(at)? {
			0x00 => (Vec::new(), 1),
			0x2E => (segs_at(at + 1, 2)?, 1 + 8),
			0x2F => {
				let count = *bytes.get(at + 1)? as usize;
				(segs_at(at + 2, count)?, 2 + 4 * count)
			}
			_ => (segs_at(at, 1)?, 4),
		};
		Some((NameString { root, parents, segs }, at + taken))
	}

	/// Whether `byte` begins a name string.
	pub fn starts(byte: u8) -> bool {
		byte == b'\\' || byte == b'^' || byte == b'_' || byte.is_ascii_uppercase() || byte == 0x2E || byte == 0x2F
	}
}

impl fmt::Display for NameString {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		if self.root {
			f.write_str("\\")?;
		}
		for _ in 0..self.parents {
			f.write_str("^")?;
		}
		for (at, seg) in self.segs.iter().enumerate() {
			if at != 0 {
				f.write_str(".")?;
			}
			f.write_str(seg.as_str())?;
		}
		Ok(())
	}
}

/// An absolute path, root first: what a node is called.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Path(pub Vec<Seg>);

impl Path {
	pub fn root() -> Path {
		Path(Vec::new())
	}

	pub fn child(&self, seg: Seg) -> Path {
		let mut segs = self.0.clone();
		segs.push(seg);
		Path(segs)
	}

	pub fn parent(&self) -> Option<Path> {
		let mut segs = self.0.clone();
		segs.pop()?;
		Some(Path(segs))
	}

	pub fn last(&self) -> Option<Seg> {
		self.0.last().copied()
	}

	/// The ASL spelling, `\_SB.PCI0` - `\` for the root.
	pub fn text(&self) -> String {
		let mut out = String::from("\\");
		for (at, seg) in self.0.iter().enumerate() {
			if at != 0 {
				out.push('.');
			}
			out.push_str(seg.as_str());
		}
		out
	}
}

impl fmt::Display for Path {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.text())
	}
}
