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

use super::*;
use alloc::format;
use core::sync::atomic::{AtomicU64, Ordering};
use proto::system::{DriversSuspended, SleepOutcome, SleepReason, SleepRecord, SleepState, SleepStep, WakeReason, Woke, application_freeze, device_sleep, platform_sleep, sleep_entry, sleep_notice, system_sleep};
use wire::{Reader, Sink, VecWriter};

// The sleep notice's bit in a manifest row's `notices`.
pub(super) const NOTICE_SLEEP: u8 = 2;

// ------------------------------------------------------------------ the clients

// THE NEAR ENDS OF EVERY `system-sleep` CONNECTION, filled where a connection is minted and read by the standing loop's
// wait set each round - a role delivered at a start, or a grant resolved by name.
pub(super) const MAX_SLEEP_CLIENTS: usize = 16;
pub(super) static SLEEP_CLIENTS: [AtomicU64; MAX_SLEEP_CLIENTS] = [const { AtomicU64::new(0) }; MAX_SLEEP_CLIENTS];

// A FRESH CONNECTION: its near end kept for the loop, its far end answered with every right a minted connection has
// - as a root's `CONNECT` mints one - so its holder can narrow it for whoever it grants it to. None when the table is
// full.
pub(super) fn mint() -> Option<u64> {
	let slot = SLEEP_CLIENTS.iter().find(|slot| slot.load(Ordering::Relaxed) == 0)?;
	let (near, far): (u64, u64) = channel()?;
	slot.store(near, Ordering::Relaxed);
	Some(far)
}

// A client that closed: its slot given back, and any inhibition it held with it.
fn retire(near: u64, sleeper: &mut Sleeper) {
	for slot in SLEEP_CLIENTS.iter() {
		if slot.load(Ordering::Relaxed) == near {
			slot.store(0, Ordering::Relaxed);
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
	Check,
	Announce,
	Freeze,
	Flush,
	Drivers,
	Platform,
	Enter,
	// The undoing ones, run in the reverse of what was taken.
	Wake,
	DriversResume,
	Release,
	Resumed,
	Thaw,
}

impl Action {
	fn name(self) -> &'static str {
		match self {
			Action::Check => "check",
			Action::Announce => "announce",
			Action::Freeze => "freeze",
			Action::Flush => "flush",
			Action::Drivers => "drivers",
			Action::Platform => "platform",
			Action::Enter => "enter",
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
			Action::Enter => ENTER_TICKS,
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
			Action::Enter => SleepStep::Enter,
			_ => SleepStep::None,
		}
	}

	fn undoing(self) -> bool {
		matches!(self, Action::Wake | Action::DriversResume | Action::Release | Action::Resumed | Action::Thaw)
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
	forward: Vec<Action>,
	// What was taken, to be undone - the last pushed is undone first.
	undo: Vec<Action>,
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
		Sleeper { record: empty_record(), txn: None, process, system, next_correlation: 0x5300_0001, inhibitors: Vec::new(), deferred: Vec::new() }
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

	// `suspend` and `hibernate`: answered at acceptance, before any step runs.
	fn request(&mut self, state: SleepState, timed_wake_ms: u64, reason: SleepReason) -> Result<(), Error> {
		if self.txn.is_some() {
			return Err(Error::Again);
		}
		if state == SleepState::Disk {
			// PART E'S: this machine is not set up for hibernation.
			self.record = SleepRecord { state, reason, outcome: SleepOutcome::Refused, step: SleepStep::None, who: String::new(), why: String::from("hibernation is not set up on this machine"), requested: clock_boot_ns(), ..empty_record() };
			return Err(Error::Unsupported);
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
		self.txn = Some(Transaction { state, timed_wake_ms, inhibited_until, forward: alloc::vec![Action::Check, Action::Announce, Action::Freeze, Action::Flush, Action::Drivers, Action::Platform, Action::Enter], undo: Vec::new(), current: None, failed: None, refused: false, ended_by: None, slept: false, announced: Vec::new(), holding: Vec::new(), wake_nodes: Vec::new(), awake_by_ms: 0 });
		Ok(())
	}

	// THE ORDERLY SEQUENCE, TAKEN AT ONCE and never refused: the transaction ends at its next step, and the sequence runs
	// once it has.
	pub(super) fn end_by(&mut self, door: Door) {
		if let Some(txn) = self.txn.as_mut() {
			say(&format!("took the orderly sequence (door: {}); the transaction ends at its next step", door.name));
			txn.forward.clear();
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
			let next = if txn.failed.is_none() && !txn.forward.is_empty() { Some(txn.forward.remove(0)) } else { txn.undo.pop() };
			let Some(action) = next else {
				return Some(self.finish());
			};
			self.start(action, state, channels);
		}
	}

	fn start(&mut self, action: Action, state: &[State; N], channels: &[u64; N]) {
		let Some(txn) = self.txn.as_ref() else { return };
		let (sleep_state, timed_wake_ms, awake_by_ms) = (txn.state, txn.timed_wake_ms, txn.awake_by_ms);
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
							sleep_state.write(w)?;
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
						sleep_state.write(w)?;
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
				sends.push((Target::Service(acpi), frame(platform_sleep::OP_WAKE, |w| sleep_state.write(w))));
			}
			Action::DriversResume => {
				let Some(dm) = index_of(b"device_manager").filter(|&index| ready(index)) else { return self.skip(action) };
				sends.push((Target::Service(dm), frame(device_sleep::OP_RESUME, |w| sleep_state.write(w))));
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
		txn.forward.clear();
	}

	// EVERY TARGET OF THE STEP ANSWERED: the step's undo owed, and the next step free to start.
	fn complete(&mut self, action: Action) {
		let Some(txn) = self.txn.as_mut() else { return };
		txn.current = None;
		// THE THAW GOES LAST in every resume, after the resume notice, whichever was taken first.
		let owed = match action {
			Action::Announce => Some(Action::Resumed),
			Action::Freeze => {
				txn.undo.insert(0, Action::Thaw);
				None
			}
			Action::Flush => Some(Action::Release),
			Action::Drivers => Some(Action::DriversResume),
			Action::Platform => Some(Action::Wake),
			_ => None,
		};
		if let Some(owed) = owed {
			// The resume notice before the thaw, whose place is fixed at the bottom.
			if owed == Action::Resumed && txn.undo.first() == Some(&Action::Thaw) {
				txn.undo.insert(1, owed);
			} else {
				txn.undo.push(owed);
			}
		}
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
			Action::Enter => match Woke::read(reader) {
				Some(woke) => {
					if let Some(txn) = self.txn.as_mut() {
						txn.slept = true;
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

// ------------------------------------------------------------------ the served interface

struct Api<'a> {
	sleeper: &'a mut Sleeper,
	client: u64,
}

impl system_sleep::Service for Api<'_> {
	fn suspend(&mut self, state: SleepState, timed_wake_ms: u64, reason: SleepReason) -> Result<(), Error> {
		if state == SleepState::Disk {
			return Err(Error::Invalid);
		}
		self.sleeper.request(state, timed_wake_ms, reason)
	}

	fn hibernate(&mut self, reason: SleepReason) -> Result<(), Error> {
		self.sleeper.request(SleepState::Disk, 0, reason)
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
}

// ONE REQUEST ON A `system-sleep` CONNECTION; a connection that closed is retired.
pub(super) fn serve(near: u64, sleeper: &mut Sleeper, buf: &mut [u8]) {
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
			let minted: u64 = mint().unwrap_or(0);
			if !try_send(near, &[], minted) && minted != 0 {
				close(minted);
			}
		}
		return;
	}
	let mut reply = alloc::vec![0u8; 4096];
	let mut reply_handles = wire::Handles::new();
	let written = system_sleep::dispatch(&mut Api { sleeper, client: near }, &buf[..len], &mut handles, &mut reply, &mut reply_handles);
	for &leftover in handles.as_slice().iter().chain(reply_handles.as_slice()) {
		close(leftover);
	}
	if let Some(written) = written {
		let _ = try_send(near, &reply[..written], 0);
	}
}
