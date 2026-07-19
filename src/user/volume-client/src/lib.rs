#![no_std]

use proto::system::{Error, OpenOpts, OpenResult};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_storage_volume_open"]
	fn volume_open(chan: u64, options: &OpenOpts) -> Option<Result<OpenResult, Error>>;
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
}
