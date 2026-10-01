// THE SLEEP POLICY'S INPUTS AND ITS DOORS - `service_logic::sleep_policy` decides; this reads what it is told and carries
// out what it decides.
//
// WHAT IT READS: the lid's `platform-switch` publication, through the same catalogue connection the power sources arrive
// on; InputService's activity signal, watched at the policy's idle timeout; DisplayService's outputs, asked at every
// closing of the lid; and the power sources this service already holds - on battery when line power is known to be
// absent, critical when a battery says so.
//
// WHAT IT ASKS: a suspend through `system-sleep` - to RAM where the machine offers it, to idle otherwise - answered at
// acceptance; a hibernation for a critical battery, and when that is refused - as it is wherever hibernation is not set
// up - the orderly power-off: the kernel's forced deadline first, through `system-power`'s `power-off-within`, then
// ServiceManager's sequence through `system-shutdown`. Never an immediate `system-power` power-off: the services are
// stopped in order.

use super::*;
use proto::system::{ActivityEdge, SleepReason, SleepState, SwitchKind, display_outputs, input_activity, platform_switch, system_power, system_shutdown, system_sleep};
use service_logic::sleep_policy::{Action, Event, Policy, Settings, Why};

// How long one question to ServiceManager, SystemManager, DisplayService or a provider may take.
const ASK_TICKS: u64 = TICKS_PER_SECOND * 2;
// THE FORCED POWER-OFF'S BOUND, armed before the orderly sequence is asked for: ten seconds, as the plan states it.
const FORCED_BOUND_SECONDS: u32 = 10;

struct Lid {
	info: ProviderInfo,
	chan: u64,
	stream: u64,
}

pub(super) struct SleepPolicy {
	policy: Policy,
	switches: u64,
	lid: Option<Lid>,
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

impl SleepPolicy {
	pub(super) fn new(catalogue: u64, activity: u64, outputs: u64, sleep: u64, syspower: u64, shutdown: u64) -> SleepPolicy {
		let settings = Settings::default();
		let switches = if catalogue != 0 { provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).subscribe(&ProviderKind::PlatformSwitch).unwrap_or(0) } else { 0 };
		let activity_stream = if activity != 0 { input_activity::Client::with_deadline(ChannelTransport { chan: activity }, clock() + ASK_TICKS).watch(&settings.idle_after_seconds.saturating_mul(1000)).unwrap_or(0) } else { 0 };
		if activity_stream == 0 {
			say("no activity signal - the idle timeout is off");
		}
		if sleep == 0 {
			say("no system-sleep client - a closed lid and idleness suspend nothing");
		}
		SleepPolicy { policy: Policy::new(settings), switches, lid: None, activity_stream, outputs, sleep, syspower, shutdown, facts: None }
	}

	// The channels the loop waits on for the policy.
	pub(super) fn handles(&self) -> Vec<u64> {
		let mut out = Vec::new();
		for handle in [self.switches, self.lid.as_ref().map_or(0, |lid| lid.stream), self.activity_stream] {
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
		if self.lid.as_ref().is_some_and(|lid| lid.stream == handle) {
			self.drain_lid(buf);
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
				self.adopt_lid(catalogue, info);
			} else if self.lid.as_ref().is_some_and(|lid| publication_of(&lid.info) == publication_of(&info)) {
				self.drop_lid("its publication was withdrawn");
			}
		}
	}

	// THE LID ARRIVED: its watch opened, its state now read as the first frame.
	fn adopt_lid(&mut self, catalogue: u64, info: ProviderInfo) {
		if self.lid.is_some() {
			return;
		}
		let Some(Ok(chan)) = provider_catalogue::Client::new(ChannelTransport { chan: catalogue }).open(&info) else {
			say("the lid's publication could not be opened");
			return;
		};
		let Some(stream) = platform_switch::Client::with_deadline(ChannelTransport { chan }, clock() + ASK_TICKS).watch() else {
			say("the lid's driver did not open its watch");
			close(chan);
			return;
		};
		say("follows the lid");
		self.lid = Some(Lid { info, chan, stream });
	}

	fn drop_lid(&mut self, why: &str) {
		if let Some(lid) = self.lid.take() {
			close(lid.stream);
			close(lid.chan);
			say(&alloc::format!("no longer follows the lid - {why}"));
		}
	}

	fn drain_lid(&mut self, buf: &mut [u8]) {
		let Some(stream) = self.lid.as_ref().map(|lid| lid.stream) else { return };
		loop {
			match try_recv_caps(stream, buf) {
				PolledCaps::Message { len, handles } => {
					let mut frame = handles;
					let Some(state) = platform_switch::watch_read(&buf[..len], &mut frame) else { continue };
					for &leftover in frame.as_slice() {
						close(leftover);
					}
					if state.kind != SwitchKind::Lid {
						continue;
					}
					say(if state.closed { "the lid is closed" } else { "the lid is open" });
					// AN EXTERNAL DISPLAY IN USE keeps the machine awake: asked at every closing, since one may have been
					// plugged in since the last.
					if state.closed {
						let external = self.external_display();
						self.feed(Event::ExternalDisplay(external));
					}
					self.feed(Event::Lid(state.closed));
				}
				PolledCaps::Empty => return,
				PolledCaps::Closed => {
					self.drop_lid("its driver ended");
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
		let action = self.policy.event(event);
		self.act(action);
	}

	fn act(&mut self, action: Action) {
		match action {
			Action::Nothing => {}
			Action::Suspend(why) => {
				let reason = match why {
					Why::Lid => SleepReason::Lid,
					Why::Idle => SleepReason::Idle,
				};
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
			Action::Hibernate => {
				say("a battery is critical - asks for hibernation");
				let refused = self.sleep == 0 || !matches!(system_sleep::Client::with_deadline(ChannelTransport { chan: self.sleep }, clock() + ASK_TICKS).hibernate(&false, &SleepReason::Critical), Some(Ok(())));
				if refused {
					say("hibernation was refused - the orderly power-off instead");
					let next = self.policy.hibernation_refused();
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
