// The boot manifest: what the loader checks the bytes it read against.
//
// `etc/boot.manifest` sits beside `etc/bootstrap.list` on whichever source the loader chose, and
// names the SHA-256 of the final bytes of every file that source will be read for - the kernel, the
// list itself, and each program the list names. Once a source has been chosen, a manifest that is
// absent, malformed or disagreeing stops the boot: choosing a source and failing a check on it are
// different things, and there is no path from the second back to the first.
//
// WHAT IT PROVES, exactly: the content matches the manifest beside it. That catches corruption, a
// half-written image, and artifacts from two different builds mixed together. It does NOT catch an
// old image - an old one carries its own old manifest and agrees with itself - and it is not a
// signature: whoever can rewrite one file can rewrite both. Signing needs a key holder, an update
// channel and a recovery story, and this project has none of the three.
//
// It lives here rather than in the loader because the loader is a UEFI binary and nothing inside one
// can be tested on the host. This is the part that needs a test.

// The version line every manifest starts with.
pub const MAGIC: &[u8] = b"liberboot-manifest 1";

// Why a file was refused, so the loader can say which - the three mean different things to whoever
// reads the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
	// The bytes are what the manifest says they should be.
	Ok,
	// No `liberboot-manifest 1` first line: not a manifest this loader understands.
	NotAManifest,
	// The manifest has no row for this path - a build that staged a file it did not record.
	NotNamed,
	// A row for this path, and the digest does not match: the content changed under the record.
	Mismatch,
}

// Whether `manifest` has a row for `path` at all, without checking any content against it.
//
// The loader needs this to answer a different question from `verify`'s: not "is this file the one
// the manifest records" but "did this manifest SUPPLY this file". A source whose manifest names
// `boot/kernel` is the source the running kernel came from, and a boot that has executed a source's
// kernel has selected that source - which decides whether a missing bootstrap list on it is an
// absence or a refusal. The signed manifest answers the same question with `find(KIND_KERNEL, ..)`;
// this is the v1 equivalent, and its absence is why the rule held on one path only.
pub fn names(manifest: &[u8], path: &[u8]) -> bool {
	let mut lines = manifest.split(|byte| *byte == b'\n');
	match lines.next() {
		Some(first) if first == MAGIC => {}
		_ => return false,
	}
	lines.any(|line| line.len() >= 66 && &line[64..66] == b"  " && &line[66..] == path)
}

// Check `bytes` against the row for `path` in `manifest`.
pub fn verify(manifest: &[u8], path: &[u8], bytes: &[u8]) -> Verdict {
	let mut lines = manifest.split(|byte| *byte == b'\n');
	match lines.next() {
		Some(first) if first == MAGIC => {}
		_ => return Verdict::NotAManifest,
	}
	let want = super::sha256::digest(bytes);
	for line in lines {
		// `<64 hex>  <path>` - two spaces, so a path may contain one.
		if line.len() < 66 || &line[64..66] != b"  " || &line[66..] != path {
			continue;
		}
		for (index, byte) in want.iter().enumerate() {
			let Some(value) = hex_byte(line[index * 2], line[index * 2 + 1]) else {
				return Verdict::Mismatch;
			};
			if value != *byte {
				return Verdict::Mismatch;
			}
		}
		return Verdict::Ok;
	}
	Verdict::NotNamed
}

fn hex_byte(high: u8, low: u8) -> Option<u8> {
	let nibble = |byte: u8| match byte {
		b'0'..=b'9' => Some(byte - b'0'),
		b'a'..=b'f' => Some(byte - b'a' + 10),
		_ => None,
	};
	Some(nibble(high)? << 4 | nibble(low)?)
}

#[cfg(test)]
mod tests;
