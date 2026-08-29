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

// The longest path a bootstrap row may name. Far above any real layout and low enough that a line
// cannot describe a path no filesystem here would accept anyway.
pub const MAX_PATH_BYTES: usize = 255;

// Is `path` a well-formed bootstrap path?
//
// THE SIDE OF A ROW THAT HAD NO GRAMMAR AT ALL. `parse_list` applied the package-name rule to the
// NAME and accepted any path that was merely non-empty, in the file that describes itself as the
// strict shared parser. NUL bytes, ASCII controls, repeated separators, a leading or trailing one,
// `.` and `..` all passed it and were refused later - differently - by whichever `ReadsFiles`
// backend the recovery path happened to select, which collapses into "could not assemble the
// package" with nothing saying why. The same list could then mean different things depending on
// which backend read it.
//
// The rule is the one `rt::RelativePath` already applies everywhere else in this tree: one or more
// separated segments, each non-empty and neither `.` nor `..`, no NUL, no control byte, no
// backslash. Written here rather than shared with it because `abi` is the layer beneath everything
// and depends on nothing; the two are checked against each other by test instead.
//
// Backend validation stays exactly as it is. This is the shared parser refusing what it should
// never have admitted, not a replacement for defence in depth.
pub fn valid_bootstrap_path(path: &[u8]) -> bool {
	if path.is_empty() || path.len() > MAX_PATH_BYTES {
		return false;
	}
	for segment in path.split(|&b| b == b'/') {
		if segment.is_empty() || segment == b"." || segment == b".." {
			return false;
		}
		for &byte in segment {
			// Controls and DEL are not names; `\` is refused because a path that means one thing to
			// a backend splitting on it and another to one that does not is a path with two
			// meanings.
			if byte < 0x20 || byte == 0x7f || byte == b'\\' {
				return false;
			}
		}
	}
	true
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
		if !crate::valid_package_name(name) || !valid_bootstrap_path(path) {
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
// Would an entry of these two sizes fit the format? The size half of `build_package`'s rule, split
// out so it can be TESTED by calling it.
//
// The test for the per-file ceiling used to build the oversized argument out of thin air:
//
//     let huge = unsafe { core::slice::from_raw_parts(1 as *const u8, MAX_FILE_BYTES + 1) };
//
// under a comment saying "the limit is checked from the slice's length, so a slice that claims the
// length is enough to prove the check runs". That is the part that is wrong. A `&[u8]` requires its
// whole range to be valid, initialised memory owned by the program for the reference's lifetime -
// not merely to go unread - so the `unsafe` block had already broken its contract before
// `build_package` was entered, whatever the callee then did. Undefined behaviour inside the suite
// whose subject is what the ABI guarantees.
//
// A function that takes two lengths is provable by calling it, which is what a limit wants.
pub fn entry_fits(name_len: usize, data_len: usize, total_so_far: usize) -> bool {
	name_len <= crate::PKG_NAME_LEN && data_len <= MAX_FILE_BYTES && total_so_far.checked_add(data_len).is_some_and(|total| total <= MAX_TOTAL_BYTES)
}

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
		if !entry_fits(name.len(), bytes.len(), total) {
			return None;
		}
		total = total.checked_add(bytes.len())?;
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

// WHY A SOURCE WAS REFUSED. Each one is a different fact about the medium, and the loader prints
// which: they are the difference between "this disk is not a LiberSystem disk" and "this disk is
// one and it has been tampered with or half-written".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
	// A list, and no manifest beside it. A source that names programs and does not say what they
	// should be cannot be checked at all.
	NoManifest,
	// The list itself is not what the manifest says.
	ListMismatch,
	// The list is not a list this loader understands.
	MalformedList,
	// The list is here and could not be read. A source that is PRESENT and damaged, which is not the
	// same as one that has no list - see `Read`.
	ListUnreadable,
	// A program the list names is not on the source.
	MissingProgram,
	// A program is here and could not be read.
	ProgramUnreadable,
	// A program is not what the manifest says.
	ProgramMismatch,
	// The set is verified and could not be packed - an allocation failure, not a trust failure,
	// and still not a source to boot from.
	OutOfMemory,
	// THE LIST IS ABSENT FROM A SOURCE THAT WAS ALREADY CHOSEN.
	//
	// Not the same refusal as `ListUnreadable` and not the same answer as `Unavailable`. A source
	// nothing selected and which has no list is simply not a LiberSystem source, and policy may look
	// at another; a source a signed pairing NAMED, or one whose own verified manifest says the list
	// is there, is a source that was chosen - and a named file missing from a chosen source is M4's
	// `Invalid`, not an absence. Deleting one file from a paired volume otherwise combines that
	// volume's kernel with another source's bootstrap set, with no signature forged.
	ListAbsentOnSelectedSource,
}

// WHAT A SOURCE IS, once it has been looked at.
//
// THE THREE USED TO BE ONE `Option`, and that is the defect: `None` meant both "there is no
// bootstrap list here, try the next disk" and "this source was selected and its manifest refused
// it". A caller cannot tell those apart, so every caller did the same thing with both - it went on
// to the next source. That turns a detected tampering into a silent fallback, which is the opposite
// of what the check is for.
pub enum Selection {
	// No bootstrap list on this source. It is not a LiberSystem boot source; policy may consider
	// another.
	Unavailable,
	// One source owns the whole set and every byte of it matched the manifest beside it.
	Verified(Vec<u8>),
	// This source was selected and failed. THE BOOT STOPS HERE - there is no path from this back to
	// trying another source.
	Invalid(Refusal),
}

// Read a source's bootstrap set and say what the source is.
//
// TWO CLOSURES RATHER THAN TWO TRAITS. `read` is the source - a UEFI filesystem in the loader, a
// table in a test - and `verify` answers one question: is this the content the manifest records for
// this path. Both are parameters so this crate stays dependency-free: the manifest format lives in
// `bootproto`, the policy lives here, and neither has to know the other's crate. They are closures
// rather than traits because the loader's filesystems are types it does not own, and the orphan
// rule does not let it implement somebody else's trait for them.
// WHAT A SOURCE ANSWERED, IN THREE STATES.
//
// `assemble` took `Option<Vec<u8>>`, so "this source has no bootstrap list" and "this source HAS one
// and it could not be read" were the same value - and the first is `Unavailable`, which invites policy
// to try another source, while the second is a source that was SELECTED and failed. Deleting a signed
// manifest's named list, or damaging it, therefore produced a kernel from one source and a bootstrap
// set from another: a downgrade performed by corrupting a file rather than forging one.
//
// The loader already distinguished the two at the filesystem (`FileRead`) and threw the distinction
// away at this boundary.
pub enum Read {
	Bytes(Vec<u8>),
	// The path names nothing on this source.
	Absent,
	// Something is there and the reader could not use it.
	Unreadable,
}

pub fn assemble(mut read: impl FnMut(&[u8]) -> Read, verify: impl Fn(&[u8], &[u8]) -> bool) -> Selection {
	let list = match read(b"etc/bootstrap.list") {
		Read::Bytes(bytes) => bytes,
		// NOT A LIBERSYSTEM SOURCE. Policy may consider another.
		Read::Absent => return Selection::Unavailable,
		// A SOURCE THAT IS HERE AND DAMAGED. The boot stops rather than looking elsewhere: the
		// alternative is that destroying one file downgrades the machine to whatever the next source
		// offers, which is the whole reason this outcome exists.
		Read::Unreadable => return Selection::Invalid(Refusal::ListUnreadable),
	};
	{};
	let Some(rows) = parse_list(&list) else {
		return Selection::Invalid(Refusal::MalformedList);
	};
	// WHICH MANIFEST IS THE CALLER'S QUESTION, and `verify` is the answer it already made. A signed
	// v2 manifest and the text one are different formats with different guarantees, and the choice
	// between them is a policy - which source, which profile, which key - that belongs where the
	// policy lives rather than inside the walk. From here on this source is the CHOSEN one: a check
	// that fails stops the boot rather than falling through to another source.
	if !verify(b"etc/bootstrap.list", &list) {
		return Selection::Invalid(Refusal::ListMismatch);
	}
	let mut blobs: Vec<Vec<u8>> = Vec::new();
	if blobs.try_reserve_exact(rows.len()).is_err() {
		return Selection::Invalid(Refusal::OutOfMemory);
	}
	for row in &rows {
		// A named program that is not on the volume is fatal rather than skipped. The bootstrap set
		// is exactly the programs the system needs before its volume is readable, so a missing one
		// produces a machine that dies later and further away, with nothing to say which program it
		// was.
		// Both failures are fatal HERE - the source was already chosen - and they are named apart so
		// a reader knows whether the program is missing or the medium is failing.
		let blob = match read(row.path) {
			Read::Bytes(bytes) => bytes,
			Read::Absent => return Selection::Invalid(Refusal::MissingProgram),
			Read::Unreadable => return Selection::Invalid(Refusal::ProgramUnreadable),
		};
		if !verify(row.path, &blob) {
			return Selection::Invalid(Refusal::ProgramMismatch);
		}
		blobs.push(blob);
	}
	let entries: Vec<(&[u8], &[u8])> = rows.iter().zip(&blobs).map(|(row, blob)| (row.name, blob.as_slice())).collect();
	match build_package(&entries) {
		Some(archive) => Selection::Verified(archive),
		None => Selection::Invalid(Refusal::OutOfMemory),
	}
}
