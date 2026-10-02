// THE SUSPEND TRANSACTION - `liber:process@1/system-sleep`, served here, and the steps it runs.
//
// ONE TRANSACTION AT A TIME, DRIVEN FROM THE STANDING LOOP'S WAIT SET: each step's requests are sent and their answers
// awaited there, under the step's bound, and nothing here blocks - two requesters are participants the transaction
// waits on (DeviceManager for the fixed sleep button, the control-method button's driver), and the loop keeps answering
// everything else meanwhile. `suspend` answers at acceptance; how the sleep ended is read from `last-sleep`.
//
// THE ORDER: a CHECK that every online binding carries the exchange (refused before anything is frozen otherwise);
// ANNOUNCE to every service declaring the sleep notice; FREEZE the applications through ProcessService's supervisor
// root; FLUSH every StorageService instance and hold its writes; SUSPEND THE DRIVERS through DeviceManager; PREPARE THE
// PLATFORM through the ACPI service where one runs; ENTER through SystemManager, which answers once the machine is
// awake. THE RESUME IS THE REVERSE - the platform woken, the drivers resumed, the writes released, the resume notice,
// the applications thawed LAST - and a step that fails unwinds the steps already taken in the same reverse. Each step's
// bound is this file's number scaled by the port's boot window, as DeviceManager scales its deadlines.
//
// THE ORDERLY SEQUENCE ENDS A TRANSACTION AT ITS NEXT STEP: the step in flight finishes, no later step starts, and the
// transaction unwinds - or resumes, if the machine has already slept - and the sequence runs once it has ended.
//
// HIBERNATION is accepted only where the image component says the machine is set up for it - asked at acceptance - and
// runs the same steps up to the entry, which takes the kernel's snapshot and answers twice. In the machine that ran on
// (`snapshot`): the bindings the image goes through resumed (DeviceManager's `resume-for-image`), the image written (the
// component's `write-image`), and the machine off through SystemManager (`off-hibernated`) - or, for a hybrid sleep,
// those bindings suspended again and the machine suspended to RAM. In the machine restored from the image (`restored`):
// the undo, as after any sleep. An image written owes its discard (`discard-image`) whenever the machine runs on after it,
// before the held writes are released - see `service_logic::sleep_transaction`.

use super::*;
use alloc::format;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use proto::system::{DriversSuspended, HibernationStatus, SleepInhibitor, SleepOutcome, SleepReason, SleepRecord, SleepState, SleepStatus, SleepStep, WakeReason, WakeSource, Woke, application_freeze, device_sleep, hibernation_image, platform_sleep, sleep_entry, sleep_notice, system_sleep};
use wire::{Reader, Sink, VecWriter};

// The sleep notice's bit in a manifest row's `notices`.
pub(super) const NOTICE_SLEEP: u8 = 2;

// ------------------------------------------------------------------ the clients

// THE NEAR ENDS OF EVERY `system-sleep` CONNECTION, filled where a connection is minted and read by the standing loop's
// wait set each round - a role delivered at a start, or a grant resolved by name.
pub(super) const MAX_SLEEP_CLIENTS: usize = 16;
// HOW MANY ARE MINTED AHEAD FOR DEVICEMANAGER'S CONTROL-METHOD BUTTONS: a lid, a power and a sleep button, and one more.
pub(super) const BUTTON_POOL: usize = 4;
pub(super) static SLEEP_CLIENTS: [AtomicU64; MAX_SLEEP_CLIENTS] = [const { AtomicU64::new(0) }; MAX_SLEEP_CLIENTS];
// WHICH OF THEM MAY SCHEDULE A WAKE: minted for the `sleep-wake` grant, and every connection a `CONNECT` on one of them
// mints - a holder narrows what it hands on, it never widens it.
static SLEEP_WAKES: [AtomicBool; MAX_SLEEP_CLIENTS] = [const { AtomicBool::new(false) }; MAX_SLEEP_CLIENTS];

// A FRESH CONNECTION: its near end kept for the loop, its far end answered with every right a minted connection has
// - as a root's `CONNECT` mints one - so its holder can narrow it for whoever it grants it to. None when the table is
// full.
pub(super) fn mint() -> Option<u64> {
	mint_with(false)
}

// The same, and whether it may schedule a wake.
pub(super) fn mint_with(wake: bool) -> Option<u64> {
	let at = SLEEP_CLIENTS.iter().position(|slot| slot.load(Ordering::Relaxed) == 0)?;
	let (near, far): (u64, u64) = channel()?;
	SLEEP_CLIENTS[at].store(near, Ordering::Relaxed);
	SLEEP_WAKES[at].store(wake, Ordering::Relaxed);
	Some(far)
}

fn may_wake(near: u64) -> bool {
	SLEEP_CLIENTS.iter().zip(SLEEP_WAKES.iter()).any(|(slot, wake)| slot.load(Ordering::Relaxed) == near && wake.load(Ordering::Relaxed))
}

// A client that closed: its slot given back, and any inhibition it held with it.
fn retire(near: u64, sleeper: &mut Sleeper) {
	for (slot, wake) in SLEEP_CLIENTS.iter().zip(SLEEP_WAKES.iter()) {
		if slot.load(Ordering::Relaxed) == near {
			slot.store(0, Ordering::Relaxed);
			wake.store(false, Ordering::Relaxed);
		}
	}
	sleeper.inhibitors.retain(|inhibitor| inhibitor.client != near);
	close(near);
}

// ------------------------------------------------------------------ the bounds

// Each step's bound, in ticks before the port's scale.
const CHECK_TICKS: u64 = 500;
const ANNOUNCE_TICKS: u64 = 500;
const FREEZE_TICKS: u64 = 300;
const FLUSH_TICKS: u64 = 1_000;
// DeviceManager bounds each binding by its own entry's number and answers a binding that missed it itself; this is the
// bound on DeviceManager answering at all.
const DRIVERS_TICKS: u64 = 12_000;
const PLATFORM_TICKS: u64 = 1_000;
// The clock is held through the sleep itself, so this bounds the entry and the wake, not the night.
const ENTER_TICKS: u64 = 3_000;
// THE IMAGE'S WRITE: every page in use, sealed and encrypted, through the disk's driver - minutes on a large machine.
const IMAGE_TICKS: u64 = 120_000;
// How long the image component may take to say whether the machine is set up, at a hibernation's acceptance.
const STATUS_TICKS: u64 = 1_000;
// The longest an inhibitor may delay an idle sleep.
const MAX_INHIBIT_MS: u64 = 10 * 60 * 1_000;
// How far below a watchdog's "awake by" the timed wake is set, so the resume runs before the timer ends.
const RESUME_MARGIN_MS: u64 = 5_000;
// The ticks the kernel's boot window is written against, as DeviceManager's `BASELINE_BOOT_WINDOW`.
const BASELINE_BOOT_WINDOW: u64 = 3_000;

fn scale() -> u64 {
	let window: u64 = BOOT_WINDOW.load(Ordering::Relaxed);
	if window == 0 { 1 } else { (window / BASELINE_BOOT_WINDOW).max(1) }
}

// ------------------------------------------------------------------ the transaction's parts

// THE ORDER IS `service_logic::sleep_transaction`'S - what is next, what each step owes the undo, the thaw last - and
// host-tested there against scripted answers; what is here is each step's IO.
use service_logic::sleep_transaction::{Action, Plan};

// WHAT A STEP IS CALLED, HOW LONG IT MAY TAKE AND WHICH STEP THE RECORD NAMES - this program's, beside the order.
trait StepInfo {
	fn name(self) -> &'static str;
	fn bound(self) -> u64;
	fn step(self) -> SleepStep;
}

impl StepInfo for Action {
	fn name(self) -> &'static str {
		match self {
			Action::Check => "check",
			Action::Announce => "announce",
			Action::Freeze => "freeze",
			Action::Flush => "flush",
			Action::Drivers => "drivers",
			Action::Platform => "platform",
			Action::Enter => "enter",
			Action::ImageDrivers => "image drivers",
			Action::ImageWrite => "image write",
			Action::DiskOff => "off, hibernated",
			Action::ImageSuspend => "image drivers suspended",
			Action::RamEnter => "hybrid suspend to RAM",
			Action::Discard => "image discarded",
			Action::Wake => "platform wake",
			Action::DriversResume => "drivers resume",
			Action::Release => "writes released",
			Action::Resumed => "resume notice",
			Action::Thaw => "thaw",
		}
	}

	fn bound(self) -> u64 {
		let ticks = match self {
			Action::Check => CHECK_TICKS,
			Action::Announce | Action::Resumed => ANNOUNCE_TICKS,
			Action::Freeze | Action::Thaw => FREEZE_TICKS,
			Action::Flush | Action::Release => FLUSH_TICKS,
			Action::Drivers | Action::DriversResume => DRIVERS_TICKS,
			Action::Platform | Action::Wake => PLATFORM_TICKS,
			Action::Enter | Action::DiskOff | Action::RamEnter => ENTER_TICKS,
			Action::ImageDrivers | Action::ImageSuspend => DRIVERS_TICKS,
			Action::ImageWrite => IMAGE_TICKS,
			Action::Discard => FLUSH_TICKS,
		};
		ticks.saturating_mul(scale())
	}

	// The record's name for a forward step.
	fn step(self) -> SleepStep {
		match self {
			Action::Check | Action::Drivers => SleepStep::Drivers,
			Action::Announce => SleepStep::Announce,
			Action::Freeze => SleepStep::Freeze,
			Action::Flush => SleepStep::Flush,
			Action::Platform => SleepStep::Platform,
			Action::Enter | Action::DiskOff | Action::RamEnter => SleepStep::Enter,
			Action::ImageDrivers | Action::ImageWrite | Action::ImageSuspend | Action::Discard => SleepStep::Image,
			_ => SleepStep::None,
		}
	}
}

// Who a request went to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
	Service(usize),
	Process,
	System,
}

struct Awaited {
	target: Target,
	correlation: u32,
}

struct Current {
	action: Action,
	deadline: u64,
	waiting: Vec<Awaited>,
}

// WHICH DOOR THE ORDERLY SEQUENCE CAME IN BY, and what it asked for.
#[derive(Clone)]
pub(super) struct Door {
	pub(super) name: String,
	pub(super) action: u64,
}

struct Transaction {
	state: SleepState,
	timed_wake_ms: u64,
	// While an idle sleep waits out its inhibitors: until when.
	inhibited_until: u64,
	// The order: the steps ahead and the undo owed.
	plan: Plan,
	current: Option<Current>,
	// The step that failed or refused, who and why.
	failed: Option<(Action, String, String)>,
	refused: bool,
	ended_by: Option<Door>,
	slept: bool,
	// The services the announcement reached and the storage instances that hold their writes: the resume's.
	announced: Vec<usize>,
	holding: Vec<usize>,
	wake_nodes: Vec<String>,
	awake_by_ms: u64,
	// Whether the scheduled wake set this transaction's timed wake.
	scheduled: bool,
	// A hibernation's image kept through a suspend to RAM rather than the machine off.
	hybrid: bool,
}

// One inhibition: its client, until when, and why.
pub(super) struct Inhibitor {
	client: u64,
	until: u64,
	reason: String,
}

// WHAT THE STANDING LOOP HOLDS OF THE SLEEP: the last sleep's record, the transaction if one runs, and its two channels
// that are not control channels - ProcessService's supervisor root and SystemManager.
pub(super) struct Sleeper {
	record: SleepRecord,
	txn: Option<Transaction>,
	process: u64,
	system: u64,
	next_correlation: u32,
	pub(super) inhibitors: Vec<Inhibitor>,
	// Services whose restart the transaction deferred: nothing is started while the drivers are suspended.
	pub(super) deferred: Vec<usize>,
	// THE SCHEDULED WAKE, seconds since the Unix epoch - zero for none - and what the last sleep armed, for `status`.
	scheduled: u64,
	last_wake_nodes: Vec<String>,
	last_awake_by_ms: u64,
}

// What `drive` found when the transaction ended.
pub(super) struct Ended {
	pub(super) door: Option<Door>,
}

fn empty_record() -> SleepRecord {
	SleepRecord { state: SleepState::Idle, reason: SleepReason::Requested, outcome: SleepOutcome::Refused, step: SleepStep::None, who: String::new(), why: String::from("no sleep has been asked for"), requested: 0, slept: 0, wake: WakeReason::Unknown, wake_detail: 0, cores: Vec::new() }
}

fn state_name(state: SleepState) -> &'static str {
	match state {
		SleepState::Idle => "suspend to idle",
		SleepState::Ram => "suspend to RAM",
		SleepState::Disk => "hibernation",
	}
}

fn say(text: &str) {
	let line = format!("ServiceManager: sleep: {text}\n");
	print(line.as_bytes());
}

impl Sleeper {
	pub(super) fn new(process: u64, system: u64) -> Sleeper {
		Sleeper { record: empty_record(), txn: None, process, system, next_correlation: 0x5300_0001, inhibitors: Vec::new(), deferred: Vec::new(), scheduled: 0, last_wake_nodes: Vec::new(), last_awake_by_ms: 0 }
	}

	pub(super) fn running(&self) -> bool {
		self.txn.is_some()
	}

	// The tick the loop must come back at: a step's bound, or an inhibition's end.
	pub(super) fn deadline(&self) -> u64 {
		let Some(txn) = &self.txn else { return 0 };
		match &txn.current {
			Some(current) => current.deadline,
			None if txn.inhibited_until != 0 => txn.inhibited_until,
			None => clock().max(1),
		}
	}

	// The channels an answer is awaited on that are not a service's control channel.
	pub(super) fn wait_handles(&self) -> Vec<(u64, Target)> {
		let mut out = Vec::new();
		if let Some(current) = self.txn.as_ref().and_then(|txn| txn.current.as_ref()) {
			for awaited in &current.waiting {
				match awaited.target {
					Target::Process if self.process != 0 => out.push((self.process, Target::Process)),
					Target::System if self.system != 0 => out.push((self.system, Target::System)),
					_ => {}
				}
			}
		}
		out
	}

	// Whether an answer from `target` is awaited now.
	pub(super) fn awaits(&self, target: Target) -> bool {
		self.txn.as_ref().and_then(|txn| txn.current.as_ref()).is_some_and(|current| current.waiting.iter().any(|awaited| awaited.target == target))
	}

	fn correlation(&mut self) -> u32 {
		let correlation = self.next_correlation;
		self.next_correlation = self.next_correlation.wrapping_add(1);
		correlation
	}

	// ------------------------------------------------------------------ the requests

	// `suspend` and `hibernate`: answered at acceptance, before any step runs. `image` is the image component's control
	// channel, 0 where it does not run.
	fn request(&mut self, state: SleepState, timed_wake_ms: u64, reason: SleepReason, hybrid: bool, image: u64) -> Result<(), Error> {
		if self.txn.is_some() {
			return Err(Error::Again);
		}
		// HIBERNATION WHERE THE MACHINE IS SET UP FOR IT - a hibernation partition as large as memory, and a TPM that seals
		// its key or none at all (with the component's warning) - which the image component says; asked here, so a refusal
		// is the requester's answer.
		if state == SleepState::Disk {
			let why = match image_status(image) {
				Ok(status) if status.set_up => None,
				Ok(status) => Some(format!("hibernation is not set up on this machine - {}", status.why)),
				Err(why) => Some(why),
			};
			if let Some(why) = why {
				say(&format!("hibernation refused: {why}"));
				self.record = SleepRecord { state, reason, outcome: SleepOutcome::Refused, step: SleepStep::None, who: String::from("hibernation_service"), why, requested: clock_boot_ns(), ..empty_record() };
				return Err(Error::Unsupported);
			}
		}
		// A STATE THE KERNEL'S ENTRY WOULD NOT TAKE is refused at acceptance, not at the entry after everything was
		// frozen: suspend to RAM needs `\_S3` registered and a firmware that can come back from it.
		let offered: u64 = sleep_states();
		let bit: u64 = match state {
			SleepState::Idle => 1 << SLEEP_STATE_IDLE,
			SleepState::Ram => 1 << SLEEP_STATE_RAM,
			SleepState::Disk => 1 << SLEEP_STATE_DISK,
		};
		if offered & bit == 0 {
			self.record = SleepRecord { state, reason, outcome: SleepOutcome::Refused, step: SleepStep::None, who: String::new(), why: format!("this machine does not offer {}", state_name(state)), requested: clock_boot_ns(), ..empty_record() };
			return Err(Error::Unsupported);
		}
		// THE SCHEDULED WAKE BECOMES THIS SLEEP'S TIMED WAKE when it is the earlier; one whose time has already passed is
		// spent now - a wall-clock alarm fires once.
		let mut timed_wake_ms = timed_wake_ms;
		let mut scheduled = false;
		if self.scheduled != 0 {
			let now = clock_rtc();
			if now != 0 && self.scheduled <= now {
				say(&format!("the scheduled wake at {} has passed - it is spent", self.scheduled));
				self.scheduled = 0;
			} else if now != 0 {
				let ms = (self.scheduled - now).saturating_mul(1000);
				if timed_wake_ms == 0 || ms < timed_wake_ms {
					timed_wake_ms = ms;
					scheduled = true;
				}
			}
		}
		// AN IDLE SLEEP WAITS OUT ITS INHIBITORS, each within its bound; nothing else waits for them.
		let now = clock();
		self.inhibitors.retain(|inhibitor| inhibitor.until > now);
		let inhibited_until = if reason == SleepReason::Idle { self.inhibitors.iter().map(|inhibitor| inhibitor.until).max().unwrap_or(0) } else { 0 };
		if inhibited_until != 0 {
			let why: Vec<&str> = self.inhibitors.iter().map(|inhibitor| inhibitor.reason.as_str()).collect();
			say(&format!("an idle sleep waits for its inhibitors ({})", why.join(", ")));
		}
		self.record = SleepRecord { state, reason, outcome: SleepOutcome::Running, step: SleepStep::None, who: String::new(), why: String::new(), requested: clock_boot_ns(), ..empty_record() };
		say(&format!("accepted {}{}", state_name(state), if timed_wake_ms != 0 { format!(", with a timed wake in {timed_wake_ms} ms") } else { String::new() }));
		if scheduled {
			say(&format!("the scheduled wake at {} is this sleep's timed wake", self.scheduled));
		}
		let plan = if state == SleepState::Disk { Plan::hibernation(hybrid) } else { Plan::new() };
		self.txn = Some(Transaction { state, timed_wake_ms, inhibited_until, plan, current: None, failed: None, refused: false, ended_by: None, slept: false, announced: Vec::new(), holding: Vec::new(), wake_nodes: Vec::new(), awake_by_ms: 0, scheduled, hybrid });
		Ok(())
	}

	// THE ORDERLY SEQUENCE, TAKEN AT ONCE and never refused: the transaction ends at its next step, and the sequence runs
	// once it has.
	pub(super) fn end_by(&mut self, door: Door) {
		if let Some(txn) = self.txn.as_mut() {
			say(&format!("took the orderly sequence (door: {}); the transaction ends at its next step", door.name));
			txn.plan.end();
			txn.inhibited_until = 0;
			txn.ended_by = Some(door);
		}
	}

	// ------------------------------------------------------------------ the steps

	// ADVANCE AS FAR AS THE ANSWERS IN ALLOW. Answers the transaction's end when it has ended.
	pub(super) fn drive(&mut self, state: &[State; N], channels: &[u64; N]) -> Option<Ended> {
		loop {
			let txn = self.txn.as_mut()?;
			if txn.current.is_some() {
				return None;
			}
			if txn.inhibited_until != 0 {
				if clock() < txn.inhibited_until && !self.inhibitors.is_empty() {
					return None;
				}
				txn.inhibited_until = 0;
			}
			// The next action: forward while nothing failed and nothing ended it, the undo otherwise.
			let next = txn.plan.next();
			let Some(action) = next else {
				return Some(self.finish());
			};
			// THE SLEEP GATE'S HOOK: the resume stops here, the drivers back and nothing after them - no `alive` answered.
			#[cfg(feature = "development")]
			if action == Action::Release && super::SLEEP_HANG.swap(false, Ordering::Relaxed) {
				say("the resume hangs after the drivers (development hook)");
				txn.current = Some(Current { action, deadline: u64::MAX, waiting: Vec::new() });
				return None;
			}
			self.start(action, state, channels);
		}
	}

	fn start(&mut self, action: Action, state: &[State; N], channels: &[u64; N]) {
		let Some(txn) = self.txn.as_ref() else { return };
		let (sleep_state, timed_wake_ms, awake_by_ms) = (txn.state, txn.timed_wake_ms, txn.awake_by_ms);
		// THE DRIVERS AND THE PLATFORM PREPARE FOR WHAT THE MACHINE ENTERS: a hybrid sleep ends in S3 - a TPM saves its
		// state for the S3 wake - and the snapshot needs no firmware.
		let platform_state = if txn.hybrid { SleepState::Ram } else { sleep_state };
		let image = index_of(b"hibernation_service").filter(|&index| state[index] == State::Ready && channels[index] != 0);
		let (wake_nodes, holding, announced) = (txn.wake_nodes.clone(), txn.holding.clone(), txn.announced.clone());
		say(&format!("step {} at tick {}", action.name(), clock()));
		// The targets and each one's request.
		let mut sends: Vec<(Target, Option<Vec<u8>>)> = Vec::new();
		let ready = |index: usize| state[index] == State::Ready && channels[index] != 0;
		match action {
			Action::Check => match index_of(b"device_manager").filter(|&index| ready(index)) {
				Some(dm) => sends.push((Target::Service(dm), frame(device_sleep::OP_CHECK, |_| Some(())))),
				None => {
					self.fail_now(action, String::from("device_manager"), String::from("DeviceManager is not running, and no binding can be asked"));
					return;
				}
			},
			Action::Announce => {
				let participants: Vec<usize> = (0..N).filter(|&index| MANIFEST[index].notices & NOTICE_SLEEP != 0 && ready(index)).collect();
				for &index in &participants {
					sends.push((Target::Service(index), frame(sleep_notice::OP_ANNOUNCE, |w| sleep_state.write(w))));
				}
				if let Some(txn) = self.txn.as_mut() {
					txn.announced = participants;
				}
			}
			Action::Freeze => sends.push((Target::Process, frame(application_freeze::OP_FREEZE, |_| Some(())))),
			Action::Flush => {
				let storage: Vec<usize> = (0..N).filter(|&index| MANIFEST[index].program == b"storage_service" && MANIFEST[index].notices & NOTICE_SLEEP != 0 && ready(index)).collect();
				for &index in &storage {
					sends.push((Target::Service(index), frame(sleep_notice::OP_HOLD_WRITES, |_| Some(()))));
				}
				if let Some(txn) = self.txn.as_mut() {
					txn.holding = storage;
				}
			}
			Action::Drivers => match index_of(b"device_manager").filter(|&index| ready(index)) {
				Some(dm) => {
					sends.push((
						Target::Service(dm),
						frame(device_sleep::OP_SUSPEND, |w| {
							platform_state.write(w)?;
							w.boolean(true)?;
							w.u64(timed_wake_ms)
						}),
					));
				}
				None => {
					self.fail_now(action, String::from("device_manager"), String::from("DeviceManager is not running"));
					return;
				}
			},
			Action::Platform => {
				// AN ACPI MACHINE'S STEP: where no ACPI service runs there is nothing to prepare.
				let Some(acpi) = index_of(b"acpi_service").filter(|&index| ready(index)) else { return self.skip(action) };
				sends.push((
					Target::Service(acpi),
					frame(platform_sleep::OP_PREPARE, |w| {
						platform_state.write(w)?;
						write_list(w, &wake_nodes)
					}),
				));
			}
			Action::Enter => {
				let timed = effective_timed_wake(timed_wake_ms, awake_by_ms);
				if timed != timed_wake_ms {
					say(&format!("a watchdog bounds the sleep: the timed wake is {timed} ms"));
				}
				if self.system == 0 {
					self.fail_now(action, String::from("system_manager"), String::from("there is no channel to SystemManager"));
					return;
				}
				sends.push((
					Target::System,
					frame(sleep_entry::OP_ENTER, |w| {
						sleep_state.write(w)?;
						w.u64(timed)
					}),
				));
			}
			Action::Wake => {
				let Some(acpi) = index_of(b"acpi_service").filter(|&index| ready(index)) else { return self.skip(action) };
				sends.push((Target::Service(acpi), frame(platform_sleep::OP_WAKE, |w| platform_state.write(w))));
			}
			// ------------------------------------------------------------------ hibernation's, after the snapshot
			Action::ImageDrivers | Action::ImageSuspend => match index_of(b"device_manager").filter(|&index| ready(index)) {
				Some(dm) => sends.push((Target::Service(dm), frame(if action == Action::ImageDrivers { device_sleep::OP_RESUME_FOR_IMAGE } else { device_sleep::OP_SUSPEND_IMAGE }, |_| Some(())))),
				None => {
					self.fail_now(action, String::from("device_manager"), String::from("DeviceManager is not running"));
					return;
				}
			},
			Action::ImageWrite | Action::Discard => match image {
				Some(index) => sends.push((Target::Service(index), frame(if action == Action::ImageWrite { hibernation_image::OP_WRITE_IMAGE } else { hibernation_image::OP_DISCARD_IMAGE }, |_| Some(())))),
				None if action == Action::Discard => {
					say("the image component does not run - the image it wrote cannot be discarded, and the next boot's refusal of it is what stands");
					return self.skip(action);
				}
				None => {
					self.fail_now(action, String::from("hibernation_service"), String::from("the image component does not run"));
					return;
				}
			},
			Action::DiskOff | Action::RamEnter => {
				if self.system == 0 {
					self.fail_now(action, String::from("system_manager"), String::from("there is no channel to SystemManager"));
					return;
				}
				sends.push((
					Target::System,
					if action == Action::DiskOff {
						frame(sleep_entry::OP_OFF_HIBERNATED, |_| Some(()))
					} else {
						frame(sleep_entry::OP_ENTER, |w| {
							SleepState::Ram.write(w)?;
							w.u64(0)
						})
					},
				));
			}
			Action::DriversResume => {
				let Some(dm) = index_of(b"device_manager").filter(|&index| ready(index)) else { return self.skip(action) };
				sends.push((Target::Service(dm), frame(device_sleep::OP_RESUME, |w| platform_state.write(w))));
			}
			Action::Release => {
				for &index in holding.iter().filter(|&&index| ready(index)) {
					sends.push((Target::Service(index), frame(sleep_notice::OP_RELEASE_WRITES, |_| Some(()))));
				}
			}
			Action::Resumed => {
				for &index in announced.iter().filter(|&&index| ready(index)) {
					sends.push((Target::Service(index), frame(sleep_notice::OP_RESUMED, |w| sleep_state.write(w))));
				}
			}
			Action::Thaw => sends.push((Target::Process, frame(application_freeze::OP_THAW, |_| Some(())))),
		}
		let mut waiting: Vec<Awaited> = Vec::new();
		for (target, bytes) in sends {
			let Some(mut bytes) = bytes else { continue };
			let correlation = self.correlation();
			if bytes.len() >= 6 {
				bytes[2..6].copy_from_slice(&correlation.to_le_bytes());
			}
			let channel = match target {
				Target::Service(index) => channels[index],
				Target::Process => self.process,
				Target::System => self.system,
			};
			if channel != 0 && try_send(channel, &bytes, 0) {
				waiting.push(Awaited { target, correlation });
			} else if !action.undoing() {
				let who = self.who(target);
				self.fail_now(action, who, String::from("its channel took no request"));
				return;
			}
		}
		// A STEP WITH NOBODY TO ASK is done at once, and its undo is owed all the same where it is one.
		let Some(txn) = self.txn.as_mut() else { return };
		txn.current = Some(Current { action, deadline: clock().saturating_add(action.bound()), waiting });
		if txn.current.as_ref().is_some_and(|current| current.waiting.is_empty()) {
			self.complete(action);
		}
	}

	// A step with nothing to ask on this machine: done.
	fn skip(&mut self, action: Action) {
		if let Some(txn) = self.txn.as_mut() {
			txn.current = Some(Current { action, deadline: 0, waiting: Vec::new() });
		}
		self.complete(action);
	}

	fn who(&self, target: Target) -> String {
		match target {
			Target::Service(index) => String::from_utf8_lossy(MANIFEST[index].name).into_owned(),
			Target::Process => String::from("process_service"),
			Target::System => String::from("system_manager"),
		}
	}

	// A FORWARD STEP FAILED BEFORE IT COULD ASK: nothing more goes forward, and the undo runs.
	fn fail_now(&mut self, action: Action, who: String, why: String) {
		if let Some(txn) = self.txn.as_mut() {
			txn.current = None;
			self.fail(action, who, why);
		}
	}

	fn fail(&mut self, action: Action, who: String, why: String) {
		let Some(txn) = self.txn.as_mut() else { return };
		if action.undoing() {
			say(&format!("{} did not complete at {who} - {why}; the resume goes on", action.name()));
			return;
		}
		if action == Action::Check {
			say(&format!("refused before anything was frozen: {who} - {why}"));
			txn.refused = true;
		} else {
			say(&format!("step {} failed at {who} - {why}; the steps taken are undone", action.name()));
		}
		if txn.failed.is_none() {
			txn.failed = Some((action, who, why));
		}
		txn.plan.failed(action);
	}

	// EVERY TARGET OF THE STEP ANSWERED: the step's undo owed, and the next step free to start.
	fn complete(&mut self, action: Action) {
		let Some(txn) = self.txn.as_mut() else { return };
		txn.current = None;
		// WHAT THE STEP OWES THE UNDO - the thaw last in every resume, after the resume notice - is the plan's.
		txn.plan.completed(action);
	}

	// ------------------------------------------------------------------ the answers

	// ONE ANSWER from `target`: taken when it is the one awaited, ignored otherwise.
	pub(super) fn answer(&mut self, target: Target, bytes: &[u8]) -> bool {
		let Some(txn) = self.txn.as_mut() else { return false };
		let Some(current) = txn.current.as_mut() else { return false };
		let Some(at) = current.waiting.iter().position(|awaited| awaited.target == target && bytes.len() >= 5 && bytes[..4] == awaited.correlation.to_le_bytes()) else { return false };
		current.waiting.remove(at);
		let action = current.action;
		let done = current.waiting.is_empty();
		let mut reader = Reader::new(&bytes[4..]);
		let ok = reader.tag().unwrap_or(false);
		let who = self.who(target);
		if !ok {
			let why = proto::system::Error::read(&mut reader).map_or(String::from("a refusal it did not explain"), |error| format!("it answered {error:?}"));
			self.fail(action, who, why);
		} else {
			self.take(action, who, &mut reader);
		}
		if done && self.txn.as_ref().is_some_and(|txn| txn.current.as_ref().is_some_and(|current| current.action == action)) {
			self.complete(action);
		}
		true
	}

	// What a successful answer carries, for the steps whose answer is more than done.
	fn take(&mut self, action: Action, who: String, reader: &mut Reader) {
		match action {
			Action::Check => {
				let unready = read_list(reader).unwrap_or_default();
				if let Some(first) = unready.first() {
					let (binding, why) = first.split_once(": ").map_or((first.clone(), String::new()), |(binding, why)| (String::from(binding), String::from(why)));
					self.fail(action, binding, why);
				}
			}
			Action::Drivers => match DriversSuspended::read(reader) {
				Some(answer) if answer.failed.is_empty() => {
					if let Some(txn) = self.txn.as_mut() {
						txn.wake_nodes = answer.wake_nodes;
						txn.awake_by_ms = answer.awake_by_ms;
					}
				}
				// THE DRIVERS' STEP UNWOUND ITSELF before it answered: its own undo is not owed.
				Some(answer) => {
					self.fail(action, answer.failed, answer.why);
					if let Some(txn) = self.txn.as_mut() {
						txn.current = None;
					}
				}
				None => self.fail(action, who, String::from("an answer that does not decode")),
			},
			Action::DriversResume => {
				let not_back = read_list(reader).unwrap_or_default();
				for device in &not_back {
					say(&format!("{device} did not come back and is bound again"));
				}
			}
			Action::ImageDrivers => {
				let resumed = read_list(reader).unwrap_or_default();
				say(&format!("the image is written through {}", if resumed.is_empty() { String::from("no binding") } else { resumed.join(", ") }));
			}
			Action::Enter | Action::RamEnter => match Woke::read(reader) {
				// THE SNAPSHOT'S FIRST ANSWER: nothing slept - the image is to be written.
				Some(woke) if woke.wake == WakeReason::Snapshot => say("hibernation's snapshot is taken - the image is written next"),
				// ITS SECOND, IN THE MACHINE THE IMAGE RESTORED, is a sleep that ended: nothing of the image's steps goes
				// forward.
				Some(woke) => {
					if let Some(txn) = self.txn.as_mut() {
						txn.slept = true;
						if woke.wake == WakeReason::Restored {
							txn.plan.restored();
						}
					}
					say(&format!("resumed - woken by {:?} after {} ms", woke.wake, woke.slept / 1_000_000));
					self.record.wake = woke.wake;
					self.record.wake_detail = woke.detail;
					self.record.slept = woke.slept;
					self.record.cores = woke.cores;
				}
				None => self.fail(action, who, String::from("an answer that does not decode")),
			},
			_ => {}
		}
	}

	// THE STEP'S BOUND PASSED with answers still owed.
	pub(super) fn expire(&mut self) {
		let Some(txn) = self.txn.as_mut() else { return };
		let Some(current) = txn.current.as_ref() else { return };
		if clock() < current.deadline {
			return;
		}
		let action = current.action;
		let (now, deadline) = (clock(), current.deadline);
		let late: Vec<Target> = current.waiting.iter().map(|awaited| awaited.target).collect();
		for target in late {
			let who = self.who(target);
			self.fail(action, who, format!("it did not answer within the step's bound (tick {now}, bound ended at {deadline})"));
		}
		self.complete(action);
	}

	// A TARGET ENDED: a service that crashed, a channel that closed. An answer it owed is now a failure.
	pub(super) fn lost(&mut self, target: Target) {
		if !self.awaits(target) {
			return;
		}
		let Some(txn) = self.txn.as_mut() else { return };
		let Some(current) = txn.current.as_mut() else { return };
		current.waiting.retain(|awaited| awaited.target != target);
		let action = current.action;
		let done = current.waiting.is_empty();
		let who = self.who(target);
		self.fail(action, who, String::from("it ended while the step waited on it"));
		if done {
			self.complete(action);
		}
	}

	// ------------------------------------------------------------------ the end

	fn finish(&mut self) -> Ended {
		let txn = self.txn.take().expect("a transaction to finish");
		// WHAT THE SLEEP ARMED, for `status`; and THE SCHEDULED WAKE SPENT once its time has come - by the sleep it woke,
		// or by one that slept past it, once however many periods it missed.
		if txn.slept {
			self.last_wake_nodes = txn.wake_nodes.clone();
			self.last_awake_by_ms = txn.awake_by_ms;
		}
		if self.scheduled != 0 && (txn.slept || txn.scheduled) {
			let now = clock_rtc();
			if now != 0 && self.scheduled <= now {
				say(&format!("the scheduled wake at {} is spent", self.scheduled));
				self.scheduled = 0;
			}
		}
		let record = &mut self.record;
		match (&txn.failed, txn.slept) {
			(_, true) => record.outcome = SleepOutcome::Resumed,
			(Some((action, who, why)), false) => {
				record.outcome = if txn.refused { SleepOutcome::Refused } else { SleepOutcome::Unwound };
				record.step = action.step();
				record.who = who.clone();
				record.why = why.clone();
			}
			(None, false) => {
				record.outcome = SleepOutcome::Unwound;
			}
		}
		// A TRANSACTION THE ORDERLY SEQUENCE ENDED names the sequence and its door, beside any step that failed as well.
		if let Some(door) = &txn.ended_by {
			let named = format!("the orderly sequence ({})", door.name);
			if record.who.is_empty() {
				record.who = named;
				record.why = String::from("it ended the transaction");
			} else {
				record.why = format!("{}; ended by {named}", record.why);
			}
		}
		say(&format!(
			"the transaction ended - {}",
			match record.outcome {
				SleepOutcome::Resumed => String::from("slept and woke"),
				SleepOutcome::Refused => format!("refused: {} - {}", record.who, record.why),
				SleepOutcome::Unwound => format!("unwound at {:?}: {} - {}", record.step, record.who, record.why),
				SleepOutcome::Running => String::from("running"),
			}
		));
		Ended { door: txn.ended_by }
	}
}

// The timed wake, below a watchdog's "awake by" where one bounds the sleep.
fn effective_timed_wake(timed_wake_ms: u64, awake_by_ms: u64) -> u64 {
	if awake_by_ms == 0 {
		return timed_wake_ms;
	}
	let cap = awake_by_ms.saturating_sub(RESUME_MARGIN_MS).max(1);
	if timed_wake_ms == 0 { cap } else { timed_wake_ms.min(cap) }
}

// ONE REQUEST FRAME: the operation, a correlation filled in when it is sent, and the arguments.
fn frame(op: u16, body: impl FnOnce(&mut VecWriter) -> Option<()>) -> Option<Vec<u8>> {
	let mut w = VecWriter::new();
	w.u16(op)?;
	w.u32(0)?;
	body(&mut w)?;
	w.into_inner()
}

fn write_list(w: &mut VecWriter, items: &[String]) -> Option<()> {
	let count = items.len().min(usize::from(u16::MAX));
	w.u16(count as u16)?;
	for item in &items[..count] {
		w.bytes_lp(item.as_bytes())?;
	}
	Some(())
}

fn read_list(reader: &mut Reader) -> Option<Vec<String>> {
	let count = reader.u16()? as usize;
	let mut out = Vec::new();
	for _ in 0..count {
		out.push(reader.string_lp()?);
	}
	Some(out)
}

// THE IMAGE COMPONENT'S STATUS, on its control channel - 0 where it does not run.
fn image_status(image: u64) -> Result<HibernationStatus, String> {
	if image == 0 {
		return Err(String::from("the hibernation image component does not run"));
	}
	match hibernation_image::Client::with_deadline(ChannelTransport { chan: image }, clock().saturating_add(STATUS_TICKS)).status() {
		Some(Ok(status)) => Ok(status),
		Some(Err(error)) => Err(format!("the image component answered {error:?}")),
		None => Err(String::from("the image component did not answer")),
	}
}

// ------------------------------------------------------------------ the served interface

struct Api<'a> {
	sleeper: &'a mut Sleeper,
	client: u64,
	// The image component's control channel, 0 where it does not run.
	image: u64,
}

impl system_sleep::Service for Api<'_> {
	fn suspend(&mut self, state: SleepState, timed_wake_ms: u64, reason: SleepReason) -> Result<(), Error> {
		if state == SleepState::Disk {
			return Err(Error::Invalid);
		}
		self.sleeper.request(state, timed_wake_ms, reason, false, 0)
	}

	fn hibernate(&mut self, hybrid: bool, reason: SleepReason) -> Result<(), Error> {
		self.sleeper.request(SleepState::Disk, 0, reason, hybrid, self.image)
	}

	fn inhibit(&mut self, milliseconds: u32, reason: String) -> Result<(), Error> {
		if milliseconds == 0 {
			return Err(Error::Invalid);
		}
		let ms = u64::from(milliseconds).min(MAX_INHIBIT_MS);
		let until = clock().saturating_add(ms.saturating_mul(TICKS_PER_SECOND).div_ceil(1000));
		self.sleeper.inhibitors.retain(|inhibitor| inhibitor.client != self.client);
		self.sleeper.inhibitors.push(Inhibitor { client: self.client, until, reason });
		Ok(())
	}

	fn release(&mut self) -> Result<(), Error> {
		self.sleeper.inhibitors.retain(|inhibitor| inhibitor.client != self.client);
		Ok(())
	}

	fn last_sleep(&mut self) -> Result<SleepRecord, Error> {
		Ok(self.sleeper.record.clone())
	}

	fn status(&mut self) -> Result<SleepStatus, Error> {
		let offered: u64 = sleep_states();
		let fixed = |bit: u64| if offered & bit != 0 { String::from("the fixed button PM1 carries") } else { String::from("a control-method device, or none") };
		let mut wake_sources: Vec<WakeSource> = alloc::vec![
			WakeSource { name: String::from("power button"), armed: offered & SLEEP_FIXED_POWER_BUTTON != 0, detail: fixed(SLEEP_FIXED_POWER_BUTTON) },
			WakeSource { name: String::from("sleep button"), armed: offered & SLEEP_FIXED_SLEEP_BUTTON != 0, detail: fixed(SLEEP_FIXED_SLEEP_BUTTON) },
			WakeSource { name: String::from("timed wake"), armed: true, detail: String::from("the one-shot timer for suspend to idle, the CMOS alarm for suspend to RAM") },
			WakeSource { name: String::from("scheduled wake"), armed: self.sleeper.scheduled != 0, detail: if self.sleeper.scheduled != 0 { format!("at {} (Unix seconds)", self.sleeper.scheduled) } else { String::from("none scheduled") } },
		];
		for node in self.sleeper.last_wake_nodes.iter().take(28) {
			wake_sources.push(WakeSource { name: node.clone(), armed: false, detail: String::from("a device whose driver armed wake at the last sleep") });
		}
		let now = clock();
		let inhibitors = self.sleeper.inhibitors.iter().filter(|inhibitor| inhibitor.until > now).take(16).map(|inhibitor| SleepInhibitor { reason: inhibitor.reason.clone(), remaining_ms: (inhibitor.until - now).saturating_mul(1000) / TICKS_PER_SECOND }).collect();
		let (hibernation_set_up, hibernation_why, last_image) = match image_status(self.image) {
			Ok(status) => (status.set_up, status.why, status.last_image),
			Err(why) => (false, why, String::from("none")),
		};
		Ok(SleepStatus { idle: offered & (1 << SLEEP_STATE_IDLE) != 0, ram: offered & (1 << SLEEP_STATE_RAM) != 0, disk: offered & (1 << SLEEP_STATE_DISK) != 0, wake_sources, inhibitors, watchdog_cap_ms: self.sleeper.last_awake_by_ms, scheduled_wake: self.sleeper.scheduled, hibernation_set_up, hibernation_why, last_image })
	}

	// A CONNECTION MINTED WITHOUT THE WAKE GRANT IS REFUSED: a program that can wake the machine can empty its battery.
	fn schedule_wake(&mut self, unix_seconds: u64) -> Result<(), Error> {
		if !may_wake(self.client) {
			return Err(Error::Denied);
		}
		if unix_seconds != 0 && unix_seconds <= clock_rtc() {
			return Err(Error::Invalid);
		}
		self.sleeper.scheduled = unix_seconds;
		say(&if unix_seconds == 0 { String::from("the scheduled wake is cancelled") } else { format!("a wake is scheduled at {unix_seconds} (Unix seconds)") });
		Ok(())
	}
}

// ONE REQUEST ON A `system-sleep` CONNECTION; a connection that closed is retired. `image` is the image component's
// control channel, 0 where it does not run.
pub(super) fn serve(near: u64, sleeper: &mut Sleeper, buf: &mut [u8], image: u64) {
	let (len, mut handles) = match try_recv_caps(near, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return,
		PolledCaps::Closed => {
			retire(near, sleeper);
			return;
		}
	};
	// THE SERVER CONVENTIONS every root answers: a `CONNECT` mints a fresh connection - PermissionManager grants each
	// launch one, minted from the connection it holds - and a heartbeat is answered.
	let op: u16 = if len >= 2 { u16::from_le_bytes([buf[0], buf[1]]) } else { 0 };
	if op == CONNECT_OP || op == HEARTBEAT_OP {
		for &leftover in handles.as_slice() {
			close(leftover);
		}
		if op == HEARTBEAT_OP {
			let _ = try_send(near, b"PONG", 0);
		} else {
			let minted: u64 = mint_with(may_wake(near)).unwrap_or(0);
			if !try_send(near, &[], minted) && minted != 0 {
				close(minted);
			}
		}
		return;
	}
	let mut reply = alloc::vec![0u8; 4096];
	let mut reply_handles = wire::Handles::new();
	let written = system_sleep::dispatch(&mut Api { sleeper, client: near, image }, &buf[..len], &mut handles, &mut reply, &mut reply_handles);
	for &leftover in handles.as_slice().iter().chain(reply_handles.as_slice()) {
		close(leftover);
	}
	if let Some(written) = written {
		let _ = try_send(near, &reply[..written], 0);
	}
}
