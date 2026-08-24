// The boot manifest, version 2: what the loader checks the bytes it is about to execute against,
// and who says so.
//
// WHAT v1 PROVED AND WHAT IT DID NOT. `boot_manifest` is a text file of `<sha256>  <path>` rows
// beside the payloads. It catches corruption, a half-written image and artifacts from two builds
// mixed together - and it proves neither ORIGIN nor AGE, because whoever can rewrite a payload can
// rewrite the manifest next to it. This format adds the first of those two: a signature over the
// whole record, made by a key the loader carries and an attacker cannot replace. It does NOT add
// the second. A correctly signed old release still verifies; enforcing a minimum version is
// anti-rollback and is deliberately not here.
//
// BINARY AND LENGTH-DELIMITED, for one reason: every field's extent is stated before its content,
// so a reader knows what it is about to read before it reads it. A text format is parsed by
// scanning for delimiters, which is the same as saying its bounds come from its content.
//
// THE PARSER IS SHARED. The loader and the host signing tool use this module - not two readings of
// one format, which is how a signed thing and a verified thing stop being the same thing.

// `LBRMAN` and the format version. A reader that does not find this exact byte string is not
// looking at a manifest, and says so rather than guessing.
pub const MAGIC: [u8; 8] = *b"LBRMAN\x02\x00";

// The one signature algorithm this version defines. The field exists so a second one can be added
// without a new format; an unknown value is a refusal rather than an assumption.
pub const ALG_ED25519: u16 = 1;

// WHAT A SIGNATURE IS OVER, and the reason it is not the payload alone: a signature that covers
// only the bytes could be replayed under any other protocol that happens to sign the same bytes.
// The domain string makes a manifest's signature mean "this is a LiberSystem boot manifest" and
// nothing else.
pub const DOMAIN: &[u8] = b"libersystem-boot-manifest-v2\0";

// Bounds, checked BEFORE anything is allocated or indexed. Each is far above any real manifest and
// far below what would make the arithmetic below interesting.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_ROWS: usize = 256;
pub const MAX_PATH_BYTES: usize = 255;
pub const MAX_NAME_BYTES: usize = 64;

// Which architecture's release this is. A manifest signed for another one is a refusal: the same
// key signs every port's release, and the field is what keeps one port's payloads out of another's
// boot.
pub const ARCH_X86_64: u8 = 1;
pub const ARCH_AARCH64: u8 = 2;
pub const ARCH_RISCV64: u8 = 3;

// Which kind of source this manifest describes. The loader knows which one it is reading from, and
// a manifest that describes a different kind is not this source's.
pub const SOURCE_SYSTEM_VOLUME: u8 = 1;
pub const SOURCE_LIVE_IMAGE: u8 = 2;
pub const SOURCE_BOOT_MEDIUM: u8 = 3;

// What an artifact row names. The kind is not decoration: the loader treats a kernel differently
// from a program, and a row that changed kind under a path is a different artifact.
pub const KIND_KERNEL: u8 = 1;
pub const KIND_BOOTSTRAP_LIST: u8 = 2;
pub const KIND_PROGRAM: u8 = 3;
pub const KIND_SYSTEM_VOLUME: u8 = 4;
pub const KIND_PACKAGE: u8 = 5;

// WHY A MANIFEST WAS REFUSED. Each is a different fact about the bytes, and the loader prints which
// - "this is not a manifest" and "this manifest is for another architecture" are different
// machines to be standing in front of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
	// Not this format at all: too short to hold a header, or the magic does not match.
	NotAManifest,
	// Longer than this reader will look at. Stated before anything is indexed.
	TooLarge,
	// A field's declared extent runs past the end of what was read.
	Truncated,
	// Bytes after the signature. A manifest is exactly as long as it says it is.
	TrailingBytes,
	// A required enum field carries a value this version does not define.
	UnknownValue,
	// More rows than this reader will hold.
	TooManyRows,
	// A path that is empty, too long, absolute, or carries a segment no path may.
	InvalidPath,
	// Two rows for one path, or rows out of canonical order. BOTH ARE THE SAME DEFECT: a manifest
	// whose rows can be reordered has more than one byte encoding, and a signature over one
	// encoding says nothing about the other.
	NotCanonical,
	// The arithmetic to reach a field overflowed. Refused rather than wrapped.
	Overflow,
}

// One artifact the manifest covers.
pub struct Row<'a> {
	pub kind: u8,
	pub path: &'a [u8],
	pub length: u64,
	pub digest: [u8; 32],
}

// A manifest that has been READ but not yet VERIFIED. Holding those apart is the point: `decode`
// answers whether the bytes are a well-formed manifest, and `payload` is exactly what a signature
// has to cover for it to be this manifest's.
pub struct Manifest<'a> {
	// The whole record, kept so the row walk can index it without a second copy.
	bytes: &'a [u8],
	// How many bytes from the start the signature covers. EXACTLY this many: a caller cannot be
	// handed a shorter payload to verify, which is the one way a signature over part of a manifest
	// would pass for a signature over all of it.
	payload_len: usize,
	pub alg: u16,
	pub key_id: u32,
	pub product: &'a [u8],
	pub arch: u8,
	pub source_kind: u8,
	pub release: &'a [u8],
	pub volume_uuid: [u8; 16],
	rows_at: usize,
	row_count: usize,
}

// Read `n` bytes at `at`, or say the record is truncated.
fn take<'a>(bytes: &'a [u8], at: usize, n: usize) -> Result<(&'a [u8], usize), Refusal> {
	let end = at.checked_add(n).ok_or(Refusal::Overflow)?;
	if end > bytes.len() {
		return Err(Refusal::Truncated);
	}
	Ok((&bytes[at..end], end))
}

fn u16_at(bytes: &[u8], at: usize) -> Result<(u16, usize), Refusal> {
	let (b, next) = take(bytes, at, 2)?;
	Ok((u16::from_le_bytes([b[0], b[1]]), next))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<(u32, usize), Refusal> {
	let (b, next) = take(bytes, at, 4)?;
	Ok((u32::from_le_bytes([b[0], b[1], b[2], b[3]]), next))
}

fn u64_at(bytes: &[u8], at: usize) -> Result<(u64, usize), Refusal> {
	let (b, next) = take(bytes, at, 8)?;
	Ok((u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]), next))
}

// Whether a path is one a manifest may name.
//
// RELATIVE, AND WITH NO TRAVERSAL. The loader joins these to a source's root, so a leading slash or
// a `..` segment is a path that names something outside what the manifest describes - which is the
// whole of what it is for.
fn path_ok(path: &[u8]) -> bool {
	if path.is_empty() || path.len() > MAX_PATH_BYTES {
		return false;
	}
	for segment in path.split(|&b| b == b'/') {
		if segment.is_empty() || segment == b"." || segment == b".." {
			return false;
		}
		for &byte in segment {
			if byte < 0x20 || byte == 0x7f || byte == b'\\' {
				return false;
			}
		}
	}
	true
}

impl<'a> Manifest<'a> {
	// Read a manifest. Answers what the bytes ARE, never whether they are trusted - that is the
	// signature's question, and `payload` is what it must be asked about.
	pub fn decode(bytes: &'a [u8]) -> Result<Manifest<'a>, Refusal> {
		if bytes.len() > MAX_MANIFEST_BYTES {
			return Err(Refusal::TooLarge);
		}
		if bytes.len() < MAGIC.len() || bytes[..MAGIC.len()] != MAGIC {
			return Err(Refusal::NotAManifest);
		}
		let mut at = MAGIC.len();
		let (alg, next) = u16_at(bytes, at)?;
		at = next;
		if alg != ALG_ED25519 {
			return Err(Refusal::UnknownValue);
		}
		let (key_id, next) = u32_at(bytes, at)?;
		at = next;

		let (len, next) = take(bytes, at, 1)?;
		at = next;
		let (product, next) = take(bytes, at, len[0] as usize)?;
		at = next;
		if product.is_empty() || product.len() > MAX_NAME_BYTES {
			return Err(Refusal::InvalidPath);
		}

		let (arch, next) = take(bytes, at, 1)?;
		at = next;
		if !matches!(arch[0], ARCH_X86_64 | ARCH_AARCH64 | ARCH_RISCV64) {
			return Err(Refusal::UnknownValue);
		}
		let (source, next) = take(bytes, at, 1)?;
		at = next;
		if !matches!(source[0], SOURCE_SYSTEM_VOLUME | SOURCE_LIVE_IMAGE | SOURCE_BOOT_MEDIUM) {
			return Err(Refusal::UnknownValue);
		}

		let (len, next) = take(bytes, at, 1)?;
		at = next;
		let (release, next) = take(bytes, at, len[0] as usize)?;
		at = next;
		if release.is_empty() || release.len() > MAX_NAME_BYTES {
			return Err(Refusal::InvalidPath);
		}

		let (uuid, next) = take(bytes, at, 16)?;
		at = next;
		let mut volume_uuid = [0u8; 16];
		volume_uuid.copy_from_slice(uuid);

		let (row_count, next) = u16_at(bytes, at)?;
		at = next;
		if row_count as usize > MAX_ROWS {
			return Err(Refusal::TooManyRows);
		}
		let rows_at = at;

		// WALKED ONCE HERE so a caller may treat the rows as well formed afterwards. A parser that
		// validates lazily is one whose refusals arrive at whatever moment somebody happens to look.
		let mut previous: Option<(u8, &[u8])> = None;
		for _ in 0..row_count {
			let (kind, next) = take(bytes, at, 1)?;
			at = next;
			if !matches!(kind[0], KIND_KERNEL | KIND_BOOTSTRAP_LIST | KIND_PROGRAM | KIND_SYSTEM_VOLUME | KIND_PACKAGE) {
				return Err(Refusal::UnknownValue);
			}
			let (path_len, next) = u16_at(bytes, at)?;
			at = next;
			let (path, next) = take(bytes, at, path_len as usize)?;
			at = next;
			if !path_ok(path) {
				return Err(Refusal::InvalidPath);
			}
			let (_length, next) = u64_at(bytes, at)?;
			at = next;
			let (_digest, next) = take(bytes, at, 32)?;
			at = next;
			// CANONICAL ORDER IS PART OF THE FORMAT. Rows ascend by kind and then by path, with no
			// repeats: a manifest whose rows can be reordered has more than one byte encoding, and a
			// signature over one encoding says nothing about the other.
			if let Some((last_kind, last_path)) = previous
				&& (kind[0], path) <= (last_kind, last_path)
			{
				return Err(Refusal::NotCanonical);
			}
			previous = Some((kind[0], path));
		}

		let payload_len = at;
		let (_signature, next) = take(bytes, at, 64)?;
		if next != bytes.len() {
			return Err(Refusal::TrailingBytes);
		}

		Ok(Manifest {
			bytes,
			payload_len,
			alg,
			key_id,
			product,
			arch: arch[0],
			source_kind: source[0],
			release,
			volume_uuid,
			rows_at,
			row_count: row_count as usize,
		})
	}

	// EXACTLY WHAT A SIGNATURE MUST COVER, domain string included. A caller cannot ask about
	// anything else: there is no accessor that returns a shorter prefix, which is the one way a
	// signature over part of a manifest would pass for a signature over all of it.
	//
	// The domain is prepended by the verifier rather than stored, so a manifest's bytes are not a
	// message anything else could be persuaded to sign.
	pub fn payload(&self) -> &'a [u8] {
		&self.bytes[..self.payload_len]
	}

	pub fn signature(&self) -> [u8; 64] {
		let mut sig = [0u8; 64];
		sig.copy_from_slice(&self.bytes[self.payload_len..self.payload_len + 64]);
		sig
	}

	pub fn row_count(&self) -> usize {
		self.row_count
	}

	// The row at `index`, or None past the end. Every field was validated by `decode`, so this
	// re-reads rather than re-checks.
	pub fn row(&self, index: usize) -> Option<Row<'a>> {
		if index >= self.row_count {
			return None;
		}
		let mut at = self.rows_at;
		for _ in 0..index {
			let path_len = u16_at(self.bytes, at + 1).ok()?.0 as usize;
			at = at + 1 + 2 + path_len + 8 + 32;
		}
		let kind = self.bytes[at];
		let (path_len, next) = u16_at(self.bytes, at + 1).ok()?;
		let (path, next) = take(self.bytes, next, path_len as usize).ok()?;
		let (length, next) = u64_at(self.bytes, next).ok()?;
		let (digest, _) = take(self.bytes, next, 32).ok()?;
		let mut d = [0u8; 32];
		d.copy_from_slice(digest);
		Some(Row { kind, path, length, digest: d })
	}

	// The row for `path` of `kind`, or None if the manifest does not name it. A file the manifest
	// does not cover is not a file this loader may execute or hand off.
	pub fn find(&self, kind: u8, path: &[u8]) -> Option<Row<'a>> {
		(0..self.row_count).map(|i| self.row(i)).find(|row| row.as_ref().is_some_and(|r| r.kind == kind && r.path == path))?
	}
}

// What a manifest says about itself, for the tool that writes one.
pub struct Header<'a> {
	pub key_id: u32,
	pub product: &'a [u8],
	pub arch: u8,
	pub source_kind: u8,
	pub release: &'a [u8],
	pub volume_uuid: [u8; 16],
}

// Write the canonical payload into `out` and answer its length. The caller signs exactly those
// bytes (with `DOMAIN` in front) and appends the 64-byte signature.
//
// NO ALLOCATION, and the buffer is the caller's: this is the same code the loader parses with, and
// a writer that allocates is one the loader cannot carry. It refuses rather than truncating - a
// short buffer produces an error, never a manifest missing its last row.
//
// THE ROWS ARE SORTED HERE, and duplicates refused, so a tool cannot produce a manifest this
// module's own reader would reject as non-canonical. One encoding per set of facts is what makes a
// signature over the encoding mean something about the facts.
pub fn encode_payload(header: &Header<'_>, rows: &mut [Row<'_>], out: &mut [u8]) -> Result<usize, Refusal> {
	if header.product.is_empty() || header.product.len() > MAX_NAME_BYTES {
		return Err(Refusal::InvalidPath);
	}
	if header.release.is_empty() || header.release.len() > MAX_NAME_BYTES {
		return Err(Refusal::InvalidPath);
	}
	if !matches!(header.arch, ARCH_X86_64 | ARCH_AARCH64 | ARCH_RISCV64) {
		return Err(Refusal::UnknownValue);
	}
	if !matches!(header.source_kind, SOURCE_SYSTEM_VOLUME | SOURCE_LIVE_IMAGE | SOURCE_BOOT_MEDIUM) {
		return Err(Refusal::UnknownValue);
	}
	if rows.len() > MAX_ROWS {
		return Err(Refusal::TooManyRows);
	}
	for row in rows.iter() {
		if !matches!(row.kind, KIND_KERNEL | KIND_BOOTSTRAP_LIST | KIND_PROGRAM | KIND_SYSTEM_VOLUME | KIND_PACKAGE) {
			return Err(Refusal::UnknownValue);
		}
		if !path_ok(row.path) {
			return Err(Refusal::InvalidPath);
		}
	}
	// `sort_unstable_by` because it is `core`'s: the stable sort allocates, and this module is one
	// the loader carries. Stability would say nothing here anyway - equal rows are refused next.
	rows.sort_unstable_by(|a, b| (a.kind, a.path).cmp(&(b.kind, b.path)));
	for pair in rows.windows(2) {
		if (pair[0].kind, pair[0].path) == (pair[1].kind, pair[1].path) {
			return Err(Refusal::NotCanonical);
		}
	}

	let mut at = 0usize;
	let mut put = |bytes: &[u8], at: &mut usize| -> Result<(), Refusal> {
		let end = at.checked_add(bytes.len()).ok_or(Refusal::Overflow)?;
		if end > out.len() {
			return Err(Refusal::Truncated);
		}
		out[*at..end].copy_from_slice(bytes);
		*at = end;
		Ok(())
	};
	put(&MAGIC, &mut at)?;
	put(&ALG_ED25519.to_le_bytes(), &mut at)?;
	put(&header.key_id.to_le_bytes(), &mut at)?;
	put(&[header.product.len() as u8], &mut at)?;
	put(header.product, &mut at)?;
	put(&[header.arch], &mut at)?;
	put(&[header.source_kind], &mut at)?;
	put(&[header.release.len() as u8], &mut at)?;
	put(header.release, &mut at)?;
	put(&header.volume_uuid, &mut at)?;
	put(&(rows.len() as u16).to_le_bytes(), &mut at)?;
	for row in rows.iter() {
		put(&[row.kind], &mut at)?;
		put(&(row.path.len() as u16).to_le_bytes(), &mut at)?;
		put(row.path, &mut at)?;
		put(&row.length.to_le_bytes(), &mut at)?;
		put(&row.digest, &mut at)?;
	}
	if at.checked_add(64).ok_or(Refusal::Overflow)? > MAX_MANIFEST_BYTES {
		return Err(Refusal::TooLarge);
	}
	Ok(at)
}

#[cfg(test)]
mod tests;
