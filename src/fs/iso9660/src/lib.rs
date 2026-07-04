//! ISO9660 - a read-only backend for optical and install media (CD-ROMs, `.iso`
//! images), behind the same [`BlockDevice`] trait FAT and LiberFS use. It sits behind
//! `Storage.Volume` as just another FS backend: per the layering principle, several
//! filesystems mount behind one volume API, and ISO9660 is the ubiquitous install/boot
//! image format so reading it makes that media browsable.
//!
//! Read-only by design - no allocation or write path. Mounting scans the volume
//! descriptors from logical block 16 for a Primary Volume Descriptor (`CD001` magic); a
//! Joliet Supplementary descriptor, when present, is preferred so files keep their long
//! Unicode names. A file is found by walking `/`-separated path segments from the root,
//! each lookup scanning a directory's records (which never span a logical block) and
//! following the extent of the next directory or file. Names come from the directory
//! record, decoded as Joliet UCS-2, a Rock Ridge `NM` system-use entry, or plain 8.3 with
//! the `;1` version suffix stripped.
//!
//! The media is untrusted: every extent is bounded by the volume's own block count
//! (whose last block is verified to exist on the device at mount) before a buffer is
//! allocated, and a malformed record parses cleanly instead of panicking - a corrupt
//! or hostile disc errors, never crashes or exhausts the mounting service.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

// One logical block. ISO9660 sets a block size in the PVD, but it is 2048 in practice
// and that is the unit a `.iso` and an optical drive read in; the device reads one
// 2048-byte block at a time, by absolute LBA.
pub const SECTOR_SIZE: usize = 2048;

// The volume descriptors begin here; LBAs 0..16 are the boot/system area.
const FIRST_DESCRIPTOR_LBA: u64 = 16;

// A block device: optical media is read one 2048-byte logical block at a time, by
// absolute LBA. Implementors map that onto their backing (disc sectors, a Vec). The
// backend never writes, so there is no write_block.
pub trait BlockDevice {
	// Read block `lba` into `buf` (exactly SECTOR_SIZE bytes). False on I/O failure.
	fn read_block(&mut self, lba: u64, buf: &mut [u8]) -> bool;
}

// An ISO9660 error. The variants map onto the `Storage.Volume` `error` enum at the
// service boundary (NotFound -> not-found, the rest -> invalid).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
	NotFound,
	Invalid,
	TooLong,
	Io,
}

// One directory entry: a name, a byte length, and whether it is a subdirectory. The
// listing the shell shows; a directory reports a length of zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
	pub name: String,
	pub size: u64,
	pub is_dir: bool,
}

// The root directory's extent (LBA and byte length), the volume's own block count
// (bounding every extent), and whether names are Joliet UCS-2; every read derives from
// these, so mounting is just locating one volume descriptor.
struct Geometry {
	root_lba: u32,
	root_len: u32,
	blocks: u32,
	joliet: bool,
}

// A mounted ISO9660 volume: the device plus its geometry. Reads are on demand, so
// nothing is cached beyond the root extent; a directory or file is read by following its
// extent as asked.
pub struct Iso9660<D: BlockDevice> {
	dev: D,
	geo: Geometry,
}

impl<D: BlockDevice> Iso9660<D> {
	// The block device this filesystem reads through.
	pub fn device(&self) -> &D {
		&self.dev
	}
	// Mount optical media: scan the volume descriptors for a Primary (and a preferred
	// Joliet) descriptor and take its root directory record. None if no PVD is found.
	pub fn mount(mut dev: D) -> Option<Iso9660<D>> {
		let mut pvd_root: Option<((u32, u32), u32, u32)> = None;
		let mut joliet_root: Option<((u32, u32), u32, u32)> = None;
		let mut block = [0u8; SECTOR_SIZE];
		for i in 0..32 {
			if !dev.read_block(FIRST_DESCRIPTOR_LBA + i, &mut block) {
				return None;
			}
			if &block[1..6] != b"CD001" {
				return None;
			}
			let found = (root_extent(&block), le32(&block[80..84]), u16::from_le_bytes([block[128], block[129]]) as u32);
			match block[0] {
				1 => pvd_root = Some(found),
				2 if is_joliet(&block) => joliet_root = Some(found),
				255 => break,
				_ => {}
			}
		}
		let (joliet, ((root_lba, root_len), blocks, block_size)) = match (joliet_root, pvd_root) {
			(Some(r), _) => (true, r),
			(None, Some(r)) => (false, r),
			(None, None) => return None,
		};
		// the logical block size is 2048 on real media and the unit this backend reads
		// in - any other legal size would be read at wrong positions, so it refuses -
		// and the root extent must fit the volume's own block count.
		if block_size != SECTOR_SIZE as u32 || blocks == 0 || root_lba as u64 + (root_len as u64).div_ceil(SECTOR_SIZE as u64) > blocks as u64 {
			return None;
		}
		// the block count is the medium's own claim: its last block must exist on the
		// device, or a forged or truncated image mounts and only fails - or allocates
		// without bound - inside a later read. The real media size then bounds every
		// extent read.
		if !dev.read_block(blocks as u64 - 1, &mut block) {
			return None;
		}
		Some(Iso9660 { dev, geo: Geometry { root_lba, root_len, blocks, joliet } })
	}

	// List the volume's root directory.
	pub fn list(&mut self) -> Result<Vec<FileInfo>, FsError> {
		self.read_dir(self.geo.root_lba, self.geo.root_len)
	}

	// List a subdirectory named by a `/`-separated path. An empty path is the root.
	pub fn list_dir(&mut self, path: &[u8]) -> Result<Vec<FileInfo>, FsError> {
		let (lba, len) = self.resolve_dir(path)?;
		self.read_dir(lba, len)
	}

	// Read a whole file named by a `/`-separated path into a Vec.
	pub fn read_file(&mut self, path: &[u8]) -> Result<Vec<u8>, FsError> {
		let (parent, name) = split_parent(path)?;
		let (lba, len) = self.resolve_dir(parent)?;
		let entry = self.find_entry(lba, len, name)?;
		if entry.is_dir {
			return Err(FsError::NotFound);
		}
		// a multi-extent or interleaved file (segments in further records, or gap blocks
		// woven into the extent) is not assembled here - refuse it rather than serve a
		// truncated or gap-riddled read as the whole.
		if entry.unsupported {
			return Err(FsError::Invalid);
		}
		self.read_extent(entry.lba, entry.size)
	}

	// Walk path segments from the root, descending into each named subdirectory, and
	// return the final directory's extent. An empty path is the root.
	fn resolve_dir(&mut self, path: &[u8]) -> Result<(u32, u32), FsError> {
		let mut lba = self.geo.root_lba;
		let mut len = self.geo.root_len;
		for seg in path.split(|&b| b == b'/').filter(|s| !s.is_empty()) {
			let entry = self.find_entry(lba, len, seg)?;
			if !entry.is_dir {
				return Err(FsError::NotFound);
			}
			lba = entry.lba;
			len = entry.size;
		}
		Ok((lba, len))
	}

	// Scan a directory extent for an entry whose name matches `name` (case-insensitively).
	// The "." / ".." self/parent records match by those names, so paths through them
	// resolve the way the other backends behind the volume API resolve them.
	fn find_entry(&mut self, lba: u32, len: u32, name: &[u8]) -> Result<Entry, FsError> {
		let data = self.read_extent(lba, len)?;
		let mut off = 0usize;
		while off < data.len() {
			let rec_len = data[off] as usize;
			if rec_len == 0 {
				// Records never span a block; a zero length skips to the next block.
				off = (off / SECTOR_SIZE + 1) * SECTOR_SIZE;
				continue;
			}
			if off + rec_len > data.len() {
				break;
			}
			let rec = &data[off..off + rec_len];
			if let Some(e) = parse_record(rec, self.geo.joliet)
				&& !e.name.is_empty()
				&& eq_ci(&e.name, name)
			{
				return Ok(e);
			}
			off += rec_len;
		}
		Err(FsError::NotFound)
	}

	// Read every record in a directory extent into FileInfos, skipping the "." / ".."
	// self/parent entries.
	fn read_dir(&mut self, lba: u32, len: u32) -> Result<Vec<FileInfo>, FsError> {
		let data = self.read_extent(lba, len)?;
		let mut out = Vec::new();
		let mut off = 0usize;
		while off < data.len() {
			let rec_len = data[off] as usize;
			if rec_len == 0 {
				off = (off / SECTOR_SIZE + 1) * SECTOR_SIZE;
				continue;
			}
			if off + rec_len > data.len() {
				break;
			}
			if let Some(e) = parse_record(&data[off..off + rec_len], self.geo.joliet)
				&& !e.special
				&& !e.name.is_empty()
				// records order equal names adjacently with versions descending, so a
				// multi-version file lists once, as its highest version.
				&& !out.last().is_some_and(|p: &FileInfo| p.name == e.name)
			{
				// a directory reports a length of zero - the FileInfo contract,
				// uniform across the backends behind the volume API.
				out.push(FileInfo { name: e.name, size: if e.is_dir { 0 } else { e.size as u64 }, is_dir: e.is_dir });
			}
			off += rec_len;
		}
		Ok(out)
	}

	// Read `size` bytes starting at logical block `lba`, one block at a time. The extent
	// is the medium's own claim: one that would leave the volume is refused BEFORE the
	// buffer is allocated - a forged length can neither allocate without bound nor read
	// past the volume.
	fn read_extent(&mut self, lba: u32, size: u32) -> Result<Vec<u8>, FsError> {
		if lba as u64 + (size as u64).div_ceil(SECTOR_SIZE as u64) > self.geo.blocks as u64 {
			return Err(FsError::Invalid);
		}
		let mut out = vec![0u8; size as usize];
		let mut block = [0u8; SECTOR_SIZE];
		let mut done = 0usize;
		let mut cur = lba as u64;
		while done < out.len() {
			if !self.dev.read_block(cur, &mut block) {
				return Err(FsError::Io);
			}
			let n = (out.len() - done).min(SECTOR_SIZE);
			out[done..done + n].copy_from_slice(&block[..n]);
			done += n;
			cur += 1;
		}
		Ok(out)
	}
}

// One parsed directory record: its extent, byte length, kind, decoded name, whether it
// is a "." / ".." self/parent entry (named so, matched by lookups, skipped in listings),
// and whether it takes a form this backend refuses rather than misreads (multi-extent
// or interleaved).
struct Entry {
	lba: u32,
	size: u32,
	is_dir: bool,
	special: bool,
	unsupported: bool,
	name: String,
}

// Take a volume descriptor's root directory record (fixed 34 bytes at offset 156): its
// extent LBA and data length, both stored little-endian first. The root record can
// carry an XAR length too - its data follows those blocks, like any record's.
fn root_extent(desc: &[u8]) -> (u32, u32) {
	let r = &desc[156..156 + 34];
	(le32(&r[2..6]).saturating_add(r[1] as u32), le32(&r[10..14]))
}

// A type-2 descriptor is Joliet when its escape sequences select UCS-2 (%/@, %/C, %/E)
// anywhere in the field - a descriptor may list several and UCS-2 need not be first.
fn is_joliet(desc: &[u8]) -> bool {
	let esc = &desc[88..120];
	[b"%/@".as_slice(), b"%/C", b"%/E"].iter().any(|s| esc.windows(3).any(|w| w == *s))
}

// Parse a directory record: extent, length, dir flag, and the name (Joliet UCS-2, a Rock
// Ridge NM entry, or plain 8.3 with the version suffix stripped). None on a short record.
fn parse_record(rec: &[u8], joliet: bool) -> Option<Entry> {
	if rec.len() < 33 {
		return None;
	}
	// an Extended Attribute Record occupies rec[1] blocks at the extent's START - the
	// data follows it, and serving the XAR as content would be a silent misread (the
	// extent gate bounds the advanced LBA like any other).
	let lba = le32(&rec[2..6]).saturating_add(rec[1] as u32);
	let size = le32(&rec[10..14]);
	let is_dir = rec[25] & 0x02 != 0;
	// an associated file (flag 0x04) is a secondary stream recorded BEFORE its
	// same-named main file - it must neither list (a duplicate name) nor match a
	// lookup (it would shadow the main content).
	if rec[25] & 0x04 != 0 {
		return None;
	}
	// multi-extent (segments in further records) and interleaving (gap blocks woven
	// into the extent) are forms the reader refuses rather than misreads.
	let unsupported = rec[25] & 0x80 != 0 || rec[26] != 0 || rec[27] != 0;
	let id_len = rec[32] as usize;
	if 33 + id_len > rec.len() {
		return None;
	}
	let id = &rec[33..33 + id_len];
	let special = id_len == 1 && (id[0] == 0 || id[0] == 1);
	let name = if special { String::from(if id[0] == 0 { "." } else { ".." }) } else { decode_name(id, rec, id_len, joliet) };
	Some(Entry { lba, size, is_dir, special, unsupported, name })
}

// Decode an entry name. Joliet is big-endian UCS-2; otherwise a Rock Ridge NM entry in
// the system-use area wins, falling back to plain ASCII 8.3 with ";version" dropped.
fn decode_name(id: &[u8], rec: &[u8], id_len: usize, joliet: bool) -> String {
	if joliet {
		let mut s = String::new();
		for c in id.chunks_exact(2) {
			let u = u16::from_be_bytes([c[0], c[1]]);
			if u == b';' as u16 {
				break;
			}
			s.push(char::from_u32(u as u32).unwrap_or('?'));
		}
		return s;
	}
	let sys_off = 33 + id_len + (id_len % 2 == 0) as usize;
	// a malformed record can end exactly after its identifier (the pad byte missing) -
	// there is no system-use area to read then, never a slice past the record.
	if let Some(sys) = rec.get(sys_off..)
		&& let Some(n) = rock_ridge_name(sys)
	{
		return n;
	}
	let mut s = String::new();
	for &b in id {
		if b == b';' {
			break;
		}
		s.push(b as char);
	}
	if s.ends_with('.') {
		s.pop();
	}
	s
}

// Find a Rock Ridge "NM" name in a record's system-use area: entries are sig(2), len,
// version, then NM's flags byte and the name bytes. None if there is no NM entry.
// Continuation areas (CE) are not followed, a SUSP skip offset (SP) is not applied, and
// deep-directory relocation (CL / PL / RE) is not interpreted - a name kept in a
// continuation degrades cleanly to the shorter NM prefix or the 8.3 form, and a tree
// mastered deeper than eight levels shows its relocation artifacts where the mastering
// tool placed them (everything stays reachable).
fn rock_ridge_name(sys: &[u8]) -> Option<String> {
	let mut off = 0usize;
	let mut out = String::new();
	while off + 4 <= sys.len() {
		let len = sys[off + 2] as usize;
		if len < 4 || off + len > sys.len() {
			break;
		}
		// an NM payload begins after sig, len, version, and flags - a shorter entry
		// carries no name and must not build an inverted range.
		if &sys[off..off + 2] == b"NM" && len >= 5 {
			out.push_str(core::str::from_utf8(&sys[off + 5..off + len]).unwrap_or(""));
		}
		off += len;
	}
	if out.is_empty() { None } else { Some(out) }
}

// Split a `/`-separated path into (parent dir, final name); errors on an empty name.
fn split_parent(path: &[u8]) -> Result<(&[u8], &[u8]), FsError> {
	let path = path.strip_prefix(b"/").unwrap_or(path);
	match path.iter().rposition(|&b| b == b'/') {
		Some(i) => Ok((&path[..i], &path[i + 1..])),
		None => Ok((b"", path)),
	}
}

// Case-insensitive ASCII name compare (8.3 names are stored uppercase, queries may not be).
fn eq_ci(a: &str, b: &[u8]) -> bool {
	a.len() == b.len() && a.bytes().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

// A little-endian u32 from a 4-byte slice; both-endian fields store LE first.
fn le32(b: &[u8]) -> u32 {
	u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
