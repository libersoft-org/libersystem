//! The concrete display clients a dynamic consumer links: DisplayService's brightness read and the brightness policy's
//! control, which the `brightness` tool uses.
//!
//! WHY A CRATE AND NOT `Client::new(ChannelTransport { .. })`, for the reason `font-client` gives: the generated client
//! is generic over its transport, so instantiating it in a consumer copies the codec into that consumer. The copy is
//! made once, inside `display-proto`, and reaches a consumer through the trampolines `display-client-provider` declares.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;
use display_proto::generated::liber::display::v1::{BacklightState, BrightnessOutput, BrightnessSet, BrightnessSettings, BrightnessTarget};

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_display_display_brightness_outputs"]
	fn read_outputs(chan: u64) -> Option<Result<Vec<BrightnessOutput>, Error>>;
	#[link_name = "liber_channel_liber_display_display_brightness_backlights"]
	fn read_backlights(chan: u64) -> Option<Result<Vec<BacklightState>, Error>>;
	#[link_name = "liber_channel_liber_display_brightness_policy_set"]
	fn policy_set(chan: u64, key: &str, target: &BrightnessTarget, allow_zero: &bool) -> Option<Result<BrightnessSet, Error>>;
	#[link_name = "liber_channel_liber_display_brightness_policy_settings"]
	fn policy_settings(chan: u64) -> Option<Result<BrightnessSettings, Error>>;
	#[link_name = "liber_channel_liber_display_brightness_policy_set_automatic"]
	fn policy_set_automatic(chan: u64, on: &bool) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_display_brightness_policy_set_idle"]
	fn policy_set_idle(chan: u64, seconds: &Option<u32>) -> Option<Result<(), Error>>;
}

/// DISPLAYSERVICE'S BRIGHTNESS READ: the outputs and every backlight.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct BrightnessClient {
	chan: u64,
}

impl BrightnessClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn outputs(&mut self) -> Option<Result<Vec<BrightnessOutput>, Error>> {
		unsafe { read_outputs(self.chan) }
	}

	#[inline(always)]
	pub fn backlights(&mut self) -> Option<Result<Vec<BacklightState>, Error>> {
		unsafe { read_backlights(self.chan) }
	}
}

/// THE BRIGHTNESS POLICY'S CONTROL: a set, and the two settings.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct BrightnessPolicyClient {
	chan: u64,
}

impl BrightnessPolicyClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn set(&mut self, key: &str, target: &BrightnessTarget, allow_zero: bool) -> Option<Result<BrightnessSet, Error>> {
		unsafe { policy_set(self.chan, key, target, &allow_zero) }
	}

	#[inline(always)]
	pub fn settings(&mut self) -> Option<Result<BrightnessSettings, Error>> {
		unsafe { policy_settings(self.chan) }
	}

	#[inline(always)]
	pub fn set_automatic(&mut self, on: bool) -> Option<Result<(), Error>> {
		unsafe { policy_set_automatic(self.chan, &on) }
	}

	#[inline(always)]
	pub fn set_idle(&mut self, seconds: Option<u32>) -> Option<Result<(), Error>> {
		unsafe { policy_set_idle(self.chan, &seconds) }
	}
}
