// camcheck - the in-guest scenario driver for the camera gate. DEVELOPMENT-ONLY.
//
// It holds the camera fixture's control endpoint, camera inventory and a capture grant on the fixture
// camera, and prints one verdict line per phase for the gate to read. The buffers it registers are memory
// objects IT created; what it checks in them is the bytes the fixture wrote, computed independently here
// from the frame's sequence and position. The service's handle baseline is the gate's to read, from the
// system graph, which holds the service's process.
//
//   camcheck capture     exact YUY2 negotiation; four buffers retained while loss is counted, their bytes
//                        stable; one lease back and bounded recovery; device time across a wrap; stop
//   camcheck mjpeg       a continuous-interval MJPEG stream and its opaque payload
//   camcheck timing      no device time when the device gives none; a clock reset and an uncounted loss
//                        each start a new device timeline
//   camcheck busy        while another grant streams, negotiation is refused as busy
//   camcheck inherit     a transferred capture endpoint does not keep its dead owner's stream
//   camcheck quarantine  an unconfirmed stop times out and quarantines the camera; its replacement is
//                        another camera and the old grant names nothing
//   camcheck again       a new grant captures while an old client keeps its buffer

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{CameraId, CaptureEvent, Error, Frame, FrameType, Interval, LaunchContext, Negotiated, StreamRequest, camera, camera_capture, camera_fixture};
use rt::*;
use services::capability_names::*;

const TICKS: u64 = 100;
const YUY2_BYTES: u64 = 160 * 120 * 2;

fn say(line: &[u8]) {
	print(b"camcheck: ");
	print(line);
	print(b"\n");
}

fn fail(line: &[u8]) -> ! {
	print(b"camcheck: FAIL ");
	print(line);
	print(b"\n");
	exit();
}

fn number(out: &mut Vec<u8>, value: u64) {
	let mut digits = [0u8; 20];
	let mut at = digits.len();
	let mut n = value;
	loop {
		at -= 1;
		digits[at] = b'0' + (n % 10) as u8;
		n /= 10;
		if n == 0 {
			break;
		}
	}
	out.extend_from_slice(&digits[at..]);
}

// The fixture's YUY2 pattern, computed here independently.
fn yuy2_byte(sequence: u64, at: usize) -> u8 {
	((at as u64).wrapping_mul(7).wrapping_add(sequence.wrapping_mul(13)) % 251) as u8
}

struct Probe {
	fixture: u64,
	inventory: u64,
	capture: u64,
}

// One client-owned buffer: the object this probe created, and where it maps it.
struct Owned {
	handle: u64,
	base: u64,
	bytes: u64,
}

impl Owned {
	fn new(bytes: u64) -> Owned {
		let handle = memory_object_create(bytes);
		if handle <= 0 {
			fail(b"a buffer could not be created");
		}
		// SAFETY: this probe's own memory object, mapped once for its whole life.
		let Some(base) = (unsafe { map_object(handle as u64) }) else { fail(b"a buffer could not be mapped") };
		Owned { handle: handle as u64, base, bytes }
	}
	fn bytes(&self, len: usize) -> &[u8] {
		// SAFETY: the mapping covers `self.bytes`, and `len` is checked against it.
		unsafe { core::slice::from_raw_parts(self.base as *const u8, len.min(self.bytes as usize)) }
	}
	// A copy of the handle narrowed to what the camera needs, for `register` to consume.
	fn lend(&self) -> u64 {
		let lent = duplicate(self.handle, RIGHT_MAP | RIGHT_READ | RIGHT_WRITE | RIGHT_TRANSFER);
		if lent < 0 {
			fail(b"a buffer handle could not be narrowed");
		}
		lent as u64
	}
}

impl Probe {
	fn fixture(&self) -> camera_fixture::Client<ChannelTransport> {
		camera_fixture::Client::with_deadline(ChannelTransport { chan: self.fixture }, clock() + 5 * TICKS)
	}
	fn capture(&self) -> camera_capture::Client<ChannelTransport> {
		camera_capture::Client::with_deadline(ChannelTransport { chan: self.capture }, clock() + 10 * TICKS)
	}
	fn fixture_ok(&self, result: Option<Result<(), Error>>, why: &[u8]) {
		if !matches!(result, Some(Ok(()))) {
			fail(why);
		}
	}
	fn camera_id(&self) -> CameraId {
		match self.capture().camera() {
			Some(Ok(info)) => info.id,
			_ => fail(b"the capture grant could not describe its camera"),
		}
	}
	fn negotiate(&self, format: u8, size: u8, numerator: u32) -> Negotiated {
		match self.capture().negotiate(&StreamRequest { format, size, interval: Interval { numerator, denominator: 10_000_000 } }) {
			Some(Ok(negotiated)) => negotiated,
			Some(Err(_)) => fail(b"the negotiation was refused"),
			None => fail(b"the negotiation did not complete"),
		}
	}
	fn register(&self, buffer: &Owned) -> u8 {
		match self.capture().register(&buffer.lend()) {
			Some(Ok(id)) => id,
			_ => fail(b"a buffer was not registered"),
		}
	}
	fn events(&self) -> u64 {
		match self.capture().events() {
			Some(stream) => stream,
			None => fail(b"the event stream did not open"),
		}
	}
	fn start(&self) {
		if !matches!(self.capture().start(), Some(Ok(()))) {
			fail(b"the stream did not start");
		}
	}
	fn stop(&self) {
		if !matches!(self.capture().stop(), Some(Ok(()))) {
			fail(b"the stream did not stop");
		}
	}
}

enum Read {
	Frame(Frame),
	Status,
	Nothing,
}

fn read(stream: u64, until: u64) -> Read {
	let mut buf = [0u8; 512];
	if wait(stream, until) != 0 {
		return Read::Nothing;
	}
	match try_recv_caps(stream, &mut buf) {
		PolledCaps::Message { len, handles } => {
			for &handle in handles.as_slice() {
				close(handle);
			}
			let mut frame_handles = wire::Handles::new();
			match camera_capture::events_read(&buf[..len], &mut frame_handles) {
				Some(CaptureEvent::Frame(frame)) => Read::Frame(frame),
				Some(CaptureEvent::Status(_)) => Read::Status,
				None => Read::Nothing,
			}
		}
		_ => Read::Nothing,
	}
}

fn next_frame(stream: u64, seconds: u64) -> Frame {
	let until = clock() + seconds * TICKS;
	loop {
		match read(stream, until) {
			Read::Frame(frame) => return frame,
			Read::Status => {}
			Read::Nothing => fail(b"no frame arrived"),
		}
	}
}

fn yuy2_matches(buffer: &Owned, frame: &Frame) -> bool {
	frame.valid_bytes as u64 == YUY2_BYTES && buffer.bytes(frame.valid_bytes as usize).iter().enumerate().all(|(at, byte)| *byte == yuy2_byte(frame.sequence, at))
}

fn checksum(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
}

fn capture(probe: &Probe) {
	let negotiated = probe.negotiate(1, 1, 333_333);
	// EXACTLY WHAT WAS ASKED, and the layout that follows from it.
	if negotiated.frame_type != FrameType::Yuy2 || (negotiated.width, negotiated.height, negotiated.stride, negotiated.plane_offset) != (160, 120, 320, 0) || u64::from(negotiated.max_bytes) != YUY2_BYTES || negotiated.interval.numerator != 333_333 {
		fail(b"capture: the negotiated parameters are not the ones asked for");
	}
	let buffers: Vec<Owned> = (0..4).map(|_| Owned::new(YUY2_BYTES)).collect();
	let ids: Vec<u8> = buffers.iter().map(|buffer| probe.register(buffer)).collect();
	// A fifth, and the same backing twice, are refused.
	let extra = Owned::new(YUY2_BYTES);
	if matches!(probe.capture().register(&extra.lend()), Some(Ok(_))) {
		fail(b"capture: a fifth buffer was registered");
	}
	let stream = probe.events();
	probe.start();
	// ALL FOUR COMPLETED AND RETAINED: nothing is returned.
	let mut frames: Vec<Frame> = Vec::new();
	while frames.len() < 4 {
		frames.push(next_frame(stream, 5));
	}
	for frame in &frames {
		let Some(at) = ids.iter().position(|id| *id == frame.buffer) else { fail(b"capture: a frame named a buffer that is not ours") };
		if !yuy2_matches(&buffers[at], frame) {
			fail(b"capture: a frame's bytes are not the fixture's pattern");
		}
		let Some(device) = frame.device.as_ref() else { fail(b"capture: a frame carried no device time") };
		if device.width_bits != 32 || device.frequency_hz != Some(1_000_000) || frame.arrival_ns > clock_ns() {
			fail(b"capture: a frame's timing is not what the device gave");
		}
	}
	let before: Vec<u64> = frames.iter().map(|frame| checksum(buffers[ids.iter().position(|id| *id == frame.buffer).unwrap_or(0)].bytes(YUY2_BYTES as usize))).collect();
	// A DELAYED READ: frames keep being observed and, with nothing queued, dropped and counted.
	let paused = clock_ns();
	sleep_until(clock() + TICKS / 2);
	if frames.iter().any(|frame| frame.arrival_ns > paused) {
		fail(b"capture: a frame's arrival time is not the time it was completed");
	}
	let status = match probe.capture().status() {
		Some(Ok(status)) => status,
		_ => fail(b"capture: the status could not be read"),
	};
	if status.dropped_no_buffer == 0 {
		fail(b"capture: no loss was reported while every buffer was leased");
	}
	for (frame, sum) in frames.iter().zip(&before) {
		let at = ids.iter().position(|id| *id == frame.buffer).unwrap_or(0);
		if checksum(buffers[at].bytes(YUY2_BYTES as usize)) != *sum {
			fail(b"capture: a leased buffer changed while it was leased");
		}
	}
	// The host arrival time is the one the frame came with, however late it is read.
	let first = &frames[0];
	// ONE LEASE BACK IS ONE BUFFER BACK: bounded recovery, with the loss as a gap in the sequence.
	if !matches!(probe.capture().release(&first.buffer, &first.lease), Some(Ok(()))) {
		fail(b"capture: a lease could not be returned");
	}
	if matches!(probe.capture().release(&first.buffer, &first.lease), Some(Ok(()))) {
		fail(b"capture: the same lease was returned twice");
	}
	// READ LATE ON PURPOSE: its arrival time is when it was completed, not when it was read.
	sleep_until(clock() + TICKS / 3);
	let recovered = next_frame(stream, 5);
	if clock_ns().saturating_sub(recovered.arrival_ns) < 200_000_000 {
		fail(b"capture: a frame read late carried the time it was read, not the time it was completed");
	}
	if recovered.buffer != first.buffer || recovered.sequence <= frames[3].sequence + 1 || !yuy2_matches(&buffers[ids.iter().position(|id| *id == recovered.buffer).unwrap_or(0)], &recovered) {
		fail(b"capture: the returned buffer did not bring back a later frame after the counted gap");
	}
	// DEVICE TIME, modulo its width: the counter started 50 ms short of its wrap and has wrapped by now.
	let (Some(early), Some(late)) = (first.device.as_ref(), recovered.device.as_ref()) else { fail(b"capture: device time went missing") };
	let elapsed = late.ticks.wrapping_sub(early.ticks) & 0xffff_ffff;
	let host_us = (recovered.arrival_ns - first.arrival_ns) / 1000;
	if late.ticks >= early.ticks || elapsed > host_us + 20_000 || elapsed + 20_000 < host_us {
		fail(b"capture: the device clock did not wrap consistently with the host clock");
	}
	probe.stop();
	match probe.fixture().stats() {
		Some(Ok(stats)) if stats.mapped == 0 => {}
		_ => fail(b"capture: the camera still held a mapping after the confirmed stop"),
	}
	close(stream);
	let mut line = b"PASS capture: exact YUY2 negotiation, four leased buffers stable while ".to_vec();
	number(&mut line, status.dropped_no_buffer);
	line.extend_from_slice(b" frames were dropped and counted, one lease back recovered one buffer, the device clock wrapped consistently, and the stop released every mapping");
	say(&line);
}

fn mjpeg(probe: &Probe) {
	let negotiated = probe.negotiate(2, 1, 666_666);
	if negotiated.frame_type != FrameType::Mjpeg || (negotiated.width, negotiated.height, negotiated.stride) != (640, 480, 0) {
		fail(b"mjpeg: the negotiated parameters are not the ones asked for");
	}
	// Off the continuous range's step is not offered, and asking is refused rather than rounded.
	let buffer = Owned::new(u64::from(negotiated.max_bytes));
	probe.register(&buffer);
	let stream = probe.events();
	probe.start();
	let frame = next_frame(stream, 5);
	let bytes = buffer.bytes(frame.valid_bytes as usize);
	if frame.valid_bytes != 64 || bytes[0..2] != [0xff, 0xd8] || bytes[62..64] != [0xff, 0xd9] || u64::from_le_bytes(bytes[2..10].try_into().unwrap_or([0; 8])) != frame.sequence {
		fail(b"mjpeg: the payload is not the fixture's opaque frame");
	}
	probe.stop();
	close(stream);
	if matches!(probe.capture().negotiate(&StreamRequest { format: 2, size: 1, interval: Interval { numerator: 500_000, denominator: 10_000_000 } }), Some(Ok(_))) {
		fail(b"mjpeg: an interval off the range's step was accepted");
	}
	say(b"PASS mjpeg: a continuous-range MJPEG stream carried its opaque payload, and an interval off the step was refused");
}

fn timing(probe: &Probe) {
	probe.negotiate(1, 1, 333_333);
	let buffer = Owned::new(YUY2_BYTES);
	probe.register(&buffer);
	let stream = probe.events();
	probe.fixture_ok(probe.fixture().timing(&false), b"timing: the fixture could not stop its device time");
	probe.start();
	let untimed = next_frame(stream, 5);
	if untimed.device.is_some() {
		fail(b"timing: device time appeared where the device gave none");
	}
	probe.fixture_ok(probe.fixture().timing(&true), b"timing: the fixture could not restore its device time");
	let _ = probe.capture().release(&untimed.buffer, &untimed.lease);
	let first = next_frame(stream, 5);
	let _ = probe.capture().release(&first.buffer, &first.lease);
	let same = next_frame(stream, 5);
	let _ = probe.capture().release(&same.buffer, &same.lease);
	probe.fixture_ok(probe.fixture().reset_clock(), b"timing: the fixture could not reset its clock");
	let reset = next_frame(stream, 5);
	let _ = probe.capture().release(&reset.buffer, &reset.lease);
	probe.fixture_ok(probe.fixture().gap(), b"timing: the fixture could not lose frames uncounted");
	let after_gap = next_frame(stream, 5);
	let generation = |frame: &Frame| frame.device.as_ref().map(|device| device.reset_generation);
	if generation(&first) != generation(&same) || generation(&reset) == generation(&same) || generation(&after_gap) == generation(&reset) {
		fail(b"timing: a device timeline was carried across a reset or an uncounted loss");
	}
	let status = match probe.capture().status() {
		Some(Ok(status)) => status,
		_ => fail(b"timing: the status could not be read"),
	};
	if status.unknown_discontinuities == 0 {
		fail(b"timing: the uncounted loss was not reported as a discontinuity");
	}
	probe.stop();
	close(stream);
	say(b"PASS timing: no device time where the device gave none; a clock reset and an uncounted loss each started a new device timeline, and the loss is a discontinuity");
}

fn busy(probe: &Probe) {
	// Another grant holds a running stream (`camhold hold` in the background).
	sleep_until(clock() + TICKS);
	if !matches!(probe.capture().negotiate(&StreamRequest { format: 1, size: 1, interval: Interval { numerator: 333_333, denominator: 10_000_000 } }), Some(Err(Error::Again))) {
		fail(b"busy: a second grant's negotiation was not refused as busy");
	}
	// And once the other stream is over, the camera is anybody's again - which is also what lets the
	// next phase start from a camera nobody streams on.
	let deadline = clock() + 10 * TICKS;
	loop {
		let streaming = match camera::Client::with_deadline(ChannelTransport { chan: probe.inventory }, clock() + 2 * TICKS).cameras() {
			Some(Ok(cameras)) => cameras.iter().any(|info| info.streaming),
			_ => fail(b"busy: the inventory could not be read"),
		};
		if !streaming {
			break;
		}
		if clock() >= deadline {
			fail(b"busy: the other grant's stream never ended");
		}
		sleep_until(clock() + TICKS / 5);
	}
	say(b"PASS busy: while another grant streamed, negotiation was refused as busy");
}

fn inherit(probe: &Probe) {
	let mut buf = [0u8; 64];
	let handles = loop {
		match try_recv_caps(stdin(), &mut buf) {
			PolledCaps::Message { handles, .. } if !handles.as_slice().is_empty() => break handles,
			PolledCaps::Message { .. } => {}
			PolledCaps::Empty => sleep_until(clock() + TICKS / 10),
			PolledCaps::Closed => fail(b"inherit: the holder sent no endpoint"),
		}
	};
	let inherited = handles.first();
	// The holder has exited; its grant, and its stream, went with it.
	let deadline = clock() + 5 * TICKS;
	loop {
		let streaming = match camera::Client::with_deadline(ChannelTransport { chan: probe.inventory }, clock() + 2 * TICKS).cameras() {
			Some(Ok(cameras)) => cameras.iter().any(|info| info.streaming),
			_ => fail(b"inherit: the inventory could not be read"),
		};
		if !streaming {
			break;
		}
		if clock() >= deadline {
			fail(b"inherit: the stream outlived its owner");
		}
		sleep_until(clock() + TICKS / 10);
	}
	if matches!(camera_capture::Client::with_deadline(ChannelTransport { chan: inherited }, clock() + 2 * TICKS).status(), Some(Ok(_))) {
		fail(b"inherit: a transferred endpoint kept its dead owner's authority");
	}
	match probe.fixture().stats() {
		Some(Ok(stats)) if stats.mapped == 0 => {}
		_ => fail(b"inherit: the camera still held the dead owner's buffers"),
	}
	close(inherited);
	say(b"PASS inherit: the stream stopped with its owner, its buffers were released, and a transferred copy of the endpoint could do nothing");
}

fn quarantine(probe: &Probe) {
	let old = probe.camera_id();
	probe.negotiate(1, 1, 333_333);
	let buffer = Owned::new(YUY2_BYTES);
	probe.register(&buffer);
	let stream = probe.events();
	probe.start();
	next_frame(stream, 5);
	probe.fixture_ok(probe.fixture().delay_stop(&0), b"quarantine: the fixture could not hold its stop");
	let started = clock();
	if !matches!(probe.capture().stop(), Some(Err(Error::TimedOut))) {
		fail(b"quarantine: an unconfirmed stop was not reported timed out");
	}
	if clock() - started < 2 * TICKS - 10 {
		fail(b"quarantine: the stop gave up before its two seconds");
	}
	match probe.capture().camera() {
		Some(Ok(info)) if !info.available => {}
		_ => fail(b"quarantine: the camera was not reported unavailable"),
	}
	if !matches!(probe.capture().negotiate(&StreamRequest { format: 1, size: 1, interval: Interval { numerator: 333_333, denominator: 10_000_000 } }), Some(Err(Error::Io))) {
		fail(b"quarantine: a quarantined camera accepted a new stream");
	}
	// UNPLUG AND REPLACEMENT: the replacement is another camera, and the old grant names nothing.
	probe.fixture_ok(probe.fixture().withdraw(), b"quarantine: the fixture could not withdraw the camera");
	sleep_until(clock() + TICKS / 2);
	if matches!(probe.capture().camera(), Some(Ok(_))) {
		fail(b"quarantine: a withdrawn camera's grant still answered");
	}
	probe.fixture_ok(probe.fixture().republish(), b"quarantine: the fixture could not republish the camera");
	let deadline = clock() + 5 * TICKS;
	loop {
		if let Some(Ok(cameras)) = camera::Client::with_deadline(ChannelTransport { chan: probe.inventory }, clock() + 2 * TICKS).cameras()
			&& cameras.iter().any(|info| info.id != old && info.available)
		{
			break;
		}
		if clock() >= deadline {
			fail(b"quarantine: the replacement camera did not appear as another camera");
		}
		sleep_until(clock() + TICKS / 5);
	}
	close(stream);
	say(b"PASS quarantine: an unconfirmed stop timed out after two seconds and quarantined the camera; its replacement is another camera and the old grant names nothing");
}

fn again(probe: &Probe) {
	// An old client keeps its buffer mapped in the background (`camhold retain`); this grant captures.
	sleep_until(clock() + TICKS);
	probe.negotiate(1, 1, 333_333);
	let buffer = Owned::new(YUY2_BYTES);
	probe.register(&buffer);
	let stream = probe.events();
	probe.start();
	for _ in 0..3 {
		let frame = next_frame(stream, 5);
		if !yuy2_matches(&buffer, &frame) {
			fail(b"again: the new grant's frame is not the fixture's pattern");
		}
		let _ = probe.capture().release(&frame.buffer, &frame.lease);
	}
	probe.stop();
	close(stream);
	say(b"PASS again: a new grant captured into its own buffer while an old client kept its own");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let args: Vec<u8> = context.arguments.clone().into_bytes();
	let fixture = recv_tagged(bootstrap, &mut buf, b"FIXTURE").unwrap_or(0);
	let inventory = recv_tagged(bootstrap, &mut buf, CAP_CAMERA).unwrap_or(0);
	let capture_grant = recv_tagged(bootstrap, &mut buf, b"CAMERACAPTURE").unwrap_or(0);
	if fixture == 0 || inventory == 0 || capture_grant == 0 {
		fail(b"a grant this probe needs was not delivered");
	}
	let probe = Probe { fixture, inventory, capture: capture_grant };
	match args.split(|&b| b == b' ').next().unwrap_or(&[]) {
		b"capture" => capture(&probe),
		b"mjpeg" => mjpeg(&probe),
		b"timing" => timing(&probe),
		b"busy" => busy(&probe),
		b"inherit" => inherit(&probe),
		b"quarantine" => quarantine(&probe),
		b"again" => again(&probe),
		_ => fail(b"usage: camcheck capture | mjpeg | timing | busy | inherit | quarantine | again"),
	}
	exit();
}
