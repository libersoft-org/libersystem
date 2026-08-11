//! fs-core - the one block-device contract and error type every filesystem backend
//! shares. LiberFS (the native read-write filesystem), FAT/exFAT (foreign removable
//! media), and ISO9660 / UDF (optical formats) all read their medium through the same
//! [`BlockDevice`] trait and report failures through the same [`FsError`], so the
//! concepts do not drift into four slightly different shapes and the storage service
//! maps one error type at its boundary rather than one per backend.
//!
//! The trait is block-size agnostic: a block is exactly `buf.len()` bytes, so the same
//! trait serves FAT's 512-byte sectors, ISO/UDF's 2048-byte blocks and LiberFS's 4 kB
//! blocks without a type parameter. Write and flush have refuse/no-op defaults, so a
//! read-only backing (ISO9660, UDF, a snapshot mount) implements only `read_block`.

#![no_std]

// A block device a filesystem reads and (for the read-write backends) writes one block
// at a time, by absolute block index. A block is exactly `buf.len()` bytes - the size
// the filesystem uses (512 for FAT sectors, 2048 for ISO9660 / UDF, 4096 for LiberFS) -
// so one trait serves every backend without a block-size type parameter. Implementors
// map the block index onto their backing (a disk's LBA range, a RAM `Vec`, a channel to
// a block service).
pub trait BlockDevice {
	// Read block `index` into `buf` (exactly one block, `buf.len()` bytes). False on I/O
	// failure.
	fn read_block(&mut self, index: u64, buf: &mut [u8]) -> bool;

	// Read `count` consecutive blocks starting at `index` into `buf` (exactly `count`
	// blocks, each `buf.len() / count` bytes). The default loops `read_block`; a backing
	// that can move a whole span in one device request (a disk's block service) overrides
	// it, so a contiguous file extent costs one round-trip instead of one per block.
	fn read_blocks(&mut self, index: u64, count: u64, buf: &mut [u8]) -> bool {
		if count == 0 {
			return true;
		}
		let block: usize = buf.len() / count as usize;
		for i in 0..count as usize {
			if !self.read_block(index + i as u64, &mut buf[i * block..(i + 1) * block]) {
				return false;
			}
		}
		true
	}

	// Write `buf` (exactly one block) to block `index`. False on I/O failure. A read-only
	// backing (ISO9660, UDF, a snapshot mount) keeps the default, which refuses the write,
	// so a read-only medium never has to carry a stub write path.
	fn write_block(&mut self, index: u64, buf: &[u8]) -> bool {
		let _ = (index, buf);
		false
	}

	// Make every write issued so far durable (flush the device's volatile write cache)
	// before any later write reaches the medium, so a commit protocol can bracket its
	// publish with a barrier. A backing with no volatile cache (memory, a write-through
	// disk, a read-only medium) keeps the default no-op. False on I/O failure.
	fn flush(&mut self) -> bool {
		true
	}
}

// A filesystem error, shared by every backend so the storage service maps one type at
// its boundary. The read-only backends (ISO9660, UDF) use only the read subset
// (`NotFound`, `NotDir`, `Invalid`, `TooLong`, `Corrupt`, `Io`); the read-write backends
// (LiberFS, FAT) use the mutation variants as well. The superset is LiberFS's, which
// already covered every other backend's variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
	// The path names nothing that exists.
	NotFound,
	// No free space to complete an allocation.
	NoSpace,
	// A path or name longer than the filesystem allows.
	TooLong,
	// A malformed path or name: an empty segment, "." or "..", not UTF-8, or a byte
	// outside the portable-name policy.
	BadName,
	// The path names a directory where a file was required (writing, truncating).
	IsDir,
	// The path names a file where a directory was required (a path component, rmdir).
	NotDir,
	// Removing or replacing a directory that still has entries.
	NotEmpty,
	// Creating something that already exists (a duplicate name or snapshot).
	Exists,
	// An operation the filesystem cannot perform, an out-of-range value read off an
	// untrusted medium, or an internal inconsistency.
	Invalid,
	// A block read back whose checksum did not match the one stored beside its pointer:
	// on-disk corruption, surfaced instead of returning the bad bytes.
	Corrupt,
	// An I/O failure reported by the block device.
	Io,
	// The mount is read-only (an optical medium, a snapshot mount, or a volume degraded
	// by corruption): every mutation is refused so the on-disk state stays intact.
	ReadOnly,
	// The filesystem could not get the MEMORY it needed, which is not the medium being full.
	//
	// They were the same answer, and they drive opposite policies: `NoSpace` says delete something
	// or use another volume, `NoMemory` says the storage service is under pressure and the same
	// request may well succeed in a moment. `MountError` had told them apart since it existed; the
	// operations underneath it had not.
	NoMemory,
	// The answer does not fit in one buffer. A file larger than a caller's address space is not a
	// damaged volume, and reporting it as `Corrupt` sent a caller looking for a filesystem fault
	// that is not there - a ranged read is the answer, not a repair.
	TooLarge,
	// A commit that MAY have landed. The superblock write was attempted or has provably been
	// published, and something after it - the barrier, or the walk that rebuilds the free map -
	// failed. Once the superblock has been offered to the device, a reported failure does not mean
	// it did not reach the medium, so the new state is adopted and the volume goes read-only.
	//
	// This exists because `Io` cannot say it. A caller told `Io` reasonably retries; a caller told
	// this one must not - the write it is holding may already be on the disk, and the volume it
	// would retry against is a different generation from the one it read. Every mutation is refused
	// from here on, so the only correct responses are to remount and look, or to repair.
	CommitUncertain,
}

// Is byte `c` allowed in a portable file name?
//
// Rejects NUL and the control bytes (0x00..=0x1F and 0x7F) and the cross-platform-reserved set
// `\ : * ? < > | "`. `/` never reaches here - it is the path separator, and a name containing one
// is a path, not a name.
//
// It lives HERE because two writable backends were enforcing two different policies while
// StorageService mounted both: LiberFS checked it and LiberMemFS did not, so an application could
// create `foo:bar` on the live `vol://system` and then fail to create it on an installed LiberFS
// one - the same call, the same API, a different answer depending on which filesystem happened to
// be underneath. A contract stated in one crate and enforced in one of its implementations is not
// a contract.
pub fn is_portable_name_byte(c: u8) -> bool {
	if c < 0x20 || c == 0x7F {
		return false;
	}
	!matches!(c, b'\\' | b':' | b'*' | b'?' | b'<' | b'>' | b'|' | b'"')
}

// The whole of what `FsError::BadName` documents, in one place: an empty segment, `.` or `..`, not
// UTF-8, or a byte outside the portable-name policy. `max` is the backend's own name-length
// ceiling, which is the one part of the rule that legitimately differs between filesystems.
pub fn validate_name_segment(seg: &[u8], max: usize) -> Result<(), FsError> {
	if seg.is_empty() || seg == b"." || seg == b".." {
		return Err(FsError::BadName);
	}
	if seg.len() > max {
		return Err(FsError::TooLong);
	}
	if core::str::from_utf8(seg).is_err() {
		return Err(FsError::BadName);
	}
	if seg.iter().any(|&c| c == b'/' || !is_portable_name_byte(c)) {
		return Err(FsError::BadName);
	}
	Ok(())
}

// Is `name` portable to FAT and NTFS media as well as legal here?
//
// A SEPARATE question from `validate_name_segment`, and the separation is the point. The filesystem
// accepts what it can address; portability is a property a caller may want to require of names it
// creates, and tightening the filesystem for it would make a volume written elsewhere unreadable
// here - a checker that refuses names the medium legitimately carries is worse than one that
// accepts them.
//
// What `validate_name_segment` already covers is the byte set. What this adds is the rest of what
// "moves cleanly onto FAT and NTFS" actually requires, and what the comment claiming it did not:
//
//   * the reserved DEVICE names, which those systems resolve to hardware rather than to files -
//     `CON`, `PRN`, `AUX`, `NUL`, `COM1`..`COM9`, `LPT1`..`LPT9`, with or without an extension
//   * a trailing dot or space, which they silently strip, so two distinct names here become one
//     there and the second write destroys the first
//
// Case folding and Unicode normalisation are NOT covered and cannot be by a rule of this shape:
// `Foo` and `foo` are two names here and one on a case-insensitive volume, and deciding that is a
// property of the destination, not of the name. Whoever copies a tree has to answer it.
pub fn is_portable_name(name: &[u8]) -> bool {
	if validate_name_segment(name, 255).is_err() {
		return false;
	}
	if name.last().is_some_and(|&c| c == b'.' || c == b' ') {
		return false;
	}
	// The device name is what precedes the first dot, compared without case.
	let stem = match name.iter().position(|&c| c == b'.') {
		Some(at) => &name[..at],
		None => name,
	};
	const RESERVED: [&[u8]; 4] = [b"CON", b"PRN", b"AUX", b"NUL"];
	let reserved_stem = RESERVED.iter().any(|word| word.len() == stem.len() && word.iter().zip(stem).all(|(a, b)| *a == b.to_ascii_uppercase()));
	let numbered = (stem.len() == 4) && matches!(stem[3], b'1'..=b'9') && (stem[..3].eq_ignore_ascii_case(b"COM") || stem[..3].eq_ignore_ascii_case(b"LPT"));
	!(reserved_stem || numbered)
}
