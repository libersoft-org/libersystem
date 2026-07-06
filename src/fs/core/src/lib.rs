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
}
