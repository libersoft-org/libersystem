// The bootstrap list and the archive built from it.
//
// The loader reads `etc/bootstrap.list` off whichever source answers, reads each program it names,
// and packs them into the PKGARCH1 archive the kernel already unpacks. Both halves of that live
// here rather than in the loader, for one reason: the loader is a UEFI binary, so nothing in it can
// be tested on the host - and these two are exactly the parts worth testing. The format's reader
// (`Package::parse`) is in this crate already; its writer belongs beside it.

use alloc::vec::Vec;

use crate::{PKG_ENTRY_LEN, PKG_HEADER_LEN, PKG_MAGIC, PKG_NAME_LEN};

// What one archive may hold. Not a guess about the format - the offsets are `u32`, so these keep a
// large set from producing a package that describes itself wrongly, and they are far above any
// bootstrap set that would still be a bootstrap set.
// The BOOTSTRAP set's bound, which is not the reader's. A handful of boot programs; the volume
// package is a different archive with a different limit (`crate::MAX_PACKAGE_ENTRIES`).
pub const MAX_ENTRIES: usize = 64;
pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

// One line of the list: the name the entry has in the archive, and the path it is read from. Both
// are needed - the kernel looks entries up by the name they have always had, which is not the path
// they now live at.
pub struct Row<'a> {
	pub name: &'a [u8],
	pub path: &'a [u8],
}

// Trim ASCII whitespace from both ends.
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

// Parse the whole list, or refuse it.
//
// STRICTLY, because this is the file that decides what the system starts and every accepted oddity
// is a machine that boots differently for a reason invisible in the file. It used to split on `\n`
// and then on the first space: a CRLF file left `\r` on every path, a tab was not a separator,
// repeated spaces became part of the path, an empty name was accepted, and a duplicate name was
// not noticed. Blank lines and `#` comments are skipped; everything else must be a well-formed row.
pub fn parse_list(bytes: &[u8]) -> Option<Vec<Row<'_>>> {
	let mut rows: Vec<Row<'_>> = Vec::new();
	for line in bytes.split(|&b| b == b'\n') {
		let line = trim(line);
		if line.is_empty() || line[0] == b'#' {
			continue;
		}
		let split = line.iter().position(|&b| b == b' ' || b == b'\t')?;
		let name = trim(&line[..split]);
		let path = trim(&line[split + 1..]);
		if !crate::valid_package_name(name) || path.is_empty() {
			return None;
		}
		if rows.iter().any(|row| row.name == name) {
			return None;
		}
		if rows.len() == MAX_ENTRIES {
			return None;
		}
		rows.push(Row { name, path });
	}
	if rows.is_empty() { None } else { Some(rows) }
}

// Build a PKGARCH1 archive from named blobs.
//
// EVERY WIDTH NARROWED ON PURPOSE. The count, each offset and each length are `u32`; unchecked
// `as u32` plus unchecked offset arithmetic meant a large enough set produced an archive whose
// table pointed somewhere other than its data, which the kernel would then read as whatever
// happened to be there. Anything that would not describe itself correctly is refused instead.
pub fn build_package(entries: &[(&[u8], &[u8])]) -> Option<Vec<u8>> {
	if entries.is_empty() || entries.len() > MAX_ENTRIES {
		return None;
	}
	let mut total: usize = 0;
	for (index, (name, bytes)) in entries.iter().enumerate() {
		// THE READER'S RULE, not a second one. `is_empty()` and a length was all this checked, so
		// the writer produced archives `Package::parse` refuses: a NUL inside a name, and duplicate
		// names.
		if !crate::valid_package_name(name) {
			return None;
		}
		if entries[..index].iter().any(|(earlier, _)| earlier == name) {
			return None;
		}
		if bytes.len() > MAX_FILE_BYTES {
			return None;
		}
		total = total.checked_add(bytes.len())?;
		if total > MAX_TOTAL_BYTES {
			return None;
		}
	}
	let table = PKG_ENTRY_LEN.checked_mul(entries.len())?.checked_add(PKG_HEADER_LEN)?;
	let mut out: Vec<u8> = Vec::new();
	out.try_reserve_exact(table.checked_add(total)?).ok()?;
	out.extend_from_slice(PKG_MAGIC);
	out.extend_from_slice(&u32::try_from(entries.len()).ok()?.to_le_bytes());
	out.extend_from_slice(&0u32.to_le_bytes());
	let mut offset = table;
	for (name, bytes) in entries {
		let mut field = [0u8; PKG_NAME_LEN];
		field[..name.len()].copy_from_slice(name);
		out.extend_from_slice(&field);
		out.extend_from_slice(&u32::try_from(offset).ok()?.to_le_bytes());
		out.extend_from_slice(&u32::try_from(bytes.len()).ok()?.to_le_bytes());
		offset = offset.checked_add(bytes.len())?;
	}
	// The end of the last blob has to be addressable too, or the last entry is the one that lies.
	u32::try_from(offset).ok()?;
	for (_, bytes) in entries {
		out.extend_from_slice(bytes);
	}
	Some(out)
}
