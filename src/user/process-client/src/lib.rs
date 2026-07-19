#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::{Error, ProcessInfo, StartResult};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_process_process_start"]
	fn process_start(chan: u64, name: &str) -> Option<Result<ProcessInfo, Error>>;
	#[link_name = "liber_channel_liber_process_process_list"]
	fn process_list(chan: u64) -> Option<Result<Vec<ProcessInfo>, Error>>;
	#[link_name = "liber_channel_liber_process_process_launch"]
	fn process_launch(chan: u64, name: &str, bootstrap: &u64) -> Option<Result<StartResult, Error>>;
	#[link_name = "liber_channel_liber_process_process_launch_bounded"]
	fn process_launch_bounded(chan: u64, name: &str, memory_limit: &u64, bootstrap: &u64) -> Option<Result<StartResult, Error>>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct ProcessClient {
	chan: u64,
}

impl ProcessClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn start(&mut self, name: &str) -> Option<Result<ProcessInfo, Error>> {
		unsafe { process_start(self.chan, name) }
	}

	#[inline(always)]
	pub fn list(&mut self) -> Option<Result<Vec<ProcessInfo>, Error>> {
		unsafe { process_list(self.chan) }
	}

	#[inline(always)]
	pub fn launch(&mut self, name: &str, bootstrap: &u64) -> Option<Result<StartResult, Error>> {
		unsafe { process_launch(self.chan, name, bootstrap) }
	}

	#[inline(always)]
	pub fn launch_bounded(&mut self, name: &str, memory_limit: &u64, bootstrap: &u64) -> Option<Result<StartResult, Error>> {
		unsafe { process_launch_bounded(self.chan, name, memory_limit, bootstrap) }
	}
}
