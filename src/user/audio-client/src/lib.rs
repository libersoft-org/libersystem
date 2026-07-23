#![no_std]

use base_proto::generated::liber::base::v1::Error;
use wire::Buffer;

unsafe extern "Rust" {
	#[link_name = "liber_channel_liber_audio_audio_beep"]
	fn audio_beep(chan: u64, freq: &u16, millis: &u32) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_audio_audio_open_stream"]
	fn audio_open_stream(chan: u64, rate: &u32, channels: &u8) -> Option<Result<u64, Error>>;
	#[link_name = "liber_channel_liber_audio_pcm_stream_write"]
	fn pcm_stream_write(chan: u64, data: &Buffer) -> Option<Result<u32, Error>>;
	#[link_name = "liber_channel_liber_audio_pcm_stream_close"]
	fn pcm_stream_close(chan: u64) -> Option<Result<(), Error>>;
	#[link_name = "liber_channel_liber_audio_audio_admin_open_streams"]
	fn audio_admin_open_streams(chan: u64) -> Option<Result<u64, Error>>;
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
}
