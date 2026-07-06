// AudioService - headless PCM playback over the virtio-sound device.
//
// ServiceManager starts this program and hands it, over its bootstrap channel, the
// virtio-snd driver's control channel ("SND" - a 0 handle when no sound device is
// present, e.g. under test) and the channel its clients reach it on ("SERVE"). Over
// the service channel clients speak the generated `liber:system` Audio bindings:
// `beep(freq, millis)` plays a tone. The tone is synthesized here (a square wave,
// signed-16-bit, 2-channel, 48 kHz - the fixed format the driver expects) and
// streamed to the driver one PCM period at a time; the driver plays each period on
// the device's transmit queue.
//
// Sound is a capability, not ambient authority: a component reaches audio only
// through the channel this interface is served on. With no device the service still
// reports in and serves, answering `beep` with a not-found error.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::audio::{self, Service};
use proto::system::Error;
use rt::*;

// The PCM format the virtio-snd driver expects: 48 kHz, 2 channels, signed 16-bit,
// so 4 bytes per frame. One period is 512 frames = 2048 bytes (matching the driver).
const RATE: u32 = 48_000;
const PERIOD_FRAMES: u32 = 512;
const PERIOD_BYTES: usize = PERIOD_FRAMES as usize * 4;
// Tone amplitude (well below the i16 full scale, a comfortable listening level).
const AMP: i16 = 6_000;

// AudioService state: the virtio-snd driver's control channel (0 = no sound device).
struct Audio {
	snd: u64,
}

impl Service for Audio {
	// Play a tone of `freq` Hz for `millis` milliseconds. Without a sound device the
	// request fails cleanly; otherwise the synthesized PCM is streamed to the driver.
	fn beep(&mut self, freq: u16, millis: u32) -> Result<(), Error> {
		if self.snd == 0 {
			return Err(Error::NotFound);
		}
		unsafe { play_tone(self.snd, freq, millis) }
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	unsafe {
		// 1. receive the virtio-snd driver's control channel (its handle is 0 when no
		//    sound device is present) and the channel clients reach us on.
		let snd: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if len >= 3 && &buf[..3] == b"SND" => handle,
			_ => fail_bootstrap(bootstrap, b"snd", b"driver channel not delivered"),
		};
		let service: u64 = recv_tagged(bootstrap, &mut buf, b"SERVE").unwrap_or_else(|| fail_bootstrap(bootstrap, b"serve", b"missing serve channel"));

		// 2. report in (with or without a device), then serve beep() until the client
		//    side closes.
		send_blocking(bootstrap, b"AudioService: online", 0);
		let mut audio = Audio { snd };
		let mut request: [u8; 256] = [0u8; 256];
		let mut reply: [u8; 256] = [0u8; 256];
		serve_multi(service, &mut request, &mut reply, |_chan: u64, req: &[u8], handle: u64, out: &mut [u8], reply_handle: &mut u64| -> Option<usize> { audio::dispatch(&mut audio, req, handle, out, reply_handle) });
	}
	exit();
}

// Synthesize a square-wave tone (signed-16-bit, 2-channel, 48 kHz) and stream it to
// the virtio-snd driver one PCM period at a time, waiting for the driver to play each
// before sending the next, then an empty message to end the stream. Returns a Closed
// error if the driver channel drops mid-tone. Integer-only synthesis (no float / FPU,
// matching the kernel's soft-float userspace).
unsafe fn play_tone(snd: u64, freq: u16, millis: u32) -> Result<(), Error> {
	unsafe {
		let freq: u32 = (freq as u32).clamp(20, 20_000);
		let millis: u32 = millis.clamp(1, 5_000);
		let total_frames: u32 = (RATE as u64 * millis as u64 / 1000) as u32;
		// Frames per half square-wave period: the sample sign flips every `half` frames.
		let half: u32 = (RATE / (2 * freq)).max(1);

		let mut period: Vec<u8> = alloc::vec![0u8; PERIOD_BYTES];
		let mut frame: u32 = 0;
		while frame < total_frames {
			// fill one period; the final, short one is padded with silence.
			for f in 0..PERIOD_FRAMES {
				let g: u32 = frame + f;
				let sample: i16 = if g < total_frames {
					if (g / half) % 2 == 0 {
						AMP
					} else {
						-AMP
					}
				} else {
					0
				};
				let le: [u8; 2] = sample.to_le_bytes();
				let off: usize = f as usize * 4;
				period[off] = le[0];
				period[off + 1] = le[1];
				period[off + 2] = le[0];
				period[off + 3] = le[1];
			}
			if !send_blocking(snd, &period, 0) || !wait_ok(snd) {
				return Err(Error::Closed);
			}
			frame += PERIOD_FRAMES;
		}
		// end of stream: the driver stops and releases the PCM stream.
		if !send_blocking(snd, &[], 0) {
			return Err(Error::Closed);
		}
		wait_ok(snd);
		Ok(())
	}
}

// Wait for the driver's per-period acknowledgement. Returns false if the channel
// closed instead.
unsafe fn wait_ok(snd: u64) -> bool {
	unsafe {
		let mut buf: [u8; 8] = [0u8; 8];
		matches!(recv_blocking(snd, &mut buf), Received::Message { .. })
	}
}
