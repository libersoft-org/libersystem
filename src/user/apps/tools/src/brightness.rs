// brightness - the outputs and every backlight, a level, and the brightness policy's two settings.
//
//   brightness                                   the outputs, every backlight and the policy's settings
//   brightness set LEVEL [--backlight KEY] [--zero]
//   brightness set PERCENT% [--backlight KEY] [--zero]
//   brightness set up|down [--backlight KEY]     one step
//   brightness auto on|off                       automatic brightness
//   brightness idle SECONDS|off                  the idle timeout after which the active backlight is dimmed
//
// A set without `--backlight` is output 0's active backlight's. Zero - a black panel - is reached only with `--zero`;
// every other set stops at the floor. Reading is DisplayService's, through the `brightness` capability; a set and the
// settings are the brightness policy's, through `brightness-control`, which forwards the set with the floor kept.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use display_client::{BrightnessClient, BrightnessPolicyClient};
use proto::generated::liber::display::v1::{BacklightKind, BacklightStanding, BacklightState, BrightnessOutput, BrightnessScale, BrightnessTarget, JoinReason, OutputSource, PciFunction};
use proto::system::LaunchContext;
use rt::*;

const USAGE: &str = "usage: brightness [set LEVEL|PERCENT%|up|down [--backlight KEY] [--zero] | auto on|off | idle SECONDS|off]";

fn out(line: &str) {
	print(line.as_bytes());
	print(b"\n");
}

fn fail(line: &str) -> ! {
	eprint(b"brightness: ");
	eprint(line.as_bytes());
	eprint(b"\n");
	exit();
}

fn function(function: &PciFunction) -> String {
	format!("{:02x}:{:02x}.{:x}", function.bus, function.dev, function.func)
}

fn print_output(output: &BrightnessOutput) {
	let source = match &output.source {
		OutputSource::Provider(publisher) => format!("a display provider at {}", function(publisher)),
		OutputSource::BootFramebuffer(Some(decoder)) => format!("the boot framebuffer, decoded by {}", function(decoder)),
		OutputSource::BootFramebuffer(None) => String::from("the boot framebuffer, decoded by no function the system knows"),
	};
	let edid = match &output.edid {
		Some(edid) => format!("; monitor {:04x}:{:04x} serial {}", edid.manufacturer, edid.product, edid.serial),
		None => String::new(),
	};
	out(&format!("output {}: {source}{edid}", output.id));
}

fn print_backlight(backlight: &BacklightState) {
	let kind = match backlight.kind {
		BacklightKind::Firmware => "firmware",
		BacklightKind::UsbMonitor => "USB monitor",
		BacklightKind::Native => "native",
	};
	let join = match (backlight.output, backlight.reason) {
		(Some(output), Some(reason)) => {
			let reason = match reason {
				JoinReason::Native => "native",
				JoinReason::FirmwareAdapter => "firmware-adapter",
				JoinReason::Edid => "edid",
				JoinReason::SingleOutput => "single-output",
			};
			format!("output {output} by {reason}")
		}
		_ => String::from("no output"),
	};
	let standing = match (backlight.standing, &backlight.shadowed_by) {
		(BacklightStanding::Active, _) => String::from("active"),
		(BacklightStanding::Shadowed, Some(winner)) => format!("shadowed by {winner}"),
		(BacklightStanding::Shadowed, None) => String::from("shadowed"),
		(BacklightStanding::Unjoined, _) => String::from("unjoined"),
	};
	out(&format!("backlight {} - {kind}, {join}, {standing}", backlight.key));
	let levels = match &backlight.scale {
		BrightnessScale::Levels(levels) => {
			let listed: Vec<String> = levels.iter().map(|level| format!("{level}")).collect();
			format!("levels {}", listed.join(" "))
		}
		BrightnessScale::Range(range) => format!("levels {}..{}", range.minimum, range.maximum),
	};
	let level = match backlight.level {
		Some(level) => format!("{level}"),
		None => String::from("not known"),
	};
	let mut line = format!("  {levels}; level {level}, floor {}", backlight.floor);
	if let Some(default) = backlight.ac_default {
		line.push_str(&format!(", AC default {default}"));
	}
	if let Some(default) = backlight.battery_default {
		line.push_str(&format!(", battery default {default}"));
	}
	out(&line);
	if backlight.failed {
		out("  NOT ANSWERING - left out of the join until it speaks again");
	}
}

fn list(read: u64, control: u64) {
	if read == 0 {
		fail("this launch holds no connection to DisplayService's brightness");
	}
	let mut client = BrightnessClient::new(read);
	match client.outputs() {
		Some(Ok(outputs)) => outputs.iter().for_each(print_output),
		Some(Err(error)) => fail(&format!("DisplayService refused the outputs: {error:?}")),
		None => fail("DisplayService did not answer"),
	}
	match client.backlights() {
		Some(Ok(backlights)) if backlights.is_empty() => out("no backlight"),
		Some(Ok(backlights)) => backlights.iter().for_each(print_backlight),
		Some(Err(error)) => fail(&format!("DisplayService refused the backlights: {error:?}")),
		None => fail("DisplayService did not answer"),
	}
	if control == 0 {
		return;
	}
	match BrightnessPolicyClient::new(control).settings() {
		Some(Ok(settings)) => {
			let idle = match settings.idle_seconds {
				Some(seconds) => format!("{seconds} s"),
				None => String::from("off"),
			};
			out(&format!("policy: automatic brightness {}, idle timeout {idle}", if settings.automatic { "on" } else { "off" }));
		}
		_ => out("policy: not answering - nothing is restored or dimmed"),
	}
}

fn set(control: u64, words: &[&str]) {
	let mut target = None;
	let mut key = String::new();
	let mut zero = false;
	let mut at = 0;
	while at < words.len() {
		match words[at] {
			"--zero" => zero = true,
			"--backlight" => {
				at += 1;
				key = String::from(*words.get(at).unwrap_or_else(|| fail(USAGE)));
			}
			"up" => target = Some(BrightnessTarget::Steps(1)),
			"down" => target = Some(BrightnessTarget::Steps(-1)),
			word => {
				target = Some(match word.strip_suffix('%') {
					Some(percent) => BrightnessTarget::Percent(percent.parse().unwrap_or_else(|_| fail(USAGE))),
					None => BrightnessTarget::Level(word.parse().unwrap_or_else(|_| fail(USAGE))),
				});
			}
		}
		at += 1;
	}
	let target = target.unwrap_or_else(|| fail(USAGE));
	match BrightnessPolicyClient::new(control).set(&key, &target, zero) {
		Some(Ok(set)) => out(&format!("level {} (change {})", set.level, set.serial)),
		Some(Err(error)) => fail(&format!("the set was refused: {error:?}")),
		None => fail("the brightness policy did not answer"),
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let read = recv_tagged(bootstrap, &mut buf, b"BRIGHTNESS").unwrap_or(0);
	let control = recv_tagged(bootstrap, &mut buf, b"BRIGHTNESSCONTROL").unwrap_or(0);
	let words: Vec<&str> = context.arguments.split_whitespace().collect();
	if words.is_empty() {
		list(read, control);
		exit();
	}
	if control == 0 {
		fail("this launch holds no connection to the brightness policy");
	}
	let mut policy = BrightnessPolicyClient::new(control);
	let answer = match (words[0], words.get(1).copied(), words.len()) {
		("set", _, _) => {
			set(control, &words[1..]);
			exit();
		}
		("auto", Some("on"), 2) => policy.set_automatic(true),
		("auto", Some("off"), 2) => policy.set_automatic(false),
		("idle", Some("off"), 2) => policy.set_idle(None),
		("idle", Some(seconds), 2) => match seconds.parse::<u32>() {
			Ok(seconds) if seconds > 0 => policy.set_idle(Some(seconds)),
			_ => fail(USAGE),
		},
		_ => fail(USAGE),
	};
	match answer {
		Some(Ok(())) => out("stored"),
		Some(Err(error)) => fail(&format!("the brightness policy refused: {error:?}")),
		None => fail("the brightness policy did not answer"),
	}
	exit();
}
