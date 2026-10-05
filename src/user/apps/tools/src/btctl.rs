// btctl - the Bluetooth operator command, for both radios.
//
// THE ONE SHIPPING HOLDER OF THE OPERATOR AUTHORITY. BluetoothService serves three interfaces to three
// kinds of holder, and the operator one - power a radio, pair, trust, forget - goes to the one component
// the permission store names for it and to nothing by default. This is that component: without it a
// machine can list its radios and never pair anything. Like `lsdev` it holds the READ beside the WRITE,
// because listing and scanning are how an operator finds what to pair.
//
// IT TAKES THE SHELL'S INTERACTIVE SHAPE, the one `play` and `less` have: the terminal it was started from
// is its own while it runs, so `pair` and `pairable` read a person's answers there. They are PROMPT
// WATCHERS: while one runs the controller is pairable and declares the IO capabilities a person can answer
// - DisplayYesNo on BR/EDR, KeyboardDisplay on LE - and every question a pairing raises arrives here with its
// deadline. Without a terminal there is no watcher, and a pairing runs with nothing to ask: Just Works.
//
// WHAT AN ANSWER PROVES: the holder of the operator authority confirmed, on an ordinary terminal another
// local program could overdraw. Numeric Comparison and Passkey Entry still defeat an attacker on the radio
// alone; Just Works proves nothing about which radio answered, and `pair` says so when that is what ran.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use bluetooth_client::{BluetoothClient, BluetoothOperatorClient};
use proto::system::{BondLevel, DeviceStatus, Error, KeyAgreement, LaunchContext, MediaCommand, PairingPrompt, PairingState, PeerAddress, PeerKind, Profile, PromptQuestion, PromptReply, Radio, bluetooth_operator};
use rt::*;
use tools::{parse_u64, split_args};

// A hundred clock ticks are a second.
const TICKS: u64 = 100;
// A scan when none is asked for, and the service's own cap on one.
const SCAN_SECONDS: u64 = 5;
const MAX_SCAN_SECONDS: u64 = 10;
// The service ends a pairing at sixty seconds whatever the peer does; this waits a little past it.
const PAIR_SECONDS: u64 = 65;
// How long `pairable` watches when no time is given, and at most.
const PAIRABLE_SECONDS: u64 = 120;
const MAX_PAIRABLE_SECONDS: u64 = 600;

const USAGE: &[u8] = b"usage: btctl [-c N] COMMAND
  list                              devices on both radios: bonded, connected and found
  scan [SECONDS]                    inquiry and LE scan together
  pair ADDRESS [public|random|bredr]
  pair-legacy ADDRESS               PIN pairing, for a device that cannot do better
  pairable [SECONDS]                answer incoming pairings from this terminal
  discoverable SECONDS|off          at most 180 seconds
  trust ADDRESS PROFILE             input, audio, voice, pan, spp or gatt
  untrust ADDRESS PROFILE
  alias ADDRESS NAME                the name an application grant names a peer by; - clears it
  connect ADDRESS PROFILE
  disconnect ADDRESS PROFILE
  enable ADDRESS | disable ADDRESS  trust for input, as before
  forget ADDRESS
  media ADDRESS play|pause|next|previous   the remote control, toward a phone playing here
  power on|off
";

struct Tool {
	read: u64,
	operator: u64,
	controller: u32,
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 256] = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: LaunchContext = match recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode) {
		Some(context) => context,
		None => exit(),
	};
	// The two authorities, in the order the permission row grants them.
	let read = recv_tagged(bootstrap, &mut buf, b"BTREAD").unwrap_or(0);
	let operator = recv_tagged(bootstrap, &mut buf, b"BTOPERATOR").unwrap_or(0);
	if read == 0 || operator == 0 {
		print(b"btctl: the Bluetooth read and operator authorities were not both granted\n");
		exit();
	}
	let mut args: Vec<&[u8]> = split_args(context.arguments.as_bytes()).collect();
	// `-c N` names the controller; the first is the default.
	let mut controller: u32 = 0;
	if args.first() == Some(&&b"-c"[..]) {
		match args.get(1).and_then(|text| parse_u64(text)).and_then(|at| u32::try_from(at).ok()) {
			Some(at) => controller = at,
			None => usage(),
		}
		args.drain(..2);
	}
	let tool = Tool { read, operator, controller };
	match (args.first().copied(), args.len()) {
		(None, _) | (Some(b"list"), 1) => tool.list(),
		(Some(b"scan"), 1) => tool.scan(SCAN_SECONDS),
		(Some(b"scan"), 2) => tool.scan(parse_u64(args[1]).unwrap_or_else(|| usage())),
		(Some(b"pair"), 2 | 3) => {
			let bytes = parse_address(args[1]).unwrap_or_else(|| usage());
			// THE KIND, IF GIVEN; otherwise each in turn. The service answers `not-found` for an identity its
			// current scan did not report, so trying them all can only ever reach the one the radio heard.
			let kinds: &[PeerKind] = match args.get(2).copied() {
				None => &[PeerKind::Bredr, PeerKind::Public, PeerKind::RandomStatic, PeerKind::Resolvable],
				Some(b"public") => &[PeerKind::Public],
				Some(b"random") => &[PeerKind::RandomStatic, PeerKind::Resolvable],
				Some(b"bredr") => &[PeerKind::Bredr],
				Some(_) => usage(),
			};
			tool.pair(bytes, kinds, false);
		}
		(Some(b"pair-legacy"), 2) => tool.pair(parse_address(args[1]).unwrap_or_else(|| usage()), &[PeerKind::Bredr], true),
		(Some(b"pairable"), 1) => tool.pairable(PAIRABLE_SECONDS),
		(Some(b"pairable"), 2) => tool.pairable(parse_u64(args[1]).unwrap_or_else(|| usage()).min(MAX_PAIRABLE_SECONDS)),
		(Some(b"discoverable"), 2) => tool.discoverable(if args[1] == b"off" { 0 } else { parse_u64(args[1]).unwrap_or_else(|| usage()) }),
		(Some(b"trust"), 3) => tool.trust(args[1], parse_profile(args[2]).unwrap_or_else(|| usage()), true),
		(Some(b"untrust"), 3) => tool.trust(args[1], parse_profile(args[2]).unwrap_or_else(|| usage()), false),
		(Some(b"alias"), 3) => tool.alias(args[1], if args[2] == b"-" { b"" } else { args[2] }),
		(Some(b"connect"), 3) => tool.connect(args[1], parse_profile(args[2]).unwrap_or_else(|| usage()), true),
		(Some(b"disconnect"), 3) => tool.connect(args[1], parse_profile(args[2]).unwrap_or_else(|| usage()), false),
		(Some(b"enable"), 2) => tool.trust(args[1], Profile::Input, true),
		(Some(b"disable"), 2) => tool.trust(args[1], Profile::Input, false),
		(Some(b"forget"), 2) => tool.forget(args[1]),
		(Some(b"media"), 3) => {
			let command = match args[2] {
				b"play" => MediaCommand::Play,
				b"pause" => MediaCommand::Pause,
				b"next" => MediaCommand::Next,
				b"previous" => MediaCommand::Previous,
				_ => usage(),
			};
			tool.media(args[1], command);
		}
		(Some(b"power"), 2) => match args[1] {
			b"on" => tool.power(true),
			b"off" => tool.power(false),
			_ => usage(),
		},
		_ => usage(),
	}
	exit();
}

fn usage() -> ! {
	print(USAGE);
	exit();
}

// `c0:ff:ee:00:00:01`, most significant byte first - the order `scan` and `list` print.
fn parse_address(text: &[u8]) -> Option<[u8; 6]> {
	let text = core::str::from_utf8(text).ok()?;
	let mut out = [0u8; 6];
	let mut parts = text.split(':');
	for byte in out.iter_mut() {
		let part = parts.next().filter(|part| part.len() == 2)?;
		*byte = u8::from_str_radix(part, 16).ok()?;
	}
	parts.next().is_none().then_some(out)
}

fn parse_profile(text: &[u8]) -> Option<Profile> {
	match text {
		b"input" => Some(Profile::Input),
		b"audio" => Some(Profile::Audio),
		b"voice" => Some(Profile::Voice),
		b"pan" => Some(Profile::Pan),
		b"spp" => Some(Profile::Spp),
		b"gatt" => Some(Profile::Gatt),
		_ => None,
	}
}

fn profile_name(profile: Profile) -> &'static str {
	match profile {
		Profile::Input => "input",
		Profile::Audio => "audio",
		Profile::Voice => "voice",
		Profile::Pan => "pan",
		Profile::Spp => "spp",
		Profile::Gatt => "gatt",
	}
}

fn hex(bytes: &[u8]) -> String {
	let mut out = String::new();
	for (at, byte) in bytes.iter().enumerate() {
		if at > 0 {
			out.push(':');
		}
		out.push_str(&format!("{byte:02x}"));
	}
	out
}

fn address(peer: &PeerAddress) -> String {
	let kind = match peer.kind {
		PeerKind::Public => "le-public",
		PeerKind::RandomStatic => "le-random",
		PeerKind::Resolvable => "le-private",
		PeerKind::Bredr => "bredr",
	};
	format!("{} {kind}", hex(&peer.bytes))
}

fn level(level: &BondLevel) -> &'static str {
	match (level.agreement, level.authenticated) {
		(KeyAgreement::SecureConnections, true) => "Secure Connections, authenticated",
		(KeyAgreement::SecureConnections, false) => "Secure Connections, Just Works (nothing proved which radio answered)",
		(KeyAgreement::P192, true) => "P-192 Simple Pairing, authenticated",
		(KeyAgreement::P192, false) => "P-192 Simple Pairing, Just Works",
		(KeyAgreement::Legacy, _) => "legacy PIN",
		(KeyAgreement::LeLegacy, _) => "LE legacy",
	}
}

// A request that was refused, or got no answer at all, said as such.
fn report<T>(what: &str, outcome: Option<Result<T, Error>>) {
	match outcome {
		Some(Err(error)) => print(format!("btctl: {what} was refused ({error:?})\n").as_bytes()),
		None => print(format!("btctl: {what} got no answer from the Bluetooth service\n").as_bytes()),
		Some(Ok(_)) => {}
	}
}

// One line from the terminal, waiting until `deadline`; `None` when it passed or the terminal ended.
fn read_line_until(deadline: u64) -> Option<String> {
	let input = stdin();
	if input == 0 {
		return None;
	}
	let mut buf = [0u8; 128];
	loop {
		match try_recv_caps(input, &mut buf) {
			PolledCaps::Message { len, handles } => {
				for &handle in handles.as_slice() {
					close(handle);
				}
				if len == 0 {
					return None;
				}
				let text = core::str::from_utf8(&buf[..len]).unwrap_or("");
				return Some(String::from(text.trim()));
			}
			PolledCaps::Empty => {
				if clock() >= deadline {
					return None;
				}
				let _ = wait(input, deadline);
			}
			PolledCaps::Closed => return None,
		}
	}
}

impl Tool {
	fn operator(&self) -> BluetoothOperatorClient {
		BluetoothOperatorClient::new(self.operator)
	}

	// Every device on this controller, both radios in one list.
	fn list(&self) {
		let controllers = match BluetoothClient::new(self.read).controllers() {
			Some(Ok(controllers)) => controllers,
			other => return report("listing the controllers", other),
		};
		if controllers.is_empty() {
			print(b"no Bluetooth controller\n");
			return;
		}
		for (at, controller) in controllers.iter().enumerate() {
			let state = if controller.powered { "on" } else { "off" };
			let pairing = if controller.secure_connections { "" } else { ", cannot pair on LE (no LE Secure Connections)" };
			let radios = if controller.classic { "BR/EDR and LE" } else { "LE only" };
			// One string grown in place: a vector of strings pushed to would import its growth routine from
			// whichever library shares that instance, which is not one this program declares.
			let mut policy = String::new();
			for (on, word) in [(controller.connectable, "connectable"), (controller.discoverable, "discoverable"), (controller.pairable, "pairable")] {
				if on {
					policy.push_str(if policy.is_empty() { " [" } else { ", " });
					policy.push_str(word);
				}
			}
			if !policy.is_empty() {
				policy.push(']');
			}
			print(format!("controller {at}: {} - {state}, {radios}{pairing}{policy}\n", address(&controller.address)).as_bytes());
			if at as u32 != self.controller {
				continue;
			}
			match self.operator().devices(at as u32) {
				Some(Ok(devices)) if devices.is_empty() => print(b"  no devices\n"),
				Some(Ok(devices)) => {
					for device in &devices {
						print_device(device);
					}
				}
				other => report("listing the devices", other),
			}
		}
	}

	fn scan(&self, seconds: u64) {
		let seconds = seconds.clamp(1, MAX_SCAN_SECONDS);
		let mut client = BluetoothClient::new(self.read);
		let handle = match client.scan(self.controller, (seconds * 1000) as u32) {
			Some(Ok(handle)) => handle,
			other => return report("the scan", other),
		};
		print(format!("scanning for {seconds} s...\n").as_bytes());
		let deadline = clock() + (seconds + 2) * TICKS;
		while clock() < deadline && matches!(client.scanning(&handle), Some(Ok(true))) {
			sleep_until(clock() + TICKS / 4);
		}
		match client.results(&handle) {
			Some(Ok(results)) if results.is_empty() => print(b"nothing found\n"),
			Some(Ok(results)) => {
				for result in &results {
					let kind = if result.human_interface { "  (human interface)" } else { "" };
					let class = if result.radio == Radio::Classic { format!("  class {:06x}", result.class_of_device) } else { String::new() };
					let mut services = String::new();
					for class in &result.services {
						services.push_str(if services.is_empty() { "  services " } else { "," });
						services.push_str(&format!("{:04x}", class.uuid));
					}
					print(format!("{}  {} dBm  \"{}\"{class}{services}{kind}\n", address(&result.address), result.rssi, result.name).as_bytes());
				}
			}
			other => report("reading the scan results", other),
		}
	}

	// The device with this address this controller knows, in the kind it knows it under.
	fn known(&self, text: &[u8]) -> Option<PeerAddress> {
		let Some(bytes) = parse_address(text) else { usage() };
		match self.operator().devices(self.controller) {
			Some(Ok(devices)) => match devices.into_iter().find(|device| device.address.bytes == bytes) {
				Some(device) => Some(device.address),
				None => {
					print(b"btctl: this controller knows nothing with that address\n");
					None
				}
			},
			other => {
				report("listing the devices", other);
				None
			}
		}
	}

	// THE PROMPT WATCHER, attached while this runs - only with a terminal to read the answers from.
	fn watch(&self) -> u64 {
		if stdin() == 0 {
			return 0;
		}
		match self.operator().prompts(self.controller) {
			Some(Ok(stream)) => stream,
			Some(Err(Error::Again)) => {
				print(b"btctl: another btctl is already answering this controller's pairings\n");
				0
			}
			other => {
				report("watching for pairing prompts", other);
				0
			}
		}
	}

	fn pair(&self, bytes: [u8; 6], kinds: &[PeerKind], legacy: bool) {
		let watcher = self.watch();
		if watcher == 0 {
			print(b"btctl: no terminal to answer on - a pairing that needs a person will be refused, Just Works will not\n");
		}
		let mut client = self.operator();
		let mut chosen = None;
		for &kind in kinds {
			let peer = PeerAddress { kind, bytes: bytes.to_vec() };
			let outcome = if legacy { client.pair_legacy(self.controller, &peer) } else { client.pair(self.controller, &peer) };
			match outcome {
				Some(Ok(())) => {
					chosen = Some(peer);
					break;
				}
				Some(Err(Error::NotFound)) => {}
				other => {
					report("the pairing", other);
					close_if(watcher);
					return;
				}
			}
		}
		let Some(peer) = chosen else {
			print(b"btctl: the controller's current scan did not report that address - run `btctl scan` first\n");
			close_if(watcher);
			return;
		};
		print(format!("pairing with {}...\n", address(&peer)).as_bytes());
		let deadline = clock() + PAIR_SECONDS * TICKS;
		loop {
			match client.progress(self.controller) {
				Some(Ok(progress)) if progress.state == PairingState::Bonded => {
					let bonded = client.bonded(self.controller).and_then(Result::ok).and_then(|peers| peers.into_iter().find(|bonded| bonded.address == peer));
					match bonded {
						Some(bonded) => print(format!("bonded: {}\n", level(&bonded.level)).as_bytes()),
						None => print(b"bonded\n"),
					}
					print(format!("to trust it for a profile: btctl trust {} PROFILE\n", hex(&peer.bytes)).as_bytes());
					break;
				}
				Some(Ok(progress)) if progress.state == PairingState::Failed => {
					print(b"btctl: the pairing failed\n");
					break;
				}
				Some(Ok(_)) => {}
				other => {
					report("reading the pairing's progress", other);
					break;
				}
			}
			if clock() >= deadline {
				print(b"btctl: the pairing did not finish in time\n");
				break;
			}
			if watcher != 0 {
				self.serve_prompts(watcher, clock() + TICKS / 4);
			} else {
				sleep_until(clock() + TICKS / 4);
			}
		}
		close_if(watcher);
	}

	// `pairable`: a watcher and nothing else, for pairings a device starts.
	fn pairable(&self, seconds: u64) {
		let watcher = self.watch();
		if watcher == 0 {
			print(b"btctl: pairable needs this terminal to answer on\n");
			return;
		}
		print(format!("pairable for {seconds} s - pairings a device starts are asked here\n").as_bytes());
		let deadline = clock() + seconds * TICKS;
		while clock() < deadline {
			if !self.serve_prompts(watcher, deadline) {
				break;
			}
		}
		close(watcher);
		print(b"no longer pairable\n");
	}

	// Answer prompts until `until`; false when the watcher ended.
	fn serve_prompts(&self, watcher: u64, until: u64) -> bool {
		let mut frame = [0u8; 512];
		loop {
			match try_recv_caps(watcher, &mut frame) {
				PolledCaps::Message { len, handles } => {
					let mut handles = handles;
					let prompt = bluetooth_operator::prompts_read(&frame[..len], &mut handles);
					for &leftover in handles.as_slice() {
						close(leftover);
					}
					if let Some(prompt) = prompt {
						self.ask(&prompt);
					}
				}
				PolledCaps::Empty => {
					if clock() >= until {
						return true;
					}
					let _ = wait(watcher, until);
				}
				PolledCaps::Closed => return false,
			}
		}
	}

	// One question, asked on this terminal, and its answer sent before the deadline.
	fn ask(&self, prompt: &PairingPrompt) {
		let who = if prompt.name.is_empty() { address(&prompt.peer) } else { format!("\"{}\" ({})", prompt.name, address(&prompt.peer)) };
		let deadline = clock() + u64::from(prompt.deadline_ms) * TICKS / 1000;
		let reply = match prompt.question {
			PromptQuestion::Compare => {
				print(format!("{who} shows a number. Does it match {:06}? [y/n] ", prompt.value).as_bytes());
				yes_no(read_line_until(deadline))
			}
			PromptQuestion::ShowPasskey => {
				if prompt.keypresses > 0 {
					print(format!("({} keys typed)\n", prompt.keypresses).as_bytes());
					return;
				}
				print(format!("Type {:06} on {who} and press its Enter key. [Enter here to wait, n to refuse] ", prompt.value).as_bytes());
				match read_line_until(deadline) {
					Some(line) if line == "n" || line == "no" => PromptReply::No,
					Some(_) => PromptReply::Yes,
					None => return,
				}
			}
			PromptQuestion::EnterPasskey => {
				print(format!("Type the six digits {who} shows: ").as_bytes());
				match read_line_until(deadline).and_then(|line| line.parse::<u32>().ok()).filter(|value| *value <= 999_999) {
					Some(value) => PromptReply::Passkey(value),
					None => PromptReply::No,
				}
			}
			PromptQuestion::EnterPin => {
				print(format!("PIN for {who}: ").as_bytes());
				match read_line_until(deadline).filter(|line| (1..=16).contains(&line.len())) {
					Some(line) => PromptReply::Pin(line.into_bytes()),
					None => PromptReply::No,
				}
			}
			PromptQuestion::Consent => {
				print(format!("{who} asks to pair. Allow it? [y/n] ").as_bytes());
				yes_no(read_line_until(deadline))
			}
		};
		match self.operator().answer(self.controller, prompt.id, &reply) {
			Some(Ok(())) => {}
			Some(Err(Error::TimedOut)) => print(b"btctl: too late - the pairing was refused at its deadline\n"),
			other => report("the answer", other),
		}
	}

	fn discoverable(&self, seconds: u64) {
		let seconds = seconds.min(180);
		match self.operator().discoverable(self.controller, seconds as u32) {
			Some(Ok(())) if seconds == 0 => print(b"not discoverable\n"),
			Some(Ok(())) => print(format!("discoverable for {seconds} s\n").as_bytes()),
			other => report("the discoverable request", other),
		}
	}

	fn trust(&self, text: &[u8], profile: Profile, on: bool) {
		let Some(peer) = self.known(text) else { return };
		match self.operator().trust(self.controller, &peer, profile, on) {
			Some(Ok(())) if on => print(format!("{} is trusted for {}\n", address(&peer), profile_name(profile)).as_bytes()),
			Some(Ok(())) => print(format!("{} is no longer trusted for {}\n", address(&peer), profile_name(profile)).as_bytes()),
			Some(Err(Error::NotFound)) => print(b"btctl: only a bonded device is trusted - pair it first\n"),
			other => report("the trust change", other),
		}
	}

	fn alias(&self, text: &[u8], alias: &[u8]) {
		let Some(peer) = self.known(text) else { return };
		let alias = core::str::from_utf8(alias).unwrap_or("");
		match self.operator().alias(self.controller, &peer, alias) {
			Some(Ok(())) if alias.is_empty() => print(format!("{} has no alias\n", address(&peer)).as_bytes()),
			Some(Ok(())) => print(format!("{} is \"{alias}\"\n", address(&peer)).as_bytes()),
			Some(Err(Error::Again)) => print(b"btctl: another device already has that alias\n"),
			Some(Err(Error::Invalid)) => print(b"btctl: an alias is up to 32 letters, digits, - and _\n"),
			other => report("the alias", other),
		}
	}

	fn connect(&self, text: &[u8], profile: Profile, on: bool) {
		let Some(peer) = self.known(text) else { return };
		let outcome = if on { self.operator().connect(self.controller, &peer, profile) } else { self.operator().disconnect(self.controller, &peer, profile) };
		match outcome {
			Some(Ok(())) if on => print(format!("connecting {} to {}\n", profile_name(profile), address(&peer)).as_bytes()),
			Some(Ok(())) => print(format!("disconnecting {} from {}\n", profile_name(profile), address(&peer)).as_bytes()),
			Some(Err(Error::Unsupported)) => print(format!("btctl: {} is not something this system connects on that radio\n", profile_name(profile)).as_bytes()),
			other => report(if on { "the connection" } else { "the disconnection" }, other),
		}
	}

	// A BUTTON ON THE REMOTE CONTROL, pressed and let go over AVRCP toward a phone this system is connected to.
	fn media(&self, text: &[u8], command: MediaCommand) {
		let Some(peer) = self.known(text) else { return };
		match self.operator().media(self.controller, &peer, command) {
			Some(Ok(())) => print(format!("sent {command:?} to {}\n", address(&peer)).as_bytes()),
			Some(Err(Error::Closed)) => print(b"btctl: the device is not connected\n"),
			other => report("the remote-control command", other),
		}
	}

	// DURABLE BEFORE IT ANSWERS: the service deletes the bond from the volume first, then drops the link.
	fn forget(&self, text: &[u8]) {
		let Some(peer) = self.known(text) else { return };
		match self.operator().forget(self.controller, &peer) {
			Some(Ok(())) => print(format!("{} is forgotten\n", address(&peer)).as_bytes()),
			other => report("forgetting it", other),
		}
	}

	fn power(&self, on: bool) {
		match self.operator().power(self.controller, on) {
			Some(Ok(())) if on => print(format!("controller {}: on - it initialises, and `btctl list` shows when it is ready\n", self.controller).as_bytes()),
			Some(Ok(())) => print(format!("controller {}: off\n", self.controller).as_bytes()),
			other => report("the power request", other),
		}
	}
}

// Profiles as one comma-separated word.
fn profiles(list: &[Profile]) -> String {
	let mut out = String::new();
	for profile in list {
		if !out.is_empty() {
			out.push(',');
		}
		out.push_str(profile_name(*profile));
	}
	out
}

fn print_device(device: &DeviceStatus) {
	let name = match (device.alias.is_empty(), device.name.is_empty()) {
		(false, _) => format!("\"{}\" ({})", device.alias, device.name),
		(true, false) => format!("\"{}\"", device.name),
		(true, true) => String::from("\"\""),
	};
	// One string grown in place, for the reason `list` gives.
	let mut state = String::new();
	if device.bonded {
		state.push_str(device.level.as_ref().map_or("bonded", level));
		state.push_str("; ");
	}
	state.push_str(if device.connected { "connected" } else { "not connected" });
	if !device.trusted.is_empty() {
		state.push_str(&format!("; trusted for {}", profiles(&device.trusted)));
	}
	if !device.connected_profiles.is_empty() {
		state.push_str(&format!("; using {}", profiles(&device.connected_profiles)));
	}
	if let Some(battery) = device.battery {
		state.push_str(&format!("; battery {battery}%"));
	}
	print(format!("  {} {name} - {state}\n", address(&device.address)).as_bytes());
}

fn yes_no(line: Option<String>) -> PromptReply {
	match line.as_deref() {
		Some("y" | "yes") => PromptReply::Yes,
		_ => PromptReply::No,
	}
}

fn close_if(handle: u64) {
	if handle != 0 {
		close(handle);
	}
}
