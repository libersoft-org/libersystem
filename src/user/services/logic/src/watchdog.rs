//! THE HARDWARE WATCHDOG SERVICE'S DECISIONS: its policy, which of the published watchdogs it arms and what it
//! does with every other one, and the liveness schedule its pets follow.
//!
//! A hardware watchdog resets the machine unless it is told, in time, that the system is alive. The service is its
//! one consumer, and it pets only after ServiceManager's standing loop has answered a liveness question within a
//! deadline - so a kernel that stopped scheduling, a supervisor that deadlocked or a watchdog service whose own loop
//! stopped all end in the reset the device exists for. Everything here is a function of its inputs and is
//! host-tested; the service is the wire around it.

use alloc::string::String;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

/// The most `watchdog` providers the service takes; a fifth is logged and not opened.
pub const MAX_PROVIDERS: usize = 4;

/// THE DEFAULT ORDER a device is chosen in when the policy names none: the ACPI watchdog, the chipset's TCO, the
/// PCI `i6300esb`, the BMC's. The first whose arm succeeds is the one armed.
pub const DEFAULT_ORDER: [&str; 4] = ["wdat", "tco", "i6300esb", "bmc"];

/// The proposed defaults of the three numbers - the timeout from the last pet to the reset, the period between
/// liveness questions, and how long an answer may take - config keys the owner confirms.
pub const DEFAULT_TIMEOUT_MS: u32 = 60_000;
pub const DEFAULT_PERIOD_MS: u32 = 15_000;
pub const DEFAULT_DEADLINE_MS: u32 = 5_000;

/// THE BOOT'S MODE, as ServiceManager hands it over in the service's mode payload: the byte strings its own `MODE`
/// bootstrap message carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootMode {
	/// A shipping image booted as one.
	Shipping,
	/// A development image.
	Development,
	/// A test boot - the kernel suite brings every manifest service up in its boot.
	Test,
}

impl BootMode {
	/// The mode a payload names. A PAYLOAD WITH NO BYTES IS REFUSED - `None` - so a start that was handed the tag
	/// alone fails rather than reading a default: a relaunched instance in a test boot that fell back to the
	/// shipping default would arm the timer this rule exists to leave off.
	pub fn from_payload(bytes: &[u8]) -> Option<BootMode> {
		match bytes {
			b"shipping" => Some(BootMode::Shipping),
			b"development" => Some(BootMode::Development),
			b"test" => Some(BootMode::Test),
			_ => None,
		}
	}

	/// Whether the watchdog is armed when the policy does not say: DEVELOPMENT IMAGES AND TEST BOOTS DEFAULT TO
	/// OFF, a shipping image to on.
	pub fn arms_by_default(self) -> bool {
		self == BootMode::Shipping
	}
}

/// THE POLICY, as the config keys and the mode make it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
	/// `watchdog.enabled`, or the mode's default.
	pub enabled: bool,
	/// `watchdog.device`: the one device to arm, or the default order.
	pub device: Option<String>,
	pub timeout_ms: u32,
	pub period_ms: u32,
	pub deadline_ms: u32,
}

/// Why a policy was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
	/// A number of zero.
	Zero,
	/// The timeout is less than three periods, so one late round could reset the machine.
	TimeoutUnderThreePeriods,
	/// The deadline is not shorter than the period, so questions would overlap.
	DeadlineNotUnderPeriod,
}

impl Policy {
	/// The policy the keys give, each absent one its default. `device` empty is the same as absent.
	pub fn from_keys(mode: BootMode, enabled: Option<bool>, device: Option<&str>, timeout_ms: Option<u32>, period_ms: Option<u32>, deadline_ms: Option<u32>) -> Result<Policy, PolicyError> {
		let timeout_ms = timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
		let period_ms = period_ms.unwrap_or(DEFAULT_PERIOD_MS);
		let deadline_ms = deadline_ms.unwrap_or(DEFAULT_DEADLINE_MS);
		if timeout_ms == 0 || period_ms == 0 || deadline_ms == 0 {
			return Err(PolicyError::Zero);
		}
		if (timeout_ms as u64) < 3 * period_ms as u64 {
			return Err(PolicyError::TimeoutUnderThreePeriods);
		}
		if deadline_ms >= period_ms {
			return Err(PolicyError::DeadlineNotUnderPeriod);
		}
		Ok(Policy { enabled: enabled.unwrap_or_else(|| mode.arms_by_default()), device: device.filter(|name| !name.is_empty()).map(String::from), timeout_ms, period_ms, deadline_ms })
	}
}

/// What became of one published provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
	/// Taken, and nothing done with it yet.
	Held,
	/// This service's one armed timer.
	Armed,
	/// Stopped where the device allows it.
	Disarmed,
	/// Kept fed at the configured timeout: a timer that cannot be stopped, or that refused to stop.
	Fed,
	/// Its arm was refused (`unsupported`, or no answer).
	Refused,
}

/// Whether a published provider is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
	Take,
	/// A fifth: logged, not opened.
	TooMany,
	/// A second provider under a name already held - two interfaces to one BMC are one timer, and a disarm
	/// through one would stop what was armed through the other: left unopened and reported, never disarmed.
	SameName,
}

/// The next thing to do with the providers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
	Arm(usize),
	Disarm(usize),
	/// Arm at the configured timeout and keep feeding - a timer that cannot be stopped.
	Feed(usize),
}

/// A provider the service holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
	pub name: String,
	pub can_disarm: bool,
	pub role: Role,
}

/// SEVERAL PROVIDERS, ONE ARMED. The choice is made at the first answered liveness question, never at start - and a
/// provider published later is taken the same way: armed if nothing is armed and it would have been chosen,
/// otherwise disarmed or kept fed. An armed timer is never switched for another.
#[derive(Clone, Debug, Default)]
pub struct Choice {
	held: Vec<Held>,
	started: bool,
}

impl Choice {
	/// Take a published provider, or say why not.
	pub fn offer(&mut self, name: &str, can_disarm: bool) -> Offer {
		if self.held.iter().any(|held| held.name == name) {
			return Offer::SameName;
		}
		if self.held.len() >= MAX_PROVIDERS {
			return Offer::TooMany;
		}
		self.held.push(Held { name: String::from(name), can_disarm, role: Role::Held });
		Offer::Take
	}

	/// A provider withdrawn: its entry goes, and if it was the armed one nothing is armed now.
	pub fn withdraw(&mut self, name: &str) {
		self.held.retain(|held| held.name != name);
	}

	pub fn held(&self) -> &[Held] {
		&self.held
	}

	pub fn armed(&self) -> Option<usize> {
		self.held.iter().position(|held| held.role == Role::Armed)
	}

	/// The first liveness answer: the choice may begin.
	pub fn start(&mut self) {
		self.started = true;
	}

	/// THE NEXT STEP the policy asks for, until there is none. Before the first answered liveness question
	/// there is none: a timer found running is its driver's until then.
	pub fn next(&self, policy: &Policy) -> Option<Step> {
		if !self.started {
			return None;
		}
		let armed = self.armed().is_some();
		if policy.enabled && !armed {
			let candidate = match policy.device.as_deref() {
				// A NAMED DEVICE IS THE ONLY ONE: absent or refusing, nothing is armed in its place.
				Some(named) => self.held.iter().position(|held| held.name == named && held.role == Role::Held),
				None => self.by_default_order(),
			};
			if let Some(at) = candidate {
				return Some(Step::Arm(at));
			}
		}
		// EVERY OTHER PROVIDER is disarmed where it allows it and kept fed where it does not. While a candidate of
		// the policy's is still untried it is not settled here - it may yet be armed.
		self.held.iter().enumerate().find_map(|(at, held)| {
			if held.role != Role::Held || (policy.enabled && !armed && self.may_still_be_chosen(policy, at)) {
				return None;
			}
			Some(if held.can_disarm { Step::Disarm(at) } else { Step::Feed(at) })
		})
	}

	fn by_default_order(&self) -> Option<usize> {
		let rank = |name: &str| DEFAULT_ORDER.iter().position(|known| *known == name).unwrap_or(DEFAULT_ORDER.len());
		self.held.iter().enumerate().filter(|(_, held)| held.role == Role::Held).min_by_key(|(at, held)| (rank(&held.name), *at)).map(|(at, _)| at)
	}

	fn may_still_be_chosen(&self, policy: &Policy, at: usize) -> bool {
		match policy.device.as_deref() {
			Some(named) => self.held[at].name == named,
			None => true,
		}
	}

	/// What an arm answered.
	pub fn arm_answered(&mut self, at: usize, armed: bool) {
		if let Some(held) = self.held.get_mut(at) {
			held.role = if armed { Role::Armed } else { Role::Refused };
		}
	}

	/// What a disarm answered: a refusal leaves the timer to be fed.
	pub fn disarm_answered(&mut self, at: usize, disarmed: bool) {
		if let Some(held) = self.held.get_mut(at) {
			held.role = if disarmed { Role::Disarmed } else { Role::Fed };
		}
	}

	/// A timer it cannot stop is fed from now on.
	pub fn feeding(&mut self, at: usize) {
		if let Some(held) = self.held.get_mut(at) {
			held.role = Role::Fed;
		}
	}

	/// The timers the schedule pets: the armed one and every one kept fed.
	pub fn petted(&self) -> impl Iterator<Item = usize> + '_ {
		self.held.iter().enumerate().filter(|(_, held)| matches!(held.role, Role::Armed | Role::Fed)).map(|(at, _)| at)
	}

	/// Whether a named device the policy asks for is absent or refused - which the service reports, arming
	/// nothing in its place.
	pub fn named_unavailable(&self, policy: &Policy) -> bool {
		let Some(named) = policy.device.as_deref() else { return false };
		policy.enabled && self.started && !self.held.iter().any(|held| held.name == named && matches!(held.role, Role::Held | Role::Armed))
	}
}

/// What the liveness schedule asks for now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
	/// Ask ServiceManager, with this sequence number.
	Ask(u64),
	/// Nothing until this time.
	Wait(u64),
}

/// THE LIVENESS SCHEDULE, in milliseconds of a monotonic clock: a question each period, and a pet only for an
/// answer within the deadline. No answer within it skips that pet - and the hardware decides after the timeout.
#[derive(Clone, Copy, Debug)]
pub struct Schedule {
	period_ms: u64,
	deadline_ms: u64,
	next_ask: u64,
	// The question in flight: its sequence and when it was asked.
	outstanding: Option<(u64, u64)>,
	sequence: u64,
	answered_once: bool,
}

impl Schedule {
	/// A schedule whose first question is asked at `now`.
	pub fn new(policy: &Policy, now: u64) -> Schedule {
		Schedule { period_ms: policy.period_ms as u64, deadline_ms: policy.deadline_ms as u64, next_ask: now, outstanding: None, sequence: 0, answered_once: false }
	}

	/// What to do at `now`. A question left unanswered past its deadline is given up, and the next one waits for
	/// its period.
	pub fn tick(&mut self, now: u64) -> Tick {
		if let Some((_, asked)) = self.outstanding {
			if now < asked + self.deadline_ms {
				return Tick::Wait(asked + self.deadline_ms);
			}
			self.outstanding = None;
		}
		if now >= self.next_ask {
			self.sequence += 1;
			self.outstanding = Some((self.sequence, now));
			self.next_ask = now + self.period_ms;
			return Tick::Ask(self.sequence);
		}
		Tick::Wait(self.next_ask)
	}

	/// An answer arrived at `now`: whether it is this question's, within its deadline - a pet. The first such
	/// answer is also what starts the choice.
	pub fn answered(&mut self, sequence: u64, now: u64) -> bool {
		match self.outstanding {
			Some((asked_sequence, asked)) if asked_sequence == sequence && now <= asked + self.deadline_ms => {
				self.outstanding = None;
				self.answered_once = true;
				true
			}
			_ => false,
		}
	}

	pub fn answered_once(&self) -> bool {
		self.answered_once
	}
}
