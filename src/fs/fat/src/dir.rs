use super::*;

// A directory entry as parsed off disk, before it becomes a FileInfo: keeps the first
// cluster so a file's bytes or a subdirectory can be read, the 8.3 short form (classic
// families; empty on exFAT) so a lookup matches either name, the exFAT NoFatChain flag,
// and the byte range of its whole entry set so unlink can mark every record of it.
pub(super) struct Raw {
	pub(super) name: String,
	pub(super) short: String,
	pub(super) size: u64,
	// the exFAT ValidDataLength: the prefix of `size` that is real on disk - the rest
	// is undefined there and reads as zeros (classic entries carry no VDL: equals size).
	pub(super) valid_len: u64,
	pub(super) is_dir: bool,
	pub(super) first_cluster: u32,
	pub(super) no_fat_chain: bool,
	pub(super) set_off: usize,
	pub(super) ent_off: usize,
}

impl Raw {
	// A lookup matches the long name, or its 8.3 short form as the fallback.
	pub(super) fn matches(&self, name: &[u8]) -> bool {
		eq_ignore_case(self.name.as_bytes(), name) || (!self.short.is_empty() && eq_ignore_case(self.short.as_bytes(), name))
	}
}

// Mark the classic entry named `name` deleted in a directory's in-memory bytes - the 8.3
// record plus its long fragments - returning the parsed entry, or None if absent. The
// caller writes the bytes back and frees the chain once the write is safe; a directory
// cannot be unlinked this way.
pub(super) fn mark_unlinked(bytes: &mut [u8], name: &[u8]) -> Result<Option<Raw>, FsError> {
	let entries = parse_fat_dir(bytes)?;
	let Some(e) = entries.into_iter().find(|e| e.matches(name)) else {
		return Ok(None);
	};
	if e.is_dir {
		return Err(FsError::Invalid);
	}
	for off in (e.set_off..=e.ent_off).step_by(32) {
		bytes[off] = 0xE5;
	}
	Ok(Some(e))
}

// The exFAT counterpart: clear the in-use bit of every record in the named entry set,
// returning the parsed entry (first cluster, size, NoFatChain) so the caller can release
// its clusters the right way once the directory write is safe.
pub(super) fn exfat_mark_unlinked(bytes: &mut [u8], name: &[u8]) -> Result<Option<Raw>, FsError> {
	let entries = parse_exfat_dir(bytes)?;
	let Some(e) = entries.into_iter().find(|e| e.matches(name)) else {
		return Ok(None);
	};
	if e.is_dir {
		return Err(FsError::Invalid);
	}
	for off in (e.set_off..=e.ent_off).step_by(32) {
		bytes[off] &= 0x7F;
	}
	Ok(Some(e))
}

// Parse a classic (FAT12/16/32) directory region: 32-byte entries, with attr 0x0F VFAT
// long-name fragments accumulated ahead of the 8.3 short entry they describe. The
// fragments' checksums and sequence numbers are validated the way the media's home
// systems do - orphan fragments (a non-LFN-aware tool deleted only the 8.3 record)
// are discarded and the 8.3 name stands, never merged into a neighbor's name. Each
// entry records the byte range of its whole set so unlink can mark every record.
pub(super) fn parse_fat_dir(bytes: &[u8]) -> Result<Vec<Raw>, FsError> {
	let mut out: Vec<Raw> = Vec::new();
	// the long-name run in progress: its units, the checksum every fragment must carry,
	// the next expected (descending) sequence number, and where the run started. A
	// fragment that breaks any rule discards the run.
	let mut lfn: Vec<u16> = Vec::new();
	let mut lfn_sum = 0u8;
	let mut lfn_next = 0u8;
	let mut set_start: Option<usize> = None;
	let mut i = 0;
	while i + 32 <= bytes.len() {
		let off = i;
		let e = &bytes[i..i + 32];
		i += 32;
		if e[0] == 0x00 {
			break;
		}
		if e[0] == 0xE5 {
			lfn.clear();
			set_start = None;
			continue;
		}
		if e[11] == 0x0F {
			// a long-name fragment: 13 UTF-16 chars at offsets 1, 14, 28. The last
			// (highest-sequence) fragment is stored first and opens the run; the rest
			// must count the sequence down carrying the same checksum.
			let seq = e[0] & 0x1F;
			let frag = lfn_fragment(e);
			if e[0] & 0x40 != 0 && seq >= 1 {
				lfn = frag;
				lfn_sum = e[13];
				lfn_next = seq - 1;
				set_start = Some(off);
			} else if set_start.is_some() && seq >= 1 && seq == lfn_next && e[13] == lfn_sum {
				let mut merged = frag;
				merged.extend_from_slice(&lfn);
				lfn = merged;
				lfn_next -= 1;
			} else {
				lfn.clear();
				set_start = None;
			}
			continue;
		}
		if e[11] & 0x08 != 0 {
			lfn.clear();
			set_start = None;
			continue;
		}
		let short = short_name(e);
		// the run pairs with this entry only when it counted down to sequence 1 and its
		// checksum matches the 8.3 field - otherwise the fragments are orphans.
		let mut sf = [0u8; 11];
		sf.copy_from_slice(&e[0..11]);
		let valid = set_start.is_some() && lfn_next == 0 && !lfn.is_empty() && lfn_sum == lfn_checksum(&sf);
		let name = if valid { decode_utf16(&lfn) } else { short.clone() };
		let set_off = if valid { set_start.take().unwrap_or(off) } else { off };
		lfn.clear();
		set_start = None;
		// an empty-named entry (an all-spaces 8.3 field) is noise, never a real file.
		if name.is_empty() {
			continue;
		}
		let is_dir = e[11] & 0x10 != 0;
		let first_cluster = ((u16::from_le_bytes([e[20], e[21]]) as u32) << 16) | u16::from_le_bytes([e[26], e[27]]) as u32;
		let size = u32::from_le_bytes([e[28], e[29], e[30], e[31]]) as u64;
		out.push(Raw { name, short, size, valid_len: size, is_dir, first_cluster, no_fat_chain: false, set_off, ent_off: off });
	}
	Ok(out)
}

// The 13 UTF-16 units of one VFAT long-name fragment (offsets 1, 14, 28).
fn lfn_fragment(e: &[u8]) -> Vec<u16> {
	let mut part: Vec<u16> = Vec::new();
	for &r in &[1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
		part.push(u16::from_le_bytes([e[r], e[r + 1]]));
	}
	part
}

// Parse an exFAT directory: a file is an entry set of a 0x85 file, a 0xC0 stream
// extension (flags + length + first cluster), and one or more 0xC1 file-name fragments.
// The set checksum is verified the way the media's home systems do - a torn or forged
// set is skipped, never trusted. The stream's NoFatChain flag (bit 0x02) marks a
// contiguous file with no FAT chain - the common form Windows writes - which must be
// read and freed by length, never by following the FAT.
pub(super) fn parse_exfat_dir(bytes: &[u8]) -> Result<Vec<Raw>, FsError> {
	let mut out: Vec<Raw> = Vec::new();
	let mut i = 0;
	while i + 32 <= bytes.len() {
		let e = &bytes[i..i + 32];
		if e[0] == 0x00 {
			break;
		}
		if e[0] != 0x85 {
			i += 32;
			continue;
		}
		let secondary = e[1] as usize;
		// a set whose records run past the buffer, or whose stored checksum does not
		// match a recomputation over the whole set, is torn or forged - skip it.
		let set_len = (secondary + 1) * 32;
		if i + set_len > bytes.len() || u16::from_le_bytes([e[2], e[3]]) != exfat_set_checksum(&bytes[i..i + set_len]) {
			i += 32;
			continue;
		}
		let is_dir = u16::from_le_bytes([e[4], e[5]]) & 0x10 != 0;
		let mut name: Vec<u16> = Vec::new();
		let mut size = 0u64;
		let mut valid_len = 0u64;
		let mut first_cluster = 0u32;
		let mut name_len = 0usize;
		let mut no_fat_chain = false;
		let mut last = i;
		for k in 1..=secondary {
			let s = i + k * 32;
			if s + 32 > bytes.len() {
				break;
			}
			last = s;
			let x = &bytes[s..s + 32];
			if x[0] == 0xC0 {
				no_fat_chain = x[1] & 0x02 != 0;
				name_len = x[3] as usize;
				valid_len = u64::from_le_bytes([x[8], x[9], x[10], x[11], x[12], x[13], x[14], x[15]]);
				first_cluster = u32::from_le_bytes([x[20], x[21], x[22], x[23]]);
				size = u64::from_le_bytes([x[24], x[25], x[26], x[27], x[28], x[29], x[30], x[31]]);
			} else if x[0] == 0xC1 {
				for c in 0..15 {
					name.push(u16::from_le_bytes([x[2 + c * 2], x[3 + c * 2]]));
				}
			}
		}
		name.truncate(name_len);
		// a degenerate set with no name (a bare 0x85 with no secondaries, or a forged
		// zero name length) is noise, never a real file - skip it.
		if name.is_empty() {
			i += (secondary + 1) * 32;
			continue;
		}
		out.push(Raw { name: decode_utf16(&name), short: String::new(), size, valid_len, is_dir, first_cluster, no_fat_chain, set_off: i, ent_off: last });
		i += (secondary + 1) * 32;
	}
	Ok(out)
}

// Decode a UTF-16 name, dropping NUL padding and replacing anything invalid with '?'.
fn decode_utf16(units: &[u16]) -> String {
	let mut s = String::new();
	for c in char::decode_utf16(units.iter().copied().take_while(|&u| u != 0)) {
		s.push(c.unwrap_or('?'));
	}
	s
}

// The 8.3 short name of a classic entry: name, optional dot, extension, trimmed. The
// NT case flags (byte 12: 0x08 = lowercase base, 0x10 = lowercase extension) are
// honored - the media's home systems store a short-only lowercase name this way
// instead of a long-name set, and the listing must render what they display.
fn short_name(e: &[u8]) -> String {
	let mut raw = [0u8; 11];
	raw.copy_from_slice(&e[0..11]);
	// the 0x05 lead byte is the spec's escape for a real 0xE5, which would read as deleted.
	if raw[0] == 0x05 {
		raw[0] = 0xE5;
	}
	let flags = e.get(12).copied().unwrap_or(0);
	if flags & 0x08 != 0 {
		raw[0..8].make_ascii_lowercase();
	}
	if flags & 0x10 != 0 {
		raw[8..11].make_ascii_lowercase();
	}
	let base = trim_spaces(&raw[0..8]);
	let ext = trim_spaces(&raw[8..11]);
	let mut s = String::from_utf8_lossy(base).into_owned();
	if !ext.is_empty() {
		s.push('.');
		s.push_str(&String::from_utf8_lossy(ext));
	}
	s
}

// Drop trailing 0x20 padding from a fixed-width 8.3 field.
pub(super) fn trim_spaces(b: &[u8]) -> &[u8] {
	let mut end = b.len();
	while end > 0 && b[end - 1] == 0x20 {
		end -= 1;
	}
	&b[..end]
}

// Split a path into (parent dir, final name), rejecting an empty final name.
pub(super) fn split_parent(path: &[u8]) -> Result<(&[u8], &[u8]), FsError> {
	let trimmed = path.strip_suffix(b"/").unwrap_or(path);
	match trimmed.iter().rposition(|&b| b == b'/') {
		Some(p) => Ok((&trimmed[..p], &trimmed[p + 1..])),
		None => Ok((b"", trimmed)),
	}
}

// Compare two names ignoring ASCII case, as FAT lookups are case-insensitive. The fold
// is deliberately ASCII-only: the media's home systems fold the full range through
// their upcase table, so a non-ASCII pair ("Café" / "café") that matches there does
// not match here - a lookup by a name's exact bytes always works.
fn eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
	a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

// Validate a name for the write path - the gates of the media's home systems, owned by
// ONE function so the write entry point and the overwrite's name-reuse guard cannot
// drift apart. The length ceiling is 255 UTF-16 UNITS (the LFN and exFAT NameLength
// limit), not UTF-8 bytes - a long non-ASCII name within it is legal there.
pub(super) fn check_name(name: &[u8]) -> Result<(), FsError> {
	if name.is_empty() {
		return Err(FsError::TooLong);
	}
	// a name ending in a dot or a space (which covers "." and "..") collides with the
	// dot-entry semantics and is invalid on the media's home systems.
	if name.last().is_some_and(|&b| b == b'.' || b == b' ') {
		return Err(FsError::Invalid);
	}
	// the characters illegal in a long name there: control bytes and `" * : < > ? \ |`
	// (the `/` never reaches here - it is the separator).
	if name.iter().any(|&b| b < 0x20 || b"\"*:<>?\\|".contains(&b)) {
		return Err(FsError::Invalid);
	}
	// the long-name forms store UTF-16: a name that is not valid UTF-8 would be stored
	// lossily (U+FFFD) and never found again by the bytes it was created with.
	let Ok(s) = core::str::from_utf8(name) else {
		return Err(FsError::Invalid);
	};
	if s.encode_utf16().count() > 255 {
		return Err(FsError::TooLong);
	}
	Ok(())
}

// Whether a name parsed off the medium may be re-emitted into a fresh entry set. A
// foreign entry can carry a name no legal write produces (a lossy decode renders an
// invalid unit as '?', an illegal character); an overwrite must not write such a name
// back, it falls back to the caller's instead.
pub(super) fn writable_name(name: &[u8]) -> bool {
	check_name(name).is_ok()
}

// Build a directory entry set: the 8.3 short entry, preceded by VFAT long-name fragments
// when the name is not a plain uppercase 8.3 name. Fragments are emitted last-first. The
// short form is generated against the directory's existing entries so it is unique
// (numeric-tailed when the name is lossy or collides) and spec-legal. The entry is
// stamped with `ts` (a DOS date/time pair) as its create and write time.
pub(super) fn build_entries(name: &[u8], dir_bytes: &[u8], first: u32, size: u32, attr: u8, ts: (u16, u16)) -> Result<Vec<[u8; 32]>, FsError> {
	let short = gen_short(name, dir_bytes)?;
	let mut out: Vec<[u8; 32]> = Vec::new();
	if name != short_name(&short).as_bytes() {
		let units: Vec<u16> = String::from_utf8_lossy(name).encode_utf16().collect();
		let sum = lfn_checksum(&short);
		let frags = units.len().div_ceil(13).max(1);
		for f in (0..frags).rev() {
			let mut e = [0u8; 32];
			e[0] = (f as u8 + 1) | if f + 1 == frags { 0x40 } else { 0 };
			e[11] = 0x0F;
			e[13] = sum;
			for c in 0..13 {
				let idx = f * 13 + c;
				let v = if idx < units.len() {
					units[idx]
				} else if idx == units.len() {
					0
				} else {
					0xFFFF
				};
				let pos = [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30][c];
				e[pos..pos + 2].copy_from_slice(&v.to_le_bytes());
			}
			out.push(e);
		}
	}
	let mut e = [0u8; 32];
	e[0..11].copy_from_slice(&short);
	e[11] = attr;
	e[14..16].copy_from_slice(&ts.1.to_le_bytes());
	e[16..18].copy_from_slice(&ts.0.to_le_bytes());
	e[18..20].copy_from_slice(&ts.0.to_le_bytes());
	e[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
	e[22..24].copy_from_slice(&ts.1.to_le_bytes());
	e[24..26].copy_from_slice(&ts.0.to_le_bytes());
	e[26..28].copy_from_slice(&(first as u16).to_le_bytes());
	e[28..32].copy_from_slice(&size.to_le_bytes());
	out.push(e);
	Ok(out)
}

// Generate a spec-legal 8.3 short name for `name`, unique within the directory: leading
// dots are stripped, the base/extension split at the last dot, 8.3-illegal bytes map to
// '_' (uppercased), and a lossy or too-long or colliding basis gains a `~N` numeric
// tail checked against the directory's existing short names - so two long names with a
// common prefix never produce identical short entries.
fn gen_short(name: &[u8], dir_bytes: &[u8]) -> Result<[u8; 11], FsError> {
	let stripped = {
		let mut s = name;
		while let Some(rest) = s.strip_prefix(b".") {
			s = rest;
		}
		s
	};
	let dot = stripped.iter().rposition(|&b| b == b'.');
	let (base_raw, ext_raw): (&[u8], &[u8]) = match dot {
		Some(p) => (&stripped[..p], &stripped[p + 1..]),
		None => (stripped, b""),
	};
	let mut lossy = stripped.len() != name.len() || base_raw.len() > 8 || ext_raw.len() > 3;
	let mut base = [0x20u8; 8];
	let mut base_len = 0usize;
	for &b in base_raw.iter().take(8) {
		let (mapped, replaced) = short_char(b);
		lossy |= replaced;
		base[base_len] = mapped;
		base_len += 1;
	}
	// a real leading 0xE5 is stored as 0x05 per the spec, or the entry reads as deleted.
	if base[0] == 0xE5 {
		base[0] = 0x05;
	}
	let mut short = [0x20u8; 11];
	short[..8].copy_from_slice(&base);
	for (i, &b) in ext_raw.iter().take(3).enumerate() {
		let (mapped, replaced) = short_char(b);
		lossy |= replaced;
		short[8 + i] = mapped;
	}
	let existing = existing_shorts(dir_bytes);
	if !lossy && base_len > 0 && !existing.contains(&short) {
		return Ok(short);
	}
	// numeric tail: BASIS~N in the base columns, N growing until the name is unique.
	for n in 1u32..1_000_000 {
		let mut digits = [0u8; 7];
		let mut len = 0usize;
		let mut v = n;
		while v > 0 {
			digits[len] = b'0' + (v % 10) as u8;
			len += 1;
			v /= 10;
		}
		let keep = base_len.min(8 - 1 - len);
		let mut cand = short;
		cand[..8].fill(0x20);
		cand[..keep].copy_from_slice(&base[..keep]);
		cand[keep] = b'~';
		for d in 0..len {
			cand[keep + 1 + d] = digits[len - 1 - d];
		}
		if !existing.contains(&cand) {
			return Ok(cand);
		}
	}
	Err(FsError::NoSpace)
}

// Map one byte into the 8.3 character set: uppercased, with the spec's illegal set
// (control bytes, space, `" * + , . / : ; < = > ? [ \ ] |` and DEL) replaced by '_'.
// Returns the mapped byte and whether it changed in a way that makes the name lossy
// (uppercasing alone is not lossy - the long name records the case).
pub(super) fn short_char(b: u8) -> (u8, bool) {
	let illegal = b < 0x20 || b == 0x7F || b" \"*+,./:;<=>?[\\]|".contains(&b);
	if illegal { (b'_', true) } else { (b.to_ascii_uppercase(), false) }
}

// Collect the 8.3 name fields of a directory's live entries, for uniqueness checks.
pub(super) fn existing_shorts(bytes: &[u8]) -> Vec<[u8; 11]> {
	let mut out: Vec<[u8; 11]> = Vec::new();
	let mut i = 0usize;
	while i + 32 <= bytes.len() {
		let e = &bytes[i..i + 32];
		i += 32;
		if e[0] == 0x00 {
			break;
		}
		if e[0] == 0xE5 || e[11] == 0x0F || e[11] & 0x08 != 0 {
			continue;
		}
		let mut s = [0u8; 11];
		s.copy_from_slice(&e[0..11]);
		out.push(s);
	}
	out
}

// The VFAT checksum of an 8.3 name: a byte-by-byte rotate-and-add, stamped on every long
// fragment so a stale fragment cannot be paired with the wrong short entry.
fn lfn_checksum(short: &[u8; 11]) -> u8 {
	let mut sum: u8 = 0;
	for &b in short {
		sum = sum.rotate_right(1).wrapping_add(b);
	}
	sum
}

// Unix seconds into the DOS (date, time) pair: date = (year-1980)<<9 | month<<5 | day,
// time = hours<<11 | minutes<<5 | seconds/2. The year clamps to the DOS 1980-2107
// range, so an unset clock (0) still yields the valid epoch date 1980-01-01.
pub(super) fn dos_datetime(unix: u64) -> (u16, u16) {
	let (y, m, d) = civil_from_days((unix / 86400) as i64);
	let y = y.clamp(1980, 2107);
	let date = (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16;
	let secs = unix % 86400;
	let time = (((secs / 3600) as u16) << 11) | ((((secs % 3600) / 60) as u16) << 5) | ((secs % 60) / 2) as u16;
	(date, time)
}

// Days since the Unix epoch into a (year, month, day) civil date - the standard
// era/day-of-era decomposition over the Gregorian 400-year cycle.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
	let z = days + 719468;
	let era = if z >= 0 { z } else { z - 146096 } / 146097;
	let doe = (z - era * 146097) as u64;
	let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
	let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
	let mp = (5 * doy + 2) / 153;
	let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
	let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
	let y = yoe as i64 + era * 400;
	(if m <= 2 { y + 1 } else { y }, m, d)
}

// Everything from the first 0x00 entry of a directory region is free space by
// specification, whatever stale bytes a corrupt volume left past it - zero the tail so
// a new entry set never lands where the parser (which stops at the terminator) cannot
// see it, and stale garbage never turns into live entries when the terminator moves.
pub(super) fn scrub_after_terminator(bytes: &mut [u8]) {
	let mut i = 0usize;
	while i + 32 <= bytes.len() {
		if bytes[i] == 0x00 {
			bytes[i..].fill(0);
			return;
		}
		i += 32;
	}
}

// Find the offset of the first run of `n` free entries (0x00 fresh or 0xE5 deleted) in a
// directory region, or None when there is no contiguous gap that large.
pub(super) fn free_run(bytes: &[u8], n: usize) -> Option<usize> {
	let mut run = 0usize;
	let mut i = 0usize;
	while i + 32 <= bytes.len() {
		if bytes[i] == 0x00 || bytes[i] == 0xE5 {
			run += 1;
			if run == n {
				return Some(i + 32 - n * 32);
			}
		} else {
			run = 0;
		}
		i += 32;
	}
	None
}

// Build an exFAT entry set for `name`: a 0x85 file entry, a 0xC0 stream extension (FAT
// chain, length, first cluster), and 0xC1 name fragments, stamped with the set checksum
// and `ts` (the exFAT 32-bit timestamp) as its create/modify/access time, marked UTC.
pub(super) fn build_exfat_set(name: &[u8], first: u32, size: u64, ts: u32) -> Vec<u8> {
	let units: Vec<u16> = String::from_utf8_lossy(name).encode_utf16().collect();
	let frags = units.len().div_ceil(15);
	let count = 1 + frags;
	let mut set = vec![0u8; (count + 1) * 32];
	set[0] = 0x85;
	set[1] = count as u8;
	set[4..6].copy_from_slice(&0x20u16.to_le_bytes());
	for field in [8usize, 12, 16] {
		set[field..field + 4].copy_from_slice(&ts.to_le_bytes());
	}
	// the UtcOffset fields: bit 0x80 = valid, offset 0 = UTC.
	set[22] = 0x80;
	set[23] = 0x80;
	set[24] = 0x80;
	set[32] = 0xC0;
	set[33] = 0x01;
	set[35] = units.len() as u8;
	set[36..38].copy_from_slice(&exfat_name_hash(&units).to_le_bytes());
	set[40..48].copy_from_slice(&size.to_le_bytes());
	set[52..56].copy_from_slice(&first.to_le_bytes());
	set[56..64].copy_from_slice(&size.to_le_bytes());
	for f in 0..frags {
		let base = (2 + f) * 32;
		set[base] = 0xC1;
		for c in 0..15 {
			let idx = f * 15 + c;
			let v = if idx < units.len() { units[idx] } else { 0 };
			set[base + 2 + c * 2..base + 4 + c * 2].copy_from_slice(&v.to_le_bytes());
		}
	}
	let sum = exfat_set_checksum(&set);
	set[2..4].copy_from_slice(&sum.to_le_bytes());
	set
}

// The exFAT entry-set checksum: a 16-bit rotate-and-add over every byte of the set,
// skipping the two checksum bytes (2, 3) of the first entry where the value lands.
pub(super) fn exfat_set_checksum(set: &[u8]) -> u16 {
	let mut sum: u16 = 0;
	for (i, &b) in set.iter().enumerate() {
		if i == 2 || i == 3 {
			continue;
		}
		sum = sum.rotate_right(1).wrapping_add(b as u16);
	}
	sum
}

// The exFAT file-name hash, over the UTF-16LE bytes of the UP-CASED name as the
// specification defines it: the media's home systems recompute it on lookup and skip a
// set whose stored hash mismatches, so hashing the name as written would leave a
// lowercase-named file listable but unopenable by name there. ASCII upcasing (a driver
// without an upcase table); other units pass through.
fn exfat_name_hash(units: &[u16]) -> u16 {
	let mut hash: u16 = 0;
	for &u in units {
		let up = if (0x61..=0x7A).contains(&u) { u - 0x20 } else { u };
		for b in up.to_le_bytes() {
			hash = hash.rotate_right(1).wrapping_add(b as u16);
		}
	}
	hash
}

// Find the first run of `n` free exFAT entries (0x00 fresh or a cleared type bit) in a
// directory region, or None when there is no contiguous gap that large.
pub(super) fn exfat_free_run(bytes: &[u8], n: usize) -> Option<usize> {
	let mut run = 0usize;
	let mut i = 0usize;
	while i + 32 <= bytes.len() {
		if bytes[i] == 0x00 || bytes[i] & 0x80 == 0 {
			run += 1;
			if run == n {
				return Some(i + 32 - n * 32);
			}
		} else {
			run = 0;
		}
		i += 32;
	}
	None
}
