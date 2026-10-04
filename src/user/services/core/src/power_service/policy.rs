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
use proto::system::{ActivityEdge, SleepReason, SleepState, SwitchKind, config, display_outputs, input_activity, platform_switch, system_power, system_shutdown, system_sleep};
use service_logic::sleep_policy::{Action, Button, ButtonAction, CriticalAction, Event, KEYS, LidAction, Policy, Settings, Why};

// How long one question to ServiceManager, SystemManager, DisplayService or a provider may take.
const ASK_TICKS: u64 = TICKS_PER_SECOND * 2;
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
		SleepPolicy { policy: Policy::new(settings), switches, followed: Vec::new(), activity_stream, outputs, sleep, syspower, shutdown, facts: None }
	}

	// The channels the loop waits on for the policy.
	pub(super) fn handles(&self) -> Vec<u64> {
		let mut out = Vec::new();
		for handle in [self.switches, self.activity_stream].into_iter().chain(self.followed.iter().map(|switch| switch.stream)) {
			if handle != 0 {
				out.push(handle);
			}
		}
		out
	}

	// ONE READY HANDLE, if it is the policy's. Answers whether it was.
	pub(super) fn serve(&mut self, handle: u64, catalogue: u64, buf: &mut [u8]) -> bool {
		if handle == self.switches {
			self.drain_switches(catalogue, buf);
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

	fn drain_switches(&mut self, catalogue: u64, buf: &mut [u8]) {
		loop {
			let (len, handles) = match try_recv_caps(self.switches, buf) {
				PolledCaps::Message { len, handles } => (len, handles),
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					close(self.switches);
					self.switches = 0;
					return;
				}
			};
			for &leftover in handles.as_slice() {
				close(leftover);
			}
			let mut frame_handles = wire::Handles::new();
			let Some(info) = provider_catalogue::subscribe_read(&buf[..len], &mut frame_handles) else { continue };
			if info.live {
				self.adopt(catalogue, info);
			} else if let Some(at) = self.followed.iter().position(|switch| publication_of(&switch.info) == publication_of(&info)) {
				self.drop_switch(at, "its publication was withdrawn");
			}
		}
	}

	// A SWITCH ARRIVED: its watch opened, its state now read as the first frame - which names what it is.
	fn adopt(&mut self, catalogue: u64, info: ProviderInfo) {
		if self.followed.iter().any(|switch| publication_of(&switch.info) == publication_of(&info)) {
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) else {
			say("a switch's publication could not be opened");
			return;
		};
		let Some(stream) = platform_switch::Client::with_deadline(ChannelTransport { chan }, clock() + ASK_TICKS).watch() else {
			say("a switch's driver did not open its watch");
			close(chan);
			return;
		};
		self.followed.push(Switch { info, chan, stream, kind: None, sequence: 0 });
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
					let Some(state) = platform_switch::watch_read(&buf[..len], &mut frame) else { continue };
					for &leftover in frame.as_slice() {
						close(leftover);
					}
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
								let external = self.external_display();
								self.feed(Event::ExternalDisplay(external));
							}
							self.feed(Event::Lid(state.closed));
						}
						// A BUTTON'S PRESS IS A FRAME `closed` WITH ITS SEQUENCE ADVANCED; its state otherwise is open.
						SwitchKind::PowerButton | SwitchKind::SleepButton if state.closed && !again => {
							let button = if state.kind == SwitchKind::PowerButton { Button::Power } else { Button::Sleep };
							say(&alloc::format!("{} was pressed - it {}", named(Some(state.kind)), does(self.policy.button(button))));
							self.feed(Event::Pressed(button));
						}
						SwitchKind::PowerButton | SwitchKind::SleepButton => {}
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
					let Some(edge) = input_activity::watch_read(&buf[..len], &mut frame) else { continue };
					self.feed(match edge {
						ActivityEdge::Idle => Event::Idle,
						ActivityEdge::Active => Event::Active,
					});
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

	// WHETHER AN EXTERNAL DISPLAY IS IN USE, as DisplayService says; one that does not answer is taken as none.
	fn external_display(&self) -> bool {
		if self.outputs == 0 {
			return false;
		}
		match display_outputs::Client::with_deadline(ChannelTransport { chan: self.outputs }, clock() + ASK_TICKS).outputs() {
			Some(Ok(outputs)) => outputs.iter().any(|output| output.active && output.external),
			_ => false,
		}
	}

	// THE POWER SOURCES, READ AGAIN after every change this service took in.
	pub(super) fn power_changed(&mut self, registry: &Registry<Held>) {
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
				match display_outputs::Client::with_deadline(ChannelTransport { chan: self.outputs }, clock() + ASK_TICKS).set_power(&on) {
					Some(Ok(())) => {}
					Some(Err(error)) => say(&alloc::format!("DisplayService refused the screen's state - {error:?}")),
					None => say("DisplayService did not answer the screen's state"),
				}
			}
			Action::Suspend(why) => {
				let reason = reason(why);
				let state = if sleep_states() & (1 << SLEEP_STATE_RAM) != 0 { SleepState::Ram } else { SleepState::Idle };
				say(&alloc::format!("asks for a suspend ({reason:?})"));
				if self.sleep == 0 {
					return;
				}
				match system_sleep::Client::with_deadline(ChannelTransport { chan: self.sleep }, clock() + ASK_TICKS).suspend(&state, &0, &reason) {
					Some(Ok(())) => {}
					Some(Err(error)) => say(&alloc::format!("the suspend was refused - {error:?}")),
					None => say("ServiceManager did not answer the suspend"),
				}
			}
			Action::Hibernate(why) => {
				if why == Why::Critical {
					say("a battery is critical - asks for hibernation");
				} else {
					say(&alloc::format!("asks for hibernation ({:?})", reason(why)));
				}
				let refused = self.sleep == 0 || !matches!(system_sleep::Client::with_deadline(ChannelTransport { chan: self.sleep }, clock() + ASK_TICKS).hibernate(&false, &reason(why)), Some(Ok(())));
				if refused {
					let next = self.policy.hibernation_refused(why);
					say(if next == Action::PowerOff { "hibernation was refused - the orderly power-off instead" } else { "hibernation was refused - nothing more is done" });
					self.act(next);
				}
			}
			Action::PowerOff => {
				// THE FORCED DEADLINE FIRST, so the machine is off within its bound whatever the sequence meets.
				if self.syspower == 0 || !matches!(system_power::Client::with_deadline(ChannelTransport { chan: self.syspower }, clock() + ASK_TICKS).power_off_within(&FORCED_BOUND_SECONDS), Some(Ok(()))) {
					say("the forced power-off deadline could not be armed");
				} else {
					say(&alloc::format!("the machine is off within {FORCED_BOUND_SECONDS} s - asking for the orderly power-off"));
				}
				if self.shutdown == 0 || !matches!(system_shutdown::Client::with_deadline(ChannelTransport { chan: self.shutdown }, clock() + ASK_TICKS).power_off(), Some(Ok(()))) {
					say("the orderly power-off was not taken - the forced deadline stands");
				}
			}
		}
	}
}
