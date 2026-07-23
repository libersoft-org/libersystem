#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use resources_proto::generated::liber::resources::v1::{Budget, ResourceType};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_resources_resources_usage"]
	fn resources_usage(chan: u64) -> Option<Result<Vec<Budget>, Error>>;
	#[link_name = "liber_channel_liber_resources_resources_set_limit"]
	fn resources_set_limit(chan: u64, name: &str, resource_type: &ResourceType, limit: &u64) -> Option<Result<Budget, Error>>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct ResourcesClient {
	chan: u64,
}

impl ResourcesClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn usage(&mut self) -> Option<Result<Vec<Budget>, Error>> {
		unsafe { resources_usage(self.chan) }
	}

	#[inline(always)]
	pub fn set_limit(&mut self, name: &str, resource_type: &ResourceType, limit: &u64) -> Option<Result<Budget, Error>> {
		unsafe { resources_set_limit(self.chan, name, resource_type, limit) }
	}
}
