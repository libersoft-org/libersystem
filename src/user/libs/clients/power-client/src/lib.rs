//! The concrete power clients a dynamic consumer links: PowerService's `power` read interface and ProcessorPowerService's
//! `processor-power-admin`, the four operations `powerctl` uses.
//!
//! WHY A CRATE AND NOT `Client::new(ChannelTransport { .. })`, for the reason `font-client` gives: the generated client
//! is generic over its transport, so instantiating it in a consumer copies the codec into that consumer. The copy is
//! made once, inside `power-proto`, and reaches a consumer through the trampolines `power-client-provider` declares.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use power_proto::generated::liber::power::v1::{CurvePoint, PowerProfile, ProcessorPowerStatus, SourceSnapshot};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_power_power_sources"]
	fn read_sources(chan: u64) -> Option<Result<Vec<SourceSnapshot>, Error>>;
	#[link_name = "liber_channel_liber_power_processor_power_admin_status"]
	fn admin_status(chan: u64) -> Option<Result<ProcessorPowerStatus, Error>>;
	#[link_name = "liber_channel_liber_power_processor_power_admin_set_profile"]
	fn admin_set_profile(chan: u64, profile: &PowerProfile) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_power_processor_power_admin_set_fan_curve"]
	fn admin_set_fan_curve(chan: u64, fan: &str, curve: &[CurvePoint]) -> Option<Result<(), Error>>;
}

/// POWERSERVICE'S READ INTERFACE: every power source it holds.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PowerClient {
	chan: u64,
}

impl PowerClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn sources(&mut self) -> Option<Result<Vec<SourceSnapshot>, Error>> {
		unsafe { read_sources(self.chan) }
	}
}

/// PROCESSORPOWERSERVICE'S OPERATOR INTERFACE: its status, the profile and a fan's curve.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct ProcessorPowerAdminClient {
	chan: u64,
}

impl ProcessorPowerAdminClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn status(&mut self) -> Option<Result<ProcessorPowerStatus, Error>> {
		unsafe { admin_status(self.chan) }
	}

	#[inline(always)]
	pub fn set_profile(&mut self, profile: &PowerProfile) -> Option<Result<(), Error>> {
		unsafe { admin_set_profile(self.chan, profile) }
	}

	#[inline(always)]
	pub fn set_fan_curve(&mut self, fan: &str, curve: &[CurvePoint]) -> Option<Result<(), Error>> {
		unsafe { admin_set_fan_curve(self.chan, fan, curve) }
	}
}
