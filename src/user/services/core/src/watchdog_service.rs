// WatchdogService - the one consumer of every published hardware watchdog, and the one thing that pets them.
//
// WHAT FEEDS A TIMER. A pet is earned: each period the service asks ServiceManager's standing loop `alive` over the
// `supervisor-liveness` channel ServiceManager handed it, and pets only for an answer within the deadline. An answer
// proves the kernel schedules and delivers, the supervisor's loop is not deadlocked, and - because this service asked
// - its own loop runs. No answer skips that pet, and the hardware decides after the timeout. THE FIRST ANSWER STARTS
// IT: nothing is armed, disarmed or fed before it, because the standing loop answers only after bring-up and a timer
// found running is its driver's to bridge until then.
//
// WHAT IT ARMS. Every `watchdog` provider published is taken, at most four, one per device name. With the policy on,
// ONE is armed - the device `watchdog.device` names, or the first of WDAT, TCO, `i6300esb`, BMC whose arm succeeds -
// and every other is disarmed where the device allows it and otherwise fed at the configured timeout. An armed timer
// is never switched for another. With the policy off, a timer is disarmed as it attaches where the device allows it,
// and one that cannot be stopped is fed. EVERY DECISION is `service_logic::watchdog`'s; this is the wire around it.
//
// THE MODE decides the policy's default, and it comes from ServiceManager at every start - a payload with no bytes is
// refused, so a relaunch that sent the bare tag fails rather than arming a test boot with the shipping default.
//
// THE ORDERLY SHUTDOWN tells it `prepare(action)` on its control channel before the kills: each running timer reset
// with the platform is disarmed or given its longest timeout and a last pet; one that survives the reset is disarmed
// at power-off and given the boot bound at reboot, so a boot that hangs is still caught.
//
// A RESTART CHANGES NOTHING the hardware holds: the replacement takes the providers again and chooses as the first
// instance did, at its first answered `alive`.
//
// THE SLEEP'S ANNOUNCEMENT, on the same control channel: every running timer it holds is set to its longest timeout and
// petted once - a disarmed one is left alone, since a timeout set on it would start it - because ServiceManager, running
// the transaction, answers no `alive` until the resume notice. Nothing is asked or petted between the two. The resume
// notice restores the configured timeout on each of them, and the questions start again. The devices' own steps in the
// sleep - disarmed, or bounding it - are their drivers'.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ipc_client::ChannelTransport;
use proto::system::{Error, ProviderInfo, ProviderKind, ShutdownAction, SleepState, WatchdogDescription, config, provider_catalogue, shutdown_notice, sleep_notice, supervisor_liveness, watchdog};
use rt::*;
use service_logic::watchdog as wl;
use wire::{Handles, Reader, Transport, TransportError};

include!(concat!(env!("OUT_DIR"), "/roles_watchdog_service.rs"));

// The roles received through the plan's reader, before the mode payload this service reads by hand.
const PLANNED_ROLES: usize = 3;
// How long a provider has to answer one request: a register write or two behind a driver's loop.
const PROVIDER_TICKS: u64 = TICKS_PER_SECOND / 2;
// THE BOOT BOUND, ten minutes (the owner, 2026-10-04): what a timer that survives the reset is armed with at an orderly
// reboot - the time a shutdown and the boot after it are given from the notice to the next watchdog service's first
// pet. The 120 s first proposed reset a development image mid-boot behind a BMC; a short bound turns one slow boot -
// a hibernation image read back, a long check - into a machine that resets itself in a loop, and a long one only
// catches a hung boot later. `watchdog.boot-bound-ms` sets another.
const DEFAULT_BOOT_BOUND_MS: u32 = 600_000;

fn now_ms() -> u64 {
	clock() * 1000 / TICKS_PER_SECOND
}

fn ticks_at(ms: u64) -> u64 {
	(ms * TICKS_PER_SECOND).div_ceil(1000)
}

// One taken provider, in the order `Choice` holds them.
struct Timer {
	info: ProviderInfo,
	chan: u64,
	description: WatchdogDescription,
}

struct Service {
	policy: wl::Policy,
	boot_bound_ms: u32,
	choice: wl::Choice,
	timers: Vec<Timer>,
	schedule: wl::Schedule,
	// The liveness question in flight, by wire correlation.
	liveness: u64,
	next_corr: u32,
	// The first answer within the deadline has come: the choice has begun.
	started: bool,
	// Set by the shutdown notice: nothing is asked, armed or petted after it.
	shutting_down: bool,
	// Set between the sleep's announcement and its resume notice: nothing is asked or petted.
	sleeping: bool,
	named_reported: bool,
}

fn say(parts: &[&[u8]]) {
	print(b"WatchdogService: ");
	for part in parts {
		print(part);
	}
	print(b"\n");
}

fn decimal(value: u64) -> String {
	alloc::format!("{value}")
}

// A generated client's request, captured instead of sent: the encoding is the generator's, the sending is this
// service's own - without waiting, under a correlation of its choosing.
struct Capture {
	bytes: Vec<u8>,
}

impl Transport for &mut Capture {
	fn call(&mut self, request: &[u8], _request_handles: &[u64], _reply_handles: &mut Handles, _deadline: u64) -> Result<Vec<u8>, TransportError> {
		self.bytes = request.to_vec();
		Err(TransportError::TimedOut)
	}
	fn discard_handles(&mut self, _handles: &[u64]) {}
}

impl Service {
	fn client(&self, at: usize) -> watchdog::Client<ChannelTransport> {
		watchdog::Client::with_deadline(ChannelTransport { chan: self.timers[at].chan }, clock() + PROVIDER_TICKS)
	}

	fn name(&self, at: usize) -> &[u8] {
		self.timers[at].info.name.as_bytes()
	}

	// ------------------------------------------------------------------ the providers

	// A PUBLICATION: taken, or refused by the rules - a fifth, or a second under a name already held.
	fn adopt(&mut self, catalogue: u64, info: ProviderInfo) {
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) else {
			say(&[b"the published watchdog ", info.name.as_bytes(), b" could not be opened"]);
			return;
		};
		let description = match watchdog::Client::with_deadline(ChannelTransport { chan }, clock() + PROVIDER_TICKS).describe() {
			Some(Ok(description)) => description,
			_ => {
				say(&[b"the published watchdog ", info.name.as_bytes(), b" did not describe itself"]);
				close(chan);
				return;
			}
		};
		match self.choice.offer(&info.name, description.can_disarm) {
			wl::Offer::Take => {}
			wl::Offer::TooMany => {
				say(&[b"a fifth watchdog, ", info.name.as_bytes(), b", is not taken"]);
				close(chan);
				return;
			}
			wl::Offer::SameName => {
				say(&[b"a second provider named ", info.name.as_bytes(), b" is one timer already held - left unopened"]);
				close(chan);
				return;
			}
		}
		let running: &[u8] = if description.running_at_bind { b", running at bind" } else { b"" };
		let last: &[u8] = if description.last_reset_was_watchdog { b", the last reset was its own" } else { b"" };
		say(&[b"took ", info.name.as_bytes(), running, last]);
		self.timers.push(Timer { info, chan, description });
		// WITH THE POLICY OFF, a timer is disarmed as it attaches where the device allows it - not at the first answer.
		let at = self.timers.len() - 1;
		// A refused disarm changes nothing: the timer stays its driver's to bridge, and the first answer settles it.
		if !self.policy.enabled && self.timers[at].description.can_disarm && matches!(self.client(at).disarm(), Some(Ok(()))) {
			self.choice.disarm_answered(at, true);
			say(&[b"disarmed ", self.name(at), b" - the policy is off"]);
		}
		self.settle();
	}

	// A PUBLICATION WITHDRAWN: its driver ended or rebinds. Nothing is written to the timer - the new instance takes
	// it over at bind and is taken again here.
	fn withdraw(&mut self, info: &ProviderInfo) {
		let Some(at) = self.timers.iter().position(|timer| timer.info.slot == info.slot && timer.info.provider_generation == info.provider_generation && timer.info.binding_generation == info.binding_generation) else { return };
		let timer = self.timers.remove(at);
		self.choice.withdraw(&timer.info.name);
		close(timer.chan);
		say(&[b"lost ", timer.info.name.as_bytes()]);
	}

	// Arm at the configured timeout and keep feeding: a timer that cannot be stopped.
	fn feed(&mut self, at: usize) {
		let timeout = self.policy.timeout_ms;
		match self.client(at).arm(&timeout) {
			Some(Ok(effective)) => say(&[b"feeding ", self.name(at), b" at ", decimal(effective as u64).as_bytes(), b" ms - it cannot be stopped"]),
			_ => say(&[b"feeding ", self.name(at), b" - it cannot be stopped, and it refused the configured timeout"]),
		}
		let _ = self.client(at).pet();
		self.choice.feeding(at);
	}

	// THE POLICY'S NEXT STEPS, until it asks for none.
	fn settle(&mut self) {
		if self.shutting_down {
			return;
		}
		while let Some(step) = self.choice.next(&self.policy) {
			match step {
				wl::Step::Arm(at) => {
					let timeout = self.policy.timeout_ms;
					match self.client(at).arm(&timeout) {
						Some(Ok(effective)) => {
							self.choice.arm_answered(at, true);
							say(&[b"armed ", self.name(at), b" at ", decimal(effective as u64).as_bytes(), b" ms"]);
						}
						Some(Err(Error::Unsupported)) => {
							self.choice.arm_answered(at, false);
							say(&[self.name(at), b" cannot reset this machine - the next is tried"]);
						}
						_ => {
							self.choice.arm_answered(at, false);
							say(&[self.name(at), b" refused the arm"]);
						}
					}
				}
				wl::Step::Disarm(at) => {
					let disarmed = matches!(self.client(at).disarm(), Some(Ok(())));
					self.choice.disarm_answered(at, disarmed);
					if disarmed {
						say(&[b"disarmed ", self.name(at)]);
					} else {
						self.feed(at);
					}
				}
				wl::Step::Feed(at) => self.feed(at),
			}
		}
		if self.choice.named_unavailable(&self.policy) && !self.named_reported {
			self.named_reported = true;
			let named = self.policy.device.clone().unwrap_or_default();
			say(&[b"the policy names ", named.as_bytes(), b", which is absent or refused - nothing is armed in its place"]);
		}
	}

	// ------------------------------------------------------------------ liveness

	fn ask(&mut self, sequence: u64) {
		let mut capture = Capture { bytes: Vec::new() };
		let _ = supervisor_liveness::Client::new(&mut capture).alive(&sequence);
		if capture.bytes.len() < 6 {
			return;
		}
		let corr = self.next_corr;
		self.next_corr = self.next_corr.wrapping_add(1);
		capture.bytes[2..6].copy_from_slice(&corr.to_le_bytes());
		let _ = try_send(self.liveness, &capture.bytes, 0);
	}

	// An answer: a pet for every timer the schedule feeds, if it came within the deadline - and the first one starts
	// the choice.
	fn answered(&mut self, reply: &[u8]) {
		let mut reader = Reader::new(reply);
		let (Some(_corr), Some(true), Some(sequence)) = (reader.u32(), reader.tag(), reader.u64()) else { return };
		if self.shutting_down || self.sleeping || !self.schedule.answered(sequence, now_ms()) {
			return;
		}
		if !self.started {
			self.started = true;
			self.choice.start();
			say(&[b"ServiceManager answered - the watchdog policy applies from now"]);
			self.settle();
		}
		let petted: Vec<usize> = self.choice.petted().collect();
		for at in petted {
			let _ = self.client(at).pet();
		}
	}

	// ------------------------------------------------------------------ the orderly shutdown

	fn prepare(&mut self, action: ShutdownAction) {
		self.shutting_down = true;
		let running: Vec<usize> = self.choice.petted().collect();
		for at in running {
			let survives = self.timers[at].description.survives_reset;
			let can_disarm = self.timers[at].description.can_disarm;
			let (disarm, timeout) = match (survives, action) {
				// Reset with the platform: stopped where it allows it, else its longest timeout and a last pet - past the
				// shutdown's own bound.
				(false, _) => (can_disarm, self.timers[at].description.max_timeout_ms),
				// Surviving the reset, at power-off: stopped.
				(true, ShutdownAction::PowerOff) => (true, self.timers[at].description.max_timeout_ms),
				// Surviving the reset, at reboot: the boot bound, so a boot that hangs is caught and the next boot's
				// service takes the timer over.
				(true, ShutdownAction::Reboot) => (false, self.boot_bound_ms),
			};
			if disarm && matches!(self.client(at).disarm(), Some(Ok(()))) {
				say(&[b"disarmed ", self.name(at), b" for the shutdown"]);
				continue;
			}
			let _ = self.client(at).arm(&timeout);
			let _ = self.client(at).pet();
			say(&[b"gave ", self.name(at), b" ", decimal(timeout as u64).as_bytes(), b" ms and a last pet for the shutdown"]);
		}
	}
}

impl Service {
	// ------------------------------------------------------------------ the sleep

	fn announce(&mut self) {
		self.sleeping = true;
		let running: Vec<usize> = self.choice.petted().collect();
		for at in running {
			let longest = self.timers[at].description.max_timeout_ms;
			match self.client(at).arm(&longest) {
				Some(Ok(effective)) => say(&[b"gave ", self.name(at), b" ", decimal(effective as u64).as_bytes(), b" ms and a last pet for the sleep"]),
				_ => say(&[self.name(at), b" refused its longest timeout for the sleep - petted as it is"]),
			}
			let _ = self.client(at).pet();
		}
	}

	fn resumed(&mut self) {
		if !self.sleeping {
			return;
		}
		self.sleeping = false;
		let timeout = self.policy.timeout_ms;
		let running: Vec<usize> = self.choice.petted().collect();
		for at in running {
			match self.client(at).arm(&timeout) {
				Some(Ok(effective)) => say(&[b"restored ", self.name(at), b" to ", decimal(effective as u64).as_bytes(), b" ms after the sleep"]),
				_ => say(&[self.name(at), b" refused its configured timeout after the sleep"]),
			}
			let _ = self.client(at).pet();
		}
		// THE QUESTIONS START AGAIN, one period from now: the ones asked before the sleep are not answers to wait for.
		self.schedule = wl::Schedule::new(&self.policy, now_ms());
	}
}

struct Notice<'a> {
	service: &'a mut Service,
}

impl sleep_notice::Service for Notice<'_> {
	fn announce(&mut self, _state: SleepState) -> Result<(), Error> {
		self.service.announce();
		Ok(())
	}

	fn hold_writes(&mut self) -> Result<(), Error> {
		Ok(())
	}

	fn release_writes(&mut self) -> Result<(), Error> {
		Ok(())
	}

	fn resumed(&mut self, _state: SleepState) -> Result<(), Error> {
		self.service.resumed();
		Ok(())
	}
}

impl shutdown_notice::Service for Notice<'_> {
	fn prepare(&mut self, action: ShutdownAction) -> Result<(), Error> {
		self.service.prepare(action);
		Ok(())
	}
}

// ------------------------------------------------------------------ the policy

fn read_policy(config_client: u64, mode: wl::BootMode) -> (wl::Policy, u32) {
	let mut client = config::Client::new(ChannelTransport { chan: config_client });
	let mut get = |key: &str| -> Option<String> {
		if config_client == 0 {
			return None;
		}
		match client.get(key) {
			Some(Ok(value)) => Some(value),
			_ => None,
		}
	};
	let enabled = get("watchdog.enabled").and_then(|value| match value.as_str() {
		"on" | "true" | "1" => Some(true),
		"off" | "false" | "0" => Some(false),
		_ => None,
	});
	let device = get("watchdog.device");
	let number = |value: Option<String>| value.and_then(|value| value.parse::<u32>().ok());
	let timeout = number(get("watchdog.timeout-ms"));
	let period = number(get("watchdog.period-ms"));
	let deadline = number(get("watchdog.deadline-ms"));
	let boot_bound = number(get("watchdog.boot-bound-ms")).filter(|ms| *ms > 0).unwrap_or(DEFAULT_BOOT_BOUND_MS);
	let policy = match wl::Policy::from_keys(mode, enabled, device.as_deref(), timeout, period, deadline) {
		Ok(policy) => policy,
		Err(refusal) => {
			// A POLICY THAT WOULD RESET A LIVE MACHINE - one late round, or overlapping questions - is refused as
			// written, and the proposed numbers stand; whether to arm is still the key's.
			let why: &[u8] = match refusal {
				wl::PolicyError::Zero => b"a number of zero",
				wl::PolicyError::TimeoutUnderThreePeriods => b"the timeout is under three periods",
				wl::PolicyError::DeadlineNotUnderPeriod => b"the deadline is not under the period",
			};
			say(&[b"the configured timing is refused (", why, b") - the defaults stand"]);
			wl::Policy::from_keys(mode, enabled, device.as_deref(), None, None, None).unwrap_or(wl::Policy { enabled: false, device: None, timeout_ms: wl::DEFAULT_TIMEOUT_MS, period_ms: wl::DEFAULT_PERIOD_MS, deadline_ms: wl::DEFAULT_DEADLINE_MS })
		}
	};
	(policy, boot_bound)
}

// THE MODE, read by hand: the plan's reader drops a payload's bytes, and here the bytes are the point.
fn receive_mode(bootstrap: u64) -> wl::BootMode {
	let tag = BOOTSTRAP_ROLES[PLANNED_ROLES].tag;
	let mut buf = [0u8; 64];
	let (len, handle) = match recv_blocking(bootstrap, &mut buf) {
		Received::Message { len, handle } => (len, handle),
		Received::Closed => exit(),
	};
	if handle != 0 {
		close(handle);
	}
	if len < tag.len() || &buf[..tag.len()] != tag {
		fail_bootstrap(bootstrap, tag, b"the mode payload did not arrive where the plan puts it");
	}
	match wl::BootMode::from_payload(&buf[tag.len()..len]) {
		Some(mode) => mode,
		None => fail_bootstrap(bootstrap, tag, b"a mode payload with no mode - the policy's default cannot be chosen"),
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	// The roles: a catalogue connection minted for `watchdog` alone, a config client, the liveness channel, and the
	// mode's bytes - the last two ServiceManager's own to fill.
	let mut roles: [u64; PLANNED_ROLES] = [0; PLANNED_ROLES];
	if let Err(error) = receive_roles(bootstrap, &BOOTSTRAP_ROLES[..PLANNED_ROLES], &mut roles) {
		fail_bootstrap(bootstrap, error.tag(), error.reason());
	}
	let (catalogue, config_client, liveness) = (roles[0], roles[1], roles[2]);
	let mode = receive_mode(bootstrap);
	let (policy, boot_bound_ms) = read_policy(config_client, mode);
	if config_client != 0 {
		close(config_client);
	}
	let subscription: u64 = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::Watchdog).unwrap_or(0) } else { 0 };
	let mut service = Service { schedule: wl::Schedule::new(&policy, now_ms()), policy, boot_bound_ms, choice: wl::Choice::default(), timers: Vec::new(), liveness, next_corr: 1, started: false, shutting_down: false, sleeping: false, named_reported: false };
	let mut report: Vec<u8> = Vec::from(&b"WatchdogService: online - "[..]);
	report.extend_from_slice(if service.policy.enabled { b"armed by policy" } else { b"the policy is off" });
	report.extend_from_slice(match mode {
		wl::BootMode::Shipping => b" (shipping)",
		wl::BootMode::Development => b" (development)",
		wl::BootMode::Test => b" (test)",
	});
	// ON THE CONSOLE AS WELL: a volume-staged service's report reaches ServiceManager and no further, and which policy
	// an instance read is what an operator - and the watchdog gate - asks first.
	print(&report);
	print(b"\n");
	send_blocking(bootstrap, &report, 0);

	let mut buf = alloc::vec![0u8; 1024];
	let mut reply_buf = alloc::vec![0u8; 256];
	let mut control = bootstrap;
	let mut subscribed = subscription != 0;
	loop {
		let deadline_ms = match service.shutting_down || service.sleeping {
			true => 0,
			false => match service.schedule.tick(now_ms()) {
				wl::Tick::Ask(sequence) => {
					service.ask(sequence);
					match service.schedule.tick(now_ms()) {
						wl::Tick::Wait(at) => at,
						wl::Tick::Ask(_) => now_ms(),
					}
				}
				wl::Tick::Wait(at) => at,
			},
		};
		let mut waitset: Vec<u64> = Vec::new();
		if control != 0 {
			waitset.push(control);
		}
		if service.liveness != 0 {
			waitset.push(service.liveness);
		}
		if subscribed {
			waitset.push(subscription);
		}
		if waitset.is_empty() {
			exit();
		}
		let deadline = if deadline_ms == 0 { 0 } else { ticks_at(deadline_ms).max(1) };
		let ready = wait_any(&waitset, deadline);
		if ready < 0 {
			continue;
		}
		let handle = waitset[ready as usize];
		if handle == service.liveness {
			loop {
				match try_recv_caps(service.liveness, &mut buf) {
					PolledCaps::Message { len, handles } => {
						for &leftover in handles.as_slice() {
							close(leftover);
						}
						service.answered(&buf[..len]);
					}
					PolledCaps::Empty => break,
					PolledCaps::Closed => {
						// SERVICEMANAGER DROPPED THE CHANNEL: nothing answers any more, so nothing is petted - the
						// hardware decides.
						close(service.liveness);
						service.liveness = 0;
						say(&[b"the liveness channel closed - nothing is petted from now"]);
						break;
					}
				}
			}
			continue;
		}
		if subscribed && handle == subscription {
			loop {
				let (len, handles) = match try_recv_caps(subscription, &mut buf) {
					PolledCaps::Message { len, handles } => (len, handles),
					PolledCaps::Empty => break,
					PolledCaps::Closed => {
						subscribed = false;
						break;
					}
				};
				for &leftover in handles.as_slice() {
					close(leftover);
				}
				let mut frame_handles = Handles::new();
				let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else { continue };
				if info.live {
					service.adopt(catalogue, info);
				} else {
					service.withdraw(&info);
				}
			}
			continue;
		}
		// THE CONTROL CHANNEL: ServiceManager's notices.
		let (len, mut handles) = match try_recv_caps(control, &mut buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => continue,
			PolledCaps::Closed => {
				control = 0;
				continue;
			}
		};
		let mut reply_handles = Handles::new();
		let op = if len >= 2 { u16::from_le_bytes([buf[0], buf[1]]) } else { 0 };
		let written = if matches!(op, sleep_notice::OP_ANNOUNCE | sleep_notice::OP_HOLD_WRITES | sleep_notice::OP_RELEASE_WRITES | sleep_notice::OP_RESUMED) { sleep_notice::dispatch(&mut Notice { service: &mut service }, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) } else { shutdown_notice::dispatch(&mut Notice { service: &mut service }, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) };
		for &leftover in handles.as_slice().iter().chain(reply_handles.as_slice()) {
			close(leftover);
		}
		if let Some(written) = written {
			let _ = try_send(control, &reply_buf[..written], 0);
		}
	}
}
