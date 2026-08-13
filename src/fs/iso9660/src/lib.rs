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
//! ## The subset this reads, stated as one
//!
//! ECMA-119 permits logical block sizes up to the logical sector size; this backend reads volumes
//! whose block size is 2048 and refuses the rest. That is a product decision - every disc a
//! mastering tool produces uses 2048 - and not an omission, so it is said here rather than
//! discovered from a refusal.
//!
//! Also deliberately outside it: multi-volume sets (a record naming a volume other than 1 is
//! refused rather than read at that LBA on whichever disc is present), multi-extent and interleaved
//! files (refused, and left out of listings rather than described with a length that is neither the
//! file's nor anything else's), and the Rock Ridge entries that continue or relocate - `CE`, `CL`,
//! `PL`, `RE` and a continued `NM` - where the reader falls back to the ISO9660 identifier instead
//! of serving a name that is half of one.
//!
//! The media is untrusted: every extent is bounded by the volume's own block count
//! (whose last block is verified to exist on the device at mount) before a buffer is
//! allocated, and a malformed record parses cleanly instead of panicking - a corrupt
//! or hostile disc errors, never crashes or exhausts the mounting service.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

// One logical block. ISO9660 sets a block size in the PVD, but it is 2048 in practice
// and that is the unit a `.iso` and an optical drive read in; the device reads one
// 2048-byte block at a time, by absolute LBA.
pub const SECTOR_SIZE: usize = 2048;

// The most one `read_file` may allocate.
//
// A disc states a file's length and this reader believed it. 64 MB is far above anything an
// optical image holds as a single file that a service is expected to buffer whole, and far below
// what would trouble the service buffering it. A file past it is `TooLarge`, which `fs-core`
// documents as "the answer does not fit in one buffer; a ranged read is the solution" - which is
// exactly the state of affairs.
const MAX_READ_BYTES: usize = 64 * 1024 * 1024;

// The most entries one directory listing may return.
//
// Far above what any disc this reader is aimed at holds in one directory, and far below what an
// image can legally declare. A directory past it is `TooLarge` - "the answer does not fit in one
// buffer" - rather than an allocation the service cannot make.
const MAX_DIR_ENTRIES: usize = 65536;

// The volume descriptors begin here; LBAs 0..16 are the boot/system area.
const FIRST_DESCRIPTOR_LBA: u64 = 16;

// A block device: optical media is read one 2048-byte logical block at a time, by
// absolute LBA. The trait is the shared fs-core one (a block is exactly `buf.len()`
// bytes); ISO9660 is read-only, so it uses only `read_block` and keeps fs-core's
// refuse-write and no-op-flush defaults.
pub use fscore::BlockDevice;

// An ISO9660 error. The variants map onto the `Storage.Volume` `error` enum at the
// service boundary (NotFound -> not-found, the rest -> invalid). The type is the shared
// fs-core one, so every backend reports through one error enum; ISO9660 uses only the
// read subset.
pub use fscore::FsError;

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
	// Whether SUSP is present at all and RRIP was announced in it, and SUSP's own offset into every
	// System Use Area. Established once, from the root directory's first record, because that is
	// where SUSP puts `SP`. Rock Ridge names are read only when this says the extension is actually
	// in use - the alternative is believing `NM` wherever those two bytes happen to appear.
	rrip: bool,
	susp_skip: usize,
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
		let mut terminated = false;
		for i in 0..32 {
			if !dev.read_block(FIRST_DESCRIPTOR_LBA + i, &mut block) {
				return None;
			}
			if &block[1..6] != b"CD001" {
				return None;
			}
			// The descriptor VERSION, which was read past entirely. It is 1 for every descriptor
			// this format defines, and it matters most for the type-2 descriptors, where a few
			// bytes decide that this is Joliet.
			if block[6] != 1 {
				return None;
			}
			// The TYPE first: a terminator carries none of the fields below, so reading them out of
			// it and requiring them to be well-formed refuses the very descriptor that says the set
			// is complete.
			if block[0] == 255 {
				terminated = true;
				break;
			}
			if block[0] != 1 && block[0] != 2 {
				continue;
			}
			// Both halves of the volume space size and of the logical block size, which were taken
			// from their little ends alone.
			let Some(blocks) = both32(&block[80..88]) else { return None };
			let Some(block_size) = both16(&block[128..132]) else { return None };
			let Some(root) = root_extent(&block) else { return None };
			let found = (root, blocks, block_size as u32);
			match block[0] {
				1 => {
					// THE VOLUME SET, at the descriptor. The header states that multi-volume sets
					// are outside this reader's subset and refused, and one line enforced it: a
					// per-RECORD sequence number check, halfway through a listing. A set larger
					// than one is refused here, which is where a reader that does not implement
					// multi-volume should say so - at mount, before anything is read.
					let Some(set_size) = both16(&block[120..124]) else { return None };
					// EXACTLY one. `> 1` admitted zero, which is not a legal set size - a volume
					// belongs to a set of at least itself.
					if set_size != 1 {
						return None;
					}
					// And this volume's own sequence number within that set. The root record's and
					// every ordinary record's were checked and the DESCRIPTOR's was not, which is
					// the one that says which volume of the set this is.
					let Some(sequence) = both16(&block[124..128]) else { return None };
					if sequence != 1 {
						return None;
					}
					pvd_root = Some(found)
				}
				2 if is_joliet(&block) => joliet_root = Some(found),
				_ => {}
			}
		}
		// The Volume Descriptor Set Terminator is REQUIRED. Nothing recorded whether one was ever
		// seen, so a set that simply stopped - or ran past the thirty-second sector this scan
		// bounds itself with - mounted anyway. The limit is a good one and reaching it is now a
		// refusal rather than a silent truncation of the search.
		if !terminated {
			return None;
		}
		// A Joliet SVD supplies the namespace, and only ON TOP of a valid Primary Volume Descriptor.
		// The match answered `(Some(joliet), _)`, so a recognised Joliet descriptor alone was enough
		// and `pvd_root` being None was not an obstacle - which ECMA-119 does not allow.
		let Some(primary) = pvd_root else { return None };
		let (joliet, ((root_lba, root_len), blocks, block_size)) = match joliet_root {
			Some(r) => (true, r),
			None => (false, primary),
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
		// SUSP, established once from the root directory's own first record - which is where the
		// standard puts `SP`. Joliet volumes do not use Rock Ridge names, so the question is only
		// asked for the ISO9660 namespace.
		let mut rrip = false;
		let mut susp_skip = 0usize;
		if !joliet && dev.read_block(root_lba as u64, &mut block) && block[0] as usize >= 34 {
			let id_len = block[32] as usize;
			let sys_off = 33 + id_len + (id_len % 2 == 0) as usize;
			if let Some(sys) = block.get(sys_off..block[0] as usize)
				&& let Some(skip) = susp_skip_of(sys)
			{
				susp_skip = skip;
				// `ER` says WHICH extension is in use. Without it SUSP is present and Rock Ridge is
				// not announced, and reading `NM` would be the same guess as before with an extra
				// step.
				//
				// AND IT MAY LIVE BEHIND A `CE`. A directory record's own system-use area is what
				// is left of a 255-byte record, and `SP`, `PX` and `TF` fill most of it - so SUSP
				// has a Continuation Entry pointing at a block where the rest goes, and that is
				// where `xorriso` puts the `ER`. Scanning the record's own area alone found no
				// announcement on any disc a real mastering tool produces, so every one of them
				// fell back to 8.3 names: `A-Long.Name.txt` came back as `A_LONG_N.TXT`.
				//
				// Found by the first golden image from an independent tool, which is exactly what
				// that test exists for.
				let sys: alloc::vec::Vec<u8> = sys.to_vec();
				rrip = announces_rockridge(&sys) || {
					let mut found = false;
					let mut area = sys.clone();
					// Bounded: SUSP allows a chain, and a disc may name one that loops.
					for _ in 0..4 {
						let Some((next_lba, offset, len)) = continuation_of(&area) else { break };
						let mut cont = [0u8; SECTOR_SIZE];
						if next_lba as u64 >= blocks as u64 || !dev.read_block(next_lba as u64, &mut cont) {
							break;
						}
						let Some(part) = cont.get(offset..offset.saturating_add(len)) else { break };
						if announces_rockridge(part) {
							found = true;
							break;
						}
						area = part.to_vec();
					}
					found
				};
			}
		}
		Some(Iso9660 { dev, geo: Geometry { root_lba, root_len, blocks, joliet, rrip, susp_skip } })
	}

	// The volume's size in bytes - the space size the Primary Volume Descriptor
	// declares, for volume status reporting. Read-only media, so it is all in use.
	pub fn total_bytes(&self) -> u64 {
		self.geo.blocks as u64 * SECTOR_SIZE as u64
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
			// `IsDir`, which fs-core has for exactly this. `NotFound` said the name does not exist,
			// when what is true is that it names a directory - and a caller cannot tell those apart.
			return Err(FsError::IsDir);
		}
		// a multi-extent or interleaved file (segments in further records, or gap blocks
		// woven into the extent) is not assembled here - refuse it rather than serve a
		// truncated or gap-riddled read as the whole.
		if entry.unsupported {
			return Err(FsError::Invalid);
		}
		self.read_extent(entry.lba, entry.size)
	}

	// A RANGED read: fill `buffer` from byte `offset` of the file, answering how much was read.
	//
	// The size of a file stops being the size of an allocation. `read_file` is the whole-file shape
	// and keeps its ceiling, which is what makes "a hostile disc never exhausts the mounting
	// service" true TODAY; this is what makes it true without a ceiling at all, and it is the same
	// idea the directory half already proved - the caller says how much it is willing to take.
	//
	// A short answer means end of file, not an error: reading past the end returns 0.
	pub fn read_file_into(&mut self, path: &[u8], offset: u64, buffer: &mut [u8]) -> Result<usize, FsError> {
		let (parent, name) = split_parent(path)?;
		let (lba, len) = self.resolve_dir(parent)?;
		let entry = self.find_entry(lba, len, name)?;
		if entry.is_dir {
			return Err(FsError::IsDir);
		}
		if entry.unsupported {
			return Err(FsError::Invalid);
		}
		// The whole extent, before a byte of it is read - the same question `read_extent` asks, from
		// the same helper, so the two APIs cannot disagree about whether a record is valid.
		self.validate_extent(entry.lba, entry.size)?;
		if offset >= entry.size as u64 || buffer.is_empty() {
			return Ok(0);
		}
		let want = ((entry.size as u64 - offset) as usize).min(buffer.len());
		// The read is sector-aligned on the medium, so it starts at the sector holding `offset` and
		// the wanted bytes are copied out of it. One scratch sector rather than the whole file,
		// which is the entire point.
		let mut done = 0usize;
		let mut sector = [0u8; SECTOR_SIZE];
		while done < want {
			let at = offset + done as u64;
			let block = entry.lba as u64 + at / SECTOR_SIZE as u64;
			if block >= self.geo.blocks as u64 {
				return Err(FsError::Invalid);
			}
			let within = (at % SECTOR_SIZE as u64) as usize;
			let take = (SECTOR_SIZE - within).min(want - done);
			if !self.dev.read_block(block, &mut sector) {
				return Err(FsError::Io);
			}
			buffer[done..done + take].copy_from_slice(&sector[within..within + take]);
			done += take;
		}
		Ok(done)
	}

	// Walk path segments from the root, descending into each named subdirectory, and
	// return the final directory's extent. An empty path is the root.
	fn resolve_dir(&mut self, path: &[u8]) -> Result<(u32, u32), FsError> {
		let mut lba = self.geo.root_lba;
		let mut len = self.geo.root_len;
		for seg in path.split(|&b| b == b'/').filter(|s| !s.is_empty()) {
			let entry = self.find_entry(lba, len, seg)?;
			if !entry.is_dir {
				// `NotDir`, which is what fs-core has for this. Answering `NotFound` said the path
				// does not exist when what is true is that a component of it is a file - two
				// different repairs for the caller, reported as one.
				return Err(FsError::NotDir);
			}
			// ECMA-119 is STRICTER for directories than for files: they may not be interleaved and
			// are a single file section. `read_file` checked this flag and the descent did not, so
			// a directory record carrying either was walked as though it were ordinary.
			if entry.unsupported {
				return Err(FsError::Corrupt);
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
		let (joliet, rrip, skip) = (self.geo.joliet, self.geo.rrip, self.geo.susp_skip);
		let mut found = None;
		self.for_each_record(lba, len, |rec| {
			if let Some(e) = parse_record(rec, joliet, rrip, skip)?
				&& !e.name.is_empty()
				&& name_matches(&e, name)
			{
				found = Some(e);
				return Ok(false);
			}
			Ok(true)
		})?;
		found.ok_or(FsError::NotFound)
	}

	// Read every record in a directory extent into FileInfos, skipping the "." / ".."
	// self/parent entries.
	fn read_dir(&mut self, lba: u32, len: u32) -> Result<Vec<FileInfo>, FsError> {
		let (joliet, rrip, skip) = (self.geo.joliet, self.geo.rrip, self.geo.susp_skip);
		let mut out: Vec<FileInfo> = Vec::new();
		let mut failed = None;
		// A CEILING ON THE LISTING. The extent is no longer read whole - `for_each_record` walks it
		// a sector at a time - but the answer built from it still grows without a bound, one owned
		// `String` per entry, from a directory whose size the medium chose. That is the same
		// unbounded allocation one layer up, and a directory of a million entries is a legal
		// ISO9660 image.
		let mut listed = 0usize;
		self.for_each_record(lba, len, |rec| {
			if let Some(e) = parse_record(rec, joliet, rrip, skip)?
				&& !e.special
				&& !e.name.is_empty()
				// A multi-extent or interleaved file is refused by `read_file`, and listing its
				// FIRST section's size showed a number that is neither the file's nor anything
				// else's - for a file this backend will not read. It is left out of the listing
				// rather than described wrongly.
				&& !e.unsupported
				// records order equal names adjacently with versions descending, so a
				// multi-version file lists once, as its highest version.
				&& !out.last().is_some_and(|p: &FileInfo| p.name == e.name)
			{
				listed += 1;
				if listed > MAX_DIR_ENTRIES {
					return Err(FsError::TooLarge);
				}
				// a directory reports a length of zero - the FileInfo contract,
				// uniform across the backends behind the volume API.
				let info = FileInfo { name: e.name, size: if e.is_dir { 0 } else { e.size as u64 }, is_dir: e.is_dir };
				if out.try_reserve(1).is_err() {
					failed = Some(FsError::NoSpace);
					return Ok(false);
				}
				out.push(info);
			}
			Ok(true)
		})?;
		match failed {
			Some(error) => Err(error),
			None => Ok(out),
		}
	}

	// Walk a directory extent one SECTOR at a time, handing each record to `visit`.
	//
	// The whole extent used to be read into one `Vec` first, and `size` is a `u32` taken from the
	// medium - so a crafted disc made `ls` allocate close to four gigabytes before the caller had
	// opened anything. A sector at a time removes that entirely, and the boundary rule falls out of
	// the structure instead of being another check to remember: a record that starts in a sector
	// must END in it, which ECMA-119 requires and the old walk never tested.
	//
	// A record this cannot use is `Corrupt` rather than a `break`. Breaking made a damaged
	// directory indistinguishable from one that simply does not hold the name - `NotFound` from a
	// lookup, a short listing with an `Ok` from a read.
	//
	// `visit` returns Ok(false) to stop early.
	fn for_each_record<F>(&mut self, lba: u32, len: u32, mut visit: F) -> Result<(), FsError>
	where
		F: FnMut(&[u8]) -> Result<bool, FsError>,
	{
		if len == 0 {
			return Ok(());
		}
		let sectors = (len as u64).div_ceil(SECTOR_SIZE as u64);
		if lba as u64 + sectors > self.geo.blocks as u64 {
			return Err(FsError::Invalid);
		}
		let mut block = [0u8; SECTOR_SIZE];
		for index in 0..sectors {
			if !self.dev.read_block(lba as u64 + index, &mut block) {
				return Err(FsError::Io);
			}
			// The last sector may be partial: only the bytes the extent claims are records.
			let valid = ((len as u64 - index * SECTOR_SIZE as u64) as usize).min(SECTOR_SIZE);
			let mut off = 0usize;
			while off < valid {
				let rec_len = block[off] as usize;
				// Zero length: the rest of THIS sector is padding, which is how ECMA-119 says a
				// directory ends a sector early.
				if rec_len == 0 {
					break;
				}
				// A Directory Record is 33 bytes before its identifier, and it may not cross the
				// sector boundary.
				if rec_len < 33 || off + rec_len > valid {
					return Err(FsError::Corrupt);
				}
				if !visit(&block[off..off + rec_len])? {
					return Ok(());
				}
				off += rec_len;
			}
		}
		Ok(())
	}

	// Read `size` bytes starting at logical block `lba`, one block at a time. The extent
	// is the medium's own claim: one that would leave the volume is refused BEFORE the
	// buffer is allocated - a forged length can neither allocate without bound nor read
	// past the volume.
	// Does this extent lie inside the volume? ONE ANSWER, for every path that trusts one.
	//
	// `read_extent` had it and `read_file_into` did not: the ranged reader checked the block it was
	// about to read and nothing about the extent as a whole, so one record was invalid through one
	// API and partly valid through the other. A volume of blocks `0..22` with a file at LBA 22
	// declaring 4096 bytes needs 22 and 23; `read_file` refused it and
	// `read_file_into(path, 0, &mut [0u8; 1])` returned the first byte as success.
	//
	// The rule this reader states is that a record describing something outside the volume is
	// corrupt, decided once, before anything is read. Deciding it per block is a different rule.
	fn validate_extent(&self, lba: u32, size: u32) -> Result<(), FsError> {
		if lba as u64 + (size as u64).div_ceil(SECTOR_SIZE as u64) > self.geo.blocks as u64 {
			return Err(FsError::Invalid);
		}
		Ok(())
	}

	fn read_extent(&mut self, lba: u32, size: u32) -> Result<Vec<u8>, FsError> {
		self.validate_extent(lba, size)?;
		// A CEILING AND A FALLIBLE ALLOCATION.
		//
		// `size` comes off the medium, up to `u32::MAX`, and the bound above proves only that the
		// extent lies inside the declared volume - not that four gigabytes may be allocated to hold
		// it. `vec![0u8; n]` is infallible, so a disc asking for more than the service has did not
		// get an error: it took the service down, in a crate whose header says a hostile disc
		// "never crashes or exhausts the mounting service".
		//
		// The ceiling is what makes that sentence true today; a ranged read is what makes it true
		// without one, and that is the larger change recorded in the milestone.
		if size as usize > MAX_READ_BYTES {
			return Err(FsError::TooLarge);
		}
		let mut out: Vec<u8> = Vec::new();
		if out.try_reserve_exact(size as usize).is_err() {
			return Err(FsError::NoMemory);
		}
		out.resize(size as usize, 0);
		// The WHOLE contiguous run in one call. `fs-core` offers `read_blocks` precisely so a
		// contiguous extent is one device round trip, and this looped a block at a time - which
		// behind the IPC-backed block device the storage service hands over is a message per 2 KiB.
		let whole = out.len() / SECTOR_SIZE;
		if whole > 0 && !self.dev.read_blocks(lba as u64, whole as u64, &mut out[..whole * SECTOR_SIZE]) {
			return Err(FsError::Io);
		}
		// The tail, when the size is not a whole number of sectors: one block read into a scratch
		// sector, of which only the bytes the extent claims are kept.
		let done = whole * SECTOR_SIZE;
		if done < out.len() {
			let mut block = [0u8; SECTOR_SIZE];
			if !self.dev.read_block(lba as u64 + whole as u64, &mut block) {
				return Err(FsError::Io);
			}
			let n = out.len() - done;
			out[done..].copy_from_slice(&block[..n]);
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
	// Which namespace the name came from, because that decides how it compares. A Rock Ridge or
	// Joliet name is preserved-case and matches exactly; a bare ISO9660 identifier is upper-case by
	// construction and folds.
	case_sensitive: bool,
}

// Take a volume descriptor's root directory record (fixed 34 bytes at offset 156): its
// extent LBA and data length, both stored little-endian first. The root record can
// carry an XAR length too - its data follows those blocks, like any record's.
fn root_extent(desc: &[u8]) -> Option<(u32, u32)> {
	let r = &desc[156..156 + 34];
	// The Root Directory Record is an ordinary 34-byte Directory Record and was taken entirely on
	// trust: two little-endian numbers read out of it and nothing else checked. A malformed
	// descriptor could therefore nominate any region of the volume as the root, as long as the
	// extent survived the bounds check further on.
	//
	// Checked now: its own length, the directory flag, the single-byte identifier `0` that names
	// the root, and both halves of the extent and the size.
	if r[0] as usize != 34 || r[32] != 1 || r[33] != 0 {
		return None;
	}
	// AND THE ROOT'S OWN VOLUME SEQUENCE NUMBER. Every ordinary record is checked for it and the
	// root was not, so the one record that decides where the whole namespace begins was exempt from
	// the rule the header states.
	if both16(&r[28..32])? != 1 {
		return None;
	}
	// Bit 1 of the file flags is the directory bit; the root is a directory or it is not the root.
	if r[25] & 0x02 == 0 {
		return None;
	}
	let extent = both32(&r[2..10])?;
	let size = both32(&r[10..18])?;
	// The XAR skip is added to the extent; `saturating_add` invented a value the medium did not
	// give when the two overflowed, so a `checked_add` and a refusal.
	Some((extent.checked_add(r[1] as u32)?, size))
}

// A type-2 descriptor is Joliet when its escape sequences select UCS-2 (%/@, %/C, %/E) at the START
// of its escape field.
fn is_joliet(desc: &[u8]) -> bool {
	// The escape sequence is written at the START of the field, not somewhere inside it.
	//
	// This searched the whole 32-byte field with `windows(3)`, so any descriptor that happened to
	// contain those three bytes anywhere - in a publisher string, in padding a mastering tool left
	// behind - was read as Joliet, and its namespace was taken as the volume's. Joliet defines the
	// three sequences for its UCS-2 levels and writes one of them first.
	let esc = &desc[88..120];
	[b"%/@".as_slice(), b"%/C", b"%/E"].iter().any(|s| esc.starts_with(s))
}

// Parse a directory record: extent, length, dir flag, and the name (Joliet UCS-2, a Rock
// Ridge NM entry, or plain 8.3 with the version suffix stripped). None on a short record.
// `Ok(None)` for a record this reader legitimately ignores, `Ok(Some)` for a valid one, and
// `Err(Corrupt)` for metadata that does not hold together.
//
// It returned `Option`, and both callers wrote `if let Some(e)` - so every semantic failure inside a
// record was indistinguishable from "not a record" and the entry was SKIPPED. Both-endian halves
// disagreeing, the XAR skip overflowing, a Volume Sequence Number other than 1, a Joliet identifier
// of odd length or carrying a surrogate: each produced `None`, so `list()` returned a short listing
// and reported success, and `find_entry` answered `NotFound` for a file whose record was present and
// damaged. That is the original finding - a corrupt directory becoming a missing file - closed for
// the record's outer length and left open for everything inside it.
fn parse_record(rec: &[u8], joliet: bool, rrip: bool, skip: usize) -> Result<Option<Entry>, FsError> {
	// Shorter than a record header: not a record at all, which the sector walk already treats as
	// corruption before it gets here.
	if rec.len() < 33 {
		return Err(FsError::Corrupt);
	}
	// an Extended Attribute Record occupies rec[1] blocks at the extent's START - the
	// data follows it, and serving the XAR as content would be a silent misread (the
	// extent gate bounds the advanced LBA like any other).
	// Both halves of the extent and the length, and a CHECKED skip past the Extended Attribute
	// Record. `saturating_add` clamped to `u32::MAX`, which invents a value the medium never gave -
	// the honest answer from a parser of untrusted media is a refusal.
	let lba = both32(&rec[2..10]).and_then(|lba| lba.checked_add(rec[1] as u32)).ok_or(FsError::Corrupt)?;
	let size = both32(&rec[10..18]).ok_or(FsError::Corrupt)?;
	// The Volume Sequence Number names the volume this extent lives on. Multi-volume sets are not
	// supported here, and with one `BlockDevice` behind this reader an extent belonging to volume 2
	// would be read at that LBA on whichever disc is in the drive. Volume 1 or nothing.
	if both16(&rec[28..32]).ok_or(FsError::Corrupt)? != 1 {
		return Err(FsError::Corrupt);
	}
	let is_dir = rec[25] & 0x02 != 0;
	// an associated file (flag 0x04) is a secondary stream recorded BEFORE its
	// same-named main file - it must neither list (a duplicate name) nor match a
	// lookup (it would shadow the main content). IGNORED rather than refused: it is a
	// legal record this reader has no use for, which is what `Ok(None)` is for.
	if rec[25] & 0x04 != 0 {
		return Ok(None);
	}
	// multi-extent (segments in further records) and interleaving (gap blocks woven
	// into the extent) are forms the reader refuses rather than misreads.
	let unsupported = rec[25] & 0x80 != 0 || rec[26] != 0 || rec[27] != 0;
	let id_len = rec[32] as usize;
	// A zero-length identifier is a record with no name. It used to yield an empty name and fall out
	// through the skip path; for a parser whose stated threat model is hostile media that is a
	// malformed record, not one to pass over.
	if id_len == 0 || 33 + id_len > rec.len() {
		return Err(FsError::Corrupt);
	}
	let id = &rec[33..33 + id_len];
	let special = id_len == 1 && (id[0] == 0 || id[0] == 1);
	let (name, case_sensitive) = if special { (String::from(if id[0] == 0 { "." } else { ".." }), false) } else { decode_name(id, rec, id_len, joliet, rrip, skip).ok_or(FsError::Corrupt)? };
	Ok(Some(Entry { lba, size, is_dir, special, unsupported, name, case_sensitive }))
}

// Decode an entry name. Joliet is big-endian UCS-2; otherwise a Rock Ridge NM entry in
// the system-use area wins, falling back to plain ASCII 8.3 with ";version" dropped.
fn decode_name(id: &[u8], rec: &[u8], id_len: usize, joliet: bool, rrip: bool, skip: usize) -> Option<(String, bool)> {
	if joliet {
		// An odd length is corruption and an invalid unit is corruption, and neither may become a
		// NAME. `chunks_exact(2)` dropped a trailing odd byte and an unpaired surrogate became '?',
		// so two distinct damaged identifiers could collide on one name - and that name is the
		// lookup key.
		if id.len() % 2 != 0 {
			return None;
		}
		let mut s = String::new();
		for c in id.chunks_exact(2) {
			let u = u16::from_be_bytes([c[0], c[1]]);
			if u == b';' as u16 {
				break;
			}
			s.push(char::from_u32(u as u32)?);
		}
		return Some((s, true));
	}
	let sys_off = 33 + id_len + (id_len % 2 == 0) as usize;
	// a malformed record can end exactly after its identifier (the pad byte missing) -
	// there is no system-use area to read then, never a slice past the record.
	if let Some(sys) = rec.get(sys_off..)
		&& let Some(n) = rock_ridge_name(sys, rrip, skip)
	{
		return Some((n, true));
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
	Some((s, false))
}

// The Rock Ridge name in a System Use Area, when RRIP is actually in use and the entries this
// parser can follow are the only ones present.
//
// SUSP is what makes the System Use Area shareable: independent extensions live there side by side,
// and `SP` announces the area while `ER` says which extensions are in it. None of that was checked -
// every non-Joliet record's system-use bytes were scanned for `NM`, so a valid non-Rock-Ridge volume
// whose area happens to contain those two bytes had its filenames replaced by whatever followed.
//
// `skip` is SUSP's own offset into the area, from the root's `SP` entry.
fn rock_ridge_name(sys: &[u8], rrip: bool, skip: usize) -> Option<String> {
	if !rrip || skip > sys.len() {
		return None;
	}
	let sys = &sys[skip..];
	let mut off = 0usize;
	let mut out = String::new();
	while off + 4 <= sys.len() {
		let len = sys[off + 2] as usize;
		// A MALFORMED ENTRY IS NOT AN END, it is a reason to distrust everything after it - and
		// everything before it, because what has been accumulated so far is a PREFIX.
		//
		// This used to `break`, and the function then returned whatever `NM` fragments it had
		// collected. That is the truncated name the comment below refuses `CE` for, arriving by the
		// other door: a structurally broken area produced a name the medium does not carry, and it
		// became a lookup key.
		if len < 4 || off + len > sys.len() {
			return None;
		}
		let sig = &sys[off..off + 2];
		// FAIL CLOSED on what this parser cannot follow, rather than answering with a different
		// name than the medium records.
		//
		// `CE` continues an entry into another block, so an `NM` split across one yields its PREFIX
		// here - and two long names differing late become the same name. `CL`, `PL` and `RE` are how
		// Rock Ridge relocates deep directories, so ignoring them dismantles the structure they
		// exist to express. Falling back to the ISO9660 identifier is a name the medium really
		// carries; a truncated `NM` is not.
		if matches!(sig, b"CE" | b"CL" | b"PL" | b"RE") {
			return None;
		}
		// `ST` ends the system use area: past it the bytes are not SUSP entries at all, and walking
		// on into them is how an unrelated extension's data becomes a filename.
		if sig == b"ST" {
			break;
		}
		// an NM payload begins after sig, len, version, and flags - a shorter entry
		// carries no name and must not build an inverted range.
		if sig == b"NM" && len >= 5 {
			// The CONTINUE flag (bit 0) says this name runs into a further entry. Honouring only
			// the part in hand is the same truncation `CE` causes.
			if sys[off + 4] & 0x01 != 0 {
				return None;
			}
			// NON-UTF-8 IS A REFUSAL, not an empty fragment. `unwrap_or("")` contributed nothing
			// and left whatever the other fragments held, which is a name assembled out of the
			// parts that happened to decode - the same partial answer, one layer down.
			let Ok(fragment) = core::str::from_utf8(&sys[off + 5..off + len]) else {
				return None;
			};
			out.push_str(fragment);
		}
		off += len;
	}
	if out.is_empty() { None } else { Some(out) }
}

// Does this System Use Area open with a SUSP `SP` entry, and if so what offset does it declare?
//
// Read from the ROOT directory's own record, which is where SUSP puts it. Without this the Rock
// Ridge path was guesswork: `NM` was believed wherever it appeared.
fn susp_skip_of(sys: &[u8]) -> Option<usize> {
	// The VERSION too, which this read past - `SP` is version 1 and an entry claiming another is a
	// structure with the same two letters and different contents.
	if sys.len() >= 7 && &sys[0..2] == b"SP" && sys[2] as usize >= 7 && sys[3] == 1 && sys[4] == 0xBE && sys[5] == 0xEF {
		return Some(sys[6] as usize);
	}
	None
}

// Walk a System Use Area entry by entry, calling `f` with each `(signature, body)`.
//
// SUSP is what makes the area shareable: independent extensions live in it side by side, each as
// `sig(2) len version body`. Reading it any other way is guessing, and this reader was: `ER` was
// found by scanning the raw bytes for the letters, so anything that happened to contain them - a
// name, another extension's payload, padding - announced Rock Ridge.
//
// `ST` ends the area. A length below the four-byte header, or one that runs past the end, stops the
// walk rather than sliding: a malformed entry means nothing after it can be located.
fn for_each_susp(sys: &[u8], mut f: impl FnMut(&SuspEntry<'_>)) {
	let mut at = 0usize;
	while at + 4 <= sys.len() {
		let sig = [sys[at], sys[at + 1]];
		let len = sys[at + 2] as usize;
		if &sig == b"ST" {
			return;
		}
		if len < 4 || at + len > sys.len() {
			return;
		}
		// THE VERSION, which this handed out as part of a slice nobody looked at.
		//
		// Every SUSP entry carries one and every entry this reader implements is version 1. An entry
		// at some other version is a structure with the same signature and different contents, and
		// reading its body as though it were the version this code knows is how another extension's
		// bytes become a filename - the thing the `ST` check above exists to stop, one field in.
		let entry = SuspEntry { signature: sig, version: sys[at + 3], body: &sys[at + 4..at + len] };
		if entry.version != 1 {
			return;
		}
		f(&entry);
		at += len;
	}
}

// One SUSP entry, with the fields the format defines rather than a signature and a slice.
//
// The walker used to hand out `(&[u8; 2], &[u8])` - the signature and everything after the header -
// so the VERSION was skipped rather than validated, for `SP`, `ER`, `CE` and `NM` alike, and each
// consumer re-derived whatever else it needed from the body by hand.
struct SuspEntry<'a> {
	signature: [u8; 2],
	version: u8,
	body: &'a [u8],
}

// The `CE` continuation this area points at, as (block, offset within it, length).
//
// SUSP splits a system-use area across blocks when a directory record cannot hold it, which is the
// ordinary case rather than an exotic one: `SP`, `PX` and `TF` leave no room for an `ER` naming
// `IEEE_P1282`. The fields are both-endian pairs; the little-endian half is read, like every other
// both-endian field in this reader.
fn continuation_of(sys: &[u8]) -> Option<(u32, usize, usize)> {
	let mut found = None;
	for_each_susp(sys, |entry| {
		let body = entry.body;
		if &entry.signature != b"CE" || body.len() < 24 || found.is_some() {
			return;
		}
		// BOTH HALVES, like every other both-endian field in this reader - which is what the comment
		// above used to claim this did while reading one.
		//
		// `both32` refuses unless the little and big copies agree, and it is used at eight sites: the
		// volume block count, the logical block size, the volume set size, the root extent and size,
		// the root's sequence number, and an ordinary record's extent, size and sequence number.
		// Reading one half is what this reader does NOWHERE else.
		//
		// It matters more here than at most of them: the pointer decides where Rock Ridge is looked
		// for, and therefore what every file on the disc is called.
		let (Some(block), Some(offset), Some(len)) = (both32(&body[0..8]), both32(&body[8..16]), both32(&body[16..24])) else {
			return;
		};
		let (offset, len) = (offset as usize, len as usize);
		// A continuation has to fit in the block it names, or it is describing something else.
		if offset >= SECTOR_SIZE || len == 0 || offset + len > SECTOR_SIZE {
			return;
		}
		found = Some((block, offset, len));
	});
	found
}

// Does this System Use Area announce a Rock Ridge extension THIS READER IMPLEMENTS?
//
// `ER` is `LEN_ID, LEN_DES, LEN_SRC, EXT_VER` and then the identifier - and it is the IDENTIFIER
// that says which extension is present. None of that was read: the test was
// `sys.windows(2).any(|w| w == b"ER")`, which is satisfied by two bytes anywhere in the area. The
// fixture written to cover it declared `LEN_ID = 1` inside a total length of 8 - exactly the
// header, so the identifier it promised did not exist - and the parser turned Rock Ridge on for it.
fn announces_rockridge(sys: &[u8]) -> bool {
	let mut announced = false;
	for_each_susp(sys, |entry| {
		let body = entry.body;
		if &entry.signature != b"ER" || body.len() < 4 {
			return;
		}
		// EVERY DECLARED LENGTH, against the entry's own size.
		//
		// `ER` is `LEN_ID, LEN_DES, LEN_SRC, EXT_VER` and then the identifier, the description and
		// the source in that order. Only `LEN_ID` was read, so an `ER` carrying `RRIP_1991A` and
		// nonsense in every other field switched the whole disc's name source.
		let (id_len, des_len, src_len) = (body[0] as usize, body[1] as usize, body[2] as usize);
		let Some(declared) = id_len.checked_add(des_len).and_then(|n| n.checked_add(src_len)) else { return };
		if 4 + declared > body.len() + 4 || declared > body.len().saturating_sub(4) {
			return;
		}
		// The identifier has to BE there, not merely be declared.
		let Some(id) = body.get(4..4 + id_len) else { return };
		// The versions of Rock Ridge whose `NM`, `CE`, `CL`, `PL` and `RE` this reader understands.
		// An extension it does not implement must not switch the name source: reading somebody
		// else's `NM` is the guess this whole check exists to remove.
		if id == b"RRIP_1991A" || id == b"IEEE_P1282" || id == b"IEEE_1282" {
			announced = true;
		}
	});
	announced
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
// Compare a directory entry's name to a wanted one, by the rule of the namespace it came from.
//
// One ASCII-folding comparison used to be applied to everything, including Rock Ridge and Joliet
// names. For a bare `FOO.TXT;1` that is the right convenience: ISO9660 identifiers are upper-case by
// construction and callers type lower-case. Rock Ridge exists to express POSIX semantics, where
// `Makefile` and `makefile` are two files - and `find_entry` returns the first match, so the second
// was unreachable by its own name. Joliet is likewise a preserved-case namespace.
fn name_matches(entry: &Entry, want: &[u8]) -> bool {
	if entry.case_sensitive {
		return entry.name.as_bytes() == want;
	}
	entry.name.len() == want.len() && entry.name.bytes().zip(want).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

// A both-endian 32-bit field: little half, then big half, and they must AGREE.
//
// ECMA-119 records the volume space size, the root extent, and each file's extent and length twice,
// in one field, once per byte order. This reader took the little half of all of them and never
// looked at the other - so a volume claiming a little-endian length of 1000 and a big-endian length
// of 0xFFFFFFFF is internally contradictory and was accepted. Comparing them is the cheapest
// structural check this format offers, and it is the one the format was designed for.
fn both32(b: &[u8]) -> Option<u32> {
	let little = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
	let big = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
	(little == big).then_some(little)
}

// The 16-bit counterpart: little half, big half, and they must agree.
fn both16(b: &[u8]) -> Option<u16> {
	let little = u16::from_le_bytes([b[0], b[1]]);
	let big = u16::from_be_bytes([b[2], b[3]]);
	(little == big).then_some(little)
}
