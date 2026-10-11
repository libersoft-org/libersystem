// Exact production tone, radio_voice write, Stream and PCM Format insertions.
// Memory-object ownership and the synchronous voice-session IPC boundary are host models.
// This is not a kernel, guest, controller, SCO transport or live scheduling test.
#![allow(dead_code, unused_variables, unused_mut)]
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Error {
	Invalid,
	Again,
	NotFound,
}
#[derive(Clone, Copy, Debug)]
struct Buffer {
	handle: u64,
	len: u64,
}
struct ObjectInfo {
	size: u64,
}
struct Object {
	samples: Vec<i16>,
	size: u64,
	mapped: bool,
}
#[derive(Clone, Copy)]
struct ChannelTransport {
	chan: u64,
}
struct PendingWrite;
struct TimerPacer;

#[derive(Default)]
struct Model {
	next: u64,
	objects: BTreeMap<u64, Object>,
	created: usize,
	maps: usize,
	unmaps: usize,
	closed: Vec<u64>,
	deny_next_map: bool,
	stream: Option<Stream>,
	response: Option<Option<Result<u32, Error>>>,
	accepted: Vec<u32>,
}
thread_local! { static MODEL: RefCell<Model> = RefCell::new(Model::default()); }
fn with<R>(f: impl FnOnce(&mut Model) -> R) -> R {
	MODEL.with(|m| f(&mut m.borrow_mut()))
}
fn memory_object_create(bytes: u64) -> i64 {
	with(|m| {
		m.next += 1;
		let handle = m.next;
		m.created += 1;
		m.objects.insert(handle, Object { samples: vec![0; (bytes as usize + 1) / 2], size: bytes, mapped: false });
		handle as i64
	})
}
fn object_info(handle: u64) -> Option<ObjectInfo> {
	with(|m| m.objects.get(&handle).map(|o| ObjectInfo { size: o.size }))
}
unsafe fn map_object(handle: u64) -> Option<u64> {
	with(|m| {
		if m.deny_next_map {
			m.deny_next_map = false;
			return None;
		}
		let object = m.objects.get_mut(&handle)?;
		assert!(!object.mapped, "memory boundary: duplicate mapping");
		object.mapped = true;
		m.maps += 1;
		Some(object.samples.as_mut_ptr() as u64)
	})
}
fn unmap_object(handle: u64) {
	with(|m| {
		let object = m.objects.get_mut(&handle).expect("memory boundary: unmap unknown object");
		assert!(object.mapped, "memory boundary: unmap before mapping");
		object.mapped = false;
		m.unmaps += 1;
	});
}
fn close(handle: u64) {
	with(|m| {
		if handle != 0 {
			let object = m.objects.remove(&handle).expect("memory boundary: duplicate or unknown close");
			assert!(!object.mapped, "memory boundary: closed mapped object");
		}
		m.closed.push(handle);
	});
}
fn fail(message: &str) -> ! {
	panic!("production fail: {message}");
}

/* EXACT_FORMAT */
/* EXACT_STREAM */
/* EXACT_TONE */

mod voice_session {
	use super::*;
	pub struct Client(ChannelTransport);
	impl Client {
		pub fn new(transport: ChannelTransport) -> Self {
			Self(transport)
		}
		pub fn write(&mut self, buffer: &Buffer) -> Option<Result<u32, Error>> {
			assert_eq!(self.0.chan, 99);
			let response = with(|m| m.response.take());
			if let Some(response) = response {
				// A modeled response bypasses service processing, but the transfer boundary
				// still consumes the passed memory capability exactly once.
				close(buffer.handle);
				return response;
			}
			let mut stream = with(|m| m.stream.take().expect("fixture stream absent"));
			let result = stream.write_buffer(*buffer);
			with(|m| {
				if let Ok(accepted) = result {
					m.accepted.push(accepted);
				}
				m.stream = Some(stream);
			});
			Some(result)
		}
	}
}

struct RadioWriter {
	written_frames: usize,
}
impl RadioWriter {
	fn new() -> Self {
		/* EXACT_CURSOR */
		Self { written_frames }
	}
	fn write(&mut self) {
		let mut written_frames = self.written_frames;
		let client = || voice_session::Client::new(ChannelTransport { chan: 99 });
		/* EXACT_WRITE */
		self.written_frames = written_frames;
	}
}

fn expected(frame: usize, channel: usize, distinct: bool) -> i16 {
	let half_period = if distinct && channel == 1 { 12 } else { 24 };
	if frame % (2 * half_period) < half_period { 4000 } else { -4000 }
}
fn reset() {
	with(|m| {
		assert!(m.objects.is_empty(), "memory boundary: unconsumed capability at reset");
		*m = Model::default();
		m.stream = Some(Stream::new(99, Front::Voice(99), Format::new(16_000, 1).unwrap(), None));
	});
}
fn take_stream<R>(f: impl FnOnce(&mut Stream) -> R) -> R {
	let mut stream = with(|m| m.stream.take().unwrap());
	let result = f(&mut stream);
	with(|m| m.stream = Some(stream));
	result
}
fn drain(frames: usize, played: &mut Vec<i16>) {
	take_stream(|stream| {
		for _ in 0..frames {
			let Some((left, right)) = stream.next_frame(16_000) else { break };
			assert_eq!(left, right, "mono source must be duplicated by actual Format");
			played.push(left);
		}
		stream.compact();
	});
}
fn assert_continuity(played: &[i16]) {
	for (at, &sample) in played.iter().enumerate() {
		assert_eq!(sample, expected(at, 0, false), "VOICE_PARTIAL_CONTINUITY: the next accepted prefix must resume at the next source frame; played index {at}");
	}
}
fn assert_released() {
	with(|m| {
		assert!(m.objects.is_empty(), "every transferred buffer must be closed");
		assert_eq!(m.maps, m.unmaps, "every successful map must be unmapped");
		assert_eq!(m.created, m.closed.iter().filter(|&&h| h != 0).count(), "exactly one close per memory object");
	});
}
fn continuity() {
	reset();
	let mut writer = RadioWriter::new();
	let mut played = Vec::new();
	// Voice reads expose 160 source frames at 16 kHz; a 480-frame producer
	// eventually fills the real 4096-frame stream and must accept prefixes.
	for _ in 0..100 {
		writer.write();
		drain(160, &mut played);
	}
	let remaining = take_stream(|stream| stream.queued_frames());
	drain(remaining, &mut played);
	// Retain the entire modeled playback even for the original causal failure.
	// The Python driver evaluates these bytes with the unchanged independent
	// peer audio_metrics function before accepting any negative verdict.
	if let Some(path) = std::env::args().nth(2) {
		let bytes: Vec<u8> = played.iter().flat_map(|sample| sample.to_le_bytes()).collect();
		std::fs::write(path, bytes).unwrap();
	}
	with(|m| {
		assert_eq!(&m.accepted[..12], &[480; 12]);
		assert_eq!(m.accepted[12], 256, "actual 4096-frame capacity must create the first partial write");
		assert!(m.accepted[13..].iter().all(|&n| n == 160));
		assert_eq!(played.len(), m.accepted.iter().map(|&n| n as usize).sum::<usize>());
		println!("VOICE_PARTIAL_TRACE capacity={} iterations={} first_partial_index=12 first_partial_frames={} following_prefix_frames=160 played_samples={}", MAX_QUEUED_FRAMES, m.accepted.len(), m.accepted[12], played.len());
	});
	assert_released();
	assert_continuity(&played);
	assert_eq!(writer.written_frames, played.len(), "cursor counts accepted source frames only");
	println!("VOICE_PARTIAL_4096_160_CONTINUITY_PASS");
}
fn prefixes() {
	reset();
	let mut writer = RadioWriter::new();
	let mut played = Vec::new();
	// Fill the actual stream without draining: eight 480s then one 256.
	for _ in 0..9 {
		writer.write();
	}
	assert_eq!(take_stream(|stream| stream.queued_frames()), MAX_QUEUED_FRAMES);
	for frames in [1, 17, 159, 160, 479, 480, 3, 47, 48, 49] {
		drain(frames, &mut played);
		writer.write();
		assert_eq!(with(|m| *m.accepted.last().unwrap()), frames as u32);
		assert_eq!(take_stream(|stream| stream.queued_frames()), MAX_QUEUED_FRAMES);
	}
	drain(MAX_QUEUED_FRAMES, &mut played);
	assert_continuity(&played);
	assert_eq!(writer.written_frames, played.len());
	assert_released();
	println!("VOICE_PARTIAL_ODD_PREFIX_CONTINUITY_PASS");
}
fn full_writes() {
	reset();
	let mut writer = RadioWriter::new();
	let mut played = Vec::new();
	for _ in 0..40 {
		writer.write();
		drain(480, &mut played);
	}
	assert_continuity(&played);
	assert_eq!(writer.written_frames, 19_200);
	with(|m| assert!(m.accepted.iter().all(|&n| n == 480)));
	assert_released();
	// Existing callers retain phase-zero tone and distinct stereo behavior.
	for channels in [1, 2] {
		for distinct in [false, true] {
			let buffer = tone_channels(97, channels, distinct);
			with(|m| {
				let object = m.objects.get(&buffer.handle).unwrap();
				assert_eq!(buffer.len, (97 * channels * 2) as u64);
				for (at, &sample) in object.samples.iter().enumerate() {
					assert_eq!(sample, expected(at / channels, at % channels, distinct));
				}
			});
			close(buffer.handle);
		}
	}
	assert_released();
	println!("VOICE_PARTIAL_FULL_WRITES_AND_LEGACY_TONES_PASS");
}
fn refused_replies() {
	for (label, response) in [("VOICE_PARTIAL_ZERO_REFUSAL", Some(Ok(0))), ("VOICE_PARTIAL_SERVICE_ERROR_REFUSAL", Some(Err(Error::Again))), ("VOICE_PARTIAL_CHANNEL_ERROR_REFUSAL", None)] {
		reset();
		let mut writer = RadioWriter::new();
		writer.written_frames = 257;
		with(|m| m.response = Some(response));
		let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.write()));
		assert!(result.is_err(), "{label}: invalid write reply must retain production failure behavior");
		assert_eq!(writer.written_frames, 257, "{label}: a rejected reply must not advance source frames");
		assert_released();
	}
	println!("VOICE_PARTIAL_BAD_REPLY_REFUSAL_PASS");
}
fn buffer_errors() {
	for scenario in ["closing", "odd-length", "oversized", "empty", "map-failure", "full"] {
		reset();
		let mut buffer = tone(480, 1);
		let expected_error = if scenario == "full" { Error::Again } else { Error::Invalid };
		match scenario {
			"closing" => take_stream(|s| s.closing = true),
			"odd-length" => buffer.len -= 1,
			"oversized" => buffer.len += 2,
			"empty" => buffer.len = 0,
			"map-failure" => with(|m| m.deny_next_map = true),
			"full" => take_stream(|s| s.append(&vec![0; MAX_QUEUED_FRAMES])),
			_ => unreachable!(),
		}
		let before = take_stream(|s| s.queued_frames());
		assert_eq!(take_stream(|s| s.write_buffer(buffer)), Err(expected_error), "{scenario}");
		assert_eq!(take_stream(|s| s.queued_frames()), before, "refused buffer must not append samples: {scenario}");
		assert_released();
	}
	reset();
	assert_eq!(take_stream(|s| s.write_buffer(Buffer { handle: 0, len: 960 })), Err(Error::Invalid));
	assert_released();
	println!("VOICE_PARTIAL_ACTUAL_STREAM_BUFFER_REFUSAL_PASS");
}
fn main() {
	let selected = std::env::args().nth(1).unwrap_or_else(|| "all".into());
	match selected.as_str() {
		"continuity" => continuity(),
		"refusals" => refused_replies(),
		"all" => {
			continuity();
			prefixes();
			full_writes();
			refused_replies();
			buffer_errors();
			println!("VOICE_PARTIAL_COMPLETE");
		}
		_ => panic!("unknown fixture selection"),
	}
}
