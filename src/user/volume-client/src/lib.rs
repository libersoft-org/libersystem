#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use storage_proto::codec::Buffer;
use storage_proto::generated::liber::storage::v1::{FsckReport, OpenOpts, OpenResult, SnapshotInfo, VolumeStatus};

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
	fn volume_list(chan: u64, path: &str) -> Option<u64>;
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
	pub fn list(&mut self, path: &str) -> Option<u64> {
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
