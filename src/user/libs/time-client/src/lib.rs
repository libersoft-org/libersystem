#![no_std]

use base_proto::generated::liber::base::v1::Error;
use time_proto::generated::liber::time::v1::Timestamp;

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_time_time_now"]
	fn time_now(chan: u64) -> Option<Result<Timestamp, Error>>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct TimeClient {
	chan: u64,
}

impl TimeClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn now(&mut self) -> Option<Result<Timestamp, Error>> {
		unsafe { time_now(self.chan) }
	}
}
