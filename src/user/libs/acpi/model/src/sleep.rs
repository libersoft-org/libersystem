//! THE PLATFORM HALF OF A SLEEP, in the order ACPI gives it - what the ACPI service runs for `platform-sleep`'s `prepare`
//! and `wake`, and how it reads the sleep types it registers at its start. The namespace, the power resources and the
//! kernel's general-purpose events are the service's (`Platform`); what runs, on what, and in what order is decided
//! here, so a host test drives it against a scripted namespace.
//!
//! `prepare`: what an earlier `prepare` took is let go first; then each wake node in turn - its `_PRW` read (an event in
//! the FADT's blocks, the deepest state it wakes from, the power resources it needs), the node passed over where it has
//! none, names an event on a GPE block device or cannot wake from the state entered; its resources held on; `_DSW`, or
//! `_PSW` where there is no `_DSW`; its event set for wake - then `_PTS` for a state the firmware enters (suspend to
//! idle is no firmware transition), then `\_SI._SST` sleeping (or sleeping with its context saved, for S4).
//! `wake`: `\_SI._SST` waking, `_WAK` for a state the firmware entered, every event `prepare` set cleared, every
//! resource it held let go, `\_SI._SST` working.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The state a sleep enters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
	Idle,
	Ram,
	Disk,
}

impl State {
	/// The S-state `_PTS`, `_WAK` and `_DSW` are given - 0 for suspend to idle, which the firmware does not enter.
	pub fn target(self) -> u64 {
		match self {
			State::Idle => 0,
			State::Ram => 3,
			State::Disk => 4,
		}
	}

	fn name(self) -> &'static str {
		match self {
			State::Idle => "suspend to idle",
			State::Ram => "S3",
			State::Disk => "S4",
		}
	}
}

/// `_SST`'s values: working, waking, sleeping, sleeping with its context saved.
pub const SST_WORKING: u64 = 1;
pub const SST_WAKING: u64 = 2;
pub const SST_SLEEPING: u64 = 3;
pub const SST_HIBERNATING: u64 = 4;

/// The absolute paths `prepare` and `wake` run.
pub const PTS: &str = "\\_PTS";
pub const WAK: &str = "\\_WAK";
pub const SST: &str = "\\_SI_._SST";

/// A wake node's `_PRW`, as the namespace answers it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Prw<R> {
	/// The node is not in the namespace.
	Absent,
	/// The node has no `_PRW`: it cannot wake the machine.
	None,
	/// `_PRW`'s event names a GPE block device - not one this service arms.
	BlockDevice,
	/// The event's number in the FADT's blocks, the deepest state it wakes from and the power resources it needs.
	Event { number: u16, deepest: u64, resources: Vec<R> },
}

/// What the service does for the sequence.
pub trait Platform {
	/// A power resource, as the service names it.
	type Resource;
	fn prw(&mut self, identity: &str) -> Prw<Self::Resource>;
	/// The resources held for `holder`, each turned on that was off.
	fn hold_power(&mut self, holder: &str, resources: &[Self::Resource]);
	/// What `holder` held let go, each resource no one holds any more turned off.
	fn release_power(&mut self, holder: &str);
	/// `_DSW(1, target, 0)` on the node, or `_PSW(1)` where it has no `_DSW`.
	fn enable_device_wake(&mut self, identity: &str, target: u64);
	/// The general-purpose event set for wake; false when the kernel refused it.
	fn set_wake_event(&mut self, number: u16) -> bool;
	fn clear_wake_event(&mut self, number: u16);
	/// A method by its absolute path with one integer, where the namespace has it - no failure where it does not.
	fn root_method(&mut self, path: &str, argument: u64);
	fn say(&mut self, line: &str);
}

/// What one `prepare` took, for its `wake` - or the next `prepare` - to give back.
#[derive(Default, Debug)]
pub struct Sleeper {
	armed: Vec<u16>,
	holders: Vec<String>,
}

impl Sleeper {
	pub fn prepare<P: Platform>(&mut self, platform: &mut P, state: State, wake_nodes: &[String]) {
		self.armed.clear();
		self.release(platform);
		for identity in wake_nodes {
			self.arm(platform, identity, state.target());
		}
		if state != State::Idle {
			platform.root_method(PTS, state.target());
		}
		platform.root_method(SST, if state == State::Disk { SST_HIBERNATING } else { SST_SLEEPING });
		platform.say(&format!("the platform is prepared for {}", state.name()));
	}

	pub fn wake<P: Platform>(&mut self, platform: &mut P, state: State) {
		platform.root_method(SST, SST_WAKING);
		if state != State::Idle {
			platform.root_method(WAK, state.target());
		}
		for number in core::mem::take(&mut self.armed) {
			platform.clear_wake_event(number);
		}
		self.release(platform);
		platform.root_method(SST, SST_WORKING);
		platform.say("the platform is awake again");
	}

	/// The events set for wake, in the order they were set.
	pub fn armed(&self) -> &[u16] {
		&self.armed
	}

	fn arm<P: Platform>(&mut self, platform: &mut P, identity: &str, target: u64) {
		let (number, deepest, resources) = match platform.prw(identity) {
			Prw::Absent => return platform.say(&format!("{identity} is not in the namespace - its wake is not armed")),
			Prw::None => return platform.say(&format!("{identity} has no _PRW - it cannot wake the machine")),
			Prw::BlockDevice => return platform.say(&format!("{identity}'s _PRW names an event on a GPE block device - its wake is not armed")),
			Prw::Event { number, deepest, resources } => (number, deepest, resources),
		};
		if deepest < target {
			return platform.say(&format!("{identity} wakes from S{deepest} at the deepest - not from S{target}; its wake is not armed"));
		}
		// THE POWER ITS WAKE NEEDS, on before its wake is enabled and held until `wake` lets it go.
		if !resources.is_empty() {
			let holder = format!("wake {identity}");
			platform.hold_power(&holder, &resources);
			self.holders.push(holder);
		}
		platform.enable_device_wake(identity, target);
		if platform.set_wake_event(number) {
			self.armed.push(number);
			platform.say(&format!("{identity} armed to wake the machine on general-purpose event {number:#04x}"));
		} else {
			platform.say(&format!("general-purpose event {number:#04x} could not be set for {identity}'s wake"));
		}
	}

	fn release<P: Platform>(&mut self, platform: &mut P) {
		for holder in core::mem::take(&mut self.holders) {
			platform.release_power(&holder);
		}
	}
}

/// A SLEEP TYPE from `\_S3`, `\_S4` or `\_S5`'s package, its elements' integers in order (`None` for one that is not an
/// integer): SLP_TYPa and SLP_TYPb, three bits each, the second defaulting to the first. `None` where the first is
/// not an integer - the package names no sleep type.
pub fn sleep_type(elements: &[Option<u64>]) -> Option<(u8, u8)> {
	let a = (*elements.first()?)?;
	let b = elements.get(1).copied().flatten().unwrap_or(a);
	Some(((a & 7) as u8, (b & 7) as u8))
}

#[cfg(test)]
mod tests;
