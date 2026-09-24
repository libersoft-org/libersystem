// camera_fixture - an in-guest camera, published as a `camera` provider over the production wire, and a
// control endpoint for the probe that drives the camera gate.
//
// DEVELOPMENT-ONLY. It binds to a QEMU test function at a pinned address only the camera gate adds, and
// its control endpoint is a kind no scope minted for real hardware admits. It is not a claim about USB
// Video transport: QEMU has no UVC device, and none is pretended here.
//
// WHAT IT PLAYS. Synthetic UVC descriptors - a YUY2 format with two sizes and discrete intervals, and a
// Motion-JPEG format with a continuous interval range - NORMALIZED BY `drivers::uvc`, the helper the USB
// Video class module will use; what it offers is what that helper made of them. Frames are written into
// the buffers CameraService queued - memory the CLIENT created - with deterministic bytes a probe can
// check: YUY2 pixels computed from their position and the frame's sequence, and an opaque MJPEG-framed
// payload carrying the sequence. Each frame carries the host time it was completed at and, unless told
// otherwise, a 32-bit device clock at 1 MHz that starts each stream just short of its wrap.
//
// WHAT THE PROBE CAN MAKE IT DO. Lose frames it counted, lose frames it could not count, stop sending
// device time, reset its clock, hold a stop's answer, withdraw and republish the camera, and read back
// what it counted - including how many client buffers it holds a mapping of right now.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use drivers::{common, uvc};
use proto::system::{CameraDeviceEvent, CameraDrop, CameraDropReason, CameraFixtureStats, CameraFrame, CameraGap, CameraOpen, ColorMatrix, ColorRange, DeviceTime, DiscreteIntervals, Error, FormatInfo, FrameSize, FrameType, Interval, IntervalRange, Intervals, Negotiated, StreamRequest, camera_device, camera_fixture};
use rt::*;

const NAME: &[u8] = b"org.libersystem.camera-fixture";
const CONTROL_NAME: &[u8] = b"org.libersystem.camera-fixture.control";
const CAMERA_TOKEN: u16 = 0;
const CONTROL_TOKEN: u16 = 1;
const VERSION: u32 = 1;
const STREAM_DEPTH: u64 = 64;
const NEVER: u64 = u64::MAX;
// The device clock: 32 bits at 1 MHz, starting with each stream 300 ms short of its wrap, so a stream
// the gate reads for a second sees one.
const CLOCK_WIDTH: u8 = 32;
const CLOCK_HZ: u64 = 1_000_000;
const CLOCK_START: u64 = (1 << 32) - 300_000;
const CLOCK_DOMAIN: u32 = 1;
// The MJPEG payload's framing: start and end of image around the sequence.
const MJPEG_BYTES: u32 = 64;

// The fixture's descriptors, as a UVC camera's video streaming interface would carry them.
fn descriptors() -> Vec<u8> {
	let mut graph: Vec<u8> = Vec::new();
	let mut format = alloc::vec![27, uvc::CS_INTERFACE, uvc::VS_FORMAT_UNCOMPRESSED, 1, 2];
	format.extend_from_slice(&uvc::YUY2_GUID);
	format.extend_from_slice(&[16, 1, 0, 0, 0, 0]);
	graph.extend_from_slice(&format);
	for (index, width, height) in [(1u8, 160u16, 120u16), (2, 320, 240)] {
		let bytes = u32::from(width) * u32::from(height) * 2;
		let mut frame = alloc::vec![34, uvc::CS_INTERFACE, uvc::VS_FRAME_UNCOMPRESSED, index, 0];
		frame.extend_from_slice(&width.to_le_bytes());
		frame.extend_from_slice(&height.to_le_bytes());
		frame.extend_from_slice(&[0; 8]);
		frame.extend_from_slice(&bytes.to_le_bytes());
		frame.extend_from_slice(&333_333u32.to_le_bytes());
		frame.push(2);
		frame.extend_from_slice(&333_333u32.to_le_bytes());
		frame.extend_from_slice(&666_666u32.to_le_bytes());
		graph.extend_from_slice(&frame);
	}
	graph.extend_from_slice(&[6, uvc::CS_INTERFACE, uvc::VS_COLORFORMAT, 1, 1, 4]);
	graph.extend_from_slice(&[11, uvc::CS_INTERFACE, uvc::VS_FORMAT_MJPEG, 2, 1, 0, 1, 0, 0, 0, 0]);
	let mut frame = alloc::vec![38, uvc::CS_INTERFACE, uvc::VS_FRAME_MJPEG, 1, 0];
	frame.extend_from_slice(&640u16.to_le_bytes());
	frame.extend_from_slice(&480u16.to_le_bytes());
	frame.extend_from_slice(&[0; 8]);
	frame.extend_from_slice(&(64 * 1024u32).to_le_bytes());
	frame.extend_from_slice(&333_333u32.to_le_bytes());
	frame.push(0);
	for value in [333_333u32, 999_999, 333_333] {
		frame.extend_from_slice(&value.to_le_bytes());
	}
	graph.extend_from_slice(&frame);
	graph.extend_from_slice(&[6, uvc::CS_INTERFACE, uvc::VS_COLORFORMAT, 1, 1, 1]);
	graph
}

fn interval(units: u32) -> Interval {
	Interval { numerator: units, denominator: uvc::INTERVAL_UNITS_PER_SECOND }
}

fn wire_format(format: &uvc::Format) -> FormatInfo {
	FormatInfo {
		index: format.index,
		frame_type: if format.kind == uvc::Kind::Yuy2 { FrameType::Yuy2 } else { FrameType::Mjpeg },
		compressed: format.kind == uvc::Kind::Mjpeg,
		sizes: format.sizes.len() as u8,
		range: match format.range {
			uvc::Range::Unknown => ColorRange::Unknown,
			uvc::Range::Limited => ColorRange::Limited,
			uvc::Range::Full => ColorRange::Full,
		},
		matrix: match format.matrix {
			uvc::Matrix::Unknown => ColorMatrix::Unknown,
			uvc::Matrix::Bt601 => ColorMatrix::Bt601,
			uvc::Matrix::Bt709 => ColorMatrix::Bt709,
		},
	}
}

fn wire_size(size: &uvc::Size) -> FrameSize {
	let intervals = match &size.intervals {
		uvc::Intervals::Discrete(values) => Intervals::Discrete(DiscreteIntervals { values: values.iter().map(|&units| interval(units)).collect() }),
		uvc::Intervals::Stepwise { minimum, maximum, step } => Intervals::Stepwise(IntervalRange { minimum: interval(*minimum), maximum: interval(*maximum), step: interval(*step) }),
	};
	FrameSize { index: size.index, width: size.width, height: size.height, max_bytes: size.max_bytes, intervals }
}

// The bytes a probe expects at `at` of a YUY2 frame with this sequence.
pub fn yuy2_byte(sequence: u64, at: usize) -> u8 {
	((at as u64).wrapping_mul(7).wrapping_add(sequence.wrapping_mul(13)) % 251) as u8
}

struct Slot {
	id: u8,
	handle: u64,
	base: u64,
	bytes: u64,
	// Queued for writing, under this lease.
	queued: Option<u64>,
}

struct Stream {
	generation: u64,
	selection: uvc::Selection,
	slots: Vec<Slot>,
	running: bool,
	sequence: u64,
	next_frame: u64,
	period: u64,
}

// One consumer's session.
#[derive(Default)]
struct Session {
	chan: u64,
	events: u64,
	event_seq: u32,
}

struct Fixture {
	live: bool,
	token: u16,
	next_token: u16,
	normalized: uvc::Normalized,
	connection: u64,
	session: Session,
	stream: Option<Stream>,
	timing: bool,
	clock_reset: u32,
	clock_zero: u64,
	drop_device: u32,
	gap_next: bool,
	stop_delay: Option<u32>,
	// A stop's answer being held: when to send it, on which connection, what.
	held_stop: Option<(u64, u64, Vec<u8>)>,
	stats: CameraFixtureStats,
}

impl Fixture {
	fn event(&mut self, event: &CameraDeviceEvent) {
		if self.session.events == 0 {
			return;
		}
		let mut frame = [0u8; 256];
		let mut handles = wire::Handles::new();
		if let Some(len) = camera_device::events_frame(self.session.event_seq, event, &mut frame, &mut handles)
			&& try_send(self.session.events, &frame[..len], 0)
		{
			self.session.event_seq += 1;
		}
	}

	// Every mapping of every client buffer released: the stream is gone from this side.
	fn release_all(&mut self) {
		if let Some(stream) = self.stream.take() {
			for slot in stream.slots {
				unmap_object(slot.handle);
				close(slot.handle);
			}
		}
		self.stats.mapped = 0;
	}

	fn departed(&mut self) {
		self.release_all();
		if self.session.events != 0 {
			close(self.session.events);
		}
		self.session = Session::default();
		self.held_stop = None;
	}

	fn device_time(&self) -> Option<DeviceTime> {
		if !self.timing {
			return None;
		}
		let elapsed_us = clock_ns().saturating_sub(self.clock_zero) / 1000;
		let ticks = (CLOCK_START + elapsed_us * CLOCK_HZ / 1_000_000) & ((1 << CLOCK_WIDTH) - 1);
		Some(DeviceTime { ticks, width_bits: CLOCK_WIDTH, frequency_hz: Some(CLOCK_HZ), clock_domain: CLOCK_DOMAIN, reset_generation: self.clock_reset })
	}

	// One frame's worth of time passed: write it into a queued buffer, or drop it and say why.
	fn produce(&mut self) {
		let now = clock();
		let device = self.device_time();
		let Some(stream) = self.stream.as_mut().filter(|stream| stream.running && now >= stream.next_frame) else { return };
		stream.next_frame = now + stream.period;
		let generation = stream.generation;
		let sequence = stream.sequence;
		// A LOSS NOBODY COUNTED: the sequence moves on by an amount the service is not told.
		let event = if self.gap_next {
			stream.sequence += 5;
			self.gap_next = false;
			self.stats.gaps += 1;
			CameraDeviceEvent::Gap(CameraGap { stream_generation: generation, next_sequence: stream.sequence })
		} else if self.drop_device > 0 {
			stream.sequence += 1;
			self.drop_device -= 1;
			self.stats.dropped_device += 1;
			CameraDeviceEvent::Dropped(CameraDrop { stream_generation: generation, first_sequence: sequence, count: 1, reason: CameraDropReason::Device })
		} else {
			stream.sequence += 1;
			let selection = stream.selection;
			match stream.slots.iter_mut().find(|slot| slot.queued.is_some()) {
				None => {
					self.stats.dropped_no_buffer += 1;
					CameraDeviceEvent::Dropped(CameraDrop { stream_generation: generation, first_sequence: sequence, count: 1, reason: CameraDropReason::NoBuffer })
				}
				Some(slot) => {
					let lease = slot.queued.take().unwrap_or(0);
					let valid = match selection.kind {
						uvc::Kind::Yuy2 => selection.stride * u32::from(selection.height),
						uvc::Kind::Mjpeg => MJPEG_BYTES,
					};
					if u64::from(valid) > slot.bytes {
						return;
					}
					// SAFETY: `base` maps the whole buffer, `slot.bytes` long and checked above, writable
					// because the handle carries `write`; nothing else in this process writes it while it
					// is queued.
					unsafe {
						let bytes = core::slice::from_raw_parts_mut(slot.base as *mut u8, valid as usize);
						match selection.kind {
							uvc::Kind::Yuy2 => {
								for (at, byte) in bytes.iter_mut().enumerate() {
									*byte = yuy2_byte(sequence, at);
								}
							}
							uvc::Kind::Mjpeg => {
								bytes.fill(0);
								bytes[0..2].copy_from_slice(&[0xff, 0xd8]);
								bytes[2..10].copy_from_slice(&sequence.to_le_bytes());
								bytes[62..64].copy_from_slice(&[0xff, 0xd9]);
							}
						}
					}
					self.stats.frames_written += 1;
					// SAMPLED WHEN THE FRAME IS COMPLETE and ready for handoff, not when anybody reads it.
					CameraDeviceEvent::Frame(CameraFrame { stream_generation: generation, sequence, buffer: slot.id, lease, valid_bytes: valid, arrival_ns: clock_ns(), device })
				}
			}
		};
		self.event(&event);
	}

	fn next_due(&self) -> u64 {
		let frame = self.stream.as_ref().filter(|stream| stream.running).map(|stream| stream.next_frame);
		let stop = self.held_stop.as_ref().map(|(release, _, _)| *release).filter(|&release| release != NEVER);
		[frame, stop].into_iter().flatten().min().unwrap_or(0)
	}

	// A held stop's time came: every mapping released, and then - only then - the answer.
	fn release_due(&mut self) {
		if let Some((release, chan, _)) = self.held_stop.as_ref()
			&& clock() >= *release
		{
			let chan = *chan;
			if let Some((_, _, bytes)) = self.held_stop.take() {
				self.release_all();
				let _ = try_send(chan, &bytes, 0);
			}
		}
	}
}

// ------------------------------------------------------------------ the provider

struct CameraView<'a> {
	fixture: &'a mut Fixture,
	// A stop's answer to hold rather than send: until when.
	hold: Option<u64>,
}

impl CameraView<'_> {
	fn stream(&mut self, generation: u64) -> Result<&mut Stream, Error> {
		self.fixture.stream.as_mut().filter(|stream| stream.generation == generation).ok_or(Error::Stale)
	}
}

impl camera_device::Service for CameraView<'_> {
	fn open(&mut self, version: u32) -> Result<CameraOpen, Error> {
		if version != VERSION {
			return Err(Error::Unsupported);
		}
		self.fixture.connection += 1;
		Ok(CameraOpen { connection_generation: self.fixture.connection, name: String::from("fixture camera"), generation: 1 })
	}

	fn formats(&mut self) -> Result<Vec<FormatInfo>, Error> {
		Ok(self.fixture.normalized.formats.iter().map(wire_format).collect())
	}

	fn sizes(&mut self, format: u8) -> Result<Vec<FrameSize>, Error> {
		let found = self.fixture.normalized.formats.iter().find(|candidate| candidate.index == format).ok_or(Error::NotFound)?;
		Ok(found.sizes.iter().map(wire_size).collect())
	}

	// THE NORMALIZER'S SELECTION, and nothing else: exactly what was asked for, or a refusal. A new
	// negotiation replaces a stream that never started, and every mapping it held.
	fn negotiate(&mut self, generation: u64, request: StreamRequest) -> Result<Negotiated, Error> {
		if self.fixture.stream.as_ref().is_some_and(|stream| stream.running) {
			return Err(Error::Again);
		}
		if request.interval.denominator != uvc::INTERVAL_UNITS_PER_SECOND {
			return Err(Error::Unsupported);
		}
		let selection = uvc::select(&self.fixture.normalized, request.format, request.size, request.interval.numerator).ok_or(Error::Unsupported)?;
		let format = self.fixture.normalized.formats.iter().find(|candidate| candidate.index == request.format).ok_or(Error::NotFound)?;
		let info = wire_format(format);
		self.fixture.release_all();
		self.fixture.stream = Some(Stream { generation, selection, slots: Vec::new(), running: false, sequence: 0, next_frame: 0, period: 1 });
		Ok(Negotiated { stream_generation: generation, format: selection.format, frame_type: info.frame_type, width: selection.width, height: selection.height, interval: request.interval, max_bytes: selection.max_bytes, stride: selection.stride, plane_offset: 0, range: info.range, matrix: info.matrix })
	}

	fn register(&mut self, generation: u64, buffer: u8, memory: u64) -> Result<(), Error> {
		let Some(object) = object_info(memory) else {
			close(memory);
			return Err(Error::Invalid);
		};
		let Ok(stream) = self.stream(generation) else {
			close(memory);
			return Err(Error::Stale);
		};
		if stream.running || stream.slots.iter().any(|slot| slot.id == buffer) {
			close(memory);
			return Err(Error::Invalid);
		}
		// SAFETY: mapping a memory object this process was handed; the base is used only while it is mapped.
		let Some(base) = (unsafe { map_object(memory) }) else {
			close(memory);
			return Err(Error::Denied);
		};
		stream.slots.push(Slot { id: buffer, handle: memory, base, bytes: object.size, queued: None });
		self.fixture.stats.mapped += 1;
		Ok(())
	}

	fn queue(&mut self, generation: u64, buffer: u8, lease: u64) -> Result<(), Error> {
		let stream = self.stream(generation)?;
		let slot = stream.slots.iter_mut().find(|slot| slot.id == buffer).ok_or(Error::NotFound)?;
		slot.queued = Some(lease);
		Ok(())
	}

	fn start(&mut self, generation: u64) -> Result<(), Error> {
		let stream = self.stream(generation)?;
		// THE NEGOTIATED INTERVAL, in scheduler ticks: a frame every interval, never faster than a tick.
		let interval_ms = u64::from(stream.selection.interval) / 10_000;
		stream.period = interval_ms.div_ceil(10).max(1);
		stream.running = true;
		stream.next_frame = clock() + stream.period;
		self.fixture.clock_zero = clock_ns();
		Ok(())
	}

	// STOP: no more writes, every buffer back and every mapping released - answered now, or held when told.
	fn stop(&mut self, generation: u64) -> Result<(), Error> {
		self.stream(generation)?;
		self.fixture.stats.stops += 1;
		if let Some(stream) = self.fixture.stream.as_mut() {
			stream.running = false;
		}
		match self.fixture.stop_delay.take() {
			Some(0) => self.hold = Some(NEVER),
			Some(delay) => self.hold = Some(clock() + u64::from(delay).div_ceil(10)),
			None => self.fixture.release_all(),
		}
		Ok(())
	}

	fn events(&mut self) -> Vec<CameraDeviceEvent> {
		Vec::new()
	}
}

// ------------------------------------------------------------------ the control endpoint

struct ControlView<'a> {
	fixture: &'a mut Fixture,
	serving: &'a mut common::Serving,
	bootstrap: u64,
	bind: &'a common::Bind,
}

impl camera_fixture::Service for ControlView<'_> {
	fn drop_device(&mut self, count: u32) -> Result<(), Error> {
		self.fixture.drop_device = count;
		Ok(())
	}

	fn gap(&mut self) -> Result<(), Error> {
		self.fixture.gap_next = true;
		Ok(())
	}

	fn timing(&mut self, on: bool) -> Result<(), Error> {
		self.fixture.timing = on;
		Ok(())
	}

	fn reset_clock(&mut self) -> Result<(), Error> {
		self.fixture.clock_reset += 1;
		self.fixture.clock_zero = clock_ns();
		Ok(())
	}

	fn delay_stop(&mut self, delay_ms: u32) -> Result<(), Error> {
		self.fixture.stop_delay = Some(delay_ms);
		Ok(())
	}

	fn withdraw(&mut self) -> Result<(), Error> {
		let fixture = &mut *self.fixture;
		if !fixture.live {
			return Err(Error::NotFound);
		}
		fixture.live = false;
		fixture.departed();
		if !common::withdraw(self.bootstrap, self.bind, fixture.token) {
			return Err(Error::Io);
		}
		Ok(())
	}

	fn republish(&mut self) -> Result<(), Error> {
		let fixture = &mut *self.fixture;
		if fixture.live {
			return Err(Error::Invalid);
		}
		let token = fixture.next_token;
		let Some((near, far)) = channel() else { return Err(Error::Exhausted) };
		if !self.serving.publish(token, near) {
			close(near);
			close(far);
			return Err(Error::Exhausted);
		}
		if !common::offer_named(self.bootstrap, self.bind, driver_protocol::provider::CAMERA, token, NAME, far) {
			return Err(Error::Io);
		}
		fixture.next_token = token + 1;
		fixture.token = token;
		fixture.live = true;
		Ok(())
	}

	fn stats(&mut self) -> Result<CameraFixtureStats, Error> {
		Ok(self.fixture.stats.clone())
	}
}

// One request on one consumer connection. False when the connection is over.
fn serve(fixture: &mut Fixture, serving: &mut common::Serving, bootstrap: u64, bind: &common::Bind, token: u16, channel: u64, buf: &mut [u8]) -> bool {
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	if len < 2 {
		return true;
	}
	let mut reply_buf = alloc::vec![0u8; 16 * 1024];
	let mut reply_handles = wire::Handles::new();
	if token == CONTROL_TOKEN {
		let mut view = ControlView { fixture: &mut *fixture, serving: &mut *serving, bootstrap, bind };
		if let Some(written) = camera_fixture::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) {
			send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
		}
		return true;
	}
	if token != fixture.token || !fixture.live {
		for &handle in handles.as_slice() {
			close(handle);
		}
		return true;
	}
	// A NEW CONSUMER CONNECTION IS A NEW SESSION: what the old one held is released.
	if fixture.session.chan != channel {
		fixture.departed();
		fixture.session.chan = channel;
	}
	let op = u16::from_le_bytes([buf[0], buf[1]]);
	if op == camera_device::OP_EVENTS {
		let mut view = CameraView { fixture: &mut *fixture, hold: None };
		let Some((corr, _)) = camera_device::events_open(&mut view, &buf[..len], &mut handles) else { return true };
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
		if fixture.session.events != 0 {
			close(fixture.session.events);
		}
		fixture.session.events = producer;
		fixture.session.event_seq = 0;
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		return true;
	}
	let mut view = CameraView { fixture: &mut *fixture, hold: None };
	let written = camera_device::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles);
	let hold = view.hold;
	for &handle in handles.as_slice() {
		close(handle);
	}
	let Some(written) = written else { return true };
	match hold {
		Some(release) => {
			fixture.held_stop = Some((release, channel, reply_buf[..written].to_vec()));
		}
		None => {
			send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
		}
	}
	true
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = common::handshake(bootstrap);
	// THE STAGED NORMALIZER, over the fixture's own descriptors: what it offers is what the helper made.
	let Ok(normalized) = uvc::normalize(&descriptors()) else {
		print(b"driver.camera-fixture: the fixture's descriptors do not normalize\n");
		exit();
	};
	let (Some((camera, camera_far)), Some((control, control_far))) = (channel(), channel()) else { exit() };
	let timer: u64 = match timer_create() {
		t if t > 0 => t as u64,
		_ => exit(),
	};
	common::online_named(bootstrap, &bind, b"driver.camera-fixture: online (a camera with YUY2 and MJPEG formats, for the camera gate)", &[(driver_protocol::provider::CAMERA, camera_far, NAME), (driver_protocol::provider::FIXTURE_CONTROL, control_far, CONTROL_NAME)]);
	let mut serving = common::Serving::from_offers(&[(CAMERA_TOKEN, camera), (CONTROL_TOKEN, control)]);
	let mut fixture = Fixture { live: true, token: CAMERA_TOKEN, next_token: CONTROL_TOKEN + 1, normalized, connection: 0, session: Session::default(), stream: None, timing: true, clock_reset: 0, clock_zero: clock_ns(), drop_device: 0, gap_next: false, stop_delay: None, held_stop: None, stats: CameraFixtureStats { frames_written: 0, dropped_no_buffer: 0, dropped_device: 0, gaps: 0, mapped: 0, stops: 0 } };
	let mut buf = alloc::vec![0u8; 8192];
	loop {
		let due = fixture.next_due();
		// ARMED EVERY PASS, and far off when nothing is due. A timer stays expired until it is armed again,
		// so leaving it alone once the last deadline passed made every later wait return at once, and the
		// fixture spun a processor until something was due - long enough to starve the probes it serves.
		timer_set(timer, if due != 0 { due } else { u64::MAX });
		match common::wait_providers_or_answer(bootstrap, &bind, &mut serving, &[timer]) {
			None => {
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, 0, true);
				}
				exit();
			}
			Some(common::ProviderReady::Connected(_)) => {}
			Some(common::ProviderReady::Device(_)) => {}
			Some(common::ProviderReady::Consumer(index)) => {
				let token = serving.token_at(index);
				let chan = serving.at(index);
				if !serve(&mut fixture, &mut serving, bootstrap, &bind, token, chan, &mut buf) {
					let token = serving.close_at(index);
					if token == fixture.token && chan == fixture.session.chan {
						// THE CONSUMER LEFT: its stream ends here, as a restarted service's would.
						fixture.departed();
					}
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
				}
			}
		}
		fixture.release_due();
		fixture.produce();
	}
}
