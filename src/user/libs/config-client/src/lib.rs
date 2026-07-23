#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use config_proto::generated::liber::config::v1::{ConfigEntry, Picked};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_config_config_get"]
	fn config_get(chan: u64, key: &str) -> Option<Result<String, Error>>;
	#[link_name = "liber_channel_liber_config_config_list"]
	fn config_list(chan: u64) -> Option<Result<Vec<ConfigEntry>, Error>>;
	#[link_name = "liber_channel_liber_config_config_set"]
	fn config_set(chan: u64, entry: &ConfigEntry) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_config_picker_pick"]
	fn picker_pick(chan: u64) -> Option<Result<Picked, Error>>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct ConfigClient {
	chan: u64,
}

impl ConfigClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn get(&mut self, key: &str) -> Option<Result<String, Error>> {
		unsafe { config_get(self.chan, key) }
	}

	#[inline(always)]
	pub fn list(&mut self) -> Option<Result<Vec<ConfigEntry>, Error>> {
		unsafe { config_list(self.chan) }
	}

	#[inline(always)]
	pub fn set(&mut self, entry: &ConfigEntry) -> Option<Result<(), Error>> {
		unsafe { config_set(self.chan, entry) }
	}
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PickerClient {
	chan: u64,
}

impl PickerClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn pick(&mut self) -> Option<Result<Picked, Error>> {
		unsafe { picker_pick(self.chan) }
	}
}
