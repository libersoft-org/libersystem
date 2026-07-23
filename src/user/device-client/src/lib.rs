#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use device_proto::generated::liber::device::v1::{DeviceEntry, UsbDevice};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_device_device_list"]
	fn device_list(chan: u64) -> Option<Result<Vec<DeviceEntry>, Error>>;
	#[link_name = "liber_channel_liber_device_device_get"]
	fn device_get(chan: u64, index: &u32) -> Option<Result<DeviceEntry, Error>>;
	#[link_name = "liber_channel_liber_device_usb_list"]
	fn usb_list(chan: u64) -> Option<Result<Vec<UsbDevice>, Error>>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct DeviceClient {
	chan: u64,
}

impl DeviceClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn list(&mut self) -> Option<Result<Vec<DeviceEntry>, Error>> {
		unsafe { device_list(self.chan) }
	}

	#[inline(always)]
	pub fn get(&mut self, index: &u32) -> Option<Result<DeviceEntry, Error>> {
		unsafe { device_get(self.chan, index) }
	}
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct UsbClient {
	chan: u64,
}

impl UsbClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn list(&mut self) -> Option<Result<Vec<UsbDevice>, Error>> {
		unsafe { usb_list(self.chan) }
	}
}
