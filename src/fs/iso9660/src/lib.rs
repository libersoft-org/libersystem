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

// THE WORK A DIRECTORY MAY COST, which nothing bounded.
//
// The previous round bounded how much MEMORY a hostile disc can make this backend spend - the walk
// reads a sector at a time instead of allocating the extent - and nothing bounded how much WORK.
// `for_each_record` checks only that `lba + sectors` fits the medium, and `MAX_DIR_ENTRIES` counts
// entries LISTED: a directory declaring a near-4 GiB extent whose every sector opens with a zero
// byte yields no entries at all, never reaches the entry limit, and drives two million synchronous
// `read_block` calls while holding the StorageService request. `find_entry` has the same shape for
// a name that is not there.
//
// 16 MiB is 8192 sectors. A directory that large is far past anything a real disc carries -
// `MAX_DIR_ENTRIES` at 65536 entries of the 34-byte minimum is about 2 MiB - so this bounds the
// hostile case and not the ordinary one.
//
// BATCHING WOULD NOT HAVE ANSWERED IT. Reading many sectors per device call cuts the IPC cost and
// leaves the work, which is the thing that holds the request; the bound has to be on the extent.
const MAX_DIR_BYTES: u64 = 16 * 1024 * 1024;

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

// WHY A MEDIUM DID NOT MOUNT, which `Option` could not say.
//
// The same four answers UDF's `MountError` carries, and for the same reason: a probe deciding which
// backend to hand a device to has to tell "somebody else's medium" from "this device did not
// answer", and StorageService turns the second into `Again` and the first into a refusal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MountError {
	// No `CD001` where the format puts it: a blank disc, or one belonging to something else.
	NotIso,
	// ISO9660, and using something this reader does not implement - a logical block size it does
	// not read in, a descriptor form it has no code for.
	Unsupported,
	// ISO9660, and its own structures failed its own checks.
	Corrupt,
	// The device did not answer. Says nothing about what is on it.
	Io,
	// This machine could not get the memory the mount needed. NOT a statement about the medium, and
	// the same distinction `FsError` draws: blaming a healthy disc sends a person to replace media
	// that is fine, and makes a transient shortage look permanent.
	NoMemory,
}

impl<D: BlockDevice> Iso9660<D> {
	// The block device this filesystem reads through.
	pub fn device(&self) -> &D {
		&self.dev
	}
	// Mount optical media: scan the volume descriptors for a Primary (and a preferred
	// Joliet) descriptor and take its root directory record. None if no PVD is found.
	// The convenience wrapper. `mount_checked` is where the reasons are.
	pub fn mount(dev: D) -> Option<Iso9660<D>> {
		Self::mount_checked(dev).ok()
	}

	// WHY a medium is not mountable, not merely that it is not.
	//
	// This answered `Option`, so "not an ISO", a bad `CD001`, a both-endian field whose halves
	// disagree, a block size this reader does not implement, an I/O failure reading a descriptor and
	// a missing terminator were all `None` - and a probe deciding which backend to hand a device to
	// could not tell "this is somebody else's medium" from "this device did not answer". UDF next
	// door has carried `MountError` since the round that introduced it, for exactly that reason, and
	// StorageService depends on the distinction to choose between `Again` and a refusal.
	pub fn mount_checked(mut dev: D) -> Result<Iso9660<D>, MountError> {
		let mut pvd_root: Option<((u32, u32), u32, u32)> = None;
		// The chosen Joliet hierarchy and the LEVEL it was chosen for, so a later descriptor of a
		// lower level cannot displace it and a later one of the same level does not either.
		let mut joliet_root: Option<((u32, u32), u32, u32)> = None;
		let mut joliet_at: u8 = 0;
		let mut block = [0u8; SECTOR_SIZE];
		let mut terminated = false;
		// The Joliet descriptor's own copies of the fields ECMA-119 requires it to share with the
		// primary: volume space size, logical block size, volume set size, volume sequence number.
		let mut joliet_common: Option<(u32, u32, u16, u16)> = None;
		// Whether the medium has identified itself as ISO9660 - see the refusal below.
		let mut recognised = false;
		let mut terminator_lba: u64 = 0;
		for i in 0..32 {
			// THE DEVICE, not the medium. An unreadable descriptor sector answered the same way a
			// blank disc does, so a probe could not tell them apart.
			if !dev.read_block(FIRST_DESCRIPTOR_LBA + i, &mut block) {
				return Err(MountError::Io);
			}
			if &block[1..6] != b"CD001" {
				// NOT ISO ONLY UNTIL THE MEDIUM HAS SAID OTHERWISE. The first descriptor deciding
				// this is a genuine "try another backend"; a later one failing it is an ISO whose
				// descriptor set is damaged, and answering `NotIso` there tells a probe to move on
				// from a medium that has already identified itself.
				return Err(if recognised { MountError::Corrupt } else { MountError::NotIso });
			}
			recognised = true;
			// THE VERSION UNDER THE TYPE, not above it.
			//
			// This refused anything whose version byte was not 1, under a comment saying version "is
			// 1 for every descriptor this format defines" - which ECMA-119's second edition makes
			// false: the Enhanced Volume Descriptor is type 2 at version 2. A well-formed set of
			// `PVD (v1)`, `Enhanced VD (v2)`, `Terminator` was refused at the second descriptor,
			// rather than the primary hierarchy this reader fully understands being read.
			//
			// Skipping something unimplemented is not the same as refusing the volume.
			//
			// The TYPE first for the other reason too: a terminator carries none of the fields below,
			// so reading them out of it and requiring them to be well-formed would refuse the very
			// descriptor that says the set is complete.
			if block[0] == 255 {
				if block[6] != 1 {
					return Err(MountError::Corrupt);
				}
				terminated = true;
				// WHERE THE SET ENDS, kept so the declared volume size can be checked against it.
				// A volume that does not contain its own descriptors is describing a medium it is
				// not on - see the geometry check below.
				terminator_lba = FIRST_DESCRIPTOR_LBA + i;
				break;
			}
			if block[0] != 1 && block[0] != 2 {
				continue;
			}
			// An Enhanced Volume Descriptor: type 2, version 2. Not implemented here, and not a
			// reason to refuse the medium.
			if block[0] == 2 && block[6] == 2 {
				continue;
			}
			if block[6] != 1 {
				return Err(MountError::Corrupt);
			}
			// Both halves of the volume space size and of the logical block size, which were taken
			// from their little ends alone.
			let Some(blocks) = both32(&block[80..88]) else { return Err(MountError::Corrupt) };
			let Some(block_size) = both16(&block[128..132]) else { return Err(MountError::Corrupt) };
			let Some(root) = root_extent(&block) else { return Err(MountError::Corrupt) };
			let found = (root, blocks, block_size as u32);
			match block[0] {
				1 => {
					// THE VOLUME SET, at the descriptor. The header states that multi-volume sets
					// are outside this reader's subset and refused, and one line enforced it: a
					// per-RECORD sequence number check, halfway through a listing. A set larger
					// than one is refused here, which is where a reader that does not implement
					// multi-volume should say so - at mount, before anything is read.
					let Some(set_size) = both16(&block[120..124]) else { return Err(MountError::Corrupt) };
					// EXACTLY one. `> 1` admitted zero, which is not a legal set size - a volume
					// belongs to a set of at least itself.
					//
					// AND THE TWO ANSWERS ARE DIFFERENT ANSWERS. A set size of zero is a volume
					// disobeying its own rules, which is `Corrupt`; a set size ABOVE one is a
					// perfectly valid ISO using a construct this backend does not implement, which
					// is what `Unsupported` means and what this error type deliberately
					// distinguishes. Returning `Corrupt` for a legal disc tells an operator their
					// medium is damaged when it is not.
					if set_size == 0 {
						return Err(MountError::Corrupt);
					}
					if set_size > 1 {
						return Err(MountError::Unsupported);
					}
					// And this volume's own sequence number within that set. The root record's and
					// every ordinary record's were checked and the DESCRIPTOR's was not, which is
					// the one that says which volume of the set this is.
					let Some(sequence) = both16(&block[124..128]) else { return Err(MountError::Corrupt) };
					if sequence != 1 {
						return Err(MountError::Corrupt);
					}
					// THE FIRST PVD IS CANONICAL, AND EVERY LATER ONE MUST AGREE.
					//
					// The standard permits a Primary Volume Descriptor to be recorded more than
					// once, and every type-1 descriptor overwrote the previous one - so a disc
					// could carry a benign PVD followed by a hostile one and be parsed by the
					// second. Which is exactly the storage accident the Joliet arm below had
					// already been fixed for, in the descriptor that decides the whole volume.
					match pvd_root {
						Some(previous) if previous != found => return Err(MountError::Corrupt),
						Some(_) => {}
						None => pvd_root = Some(found),
					}
				}
				// AND ITS COMMON FIELDS HAVE TO AGREE WITH THE PRIMARY'S.
				//
				// The `1 =>` arm validates the volume set size and sequence number; this one took
				// the Joliet root and the geometry and checked neither - and when Joliet is present
				// that geometry BECOMES the filesystem's. ECMA-119 requires a Supplementary
				// descriptor's common fields to match the primary's within one set, so a crafted
				// medium could declare one volume space in the PVD and another here and have the
				// second used without the two ever being compared. What an SVD is allowed to differ
				// in is the root directory and the hierarchy under it.
				2 if joliet_level(&block).is_some() => {
					let Some(set_size) = both16(&block[120..124]) else { return Err(MountError::Corrupt) };
					let Some(sequence) = both16(&block[124..128]) else { return Err(MountError::Corrupt) };
					// EVERY RECOGNISED JOLIET DESCRIPTOR IS CHECKED, not only the last one. These
					// were plain `Option`s, so a second Joliet SVD overwrote the first and the
					// comparison after the scan saw only whichever came last - a volume could carry
					// a contradictory descriptor and a conforming one and mount as though the
					// contradictory one were not there. ECMA-119 permits zero or more supplementary
					// descriptors, so "the last one" is not a rule, it was an accident of storage.
					if let Some(previous) = joliet_common
						&& previous != (blocks, block_size as u32, set_size, sequence)
					{
						return Err(MountError::Corrupt);
					}
					joliet_common = Some((blocks, block_size as u32, set_size, sequence));
					// THE HIGHEST LEVEL, AND THE FIRST OCCURRENCE WITHIN IT - stated rather than
					// emergent. Every Joliet SVD used to overwrite the chosen root, so a medium
					// carrying Level 3 and then Level 1 was read at Level 1 and which hierarchy a
					// disc got depended on descriptor order.
					let level = joliet_level(&block).unwrap_or(0);
					if joliet_root.is_none() || level > joliet_at {
						joliet_at = level;
						joliet_root = Some(found);
					}
				}
				_ => {}
			}
		}
		// The Volume Descriptor Set Terminator is REQUIRED. Nothing recorded whether one was ever
		// seen, so a set that simply stopped - or ran past the thirty-second sector this scan
		// bounds itself with - mounted anyway. The limit is a good one and reaching it is now a
		// refusal rather than a silent truncation of the search.
		if !terminated {
			return Err(MountError::Corrupt);
		}
		// A Joliet SVD supplies the namespace, and only ON TOP of a valid Primary Volume Descriptor.
		// The match answered `(Some(joliet), _)`, so a recognised Joliet descriptor alone was enough
		// and `pvd_root` being None was not an obstacle - which ECMA-119 does not allow.
		let Some(primary) = pvd_root else { return Err(MountError::Corrupt) };
		// The primary's geometry is canonical, and the Joliet descriptor is required to agree with
		// it field by field - so a mismatch is a refusal rather than a silent change of which
		// numbers the filesystem runs on. Per field, because one combined comparison would pass on
		// whichever of the four somebody happened to check.
		if let Some((blocks, block_size, set_size, sequence)) = joliet_common {
			let ((_, _), primary_blocks, primary_block_size) = primary;
			if blocks != primary_blocks || block_size != primary_block_size || set_size != 1 || sequence != 1 {
				return Err(MountError::Corrupt);
			}
		}
		let (joliet, ((root_lba, root_len), blocks, block_size)) = match joliet_root {
			Some(r) => (true, r),
			None => (false, primary),
		};
		// the logical block size is 2048 on real media and the unit this backend reads
		// in - any other legal size would be read at wrong positions, so it refuses -
		// and the root extent must fit the volume's own block count.
		if block_size != SECTOR_SIZE as u32 {
			// A legal size this reader does not read in: it would read every extent at the wrong
			// position. Not the medium's fault.
			return Err(MountError::Unsupported);
		}
		if blocks == 0 || root_lba as u64 + (root_len as u64).div_ceil(SECTOR_SIZE as u64) > blocks as u64 {
			return Err(MountError::Corrupt);
		}
		// THE DECLARED VOLUME MUST CONTAIN THE DESCRIPTORS IT WAS READ FROM. Only `blocks != 0` and
		// the root's fit were checked, so an image could place a valid descriptor set at LBA 16 and
		// upwards while claiming `VolumeSpaceSize = 1` - geometry contradicting the very bytes it
		// was built from, mounted as healthy. That hides truncation at mount instead of at the
		// first read that runs off the end.
		if (blocks as u64) <= terminator_lba {
			return Err(MountError::Corrupt);
		}
		// AND THE ROOT MUST BE A DIRECTORY THAT COULD HOLD ITS OWN MANDATORY RECORDS. `root_extent`
		// accepted a data length of zero, so a medium with no root directory at all mounted and
		// listed as a healthy empty volume - which a caller cannot tell from a genuinely empty one.
		// The two self/parent records are 34 bytes each; anything shorter is not a directory.
		if root_len < 68 {
			return Err(MountError::Corrupt);
		}
		// the block count is the medium's own claim: its last block must exist on the
		// device, or a forged or truncated image mounts and only fails - or allocates
		// without bound - inside a later read. The real media size then bounds every
		// extent read.
		if !dev.read_block(blocks as u64 - 1, &mut block) {
			return Err(MountError::Io);
		}
		// SUSP, established once from the root directory's own first record - which is where the
		// standard puts `SP`. Joliet volumes do not use Rock Ridge names, so the question is only
		// asked for the ISO9660 namespace.
		let mut rrip = false;
		let mut susp_skip = 0usize;
		// THE PROBE'S READ IS PART OF THE MOUNT, so a device that will not answer it is an I/O
		// failure and not a volume without Rock Ridge. This was `&& dev.read_block(...)`, so a
		// failed read made the whole condition false and the mount SUCCEEDED with `rrip = false` -
		// which is the one answer that cannot be distinguished from the truth by anything
		// downstream. The distinction `MountError::Io` was added for stopped one line short of the
		// place it mattered most.
		if !joliet && !dev.read_block(root_lba as u64, &mut block) {
			return Err(MountError::Io);
		}
		if !joliet && block[0] as usize >= 34 {
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
				// THREE ENDINGS, NOT ONE. A failed `read_block`, an out-of-range LBA and a genuine
				// end of chain shared a single `break`, so a disc read error, a malformed pointer
				// and "there is no continuation" all produced the same silent fall back to the 8.3
				// namespace - a medium that fails to read halfway through its Rock Ridge data
				// mounted with different file names than it has, and said nothing.
				//
				// `Io` propagates, a range error refuses the volume, and the chain ends only when
				// it actually ends.
				rrip = if announces_rockridge(&sys) {
					true
				} else {
					let mut found = false;
					let mut area = sys.clone();
					// EVERY CONTINUATION VISITED, so a cycle is refused rather than merely exhausting
					// the iteration cap - reaching the cap and reaching the end of the chain used to
					// be the same observation, and both produced a quietly non-Rock-Ridge mount.
					let mut visited: alloc::vec::Vec<(u32, usize, usize)> = alloc::vec::Vec::new();
					// Bounded: SUSP allows a chain, and a disc may name one that loops.
					for _ in 0..4 {
						// The chain ENDING is the one case that is not an error; a malformed area or
						// a malformed `CE` is now told apart from it.
						let next = match continuation_of(&area) {
							Ok(Some(next)) => next,
							Ok(None) => break,
							Err(()) => return Err(MountError::Corrupt),
						};
						if visited.contains(&next) {
							return Err(MountError::Corrupt);
						}
						if visited.try_reserve(1).is_err() {
							return Err(MountError::NoMemory);
						}
						visited.push(next);
						let (next_lba, offset, len) = next;
						// A pointer outside the medium is the disc contradicting itself, not the end
						// of anything.
						if next_lba as u64 >= blocks as u64 {
							return Err(MountError::Corrupt);
						}
						let mut cont = [0u8; SECTOR_SIZE];
						// And the device not answering is the device.
						if !dev.read_block(next_lba as u64, &mut cont) {
							return Err(MountError::Io);
						}
						let Some(part) = cont.get(offset..offset.saturating_add(len)) else {
							return Err(MountError::Corrupt);
						};
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
		Ok(Iso9660 { dev, geo: Geometry { root_lba, root_len, blocks, joliet, rrip, susp_skip } })
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
		// BATCHED: head sector, then the aligned middle in ONE `read_blocks`, then the tail.
		//
		// This looped `read_block` per 2048 bytes, and `IsoBlockDevice::read_blocks` exists because
		// that loop is one IPC round trip per sector behind the block device the storage service
		// hands over - a 64 MiB window was thirty-two thousand messages.
		let mut done = 0usize;
		let mut sector = [0u8; SECTOR_SIZE];
		let bound = |block: u64, count: u64| -> Result<(), FsError> { if block.checked_add(count).is_none_or(|end| end > self.geo.blocks as u64) { Err(FsError::Invalid) } else { Ok(()) } };
		// The head, when the window does not start on a sector boundary.
		let within = (offset % SECTOR_SIZE as u64) as usize;
		if within != 0 {
			let block = entry.lba as u64 + offset / SECTOR_SIZE as u64;
			bound(block, 1)?;
			if !self.dev.read_block(block, &mut sector) {
				return Err(FsError::Io);
			}
			let take = (SECTOR_SIZE - within).min(want);
			buffer[..take].copy_from_slice(&sector[within..within + take]);
			done = take;
		}
		// The aligned middle, whole sectors, in one call.
		let whole = (want - done) / SECTOR_SIZE;
		if whole > 0 {
			let block = entry.lba as u64 + (offset + done as u64) / SECTOR_SIZE as u64;
			bound(block, whole as u64)?;
			if !self.dev.read_blocks(block, whole as u64, &mut buffer[done..done + whole * SECTOR_SIZE]) {
				return Err(FsError::Io);
			}
			done += whole * SECTOR_SIZE;
		}
		// The tail, when the window does not end on one.
		if done < want {
			let block = entry.lba as u64 + (offset + done as u64) / SECTOR_SIZE as u64;
			bound(block, 1)?;
			if !self.dev.read_block(block, &mut sector) {
				return Err(FsError::Io);
			}
			let take = want - done;
			buffer[done..done + take].copy_from_slice(&sector[..take]);
			done += take;
		}
		Ok(done)
	}

	// Walk path segments from the root, descending into each named subdirectory, and
	// return the final directory's extent. An empty path is the root.
	fn resolve_dir(&mut self, path: &[u8]) -> Result<(u32, u32), FsError> {
		let mut lba = self.geo.root_lba;
		let mut len = self.geo.root_len;
		// EMPTY SEGMENTS, `.` AND `..` ARE REFUSED HERE, which `fs-core` documents as `BadName` and
		// this backend used to admit. `find_entry`'s own comment claimed the special records
		// "resolve the way the other backends behind the volume API resolve them" - and that was
		// factually wrong: the writable backends refuse all three, and so does `rt::RelativePath`,
		// which every `vol://` path is parsed by before a backend sees it.
		//
		// So nothing governed could ever deliver these spellings, and admitting them only kept this
		// backend disagreeing with the shared contract about what a path IS. The self and parent
		// records are still parsed - `check_hierarchy` requires them - they are simply not exposed
		// as traversal components.
		for seg in path.split(|&b| b == b'/') {
			// EMPTY SEGMENTS ARE STILL TOLERATED, and only at the edges in practice, because the
			// leading-slash form is what this crate's own fixtures and callers use throughout -
			// tightening it turned ten tests red for spelling rather than for behaviour. That is
			// the same call recorded in `libermemfs`: the boundary has already decided, so this is
			// unreachable rather than wrong.
			if seg.is_empty() {
				continue;
			}
			if seg == b"." || seg == b".." {
				return Err(FsError::BadName);
			}
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
	//
	// The `.` / `..` self and parent records are STRUCTURE, not path syntax. This comment used to
	// say paths through them "resolve the way the other backends behind the volume API resolve
	// them", and that was factually wrong in both directions: `fs-core` documents `.` and `..` as
	// `BadName`, the writable backends refuse them, and `rt::RelativePath` refuses them before any
	// backend sees a `vol://` path. `resolve_dir` now refuses them here too, and `check_hierarchy`
	// requires the records themselves to be present and well formed.
	// ECMA-119'S HIERARCHY RULE, WHICH NEITHER WALKER ENFORCED.
	//
	// A directory's first two records are its SELF and PARENT records, in that order, both marked
	// as directories. `parse_record` recognised the one-byte identifiers 0 and 1 and named them `.`
	// and `..`, and nothing ever checked that they were there, that they came first, that they came
	// in that order, or that they were directories - so a missing, reordered, duplicated or
	// file-flagged pair mounted and listed as a healthy directory, and the parent relation the
	// format guarantees was whatever the medium said.
	//
	// One pass, shared by listing and lookup, so the two cannot disagree about whether a directory
	// is well formed - which is the same reason `find_entry` now walks the whole extent.
	fn check_hierarchy(&mut self, lba: u32, len: u32) -> Result<(), FsError> {
		let (joliet, rrip, skip) = (self.geo.joliet, self.geo.rrip, self.geo.susp_skip);
		let mut seen = 0usize;
		let mut wrong = false;
		self.for_each_record(lba, len, |rec| {
			let Some(e) = parse_record(rec, joliet, rrip, skip)? else { return Ok(true) };
			match seen {
				// The self record: identifier 0, and a directory.
				//
				// NOT its extent. A record's `lba` is where the extent STARTS, and an extended
				// attribute record sits in front of the data - so on a root carrying one, the self
				// record names a block before the one this walk is reading, correctly. Checking
				// them equal refused `a_root_extended_attribute_record_is_skipped`, which is a legal
				// image. Making it XAR-aware means carrying the attribute length into the walk, and
				// what ISO-004 is actually about is the records being PRESENT, in order, and of the
				// right kind.
				0 => wrong |= !e.special || e.name != "." || !e.is_dir,
				// The parent record: identifier 1, a directory. Its extent is the parent's, which
				// this walk does not know - the descent would have to carry it - so what is checked
				// here is its identity and kind.
				1 => wrong |= !e.special || e.name != ".." || !e.is_dir,
				// And no third special record: a duplicate `.` or `..` later in the extent is a
				// second claim about the same relation.
				_ => wrong |= e.special,
			}
			seen += 1;
			Ok(!wrong)
		})?;
		if wrong || seen < 2 { Err(FsError::Corrupt) } else { Ok(()) }
	}

	// THE WHOLE DIRECTORY IS WALKED, EVEN AFTER A MATCH.
	//
	// This stopped at the first record whose name matched, so it never looked at what came after:
	// a SECOND record resolving to the same name went unseen, and a structurally corrupt record
	// later in the extent went unseen too. That made lookup and listing disagree - `read_dir` scans
	// the whole extent and refuses a duplicate or a bad record, so the same directory could be
	// valid to `open` and corrupt to `list` - and it made WHICH object a name reaches depend on
	// record order, on a medium that install images are loaded from.
	//
	// A second match is a collision - EXCEPT the one legitimate repeat, which is a primary-namespace
	// version record. `F;2` and `F;1` both decode to `F`, and ECMA-119 orders them ADJACENTLY with
	// versions descending, so the first match is the highest version and an immediately following
	// one is the same file. That is the identical rule `read_dir` applies, and it is applied the
	// identical way here so lookup and listing cannot disagree: the coalescing is for the primary
	// namespace only, because Joliet and Rock Ridge have no version suffixes and two records
	// decoding to one name there are two objects claiming one name.
	fn find_entry(&mut self, lba: u32, len: u32, name: &[u8]) -> Result<Entry, FsError> {
		self.check_hierarchy(lba, len)?;
		let (joliet, rrip, skip) = (self.geo.joliet, self.geo.rrip, self.geo.susp_skip);
		let versioned = !joliet && !rrip;
		let mut found: Option<Entry> = None;
		let mut previous_matched = false;
		let mut duplicate = false;
		self.for_each_record(lba, len, |rec| {
			let matched = if let Some(e) = parse_record(rec, joliet, rrip, skip)?
				&& !e.name.is_empty()
				&& name_matches(&e, name)
			{
				match &found {
					// A later version of the file already found, immediately behind it.
					Some(_) if versioned && previous_matched => {}
					Some(_) => duplicate = true,
					None => found = Some(e),
				}
				true
			} else {
				false
			};
			previous_matched = matched;
			Ok(!duplicate)
		})?;
		if duplicate {
			return Err(FsError::Corrupt);
		}
		found.ok_or(FsError::NotFound)
	}

	// Read every record in a directory extent into FileInfos, skipping the "." / ".."
	// self/parent entries.
	fn read_dir(&mut self, lba: u32, len: u32) -> Result<Vec<FileInfo>, FsError> {
		self.check_hierarchy(lba, len)?;
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
				//
				// THE PRIMARY NAMESPACE ONLY. This ran for Joliet and Rock Ridge too, where two
				// adjacent records decoding to one name are not a version pair - there are no
				// version suffixes in either - but a NAMESPACE COLLISION. Suppressing the second
				// discarded it before the whole-listing duplicate check below could see it, so the
				// one thing that would have reported the collision never got the chance: the
				// listing showed one entry and `open` reached whichever came first.
				&& !(!joliet && !rrip && out.last().is_some_and(|p: &FileInfo| p.name == e.name))
			{
				listed += 1;
				if listed > MAX_DIR_ENTRIES {
					return Err(FsError::TooLarge);
				}
				// a directory reports a length of zero - the FileInfo contract,
				// uniform across the backends behind the volume API.
				let info = FileInfo { name: e.name, size: if e.is_dir { 0 } else { e.size as u64 }, is_dir: e.is_dir };
				if out.try_reserve(1).is_err() {
					failed = Some(FsError::NoMemory);
					return Ok(false);
				}
				out.push(info);
			}
			Ok(true)
		})?;
		if let Some(error) = failed {
			return Err(error);
		}
		// AND NO NAME APPEARS TWICE, checked over the WHOLE listing.
		//
		// The de-duplication above compares against `out.last()` alone, which is right for ISO
		// version records - their identifiers are ordered, so equal names are adjacent - and wrong
		// for Rock Ridge: records ordered by ISO identifier can carry the same `NM` in non-adjacent
		// positions, so a listing showed two entries called `same` and `open("same")` could only
		// ever reach the first. One of the two was unreachable by the name it was listed under,
		// which is the namespace being ambiguous rather than merely untidy.
		//
		// Sorted rather than kept in a set: the listing is already in hand and bounded by
		// `MAX_DIR_ENTRIES`, and a sort plus a neighbour scan needs no second allocation, which
		// matters on a path whose whole point is that a hostile disc cannot make it spend.
		let mut names: Vec<&str> = Vec::new();
		if names.try_reserve_exact(out.len()).is_err() {
			return Err(FsError::NoMemory);
		}
		names.extend(out.iter().map(|entry| entry.name.as_str()));
		names.sort_unstable();
		if names.windows(2).any(|pair| pair[0] == pair[1]) {
			return Err(FsError::Corrupt);
		}
		Ok(out)
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
		// The extent's own size, before a single sector is read. `TooLarge` rather than `Corrupt`:
		// the directory may be perfectly well formed and this backend will not walk something that
		// size in one request, which is what that error means everywhere else in this crate.
		if len as u64 > MAX_DIR_BYTES {
			return Err(FsError::TooLarge);
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
				//
				// AND PADDING IS ZEROES, WHICH WAS NOT CHECKED. One zero byte ended the sector and
				// whatever followed was never looked at, so a sector holding a zero and then
				// arbitrary bytes was treated as canonical padding: localised corruption hidden, and
				// a short directory reported as a complete one. Nothing was parsed out of those
				// bytes, so this is integrity rather than memory safety - but "the directory ended
				// here" is a claim, and it is only true if the rest really is padding.
				if rec_len == 0 {
					if block[off..valid].iter().any(|&b| b != 0) {
						return Err(FsError::Corrupt);
					}
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
// The shape a DIRECTORY record must have, whether it is the root or an ordinary entry.
//
// ECMA-119 says a directory is not an associated file, is not interleaved and has a single file
// section. `parse_record` folded the last two into a general `unsupported` flag and `read_dir`
// filtered those entries out - which is a defensible product decision for a regular FILE this
// backend will not read, and for a DIRECTORY it is a short listing presented as a complete one. The
// associated-file case was worse: the record returned `Ok(None)` regardless of `is_dir`, so an
// associated DIRECTORY was ignored as though it were a legitimate associated file.
//
// `root_extent` checked the directory bit, the identifier, the volume sequence, the endian copies
// and the XAR, and none of these three - so the one record that decides where the whole namespace
// begins was exempt from the rules its children are held to. One function, both callers.
fn validate_directory_shape(flags: u8, file_unit: u8, gap: u8) -> Result<(), FsError> {
	// Bit 2 is associated, bit 7 is multi-extent; a non-zero file unit size or interleave gap is
	// interleaving. None of the three may appear on a directory.
	if flags & 0x04 != 0 || flags & 0x80 != 0 || file_unit != 0 || gap != 0 {
		return Err(FsError::Corrupt);
	}
	Ok(())
}

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
	// And the shape every directory must have - the root included, which it was not.
	if validate_directory_shape(r[25], r[26], r[27]).is_err() {
		return None;
	}
	let extent = both32(&r[2..10])?;
	let size = both32(&r[10..18])?;
	// The XAR skip is added to the extent; `saturating_add` invented a value the medium did not
	// give when the two overflowed, so a `checked_add` and a refusal.
	Some((extent.checked_add(r[1] as u32)?, size))
}

// WHICH Joliet level a type-2 descriptor selects, or `None` if it selects none.
//
// It answered a bool and each Joliet SVD overwrote the chosen root, so a medium carrying Level 3 and
// then Level 1 was read at Level 1 - a policy that was emergent rather than stated. The level
// travels now and the caller picks the highest, with the first occurrence winning within a level.
//
// THE FIELD IS READ, AND IT MAY HOLD MORE THAN ONE SEQUENCE.
//
// Two mistakes have been made here in opposite directions. It first searched the field with
// `windows(3)`, so any descriptor happening to contain those bytes anywhere - in a publisher
// string, in a tool's leftover padding - was read as Joliet and its namespace taken as the volume's.
// The fix required the field to be one Joliet sequence and then nothing but zeros, which is too
// strict the other way: ECMA-119 says the field holds ONE OR MORE escape sequences, packed without
// gaps, with only the unused remainder zero.
//
// AND THE SEQUENCE GRAMMAR WAS WRONG IN BOTH DIRECTIONS TOO. It required each sequence to begin
// `0x1b` or `%`, and ECMA-119 OMITS the escape character when writing this field - so an explicit
// `ESC` is malformed and was being accepted, while a perfectly legal accompanying sequence that
// does not happen to start with `%` was refused, taking the whole field with it. The comment
// claimed any other well-formed sequence was skipped and the code only skipped `%`-led ones.
//
// The grammar, from ECMA-35 with the escape omitted: zero or more intermediate bytes in
// `0x20..=0x2F`, then one final byte in `0x30..=0x7E`. Joliet's three fall out of it - `%` and `/`
// are intermediates and the level is the final byte - so they are compared as whole slices rather
// than assumed to be three bytes beginning with a particular one.
fn joliet_level(desc: &[u8]) -> Option<u8> {
	let esc = &desc[88..120];
	let mut at = 0usize;
	let mut level: Option<u8> = None;
	while at < esc.len() {
		if esc[at] == 0 {
			// The unused remainder. Everything from here on must be zero, or this field is not a
			// sequence list and nothing in it can be trusted.
			return if esc[at..].iter().all(|byte| *byte == 0) { level } else { None };
		}
		// An explicit escape character is not written in this field.
		if esc[at] == 0x1b {
			return None;
		}
		let start = at;
		while at < esc.len() && (0x20..=0x2f).contains(&esc[at]) {
			at += 1;
		}
		if at >= esc.len() || !(0x30..=0x7e).contains(&esc[at]) {
			return None;
		}
		at += 1;
		// The three UCS-2 levels Joliet defines. A level already found is not replaced by a lower
		// one appearing later in the same field.
		let found = match &esc[start..at] {
			b"%/@" => Some(1u8),
			b"%/C" => Some(2),
			b"%/E" => Some(3),
			_ => None,
		};
		if let Some(found) = found
			&& level.is_none_or(|held| found > held)
		{
			level = Some(found);
		}
	}
	level
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
	// A DIRECTORY IS HELD TO ITS OWN SHAPE, and refused rather than dropped.
	//
	// Skipping a regular FILE this backend will not read is a product decision; skipping a
	// DIRECTORY whose shape the format forbids is a short listing presented as a complete one, and
	// the subtree behind it disappears with no error anywhere.
	if is_dir {
		validate_directory_shape(rec[25], rec[26], rec[27])?;
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
	// THE PADDING BYTE after an even-length identifier, which the format requires to be present and
	// zero. The offset was computed and the record carried on if it was short, so a parser that
	// describes itself as strict was taking the format's word for one field and not the other.
	if id_len % 2 == 0 {
		let Some(&pad) = rec.get(33 + id_len) else {
			return Err(FsError::Corrupt);
		};
		if pad != 0 {
			return Err(FsError::Corrupt);
		}
	}
	let id = &rec[33..33 + id_len];
	// AN ASSOCIATED FILE IS DECIDED AFTER THE RECORD IS KNOWN TO BE WELL-FORMED.
	//
	// This returned before the identifier length, the bounds and the identifier itself were checked -
	// so an associated-file record with `id_len = 0` was silently dropped while the same `id_len = 0`
	// on an ordinary record was `Corrupt`. That is precisely the shape the `Result<Option<Entry>>`
	// refactor was written to remove: a malformed record disappearing from a directory rather than
	// refusing it.
	//
	// `Ok(None)` means "a well-formed record this reader deliberately does not surface", which is
	// what the type was introduced to say. An associated file (flag 0x04) is a secondary stream
	// recorded BEFORE its same-named main file: it must neither list (a duplicate name) nor match a
	// lookup (it would shadow the main content).
	// AND THE IDENTIFIER IS DECODED FIRST, so "well-formed" means the whole record.
	//
	// The skip moved after the structural checks and stopped in front of the NAME, which left one
	// asymmetry of the pair this was meant to remove: an odd-length UCS-2 identifier on a Joliet
	// associated record was dropped silently, while the identical identifier on an ordinary record
	// was `Corrupt`. The milestone's own word for what `Ok(None)` means is "a WELL-FORMED record
	// this reader deliberately does not surface", and a record whose name does not decode is not
	// well-formed.
	//
	// The decode's cost is paid for a record that is then discarded, which is the price of the
	// claim being true.
	let special = id_len == 1 && (id[0] == 0 || id[0] == 1);
	let (name, case_sensitive) = if special { (String::from(if id[0] == 0 { "." } else { ".." }), false) } else { decode_name(id, rec, id_len, joliet, rrip, skip)? };
	// The decoded name has to BE a name: one path component, with nothing in it that makes a lookup
	// mean something other than what the listing showed.
	if !special {
		validate_component(&name, joliet)?;
	}
	if rec[25] & 0x04 != 0 {
		return Ok(None);
	}
	Ok(Some(Entry { lba, size, is_dir, special, unsupported, name, case_sensitive }))
}

// ONE PATH COMPONENT, and nothing that corrupts a terminal.
//
// `decode_name` and `rock_ridge_name` built a `String` and accepted `/`, NUL and control characters.
// A UCS-2 `/` therefore produced an entry listed as `aaa/bbb` that lookup splits into two
// components, so the entry could not be opened by the name it was listed under - and control
// characters additionally corrupt whatever renders the listing. Joliet's own rules forbid the
// control code points along with `/`, `:`, `;`, `?` and `\\`.
//
// `/` and NUL are `Corrupt` on every namespace, because they are the two characters that make a
// name mean something other than a name. The rest of Joliet's forbidden set is refused on the
// Joliet namespace, where it is the format's rule, and tolerated on the ISO and Rock Ridge ones,
// where those characters are legal in a name a real disc carries.
fn validate_component(name: &str, joliet: bool) -> Result<(), FsError> {
	for ch in name.chars() {
		if ch == '/' || ch == '\0' || ch.is_control() {
			return Err(FsError::Corrupt);
		}
		if joliet && matches!(ch, ':' | ';' | '?' | '\\') {
			return Err(FsError::Corrupt);
		}
	}
	Ok(())
}

// Decode an entry name. Joliet is big-endian UCS-2; otherwise a Rock Ridge NM entry in
// the system-use area wins, falling back to plain ASCII 8.3 with ";version" dropped.
// THE THREE ANSWERS A NAME DECODE HAS, told apart.
//
// This returned `Option`, and `parse_record` turned every `None` into `FsError::Corrupt`. So a
// failed reservation - a healthy disc read on a machine briefly short of memory - was reported as a
// DAMAGED MEDIUM, which sends a person to replace media that is fine. The Rock Ridge arm was worse:
// there a reserve failure read as "this record has no NM entry" and the reader fell back to the
// primary ISO namespace, so a file appeared under a different name than the disc intends.
//
// `Err(NoMemory)` is now distinct from `Err(Corrupt)`, and absence stays absence.
fn decode_name(id: &[u8], rec: &[u8], id_len: usize, joliet: bool, rrip: bool, skip: usize) -> Result<(String, bool), FsError> {
	if joliet {
		// An odd length is corruption and an invalid unit is corruption, and neither may become a
		// NAME. `chunks_exact(2)` dropped a trailing odd byte and an unpaired surrogate became '?',
		// so two distinct damaged identifiers could collide on one name - and that name is the
		// lookup key.
		if id.len() % 2 != 0 {
			return Err(FsError::Corrupt);
		}
		// RESERVED UP FRONT, FALLIBLY. `read_dir` reserves with `try_reserve` and returns
		// `NoMemory`, and the NAMES on the same path used `String::new` and `push` with no
		// reservation - so the milestone's claim that a hostile disc never exhausts the service had
		// one allocation with no route to `FsError::NoMemory` in it. A single name is bounded by
		// the 255-byte record, so this is small; what matters is that the path has no infallible
		// step left. Worst case is three UTF-8 bytes per UCS-2 unit.
		let mut s = String::new();
		if s.try_reserve(id.len() / 2 * 3).is_err() {
			return Err(FsError::NoMemory);
		}
		for c in id.chunks_exact(2) {
			let u = u16::from_be_bytes([c[0], c[1]]);
			if u == b';' as u16 {
				break;
			}
			// An invalid UCS-2 unit is corruption in the identifier, not a shortage.
			let Some(c) = char::from_u32(u as u32) else {
				return Err(FsError::Corrupt);
			};
			s.push(c);
		}
		return Ok((s, true));
	}
	let sys_off = 33 + id_len + (id_len % 2 == 0) as usize;
	// a malformed record can end exactly after its identifier (the pad byte missing) -
	// there is no system-use area to read then, never a slice past the record.
	if let Some(sys) = rec.get(sys_off..)
		&& let Some(n) = rock_ridge_name(sys, rrip, skip)?
	{
		return Ok((n, true));
	}
	// THE VERSION SUFFIX IS PARSED BEFORE IT IS REMOVED.
	//
	// Both decoders stopped at the first `;` and never asked what followed it, so `A;garbage`,
	// `A;0`, `A;1;2` and `;1` all became `A` or an empty name. Normalisation without validation
	// makes distinct malformed identifiers COLLIDE with a valid one - and the decoded name is the
	// lookup key, so a nonconforming record could hide a real file or be returned under a name the
	// medium never legally declared. `validate_component` runs afterwards, by which time the
	// evidence has been removed.
	//
	// ECMA-119 fixes the shape: one separator, then a decimal version in 1..=32767, and a base
	// identifier that is not empty.
	if let Some(at) = id.iter().position(|&b| b == b';') {
		let (base, version) = (&id[..at], &id[at + 1..]);
		if base.is_empty() || version.is_empty() || version.len() > 5 {
			return Err(FsError::Corrupt);
		}
		if version.iter().any(|b| !b.is_ascii_digit()) {
			return Err(FsError::Corrupt);
		}
		// A leading zero is a second spelling of one number, and two spellings of one name are what
		// this check exists to remove.
		if version[0] == b'0' {
			return Err(FsError::Corrupt);
		}
		let value = version.iter().fold(0u32, |acc, b| acc * 10 + (b - b'0') as u32);
		if value == 0 || value > 32767 {
			return Err(FsError::Corrupt);
		}
	}
	let mut s = String::new();
	// The same reservation as the Joliet arm: one byte per identifier byte is enough, because the
	// identifier is ASCII-oriented and a byte above 0x7F costs at most two.
	if s.try_reserve(id.len() * 2).is_err() {
		return Err(FsError::NoMemory);
	}
	for &b in id {
		if b == b';' {
			break;
		}
		s.push(b as char);
	}
	if s.ends_with('.') {
		s.pop();
	}
	Ok((s, false))
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
// `Ok(None)` is a record with no usable `NM`, which is ordinary; `Err(NoMemory)` is this machine
// being short, which must never be mistaken for it - falling back to the ISO name on a memory
// shortage renames a file for a reason that has nothing to do with the disc.
fn rock_ridge_name(sys: &[u8], rrip: bool, skip: usize) -> Result<Option<String>, FsError> {
	if !rrip || skip > sys.len() {
		return Ok(None);
	}
	let sys = &sys[skip..];
	let mut out = String::new();
	// The whole System Use Area bounds every fragment this can accumulate, and a `NM` cannot be
	// longer than the record that holds it. Reserved once so the pushes below cannot be the one
	// infallible allocation on an otherwise fallible path.
	if out.try_reserve(sys.len()).is_err() {
		return Err(FsError::NoMemory);
	}
	// ONE SUSP PARSER. This walked the area itself - its own length and signature decode, reading
	// `sys[off + 2]` and `sys[off..off + 2]` and never `sys[off + 3]` - so the version rule the
	// walker enforces did not apply to the one entry whose contents become a filename. The test that
	// covers the version rule alters an `ER`, which does go through the walker.
	//
	// A MALFORMED ENTRY IS NOT AN END, it is a reason to distrust everything after it - and
	// everything before it, because what has been accumulated so far is a PREFIX. The walker's
	// `Err(())` carries exactly that, which a `FnMut` callback could not.
	let walked = for_each_susp(sys, |entry| {
		// FAIL CLOSED on what this parser cannot follow, rather than answering with a different
		// name than the medium records.
		//
		// `CE` continues an entry into another block, so an `NM` split across one yields its PREFIX
		// here - and two long names differing late become the same name. `CL`, `PL` and `RE` are how
		// Rock Ridge relocates deep directories, so ignoring them dismantles the structure they
		// exist to express. Falling back to the ISO9660 identifier is a name the medium really
		// carries; a truncated `NM` is not.
		if matches!(&entry.signature, b"CE" | b"CL" | b"PL" | b"RE") {
			return Err(());
		}
		// an NM payload begins after the flags byte - a shorter entry carries no name.
		if &entry.signature == b"NM" && !entry.body.is_empty() {
			// The CONTINUE flag (bit 0) says this name runs into a further entry. Honouring only
			// the part in hand is the same truncation `CE` causes.
			if entry.body[0] & 0x01 != 0 {
				return Err(());
			}
			// NON-UTF-8 IS A REFUSAL, not an empty fragment. `unwrap_or("")` contributed nothing
			// and left whatever the other fragments held, which is a name assembled out of the
			// parts that happened to decode - the same partial answer, one layer down.
			let Ok(fragment) = core::str::from_utf8(&entry.body[1..]) else {
				return Err(());
			};
			// A SECOND `NM` WITHOUT CONTINUE IS FAIL-CLOSED, not something to append.
			//
			// The CONTINUE flag is refused above because continued names are not implemented, and
			// then every further `NM` was appended regardless - so `NM "foo"` and `NM "bar"`,
			// neither continued, produced `foobar`: a third name the medium does not carry, used as
			// the lookup key. Whether that layout should be `Corrupt` or fall back to the ISO
			// identifier is a judgement; inventing a name is the one answer that is wrong, and
			// until continuation is implemented this is the same refusal `CE` already gets.
			if !out.is_empty() {
				return Err(());
			}
			out.push_str(fragment);
		}
		Ok(())
	});
	if walked.is_err() {
		return Ok(None);
	}
	Ok(if out.is_empty() { None } else { Some(out) })
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
// `Ok(())` when the area was walked to its end or to an `ST`; `Err(())` when an entry is malformed
// or carries a version this reader does not implement.
//
// THE DIFFERENCE MATTERS TO ONE CALLER AND A `FnMut` CANNOT CARRY IT. `rock_ridge_name` fails closed
// - a structurally broken area means the name it has accumulated is a PREFIX, and a prefix that
// becomes a lookup key is two long names collapsing into one - and it could not use this walker
// while the walker could only stop. So it had a second length-and-signature decode of its own, which
// never read the version byte at all: an `NM` declaring version 2 was consumed as though it were
// version 1, and `NM` is the entry whose contents become a filename.
//
// The callback may also stop the walk itself, by answering `Err(())` - which is how a caller refuses
// a signature it cannot follow.
fn for_each_susp(sys: &[u8], mut f: impl FnMut(&SuspEntry<'_>) -> Result<(), ()>) -> Result<(), ()> {
	let mut at = 0usize;
	while at + 4 <= sys.len() {
		let sig = [sys[at], sys[at + 1]];
		let len = sys[at + 2] as usize;
		// TOLERANT, AND THAT IS THE DECISION rather than the behaviour that happens to exist.
		//
		// `ST` ends the walk on its signature alone: its own length and version are not validated,
		// and everything after it is unread whatever it holds. A maximally strict reading would
		// check the terminator's structure too and state how much trailing padding is acceptable.
		//
		// The argument against strictness wins here and it is about what the reader is FOR. Every
		// byte past `ST` is already outside the area's meaning, so a malformed terminator cannot
		// mislead this parser into reading something as a filename - which is the failure mode the
		// checks above exist against. What strictness would buy is refusing a disc whose mastering
		// tool left harmless slack, and those discs work everywhere else. A reader of removable
		// media that refuses what every other reader accepts is wrong even when it is right.
		//
		// The line that would move if this were reconsidered: validate `len >= 4` and
		// `sys[at + 3] == 1` before returning, and refuse a `ST` that runs past the area.
		if &sig == b"ST" {
			return Ok(());
		}
		// PADDING IS NOT DAMAGE. A System Use Area is padded to the record's length, and the padding
		// is zeros - so a zero signature is the end of the entries rather than an entry this reader
		// could not parse. Reading it as damage makes every ordinary Rock Ridge area malformed.
		//
		// The same decision as `ST` above and for the same reason: a signature of zero is treated as
		// the end by SIGNATURE ALONE, without asking whether the rest of the area is really zero.
		// Checking it would refuse discs over bytes that mean nothing to this reader either way.
		if sig == [0, 0] {
			return Ok(());
		}
		if len < 4 || at + len > sys.len() {
			return Err(());
		}
		// THE VERSION, which this handed out as part of a slice nobody looked at.
		//
		// Every SUSP entry carries one and every entry this reader implements is version 1. An entry
		// at some other version is a structure with the same signature and different contents, and
		// reading its body as though it were the version this code knows is how another extension's
		// bytes become a filename - the thing the `ST` check above exists to stop, one field in.
		let entry = SuspEntry { signature: sig, version: sys[at + 3], body: &sys[at + 4..at + len] };
		if entry.version != 1 {
			return Err(());
		}
		f(&entry)?;
		at += len;
	}
	Ok(())
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
// THREE STATES, NOT TWO. This returned `Option` for a genuine absence, a malformed SUSP area and a
// malformed `CE`, and the mount loop read every `None` as "the chain ended normally". So a small
// corruption in one entry turned Rock Ridge OFF for the whole disc with no error anywhere - every
// filename silently becoming its ISO fallback, which can collide with another file's, and the
// caller opening a different path than the medium intends.
//
// `Err(())` is now "this area or this pointer is damaged" and `Ok(None)` is "there is no `CE`".
fn continuation_of(sys: &[u8]) -> Result<Option<(u32, usize, usize)>, ()> {
	let mut found = None;
	// Set when a `CE` IS present and its own fields are impossible. Kept apart from the walk's own
	// failure, which is a different thing: an area this reader cannot walk may simply hold a SUSP
	// entry at a version it does not implement, and ending the chain there is conservative. A
	// broken `CE` is damage in the pointer that decides where Rock Ridge is looked for.
	let mut damaged = false;
	// A malformed area answers `None` here rather than the entries before the damage: what a `CE`
	// points at decides where Rock Ridge is looked for, and a pointer read out of a broken area is a
	// pointer to nothing this reader can vouch for.
	let walked = for_each_susp(sys, |entry| {
		let body = entry.body;
		if &entry.signature != b"CE" || body.len() < 24 || found.is_some() {
			return Ok(());
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
		// A `CE` whose both-endian halves disagree is damage, not an absent pointer.
		let (Some(block), Some(offset), Some(len)) = (both32(&body[0..8]), both32(&body[8..16]), both32(&body[16..24])) else {
			damaged = true;
			return Ok(());
		};
		let (offset, len) = (offset as usize, len as usize);
		// A continuation has to fit in the block it names, or it is describing something else.
		if offset >= SECTOR_SIZE || len == 0 || offset + len > SECTOR_SIZE {
			damaged = true;
			return Ok(());
		}
		found = Some((block, offset, len));
		Ok(())
	});
	// AN AREA THIS READER CANNOT WALK IS NOT DAMAGE. It may hold a SUSP entry at a version this
	// build does not implement, which is a different structure rather than a broken one - so the
	// chain ends here, conservatively, exactly as before. What became an error is narrower and is
	// the thing the audit named: a `CE` that IS present and whose fields are impossible.
	if damaged {
		return Err(());
	}
	if walked.is_err() {
		return Ok(None);
	}
	Ok(found)
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
	// And a malformed area announces NOTHING: switching the whole disc's name source on the strength
	// of an area this reader could not parse is the guess the `ER` check exists to remove.
	let walked = for_each_susp(sys, |entry| {
		let body = entry.body;
		if &entry.signature != b"ER" || body.len() < 4 {
			return Ok(());
		}
		// EVERY DECLARED LENGTH, against the entry's own size.
		//
		// `ER` is `LEN_ID, LEN_DES, LEN_SRC, EXT_VER` and then the identifier, the description and
		// the source in that order. Only `LEN_ID` was read, so an `ER` carrying `RRIP_1991A` and
		// nonsense in every other field switched the whole disc's name source.
		let (id_len, des_len, src_len) = (body[0] as usize, body[1] as usize, body[2] as usize);
		let Some(declared) = id_len.checked_add(des_len).and_then(|n| n.checked_add(src_len)) else { return Ok(()) };
		if 4 + declared > body.len() + 4 || declared > body.len().saturating_sub(4) {
			return Ok(());
		}
		// The identifier has to BE there, not merely be declared.
		let Some(id) = body.get(4..4 + id_len) else { return Ok(()) };
		// The versions of Rock Ridge whose `NM`, `CE`, `CL`, `PL` and `RE` this reader understands.
		// An extension it does not implement must not switch the name source: reading somebody
		// else's `NM` is the guess this whole check exists to remove.
		// AND THE VERSION, which `EXT_VER` records and this ignored - so a medium could pair the
		// known `RRIP_1991A` identifier with a different extension version and still get the parser
		// written for the version this reader knows. Every one of these extensions is version 1;
		// anything else is a structure with the same name and a layout nobody here has seen.
		if body[3] != 1 {
			return Ok(());
		}
		if id == b"RRIP_1991A" || id == b"IEEE_P1282" || id == b"IEEE_1282" {
			announced = true;
		}
		Ok(())
	});
	walked.is_ok() && announced
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
