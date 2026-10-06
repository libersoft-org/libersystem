// btclassic - the in-guest scenario driver for the BR/EDR gate. DEVELOPMENT-ONLY.
//
// It holds Bluetooth's read and operator authorities and the fixture's control endpoint. On the operator
// connection it is the PROMPT WATCHER a person running `btctl pair` would be, answering each prompt the way
// the phase needs; through the control endpoint it makes the fixture's BR/EDR devices act from the far side
// of the radio, and reads what they saw. The verdict on each claim is the far side's account where the claim
// is about the radio, and the host's own only where the claim is about what the host reports.
//
//   btclassic pair     a dual-mode controller; discoverable for at most the time asked; inquiry beside the LE
//                      scan; Numeric Comparison, Passkey Entry and Just Works through the prompts, with the
//                      levels they earn; pairable only while a watcher is attached; an incoming pairing refused
//                      without one and asked with one; a PIN only on the operator's word; no downgrade on either
//                      axis; aliases
//   btclassic policy   the inbound policy at L2CAP by PSM and on RFCOMM by channel; this host's SDP server; the
//                      SDP client and RFCOMM finding and opening a serial port, credits flowing both ways; sniff
//   btclassic reuse    after a restart or a cold reboot: the trusted keyboard pages, and the link is secured with
//                      the stored key - which the fixture reports - without pairing again
//   btclassic serial   the serial device aliased `serial-1` and trusted for the serial port, for `btserial`'s grant
//   btclassic transfer the phone and the serial device bonded for the file pushes, and a push the phone tries with no
//                      receiver waiting refused
//   btclassic push N   the phone pushes an object of N bytes five seconds later: the gate's next line is the receiver
//   btclassic forget   the keyboard forgotten: this host no longer takes its page
//   btclassic input    the keyboard paired, trusted for input, reconnecting its HID channels, its report descriptor
//                      read from its record, and connected for input; then it is told to type, after this probe has
//                      ended: `btclassic typed` at the prompt, Ctrl+Alt+Delete and its Power key, and `btclassic alive`
//   btclassic typed    the line the Bluetooth keyboard typed at the shell's prompt, run
//   btclassic alive    the line it typed after its Ctrl+Alt+Delete and Power key: the machine still runs
//   btclassic gamepad  the second half of `gamepad --lines | btclassic gamepad`: the Bluetooth gamepad paired, trusted
//                      for input and reconnected arrives in the gamepad set at rest, its button and hat show on it,
//                      and its link going is its departure
//   btclassic le       LE beyond one mouse: the tag paired by Numeric Comparison from its private address and bonded
//                      under the identity it gave with its resolving key, its battery level in its status; the display by Passkey Entry this host
//                      types; the remote refused until `pair-legacy`, then bonded at the legacy level with its EDIV and
//                      Rand; three LE links at once; this host's name read through its attribute server; and the tag
//                      and the remote reconnecting through the accept list, the tag resolved, each on its stored key
//   btclassic audio    music: the headset connected for audio streams what AudioService plays - the fixture hears
//                      SBC at the configuration both support with every CRC good and the tone in its subband - its
//                      delay is in the latency, its level goes both ways and its play button is answered not
//                      implemented; the phone streams to this host as a route to the default output, and takes the
//                      operator's pause
//   btclassic voice    calls: the headset's hands-free unit connects to this host's voice gateway and sets its service
//                      level connection up with mSBC, its battery in its status and its voice AudioService's default
//                      voice device; with no session its answer and its request for audio are refused; a voice session
//                      brings the synchronous link up and carries a tone both ways, mSBC every CRC good; the call is
//                      relayed - rung, answered and hung up from the headset - its level is its speaker gain, and the
//                      session's end takes the link down
//   btclassic ctkd     the other radio's key: the phone paired on BR/EDR has its LE key derived over the BR/EDR
//                      Security Manager, and its LE half is encrypted on it; paired anew on LE, its BR/EDR key is
//                      derived from the LTK, and its BR/EDR half authenticates on it - both sides deriving apart

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::codec::Buffer;
use proto::system::{AudioDevice, AudioTransport, BondedPeer, CallCommand, CallState, FixtureAction, KeyAgreement, LaunchContext, MediaCommand, PairingPrompt, PairingState, PeerAddress, PeerKind, Profile, PromptQuestion, PromptReply, Radio, audio, audio_control, bluetooth, bluetooth_fixture, bluetooth_operator, pcm_stream, voice_session};
use rt::*;

const TICKS: u64 = 100;
// The devices' numbers on the control endpoint, and their addresses.
const PHONE: u8 = 2;
const KEYBOARD: u8 = 3;
const HEADSET: u8 = 4;
const SERIAL: u8 = 5;
const LEGACY: u8 = 6;
const GAMEPAD: u8 = 7;
const TAG: u8 = 8;
const REMOTE: u8 = 9;
const DISPLAY: u8 = 10;

// An LE world device's identity address, static random.
fn identity_of(device: u8) -> PeerAddress {
	PeerAddress { kind: PeerKind::RandomStatic, bytes: alloc::vec![0xd0, 0x1b, 0xdc, 0x20, 0x00, device] }
}

fn address_of(device: u8) -> PeerAddress {
	PeerAddress { kind: PeerKind::Bredr, bytes: alloc::vec![0x00, 0x1b, 0xdc, 0x20, 0x00, device] }
}

fn say(line: &str) {
	print(format!("btclassic: {line}\n").as_bytes());
}

fn fail(line: &str) -> ! {
	print(format!("btclassic: FAIL {line}\n").as_bytes());
	exit_with(1);
}

struct Probe {
	read: u64,
	operator: u64,
	fixture: u64,
	// What an application plays through, what `audioctl` reads the devices through, and what a call opens its voice
	// session through.
	audio: u64,
	control: u64,
	voice: u64,
	// What the far side said, not yet matched.
	seen: Vec<String>,
	watcher: u64,
}

impl Probe {
	fn read(&self) -> bluetooth::Client<ChannelTransport> {
		bluetooth::Client::new(ChannelTransport { chan: self.read })
	}

	fn operator(&self) -> bluetooth_operator::Client<ChannelTransport> {
		bluetooth_operator::Client::new(ChannelTransport { chan: self.operator })
	}

	fn act(&mut self, device: u8, action: FixtureAction, argument: u32) -> u32 {
		match bluetooth_fixture::Client::new(ChannelTransport { chan: self.fixture }).act(&device, &action, &argument) {
			Some(Ok(result)) => result,
			_ => fail(&format!("the fixture refused {action:?} for device {device}")),
		}
	}

	fn collect(&mut self) {
		if let Some(Ok(lines)) = bluetooth_fixture::Client::new(ChannelTransport { chan: self.fixture }).events() {
			self.seen.extend(lines);
		}
	}

	// THE FIRST LINE the far side said that `matches`, waiting up to `ticks`; it and everything before it are
	// consumed, so a later wait sees only what came after.
	fn expect(&mut self, what: &str, ticks: u64, matches: impl Fn(&str) -> bool) -> String {
		let deadline = clock() + ticks;
		loop {
			self.collect();
			if let Some(at) = self.seen.iter().position(|line| matches(line)) {
				let line = self.seen[at].clone();
				self.seen.drain(..=at);
				return line;
			}
			if clock() >= deadline {
				for line in &self.seen {
					say(&format!("  the fixture said: {line}"));
				}
				fail(what);
			}
			sleep_until(clock() + TICKS / 10);
		}
	}

	// Whether the far side said a line `matches` within `ticks` - without failing when it did not.
	fn saw(&mut self, ticks: u64, matches: impl Fn(&str) -> bool) -> bool {
		let deadline = clock() + ticks;
		loop {
			self.collect();
			if let Some(at) = self.seen.iter().position(|line| matches(line)) {
				self.seen.drain(..=at);
				return true;
			}
			if clock() >= deadline {
				return false;
			}
			sleep_until(clock() + TICKS / 10);
		}
	}

	fn controller(&self) -> proto::system::ControllerInfo {
		match self.read().controllers() {
			Some(Ok(controllers)) if !controllers.is_empty() => controllers[0].clone(),
			_ => fail("no controller"),
		}
	}

	fn ready(&self) {
		let deadline = clock() + 20 * TICKS;
		loop {
			if let Some(Ok(controllers)) = self.read().controllers()
				&& controllers.first().is_some_and(|controller| controller.powered && controller.classic)
			{
				return;
			}
			if clock() >= deadline {
				fail("no powered dual-mode controller within twenty seconds");
			}
			sleep_until(clock() + TICKS / 4);
		}
	}

	fn watch(&mut self) {
		if self.watcher != 0 {
			return;
		}
		self.watcher = match self.operator().prompts(&0) {
			Some(Ok(stream)) => stream,
			_ => fail("the prompt watcher was refused"),
		};
	}

	fn unwatch(&mut self) {
		if self.watcher != 0 {
			close(self.watcher);
			self.watcher = 0;
		}
	}

	// The next NEW prompt on the watcher, waiting up to `ticks`: a keypress update of one already answered is
	// passed over.
	fn prompt(&mut self, what: &str, ticks: u64) -> PairingPrompt {
		loop {
			let prompt = self.any_prompt(what, ticks);
			if prompt.keypresses == 0 {
				return prompt;
			}
		}
	}

	fn any_prompt(&mut self, what: &str, ticks: u64) -> PairingPrompt {
		let deadline = clock() + ticks;
		let mut buf = [0u8; 512];
		loop {
			match try_recv_caps(self.watcher, &mut buf) {
				PolledCaps::Message { len, handles } => {
					let mut handles = handles;
					let prompt = bluetooth_operator::prompts_read(&buf[..len], &mut handles);
					for &leftover in handles.as_slice() {
						close(leftover);
					}
					if let Some(prompt) = prompt {
						return prompt;
					}
				}
				PolledCaps::Empty => {
					if clock() >= deadline {
						fail(what);
					}
					let _ = wait(self.watcher, deadline);
				}
				PolledCaps::Closed => fail("the prompt watcher was closed by the service"),
			}
		}
	}

	fn answer(&self, prompt: &PairingPrompt, reply: PromptReply) {
		if !matches!(self.operator().answer(&0, &prompt.id, &reply), Some(Ok(()))) {
			fail("an answer to a prompt was refused");
		}
	}

	// A pairing's end, as the operator's `progress` reports it.
	fn settle(&self, ticks: u64) -> PairingState {
		let deadline = clock() + ticks;
		loop {
			match self.operator().progress(&0) {
				Some(Ok(progress)) if matches!(progress.state, PairingState::Bonded | PairingState::Failed) => return progress.state,
				Some(Ok(_)) => {}
				_ => fail("the pairing's progress could not be read"),
			}
			if clock() >= deadline {
				fail("a pairing did not end within its time");
			}
			sleep_until(clock() + TICKS / 10);
		}
	}

	fn bond(&self, device: u8) -> Option<BondedPeer> {
		let wanted = if device >= TAG { identity_of(device) } else { address_of(device) };
		match self.operator().bonded(&0) {
			Some(Ok(peers)) => peers.into_iter().find(|peer| peer.address == wanted),
			_ => fail("the bonds could not be listed"),
		}
	}

	fn pair(&self, device: u8) {
		if !matches!(self.operator().pair(&0, &address_of(device)), Some(Ok(()))) {
			fail(&format!("the operator's pairing with device {device} was refused"));
		}
	}

	// THE DEVICE PAGES THIS HOST from no link: one it still holds is dropped first, so the page is a new
	// connection the host decides afresh.
	fn page(&mut self, device: u8) {
		if self.act(device, FixtureAction::Disconnect, 0) == 0 {
			let gone = format!("{} disconnected", ["phone", "keyboard", "headset", "serial", "legacy", "gamepad"][usize::from(device - PHONE)]);
			self.expect("a device did not drop its link before paging", 2 * TICKS, |line| line == gone);
		}
		if self.act(device, FixtureAction::Page, 0) != 0 {
			fail(&format!("device {device} could not page this host - it was not connectable"));
		}
	}

	fn trust(&self, device: u8, profile: Profile, on: bool) {
		let peer = if device >= TAG { identity_of(device) } else { address_of(device) };
		if !matches!(self.operator().trust(&0, &peer, &profile, &on), Some(Ok(()))) {
			fail(&format!("trusting device {device} for {profile:?} was refused"));
		}
	}
}

// The six digits the far side's line ends with.
fn digits(line: &str) -> u32 {
	line.rsplit(' ').next().and_then(|word| word.parse().ok()).unwrap_or(u32::MAX)
}

// ------------------------------------------------------------------ pair

fn pair(probe: &mut Probe) {
	probe.ready();
	let controller = probe.controller();
	if controller.connectable || controller.discoverable || controller.pairable {
		fail("a fresh controller with nothing trusted and no watcher was connectable, discoverable or pairable");
	}
	say("dual-mode, and neither connectable nor discoverable with nothing trusted and no watcher");

	// DISCOVERABLE FOR THE TIME ASKED, and then not.
	if !matches!(probe.operator().discoverable(&0, &2), Some(Ok(()))) {
		fail("discoverable was refused");
	}
	probe.expect("discoverable did not make the radio scan for inquiries", 2 * TICKS, |line| line == "scan 3");
	if !probe.controller().discoverable {
		fail("discoverable did not show in the controller's state");
	}
	probe.expect("discoverable did not end at its time", 5 * TICKS, |line| line == "scan 0");
	say("discoverable for two seconds, then not");

	// INQUIRY BESIDE THE LE SCAN.
	let handle = match probe.read().scan(&0, &4000) {
		Some(Ok(handle)) => handle,
		_ => fail("the scan was refused"),
	};
	let deadline = clock() + 7 * TICKS;
	while clock() < deadline && matches!(probe.read().scanning(&handle), Some(Ok(true))) {
		sleep_until(clock() + TICKS / 4);
	}
	let results = match probe.read().results(&handle) {
		Some(Ok(results)) => results,
		_ => fail("the scan's results could not be read"),
	};
	for (device, name, class) in [
		(PHONE, "fixture phone", 0x5a020c),
		(KEYBOARD, "fixture keyboard", 0x002540),
		(HEADSET, "fixture headset", 0x240404),
		(SERIAL, "fixture serial", 0x001f00),
		(LEGACY, "fixture legacy", 0x001f00),
	] {
		let address = address_of(device);
		let Some(found) = results.iter().find(|result| result.address == address) else { fail(&format!("inquiry did not find {name}")) };
		if found.radio != Radio::Classic || found.name != name || found.class_of_device != class {
			fail(&format!("{name} was found with the wrong radio, name or class"));
		}
	}
	if !results.iter().any(|result| result.address == address_of(PHONE) && result.services.iter().any(|class| class.uuid == 0x1105)) {
		fail("the phone's extended inquiry response did not carry its service classes");
	}
	if !results.iter().any(|result| result.address == address_of(KEYBOARD) && result.human_interface) {
		fail("the keyboard was not reported as a human interface device");
	}
	if !results.iter().any(|result| result.radio == Radio::Le) {
		fail("the LE scan beside the inquiry found nothing");
	}
	say("one scan found the five BR/EDR devices with their names, classes and services, and the LE mouse");

	// PAIRABLE ONLY WHILE A WATCHER IS ATTACHED.
	probe.watch();
	probe.expect("a watcher did not make the radio connectable", 2 * TICKS, |line| line == "scan 2");
	if !probe.controller().pairable {
		fail("the watcher did not make the controller pairable");
	}

	// NUMERIC COMPARISON: the phone.
	probe.pair(PHONE);
	let prompt = probe.prompt("the phone's pairing raised no prompt", 10 * TICKS);
	let shown = probe.expect("the phone did not show a number", 2 * TICKS, |line| line.starts_with("compare phone "));
	if prompt.question != PromptQuestion::Compare || prompt.value != digits(&shown) || prompt.deadline_ms == 0 || prompt.deadline_ms > 25_000 {
		fail("the phone's prompt was not a comparison of the number the phone shows, within 25 seconds");
	}
	probe.answer(&prompt, PromptReply::Yes);
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the phone did not bond after a yes");
	}
	probe.expect("the phone did not report an authenticated P-256 key", 2 * TICKS, |line| line.starts_with("paired phone type 0x08"));
	match probe.bond(PHONE) {
		Some(bond) if bond.radio == Radio::Classic && bond.level.agreement == KeyAgreement::SecureConnections && bond.level.authenticated => {}
		_ => fail("the phone's bond is not Secure Connections, authenticated"),
	}
	say("Numeric Comparison: the six digits matched, a yes bonded the phone at Secure Connections, authenticated");

	// PASSKEY ENTRY: the keyboard types what this host shows.
	probe.pair(KEYBOARD);
	let prompt = probe.prompt("the keyboard's pairing raised no prompt", 10 * TICKS);
	let shown = probe.expect("the keyboard was not shown a passkey", 2 * TICKS, |line| line.starts_with("passkey keyboard "));
	if prompt.question != PromptQuestion::ShowPasskey || prompt.value != digits(&shown) {
		fail("the keyboard's prompt did not show the passkey to type");
	}
	probe.answer(&prompt, PromptReply::Yes);
	probe.act(KEYBOARD, FixtureAction::TypePasskey, prompt.value);
	let typed = probe.any_prompt("the keyboard's keypresses did not reach the watcher", 5 * TICKS);
	if typed.id != prompt.id || typed.keypresses == 0 {
		fail("a keypress notification did not update the prompt");
	}
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the keyboard did not bond after typing the passkey");
	}
	match probe.bond(KEYBOARD) {
		Some(bond) if bond.level.agreement == KeyAgreement::SecureConnections && bond.level.authenticated => {}
		_ => fail("the keyboard's bond is not Secure Connections, authenticated"),
	}
	say("Passkey Entry: this host showed the digits, the keyboard typed them, and it bonded authenticated");

	// JUST WORKS, OUTGOING: the headset asks nothing.
	probe.pair(HEADSET);
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the headset did not bond");
	}
	match probe.bond(HEADSET) {
		Some(bond) if bond.level.agreement == KeyAgreement::SecureConnections && !bond.level.authenticated => {}
		_ => fail("the headset's bond is not Secure Connections, Just Works"),
	}
	say("Just Works with the headset: the operator asked, nothing was prompted, and the bond says not authenticated");

	// WITHOUT A WATCHER this host declares NoInputNoOutput, and is not connectable.
	probe.unwatch();
	probe.expect("the radio stayed connectable after the watcher left", 2 * TICKS, |line| line == "scan 0");
	probe.pair(SERIAL);
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the serial device did not bond");
	}
	probe.expect("the host declared a capability other than NoInputNoOutput without a watcher", 2 * TICKS, |line| line.starts_with("io-capability serial host 3"));
	match probe.bond(SERIAL) {
		Some(bond) if bond.level.agreement == KeyAgreement::P192 && !bond.level.authenticated => {}
		_ => fail("the serial device's bond is not P-192, Just Works"),
	}
	say("without a watcher: NoInputNoOutput declared, and the P-192 device bonded at P-192");

	// A PIN ONLY ON THE OPERATOR'S WORD.
	probe.watch();
	probe.pair(LEGACY);
	probe.expect("a PIN was not refused without the operator asking for one", 10 * TICKS, |line| line == "pin refused by the host legacy");
	if probe.settle(10 * TICKS) != PairingState::Failed {
		fail("a legacy pairing the operator did not ask for did not fail");
	}
	probe.expect("the refused legacy link was not dropped", 3 * TICKS, |line| line == "disconnected by the host legacy");
	if !matches!(probe.operator().pair_legacy(&0, &address_of(LEGACY)), Some(Ok(()))) {
		fail("pair-legacy was refused");
	}
	let prompt = probe.prompt("pair-legacy raised no PIN prompt", 10 * TICKS);
	if prompt.question != PromptQuestion::EnterPin {
		fail("pair-legacy's prompt did not ask for a PIN");
	}
	probe.answer(&prompt, PromptReply::Pin(b"0000".to_vec()));
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the legacy device did not bond with its PIN");
	}
	match probe.bond(LEGACY) {
		Some(bond) if bond.level.agreement == KeyAgreement::Legacy => {}
		_ => fail("the legacy device's bond is not at the legacy level"),
	}
	say("a PIN pairing was refused until the operator asked for one, and then bonded at the legacy level");

	// AN INCOMING PAIRING: refused with no watcher, asked with one.
	probe.trust(HEADSET, Profile::Audio, true);
	probe.unwatch();
	let controller = probe.controller();
	if !controller.connectable || controller.pairable {
		fail("with the headset trusted for audio and no watcher, the radio was not connectable, or was still pairable");
	}
	if probe.saw(TICKS, |line| line == "scan 0") {
		fail("the radio stopped being connectable when the watcher left, with a peer trusted for audio");
	}
	probe.act(HEADSET, FixtureAction::Forget, 0);
	probe.page(HEADSET);
	probe.expect("this host did not take the trusted headset's page", 3 * TICKS, |line| line.starts_with("accepted headset"));
	probe.act(HEADSET, FixtureAction::Pair, 0);
	probe.expect("an incoming pairing was not refused without a watcher", 3 * TICKS, |line| line.starts_with("pairing refused by the host headset"));
	probe.watch();
	probe.act(HEADSET, FixtureAction::Pair, 0);
	let prompt = probe.prompt("an incoming pairing raised no prompt", 5 * TICKS);
	if prompt.question != PromptQuestion::Consent {
		fail("an incoming Just Works pairing did not ask for consent");
	}
	probe.answer(&prompt, PromptReply::Yes);
	probe.expect("the headset did not pair again after consent", 5 * TICKS, |line| line.starts_with("paired headset type 0x07"));
	say("an incoming pairing: refused with nobody watching, asked for consent with a watcher, and bonded on a yes");

	// NO DOWNGRADE, AGREEMENT AXIS: the phone, reset, pairs again on P-192.
	probe.act(PHONE, FixtureAction::Forget, 0);
	probe.act(PHONE, FixtureAction::KeyType, 0x05);
	probe.page(PHONE);
	probe.expect("this host did not take the phone's page", 3 * TICKS, |line| line.starts_with("accepted phone"));
	probe.act(PHONE, FixtureAction::Pair, 0);
	let prompt = probe.prompt("the phone's new pairing raised no prompt", 5 * TICKS);
	probe.answer(&prompt, PromptReply::Yes);
	probe.expect("the phone did not make its P-192 key", 5 * TICKS, |line| line.starts_with("paired phone type 0x05"));
	probe.expect("this host kept a link that paired down to P-192", 5 * TICKS, |line| line == "disconnected by the host phone");
	match probe.bond(PHONE) {
		Some(bond) if bond.level.agreement == KeyAgreement::SecureConnections && bond.level.authenticated => {}
		_ => fail("a downgrade replaced the phone's Secure Connections bond"),
	}
	say("no downgrade on the agreement axis: P-192 offered over a P-256 bond was refused and the bond kept");

	// NO DOWNGRADE, AUTHENTICATION AXIS: the keyboard, paired again by the operator with nobody to answer.
	probe.unwatch();
	probe.pair(KEYBOARD);
	probe.expect("Just Works over an authenticated bond was not refused", 10 * TICKS, |line| line == "confirmation refused by the host keyboard");
	if probe.settle(10 * TICKS) != PairingState::Failed {
		fail("a Just Works pairing over an authenticated bond did not fail");
	}
	match probe.bond(KEYBOARD) {
		Some(bond) if bond.level.authenticated => {}
		_ => fail("a Just Works pairing replaced the keyboard's authenticated bond"),
	}
	say("no downgrade on the authentication axis: Just Works over an authenticated bond was refused and the bond kept");

	// ALIASES: one peer each, and only names a grant can carry.
	if !matches!(probe.operator().alias(&0, &address_of(PHONE), "phone-1"), Some(Ok(()))) {
		fail("an alias was refused");
	}
	if !matches!(probe.operator().alias(&0, &address_of(KEYBOARD), "phone-1"), Some(Err(proto::system::Error::Again))) {
		fail("a second peer was given an alias already taken");
	}
	if !matches!(probe.operator().alias(&0, &address_of(KEYBOARD), "my keyboard"), Some(Err(proto::system::Error::Invalid))) {
		fail("an alias with a space was taken");
	}
	if probe.bond(PHONE).is_none_or(|bond| bond.alias != "phone-1") {
		fail("the alias is not on the phone's bond");
	}
	say("aliases: one peer each, and only letters, digits, - and _");
	print(b"btclassic: PASS pair\n");
}

// ------------------------------------------------------------------ policy

fn policy(probe: &mut Probe) {
	probe.ready();
	// THE KEYBOARD, TRUSTED FOR INPUT, pages and opens HID's control channel: secured with its key first, then
	// admitted - and answered "not supported" until the HID host is here.
	probe.trust(KEYBOARD, Profile::Input, true);
	probe.page(KEYBOARD);
	probe.expect("this host did not take the trusted keyboard's page", 3 * TICKS, |line| line.starts_with("accepted keyboard"));
	probe.act(KEYBOARD, FixtureAction::OpenPsm, 0x0011);
	probe.expect("this host did not secure the keyboard's link before its channel", 5 * TICKS, |line| line.starts_with("authenticated keyboard with a remembered key"));
	probe.expect("the keyboard's link was not encrypted", 3 * TICKS, |line| line == "encrypted keyboard");
	let line = probe.expect("the keyboard's HID channel got no answer", 3 * TICKS, |line| line.starts_with("l2cap keyboard psm 0x0011 result"));
	if line.ends_with("result 3") {
		fail("the keyboard trusted for input was refused on security grounds");
	}
	say("a trusted keyboard's channel waited for its link to be secured with the stored key, and was not refused for security");

	// THE HEADSET, TRUSTED FOR AUDIO AND NOT INPUT: refused HID for security, admitted for AVDTP.
	if probe.act(HEADSET, FixtureAction::Page, 0) == 0 {
		probe.expect("this host did not take the headset's page", 3 * TICKS, |line| line.starts_with("accepted headset"));
	}
	probe.act(HEADSET, FixtureAction::OpenPsm, 0x0011);
	probe.expect("the headset, not trusted for input, was not refused HID on security grounds", 5 * TICKS, |line| line == "l2cap headset psm 0x0011 result 3");
	probe.act(HEADSET, FixtureAction::OpenPsm, 0x0019);
	let line = probe.expect("the headset's AVDTP channel got no answer", 3 * TICKS, |line| line.starts_with("l2cap headset psm 0x0019 result"));
	if line.ends_with("result 3") {
		fail("the headset trusted for audio was refused AVDTP on security grounds");
	}
	say("policy by PSM: HID refused for a peer not trusted for input, AVDTP admitted for one trusted for audio");

	// SDP ANSWERS ANY CONNECTED PEER: the voice gateway's record found, and none for a role this host does not run.
	probe.act(HEADSET, FixtureAction::SdpSearch, 0x111f);
	probe.expect("this host's SDP server did not offer the voice gateway", 3 * TICKS, |line| line.starts_with("sdp headset found 1 records for 0x111f"));
	probe.act(HEADSET, FixtureAction::SdpSearch, 0x1124);
	probe.expect("this host's SDP server offered a role it does not run", 3 * TICKS, |line| line.starts_with("sdp headset found 0 records for 0x1124"));
	say("this host's SDP server answered a connected peer's searches: its voice gateway, and no HID device");

	// RFCOMM: the session needs voice trust; a channel needs a role this host runs on it.
	probe.act(HEADSET, FixtureAction::RfcommOpen, 5);
	probe.expect("an RFCOMM session from a peer not trusted for voice was not refused", 3 * TICKS, |line| line == "l2cap headset psm 0x0003 result 3");
	probe.trust(HEADSET, Profile::Voice, true);
	probe.act(HEADSET, FixtureAction::RfcommOpen, 5);
	probe.expect("a channel no role serves was not refused", 5 * TICKS, |line| line == "rfcomm headset channel 5 refused by the host");
	say("policy by RFCOMM channel: the session refused without voice trust, and a channel nothing serves refused with it");

	// THE SERIAL PORT, FOUND AND OPENED: page, the stored key, an SDP search, RFCOMM with credits. The link the
	// pairing left goes first, so the connection starts from nothing.
	if probe.act(SERIAL, FixtureAction::Disconnect, 0) == 0 {
		probe.expect("the serial device did not drop its link", 2 * TICKS, |line| line == "serial disconnected");
	}
	if !matches!(probe.operator().connect(&0, &address_of(SERIAL), &Profile::Spp), Some(Ok(()))) {
		fail("connecting the serial port was refused");
	}
	probe.expect("the serial device was not paged", 5 * TICKS, |line| line.starts_with("connected serial"));
	probe.expect("the serial link was not secured with its stored key", 5 * TICKS, |line| line.starts_with("authenticated serial with a remembered key"));
	probe.expect("this host did not search the serial device's records", 5 * TICKS, |line| line == "sdp serial answered a search with 1 records");
	probe.expect("the serial port's RFCOMM channel did not open", 5 * TICKS, |line| line.starts_with("rfcomm serial channel 3 open"));
	let deadline = clock() + 3 * TICKS;
	loop {
		let connected = match probe.operator().devices(&0) {
			Some(Ok(devices)) => devices.into_iter().any(|device| device.address == address_of(SERIAL) && device.connected && device.connected_profiles.contains(&Profile::Spp)),
			_ => false,
		};
		if connected {
			break;
		}
		if clock() >= deadline {
			fail("the serial port did not show as connected");
		}
		sleep_until(clock() + TICKS / 10);
	}
	say("SPP: the record found by SDP named channel 3, and RFCOMM opened it");

	// CREDITS: twenty frames against seven credits, and the rest flow as this host gives credits back.
	probe.act(SERIAL, FixtureAction::RfcommSend, 20);
	probe.expect("the serial device did not hold frames back for credits", 3 * TICKS, |line| line == "rfcomm serial sent 7 frames, 13 wait for credits");
	let mut granted = 0u32;
	let deadline = clock() + 5 * TICKS;
	while granted < 13 && clock() < deadline {
		if probe.saw(TICKS / 2, |line| line.starts_with("rfcomm serial the host granted ")) {
			granted += 1;
		}
	}
	if granted == 0 {
		fail("this host never gave the serial device credits back");
	}
	say("credit-based flow control: the device stopped at its credits and this host gave more as it read");

	// SNIFF: an idle link is put to sleep.
	probe.expect("an idle link was not put into sniff mode", 8 * TICKS, |line| line.starts_with("sniff "));
	say("an idle link went into sniff mode");

	if !matches!(probe.operator().disconnect(&0, &address_of(SERIAL), &Profile::Spp), Some(Ok(()))) {
		fail("disconnecting the serial port was refused");
	}
	probe.expect("the serial port's channel was not closed", 3 * TICKS, |line| line == "rfcomm serial channel 3 closed by the host");
	print(b"btclassic: PASS policy\n");
}

// ------------------------------------------------------------------ reuse

fn reuse(probe: &mut Probe) {
	probe.ready();
	if probe.bond(KEYBOARD).is_none_or(|bond| !bond.trusted.contains(&Profile::Input)) {
		fail("the keyboard's bond and its input trust did not survive");
	}
	let deadline = clock() + 5 * TICKS;
	while !probe.controller().connectable {
		if clock() >= deadline {
			fail("a controller with a trusted keyboard was not connectable");
		}
		sleep_until(clock() + TICKS / 4);
	}
	probe.page(KEYBOARD);
	probe.expect("this host did not take the keyboard's page", 3 * TICKS, |line| line.starts_with("accepted keyboard"));
	probe.act(KEYBOARD, FixtureAction::OpenPsm, 0x0011);
	let line = probe.expect("the keyboard's link was not secured with a stored key", 5 * TICKS, |line| line.starts_with("authenticated keyboard with"));
	say(&format!("the far side said: {line}"));
	probe.expect("the keyboard's link was not encrypted", 3 * TICKS, |line| line == "encrypted keyboard");
	if probe.saw(TICKS, |line| line.starts_with("paired keyboard")) {
		fail("the keyboard paired again instead of using its bond");
	}
	probe.act(KEYBOARD, FixtureAction::Disconnect, 0);
	print(b"btclassic: PASS reuse\n");
}

// ------------------------------------------------------------------ input

fn input(probe: &mut Probe) {
	probe.ready();
	// THE KEYBOARD, PAIRED WITH ITS PASSKEY from a scan, and trusted for input.
	let handle = match probe.read().scan(&0, &3000) {
		Some(Ok(handle)) => handle,
		_ => fail("the scan was refused"),
	};
	let deadline = clock() + 6 * TICKS;
	while clock() < deadline && matches!(probe.read().scanning(&handle), Some(Ok(true))) {
		sleep_until(clock() + TICKS / 4);
	}
	probe.watch();
	probe.pair(KEYBOARD);
	let prompt = probe.prompt("the keyboard's pairing raised no prompt", 10 * TICKS);
	if prompt.question != PromptQuestion::ShowPasskey {
		fail("the keyboard's pairing did not show a passkey");
	}
	probe.answer(&prompt, PromptReply::Yes);
	probe.act(KEYBOARD, FixtureAction::TypePasskey, prompt.value);
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the keyboard did not bond");
	}
	probe.unwatch();
	probe.trust(KEYBOARD, Profile::Input, true);
	say("the keyboard is bonded and trusted for input");

	// IT RECONNECTS ITS INPUT AS A KEYBOARD DOES: it pages, and opens control and interrupt itself.
	probe.page(KEYBOARD);
	probe.expect("this host did not take the keyboard's page", 3 * TICKS, |line| line.starts_with("accepted keyboard"));
	probe.act(KEYBOARD, FixtureAction::HidConnect, 0);
	probe.expect("the keyboard's link was not secured with its stored key", 5 * TICKS, |line| line.starts_with("authenticated keyboard with a remembered key"));
	probe.expect("the keyboard's control channel was not accepted", 5 * TICKS, |line| line == "l2cap keyboard psm 0x0011 result 0");
	probe.expect("the keyboard's interrupt channel was not accepted", 5 * TICKS, |line| line == "l2cap keyboard psm 0x0013 result 0");
	probe.expect("this host did not read the keyboard's report descriptor from its record", 5 * TICKS, |line| line == "sdp keyboard answered a search with 1 records");
	let deadline = clock() + 5 * TICKS;
	loop {
		let connected = match probe.operator().devices(&0) {
			Some(Ok(devices)) => devices.into_iter().any(|device| device.address == address_of(KEYBOARD) && device.connected_profiles.contains(&Profile::Input)),
			_ => false,
		};
		if connected {
			break;
		}
		if clock() >= deadline {
			fail("the keyboard did not show as connected for input");
		}
		sleep_until(clock() + TICKS / 10);
	}
	say("the keyboard's HID channels are up, its descriptor read from its record, and it is connected for input");

	// WHAT IT TYPES, after this probe has ended and the prompt is the shell's again.
	for (text, delay) in [("btclassic typed\n", 4000), ("{cad}{power}", 8000), ("btclassic alive\n", 11000)] {
		if !matches!(bluetooth_fixture::Client::new(ChannelTransport { chan: probe.fixture }).type_text(&KEYBOARD, text, &delay), Some(Ok(()))) {
			fail("the fixture refused to type");
		}
	}
	print(b"btclassic: PASS input\n");
}

// ------------------------------------------------------------------ gamepad

// The gamepad tool's lines, on this probe's input: `gamepad: <word> <id> <rest>`.
struct Lines {
	input: u64,
	held: Vec<u8>,
}

impl Lines {
	fn next(&mut self, deadline: u64) -> Option<String> {
		loop {
			if let Some(end) = self.held.iter().position(|&byte| byte == b'\n') {
				let line: Vec<u8> = self.held.drain(..=end).take(end).collect();
				return Some(String::from(core::str::from_utf8(&line).unwrap_or("")));
			}
			let mut buf = [0u8; 512];
			match try_recv_caps(self.input, &mut buf) {
				PolledCaps::Message { len, handles } => {
					for &handle in handles.as_slice() {
						close(handle);
					}
					self.held.extend_from_slice(&buf[..len]);
				}
				PolledCaps::Empty => {
					if clock() >= deadline {
						return None;
					}
					let _ = wait_any(&[self.input], deadline);
				}
				PolledCaps::Closed => fail("the gamepad tool's output ended"),
			}
		}
	}

	// The first line starting `gamepad: <word> ` that `matches`, within ten seconds.
	fn expect(&mut self, what: &str, word: &str, matches: impl Fn(&str) -> bool) -> String {
		let deadline = clock() + 10 * TICKS;
		let prefix = format!("gamepad: {word} ");
		loop {
			let Some(line) = self.next(deadline) else { fail(what) };
			if let Some(rest) = line.strip_prefix(&prefix)
				&& matches(rest)
			{
				return String::from(rest);
			}
		}
	}
}

fn gamepad(probe: &mut Probe) {
	if stdin() == 0 {
		fail("there is no input - run it as `gamepad --lines | btclassic gamepad`");
	}
	let mut lines = Lines { input: stdin(), held: Vec::new() };
	let deadline = clock() + 10 * TICKS;
	loop {
		match lines.next(deadline) {
			Some(line) if line == "gamepad: watching" => break,
			Some(_) => {}
			None => fail("the gamepad tool never said it was watching"),
		}
	}
	probe.ready();
	let handle = match probe.read().scan(&0, &3000) {
		Some(Ok(handle)) => handle,
		_ => fail("the scan was refused"),
	};
	let deadline = clock() + 6 * TICKS;
	while clock() < deadline && matches!(probe.read().scanning(&handle), Some(Ok(true))) {
		sleep_until(clock() + TICKS / 4);
	}
	probe.pair(GAMEPAD);
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the gamepad did not bond");
	}
	probe.trust(GAMEPAD, Profile::Input, true);
	probe.page(GAMEPAD);
	probe.expect("this host did not take the gamepad's page", 3 * TICKS, |line| line.starts_with("accepted gamepad"));
	probe.act(GAMEPAD, FixtureAction::HidConnect, 0);
	probe.expect("the gamepad's interrupt channel was not accepted", 8 * TICKS, |line| line == "l2cap gamepad psm 0x0013 result 0");
	// IT ARRIVES IN THE GAMEPAD SET, under the shape its descriptor gave, at rest.
	let arrived = lines.expect("the Bluetooth gamepad did not arrive in the gamepad set", "arrived", |rest| rest.contains("label=bluetooth gamepad"));
	let id = String::from(arrived.split(' ').next().unwrap_or(""));
	if !arrived.contains("buttons=16") || !arrived.contains("hats=1") {
		fail("the Bluetooth gamepad arrived without its sixteen buttons and one hat");
	}
	say(&format!("the Bluetooth gamepad arrived as gamepad {id}"));
	let state_of = |rest: &str, key: &str| rest.split(' ').find_map(|part| part.strip_prefix(key).and_then(|value| value.strip_prefix('='))).map(String::from);
	// BUTTON 3 AND THE HAT EAST, and back.
	probe.act(GAMEPAD, FixtureAction::GamepadReport, (1 << 2) | (2 << 16) | (127 << 24));
	lines.expect("button 3 and the hat east did not show on the Bluetooth gamepad", "state", |rest| rest.starts_with(&format!("{id} ")) && state_of(rest, "buttons").as_deref() == Some("3") && state_of(rest, "hats").as_deref() == Some("E"));
	probe.act(GAMEPAD, FixtureAction::GamepadReport, (8 << 16) | (255 << 24));
	lines.expect("the release and X at 255 did not show on the Bluetooth gamepad", "state", |rest| rest.starts_with(&format!("{id} ")) && state_of(rest, "buttons").as_deref() == Some("none") && state_of(rest, "axes").as_deref() == Some("255,127,127,127"));
	say("its button, its hat and its X axis showed on it");
	// ITS LINK GOING IS ITS DEPARTURE.
	probe.act(GAMEPAD, FixtureAction::Disconnect, 0);
	lines.expect("the Bluetooth gamepad's departure did not show", "departed", |rest| rest.starts_with(&id));
	print(b"btclassic: PASS gamepad\n");
	// THE TOOL ENDS ON A CLOSED PIPE, so this input closes first and the gamepad comes back and reports: the line the
	// tool then writes meets the closed pipe, which ends it and lets the shell take the next command.
	close(stdin());
	probe.page(GAMEPAD);
	probe.expect("this host did not take the gamepad's page again", 3 * TICKS, |line| line.starts_with("accepted gamepad"));
	probe.act(GAMEPAD, FixtureAction::HidConnect, 0);
	probe.expect("the gamepad's interrupt channel was not accepted again", 8 * TICKS, |line| line == "l2cap gamepad psm 0x0013 result 0");
	// Its stream opens on InputService's own retry; a report a little later is the line that meets the pipe.
	sleep_until(clock() + 3 * TICKS);
	probe.act(GAMEPAD, FixtureAction::GamepadReport, 1);
	sleep_until(clock() + TICKS / 2);
	probe.act(GAMEPAD, FixtureAction::GamepadReport, 8 << 16);
}

// ------------------------------------------------------------------ le

fn le(probe: &mut Probe) {
	probe.ready();
	let handle = match probe.read().scan(&0, &4000) {
		Some(Ok(handle)) => handle,
		_ => fail("the scan was refused"),
	};
	let deadline = clock() + 7 * TICKS;
	while clock() < deadline && matches!(probe.read().scanning(&handle), Some(Ok(true))) {
		sleep_until(clock() + TICKS / 4);
	}
	let results = match probe.read().results(&handle) {
		Some(Ok(results)) => results,
		_ => fail("the scan's results could not be read"),
	};
	// THE TAG IS HEARD FROM A PRIVATE ADDRESS no bond resolves yet; the other two from their identities.
	let Some(tag) = results.iter().find(|result| result.name == "fixture tag").map(|result| result.address.clone()) else { fail("the scan did not hear the tag") };
	if tag.kind != PeerKind::Resolvable {
		fail("the tag was not reported as a resolvable private address");
	}
	for device in [REMOTE, DISPLAY] {
		if !results.iter().any(|result| result.address == identity_of(device) && result.radio == Radio::Le) {
			fail(&format!("the scan did not hear LE device {device} from its identity"));
		}
	}
	probe.expect("this host never set a private address of its own", 2 * TICKS, |line| line.starts_with("the host's private address "));
	say("the tag was heard from a private address, the remote and the display from their identities, and this host uses a private address of its own");

	// NUMERIC COMPARISON ON LE, from the tag's private address.
	probe.watch();
	if !matches!(probe.operator().pair(&0, &tag), Some(Ok(()))) {
		fail("pairing the tag was refused");
	}
	let prompt = probe.prompt("the tag's pairing raised no prompt", 10 * TICKS);
	let shown = probe.expect("the tag did not show a number", 3 * TICKS, |line| line.starts_with("compare tag "));
	if prompt.question != PromptQuestion::Compare || prompt.value != digits(&shown) || prompt.radio != Radio::Le {
		fail("the tag's prompt was not an LE comparison of the number it shows");
	}
	probe.answer(&prompt, PromptReply::Yes);
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the tag did not bond after a yes");
	}
	let paired_tag = probe.expect("the tag's link was not encrypted by its pairing", 3 * TICKS, |line| line.starts_with("encrypted tag with key "));
	probe.expect("the tag did not give its identity", 3 * TICKS, |line| line == "tag gave its identity and its resolving key");
	probe.expect("the tag did not learn this host's identity", 3 * TICKS, |line| line.starts_with("tag learned the host's identity, public "));
	match probe.bond(TAG) {
		Some(bond) if bond.address == identity_of(TAG) && bond.radio == Radio::Le && bond.level.agreement == KeyAgreement::SecureConnections && bond.level.authenticated => {}
		_ => fail("the tag's bond is not under its identity at Secure Connections, authenticated"),
	}
	say("Numeric Comparison on LE: the tag bonded at Secure Connections, authenticated, under the identity it gave");
	// ITS BATTERY SERVICE, read by the stack itself into the device's status.
	let deadline = clock() + 3 * TICKS;
	while !matches!(probe.operator().devices(&0), Some(Ok(devices)) if devices.iter().any(|device| device.address == identity_of(TAG) && device.battery == Some(87))) {
		if clock() >= deadline {
			fail("the tag's battery level is not in its status");
		}
		sleep_until(clock() + TICKS / 10);
	}
	say("the tag's Battery Service read into its status: 87");

	// PASSKEY ENTRY, THIS HOST TYPING what the display shows.
	if !matches!(probe.operator().pair(&0, &identity_of(DISPLAY)), Some(Ok(()))) {
		fail("pairing the display was refused");
	}
	let prompt = probe.prompt("the display's pairing raised no prompt", 10 * TICKS);
	let shown = probe.expect("the display did not show a passkey", 3 * TICKS, |line| line.starts_with("passkey display "));
	if prompt.question != PromptQuestion::EnterPasskey {
		fail("the display's pairing did not ask this host to type a passkey");
	}
	probe.answer(&prompt, PromptReply::Passkey(digits(&shown)));
	if probe.settle(15 * TICKS) != PairingState::Bonded {
		fail("the display did not bond with the passkey typed");
	}
	match probe.bond(DISPLAY) {
		Some(bond) if bond.level.agreement == KeyAgreement::SecureConnections && bond.level.authenticated => {}
		_ => fail("the display's bond is not Secure Connections, authenticated"),
	}
	say("Passkey Entry on LE: this host typed the digits the display showed, twenty rounds, and it bonded authenticated");

	// LE LEGACY ONLY ON THE OPERATOR'S WORD.
	if !matches!(probe.operator().pair(&0, &identity_of(REMOTE)), Some(Ok(()))) {
		fail("pairing the remote was refused outright");
	}
	if probe.settle(10 * TICKS) != PairingState::Failed {
		fail("a device without Secure Connections bonded without the operator asking for legacy pairing");
	}
	probe.expect("the refused remote's link was not dropped", 3 * TICKS, |line| line == "disconnected by the host remote");
	if !matches!(probe.operator().pair_legacy(&0, &identity_of(REMOTE)), Some(Ok(()))) {
		fail("pair-legacy was refused for the remote");
	}
	let prompt = probe.prompt("the remote's legacy pairing raised no prompt", 10 * TICKS);
	if prompt.question != PromptQuestion::ShowPasskey {
		fail("the remote's legacy pairing did not show a passkey to type on it");
	}
	probe.answer(&prompt, PromptReply::Yes);
	probe.act(REMOTE, FixtureAction::TypePasskey, prompt.value);
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the remote did not bond at the legacy level");
	}
	let given = probe.expect("the remote did not give its legacy key", 3 * TICKS, |line| line.starts_with("remote gave its long-term key "));
	let remote_key = String::from(given.split(' ').nth(5).unwrap_or(""));
	match probe.bond(REMOTE) {
		Some(bond) if bond.level.agreement == KeyAgreement::LeLegacy && bond.level.authenticated => {}
		_ => fail("the remote's bond is not at the legacy level, authenticated"),
	}
	if !matches!(probe.operator().pair_legacy(&0, &identity_of(TAG)), Some(Err(proto::system::Error::Denied))) {
		fail("legacy pairing was not refused over the tag's Secure Connections bond");
	}
	say("LE legacy: refused until pair-legacy, then a passkey bond at the legacy level; never over a Secure Connections bond");

	// SEVERAL LE LINKS AT ONCE, and this host's attribute server.
	let connected = match probe.operator().devices(&0) {
		Some(Ok(devices)) => devices.into_iter().filter(|device| device.radio == Radio::Le && device.connected).count(),
		_ => 0,
	};
	if connected < 3 {
		fail(&format!("{connected} LE links were up at once; the tag, the display and the remote are expected"));
	}
	probe.act(TAG, FixtureAction::ReadHostName, 0);
	probe.expect("the tag could not read this host's name", 3 * TICKS, |line| line == "tag read the host's name LiberSystem");
	say("three LE links at once, and this host's name read through its attribute server");

	// RECONNECTION THROUGH THE ACCEPT LIST, the tag resolved by its key, each on its stored key.
	probe.unwatch();
	probe.trust(TAG, Profile::Input, true);
	probe.trust(REMOTE, Profile::Input, true);
	probe.act(TAG, FixtureAction::Disconnect, 0);
	let back = probe.expect("the tag did not come back through the accept list", 5 * TICKS, |line| line == "connected tag - the host from its random address");
	let _ = back;
	probe.expect("the tag did not resolve this host's private address", 2 * TICKS, |line| line == "tag resolved the host's private address with the identity it was given");
	let encrypted = probe.expect("the tag's link was not encrypted on its stored key", 5 * TICKS, |line| line.starts_with("encrypted tag with key "));
	if encrypted != paired_tag {
		fail("the tag reconnected on a key other than the one its pairing made");
	}
	probe.act(REMOTE, FixtureAction::Disconnect, 0);
	probe.expect("the remote did not come back through the accept list", 5 * TICKS, |line| line == "connected remote - the host from its random address");
	let encrypted = probe.expect("the remote's link was not encrypted on its legacy key", 5 * TICKS, |line| line.starts_with("encrypted remote with key "));
	if !encrypted.ends_with(&remote_key) {
		fail("the remote reconnected on a key other than the one it gave");
	}
	say("the tag and the remote reconnected through the accept list, the tag resolved by its key, each encrypted on its stored key");
	// THE TAG, NAMED FOR APPLICATIONS: an alias, and trust for GATT - what `btgatt`'s grant is checked against.
	if !matches!(probe.operator().alias(&0, &identity_of(TAG), "tag-1"), Some(Ok(()))) {
		fail("the tag could not be aliased");
	}
	probe.trust(TAG, Profile::Gatt, true);
	print(b"btclassic: PASS le\n");
}

// ------------------------------------------------------------------ ctkd

// The phone's public address, as its LE half is heard and bonded under.
fn phone_le() -> PeerAddress {
	PeerAddress { kind: PeerKind::Public, bytes: alloc::vec![0x00, 0x1b, 0xdc, 0x20, 0x00, PHONE] }
}

// A bond the operator lists, by address.
fn bond_at(probe: &Probe, address: &PeerAddress) -> Option<BondedPeer> {
	match probe.operator().bonded(&0) {
		Some(Ok(peers)) => peers.into_iter().find(|peer| peer.address == *address),
		_ => fail("the bonds could not be listed"),
	}
}

// The key fingerprint a far-side line ends with.
fn key_of(line: &str) -> String {
	String::from(line.rsplit(' ').next().unwrap_or(""))
}

fn ctkd(probe: &mut Probe) {
	probe.ready();
	probe.watch();
	// BR/EDR FIRST: Numeric Comparison with the phone makes an authenticated P-256 key, and this host - the link's
	// central - derives the LE key from it over the BR/EDR Security Manager.
	probe.pair(PHONE);
	let prompt = probe.prompt("the phone's pairing raised no prompt", 10 * TICKS);
	probe.answer(&prompt, PromptReply::Yes);
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the phone did not bond on BR/EDR");
	}
	probe.expect("the phone's pairing did not make a Secure Connections key", 3 * TICKS, |line| line.starts_with("paired phone type 0x08 "));
	let derived = probe.expect("the phone did not derive an LE key over BR/EDR", 5 * TICKS, |line| line.starts_with("phone derived an LE key over BR/EDR, key "));
	probe.expect("the phone did not learn this host's identity over BR/EDR", 3 * TICKS, |line| line == "phone learned the host's identity over BR/EDR");
	let deadline = clock() + 3 * TICKS;
	let le = loop {
		if let Some(bond) = bond_at(probe, &phone_le()) {
			break bond;
		}
		if clock() >= deadline {
			fail("no LE bond for the phone was derived from its BR/EDR key");
		}
		sleep_until(clock() + TICKS / 10);
	};
	if le.radio != Radio::Le || le.level.agreement != KeyAgreement::SecureConnections || !le.level.authenticated {
		fail("the phone's derived LE bond is not Secure Connections, authenticated - the level of the key it came from");
	}
	say("BR/EDR to LE: the phone's LE key derived over the BR/EDR Security Manager, at the link key's level");
	// THE LE HALF, ENCRYPTED ON THE KEY BOTH SIDES DERIVED APART.
	probe.act(PHONE, FixtureAction::Disconnect, 0);
	probe.expect("the phone did not drop its BR/EDR link", 2 * TICKS, |line| line == "phone disconnected");
	if !matches!(probe.operator().trust(&0, &phone_le(), &Profile::Gatt, &true), Some(Ok(()))) {
		fail("the phone's LE bond could not be trusted");
	}
	probe.act(PHONE, FixtureAction::LeAdvertise, 1);
	probe.expect("the phone's LE half did not come through the accept list", 5 * TICKS, |line| line.starts_with("connected phone - the host from its "));
	let encrypted = probe.expect("the phone's LE link was not encrypted", 5 * TICKS, |line| line.starts_with("encrypted phone with key ") || line == "encryption REFUSED for phone");
	if key_of(&encrypted) != key_of(&derived) {
		fail("the phone's LE half was not encrypted on the key its BR/EDR half derived");
	}
	say("the phone's LE half encrypted on the derived key, which both sides computed apart");

	// LE FIRST: the phone forgotten on both radios and reset, then paired on LE; the BR/EDR key comes from its LTK.
	probe.act(PHONE, FixtureAction::LeAdvertise, 0);
	for address in [address_of(PHONE), phone_le()] {
		if !matches!(probe.operator().forget(&0, &address), Some(Ok(()))) {
			fail("forgetting the phone was refused");
		}
	}
	probe.act(PHONE, FixtureAction::Forget, 0);
	probe.act(PHONE, FixtureAction::LeAdvertise, 1);
	// THE OPERATOR PAIRS WHAT A SCAN HEARD: the phone's LE half, from its public address.
	let scan = match probe.read().scan(&0, &3000) {
		Some(Ok(handle)) => handle,
		_ => fail("the scan was refused"),
	};
	let deadline = clock() + 6 * TICKS;
	loop {
		let heard = matches!(probe.read().results(&scan), Some(Ok(results)) if results.iter().any(|result| result.address == phone_le() && result.radio == Radio::Le));
		if heard {
			break;
		}
		if clock() >= deadline {
			fail("the scan did not hear the phone's LE half from its public address");
		}
		sleep_until(clock() + TICKS / 4);
	}
	if !matches!(probe.operator().pair(&0, &phone_le()), Some(Ok(()))) {
		fail("pairing the phone on LE was refused");
	}
	let prompt = probe.prompt("the phone's LE pairing raised no prompt", 10 * TICKS);
	let shown = probe.expect("the phone's LE half did not show a number", 3 * TICKS, |line| line.starts_with("compare phone "));
	if prompt.question != PromptQuestion::Compare || prompt.value != digits(&shown) || prompt.radio != Radio::Le {
		fail("the phone's LE prompt was not a comparison of the number it shows");
	}
	probe.answer(&prompt, PromptReply::Yes);
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the phone did not bond on LE");
	}
	let derived = probe.expect("the phone did not derive its BR/EDR key from the LE pairing", 3 * TICKS, |line| line.starts_with("phone derived its BR/EDR key from the LE pairing, key "));
	match probe.bond(PHONE) {
		Some(bond) if bond.radio == Radio::Classic && bond.level.agreement == KeyAgreement::SecureConnections && bond.level.authenticated => {}
		_ => fail("no BR/EDR bond for the phone was derived from its LE pairing at Secure Connections, authenticated"),
	}
	say("LE to BR/EDR: the phone's BR/EDR key derived from the LTK, at its level");
	// THE BR/EDR HALF AUTHENTICATES ON IT: the phone pages and asks for the key it derived.
	probe.act(PHONE, FixtureAction::LeAdvertise, 0);
	probe.page(PHONE);
	probe.expect("this host did not take the phone's page", 3 * TICKS, |line| line == "accepted phone - this host took its page");
	probe.act(PHONE, FixtureAction::Pair, 0);
	let authenticated = probe.expect("the phone's BR/EDR half did not authenticate", 5 * TICKS, |line| line.starts_with("authenticated phone with ") || line.starts_with("authentication REFUSED for phone"));
	if !authenticated.starts_with("authenticated phone with a remembered key ") || key_of(&authenticated) != key_of(&derived) {
		fail("the phone's BR/EDR half did not authenticate on the key its LE half derived");
	}
	say("the phone's BR/EDR half authenticated on the derived link key, which both sides computed apart");
	print(b"btclassic: PASS ctkd\n");
}

// ------------------------------------------------------------------ audio

// AudioService's devices, as `audioctl` reads them.
fn audio_devices(probe: &Probe) -> Vec<AudioDevice> {
	audio_control::Client::new(ChannelTransport { chan: probe.control }).devices().unwrap_or_else(|| fail("AudioService's inventory could not be read"))
}

// The first Bluetooth device AudioService lists that `matches`, waiting up to `ticks`.
fn bluetooth_device(probe: &Probe, what: &str, ticks: u64, matches: impl Fn(&AudioDevice) -> bool) -> AudioDevice {
	let deadline = clock() + ticks;
	loop {
		if let Some(device) = audio_devices(probe).into_iter().find(|device| device.transport == AudioTransport::Bluetooth && matches(device)) {
			return device;
		}
		if clock() >= deadline {
			fail(what);
		}
		sleep_until(clock() + TICKS / 5);
	}
}

// Ten milliseconds of a 7.5 kHz tone at 48 kHz stereo - the third of eight subbands - as a buffer a write carries.
fn tone_period(frames: usize, start: &mut usize) -> Buffer {
	let bytes = frames * 4;
	let handle = memory_object_create(bytes as u64);
	if handle < 0 {
		fail("no memory for a period");
	}
	let handle = handle as u64;
	let Some(base) = (unsafe { map_object(handle) }) else { fail("a period could not be mapped") };
	let samples = unsafe { core::slice::from_raw_parts_mut(base as *mut i16, frames * 2) };
	for frame in 0..frames {
		// A square wave of period 6.4 samples, near enough a tone at 7.5 kHz for the subband it falls in.
		let phase = (*start + frame) * 10 % 64;
		let value: i16 = if phase < 32 { 9_000 } else { -9_000 };
		samples[frame * 2] = value;
		samples[frame * 2 + 1] = value;
	}
	*start += frames;
	unmap_object(handle);
	Buffer { handle, len: bytes as u64 }
}

fn music(probe: &mut Probe) {
	probe.ready();
	// THE HEADSET, paired by Just Works, trusted for audio and connected for it.
	let handle = match probe.read().scan(&0, &3000) {
		Some(Ok(handle)) => handle,
		_ => fail("the scan was refused"),
	};
	let deadline = clock() + 6 * TICKS;
	while clock() < deadline && !matches!(probe.read().results(&handle), Some(Ok(results)) if [HEADSET, PHONE].iter().all(|device| results.iter().any(|result| result.address == address_of(*device)))) {
		sleep_until(clock() + TICKS / 4);
	}
	probe.pair(HEADSET);
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the headset did not bond");
	}
	probe.trust(HEADSET, Profile::Audio, true);
	if !matches!(probe.operator().connect(&0, &address_of(HEADSET), &Profile::Audio), Some(Ok(()))) {
		fail("connecting the headset for audio was refused");
	}
	probe.expect("the headset was never configured for SBC", 8 * TICKS, |line| line == "headset was configured for SBC");
	probe.expect("the headset reported no delay", 2 * TICKS, |line| line == "headset reported a delay of 150 ms");
	probe.expect("the headset's media channel never opened", 3 * TICKS, |line| line == "headset media channel open");
	let headset = bluetooth_device(probe, "AudioService never listed the headset as an output", 6 * TICKS, |device| device.output.is_some() && !device.route && !device.voice);
	if !headset.default_output {
		fail("the headset that arrived did not become the default output");
	}
	if headset.latency_us < 150_000 {
		fail("the headset's latency does not include the delay it reported");
	}
	if !headset.hardware_volume || headset.volume != 50 {
		fail("the headset's own level was not taken up - it registered the absolute volume at 0x40");
	}
	say(&format!("the headset is AudioService's default output: {} Hz, its delay in a latency of {} us, its own level {}", headset.output.as_ref().map_or(0, |format| format.rate), headset.latency_us, headset.volume));

	// MUSIC, FROM AN APPLICATION'S STREAM, through SBC to the headset.
	let stream = match audio::Client::new(ChannelTransport { chan: probe.audio }).open_stream(&48_000, &2) {
		Some(Ok(stream)) => stream,
		other => fail(&format!("a stream could not be opened: {other:?}")),
	};
	let mut start = 0usize;
	for _ in 0..100 {
		match pcm_stream::Client::new(ChannelTransport { chan: stream }).write(&tone_period(480, &mut start)) {
			Some(Ok(accepted)) if accepted > 0 => {}
			other => fail(&format!("a write was not accepted: {other:?}")),
		}
	}
	let _ = pcm_stream::Client::new(ChannelTransport { chan: stream }).close();
	close(stream);
	probe.expect("the headset never started its stream", 3 * TICKS, |line| line == "headset started its stream");
	let heard = probe.expect("the headset said nothing of what it heard", 8 * TICKS, |line| line.starts_with("headset heard "));
	if !heard.contains("48000 Hz joint stereo, 8 subbands, bitpool 53, every CRC good, the loudest subband 2") {
		fail(&format!("the headset did not hear the tone as it was sent: {heard}"));
	}
	say(&format!("a second of a tone through SBC: {}", heard.trim_start_matches("headset ")));

	// THE LEVEL, BOTH WAYS: sent as the absolute volume, and the headset's own change taken back.
	if !matches!(audio_control::Client::new(ChannelTransport { chan: probe.control }).set_volume(&headset.id, &80), Some(Ok(()))) {
		fail("the headset's level could not be set");
	}
	probe.expect("the headset was not sent the level", 3 * TICKS, |line| line == "headset's volume was set to 102");
	probe.act(HEADSET, FixtureAction::Volume, 0x7f);
	bluetooth_device(probe, "the headset's own change of level did not reach AudioService", 3 * TICKS, |device| device.id == headset.id && device.volume == 100);
	say("the level both ways: 80 sent as absolute volume 102, and the headset's own change to 127 taken back as 100");

	// ITS PLAY BUTTON, answered NOT IMPLEMENTED: there is no media session.
	probe.act(HEADSET, FixtureAction::PressPlay, 0);
	probe.expect("the headset's play was not answered not implemented", 3 * TICKS, |line| line == "headset's play was answered not implemented");
	say("the headset's play button answered not implemented - there is no media session to act on");

	// THE PHONE STREAMS TO THIS HOST: a route to the default output, never an input.
	probe.watch();
	probe.pair(PHONE);
	let prompt = probe.prompt("the phone's pairing raised no prompt", 10 * TICKS);
	probe.answer(&prompt, PromptReply::Yes);
	if probe.settle(10 * TICKS) != PairingState::Bonded {
		fail("the phone did not bond");
	}
	probe.unwatch();
	probe.trust(PHONE, Profile::Audio, true);
	probe.act(PHONE, FixtureAction::Stream, 3);
	probe.expect("the phone never streamed", 8 * TICKS, |line| line == "phone is streaming to the host");
	let route = bluetooth_device(probe, "AudioService never listed the phone's stream", 4 * TICKS, |device| device.route);
	if route.input.is_none() || route.output.is_some() || route.default_input {
		fail("the phone's stream is listed as something other than a route");
	}
	let deadline = clock() + 3 * TICKS;
	loop {
		let streams = audio_control::Client::new(ChannelTransport { chan: probe.control }).streams().unwrap_or_default();
		if streams.iter().any(|stream| stream.route && stream.device == Some(headset.id)) {
			break;
		}
		if clock() >= deadline {
			fail("the phone's stream is not playing on the default output");
		}
		sleep_until(clock() + TICKS / 5);
	}
	say(&format!("the phone's stream is a route at {} Hz, played on the default output and offered to no recorder", route.input.as_ref().map_or(0, |format| format.rate)));
	if !matches!(probe.operator().media(&0, &address_of(PHONE), &MediaCommand::Pause), Some(Ok(()))) {
		fail("the operator's pause was refused");
	}
	probe.expect("the phone was not told pause", 3 * TICKS, |line| line == "phone was told pause");
	probe.expect("the phone's stream did not end", 6 * TICKS, |line| line.starts_with("phone stopped streaming after "));
	say("the operator's pause reached the phone over the remote control");

	// THE HEADSET GOES: the default returns to what it displaced.
	probe.act(HEADSET, FixtureAction::Disconnect, 0);
	let deadline = clock() + 4 * TICKS;
	while audio_devices(probe).iter().any(|device| device.id == headset.id) {
		if clock() >= deadline {
			fail("the headset's output did not leave AudioService when its link did");
		}
		sleep_until(clock() + TICKS / 5);
	}
	say("the headset's link gone, its output left AudioService's inventory");
	print(b"btclassic: PASS audio\n");
}

// ------------------------------------------------------------------ voice

// The calls a voice session's commands stream delivered, as far as they have come.
fn commands_seen(stream: u64, seen: &mut Vec<CallCommand>) {
	let mut buf = [0u8; 64];
	while let PolledCaps::Message { len, handles } = try_recv_caps(stream, &mut buf) {
		let mut handles = handles;
		if let Some(command) = voice_session::commands_read(&buf[..len], &mut handles) {
			seen.push(command);
		}
		for &leftover in handles.as_slice() {
			close(leftover);
		}
	}
}

// Ten milliseconds of a 2.5 kHz tone at 16 kHz mono - the third of mSBC's eight subbands - as a buffer.
fn voice_period(start: &mut usize) -> Buffer {
	let frames = 160usize;
	let handle = memory_object_create((frames * 2) as u64);
	if handle < 0 {
		fail("no memory for a period");
	}
	let handle = handle as u64;
	let Some(base) = (unsafe { map_object(handle) }) else { fail("a period could not be mapped") };
	let samples = unsafe { core::slice::from_raw_parts_mut(base as *mut i16, frames) };
	for (at, sample) in samples.iter_mut().enumerate() {
		// A square wave of period 6.4 samples: 2.5 kHz at 16 kHz.
		*sample = if (*start + at) * 10 % 64 < 32 { 8_000 } else { -8_000 };
	}
	*start += frames;
	unmap_object(handle);
	Buffer { handle, len: (frames * 2) as u64 }
}

fn voice(probe: &mut Probe) {
	probe.ready();
	// THE HEADSET, bonded - by the music phase before this one, or paired here - and trusted for voice.
	if probe.bond(HEADSET).is_none() {
		let handle = match probe.read().scan(&0, &3000) {
			Some(Ok(handle)) => handle,
			_ => fail("the scan was refused"),
		};
		let deadline = clock() + 6 * TICKS;
		while clock() < deadline && !matches!(probe.read().results(&handle), Some(Ok(results)) if results.iter().any(|result| result.address == address_of(HEADSET))) {
			sleep_until(clock() + TICKS / 4);
		}
		probe.pair(HEADSET);
		if probe.settle(10 * TICKS) != PairingState::Bonded {
			fail("the headset did not bond");
		}
	}
	probe.trust(HEADSET, Profile::Voice, true);
	// THE HEADSET CONNECTS TO THIS GATEWAY and sets its service level connection up.
	probe.act(HEADSET, FixtureAction::HfpConnect, 0);
	probe.expect("the headset's service level connection did not come up", 10 * TICKS, |line| line == "headset's service level connection is up, codec mSBC, its battery 80");
	let headset = bluetooth_device(probe, "AudioService never listed the headset's voice", 5 * TICKS, |device| device.voice);
	if !headset.default_voice || headset.output.as_ref().map(|format| (format.rate, format.channels)) != Some((16_000, 1)) || headset.input.as_ref().map(|format| format.rate) != Some(16_000) {
		fail("the headset's voice is not the default voice device at 16 kHz mono both ways");
	}
	match probe.operator().devices(&0) {
		Some(Ok(devices)) if devices.iter().any(|device| device.address == address_of(HEADSET) && device.battery == Some(80)) => {}
		_ => fail("the headset's battery is not in its status"),
	}
	say("the headset's service level connection: mSBC negotiated, its voice AudioService's default voice device at 16 kHz, its battery 80 in its status");

	// NO SESSION, NO CALL: every call command refused, and no audio link.
	probe.act(HEADSET, FixtureAction::Answer, 0);
	probe.expect("the headset's answer with no call was not refused", 3 * TICKS, |line| line == "headset's ATA was answered ERROR");
	probe.act(HEADSET, FixtureAction::AudioRequest, 0);
	probe.expect("the headset's request for audio with no session was not refused", 3 * TICKS, |line| line == "headset's AT+BCC was answered ERROR");
	say("with no session: the headset's answer and its request for audio refused, and no link set up");

	// A VOICE SESSION: the link comes up, both directions carry the tone, and the call is relayed both ways.
	let session = match audio::Client::new(ChannelTransport { chan: probe.voice }).open_voice(&16_000) {
		Some(Ok(session)) => session,
		other => fail(&format!("the voice session could not be opened: {other:?}")),
	};
	let client = || voice_session::Client::new(ChannelTransport { chan: session });
	probe.expect("the voice link did not come up with the session", 5 * TICKS, |line| line == "headset's voice link is up: transparent, mSBC");
	let commands = match client().commands() {
		Some(Ok(stream)) => stream,
		other => fail(&format!("the commands stream was refused: {other:?}")),
	};
	let mut start = 0usize;
	let mut heard = 0usize;
	let mut loud = false;
	for _ in 0..80 {
		match client().write(&voice_period(&mut start)) {
			Some(Ok(accepted)) if accepted > 0 => {}
			other => fail(&format!("the session's write was not accepted: {other:?}")),
		}
		if let Some(Ok(period)) = client().read() {
			heard += period.len() / 2;
			loud |= period.chunks_exact(2).any(|pair| i16::from_le_bytes([pair[0], pair[1]]).unsigned_abs() > 1_000);
		}
	}
	if heard < 8_000 || !loud {
		fail(&format!("the headset's microphone did not reach the session: {heard} samples"));
	}
	say(&format!("a session's voice both ways: {heard} samples of the headset's microphone read, at 16 kHz"));
	if !matches!(client().set_call(&CallState::Incoming), Some(Ok(()))) {
		fail("the incoming call could not be declared");
	}
	probe.expect("the headset did not ring", 3 * TICKS, |line| line == "headset saw RING");
	probe.act(HEADSET, FixtureAction::Answer, 0);
	probe.expect("the headset's answer was not taken", 3 * TICKS, |line| line == "headset's ATA was answered OK");
	let mut seen = Vec::new();
	let deadline = clock() + 3 * TICKS;
	while !seen.contains(&CallCommand::Answer) {
		commands_seen(commands, &mut seen);
		if clock() >= deadline {
			fail("the headset's answer did not reach the session");
		}
		sleep_until(clock() + TICKS / 10);
	}
	let _ = client().set_call(&CallState::Active);
	probe.expect("the active call was not indicated", 3 * TICKS, |line| line == "headset saw +CIEV: 2,1");
	probe.act(HEADSET, FixtureAction::HangUp, 0);
	let deadline = clock() + 3 * TICKS;
	while !seen.contains(&CallCommand::HangUp) {
		commands_seen(commands, &mut seen);
		if clock() >= deadline {
			fail("the headset's hang-up did not reach the session");
		}
		sleep_until(clock() + TICKS / 10);
	}
	let _ = client().set_call(&CallState::None);
	probe.expect("the end of the call was not indicated", 3 * TICKS, |line| line == "headset saw +CIEV: 2,0");
	say("the call relayed both ways: rung, answered from the headset, active, hung up from the headset");

	// THE LEVEL, as the speaker gain.
	if !matches!(audio_control::Client::new(ChannelTransport { chan: probe.control }).set_volume(&headset.id, &60), Some(Ok(()))) {
		fail("the headset's level could not be set");
	}
	probe.expect("the headset's speaker gain was not set", 3 * TICKS, |line| line == "headset's speaker gain was set to 9");

	// THE SESSION CLOSES: the link goes down, and the headset heard the session's tone.
	let _ = client().close();
	close(commands);
	close(session);
	let report = probe.expect("the voice link did not go down with the session", 5 * TICKS, |line| line.starts_with("headset heard ") && line.ends_with("its voice link is down"));
	if !report.contains("every CRC good, the loudest subband 2") {
		fail(&format!("the headset did not hear the session's tone: {report}"));
	}
	say(&format!("the session closed and its link with it: {}", report.trim_start_matches("headset ")));
	print(b"btclassic: PASS voice\n");
}

// ------------------------------------------------------------------ serial

// THE SERIAL DEVICE, NAMED FOR APPLICATIONS: an alias, and trust for the serial port - what `btserial`'s grant is
// checked against. Bonded by the pairing phase.
fn serial(probe: &mut Probe) {
	probe.ready();
	if probe.bond(SERIAL).is_none() {
		fail("the serial device is not bonded");
	}
	if !matches!(probe.operator().alias(&0, &address_of(SERIAL), "serial-1"), Some(Ok(()))) {
		fail("the serial device could not be aliased");
	}
	probe.trust(SERIAL, Profile::Spp, true);
	say("the serial device is aliased serial-1 and trusted for the serial port, for an application's grant");
	print(b"btclassic: PASS serial\n");
}

// ------------------------------------------------------------------ transfer

// THE PEERS OF THE FILE PUSHES: the phone bonded by Numeric Comparison, the serial device by Just Works - and a push the
// phone tries with no receiver waiting refused at this host's door.
fn transfer(probe: &mut Probe) {
	probe.ready();
	let handle = match probe.read().scan(&0, &3000) {
		Some(Ok(handle)) => handle,
		_ => fail("the scan was refused"),
	};
	let deadline = clock() + 6 * TICKS;
	while clock() < deadline && !matches!(probe.read().results(&handle), Some(Ok(results)) if [PHONE, SERIAL].iter().all(|device| results.iter().any(|result| result.address == address_of(*device)))) {
		sleep_until(clock() + TICKS / 4);
	}
	if probe.bond(PHONE).is_none() {
		probe.watch();
		probe.pair(PHONE);
		let prompt = probe.prompt("the phone's pairing raised no prompt", 10 * TICKS);
		probe.answer(&prompt, PromptReply::Yes);
		if probe.settle(10 * TICKS) != PairingState::Bonded {
			fail("the phone did not bond");
		}
		probe.unwatch();
	}
	if probe.bond(SERIAL).is_none() {
		probe.pair(SERIAL);
		if probe.settle(10 * TICKS) != PairingState::Bonded {
			fail("the serial device did not bond");
		}
	}
	say("the phone and the serial device are bonded");
	// NO RECEIVER WAITING: the phone's push is refused before a byte of it.
	probe.act(PHONE, FixtureAction::PushObject, 100);
	probe.expect("a push with no receiver waiting was not refused", 10 * TICKS, |line| line.starts_with("phone's push was refused"));
	say("a push with no receiver waiting was refused");
	print(b"btclassic: PASS transfer\n");
}

// THE PHONE WILL PUSH an object of `size` bytes five seconds from now: the gate's next line is the receiver.
fn push(probe: &mut Probe, size: u32) {
	probe.ready();
	probe.act(PHONE, FixtureAction::PushObject, size);
	probe.expect("the phone did not take the word to push", 3 * TICKS, |line| line.starts_with("phone will push "));
	print(format!("btclassic: PASS push - the phone pushes {size} bytes in five seconds\n").as_bytes());
}

// ------------------------------------------------------------------ forget

fn forget(probe: &mut Probe) {
	probe.ready();
	if !matches!(probe.operator().forget(&0, &address_of(KEYBOARD)), Some(Ok(()))) {
		fail("forgetting the keyboard was refused");
	}
	if probe.bond(KEYBOARD).is_some() {
		fail("the keyboard's bond survived a forget");
	}
	// The headset is still trusted, so the radio is still connectable - and the keyboard's page is refused.
	probe.page(KEYBOARD);
	probe.expect("this host took a forgotten keyboard's page", 3 * TICKS, |line| line.starts_with("rejected keyboard"));
	print(b"btclassic: PASS forget\n");
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf = [0u8; 256];
	inherit_stdout(bootstrap);
	let context: Option<LaunchContext> = recv_launch_bytes(bootstrap).as_deref().and_then(LaunchContext::decode);
	// THE GRANTS IN THE ORDER PERMISSIONMANAGER DELIVERS THEM.
	let audio = recv_tagged(bootstrap, &mut buf, b"AUDIO_STREAM").unwrap_or(0);
	let read = recv_tagged(bootstrap, &mut buf, b"BTREAD").unwrap_or(0);
	let operator = recv_tagged(bootstrap, &mut buf, b"BTOPERATOR").unwrap_or(0);
	let fixture = recv_tagged(bootstrap, &mut buf, b"FIXTURE").unwrap_or(0);
	let control = recv_tagged(bootstrap, &mut buf, b"AUDIOCONTROL").unwrap_or(0);
	let voice_grant = recv_tagged(bootstrap, &mut buf, b"AUDIO_VOICE").unwrap_or(0);
	if read == 0 || operator == 0 || fixture == 0 || audio == 0 || control == 0 || voice_grant == 0 {
		fail("a grant this probe needs was not delivered");
	}
	let mut probe = Probe { read, operator, fixture, audio, control, voice: voice_grant, seen: Vec::new(), watcher: 0 };
	let phase = context.map(|context| String::from(context.arguments.trim())).unwrap_or_default();
	match phase.as_str() {
		"pair" => pair(&mut probe),
		"policy" => policy(&mut probe),
		"reuse" => reuse(&mut probe),
		"forget" => forget(&mut probe),
		"input" => input(&mut probe),
		"gamepad" => gamepad(&mut probe),
		"le" => le(&mut probe),
		"ctkd" => ctkd(&mut probe),
		"audio" => music(&mut probe),
		"voice" => voice(&mut probe),
		"serial" => serial(&mut probe),
		"transfer" => transfer(&mut probe),
		other if other.starts_with("push ") => push(&mut probe, other[5..].trim().parse().unwrap_or_else(|_| fail("usage: btclassic push BYTES"))),
		// TYPED AT THE PROMPT BY THE BLUETOOTH KEYBOARD: that this ran is the claim.
		"typed" => print(b"btclassic: PASS typed - this command was typed by the Bluetooth keyboard\n"),
		"alive" => print(b"btclassic: PASS alive - the machine still runs after the keyboard's Ctrl+Alt+Delete and Power key\n"),
		_ => fail("usage: btclassic pair|policy|reuse|forget|input|typed|alive|gamepad|le|ctkd|audio|voice|serial|transfer|push BYTES"),
	}
	probe.unwatch();
	exit();
}
