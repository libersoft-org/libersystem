// audioprobe - the in-guest scenario driver for the audio device model's gate. DEVELOPMENT-ONLY.
//
// It holds what an application would - a playback stream, a recorder and a voice session, each from its own grant -
// and the operator authority `audioctl` holds, through which it reads where each of them plays. The gate plugs and
// unplugs a device from the host while `hold` runs; the verdict on each claim is the service's own inventory and
// stream placement, read through that authority.
//
//   audioprobe inventory   every catalogue provider is a device, with its formats and latency, and there is one
//                          default output
//   audioprobe hold        a stream plays on the default; a device plugged in takes it, and when it is unplugged the
//                          stream returns to the device it left - the stream answered throughout, never ended
//   audioprobe operator    the operator's default and level: a device chosen takes the default output and a stream
//                          that follows it, a stream that named a device stays there, and a level is kept per device
//   audioprobe voice       a voice session at 16 kHz on the default output and input: written, read, its call state
//                          set, its latency stated; the voice grant opens nothing else and a stream grant no voice

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::codec::Buffer;
use proto::system::{AudioDevice, AudioDirection, AudioStreamInfo, CallCommand, CallState, Error, LaunchContext, audio, audio_control, pcm_capture, pcm_stream, voice_session};
use rt::*;

const TICKS: u64 = 100;

fn say(line: &str) {
	print(format!("audioprobe: {line}\n").as_bytes());
}

fn fail(line: &str) -> ! {
	print(format!("audioprobe: FAIL {line}\n").as_bytes());
	exit_with(1);
}

struct Probe {
	stream: u64,
	capture: u64,
	control: u64,
	voice: u64,
}

impl Probe {
	fn control(&self) -> audio_control::Client<ChannelTransport> {
		audio_control::Client::new(ChannelTransport { chan: self.control })
	}

	fn devices(&self) -> Vec<AudioDevice> {
		self.control().devices().unwrap_or_else(|| fail("the inventory could not be read"))
	}

	fn streams(&self) -> Vec<AudioStreamInfo> {
		self.control().streams().unwrap_or_else(|| fail("the streams could not be read"))
	}

	fn default_output(&self) -> Option<u32> {
		self.devices().into_iter().find(|device| device.default_output).map(|device| device.id)
	}

	// THE ONE STREAM THIS PROBE HOLDS, as the operator sees it: the only one that is neither a voice session's nor the
	// phone's.
	fn mine(&self) -> AudioStreamInfo {
		let streams: Vec<AudioStreamInfo> = self.streams().into_iter().filter(|stream| !stream.voice && !stream.route).collect();
		if streams.len() != 1 {
			fail(&format!("{} streams were listed where this probe holds one", streams.len()));
		}
		streams[0].clone()
	}

	fn open(&self, named: Option<u32>) -> u64 {
		let mut client = audio::Client::new(ChannelTransport { chan: self.stream });
		let answer = match named {
			Some(device) => client.open_stream_to(&device, &48_000, &2),
			None => client.open_stream(&48_000, &2),
		};
		match answer {
			Some(Ok(stream)) => stream,
			other => fail(&format!("a stream could not be opened: {other:?}")),
		}
	}
}

// Some frames of a square wave, as a buffer the write carries.
fn tone(frames: usize, channels: usize) -> Buffer {
	tone_channels(frames, channels, false)
}

fn tone_channels(frames: usize, channels: usize, distinct: bool) -> Buffer {
	let bytes = frames * channels * 2;
	let handle = memory_object_create(bytes as u64);
	if handle < 0 {
		fail("no memory for a period");
	}
	let handle = handle as u64;
	let Some(base) = (unsafe { map_object(handle) }) else { fail("the period could not be mapped") };
	let samples = unsafe { core::slice::from_raw_parts_mut(base as *mut i16, frames * channels) };
	for (at, sample) in samples.iter_mut().enumerate() {
		let half_period = if distinct && at % channels == 1 { 12 } else { 24 };
		*sample = if (at / channels / half_period) % 2 == 0 { 4_000 } else { -4_000 };
	}
	unmap_object(handle);
	Buffer { handle, len: bytes as u64 }
}

// One write, answered: the stream is alive and accepting.
fn write(stream: u64, frames: usize) {
	match pcm_stream::Client::new(ChannelTransport { chan: stream }).write(&tone(frames, 2)) {
		Some(Ok(accepted)) if accepted > 0 => {}
		other => fail(&format!("a write was not accepted: {other:?}")),
	}
}

// ------------------------------------------------------------------ inventory

fn inventory(probe: &Probe) {
	let devices = probe.devices();
	if devices.is_empty() {
		fail("no device in the inventory");
	}
	for device in &devices {
		if device.output.is_none() && device.input.is_none() {
			fail("a device serves no direction");
		}
		say(&format!("device {} {} out {:?} in {:?} latency {} us level {}", device.id, device.label, device.output.as_ref().map(|format| (format.rate, format.channels)), device.input.as_ref().map(|format| (format.rate, format.channels)), device.latency_us, device.volume));
	}
	if devices.iter().filter(|device| device.default_output).count() != 1 {
		fail("there is not exactly one default output");
	}
	say(&format!("PASS inventory - {} devices, one default output", devices.len()));
}

// ------------------------------------------------------------------ hold

fn hold(probe: &Probe) {
	let before: Vec<u32> = probe.devices().into_iter().map(|device| device.id).collect();
	let home = probe.default_output().unwrap_or_else(|| fail("no default output to play on"));
	let stream = probe.open(None);
	write(stream, 4_096);
	let deadline = clock() + 2 * TICKS;
	while probe.mine().device != Some(home) {
		if clock() >= deadline {
			fail("the stream did not play on the default output");
		}
		sleep_until(clock() + TICKS / 10);
	}
	let moves = probe.control().counters().map_or(0, |counters| counters.moves);
	say(&format!("playing on device {home}, waiting for a device to arrive"));
	// THE GATE PLUGS ONE IN. The stream is fed meanwhile, so what moves is a stream that is playing.
	let deadline = clock() + 60 * TICKS;
	let arrived = loop {
		write(stream, 1_024);
		if let Some(device) = probe.devices().into_iter().find(|device| !before.contains(&device.id)) {
			break device;
		}
		if clock() >= deadline {
			fail("no device arrived");
		}
	};
	if !arrived.default_output {
		fail("the device that arrived did not become the default output");
	}
	let deadline = clock() + 2 * TICKS;
	while probe.mine().device != Some(arrived.id) {
		write(stream, 512);
		if clock() >= deadline {
			fail("the stream did not move to the device that arrived");
		}
	}
	say(&format!("the stream moved to the device that arrived (device {}), and is waiting for it to leave", arrived.id));
	// AND UNPLUGS IT.
	let deadline = clock() + 60 * TICKS;
	loop {
		write(stream, 1_024);
		if !probe.devices().iter().any(|device| device.id == arrived.id) {
			break;
		}
		if clock() >= deadline {
			fail("the device that arrived never left");
		}
	}
	if probe.default_output() != Some(home) {
		fail("the default output did not return to the device the arrival displaced");
	}
	let deadline = clock() + 2 * TICKS;
	while probe.mine().device != Some(home) {
		write(stream, 512);
		if clock() >= deadline {
			fail("the stream did not return to the device it left");
		}
	}
	write(stream, 512);
	let counted = probe.control().counters().map_or(0, |counters| counters.moves);
	if counted < moves + 2 {
		fail("the two moves were not counted");
	}
	say(&format!("the stream returned to device {home} when the device left, and answered every write throughout"));
	let _ = pcm_stream::Client::new(ChannelTransport { chan: stream }).close();
	close(stream);
	say("PASS hold");
}

// ------------------------------------------------------------------ operator

fn operator(probe: &Probe) {
	let devices = probe.devices();
	let outputs: Vec<u32> = devices.iter().filter(|device| device.output.is_some() && !device.voice && !device.route).map(|device| device.id).collect();
	if outputs.len() < 2 {
		fail("the operator's choice needs two outputs");
	}
	let home = probe.default_output().unwrap_or_else(|| fail("no default output"));
	let other = *outputs.iter().find(|id| **id != home).unwrap_or_else(|| fail("no second output"));
	// A STREAM THAT NAMED THE HOME DEVICE stays on it; the one that follows moves with the operator's choice.
	let named = probe.open(Some(home));
	write(named, 2_048);
	if !matches!(probe.control().set_default(&other, &AudioDirection::Output), Some(Ok(()))) {
		fail("the operator could not choose the default output");
	}
	if probe.default_output() != Some(other) {
		fail("the chosen device is not the default output");
	}
	let streams = probe.streams();
	if !streams.iter().any(|stream| stream.named == Some(home) && stream.device == Some(home)) {
		fail("a stream that named a device did not stay on it");
	}
	say(&format!("device {other} chosen as the default output; a stream that named device {home} stayed there"));
	let _ = pcm_stream::Client::new(ChannelTransport { chan: named }).close();
	close(named);
	let follower = probe.open(None);
	write(follower, 2_048);
	let deadline = clock() + 2 * TICKS;
	while !probe.streams().iter().any(|stream| stream.named.is_none() && stream.device == Some(other) && !stream.voice) {
		if clock() >= deadline {
			fail("a stream that follows the default did not play on the chosen one");
		}
		sleep_until(clock() + TICKS / 10);
	}
	let latency = pcm_stream::Client::new(ChannelTransport { chan: follower }).latency().unwrap_or(0);
	if latency == 0 {
		fail("the stream stated no latency");
	}
	say(&format!("a stream that follows the default plays on it, its latency {latency} us"));
	let _ = pcm_stream::Client::new(ChannelTransport { chan: follower }).close();
	close(follower);
	// THE LEVEL IS THE DEVICE'S: kept for it, and not for the other.
	if !matches!(probe.control().set_volume(&other, &40), Some(Ok(()))) {
		fail("the level could not be set");
	}
	if !matches!(probe.control().set_volume(&other, &101), Some(Err(Error::Invalid))) {
		fail("a level past 100 was not refused");
	}
	let devices = probe.devices();
	let level = |id: u32| devices.iter().find(|device| device.id == id).map_or(0, |device| device.volume);
	if level(other) != 40 || level(home) == 40 {
		fail("the level was not kept per device");
	}
	let _ = probe.control().set_volume(&other, &100);
	let _ = probe.control().set_default(&home, &AudioDirection::Output);
	say(&format!("a level of 40 kept for device {other} alone, and a level past 100 refused"));
	say("PASS operator");
}

// ------------------------------------------------------------------ voice

fn voice(probe: &Probe) {
	// THE GRANTS ARE KEPT APART: the voice grant opens no stream, the stream grant no voice session.
	if !matches!(audio::Client::new(ChannelTransport { chan: probe.voice }).open_stream(&48_000, &2), Some(Err(Error::Denied))) {
		fail("the voice grant opened a playback stream");
	}
	if !matches!(audio::Client::new(ChannelTransport { chan: probe.stream }).open_voice(&16_000), Some(Err(Error::Denied))) {
		fail("the stream grant opened a voice session");
	}
	if !matches!(audio::Client::new(ChannelTransport { chan: probe.voice }).open_voice(&44_100), Some(Err(Error::Invalid))) {
		fail("a voice session at a music rate was not refused");
	}
	let session = match audio::Client::new(ChannelTransport { chan: probe.voice }).open_voice(&16_000) {
		Some(Ok(session)) => session,
		other => fail(&format!("the voice session could not be opened: {other:?}")),
	};
	let client = || voice_session::Client::new(ChannelTransport { chan: session });
	say("a voice session opens only from the voice grant, at 8 or 16 kHz");
	for _ in 0..4 {
		match client().write(&tone(320, 1)) {
			Some(Ok(accepted)) if accepted > 0 => {}
			other => fail(&format!("the session's write was not accepted: {other:?}")),
		}
	}
	let output = probe.default_output();
	let streams = probe.streams();
	if !streams.iter().any(|stream| stream.voice && stream.device == output && stream.format.rate == 16_000 && stream.format.channels == 1) {
		fail("the session's playback is not on the default output at 16 kHz mono");
	}
	// ITS CAPTURE HALF: a period of what the default input heard, converted to the session's rate.
	match client().read() {
		Some(Ok(period)) if !period.is_empty() && period.len() % 2 == 0 && period.len() < 2_048 => say(&format!("the session read {} bytes from the default input, at 16 kHz mono", period.len())),
		Some(Err(Error::NotFound)) => say("the default input refused to capture; the session said so rather than waiting"),
		other => fail(&format!("the session's read answered {other:?}")),
	}
	if !matches!(client().set_call(&CallState::Active), Some(Ok(()))) {
		fail("the call state could not be set");
	}
	let commands = match client().commands() {
		Some(Ok(stream)) => stream,
		other => fail(&format!("the commands stream was refused: {other:?}")),
	};
	let latency = client().latency().unwrap_or(0);
	if latency == 0 {
		fail("the session stated no latency");
	}
	say(&format!("the call declared active, the commands stream open, the latency {latency} us"));
	let _ = client().set_call(&CallState::None);
	let _ = client().close();
	close(commands);
	close(session);
	// AND A RECORDER ON ITS OWN GRANT, beside it: one period, converted.
	let capture = match audio::Client::new(ChannelTransport { chan: probe.capture }).open_capture(&8_000, &1) {
		Some(Ok(capture)) => capture,
		other => fail(&format!("the recorder could not be opened: {other:?}")),
	};
	match pcm_capture::Client::new(ChannelTransport { chan: capture }).read() {
		Some(Ok(period)) if !period.is_empty() => {}
		Some(Err(Error::NotFound)) => {}
		other => fail(&format!("the recorder's read answered {other:?}")),
	}
	close(capture);
	say("PASS voice");
}

// Independent host peers consume this audio and answer the call. This driver uses the same grants as an
// application; neither it nor AudioService can manufacture the host's received packets or codec verdict.
fn radio_music(probe: &Probe, stereo: bool) {
	let stream = probe.open(None);
	say(if stereo { "radio-stereo started" } else { "radio-music started" });
	for _ in 0..300 {
		match pcm_stream::Client::new(ChannelTransport { chan: stream }).write(&tone_channels(480, 2, stereo)) {
			Some(Ok(accepted)) if accepted > 0 => {}
			other => fail(&format!("a radio music write was not accepted: {other:?}")),
		}
	}
	let _ = pcm_stream::Client::new(ChannelTransport { chan: stream }).close();
	close(stream);
	say(if stereo { "PASS radio-stereo" } else { "PASS radio-music" });
}

fn radio_voice(probe: &Probe) {
	let session = match audio::Client::new(ChannelTransport { chan: probe.voice }).open_voice(&16_000) {
		Some(Ok(session)) => session,
		other => fail(&format!("the radio voice session could not be opened: {other:?}")),
	};
	let client = || voice_session::Client::new(ChannelTransport { chan: session });
	let commands = match client().commands() {
		Some(Ok(commands)) => commands,
		other => fail(&format!("the radio call command stream was refused: {other:?}")),
	};
	if !matches!(client().set_call(&CallState::Incoming), Some(Ok(()))) {
		fail("the radio incoming call could not be declared");
	}
	say("radio-voice incoming");
	let mut answered = false;
	let mut hung_up = false;
	let mut heard = 0usize;
	let mut energy = 0u64;
	let mut crossings = 0usize;
	let mut previous = 0i16;
	let deadline = clock() + 30 * TICKS;
	while !hung_up {
		if clock() >= deadline {
			fail("the independent peer did not answer and hang up the radio call");
		}
		match client().write(&tone(480, 1)) {
			Some(Ok(accepted)) if accepted > 0 => {}
			other => fail(&format!("the radio voice write failed: {other:?}")),
		}
		if let Some(Ok(period)) = client().read() {
			for pair in period.chunks_exact(2) {
				let sample = i16::from_le_bytes([pair[0], pair[1]]);
				crossings += usize::from(previous <= 0 && sample > 0);
				previous = sample;
				energy += (i64::from(sample) * i64::from(sample)) as u64;
				heard += 1;
			}
		}
		let mut bytes = [0u8; 64];
		while let PolledCaps::Message { len, handles } = try_recv_caps(commands, &mut bytes) {
			let mut handles = handles;
			match voice_session::commands_read(&bytes[..len], &mut handles) {
				Some(CallCommand::Answer) if !answered => {
					answered = true;
					if !matches!(client().set_call(&CallState::Active), Some(Ok(()))) {
						fail("the answered radio call could not become active");
					}
					say("radio-voice answered");
				}
				Some(CallCommand::HangUp) if answered => hung_up = true,
				_ => {}
			}
			for &handle in handles.as_slice() {
				close(handle);
			}
		}
	}
	let _ = client().set_call(&CallState::None);
	let _ = client().close();
	close(commands);
	close(session);
	if heard < 8_000 || energy / (heard as u64) < 100_000 {
		fail(&format!("the independent microphone did not reach the session: {heard} samples, energy {energy}"));
	}
	let frequency = crossings * 16_000 / heard;
	if !(1_300..=1_700).contains(&frequency) {
		fail(&format!("the independent microphone tone was not 1500 Hz: {frequency} Hz over {heard} samples"));
	}
	say(&format!("PASS radio-voice samples {heard} mean-square {} frequency {frequency}", energy / heard as u64));
}

// The independent headset holds its real SCO acceptance while this application owns the voice
// endpoint. Its HFP hangup closes that ownership before the held setup is allowed to complete.
fn radio_voice_close(probe: &Probe) {
	let session = match audio::Client::new(ChannelTransport { chan: probe.voice }).open_voice(&16_000) {
		Some(Ok(session)) => session,
		other => fail(&format!("the pending voice session could not be opened: {other:?}")),
	};
	let client = || voice_session::Client::new(ChannelTransport { chan: session });
	let commands = match client().commands() {
		Some(Ok(commands)) => commands,
		other => fail(&format!("the pending voice command stream was refused: {other:?}")),
	};
	if !matches!(client().set_call(&CallState::Active), Some(Ok(()))) {
		fail("the pending voice call could not be declared");
	}
	say("radio-voice-close waiting");
	let deadline = clock() + 20 * TICKS;
	let mut bytes = [0u8; 64];
	loop {
		match try_recv_caps(commands, &mut bytes) {
			PolledCaps::Message { len, handles } => {
				let mut handles = handles;
				let command = voice_session::commands_read(&bytes[..len], &mut handles);
				for &handle in handles.as_slice() {
					close(handle);
				}
				if command == Some(CallCommand::HangUp) {
					break;
				}
			}
			PolledCaps::Closed => fail("the pending voice command stream closed before hangup"),
			PolledCaps::Empty => {}
		}
		if clock() >= deadline {
			fail("the independent headset did not hang up the pending voice session");
		}
		sleep_until(clock() + 1);
	}
	if !matches!(client().set_call(&CallState::None), Some(Ok(()))) || !matches!(client().close(), Some(Ok(()))) {
		fail("the pending voice session did not close");
	}
	close(commands);
	close(session);
	say("PASS radio-voice-close");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: Option<LaunchContext> = recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode);
	// THE GRANTS IN THE ORDER PERMISSIONMANAGER DELIVERS THEM.
	let stream = recv_tagged(bootstrap, &mut buf, b"AUDIO_STREAM").unwrap_or(0);
	let capture = recv_tagged(bootstrap, &mut buf, b"AUDIO_CAPTURE").unwrap_or(0);
	let control = recv_tagged(bootstrap, &mut buf, b"AUDIOCONTROL").unwrap_or(0);
	let voice_grant = recv_tagged(bootstrap, &mut buf, b"AUDIO_VOICE").unwrap_or(0);
	if stream == 0 || capture == 0 || control == 0 || voice_grant == 0 {
		fail("a grant this probe needs was not delivered");
	}
	let probe = Probe { stream, capture, control, voice: voice_grant };
	let phase = context.map(|context| String::from(context.arguments.trim())).unwrap_or_default();
	match phase.as_str() {
		"inventory" => inventory(&probe),
		"hold" => hold(&probe),
		"operator" => operator(&probe),
		"voice" => voice(&probe),
		"radio-music" => radio_music(&probe, false),
		"radio-stereo" => radio_music(&probe, true),
		"radio-voice" => radio_voice(&probe),
		"radio-voice-close" => radio_voice_close(&probe),
		_ => fail("usage: audioprobe inventory|hold|operator|voice|radio-music|radio-stereo|radio-voice|radio-voice-close"),
	}
	exit();
}
