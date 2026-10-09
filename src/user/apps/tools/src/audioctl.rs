// audioctl - the audio operator command: the devices, the defaults, the levels and where the streams play.
//
// THE ONE SHIPPING HOLDER OF `audio-control`. AudioService keeps an inventory of every sound card, USB audio function
// and Bluetooth endpoint, one default output, input and voice device by its routing rule - a device that arrives
// becomes the default for what it serves, and the one it displaced returns when it leaves - and each device's level.
// This tool reads all of it and changes the two things an operator may: which device is the default, and a device's
// level. It plays nothing and records nothing: it holds no stream, recorder or voice session of its own.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use audio_client::AudioControlClient;
use proto::system::{AudioDevice, AudioDirection, AudioFormat, AudioStreamInfo, AudioTransport, Error, LaunchContext};
use rt::*;
use tools::{parse_u64, split_args};

const USAGE: &[u8] = b"usage: audioctl COMMAND
  devices                           every device, its formats, latency, level and the defaults it is
  default DEVICE output|input|voice make it the default, as its arrival would
  volume DEVICE LEVEL               its existing speaker/device level, 0 to 100
  microphone DEVICE [LEVEL]         read or set independent microphone level, 0 to 100
  streams                           where every stream plays, its latency and the silence it played
  counters                          streams moved, frames played into no device, the phone stream's buffer
";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let control = recv_tagged(bootstrap, &mut buf, b"AUDIOCONTROL").unwrap_or(0);
	if control == 0 {
		print(b"audioctl: the audio operator authority was not granted\n");
		exit();
	}
	let mut client = AudioControlClient::new(control);
	let args: Vec<&[u8]> = split_args(context.arguments.as_bytes()).collect();
	match (args.first().copied(), args.len()) {
		(None, _) | (Some(b"devices"), 1) => devices(&mut client),
		(Some(b"default"), 3) => {
			let device = parse_device(args[1]);
			let direction = match args[2] {
				b"output" => AudioDirection::Output,
				b"input" => AudioDirection::Input,
				b"voice" => AudioDirection::Voice,
				_ => usage(),
			};
			done(client.set_default(&device, &direction), "setting the default");
		}
		(Some(b"volume"), 3) => {
			let device = parse_device(args[1]);
			let level = parse_u64(args[2]).filter(|level| *level <= 100).unwrap_or_else(|| usage()) as u8;
			done(client.set_volume(&device, &level), "setting the level");
		}
		(Some(b"microphone"), 2 | 3) => {
			let device = parse_device(args[1]);
			if args.len() == 3 {
				let level = parse_u64(args[2]).filter(|level| *level <= 100).unwrap_or_else(|| usage()) as u8;
				done(client.set_microphone_volume(&device, &level), "setting the microphone level");
			} else {
				match client.microphone_volume(&device) {
					Some(Ok(level)) => print(format!("device {device}: microphone level {level}\n").as_bytes()),
					Some(Err(error)) => done(Some(Err(error)), "reading the microphone level"),
					None => done(None, "reading the microphone level"),
				}
			}
		}
		(Some(b"streams"), 1) => streams(&mut client),
		(Some(b"counters"), 1) => match client.counters() {
			Some(counters) => print(format!("moves {} silent-frames {} route-overflows {} route-underruns {}\n", counters.moves, counters.silent_frames, counters.route_overflows, counters.route_underruns).as_bytes()),
			None => print(b"audioctl: AudioService did not answer\n"),
		},
		_ => usage(),
	}
	exit();
}

fn usage() -> ! {
	print(USAGE);
	exit();
}

fn parse_device(text: &[u8]) -> u32 {
	parse_u64(text).and_then(|id| u32::try_from(id).ok()).unwrap_or_else(|| usage())
}

fn done(answer: Option<Result<(), Error>>, what: &str) {
	match answer {
		Some(Ok(())) => print(b"done\n"),
		Some(Err(Error::NotFound)) => print(format!("audioctl: {what}: no such device\n").as_bytes()),
		Some(Err(Error::Invalid)) => print(format!("audioctl: {what}: the device does not serve that, or the value is out of range\n").as_bytes()),
		Some(Err(error)) => print(format!("audioctl: {what}: refused ({error:?})\n").as_bytes()),
		None => print(format!("audioctl: {what}: AudioService did not answer\n").as_bytes()),
	}
}

fn format_text(format: &Option<AudioFormat>) -> String {
	match format {
		Some(format) => format!("{} Hz {}", format.rate, if format.channels == 1 { "mono" } else { "stereo" }),
		None => String::from("-"),
	}
}

fn devices(client: &mut AudioControlClient) {
	let Some(devices) = client.devices() else {
		print(b"audioctl: AudioService did not answer\n");
		return;
	};
	if devices.is_empty() {
		print(b"no audio device - streams play into silence until one arrives\n");
		return;
	}
	for device in &devices {
		print_device(device);
	}
}

fn print_device(device: &AudioDevice) {
	let transport = match device.transport {
		AudioTransport::Provider => "sound device",
		AudioTransport::Bluetooth => "Bluetooth",
	};
	// One string grown in place: a vector of strings pushed to would import its growth routine from whichever library
	// shares that instance, which is not one this program declares.
	let mut defaults = String::new();
	for (on, word) in [(device.default_output, "default output"), (device.default_input, "default input"), (device.default_voice, "default voice")] {
		if on {
			defaults.push_str(if defaults.is_empty() { " [" } else { ", " });
			defaults.push_str(word);
		}
	}
	if !defaults.is_empty() {
		defaults.push(']');
	}
	let kind = if device.voice {
		", voice"
	} else if device.route {
		", a phone playing to this system"
	} else {
		""
	};
	let level = if device.hardware_volume { "on the device" } else { "scaled here" };
	print(format!("device {}: {} ({transport}{kind}) - out {}, in {}, latency {} us, level {} {level}{defaults}\n", device.id, device.label, format_text(&device.output), format_text(&device.input), device.latency_us, device.volume).as_bytes());
}

fn streams(client: &mut AudioControlClient) {
	let Some(streams) = client.streams() else {
		print(b"audioctl: AudioService did not answer\n");
		return;
	};
	if streams.is_empty() {
		print(b"no streams\n");
		return;
	}
	for (at, stream) in streams.iter().enumerate() {
		print_stream(at, stream);
	}
}

fn print_stream(at: usize, stream: &AudioStreamInfo) {
	let place = match (stream.device, stream.named) {
		(Some(device), Some(_)) => format!("on device {device}, which it named"),
		(Some(device), None) => format!("on device {device}, the default"),
		(None, _) => String::from("on no device - playing into silence"),
	};
	let kind = if stream.voice {
		" (voice)"
	} else if stream.route {
		" (the phone's stream)"
	} else {
		""
	};
	let format = format_text(&Some(stream.format.clone()));
	print(format!("stream {at}{kind}: {format} {place}, latency {} us, silent-frames {}\n", stream.latency_us, stream.silent_frames).as_bytes());
}
