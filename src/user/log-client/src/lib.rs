#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::{Entry, Error, Query};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_log_log_emit"]
	fn log_emit(chan: u64, entry: &Entry) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_log_log_query"]
	fn log_query(chan: u64, query: &Query) -> Option<Result<Vec<Entry>, Error>>;
	#[link_name = "liber_channel_liber_log_log_tail"]
	fn log_tail(chan: u64, query: &Query) -> Option<u64>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct LogClient {
	chan: u64,
}

impl LogClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn emit(&mut self, entry: &Entry) -> Option<Result<(), Error>> {
		unsafe { log_emit(self.chan, entry) }
	}

	#[inline(always)]
	pub fn query(&mut self, query: &Query) -> Option<Result<Vec<Entry>, Error>> {
		unsafe { log_query(self.chan, query) }
	}

	#[inline(always)]
	pub fn tail(&mut self, query: &Query) -> Option<u64> {
		unsafe { log_tail(self.chan, query) }
	}
}
