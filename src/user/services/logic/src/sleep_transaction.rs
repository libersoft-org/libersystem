//! THE SUSPEND TRANSACTION'S ORDER - what ServiceManager does next and what it owes, as a function of the steps taken and
//! the ones that failed. ServiceManager sends the requests and reads the answers; every decision about the order is here.
//!
//! FORWARD: check, announce, freeze, flush, the drivers, the platform, the entry. EACH STEP TAKEN OWES ITS UNDO - the
//! announcement the resume notice, the freeze the thaw, the flush the release of the held writes, the drivers their
//! resume, the platform its wake - and THE UNDO RUNS IN REVERSE, but for the thaw, which goes LAST whenever it is owed
//! and the resume notice just before it. A step that fails ends the forward order and starts the undo; so does the
//! orderly sequence, which ends the transaction at its next step. A failed undo step does not stop the ones after it:
//! the resume goes on. The drivers' step UNWINDS ITSELF before it answers a failure, so a failed drivers' step owes no
//! resume.
//!
//! HIBERNATION goes on past the entry, which takes the snapshot and answers twice: in the machine that ran on, the
//! bindings the image is written through are resumed, the image is written, and the machine goes off (`DiskOff`) - or,
//! for a HYBRID sleep, those bindings are suspended again and the machine suspends to RAM (`RamEnter`); in the machine
//! RESTORED from the image (`restored`), nothing more goes forward and the undo runs as after any sleep. A step of the
//! image that fails ends the forward order, and the undo brings the machine back running: the machine that could not be
//! hibernated is not left half-stopped. AN IMAGE WRITTEN OWES ITS DISCARD: a machine that runs on after it - a hybrid
//! sleep's S3 resumed, an S3 or an S4 that did not happen - must never find it again at a boot, where it would restore
//! memory older than the filesystem that moved on. So the discard runs once the drivers are back and BEFORE the held
//! writes are released: nothing reaches the volume while a valid image of an older state is on the disk.

use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
	Check,
	Announce,
	Freeze,
	Flush,
	Drivers,
	Platform,
	Enter,
	// Hibernation's, after the snapshot.
	ImageDrivers,
	ImageWrite,
	DiskOff,
	ImageSuspend,
	RamEnter,
	Discard,
	// The undoing ones.
	Wake,
	DriversResume,
	Release,
	Resumed,
	Thaw,
}

impl Action {
	pub fn undoing(self) -> bool {
		matches!(self, Action::Wake | Action::DriversResume | Action::Discard | Action::Release | Action::Resumed | Action::Thaw)
	}

	/// What a completed step owes the undo.
	fn owes(self) -> Option<Action> {
		match self {
			Action::Announce => Some(Action::Resumed),
			Action::Freeze => Some(Action::Thaw),
			Action::Flush => Some(Action::Release),
			Action::Drivers => Some(Action::DriversResume),
			Action::Platform => Some(Action::Wake),
			Action::ImageWrite => Some(Action::Discard),
			_ => None,
		}
	}
}

pub const FORWARD: [Action; 7] = [Action::Check, Action::Announce, Action::Freeze, Action::Flush, Action::Drivers, Action::Platform, Action::Enter];
/// HIBERNATION'S STEPS AFTER THE SNAPSHOT: the image written, then the machine off.
pub const HIBERNATE: [Action; 3] = [Action::ImageDrivers, Action::ImageWrite, Action::DiskOff];
/// A HYBRID SLEEP'S: the image written, its bindings suspended again, S3, and the image discarded once S3 resumed.
pub const HYBRID: [Action; 4] = [Action::ImageDrivers, Action::ImageWrite, Action::ImageSuspend, Action::RamEnter];

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
	forward: Vec<Action>,
	// What was taken, to be undone - the last pushed is undone first, and the thaw is kept at the bottom.
	undo: Vec<Action>,
	failed: bool,
}

impl Default for Plan {
	fn default() -> Plan {
		Plan::new()
	}
}

impl Plan {
	pub fn new() -> Plan {
		Plan { forward: FORWARD.to_vec(), undo: Vec::new(), failed: false }
	}

	/// A HIBERNATION'S PLAN - `hybrid` for the image kept through a suspend to RAM.
	pub fn hibernation(hybrid: bool) -> Plan {
		let mut forward = FORWARD.to_vec();
		forward.extend_from_slice(if hybrid { &HYBRID[..] } else { &HIBERNATE[..] });
		Plan { forward, undo: Vec::new(), failed: false }
	}

	/// THE ENTRY'S SECOND ANSWER: this is the machine restored from the image - nothing of the image's steps goes forward,
	/// and the undo resumes it as after any sleep.
	pub fn restored(&mut self) {
		self.forward.clear();
	}

	/// The next step: forward while nothing failed and nothing ended the transaction, the undo otherwise; `None` once
	/// the transaction is over.
	pub fn next(&mut self) -> Option<Action> {
		if !self.failed && !self.forward.is_empty() {
			return Some(self.forward.remove(0));
		}
		self.undo.pop()
	}

	/// A step completed - whatever its answer - and what it owes goes onto the undo: the thaw at the bottom, the resume
	/// notice right above it.
	pub fn completed(&mut self, action: Action) {
		let Some(owed) = action.owes() else { return };
		match owed {
			Action::Thaw => self.undo.insert(0, Action::Thaw),
			Action::Resumed if self.undo.first() == Some(&Action::Thaw) => self.undo.insert(1, Action::Resumed),
			// THE DISCARD, AFTER THE DRIVERS' RESUME AND BEFORE THE RELEASE: just above the release in what is owed.
			Action::Discard => {
				let at = self.undo.iter().position(|owed| *owed == Action::Release).map_or(self.undo.len(), |release| release + 1);
				self.undo.insert(at, Action::Discard);
			}
			other => self.undo.push(other),
		}
	}

	/// A forward step failed: nothing more goes forward. A failed undo step changes nothing - the resume goes on.
	pub fn failed(&mut self, action: Action) {
		if !action.undoing() {
			self.failed = true;
		}
	}

	/// THE ORDERLY SEQUENCE: the step in flight finishes, and no later step starts.
	pub fn end(&mut self) {
		self.forward.clear();
	}

	pub fn has_failed(&self) -> bool {
		self.failed
	}

	/// What the undo still owes, next first - for the record and the log.
	pub fn owed(&self) -> impl Iterator<Item = Action> + '_ {
		self.undo.iter().rev().copied()
	}
}

#[cfg(test)]
mod tests;
