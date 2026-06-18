// Init package: a tiny read-only archive of the userspace programs the kernel
// loads at boot. The bootloader hands it to the kernel as a Limine module; the
// kernel parses it in place (the module memory is 'static) and looks up programs
// by name. The on-disk format is produced by the kernel's build.rs.
//
// Layout (all integers little-endian):
//   header  : magic [8] = b"LIBERPK1", count u32, reserved u32        (16 bytes)
//   entries : count * { name [24] NUL-padded, offset u32, size u32 }  (32 bytes each)
//   blobs   : the file contents, concatenated; each entry's offset/size is an
//             absolute byte range into the package.

#![allow(dead_code)]

use abi::{PKG_ENTRY_LEN as ENTRY_LEN, PKG_HEADER_LEN as HEADER_LEN, PKG_MAGIC as MAGIC, PKG_NAME_LEN as NAME_LEN};

// A parsed init package borrowing the underlying module bytes.
pub struct Package<'a> {
	bytes: &'a [u8],
	count: usize,
}

impl<'a> Package<'a> {
	// Parse and validate a package header. Returns None if the bytes are too
	// short, the magic is wrong, or the entry table does not fit.
	pub fn parse(bytes: &'a [u8]) -> Option<Self> {
		if bytes.len() < HEADER_LEN {
			return None;
		}
		if &bytes[0..8] != MAGIC {
			return None;
		}
		let count = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
		let table_end = HEADER_LEN.checked_add(count.checked_mul(ENTRY_LEN)?)?;
		if table_end > bytes.len() {
			return None;
		}
		Some(Self { bytes, count })
	}

	// Number of files in the package.
	pub fn len(&self) -> usize {
		self.count
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	// The name of the `index`-th file (its stored name up to the first NUL), or
	// None if the index is out of range. Lets a caller enumerate the archive.
	pub fn name(&self, index: usize) -> Option<&'a [u8]> {
		if index >= self.count {
			return None;
		}
		let base = HEADER_LEN + index * ENTRY_LEN;
		let stored = &self.bytes[base..base + NAME_LEN];
		match stored.iter().position(|&b| b == 0) {
			Some(end) => Some(&stored[..end]),
			None => Some(stored),
		}
	}

	// Find a file by name, returning its blob. The stored name is compared up to
	// its first NUL. Returns None if absent, or if its byte range is out of bounds.
	pub fn lookup(&self, name: &[u8]) -> Option<&'a [u8]> {
		for index in 0..self.count {
			let base = HEADER_LEN + index * ENTRY_LEN;
			let entry = &self.bytes[base..base + ENTRY_LEN];
			let stored = &entry[0..NAME_LEN];
			let stored_name = match stored.iter().position(|&b| b == 0) {
				Some(end) => &stored[..end],
				None => stored,
			};
			if stored_name != name {
				continue;
			}
			let offset = u32::from_le_bytes(entry[24..28].try_into().ok()?) as usize;
			let size = u32::from_le_bytes(entry[28..32].try_into().ok()?) as usize;
			let end = offset.checked_add(size)?;
			if end > self.bytes.len() {
				return None;
			}
			return Some(&self.bytes[offset..end]);
		}
		None
	}
}
