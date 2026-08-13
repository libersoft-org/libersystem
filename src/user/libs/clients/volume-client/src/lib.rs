#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use storage_proto::codec::Buffer;
use storage_proto::generated::liber::storage::v1::{FileInfo, FsckReport, OpenOpts, OpenResult, SnapshotInfo, VolumeStatus, WriterMode};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_storage_volume_open"]
	fn volume_open(chan: u64, options: &OpenOpts) -> Option<Result<OpenResult, Error>>;
	#[link_name = "liber_channel_liber_storage_volume_remove"]
	fn volume_remove(chan: u64, path: &str) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_mkdir"]
	fn volume_mkdir(chan: u64, path: &str) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_rmdir"]
	fn volume_rmdir(chan: u64, path: &str) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_list"]
	fn volume_list(chan: u64, path: &str) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_storage_volume_write"]
	fn volume_write(chan: u64, path: &str, data: &Buffer) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_snap_create"]
	fn volume_snap_create(chan: u64, name: &str) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_snap_list"]
	fn volume_snap_list(chan: u64) -> Option<Result<Vec<SnapshotInfo>, Error>>;
	#[link_name = "liber_channel_liber_storage_volume_snap_delete"]
	fn volume_snap_delete(chan: u64, name: &str) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_snap_open"]
	fn volume_snap_open(chan: u64, snapshot: &str, path: &str) -> Option<Result<OpenResult, Error>>;
	#[link_name = "liber_channel_liber_storage_volume_capacity"]
	fn volume_capacity(chan: u64) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_storage_volume_status"]
	fn volume_status(chan: u64) -> Option<Result<VolumeStatus, Error>>;
	#[link_name = "liber_channel_liber_storage_volume_set_compression"]
	fn volume_set_compression(chan: u64, enabled: &bool) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_fsck"]
	fn volume_fsck(chan: u64) -> Option<Result<FsckReport, Error>>;
	#[link_name = "liber_channel_liber_storage_volume_restore"]
	fn volume_restore(chan: u64, path: &str, snapshot: &str) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_write_stream_begin"]
	fn volume_write_stream_begin(chan: u64, correlation: u32, path: &str, data: u64) -> bool;
	#[link_name = "liber_channel_liber_storage_volume_write_stream_finish"]
	fn volume_write_stream_finish(chan: u64, correlation: u32) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_stat"]
	fn volume_stat(chan: u64, path: &str) -> Option<Result<FileInfo, Error>>;
	#[link_name = "liber_channel_liber_storage_volume_rename"]
	fn volume_rename(chan: u64, from: &str, to: &str) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_truncate"]
	fn volume_truncate(chan: u64, path: &str, length: &u64) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_touch"]
	fn volume_touch(chan: u64, path: &str, create: &bool, at: &u64) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_read"]
	fn volume_read(chan: u64, path: &str, offset: &u64, length: &u32) -> Option<Result<Buffer, Error>>;
	#[link_name = "liber_channel_liber_storage_volume_watch"]
	fn volume_watch(chan: u64, path: &str) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_storage_volume_open_writer"]
	fn volume_open_writer(chan: u64, path: &str, mode: &WriterMode) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_storage_writer_write"]
	fn writer_write(chan: u64, data: &[u8]) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_storage_writer_write_at"]
	fn writer_write_at(chan: u64, offset: &u64, data: &[u8]) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_writer_truncate"]
	fn writer_truncate(chan: u64, length: &u64) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_writer_flush"]
	fn writer_flush(chan: u64) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_storage_writer_commit"]
	fn writer_commit(chan: u64) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_storage_writer_abort"]
	fn writer_abort(chan: u64) -> Option<Result<(), Error>>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct VolumeClient {
	chan: u64,
}

impl VolumeClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn open(&mut self, options: &OpenOpts) -> Option<Result<OpenResult, Error>> {
		unsafe { volume_open(self.chan, options) }
	}

	#[inline(always)]
	pub fn remove(&mut self, path: &str) -> Option<Result<(), Error>> {
		unsafe { volume_remove(self.chan, path) }
	}

	#[inline(always)]
	pub fn mkdir(&mut self, path: &str) -> Option<Result<(), Error>> {
		unsafe { volume_mkdir(self.chan, path) }
	}

	#[inline(always)]
	pub fn rmdir(&mut self, path: &str) -> Option<Result<(), Error>> {
		unsafe { volume_rmdir(self.chan, path) }
	}

	#[inline(always)]
	pub fn list(&mut self, path: &str) -> Option<Result<u64, Error>> {
		unsafe { volume_list(self.chan, path) }
	}

	#[inline(always)]
	pub fn write(&mut self, path: &str, data: &Buffer) -> Option<Result<(), Error>> {
		unsafe { volume_write(self.chan, path, data) }
	}

	#[inline(always)]
	pub fn snap_create(&mut self, name: &str) -> Option<Result<(), Error>> {
		unsafe { volume_snap_create(self.chan, name) }
	}

	#[inline(always)]
	pub fn snap_list(&mut self) -> Option<Result<Vec<SnapshotInfo>, Error>> {
		unsafe { volume_snap_list(self.chan) }
	}

	#[inline(always)]
	pub fn snap_delete(&mut self, name: &str) -> Option<Result<(), Error>> {
		unsafe { volume_snap_delete(self.chan, name) }
	}

	#[inline(always)]
	pub fn snap_open(&mut self, snapshot: &str, path: &str) -> Option<Result<OpenResult, Error>> {
		unsafe { volume_snap_open(self.chan, snapshot, path) }
	}

	#[inline(always)]
	pub fn capacity(&mut self) -> Option<Result<u64, Error>> {
		unsafe { volume_capacity(self.chan) }
	}

	#[inline(always)]
	pub fn status(&mut self) -> Option<Result<VolumeStatus, Error>> {
		unsafe { volume_status(self.chan) }
	}

	#[inline(always)]
	pub fn set_compression(&mut self, enabled: &bool) -> Option<Result<(), Error>> {
		unsafe { volume_set_compression(self.chan, enabled) }
	}

	#[inline(always)]
	pub fn fsck(&mut self) -> Option<Result<FsckReport, Error>> {
		unsafe { volume_fsck(self.chan) }
	}

	#[inline(always)]
	pub fn restore(&mut self, path: &str, snapshot: &str) -> Option<Result<(), Error>> {
		unsafe { volume_restore(self.chan, path, snapshot) }
	}

	#[inline(always)]
	pub fn stat(&mut self, path: &str) -> Option<Result<FileInfo, Error>> {
		unsafe { volume_stat(self.chan, path) }
	}

	#[inline(always)]
	pub fn rename(&mut self, from: &str, to: &str) -> Option<Result<(), Error>> {
		unsafe { volume_rename(self.chan, from, to) }
	}

	#[inline(always)]
	pub fn truncate(&mut self, path: &str, length: u64) -> Option<Result<(), Error>> {
		unsafe { volume_truncate(self.chan, path, &length) }
	}

	#[inline(always)]
	/// Stamp `path`'s modification time to `at` (Unix seconds, UTC), creating it when asked to.
	/// Zero leaves the service's own clock in place.
	pub fn touch(&mut self, path: &str, create: bool, at: u64) -> Option<Result<(), Error>> {
		unsafe { volume_touch(self.chan, path, &create, &at) }
	}

	/// One window of a file, as a shared buffer of exactly the bytes delivered. A short answer is
	/// the end of the file; see the contract at `volume.read`.
	#[inline(always)]
	pub fn read(&mut self, path: &str, offset: u64, length: u32) -> Option<Result<Buffer, Error>> {
		unsafe { volume_read(self.chan, path, &offset, &length) }
	}

	/// Subscribe to changes to a path. The handle is the consumer end of an event stream; each
	/// message on it decodes with `volume::watch_read`, and the stream ends when either side
	/// closes it.
	#[inline(always)]
	pub fn watch(&mut self, path: &str) -> Option<Result<u64, Error>> {
		unsafe { volume_watch(self.chan, path) }
	}

	/// Open a transactional writer over one path. Nothing the session writes is visible until
	/// `commit`; dropping the client aborts it.
	#[inline(always)]
	pub fn open_writer(&mut self, path: &str, mode: WriterMode) -> Option<Result<WriterClient, Error>> {
		match unsafe { volume_open_writer(self.chan, path, &mode) } {
			Some(Ok(chan)) => Some(Ok(WriterClient { chan })),
			Some(Err(e)) => Some(Err(e)),
			None => None,
		}
	}

	#[inline(always)]
	pub fn begin_write_stream(self, path: &str, data: u64) -> Option<PendingWrite> {
		const CORRELATION: u32 = 0;
		if unsafe { volume_write_stream_begin(self.chan, CORRELATION, path, data) } { Some(PendingWrite { chan: self.chan, correlation: CORRELATION }) } else { None }
	}
}

pub struct PendingWrite {
	chan: u64,
	correlation: u32,
}

impl PendingWrite {
	#[inline(always)]
	pub fn finish(self) -> Option<Result<(), Error>> {
		unsafe { volume_write_stream_finish(self.chan, self.correlation) }
	}
}

/// A transactional writer session over one path, from `VolumeClient::open_writer`.
///
/// The session stages bytes in StorageService and publishes them only on `commit`, so a client
/// that dies half way leaves the file exactly as it was. The channel is NOT closed by this type:
/// its owner closes it, and closing an uncommitted session is an abort.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct WriterClient {
	chan: u64,
}

impl WriterClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	/// The session's channel, for the owner that has to close it.
	#[inline(always)]
	pub const fn handle(&self) -> u64 {
		self.chan
	}

	/// Append at the cursor, returning the staged length. One call carries at most 65535 bytes -
	/// the wire's list length - so a large file is written by repeating it.
	#[inline(always)]
	pub fn write(&mut self, data: &[u8]) -> Option<Result<u64, Error>> {
		unsafe { writer_write(self.chan, data) }
	}

	#[inline(always)]
	pub fn write_at(&mut self, offset: u64, data: &[u8]) -> Option<Result<(), Error>> {
		unsafe { writer_write_at(self.chan, &offset, data) }
	}

	#[inline(always)]
	pub fn truncate(&mut self, length: u64) -> Option<Result<(), Error>> {
		unsafe { writer_truncate(self.chan, &length) }
	}

	#[inline(always)]
	pub fn flush(&mut self) -> Option<Result<u64, Error>> {
		unsafe { writer_flush(self.chan) }
	}

	/// Publish the staged bytes and end the session, returning the published length.
	#[inline(always)]
	pub fn commit(&mut self) -> Option<Result<u64, Error>> {
		unsafe { writer_commit(self.chan) }
	}

	/// Discard everything staged and end the session.
	#[inline(always)]
	pub fn abort(&mut self) -> Option<Result<(), Error>> {
		unsafe { writer_abort(self.chan) }
	}
}
