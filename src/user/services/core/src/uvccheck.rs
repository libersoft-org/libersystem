// Development USB camera client: ordinary owner-bound capture through CameraService, with no fixture control.
// The far-end USB model supplies independently checked frame bytes, loss and an unplug.
#![no_std]
#![no_main]
extern crate alloc;

use alloc::{format, vec::Vec};
use ipc_client::ChannelTransport;
use proto::system::{BindingState, CameraId, CaptureEvent, Error, Frame, FrameType, Interval, LaunchContext, PolicyOutcome, PolicyVerb, StreamRequest, camera, camera_capture, device, device_policy_admin};
use rt::*;
use services::capability_names::*;

const BYTES: usize = 64 * 48 * 2;
fn say(text: &str) {
	print(format!("uvccheck: {text}\n").as_bytes());
}
fn fail(text: &str) -> ! {
	say(&format!("FAIL {text}"));
	exit();
}
fn require<T>(result: Option<Result<T, Error>>, what: &str) -> T {
	match result {
		Some(Ok(value)) => value,
		other => {
			let _ = other;
			fail(what)
		}
	}
}
fn capture(channel: u64) -> camera_capture::Client<ChannelTransport> {
	camera_capture::Client::with_deadline(ChannelTransport { chan: channel }, clock() + 10 * TICKS_PER_SECOND)
}

struct Buffer {
	handle: u64,
	base: u64,
	id: u8,
}
impl Buffer {
	fn new(channel: u64) -> Self {
		let handle = memory_object_create(BYTES as u64);
		if handle < 0 {
			fail("client buffer allocation");
		}
		let handle = handle as u64;
		let base = unsafe { map_object(handle) }.unwrap_or_else(|| fail("client buffer mapping"));
		let lent = duplicate(handle, RIGHT_MAP | RIGHT_READ | RIGHT_WRITE | RIGHT_TRANSFER);
		if lent < 0 {
			fail("client buffer duplicate");
		}
		let id = require(capture(channel).register(&(lent as u64)), "buffer registration");
		Self { handle, base, id }
	}
	fn bytes(&self) -> &[u8] {
		// The object is mapped for BYTES until the explicit cleanup, and read only while the client owns its lease.
		unsafe { core::slice::from_raw_parts(self.base as *const u8, BYTES) }
	}
}
fn next_frame(stream: u64) -> (Frame, u64) {
	let until = clock() + 10 * TICKS_PER_SECOND;
	let mut buf = [0u8; 512];
	loop {
		if wait(stream, until) != 0 {
			fail("frame deadline or stream closed");
		}
		match try_recv_caps(stream, &mut buf) {
			PolledCaps::Message { len, mut handles } => {
				let received_ns = clock_ns();
				let event = camera_capture::events_read(&buf[..len], &mut handles);
				for &handle in handles.as_slice() {
					close(handle);
				}
				match event {
					Some(CaptureEvent::Frame(frame)) => return (frame, received_ns),
					Some(CaptureEvent::Status(status)) if status.ended.is_none() => {}
					_ => fail("invalid or terminal event before the frame"),
				}
			}
			PolledCaps::Empty => {}
			PolledCaps::Closed => fail("frame stream closed"),
		}
	}
}
fn verify(buffers: &[Buffer], frame: &Frame, generation: u64, received_ns: u64) -> u64 {
	let buffer = buffers.iter().find(|buffer| buffer.id == frame.buffer).unwrap_or_else(|| fail("foreign buffer"));
	if frame.stream_generation != generation || frame.valid_bytes as usize != BYTES || frame.device.is_some() || frame.arrival_ns > received_ns {
		fail("frame generation, size or invented timestamp");
	}
	let bytes = buffer.bytes();
	let number = u64::from_le_bytes(bytes[..8].try_into().unwrap());
	if number == 2 || number == 3 || !bytes[8..].iter().enumerate().all(|(at, byte)| *byte == (((at + 8) as u64 * 7 + number * 13) % 251) as u8) {
		fail("damaged USB frame or wrong far-end bytes");
	}
	number
}
fn release(channel: u64, frame: &Frame) {
	require(capture(channel).release(&frame.buffer, &frame.lease), "lease release");
}
fn inventory(channel: u64) -> Vec<proto::system::CameraInfo> {
	require(camera::Client::with_deadline(ChannelTransport { chan: channel }, clock() + 5 * TICKS_PER_SECOND).cameras(), "inventory")
}
fn await_retired(channel: u64) {
	let until = clock() + 20 * TICKS_PER_SECOND;
	loop {
		match channel_peek(channel) {
			ERR_PEER_CLOSED => return,
			ERR_WOULD_BLOCK => {}
			_ => fail("unexpected capture reply while awaiting retirement"),
		}
		if clock() >= until {
			fail("old grant survived camera withdrawal");
		}
		sleep_until(clock() + 1);
	}
}
fn apply(policy: u64, index: u32, verb: PolicyVerb) {
	let until = clock() + 20 * TICKS_PER_SECOND;
	loop {
		match device_policy_admin::Client::with_deadline(ChannelTransport { chan: policy }, until).apply(&index, &verb, "") {
			Some(Ok(PolicyOutcome::Accepted)) => return,
			Some(Ok(PolicyOutcome::Busy)) if clock() < until => sleep_until(clock() + 5),
			_ => fail("controller policy operation"),
		}
	}
}
fn cycle(device_channel: u64, policy: u64, reader: u64, channel: u64, old: &CameraId, buffers: &[Buffer]) {
	let bindings = require(device::Client::new(ChannelTransport { chan: device_channel }).bindings(), "device bindings");
	let controllers: Vec<_> = bindings.iter().filter(|binding| binding.artifact == "xhci").collect();
	let [controller] = controllers.as_slice() else {
		fail("expected exactly one xHCI controller");
	};
	let started = clock_ns();
	apply(policy, controller.index, PolicyVerb::Disable);
	await_retired(channel);
	let retained: Vec<_> = buffers.iter().map(|buffer| buffer.bytes().to_vec()).collect();
	let until = clock() + 30 * TICKS_PER_SECOND;
	loop {
		let bindings = require(device::Client::new(ChannelTransport { chan: device_channel }).bindings(), "disabled device bindings");
		if bindings.iter().any(|binding| binding.index == controller.index && binding.state == BindingState::Disabled) {
			break;
		}
		if clock() >= until {
			fail("controller never disabled");
		}
		sleep_until(clock() + 5);
	}
	apply(policy, controller.index, PolicyVerb::Enable);
	let until = clock() + 30 * TICKS_PER_SECOND;
	loop {
		let fresh = inventory(reader);
		if fresh.len() == 1 && fresh[0].id != *old && fresh[0].available {
			break;
		}
		if clock() >= until {
			fail("camera replacement never appeared");
		}
		sleep_until(clock() + 5);
	}
	if channel_peek(channel) != ERR_PEER_CLOSED {
		fail("old grant rebound to replacement");
	}
	if buffers.iter().zip(&retained).any(|(buffer, bytes)| buffer.bytes() != bytes) {
		fail("replacement wrote into a retired client's buffer");
	}
	say(&format!("PASS cycle: active capture revoked, replacement admitted in {} us, old grant stays closed", (clock_ns() - started) / 1000));
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	inherit_stdout(bootstrap);
	let context = recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode).unwrap_or_else(|| fail("launch context"));
	let mut buf = [0u8; 256];
	let device_channel = recv_tagged(bootstrap, &mut buf, b"DEVICE").unwrap_or(0);
	let policy = recv_tagged(bootstrap, &mut buf, b"DEVPOLICY").unwrap_or(0);
	let reader = recv_tagged(bootstrap, &mut buf, CAP_CAMERA).unwrap_or(0);
	let channel = recv_tagged(bootstrap, &mut buf, b"CAMERACAPTURE").unwrap_or(0);
	if [device_channel, policy, reader, channel].contains(&0) {
		fail("required scoped grant absent");
	}
	let mode = context.arguments.trim();
	if !matches!(mode, "capture" | "cycle" | "unplug") {
		fail("usage: uvccheck capture | cycle | unplug");
	}
	let info = require(capture(channel).camera(), "capture camera identity");
	if !inventory(reader).iter().any(|camera| camera.id == info.id) {
		fail("capture identity missing from inventory");
	}
	if mode == "capture" {
		// An inventory-only connection cannot start this real USB provider. Use a fresh
		// connection because the service deliberately closes it on a capture operation.
		let inventory_only = service_connect(reader).unwrap_or_else(|| fail("inventory-only connection"));
		if matches!(capture(inventory_only).start(), Some(Ok(()))) {
			fail("inventory authority started capture");
		}
		await_retired(inventory_only);
		close(inventory_only);
		if inventory(reader).iter().any(|camera| camera.streaming) {
			fail("camera streamed without a capture start");
		}
		say("PASS inventory: capture operation denied and USB stream remains stopped");
	}
	let negotiated = require(capture(channel).negotiate(&StreamRequest { format: 1, size: 1, interval: Interval { numerator: 333_333, denominator: 10_000_000 } }), "negotiate");
	if negotiated.format != 1 || negotiated.interval != (Interval { numerator: 333_333, denominator: 10_000_000 }) || negotiated.frame_type != FrameType::Yuy2 || (negotiated.width, negotiated.height, negotiated.stride, negotiated.plane_offset, negotiated.max_bytes) != (64, 48, 128, 0, BYTES as u32) {
		fail("negotiated another format");
	}
	let buffers: Vec<Buffer> = (0..2).map(|_| Buffer::new(channel)).collect();
	let stream = capture(channel).events().unwrap_or_else(|| fail("capture event subscription"));
	require(capture(channel).start(), "start");
	let (first, received) = next_frame(stream);
	let first_number = verify(&buffers, &first, negotiated.stream_generation, received);
	if mode == "capture" {
		let (second, received) = next_frame(stream);
		let second_number = verify(&buffers, &second, negotiated.stream_generation, received);
		if first.buffer == second.buffer || second.sequence <= first.sequence || second_number <= first_number || second.arrival_ns < first.arrival_ns {
			fail("leased buffer reused or frame order moved backwards");
		}
		let retained: Vec<_> = buffers.iter().map(|buffer| buffer.bytes().to_vec()).collect();
		sleep_until(clock() + TICKS_PER_SECOND / 2);
		if buffers.iter().zip(&retained).any(|(buffer, bytes)| buffer.bytes() != bytes) {
			fail("leased bytes overwritten");
		}
		let status = require(capture(channel).status(), "retained-buffer status");
		if status.dropped_no_buffer == 0 {
			fail("unreported no-buffer loss");
		}
		release(channel, &first);
		release(channel, &second);
		let started = clock_ns();
		let mut delays = Vec::new();
		let mut previous = second_number;
		let mut sequence = second.sequence;
		let mut arrival = second.arrival_ns;
		while clock_ns() - started < 2_000_000_000 {
			let (frame, received) = next_frame(stream);
			let number = verify(&buffers, &frame, negotiated.stream_generation, received);
			if number <= previous || frame.sequence <= sequence || frame.arrival_ns < arrival {
				fail("frame order");
			}
			previous = number;
			sequence = frame.sequence;
			arrival = frame.arrival_ns;
			delays.push(received - frame.arrival_ns);
			release(channel, &frame);
		}
		let elapsed = clock_ns() - started;
		if delays.is_empty() {
			fail("no measured frames");
		}
		let status = require(capture(channel).status(), "measured status");
		require(capture(channel).stop(), "confirmed stop");
		let stopped: Vec<_> = buffers.iter().map(|buffer| buffer.bytes().to_vec()).collect();
		sleep_until(clock() + TICKS_PER_SECOND / 5);
		if buffers.iter().zip(&stopped).any(|(buffer, bytes)| buffer.bytes() != bytes) {
			fail("buffer changed after confirmed stop");
		}
		delays.sort_unstable();
		say(&format!("PASS capture: {} frames in {} us, client buffers={} bytes, completion latency us min={} median={} max={}, no-buffer drops={}, device drops={}, discontinuities={}; leased and stopped bytes stable", delays.len(), elapsed / 1000, 2 * BYTES, delays[0] / 1000, delays[delays.len() / 2] / 1000, delays[delays.len() - 1] / 1000, status.dropped_no_buffer, status.dropped_device, status.unknown_discontinuities));
	} else if mode == "cycle" {
		cycle(device_channel, policy, reader, channel, &info.id, &buffers);
	} else {
		await_retired(channel);
		if inventory(reader).iter().any(|camera| camera.id == info.id) {
			fail("unplugged camera remains in inventory");
		}
		let retained: Vec<_> = buffers.iter().map(|buffer| buffer.bytes().to_vec()).collect();
		sleep_until(clock() + TICKS_PER_SECOND / 5);
		if buffers.iter().zip(&retained).any(|(buffer, bytes)| buffer.bytes() != bytes) {
			fail("unplugged camera still writes");
		}
		say("PASS unplug: replacement captured exact USB bytes, unplug retired its grant and stopped buffer writes");
	}
	close(stream);
	for buffer in buffers {
		unmap_object(buffer.handle);
		close(buffer.handle);
	}
	for held in [channel, reader, policy, device_channel] {
		close(held);
	}
	exit();
}
