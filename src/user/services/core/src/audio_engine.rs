// Event-driven AudioService engine: typed PCM streams, bounded source queues,
// nearest-neighbor rate conversion, saturating software mixing, and one-period
// virtio-snd backpressure without blocking other clients on the driver ACK.
//
// CAPTURE RUNS THROUGH THE SAME ONE-OP-AT-A-TIME DRIVER. The driver is synchronous - it blocks on
// the device's interrupt for the period it is playing or filling - so this engine may have exactly
// one request outstanding to it, and `DriverPending` is what says which. A `read` that arrives with
// no period ready is DEFERRED the way a `write` with no capacity is: the raw request is kept, the
// period is asked for, and the request is re-dispatched when the answer lands. Nothing about
// capture blocks an unrelated client's RPC.

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use pcm::encode::{Remix, Resample};
use pcm::{Format, OUTPUT_RATE};
use proto::codec::Buffer;
use proto::system::Error;
use proto::system::audio::{self, Service as AudioService};
use proto::system::audio_admin::{self, Service as AdminService};
use proto::system::pcm_capture::{self, Service as PcmCaptureService};
use proto::system::pcm_stream::{self, Service as PcmService};
use proto::system::{ProviderInfo, ProviderKind, provider_catalogue};
use rt::*;

const PERIOD_FRAMES: usize = 512;
const PERIOD_BYTES: usize = PERIOD_FRAMES * 4;
const MAX_QUEUED_FRAMES: usize = 4_096;
const MAX_STREAMS: usize = 16;
// One recorder at a time, and it is a hardware limit rather than a policy: there is one input
// stream on the device and its periods cannot be handed to two readers without deciding which of
// them the audio belongs to.
const MAX_CAPTURES: usize = 1;
// The driver's one-byte commands. Declared here as well as there because the two ends of a protocol
// with three message shapes should each state what they send - see the block beside `CMD_CAPTURE`
// in driver.virtio-snd for the whole of it.
const CMD_CAPTURE: u8 = 1;
const CMD_CAPTURE_STOP: u8 = 2;
const MAX_TONES: usize = 8;
const AMP: i16 = 6_000;
const REQUEST_MAX: usize = 128;
// A capture `read` answers with a whole period inline, so the reply buffer holds one: 512 stereo
// frames is PERIOD_BYTES, and everything else this service answers is a few bytes.
const REPLY_MAX: usize = PERIOD_BYTES + 128;

struct PendingWrite {
	request: Vec<u8>,
	// The WHOLE capability list the deferred write carried, not its first handle.
	caps: proto::codec::Handles,
}

struct Stream {
	chan: u64,
	format: Format,
	samples: Vec<i16>,
	read_frame: usize,
	phase: u32,
	closing: bool,
	pending: Option<PendingWrite>,
}

impl Stream {
	fn queued_frames(&self) -> usize {
		self.samples.len() / self.format.channels() as usize - self.read_frame
	}

	fn capacity(&self) -> usize {
		MAX_QUEUED_FRAMES.saturating_sub(self.queued_frames())
	}

	fn next_frame(&mut self) -> Option<(i16, i16)> {
		if self.read_frame >= self.samples.len() / self.format.channels() as usize {
			return None;
		}
		let frame = self.format.stereo_frame(&self.samples, self.read_frame)?;
		self.format.advance(&mut self.phase, &mut self.read_frame);
		Some(frame)
	}

	fn compact(&mut self) {
		let consumed: usize = self.read_frame * self.format.channels() as usize;
		if consumed != 0 && (self.read_frame >= self.samples.len() / self.format.channels() as usize || consumed >= 2_048) {
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
			let requested: usize = self.format.frames_in(data.len).ok_or(Error::Invalid)?;
			let info: ObjectInfo = unsafe { object_info(handle) }.ok_or(Error::Invalid)?;
			if data.len > info.size {
				return Err(Error::Invalid);
			}
			let accepted: usize = requested.min(self.capacity());
			if accepted == 0 {
				return Err(Error::Again);
			}
			let mapped: u64 = unsafe { map_object(handle) }.ok_or(Error::Invalid)?;
			let byte_count: usize = accepted * self.format.frame_bytes() as usize;
			let bytes: &[u8] = unsafe { core::slice::from_raw_parts(mapped as *const u8, byte_count) };
			self.format.append_i16_le(bytes, accepted, &mut self.samples).ok_or(Error::Invalid)?;
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
	// A capture period was asked for on behalf of the recorder at this index.
	Capture(usize),
	// The capture stream was asked to stop. Nobody is waiting for the answer, but the driver owes
	// one and it has to be read off the channel before the next request.
	CaptureStop,
}

// One recorder: the channel it is served on, the conversion from the device's 48 kHz stereo down to
// what it asked for, and the two halves of the deferral - the request that is waiting for a period
// and the period that is waiting for a request.
struct Capture {
	chan: u64,
	remix: Remix,
	resample: Resample,
	// The raw `read` request kept while the driver fills a period. Re-dispatched when it lands.
	pending: Option<Vec<u8>>,
	// The converted period an answered `read` takes.
	ready: Option<Vec<u8>>,
	// The device has no input stream, or refused to start one. The next `read` says so and every
	// one after it - a recorder is not left waiting for periods that will never come.
	unavailable: bool,
	closing: bool,
}

struct Audio {
	snd: u64,
	streams: Vec<Stream>,
	captures: Vec<Capture>,
	tones: Vec<Tone>,
	driver_pending: DriverPending,
	driver_running: bool,
	capture_running: bool,
	period: Vec<u8>,
}

// What a connection may ask for. `Full` is the service channel ServiceManager holds; the other two
// are what `audio-admin` mints for a launcher, one direction each.
//
// CAPTURE IS NOT A SUBSET OF PLAYBACK. A program granted `audio-stream` may make a sound and may
// not record; a program granted `audio-capture` may record and may not make a sound. Two scopes
// rather than one with a flag, because the launcher's manifest names one or the other.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
	Full,
	StreamOnly,
	CaptureOnly,
}

struct Client {
	chan: u64,
	scope: Scope,
}

impl Audio {
	fn new(snd: u64) -> Audio {
		Audio { snd, streams: Vec::new(), captures: Vec::new(), tones: Vec::new(), driver_pending: DriverPending::None, driver_running: false, capture_running: false, period: alloc::vec![0; PERIOD_BYTES] }
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

	// Ask the driver for one capture period on behalf of the first recorder that is waiting for one.
	// Returns whether a request went out, so `pump` knows the driver is busy.
	//
	// CAPTURE IS ASKED FOR BEFORE PLAYBACK IS PUMPED. The device fills a period on its own clock and
	// a recorder that is late loses audio; a playback period that is late is a period the mixer
	// simply sends on the next turn, because the device's own ring holds several.
	fn pump_capture(&mut self) -> bool {
		if self.snd == 0 || self.driver_pending != DriverPending::None {
			return false;
		}
		let Some(index) = self.captures.iter().position(|capture| capture.pending.is_some() && capture.ready.is_none() && !capture.unavailable) else {
			return false;
		};
		if unsafe { send_blocking(self.snd, &[CMD_CAPTURE], 0) } {
			self.driver_pending = DriverPending::Capture(index);
			self.capture_running = true;
			true
		} else {
			self.driver_failed();
			false
		}
	}

	// One captured period from the driver, converted into the recorder's format. An EMPTY message is
	// the driver saying this device cannot capture - see `serve_command` in driver.virtio-snd.
	fn capture_ready(&mut self, index: usize, period: &[u8]) {
		self.driver_pending = DriverPending::None;
		let Some(capture) = self.captures.get_mut(index) else { return };
		if period.is_empty() {
			capture.unavailable = true;
			return;
		}
		// The device's period is 48 kHz stereo signed-16-bit little-endian. Mixed down to the
		// recorder's channel count first, then resampled: the resampler is built for that channel
		// count, so remixing after it would interpolate across channels that are not there yet.
		let mut stereo: Vec<i16> = Vec::with_capacity(period.len() / 2);
		for frame in period.chunks_exact(2) {
			stereo.push(i16::from_le_bytes([frame[0], frame[1]]));
		}
		let mut remixed: Vec<i16> = Vec::new();
		if capture.remix.apply(&stereo, &mut remixed).is_none() {
			capture.unavailable = true;
			return;
		}
		let mut converted: Vec<i16> = Vec::new();
		if capture.resample.push(&remixed, &mut converted).is_none() {
			capture.unavailable = true;
			return;
		}
		let mut bytes: Vec<u8> = Vec::with_capacity(converted.len() * 2);
		for sample in converted {
			bytes.extend_from_slice(&sample.to_le_bytes());
		}
		capture.ready = Some(bytes);
	}

	// Tell the driver the capture stream is over, when the last recorder has gone.
	fn stop_capture(&mut self) {
		if self.snd == 0 || self.driver_pending != DriverPending::None || !self.captures.is_empty() {
			return;
		}
		if !self.capture_running {
			return;
		}
		if unsafe { send_blocking(self.snd, &[CMD_CAPTURE_STOP], 0) } {
			self.driver_pending = DriverPending::CaptureStop;
			self.capture_running = false;
		} else {
			self.driver_failed();
		}
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
		self.capture_running = false;
		self.tones.clear();
		// A recorder outlives the device only as a stream that says so: the channel stays open and
		// every `read` on it answers not-found, so a recording in progress ends with an error rather
		// than with a file that stops mid-sentence and looks complete.
		for capture in &mut self.captures {
			capture.unavailable = true;
			capture.ready = None;
		}
		while let Some(mut stream) = self.streams.pop() {
			if let Some(pending) = stream.pending.take() {
				for &handle in pending.caps.as_slice() {
					unsafe { close(handle) };
				}
			}
			if stream.chan != 0 {
				unsafe { close(stream.chan) };
			}
		}
	}

	fn remove_stream(&mut self, index: usize) {
		let mut stream: Stream = self.streams.swap_remove(index);
		if let Some(pending) = stream.pending.take() {
			for &handle in pending.caps.as_slice() {
				unsafe { close(handle) };
			}
		}
		if stream.chan != 0 {
			unsafe { close(stream.chan) };
		}
	}

	fn remove_capture(&mut self, index: usize) {
		let capture: Capture = self.captures.swap_remove(index);
		if capture.chan != 0 {
			unsafe { close(capture.chan) };
		}
	}

	// Re-dispatch a deferred `read` once its period has landed - or once the stream is known to be
	// unavailable, which is an answer too. Exactly the shape `service_pending_writes` has for the
	// playback direction.
	fn service_pending_reads(&mut self) {
		let mut index: usize = 0;
		while index < self.captures.len() {
			if (self.captures[index].ready.is_some() || self.captures[index].unavailable)
				&& let Some(request) = self.captures[index].pending.take()
			{
				self.dispatch_capture(index, &request);
			}
			index += 1;
		}
	}

	fn dispatch_capture(&mut self, index: usize, request: &[u8]) {
		let chan: u64 = self.captures[index].chan;
		let mut reply: [u8; REPLY_MAX] = [0; REPLY_MAX];
		let mut reply_handle = proto::codec::Handles::new();
		let mut request_handle = proto::codec::Handles::new();
		let mut call = CaptureCall { capture: &mut self.captures[index] };
		if let Some(len) = pcm_capture::dispatch(&mut call, request, &mut request_handle, &mut reply, &mut reply_handle) {
			if !unsafe { send_caps_blocking(chan, &reply[..len], reply_handle.as_slice()) } {
				for &leftover in reply_handle.as_slice() {
					unsafe { close(leftover) };
				}
			}
		} else {
			for &leftover in reply_handle.as_slice() {
				unsafe { close(leftover) };
			}
		}
		for &unclaimed in request_handle.as_slice() {
			unsafe { close(unclaimed) };
		}
	}

	fn poll_captures(&mut self) {
		let mut request: [u8; REQUEST_MAX] = [0; REQUEST_MAX];
		let mut index: usize = 0;
		while index < self.captures.len() {
			if self.captures[index].chan == 0 || self.captures[index].pending.is_some() {
				index += 1;
				continue;
			}
			let chan: u64 = self.captures[index].chan;
			match unsafe { try_recv_caps(chan, &mut request) } {
				PolledCaps::Message { len, handles } => {
					for &unclaimed in handles.as_slice() {
						unsafe { close(unclaimed) };
					}
					self.take_capture_request(index, &request[..len]);
					index += 1;
				}
				PolledCaps::Empty => index += 1,
				PolledCaps::Closed => self.remove_capture(index),
			}
		}
	}

	// A `read` with nothing ready is DEFERRED rather than refused: the request is kept, a period is
	// asked for, and the reply goes out when it lands. Anything else is answered now.
	fn take_capture_request(&mut self, index: usize, request: &[u8]) {
		let op: u16 = if request.len() >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
		if op == pcm_capture::OP_READ && self.captures[index].ready.is_none() && !self.captures[index].unavailable {
			self.captures[index].pending = Some(request.to_vec());
		} else {
			self.dispatch_capture(index, request);
		}
	}

	fn cleanup_drained(&mut self) {
		let mut index: usize = 0;
		while index < self.captures.len() {
			if self.captures[index].closing && self.captures[index].pending.is_none() {
				self.remove_capture(index);
			} else {
				index += 1;
			}
		}
		index = 0;
		while index < self.streams.len() {
			if self.streams[index].closing && self.streams[index].queued_frames() == 0 && self.streams[index].pending.is_none() {
				self.remove_stream(index);
			} else {
				index += 1;
			}
		}
	}

	// Takes the whole capability list rather than one handle - see `rt::recv_caps_blocking`.
	fn dispatch_stream(&mut self, index: usize, request: &[u8], caps: proto::codec::Handles) {
		let chan: u64 = self.streams[index].chan;
		let mut reply: [u8; REPLY_MAX] = [0; REPLY_MAX];
		let mut reply_handle = proto::codec::Handles::new();
		let mut request_handle = caps;
		let mut call = StreamCall { stream: &mut self.streams[index] };
		if let Some(len) = pcm_stream::dispatch(&mut call, request, &mut request_handle, &mut reply, &mut reply_handle) {
			if !unsafe { send_caps_blocking(chan, &reply[..len], reply_handle.as_slice()) } {
				for &leftover in reply_handle.as_slice() {
					unsafe { close(leftover) };
				}
			}
		} else {
			for &leftover in reply_handle.as_slice() {
				unsafe { close(leftover) };
			}
		}
		for &unclaimed in request_handle.as_slice() {
			unsafe { close(unclaimed) };
		}
	}

	fn service_pending_writes(&mut self) {
		let mut index: usize = 0;
		while index < self.streams.len() {
			if self.streams[index].capacity() != 0
				&& let Some(pending) = self.streams[index].pending.take()
			{
				self.dispatch_stream(index, &pending.request, pending.caps);
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
			match unsafe { try_recv_caps(chan, &mut request) } {
				PolledCaps::Message { len, handles } => {
					let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
					if op == pcm_stream::OP_WRITE && self.streams[index].capacity() == 0 && !handles.is_empty() {
						self.streams[index].pending = Some(PendingWrite { request: request[..len].to_vec(), caps: handles });
					} else {
						self.dispatch_stream(index, &request[..len], handles);
					}
					index += 1;
				}
				PolledCaps::Empty => index += 1,
				PolledCaps::Closed => {
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
	scope: Scope,
}

impl AudioService for RootCall<'_> {
	fn beep(&mut self, freq: u16, millis: u32) -> Result<(), Error> {
		// Neither restricted scope may beep - see `audio-admin` in the IDL.
		if self.scope != Scope::Full {
			return Err(Error::Denied);
		}
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
		if self.scope == Scope::CaptureOnly {
			return Err(Error::Denied);
		}
		if self.audio.snd == 0 {
			return Err(Error::NotFound);
		}
		let format: Format = Format::new(rate, channels).ok_or(Error::Invalid)?;
		if self.audio.streams.len() >= MAX_STREAMS {
			return Err(Error::Again);
		}
		let (server, client): (u64, u64) = unsafe { channel() }.ok_or(Error::Again)?;
		self.audio.streams.push(Stream { chan: server, format, samples: Vec::new(), read_frame: 0, phase: 0, closing: false, pending: None });
		Ok(client)
	}

	// WHETHER THE DEVICE CAN CAPTURE IS NOT KNOWN HERE, and asking would mean a blocking round trip
	// to the driver inside a dispatch - which is the one thing this engine does not do. A machine
	// with a sound device but no input stream therefore opens the stream and answers not-found on
	// the first `read`, which is where a recorder finds out either way: it has to read before it
	// knows there is anything to record.
	fn open_capture(&mut self, rate: u32, channels: u8) -> Result<u64, Error> {
		if self.scope == Scope::StreamOnly {
			return Err(Error::Denied);
		}
		if self.audio.snd == 0 {
			return Err(Error::NotFound);
		}
		Format::new(rate, channels).ok_or(Error::Invalid)?;
		let remix: Remix = Remix::new(2, channels).ok_or(Error::Invalid)?;
		let resample: Resample = Resample::new(OUTPUT_RATE, rate, channels).ok_or(Error::Invalid)?;
		if self.audio.captures.len() >= MAX_CAPTURES {
			return Err(Error::Again);
		}
		let (server, client): (u64, u64) = unsafe { channel() }.ok_or(Error::Again)?;
		self.audio.captures.push(Capture { chan: server, remix, resample, pending: None, ready: None, unavailable: false, closing: false });
		Ok(client)
	}
}

struct AdminCall<'a> {
	clients: &'a mut Vec<Client>,
}

impl AdminService for AdminCall<'_> {
	fn open_streams(&mut self) -> Result<u64, Error> {
		let (server, client): (u64, u64) = unsafe { channel() }.ok_or(Error::Again)?;
		self.clients.push(Client { chan: server, scope: Scope::StreamOnly });
		Ok(client)
	}

	fn open_captures(&mut self) -> Result<u64, Error> {
		let (server, client): (u64, u64) = unsafe { channel() }.ok_or(Error::Again)?;
		self.clients.push(Client { chan: server, scope: Scope::CaptureOnly });
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

struct CaptureCall<'a> {
	capture: &'a mut Capture,
}

impl PcmCaptureService for CaptureCall<'_> {
	// One period, or the reason there is not one. This only ever runs with an answer available:
	// `take_capture_request` defers a `read` that has neither until the driver has answered.
	fn read(&mut self) -> Result<Vec<u8>, Error> {
		if let Some(period) = self.capture.ready.take() {
			return Ok(period);
		}
		if self.capture.unavailable {
			return Err(Error::NotFound);
		}
		Err(Error::Again)
	}

	fn close(&mut self) -> Result<(), Error> {
		self.capture.closing = true;
		Ok(())
	}
}

pub fn run(bootstrap: u64) -> ! {
	let mut bootstrap_buf: [u8; 256] = [0; 256];
	unsafe {
		let admin: u64 = recv_tagged(bootstrap, &mut bootstrap_buf, b"ADMIN").unwrap_or_else(|| fail_bootstrap(bootstrap, b"admin", b"audio admin channel not delivered"));
		let root: u64 = recv_tagged(bootstrap, &mut bootstrap_buf, b"SERVE").unwrap_or_else(|| fail_bootstrap(bootstrap, b"serve", b"missing serve channel"));
		// THE DEVICE IS DISCOVERED, NOT HANDED OVER (2026-08-31).
		//
		// This service used to be given the sound driver's channel under `SND`, routed to it by
		// DeviceManager out of a slot of its own - the per-kind injection P02M0164 exists to replace.
		// What arrives now is a connection to the provider CATALOGUE, and this service asks it for
		// the audio kind. Two things follow that the injection could not do: a sound card bound
		// AFTER this service started reaches it, because a subscription answers with what is
		// published now and continues as a stream; and a machine with two of them has a second
		// entry to offer rather than a slot that is already full.
		//
		// LAST IN THE ROLE LIST, because the bootstrap is read POSITIONALLY at every hop.
		let catalogue: u64 = recv_tagged(bootstrap, &mut bootstrap_buf, b"CATALOGUE").unwrap_or(0);
		send_blocking(bootstrap, b"AudioService: online", 0);
		// The subscription is opened BEFORE anything is served, so the snapshot and the stream are
		// one operation - a provider published between the two would otherwise be lost, which is the
		// same race as a service started later seeing nothing.
		let providers: u64 = if catalogue == 0 { 0 } else { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Audio).unwrap_or(0) };
		if providers == 0 {
			// NOT A FAILURE. A boot that granted no catalogue connection, or a machine whose
			// catalogue refuses the subscription, is a system with no sound - which this service is
			// built to report rather than to die of.
			print(b"AudioService: no provider subscription - this instance serves no device\n");
		}
		serve(root, admin, catalogue, providers, Audio::new(0));
	}
}

// OPEN A CONNECTION TO ONE PUBLISHED PROVIDER, or answer zero.
//
// The catalogue mints the pair and hands the driver the server end; what comes back is the client
// end this service talks the driver's byte protocol over - the same channel `SND` used to carry,
// reached by asking instead of by being given.
unsafe fn open_provider(catalogue: u64, info: &ProviderInfo) -> u64 {
	if catalogue == 0 {
		return 0;
	}
	match provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(info) {
		Some(Ok(handle)) => handle,
		// A REFUSAL IS SAID. A kind that admits one consumer refuses the second ask, and a service
		// that cannot tell that from "no device" cannot report either.
		Some(Err(_)) => {
			unsafe { print(b"AudioService: the catalogue refused a connection to the audio provider it published\n") };
			0
		}
		// A TRANSPORT THAT DID NOT ANSWER IS NOT AN ABSENT DEVICE either, and a service that cannot
		// tell the two apart cannot report what its machine has.
		None => {
			unsafe { print(b"AudioService: the catalogue did not answer the connection it published\n") };
			0
		}
	}
}

unsafe fn serve(root: u64, admin: u64, catalogue: u64, mut providers: u64, mut state: Audio) -> ! {
	unsafe {
		let mut clients: Vec<Client> = alloc::vec![Client { chan: root, scope: Scope::Full }];
		let mut request: [u8; REQUEST_MAX] = [0; REQUEST_MAX];
		let mut reply: [u8; REPLY_MAX] = [0; REPLY_MAX];
		loop {
			state.poll_streams();
			state.poll_captures();
			state.service_pending_writes();
			state.service_pending_reads();
			state.cleanup_drained();
			// The capture side is asked for first and the stop is offered last: a period the device
			// has already filled is audio that is lost if it is not collected, while a playback
			// period is one the device's own ring can wait for.
			if !state.pump_capture() {
				state.pump();
				state.stop_capture();
			}

			let driver_first: bool = state.snd != 0 && state.driver_pending != DriverPending::None;
			let mut waits: Vec<u64> = Vec::with_capacity(driver_first as usize + clients.len() + state.streams.len() + state.captures.len() + 2);
			if driver_first {
				waits.push(state.snd);
			}
			// THE SUBSCRIPTION IS WAITED ON LIKE ANY OTHER ENDPOINT, which is what makes a provider
			// published after this service started reachable at all. It sits before the clients for
			// the same reason the manager's channel does elsewhere: a busy service must not starve
			// the path that tells it its device has arrived or gone.
			if providers != 0 {
				waits.push(providers);
			}
			waits.push(admin);
			waits.extend(clients.iter().map(|client| client.chan));
			for stream in &state.streams {
				if stream.chan != 0 {
					waits.push(stream.chan);
				}
			}
			for capture in &state.captures {
				if capture.chan != 0 && capture.pending.is_none() {
					waits.push(capture.chan);
				}
			}
			let ready: i64 = wait_any(&waits, 0);
			if ready < 0 {
				continue;
			}
			let ready_chan: u64 = waits[ready as usize];
			// A PUBLICATION OR A WITHDRAWAL. `live` is what tells them apart, and it is the whole of
			// what this service does about either: take a connection to a device it has none for, and
			// let go of one whose provider has gone.
			if providers != 0 && ready_chan == providers {
				let mut frame: [u8; 256] = [0; 256];
				match recv_blocking(providers, &mut frame) {
					Received::Message { len, handle } => {
						if handle != 0 {
							close(handle);
						}
						let mut frame_handles = wire::Handles::new();
						let Some(info) = provider_catalogue::subscribe_read(&frame[..len], &mut frame_handles) else {
							print(b"AudioService: a provider frame did not decode\n");
							continue;
						};
						{
							if info.live && state.snd == 0 {
								let opened = open_provider(catalogue, &info);
								if opened == 0 {
									print(b"AudioService: an audio provider is published and this service could not connect to it\n");
								} else {
									state.snd = opened;
									print(b"AudioService: an audio provider was published and this service connected to it\n");
								}
							} else if !info.live && state.snd != 0 {
								// The provider this service is on may or may not be the one being
								// withdrawn, and nothing here can tell: the connection carries no
								// identity back. What a withdrawal DOES mean is that a driver went
								// away, and the driver channel closing is the authoritative signal -
								// which `driver_failed` already handles on the next wait.
								print(b"AudioService: an audio provider was withdrawn\n");
							}
						}
					}
					// THE SUBSCRIPTION ENDED. DeviceManager is gone or dropped it; this service keeps
					// whatever device it already has and stops expecting new ones.
					Received::Closed => {
						close(providers);
						providers = 0;
					}
				}
				continue;
			}
			if driver_first && ready_chan == state.snd {
				// A capture reply carries a whole period, so it is received into a buffer that can
				// hold one rather than into the small request scratch.
				match state.driver_pending {
					DriverPending::Capture(index) => {
						let mut period: Vec<u8> = alloc::vec![0; PERIOD_BYTES];
						match recv_blocking(state.snd, &mut period) {
							Received::Message { len, handle } => {
								if handle != 0 {
									close(handle);
								}
								state.capture_ready(index, &period[..len]);
							}
							Received::Closed => state.driver_failed(),
						}
					}
					_ => match recv_blocking(state.snd, &mut request) {
						Received::Message { handle, .. } => state.driver_ready(handle),
						Received::Closed => state.driver_failed(),
					},
				}
				continue;
			}
			if ready_chan == admin {
				match recv_caps_blocking(admin, &mut request) {
					ReceivedCaps::Message { len, handles: caps } => {
						let mut reply_handle = proto::codec::Handles::new();
						// EVERY CAPABILITY THE MESSAGE CARRIED. This was `Handles::from_slice(&[handle])`
						// over the single-handle receive, which keeps the first and drops the rest - so a
						// client sending stdin, stdout and stderr had two destroyed before dispatch.
						let mut handle = caps;
						let mut call = AdminCall { clients: &mut clients };
						if let Some(reply_len) = audio_admin::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle) {
							if !send_caps_blocking(admin, &reply[..reply_len], reply_handle.as_slice()) {
								for &leftover in reply_handle.as_slice() {
									close(leftover);
								}
							}
						} else {
							for &leftover in reply_handle.as_slice() {
								close(leftover);
							}
						}
						for &unclaimed in handle.as_slice() {
							close(unclaimed);
						}
					}
					ReceivedCaps::Closed => exit(),
				}
				continue;
			}
			if let Some(index) = clients.iter().position(|client| client.chan == ready_chan) {
				let scope: Scope = clients[index].scope;
				match recv_caps_blocking(ready_chan, &mut request) {
					ReceivedCaps::Message { len, handles: caps } => {
						// EVERY CAPABILITY THE MESSAGE CARRIED. This was `Handles::from_slice(&[handle])`
						// over the single-handle receive, which keeps the first and drops the rest - so a
						// client sending stdin, stdout and stderr had two destroyed before dispatch.
						let mut handle = caps;
						let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
						if op == HEARTBEAT_OP {
							send_blocking(ready_chan, b"PONG", 0);
						} else if op == CONNECT_OP && scope == Scope::Full {
							if let Some((server, client)) = channel() {
								clients.push(Client { chan: server, scope });
								send_blocking(ready_chan, &[], client);
							}
						} else {
							let mut reply_handle = proto::codec::Handles::new();
							let mut call = RootCall { audio: &mut state, scope };
							if let Some(reply_len) = audio::dispatch(&mut call, &request[..len], &mut handle, &mut reply, &mut reply_handle) {
								if !send_caps_blocking(ready_chan, &reply[..reply_len], reply_handle.as_slice()) {
									for &leftover in reply_handle.as_slice() {
										close(leftover);
									}
								}
							} else {
								for &leftover in reply_handle.as_slice() {
									close(leftover);
								}
							}
						}
						for &unclaimed in handle.as_slice() {
							close(unclaimed);
						}
					}
					ReceivedCaps::Closed => {
						if index == 0 {
							exit();
						}
						close(ready_chan);
						clients.swap_remove(index);
					}
				}
				continue;
			}
			if let Some(index) = state.captures.iter().position(|capture| capture.chan == ready_chan) {
				match recv_caps_blocking(ready_chan, &mut request) {
					ReceivedCaps::Message { len, handles } => {
						for &unclaimed in handles.as_slice() {
							close(unclaimed);
						}
						state.take_capture_request(index, &request[..len]);
					}
					ReceivedCaps::Closed => state.remove_capture(index),
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
			match recv_caps_blocking(ready_chan, &mut request) {
				ReceivedCaps::Message { len, handles } => {
					let op: u16 = if len >= 2 { u16::from_le_bytes([request[0], request[1]]) } else { 0 };
					if op == pcm_stream::OP_WRITE && state.streams[index].capacity() == 0 && !handles.is_empty() {
						state.streams[index].pending = Some(PendingWrite { request: request[..len].to_vec(), caps: handles });
					} else {
						state.dispatch_stream(index, &request[..len], handles);
					}
				}
				ReceivedCaps::Closed => {
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
