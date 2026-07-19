#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::{Error, Graph, SupervisorStat};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_observability_system_graph_snapshot"]
	fn system_graph_snapshot(chan: u64) -> Option<Result<Graph, Error>>;
	#[link_name = "liber_channel_liber_observability_supervisor_status"]
	fn supervisor_status(chan: u64) -> Option<Result<Vec<SupervisorStat>, Error>>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct SystemGraphClient {
	chan: u64,
}

impl SystemGraphClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn snapshot(&mut self) -> Option<Result<Graph, Error>> {
		unsafe { system_graph_snapshot(self.chan) }
	}
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct SupervisorClient {
	chan: u64,
}

impl SupervisorClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn status(&mut self) -> Option<Result<Vec<SupervisorStat>, Error>> {
		unsafe { supervisor_status(self.chan) }
	}
}
