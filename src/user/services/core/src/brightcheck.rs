// brightcheck - the brightness gate's driver. DEVELOPMENT-ONLY.
//
// It holds what the `brightness` tool holds - DisplayService's read and the brightness policy's control - and prints
// one verdict line per case, `brightcheck: ...`, for the gate to match.
//
//   brightcheck outputs              output 0 and its source: a provider's function, or the boot framebuffer's decoder
//   brightcheck list                 every backlight: key, kind, output, join reason, standing, level, floor
//   brightcheck wait SECONDS         until output 0 has an active backlight
//   brightcheck set LEVEL            an absolute level
//   brightcheck percent PERCENT      a percent of the range
//   brightcheck up | down            one step
//   brightcheck zero                 level zero without the explicit zero: it lands on the floor
//   brightcheck zero-allowed         level zero with it
//   brightcheck level LEVEL SECONDS  until the active backlight's level is LEVEL, or the time is spent
//   brightcheck idle-off             the idle timeout off, stored, so no dim moves a level the gate compares
//   brightcheck auto on|off          automatic brightness, stored
//   brightcheck settings             the policy's two settings

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{BacklightStanding, BacklightState, BrightnessTarget, JoinReason, LaunchContext, OutputSource, PciFunction, brightness_policy, display_brightness};
use rt::*;
use services::capability_names::*;

const ASK_TICKS: u64 = TICKS_PER_SECOND * 3;

fn say(line: &str) {
	print(b"brightcheck: ");
	print(line.as_bytes());
	print(b"\n");
}

fn fail(line: &str) -> ! {
	say(&format!("FAIL {line}"));
	exit();
}

fn backlights(read: u64) -> Vec<BacklightState> {
	match display_brightness::Client::with_deadline(ChannelTransport { chan: read }, clock() + ASK_TICKS).backlights() {
		Some(Ok(backlights)) => backlights,
		_ => fail("DisplayService did not list the backlights"),
	}
}

fn active(read: u64) -> Option<BacklightState> {
	backlights(read).into_iter().find(|backlight| backlight.standing == BacklightStanding::Active && backlight.output == Some(0))
}

fn describe(backlight: &BacklightState) -> String {
	let reason = match backlight.reason {
		Some(JoinReason::Native) => "native",
		Some(JoinReason::FirmwareAdapter) => "firmware-adapter",
		Some(JoinReason::Edid) => "edid",
		Some(JoinReason::SingleOutput) => "single-output",
		None => "none",
	};
	let standing = match backlight.standing {
		BacklightStanding::Active => "active",
		BacklightStanding::Shadowed => "shadowed",
		BacklightStanding::Unjoined => "unjoined",
	};
	let output = backlight.output.map(|output| format!("{output}")).unwrap_or_else(|| String::from("none"));
	let level = backlight.level.map(|level| format!("{level}")).unwrap_or_else(|| String::from("unknown"));
	format!("backlight {} kind={:?} output={output} reason={reason} standing={standing} level={level} floor={}", backlight.key, backlight.kind, backlight.floor)
}

fn set(control: u64, name: &str, target: BrightnessTarget, allow_zero: bool) {
	match brightness_policy::Client::with_deadline(ChannelTransport { chan: control }, clock() + ASK_TICKS).set("", &target, &allow_zero) {
		Some(Ok(set)) => say(&format!("{name} level={}", set.level)),
		Some(Err(error)) => say(&format!("{name} refused {error:?}")),
		None => fail(&format!("{name}: the brightness policy did not answer")),
	}
}

fn number(word: Option<&&str>) -> u32 {
	word.and_then(|word| word.parse().ok()).unwrap_or_else(|| fail("a number was expected"))
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	let read = recv_tagged(bootstrap, &mut buf, CAP_BRIGHTNESS).unwrap_or(0);
	let control = recv_tagged(bootstrap, &mut buf, CAP_BRIGHTNESS_CONTROL).unwrap_or(0);
	if read == 0 || control == 0 {
		fail("the read and the control were not both granted");
	}
	let words: Vec<&str> = context.arguments.split_whitespace().collect();
	let mut policy = brightness_policy::Client::with_deadline(ChannelTransport { chan: control }, clock() + ASK_TICKS);
	match words.first().copied() {
		Some("outputs") => match display_brightness::Client::with_deadline(ChannelTransport { chan: read }, clock() + ASK_TICKS).outputs() {
			Some(Ok(outputs)) => {
				for output in &outputs {
					let function = |function: &PciFunction| format!("{:02x}:{:02x}.{:x}", function.bus, function.dev, function.func);
					let source = match &output.source {
						OutputSource::Provider(publisher) => format!("provider {}", function(publisher)),
						OutputSource::BootFramebuffer(Some(decoder)) => format!("boot-framebuffer decoder {}", function(decoder)),
						OutputSource::BootFramebuffer(None) => String::from("boot-framebuffer decoder none"),
					};
					say(&format!("output {} source {source}", output.id));
				}
			}
			other => fail(&format!("the outputs were not listed: {other:?}")),
		},
		Some("list") => {
			let all = backlights(read);
			say(&format!("{} backlight(s)", all.len()));
			for backlight in &all {
				say(&describe(backlight));
			}
		}
		Some("wait") => {
			let deadline = clock() + u64::from(number(words.get(1))) * TICKS_PER_SECOND;
			loop {
				if let Some(backlight) = active(read) {
					say(&format!("active {}", describe(&backlight)));
					break;
				}
				if clock() >= deadline {
					fail("no active backlight on output 0");
				}
				sleep_until(clock() + TICKS_PER_SECOND / 10);
			}
		}
		Some("set") => set(control, "set", BrightnessTarget::Level(number(words.get(1))), false),
		Some("percent") => set(control, "percent", BrightnessTarget::Percent(number(words.get(1))), false),
		Some("up") => set(control, "up", BrightnessTarget::Steps(1), false),
		Some("down") => set(control, "down", BrightnessTarget::Steps(-1), false),
		Some("zero") => set(control, "zero", BrightnessTarget::Level(0), false),
		Some("zero-allowed") => set(control, "zero-allowed", BrightnessTarget::Level(0), true),
		Some("level") => {
			let wanted = number(words.get(1));
			let deadline = clock() + u64::from(number(words.get(2))) * TICKS_PER_SECOND;
			loop {
				let level = active(read).and_then(|backlight| backlight.level);
				if level == Some(wanted) {
					say(&format!("level reached {wanted}"));
					break;
				}
				if clock() >= deadline {
					say(&format!("level stayed {}", level.map(|level| format!("{level}")).unwrap_or_else(|| String::from("unknown"))));
					break;
				}
				sleep_until(clock() + TICKS_PER_SECOND / 10);
			}
		}
		Some("idle-off") => match policy.set_idle(&None) {
			Some(Ok(())) => say("idle off"),
			other => fail(&format!("the idle timeout was not turned off: {other:?}")),
		},
		Some("auto") => {
			let on = match words.get(1).copied() {
				Some("on") => true,
				Some("off") => false,
				_ => fail("auto on|off"),
			};
			match policy.set_automatic(&on) {
				Some(Ok(())) => say(if on { "auto on" } else { "auto off" }),
				other => fail(&format!("automatic brightness was not set: {other:?}")),
			}
		}
		Some("settings") => match policy.settings() {
			Some(Ok(settings)) => say(&format!("settings automatic={} idle={}", settings.automatic, settings.idle_seconds.map(|seconds| format!("{seconds}")).unwrap_or_else(|| String::from("off")))),
			other => fail(&format!("the settings were not read: {other:?}")),
		},
		_ => fail("unknown case"),
	}
	exit();
}
