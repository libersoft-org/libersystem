// btctl - the Bluetooth operator command: list, scan, pair, enable, disable, forget and power.
//
// THE ONE SHIPPING HOLDER OF THE OPERATOR AUTHORITY. BluetoothService serves three interfaces to three
// kinds of holder, and the operator one - power a radio, pair, forget, make a mouse an input source - goes
// to the one component the permission store names for it and to nothing by default. This is that
// component: without it a machine can list its radios and never pair anything. Like `lsdev` it holds the
// READ beside the WRITE, because listing and scanning are how an operator finds what to pair.
//
// WHAT A PAIRING HERE IS: LE Secure Connections with Just Works. The link is encrypted and the bond is
// durable, and nothing proved which radio answered - which is what `pair` says when it succeeds. The
// service pairs only an address its controller's current scan reported, so `scan` comes first.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use bluetooth_client::{BluetoothClient, BluetoothOperatorClient};
use proto::system::{Error, LaunchContext, PairingState, PeerAddress, PeerKind, SecurityLevel};
use rt::*;
use tools::{parse_u64, split_args};

// A hundred clock ticks are a second.
const TICKS: u64 = 100;
// A scan when none is asked for, and the service's own cap on one.
const SCAN_SECONDS: u64 = 5;
const MAX_SCAN_SECONDS: u64 = 10;
// The service ends a pairing at sixty seconds whatever the peer does; this waits a little past it.
const PAIR_SECONDS: u64 = 65;

const USAGE: &[u8] = b"usage: btctl [-c N] [list | scan [SECONDS] | pair ADDRESS [public|random] | enable ADDRESS | disable ADDRESS | forget ADDRESS | power on|off]\n";

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
	match (args.first().copied(), args.len()) {
		(None, _) | (Some(b"list"), 1) => list(read, operator),
		(Some(b"scan"), 1) => scan(read, controller, SCAN_SECONDS),
		(Some(b"scan"), 2) => scan(read, controller, parse_u64(args[1]).unwrap_or_else(|| usage())),
		(Some(b"pair"), 2 | 3) => {
			let bytes = parse_address(args[1]).unwrap_or_else(|| usage());
			// THE KIND, IF GIVEN; otherwise each in turn. The service answers `not-found` for an identity its
			// current scan did not report, so trying both can only ever reach the one the radio heard.
			let kinds: &[PeerKind] = match args.get(2).copied() {
				None => &[PeerKind::Public, PeerKind::RandomStatic],
				Some(b"public") => &[PeerKind::Public],
				Some(b"random") => &[PeerKind::RandomStatic],
				Some(_) => usage(),
			};
			pair(operator, controller, bytes, kinds);
		}
		(Some(b"enable"), 2) => enable(operator, controller, args[1], true),
		(Some(b"disable"), 2) => enable(operator, controller, args[1], false),
		(Some(b"forget"), 2) => forget(operator, controller, args[1]),
		(Some(b"power"), 2) => match args[1] {
			b"on" => power(operator, controller, true),
			b"off" => power(operator, controller, false),
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
		PeerKind::Public => "public",
		PeerKind::RandomStatic => "random",
	};
	format!("{} {kind}", hex(&peer.bytes))
}

fn security(level: SecurityLevel) -> &'static str {
	match level {
		SecurityLevel::None => "not encrypted",
		SecurityLevel::EncryptedUnauthenticated => "encrypted, not authenticated (Just Works: nothing proved which radio answered)",
		SecurityLevel::EncryptedAuthenticated => "encrypted and authenticated",
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

// Every controller, and on each the peers bonded to it.
fn list(read: u64, operator: u64) {
	let controllers = match BluetoothClient::new(read).controllers() {
		Some(Ok(controllers)) => controllers,
		other => return report("listing the controllers", other),
	};
	if controllers.is_empty() {
		print(b"no Bluetooth controller\n");
		return;
	}
	let mut client = BluetoothOperatorClient::new(operator);
	for (at, controller) in controllers.iter().enumerate() {
		let state = if controller.powered { "on" } else { "off" };
		let pairing = if controller.secure_connections { "" } else { ", cannot pair (no LE Secure Connections)" };
		print(format!("controller {at}: {} - {state}{pairing}\n", address(&controller.address)).as_bytes());
		match client.bonded(at as u32) {
			Some(Ok(peers)) if peers.is_empty() => print(b"  nothing bonded\n"),
			Some(Ok(peers)) => {
				for peer in &peers {
					let input = if peer.enabled { "input enabled" } else { "input disabled" };
					print(format!("  bonded {} \"{}\" - {input}, {}\n", address(&peer.address), peer.name, security(peer.security)).as_bytes());
				}
			}
			other => report("listing the bonds", other),
		}
	}
}

fn scan(read: u64, controller: u32, seconds: u64) {
	let seconds = seconds.clamp(1, MAX_SCAN_SECONDS);
	let mut client = BluetoothClient::new(read);
	let handle = match client.scan(controller, (seconds * 1000) as u32) {
		Some(Ok(handle)) => handle,
		other => return report("the scan", other),
	};
	print(format!("scanning for {seconds} s...\n").as_bytes());
	let deadline = clock() + (seconds + 1) * TICKS;
	while clock() < deadline && matches!(client.scanning(&handle), Some(Ok(true))) {
		sleep_until(clock() + TICKS / 4);
	}
	match client.results(&handle) {
		Some(Ok(results)) if results.is_empty() => print(b"nothing found\n"),
		Some(Ok(results)) => {
			for result in &results {
				let kind = if result.human_interface { "  (human interface)" } else { "" };
				print(format!("{}  {} dBm  \"{}\"{kind}\n", address(&result.address), result.rssi, result.name).as_bytes());
			}
		}
		other => report("reading the scan results", other),
	}
}

fn pair(operator: u64, controller: u32, bytes: [u8; 6], kinds: &[PeerKind]) {
	let mut client = BluetoothOperatorClient::new(operator);
	let mut chosen = None;
	for &kind in kinds {
		let peer = PeerAddress { kind, bytes: bytes.to_vec() };
		match client.pair(controller, &peer) {
			Some(Ok(())) => {
				chosen = Some(peer);
				break;
			}
			Some(Err(Error::NotFound)) => {}
			other => return report("the pairing", other),
		}
	}
	let Some(peer) = chosen else {
		print(b"btctl: the controller's current scan did not report that address - run `btctl scan` first\n");
		return;
	};
	print(format!("pairing with {}...\n", address(&peer)).as_bytes());
	let deadline = clock() + PAIR_SECONDS * TICKS;
	loop {
		match client.progress(controller) {
			Some(Ok(progress)) if progress.state == PairingState::Bonded => {
				print(format!("bonded: {}\n", security(progress.security)).as_bytes());
				print(format!("to use it as a pointer: btctl enable {}\n", hex(&peer.bytes)).as_bytes());
				return;
			}
			Some(Ok(progress)) if progress.state == PairingState::Failed => {
				print(b"btctl: the pairing failed\n");
				return;
			}
			Some(Ok(_)) => {}
			other => return report("reading the pairing's progress", other),
		}
		if clock() >= deadline {
			print(b"btctl: the pairing did not finish in time\n");
			return;
		}
		sleep_until(clock() + TICKS / 4);
	}
}

// The bonded peer with this address, in the kind it was bonded under.
fn bonded(client: &mut BluetoothOperatorClient, controller: u32, text: &[u8]) -> Option<PeerAddress> {
	let Some(bytes) = parse_address(text) else { usage() };
	match client.bonded(controller) {
		Some(Ok(peers)) => match peers.into_iter().find(|peer| peer.address.bytes == bytes) {
			Some(peer) => Some(peer.address),
			None => {
				print(b"btctl: nothing with that address is bonded to this controller\n");
				None
			}
		},
		other => {
			report("listing the bonds", other);
			None
		}
	}
}

fn enable(operator: u64, controller: u32, text: &[u8], on: bool) {
	let mut client = BluetoothOperatorClient::new(operator);
	let Some(peer) = bonded(&mut client, controller, text) else { return };
	match client.enable(controller, &peer, on) {
		Some(Ok(())) if on => print(format!("{} is an input source\n", address(&peer)).as_bytes()),
		Some(Ok(())) => print(format!("{} is no longer an input source\n", address(&peer)).as_bytes()),
		other => report(if on { "enabling it" } else { "disabling it" }, other),
	}
}

// DURABLE BEFORE IT ANSWERS: the service deletes the bond from the volume first, then drops the link.
fn forget(operator: u64, controller: u32, text: &[u8]) {
	let mut client = BluetoothOperatorClient::new(operator);
	let Some(peer) = bonded(&mut client, controller, text) else { return };
	match client.forget(controller, &peer) {
		Some(Ok(())) => print(format!("{} is forgotten\n", address(&peer)).as_bytes()),
		other => report("forgetting it", other),
	}
}

fn power(operator: u64, controller: u32, on: bool) {
	match BluetoothOperatorClient::new(operator).power(controller, on) {
		Some(Ok(())) if on => print(format!("controller {controller}: on - it initialises, and `btctl list` shows when it is ready\n").as_bytes()),
		Some(Ok(())) => print(format!("controller {controller}: off\n").as_bytes()),
		other => report("the power request", other),
	}
}
