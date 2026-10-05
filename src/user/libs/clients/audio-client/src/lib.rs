#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use audio_proto::generated::liber::audio::v1::{AudioCounters, AudioDevice, AudioDirection, AudioStreamInfo};
use base_proto::generated::liber::base::v1::Error;
use wire::Buffer;

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_audio_audio_beep"]
	fn audio_beep(chan: u64, freq: &u16, millis: &u32) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_audio_audio_open_stream"]
	fn audio_open_stream(chan: u64, rate: &u32, channels: &u8) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_audio_audio_open_capture"]
	fn audio_open_capture(chan: u64, rate: &u32, channels: &u8) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_audio_pcm_capture_read"]
	fn pcm_capture_read(chan: u64) -> Option<Result<Vec<u8>, Error>>;
	#[link_name = "liber_channel_liber_audio_pcm_capture_close"]
	fn pcm_capture_close(chan: u64) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_audio_pcm_stream_write"]
	fn pcm_stream_write(chan: u64, data: &Buffer) -> Option<Result<u32, Error>>;
	#[link_name = "liber_channel_liber_audio_pcm_stream_close"]
	fn pcm_stream_close(chan: u64) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_audio_audio_admin_open_streams"]
	fn audio_admin_open_streams(chan: u64) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_audio_audio_admin_open_captures"]
	fn audio_admin_open_captures(chan: u64) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_audio_audio_control_devices"]
	fn audio_control_devices(chan: u64) -> Option<Vec<AudioDevice>>;
	#[link_name = "liber_channel_liber_audio_audio_control_set_default"]
	fn audio_control_set_default(chan: u64, device: &u32, direction: &AudioDirection) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_audio_audio_control_set_volume"]
	fn audio_control_set_volume(chan: u64, device: &u32, volume: &u8) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_audio_audio_control_streams"]
	fn audio_control_streams(chan: u64) -> Option<Vec<AudioStreamInfo>>;
	#[link_name = "liber_channel_liber_audio_audio_control_counters"]
	fn audio_control_counters(chan: u64) -> Option<AudioCounters>;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct AudioClient {
	chan: u64,
}

impl AudioClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn beep(&mut self, freq: &u16, millis: &u32) -> Option<Result<(), Error>> {
		unsafe { audio_beep(self.chan, freq, millis) }
	}

	#[inline(always)]
	pub fn open_stream(&mut self, rate: &u32, channels: &u8) -> Option<Result<u64, Error>> {
		unsafe { audio_open_stream(self.chan, rate, channels) }
	}

	#[inline(always)]
	pub fn open_capture(&mut self, rate: &u32, channels: &u8) -> Option<Result<u64, Error>> {
		unsafe { audio_open_capture(self.chan, rate, channels) }
	}
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PcmCaptureClient {
	chan: u64,
}

impl PcmCaptureClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn read(&mut self) -> Option<Result<Vec<u8>, Error>> {
		unsafe { pcm_capture_read(self.chan) }
	}

	#[inline(always)]
	pub fn close(&mut self) -> Option<Result<(), Error>> {
		unsafe { pcm_capture_close(self.chan) }
	}
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PcmStreamClient {
	chan: u64,
}

impl PcmStreamClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn write(&mut self, data: &Buffer) -> Option<Result<u32, Error>> {
		unsafe { pcm_stream_write(self.chan, data) }
	}

	#[inline(always)]
	pub fn close(&mut self) -> Option<Result<(), Error>> {
		unsafe { pcm_stream_close(self.chan) }
	}
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct AudioAdminClient {
	chan: u64,
}

impl AudioAdminClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn open_streams(&mut self) -> Option<Result<u64, Error>> {
		unsafe { audio_admin_open_streams(self.chan) }
	}

	#[inline(always)]
	pub fn open_captures(&mut self) -> Option<Result<u64, Error>> {
		unsafe { audio_admin_open_captures(self.chan) }
	}
}

/// THE OPERATOR'S AUTHORITY ON AUDIOSERVICE: the inventory, the defaults, the levels and where the streams play.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct AudioControlClient {
	chan: u64,
}

impl AudioControlClient {
	#[inline(always)]
	pub const fn new(chan: u64) -> Self {
		Self { chan }
	}

	#[inline(always)]
	pub fn devices(&mut self) -> Option<Vec<AudioDevice>> {
		unsafe { audio_control_devices(self.chan) }
	}

	#[inline(always)]
	pub fn set_default(&mut self, device: &u32, direction: &AudioDirection) -> Option<Result<(), Error>> {
		unsafe { audio_control_set_default(self.chan, device, direction) }
	}

	#[inline(always)]
	pub fn set_volume(&mut self, device: &u32, volume: &u8) -> Option<Result<(), Error>> {
		unsafe { audio_control_set_volume(self.chan, device, volume) }
	}

	#[inline(always)]
	pub fn streams(&mut self) -> Option<Vec<AudioStreamInfo>> {
		unsafe { audio_control_streams(self.chan) }
	}

	#[inline(always)]
	pub fn counters(&mut self) -> Option<AudioCounters> {
		unsafe { audio_control_counters(self.chan) }
	}
}
