#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use device_proto::generated::liber::device::v1::{BindingRecord, DeviceEntry, IncidentReport, PolicyOutcome, PolicyVerb, UsbDevice};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_device_device_list"]
	fn device_list(chan: u64) -> Option<Result<Vec<DeviceEntry>, Error>>;
	#[link_name = "liber_channel_liber_device_device_get"]
	fn device_get(chan: u64, index: &u32) -> Option<Result<DeviceEntry, Error>>;
	#[link_name = "liber_channel_liber_device_device_bindings"]
	fn device_bindings(chan: u64) -> Option<Result<Vec<BindingRecord>, Error>>;
	#[link_name = "liber_channel_liber_device_device_policy_admin_apply"]
	fn policy_apply(chan: u64, index: &u32, verb: &PolicyVerb, artifact: &str) -> Option<Result<PolicyOutcome, Error>>;
	#[link_name = "liber_channel_liber_device_device_policy_admin_stored"]
	fn policy_stored(chan: u64, index: &u32) -> Option<Result<String, Error>>;
	#[link_name = "liber_channel_liber_device_device_policy_admin_incident"]
	fn policy_incident(chan: u64, index: &u32) -> Option<Result<IncidentReport, Error>>;
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

	// The binding of every device node, forwarded from DeviceManager. See the interface: this is
	// not derived anywhere on the way.
	#[inline(always)]
	pub fn bindings(&mut self) -> Option<Result<Vec<BindingRecord>, Error>> {
		unsafe { device_bindings(self.chan) }
	}
}

// THE OPERATOR'S WRITE, behind its own client type - because it is its own capability. A component
// holding `DeviceClient` renders a device list; one holding this changes a binding.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct DevicePolicyClient {
	chan: u64,
}

impl DevicePolicyClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn apply(&mut self, index: &u32, verb: &PolicyVerb, artifact: &str) -> Option<Result<PolicyOutcome, Error>> {
		unsafe { policy_apply(self.chan, index, verb, artifact) }
	}

	#[inline(always)]
	pub fn stored(&mut self, index: &u32) -> Option<Result<String, Error>> {
		unsafe { policy_stored(self.chan, index) }
	}

	// The last incident on this binding. The capture belongs where the teardown is; this is the
	// display asking for it.
	#[inline(always)]
	pub fn incident(&mut self, index: &u32) -> Option<Result<IncidentReport, Error>> {
		unsafe { policy_incident(self.chan, index) }
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
