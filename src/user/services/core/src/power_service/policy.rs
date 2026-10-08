// THE SLEEP POLICY'S INPUTS AND ITS DOORS - `service_logic::sleep_policy` decides; this reads what it is told and carries
// out what it decides.
//
// WHAT IT READS: its settings, from ConfigService's tree once at start (`sleep_policy::KEYS`); every `platform-switch`
// publication - the lid's, and the power and sleep buttons', fixed and control-method alike - through the same catalogue
// connection the power sources arrive on, each one's kind told by its first frame; InputService's activity signal,
// watched at the policy's idle timeout; DisplayService's outputs, asked at every closing of the lid; and the power
// sources this service already holds - on battery when line power is known to be absent, critical when a battery says
// so.
//
// WHAT IT ASKS: the screen off and on again through DisplayService's `display-outputs`; a suspend through
// `system-sleep` - to RAM where the machine offers it, to idle otherwise - answered at acceptance; for a critical battery
// nothing by default, said on the console, and where the setting asks for it the orderly power-off - the kernel's
// forced deadline first, through `system-power`'s `power-off-within`, then ServiceManager's sequence through
// `system-shutdown` - or a hibernation, with the orderly power-off whenever that is refused, as it is wherever
// hibernation is not set up; for a press of the power button the same orderly power-off by default, and of the sleep
// button a suspend. Never an immediate `system-power` power-off: the services are stopped in order.

use super::*;
use alloc::string::String;
use proto::system::{ActivityEdge, DisplayOutput, SleepReason, SleepState, SwitchKind, config, display_outputs, input_activity, platform_switch, system_power, system_shutdown, system_sleep};
use service_logic::sleep_policy::{Action, Button, ButtonAction, CriticalAction, Event, KEYS, LidAction, Policy, Settings, Why};

// How long one question to ServiceManager, SystemManager, DisplayService or a provider may take.
pub(super) const ASK_TICKS: u64 = TICKS_PER_SECOND * 2;
// THE FORCED POWER-OFF'S BOUND, armed before the orderly sequence is asked for: ten seconds, as the plan states it.
const FORCED_BOUND_SECONDS: u32 = 10;

// ONE `platform-switch` PUBLICATION FOLLOWED: a lid or a button, its kind known from its first frame, and a button's
// last sequence, so a frame read again is no second press.
struct Switch {
	info: ProviderInfo,
	chan: u64,
	stream: u64,
	kind: Option<SwitchKind>,
	sequence: u32,
}

#[derive(Clone, Copy)]
enum Request {
	Outputs,
	Screen,
	Suspend,
	Hibernate(Why),
	ArmPowerOff,
	PowerOff,
}

struct Pending {
	kind: Request,
	chan: u64,
	corr: u32,
	deadline: u64,
}

// Generated request encoding, with the same nonblocking sending used by the provider controls.
struct Capture(Vec<u8>);

impl wire::Transport for Capture {
	fn call(&mut self, request: &[u8], handles: &[u64], _reply_handles: &mut wire::Handles, _deadline: u64) -> Result<Vec<u8>, wire::TransportError> {
		if handles.is_empty() {
			self.0 = request.to_vec();
		}
		Err(wire::TransportError::TimedOut)
	}
	fn discard_handles(&mut self, handles: &[u64]) {
		for &handle in handles {
			close(handle);
		}
	}
}

impl wire::Transport for &mut Capture {
	fn call(&mut self, request: &[u8], handles: &[u64], replies: &mut wire::Handles, deadline: u64) -> Result<Vec<u8>, wire::TransportError> {
		(**self).call(request, handles, replies, deadline)
	}
	fn discard_handles(&mut self, handles: &[u64]) {
		(**self).discard_handles(handles);
	}
}

fn named(kind: Option<SwitchKind>) -> &'static str {
	match kind {
		Some(SwitchKind::Lid) => "the lid",
		Some(SwitchKind::PowerButton) => "the power button",
		Some(SwitchKind::SleepButton) => "the sleep button",
		None => "a switch",
	}
}

fn reason(why: Why) -> SleepReason {
	match why {
		Why::Lid => SleepReason::Lid,
		Why::Idle => SleepReason::Idle,
		Why::Critical => SleepReason::Critical,
		Why::PowerButton => SleepReason::PowerButton,
		Why::SleepButton => SleepReason::SleepButton,
	}
}

fn does(action: ButtonAction) -> &'static str {
	match action {
		ButtonAction::PowerOff => "powers off in order",
		ButtonAction::Suspend => "suspends",
		ButtonAction::Hibernate => "hibernates",
		ButtonAction::Nothing => "does nothing",
	}
}

pub(super) struct SleepPolicy {
	policy: Policy,
	switches: u64,
	followed: Vec<Switch>,
	activity_stream: u64,
	outputs: u64,
	sleep: u64,
	syspower: u64,
	shutdown: u64,
	// What the power sources said last, so an unchanged reading tells the policy nothing twice.
	facts: Option<(bool, bool)>,
	// At most one policy action is in flight. Its input streams remain queued while it is pending,
	// but provider discovery, power clients and their deadlines continue in the main service loop.
	pending: Option<Pending>,
	next_corr: u32,
}

fn say(text: &str) {
	let line = alloc::format!("PowerService: sleep policy: {text}\n");
	print(line.as_bytes());
}

// THE SETTINGS, from ConfigService's tree - a key absent or a tree that does not answer keeps its default, and a value
// refused is named. The client is closed once read: the settings are this instance's, and a relaunch reads them again.
fn read_settings(config_client: u64) -> Settings {
	if config_client == 0 {
		say("no configuration tree - the default settings stand");
		return Settings::default();
	}
	let mut client = config::Client::with_deadline(ChannelTransport { chan: config_client }, clock() + ASK_TICKS);
	let mut values: [Option<String>; KEYS.len()] = Default::default();
	for (value, key) in values.iter_mut().zip(KEYS) {
		*value = match client.get(key) {
			Some(Ok(text)) => Some(text),
			_ => None,
		};
	}
	close(config_client);
	let (settings, refused) = Settings::from_keys(values.each_ref().map(|value| value.as_deref()));
	for key in refused {
		say(&alloc::format!("the value of {key} is not one it takes - its default stands"));
	}
	let lid = match settings.lid {
		LidAction::ScreenOff => "turns the screen off",
		LidAction::Suspend => "suspends",
		LidAction::Nothing => "does nothing",
	};
	let idle = if settings.idle_suspends_on_battery { "suspends on battery" } else { "suspends nothing" };
	let critical = match settings.critical {
		CriticalAction::PowerOff => "powers off in order",
		CriticalAction::Hibernate => "hibernates",
		CriticalAction::Nothing => "does nothing",
	};
	say(&alloc::format!("closing the lid {lid}, idleness after {} s {idle}, a critical battery {critical}, the power button {}, the sleep button {}", settings.idle_after_seconds, does(settings.power_button), does(settings.sleep_button)));
	settings
}

impl SleepPolicy {
	pub(super) fn new(catalogue: u64, activity: u64, outputs: u64, sleep: u64, syspower: u64, shutdown: u64, config_client: u64) -> SleepPolicy {
		let settings = read_settings(config_client);
		let switches = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::PlatformSwitch).unwrap_or(0) } else { 0 };
		let activity_stream = if activity != 0 { input_activity::Client::with_deadline(ChannelTransport { chan: activity }, clock() + ASK_TICKS).watch(&settings.idle_after_seconds.saturating_mul(1000)).unwrap_or(0) } else { 0 };
		if activity_stream == 0 {
			say("no activity signal - the idle timeout is off");
		}
		if sleep == 0 {
			say("no system-sleep client - a closed lid, idleness and the buttons suspend nothing");
		}
		if outputs == 0 {
			say("no display-outputs client - a closed lid turns no screen off");
		}
		SleepPolicy { policy: Policy::new(settings), switches, followed: Vec::new(), activity_stream, outputs, sleep, syspower, shutdown, facts: None, pending: None, next_corr: 1 }
	}

	// The channels the loop waits on for the policy.
	pub(super) fn handles(&self) -> Vec<u64> {
		let mut out = Vec::new();
		for handle in [self.switches, self.outputs, self.sleep, self.syspower, self.shutdown] {
			if handle != 0 {
				out.push(handle);
			}
		}
		if self.pending.is_none() {
			if self.activity_stream != 0 {
				out.push(self.activity_stream);
			}
			out.extend(self.followed.iter().map(|switch| switch.stream));
		}
		out
	}

	// ONE READY HANDLE, if it is the policy's. Answers whether it was.
	pub(super) fn serve(&mut self, handle: u64, buf: &mut [u8]) -> bool {
		if [self.outputs, self.sleep, self.syspower, self.shutdown].contains(&handle) {
			self.reply(handle, buf);
			return true;
		}
		if let Some(at) = self.followed.iter().position(|switch| switch.stream == handle) {
			self.drain_switch(at, buf);
			return true;
		}
		if handle == self.activity_stream {
			self.drain_activity(buf);
			return true;
		}
		false
	}

	pub(super) fn switch_subscription(&self) -> u64 {
		self.switches
	}

	// One announcement per free opening slot. The main loop owns all opens on the shared catalogue;
	// it leaves this stream unread when its bounded set of handshakes is full.
	pub(super) fn drain_switches(&mut self, buf: &mut [u8]) -> Option<ProviderInfo> {
		let (len, handles) = match try_recv_caps(self.switches, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return None,
			PolledCaps::Closed => {
				close(self.switches);
				self.switches = 0;
				return None;
			}
		};
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		provider_catalogue::subscribe_read(&buf[..len], &mut wire::Handles::new())
	}

	pub(super) fn follows(&self, info: &ProviderInfo) -> bool {
		self.followed.iter().any(|switch| publication_of(&switch.info) == publication_of(info))
	}

	// A SWITCH ARRIVED: its watch opened, its state now read as the first frame - which names what it is.
	pub(super) fn opened(&mut self, info: ProviderInfo, chan: u64, stream: u64) {
		self.followed.push(Switch { info, chan, stream, kind: None, sequence: 0 });
	}

	pub(super) fn withdraw_switch(&mut self, info: &ProviderInfo) {
		if let Some(at) = self.followed.iter().position(|switch| publication_of(&switch.info) == publication_of(info)) {
			self.drop_switch(at, "its publication was withdrawn");
		}
	}

	fn drop_switch(&mut self, at: usize, why: &str) {
		let switch = self.followed.swap_remove(at);
		close(switch.stream);
		close(switch.chan);
		say(&alloc::format!("no longer follows {} - {why}", named(switch.kind)));
	}

	fn drain_switch(&mut self, at: usize, buf: &mut [u8]) {
		let stream = self.followed[at].stream;
		loop {
			match try_recv_caps(stream, buf) {
				PolledCaps::Message { len, handles } => {
					let mut frame = handles;
					let state = platform_switch::watch_read(&buf[..len], &mut frame);
					for &leftover in frame.as_slice() {
						close(leftover);
					}
					let Some(state) = state else { continue };
					let switch = &mut self.followed[at];
					let first = switch.kind.is_none();
					let again = !first && switch.sequence == state.sequence;
					switch.kind = Some(state.kind);
					switch.sequence = state.sequence;
					if first {
						say(&alloc::format!("follows {}", named(Some(state.kind))));
					}
					match state.kind {
						SwitchKind::Lid => {
							say(if state.closed { "the lid is closed" } else { "the lid is open" });
							// AN EXTERNAL DISPLAY IN USE keeps the machine awake: asked at every closing, since one may
							// have been plugged in since the last.
							if state.closed {
								self.ask(Request::Outputs, self.outputs, |capture| {
									let _ = display_outputs::Client::new(capture).outputs();
								});
							} else {
								self.feed(Event::Lid(false));
							}
						}
						// A BUTTON'S PRESS IS A FRAME `closed` WITH ITS SEQUENCE ADVANCED; its state otherwise is open.
						SwitchKind::PowerButton | SwitchKind::SleepButton if state.closed && !again => {
							let button = if state.kind == SwitchKind::PowerButton { Button::Power } else { Button::Sleep };
							say(&alloc::format!("{} was pressed - it {}", named(Some(state.kind)), does(self.policy.button(button))));
							self.feed(Event::Pressed(button));
						}
						SwitchKind::PowerButton | SwitchKind::SleepButton => {}
					}
					if self.pending.is_some() {
						return;
					}
				}
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					self.drop_switch(at, "its driver ended");
					return;
				}
			}
		}
	}

	fn drain_activity(&mut self, buf: &mut [u8]) {
		loop {
			match try_recv_caps(self.activity_stream, buf) {
				PolledCaps::Message { len, handles } => {
					let mut frame = handles;
					let edge = input_activity::watch_read(&buf[..len], &mut frame);
					for &leftover in frame.as_slice() {
						close(leftover);
					}
					let Some(edge) = edge else { continue };
					self.feed(match edge {
						ActivityEdge::Idle => Event::Idle,
						ActivityEdge::Active => Event::Active,
					});
					if self.pending.is_some() {
						return;
					}
				}
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					close(self.activity_stream);
					self.activity_stream = 0;
					say("the activity signal ended - the idle timeout is off until this service starts again");
					return;
				}
			}
		}
	}

	// THE POWER SOURCES, READ AGAIN after every change this service took in.
	pub(super) fn power_changed(&mut self, registry: &Registry<Held>) {
		if self.pending.is_some() {
			return;
		}
		let supply = power_model::canon::supply(registry.sources().iter().map(|(_, _, held)| &held.0));
		let facts = (supply.on_battery, supply.critical);
		if self.facts == Some(facts) {
			return;
		}
		self.facts = Some(facts);
		let (on_battery, critical) = facts;
		self.feed(Event::Power { on_battery, critical });
	}

	fn feed(&mut self, event: Event) {
		let critical_edge = matches!(event, Event::Power { critical: true, .. }) && !self.policy.critical_told();
		let action = self.policy.event(event);
		if critical_edge {
			match self.policy.settings.critical {
				CriticalAction::PowerOff => say("a battery is critical - powers the machine off in order"),
				CriticalAction::Nothing => say("a battery is critical - the policy does nothing, as it is set to"),
				CriticalAction::Hibernate => {}
			}
		}
		self.act(action);
	}

	fn act(&mut self, action: Action) {
		match action {
			Action::Nothing => {}
			Action::ScreenOff | Action::ScreenOn => {
				let on = action == Action::ScreenOn;
				say(if on { "turns the screen on" } else { "turns the screen off" });
				if self.outputs == 0 {
					return;
				}
				self.ask(Request::Screen, self.outputs, |capture| {
					let _ = display_outputs::Client::new(capture).set_power(&on);
				});
			}
			Action::Suspend(why) => {
				let reason = reason(why);
				let state = if sleep_states() & (1 << SLEEP_STATE_RAM) != 0 { SleepState::Ram } else { SleepState::Idle };
				say(&alloc::format!("asks for a suspend ({reason:?})"));
				if self.sleep == 0 {
					return;
				}
				self.ask(Request::Suspend, self.sleep, |capture| {
					let _ = system_sleep::Client::new(capture).suspend(&state, &0, &reason);
				});
			}
			Action::Hibernate(why) => {
				if why == Why::Critical {
					say("a battery is critical - asks for hibernation");
				} else {
					say(&alloc::format!("asks for hibernation ({:?})", reason(why)));
				}
				self.ask(Request::Hibernate(why), self.sleep, |capture| {
					let _ = system_sleep::Client::new(capture).hibernate(&false, &reason(why));
				});
			}
			Action::PowerOff => {
				// THE FORCED DEADLINE FIRST, so the machine is off within its bound whatever the sequence meets.
				self.ask(Request::ArmPowerOff, self.syspower, |capture| {
					let _ = system_power::Client::new(capture).power_off_within(&FORCED_BOUND_SECONDS);
				});
			}
		}
	}

	fn ask(&mut self, kind: Request, chan: u64, encode: impl FnOnce(&mut Capture)) {
		let corr = self.next_corr;
		if corr != 0 {
			self.next_corr = corr.checked_add(1).unwrap_or(0);
		}
		let mut capture = Capture(Vec::new());
		encode(&mut capture);
		if corr != 0 && chan != 0 && capture.0.len() >= 6 {
			capture.0[2..6].copy_from_slice(&corr.to_le_bytes());
			if try_send(chan, &capture.0, 0) {
				self.pending = Some(Pending { kind, chan, corr, deadline: clock() + ASK_TICKS });
				return;
			}
		}
		self.completed(kind, None);
	}

	pub(super) fn next_deadline(&self) -> Option<u64> {
		self.pending.as_ref().map(|pending| pending.deadline)
	}

	pub(super) fn tick(&mut self) {
		if self.pending.as_ref().is_some_and(|pending| clock() >= pending.deadline) {
			let pending = self.pending.take().unwrap();
			self.completed(pending.kind, None);
		}
	}

	fn reply(&mut self, handle: u64, buf: &mut [u8]) {
		let (len, handles) = match try_recv_caps(handle, buf) {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return,
			PolledCaps::Closed => {
				for held in [&mut self.outputs, &mut self.sleep, &mut self.syspower, &mut self.shutdown] {
					if *held == handle {
						*held = 0;
					}
				}
				close(handle);
				if self.pending.as_ref().is_some_and(|pending| pending.chan == handle) {
					let pending = self.pending.take().unwrap();
					self.completed(pending.kind, None);
				}
				return;
			}
		};
		// These replies carry no capabilities. Dispose of unexpected or stale ones even if no action
		// is pending, so a timed-out response cannot leak a handle or settle a later action.
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		let mut reader = wire::Reader::new(&buf[..len]);
		let corr = reader.u32();
		let Some(pending) = self.pending.as_ref() else { return };
		if pending.chan != handle || corr != Some(pending.corr) {
			return;
		}
		let pending = self.pending.take().unwrap();
		let answer = if !handles.as_slice().is_empty() || clock() >= pending.deadline {
			None
		} else {
			(|| {
				let result = if reader.tag()? {
					let mut external = false;
					if matches!(pending.kind, Request::Outputs) {
						for _ in 0..reader.u16()? {
							let output = DisplayOutput::read(&mut reader)?;
							external |= output.active && output.external;
						}
					}
					Ok(external)
				} else {
					Err(Error::read(&mut reader)?)
				};
				reader.finish()?;
				Some(result)
			})()
		};
		self.completed(pending.kind, answer);
	}

	fn completed(&mut self, kind: Request, answer: Option<Result<bool, Error>>) {
		match kind {
			Request::Outputs => {
				self.feed(Event::ExternalDisplay(matches!(answer, Some(Ok(true)))));
				self.feed(Event::Lid(true));
			}
			Request::Screen => match answer {
				Some(Ok(_)) => {}
				Some(Err(error)) => say(&alloc::format!("DisplayService refused the screen's state - {error:?}")),
				None => say("DisplayService did not answer the screen's state"),
			},
			Request::Suspend => match answer {
				Some(Ok(_)) => {}
				Some(Err(error)) => say(&alloc::format!("the suspend was refused - {error:?}")),
				None => say("ServiceManager did not answer the suspend"),
			},
			Request::Hibernate(why) if !matches!(answer, Some(Ok(_))) => {
				let next = self.policy.hibernation_refused(why);
				say(if next == Action::PowerOff { "hibernation was refused - the orderly power-off instead" } else { "hibernation was refused - nothing more is done" });
				self.act(next);
			}
			Request::Hibernate(_) => {}
			Request::ArmPowerOff => {
				if matches!(answer, Some(Ok(_))) {
					say(&alloc::format!("the machine is off within {FORCED_BOUND_SECONDS} s - asking for the orderly power-off"));
				} else {
					say("the forced power-off deadline could not be armed");
				}
				self.ask(Request::PowerOff, self.shutdown, |capture| {
					let _ = system_shutdown::Client::new(capture).power_off();
				});
			}
			Request::PowerOff if !matches!(answer, Some(Ok(_))) => say("the orderly power-off was not taken - the forced deadline stands"),
			Request::PowerOff => {}
		}
	}
}
