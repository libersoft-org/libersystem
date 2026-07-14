// Event-driven AudioService engine: typed PCM streams, bounded source queues,
// nearest-neighbor rate conversion, saturating software mixing, and one-period
// virtio-snd backpressure without blocking other clients on the driver ACK.

extern crate alloc;

use alloc::vec::Vec;
use proto::codec::Buffer;
use proto::system::Error;
use proto::system::audio::{self, Service as AudioService};
use proto::system::pcm_stream::{self, Service as PcmService};
use rt::*;

const OUTPUT_RATE: u32 = 48_000;
const PERIOD_FRAMES: usize = 512;
const PERIOD_BYTES: usize = PERIOD_FRAMES * 4;
const MIN_RATE: u32 = 8_000;
const MAX_QUEUED_FRAMES: usize = 4_096;
const MAX_STREAMS: usize = 16;
const MAX_TONES: usize = 8;
const AMP: i16 = 6_000;
const REQUEST_MAX: usize = 128;
const REPLY_MAX: usize = 128;

struct PendingWrite {
	request: Vec<u8>,
	handle: u64,
}

struct Stream {
	chan: u64,
	rate: u32,
	channels: u8,
	samples: Vec<i16>,
	read_frame: usize,
	phase: u32,
	closing: bool,
	pending: Option<PendingWrite>,
}

impl Stream {
	fn queued_frames(&self) -> usize {
		self.samples.len() / self.channels as usize - self.read_frame
	}

	fn capacity(&self) -> usize {
		MAX_QUEUED_FRAMES.saturating_sub(self.queued_frames())
	}

	fn next_frame(&mut self) -> Option<(i16, i16)> {
		if self.read_frame >= self.samples.len() / self.channels as usize {
			return None;
		}
		let offset: usize = self.read_frame * self.channels as usize;
		let left: i16 = self.samples[offset];
		let right: i16 = if self.channels == 2 { self.samples[offset + 1] } else { left };
		self.phase += self.rate;
		while self.phase >= OUTPUT_RATE {
			self.phase -= OUTPUT_RATE;
			self.read_frame += 1;
		}
		Some((left, right))
	}

	fn compact(&mut self) {
		let consumed: usize = self.read_frame * self.channels as usize;
		if consumed != 0 && (self.read_frame >= self.samples.len() / self.channels as usize || consumed >= 2_048) {
			self.samples.drain(..consumed);
			self.read_frame = 0;
		}
	}

	fn write_buffer(&mut self, data: Buffer) -> Result<u32, Error> {
		let handle: u64 = data.handle;
		let result: Result<u32, Error> = (|| {
			if self.closing || handle == 0 {
				return Err(Error::Invalid);
			}
			let frame_bytes: u64 = self.channels as u64 * 2;
			if data.len == 0 || data.len % frame_bytes != 0 {
				return Err(Error::Invalid);
			}
			let info: ObjectInfo = unsafe { object_info(handle) }.ok_or(Error::Invalid)?;
			if data.len > info.size {
				return Err(Error::Invalid);
			}
			let requested: usize = usize::try_from(data.len / frame_bytes).map_err(|_| Error::Invalid)?;
			let accepted: usize = requested.min(self.capacity());
			if accepted == 0 {
				return Err(Error::Again);
			}
			let mapped: u64 = unsafe { map_object(handle) }.ok_or(Error::Invalid)?;
			let sample_count: usize = accepted * self.channels as usize;
			let bytes: &[u8] = unsafe { core::slice::from_raw_parts(mapped as *const u8, sample_count * 2) };
			self.samples.reserve(sample_count);
			for sample in bytes.chunks_exact(2) {
				self.samples.push(i16::from_le_bytes([sample[0], sample[1]]));
			}
			unsafe { unmap_object(handle) };
			Ok(accepted as u32)
		})();
		unsafe { close(handle) };
		result
	}
}

struct Tone {
	remaining: u32,
	frame: u32,
	half_period: u32,
}

impl Tone {
	fn next_frame(&mut self) -> Option<(i16, i16)> {
		if self.remaining == 0 {
			return None;
		}
		let sample: i16 = if (self.frame / self.half_period) % 2 == 0 { AMP } else { -AMP };
		self.frame += 1;
		self.remaining -= 1;
		Some((sample, sample))
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DriverPending {
	None,
	Period,
	Stop,
}

struct Audio {
	snd: u64,
	streams: Vec<Stream>,
	tones: Vec<Tone>,
	driver_pending: DriverPending,
	driver_running: bool,
	period: Vec<u8>,
}

impl Audio {
	fn new(snd: u64) -> Audio {
		Audio { snd, streams: Vec::new(), tones: Vec::new(), driver_pending: DriverPending::None, driver_running: false, period: alloc::vec![0; PERIOD_BYTES] }
	}

	fn has_audio(&self) -> bool {
		self.streams.iter().any(|stream| stream.queued_frames() != 0) || self.tones.iter().any(|tone| tone.remaining != 0)
	}

	fn fill_period(&mut self) {
		for frame in 0..PERIOD_FRAMES {
			let mut left: i32 = 0;
			let mut right: i32 = 0;
			for stream in &mut self.streams {
				if let Some((l, r)) = stream.next_frame() {
					left += l as i32;
					right += r as i32;
				}
			}
			for tone in &mut self.tones {
				if let Some((l, r)) = tone.next_frame() {
					left += l as i32;
					right += r as i32;
				}
			}
			let left: [u8; 2] = (left.clamp(i16::MIN as i32, i16::MAX as i32) as i16).to_le_bytes();
			let right: [u8; 2] = (right.clamp(i16::MIN as i32, i16::MAX as i32) as i16).to_le_bytes();
			let offset: usize = frame * 4;
			self.period[offset..offset + 2].copy_from_slice(&left);
			self.period[offset + 2..offset + 4].copy_from_slice(&right);
		}
		for stream in &mut self.streams {
			stream.compact();
		}
		self.tones.retain(|tone| tone.remaining != 0);
	}

	fn pump(&mut self) {
		if self.snd == 0 || self.driver_pending != DriverPending::None {
			return;
		}
		if self.has_audio() {
			self.fill_period();
			if unsafe { send_blocking(self.snd, &self.period, 0) } {
				self.driver_pending = DriverPending::Period;
				self.driver_running = true;
			} else {
				self.driver_failed();
			}
		} else if self.driver_running {
			if unsafe { send_blocking(self.snd, &[], 0) } {
				self.driver_pending = DriverPending::Stop;
			} else {
				self.driver_failed();
			}
		}
	}

	fn driver_ready(&mut self, handle: u64) {
		if handle != 0 {
			unsafe { close(handle) };
		}
		if self.driver_pending == DriverPending::Stop {
			self.driver_running = false;
		}
		self.driver_pending = DriverPending::None;
	}

	fn driver_failed(&mut self) {
		if self.snd != 0 {
			unsafe { close(self.snd) };
		}
		self.snd = 0;
		self.driver_pending = DriverPending::None;
		self.driver_running = false;
		self.tones.clear();
		while let Some(mut stream) = self.streams.pop() {
			if let Some(pending) = stream.pending.take() {
				unsafe { close(pending.handle) };
			}
			if stream.chan != 0 {
				unsafe { close(stream.chan) };
			}
		}
	}

	fn remove_stream(&mut self, index: usize) {
		let mut stream: Stream = self.streams.swap_remove(index);
		if let Some(pending) = stream.pending.take() {
			unsafe { close(pending.handle) };
		}
		if stream.chan != 0 {
			unsafe { close(stream.chan) };
		}
	}

	fn cleanup_drained(&mut self) {
		let mut index: usize = 0;
		while index < self.streams.len() {
			if self.streams[index].closing && self.streams[index].queued_frames() == 0 && self.streams[index].pending.is_none() {
				self.remove_stream(index);
			} else {
				index += 1;
			}
		}
	}

	fn dispatch_stream(&mut self, index: usize, request: &[u8], mut request_handle: u64) {
		let chan: u64 = self.streams[index].chan;
		let mut reply: [u8; REPLY_MAX] = [0; REPLY_MAX];
		let mut reply_handle: u64 = 0;
		let mut call = StreamCall { stream: &mut self.streams[index] };
		if let Some(len) = pcm_stream::dispatch(&mut call, request, &mut request_handle, &mut reply, &mut reply_handle) {
			if !unsafe { send_blocking(chan, &reply[..len], reply_handle) } && reply_handle != 0 {
				unsafe { close(reply_handle) };
			}
		} else if reply_handle != 0 {
			unsafe { close(reply_handle) };
		}
		if request_handle != 0 {
			unsafe { close(request_handle) };
		}
	}

	fn service_pending_writes(&mut self) {
		let mut index: usize = 0;
		while index < self.streams.len() {
			if self.streams[index].capacity() != 0
				&& let Some(pending) = self.streams[index].pending.take()
			{
				self.dispatch_stream(index, &pending.request, pending.handle);
			}
			index += 1;
		}
	}

	fn poll_streams(&mut self) {
		let mut request: [u8; REQUEST_MAX] = [0; REQUEST_MAX];
		let mut index: usize = 0;
		while index < self.streams.len() {
			if self.streams[index].chan == 0 || self.streams[index].pending.is_some() {
				index += 1;
				continue;
			}
			let chan: u64 = self.streams[index].chan;
			match unsafe { try_recv(chan, &mut request) } {
				Polled::Message { len, handle } => {
					let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
					if op == pcm_stream::OP_WRITE && self.streams[index].capacity() == 0 && handle != 0 {
						self.streams[index].pending = Some(PendingWrite { request: request[..len].to_vec(), handle });
					} else {
						self.dispatch_stream(index, &request[..len], handle);
					}
					index += 1;
				}
				Polled::Empty => index += 1,
				Polled::Closed => {
					if self.streams[index].closing {
						unsafe { close(chan) };
						self.streams[index].chan = 0;
						index += 1;
					} else {
						self.remove_stream(index);
					}
				}
			}
		}
	}
}

struct RootCall<'a> {
	audio: &'a mut Audio,
}

impl AudioService for RootCall<'_> {
	fn beep(&mut self, freq: u16, millis: u32) -> Result<(), Error> {
		if self.audio.snd == 0 {
			return Err(Error::NotFound);
		}
		if self.audio.tones.len() >= MAX_TONES {
			return Err(Error::Again);
		}
		let freq: u32 = (freq as u32).clamp(20, 20_000);
		let millis: u32 = millis.clamp(1, 5_000);
		let remaining: u32 = ((OUTPUT_RATE as u64 * millis as u64) / 1_000).max(1) as u32;
		self.audio.tones.push(Tone { remaining, frame: 0, half_period: (OUTPUT_RATE / (2 * freq)).max(1) });
		Ok(())
	}

	fn open_stream(&mut self, rate: u32, channels: u8) -> Result<u64, Error> {
		if self.audio.snd == 0 {
			return Err(Error::NotFound);
		}
		if !(MIN_RATE..=OUTPUT_RATE).contains(&rate) || !(1..=2).contains(&channels) {
			return Err(Error::Invalid);
		}
		if self.audio.streams.len() >= MAX_STREAMS {
			return Err(Error::Again);
		}
		let (server, client): (u64, u64) = unsafe { channel() }.ok_or(Error::Again)?;
		self.audio.streams.push(Stream { chan: server, rate, channels, samples: Vec::new(), read_frame: 0, phase: 0, closing: false, pending: None });
		Ok(client)
	}
}

struct StreamCall<'a> {
	stream: &'a mut Stream,
}

impl PcmService for StreamCall<'_> {
	fn write(&mut self, data: Buffer) -> Result<u32, Error> {
		self.stream.write_buffer(data)
	}

	fn close(&mut self) -> Result<(), Error> {
		self.stream.closing = true;
		Ok(())
	}
}

pub fn run(bootstrap: u64) -> ! {
	let mut bootstrap_buf: [u8; 256] = [0; 256];
	unsafe {
		let snd: u64 = match recv_blocking(bootstrap, &mut bootstrap_buf) {
			Received::Message { len, handle } if len >= 3 && &bootstrap_buf[..3] == b"SND" => handle,
			_ => fail_bootstrap(bootstrap, b"snd", b"driver channel not delivered"),
		};
		let root: u64 = recv_tagged(bootstrap, &mut bootstrap_buf, b"SERVE").unwrap_or_else(|| fail_bootstrap(bootstrap, b"serve", b"missing serve channel"));
		send_blocking(bootstrap, b"AudioService: online", 0);
		serve(root, Audio::new(snd));
	}
}

unsafe fn serve(root: u64, mut state: Audio) -> ! {
	unsafe {
		let mut clients: Vec<u64> = alloc::vec![root];
		let mut request: [u8; REQUEST_MAX] = [0; REQUEST_MAX];
		let mut reply: [u8; REPLY_MAX] = [0; REPLY_MAX];
		loop {
			state.poll_streams();
			state.service_pending_writes();
			state.cleanup_drained();
			state.pump();

			let driver_first: bool = state.snd != 0 && state.driver_pending != DriverPending::None;
			let mut waits: Vec<u64> = Vec::with_capacity(driver_first as usize + clients.len() + state.streams.len());
			if driver_first {
				waits.push(state.snd);
			}
			waits.extend_from_slice(&clients);
			for stream in &state.streams {
				if stream.chan != 0 {
					waits.push(stream.chan);
				}
			}
			let ready: i64 = wait_any(&waits, 0);
			if ready < 0 {
				continue;
			}
			let ready_chan: u64 = waits[ready as usize];
			if driver_first && ready_chan == state.snd {
				match recv_blocking(state.snd, &mut request) {
					Received::Message { handle, .. } => state.driver_ready(handle),
					Received::Closed => state.driver_failed(),
				}
				continue;
			}
			if let Some(index) = clients.iter().position(|client| *client == ready_chan) {
				match recv_blocking(ready_chan, &mut request) {
					Received::Message { len, mut handle } => {
						let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
						if op == HEARTBEAT_OP {
							send_blocking(ready_chan, b"PONG", 0);
						} else if op == CONNECT_OP {
							if let Some((server, client)) = channel() {
								clients.push(server);
								send_blocking(ready_chan, &[], client);
							}
						} else {
							let mut reply_handle: u64 = 0;
							let mut call = RootCall { audio: &mut state };
							if let Some(reply_len) = audio::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle) {
								if !send_blocking(ready_chan, &reply[..reply_len], reply_handle) && reply_handle != 0 {
									close(reply_handle);
								}
							} else if reply_handle != 0 {
								close(reply_handle);
							}
						}
						if handle != 0 {
							close(handle);
						}
					}
					Received::Closed => {
						if index == 0 {
							exit();
						}
						close(ready_chan);
						clients.swap_remove(index);
					}
				}
				continue;
			}
			let Some(index) = state.streams.iter().position(|stream| stream.chan == ready_chan) else { continue };
			if state.streams[index].pending.is_some() {
				match recv_blocking(ready_chan, &mut request) {
					Received::Message { handle, .. } => {
						if handle != 0 {
							close(handle);
						}
						state.remove_stream(index);
					}
					Received::Closed => state.remove_stream(index),
				}
				continue;
			}
			match recv_blocking(ready_chan, &mut request) {
				Received::Message { len, handle } => {
					let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
					if op == pcm_stream::OP_WRITE && state.streams[index].capacity() == 0 && handle != 0 {
						state.streams[index].pending = Some(PendingWrite { request: request[..len].to_vec(), handle });
					} else {
						state.dispatch_stream(index, &request[..len], handle);
					}
				}
				Received::Closed => {
					if state.streams[index].closing {
						close(ready_chan);
						state.streams[index].chan = 0;
					} else {
						state.remove_stream(index);
					}
				}
			}
		}
	}
}
