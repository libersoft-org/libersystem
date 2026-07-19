#![no_std]

use proto::system::{Error, OpenOpts, OpenResult};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_storage_volume_open"]
	fn volume_open(chan: u64, options: &OpenOpts) -> Option<Result<OpenResult, Error>>;
	#[link_name = "liber_channel_liber_storage_volume_remove"]
	fn volume_remove(chan: u64, path: &str) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_mkdir"]
	fn volume_mkdir(chan: u64, path: &str) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_storage_volume_rmdir"]
	fn volume_rmdir(chan: u64, path: &str) -> Option<Result<(), Error>>;
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
	pub fn begin_write_stream(self, path: &str, data: u64) -> Option<PendingWrite> {
		const CORRELATION: u32 = 0;
		if unsafe { volume_write_stream_begin(self.chan, CORRELATION, path, data) } {
			Some(PendingWrite { chan: self.chan, correlation: CORRELATION })
		} else {
			None
		}
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
