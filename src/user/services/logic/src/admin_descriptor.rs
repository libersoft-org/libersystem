//! WHAT A PERSON IS ASKED TO CONFIRM, and how it is shown.
//!
//! THE DESCRIPTOR IS THE OPERATION. The executor that owns a target freezes it at preparation - its own
//! identity and epoch, the live target instance and generation, the parameters, and the payload's length
//! and SHA-256 - and that is all a confirmation authorizes. It is checked here against what was asked for:
//! an executor that answered another action, other parameters or another payload length has not prepared
//! the operation the requester asked for, and nothing it answered is shown.
//!
//! WHAT AN ACTION DOES IS SAID IN THE SERVICE'S WORDS. An action whose parameters say what it will do - a BMC's log
//! erased, its chassis stopped - gets an `Operation:` line rendered from the action and its parameters by a fixed
//! table, and parameters the table cannot render are refused before anything reaches an executor.
//!
//! THE LABEL IS THE REQUESTER'S WORDS, NEVER IDENTITY. It is bounded to 128 UTF-8 bytes and every control
//! and bidirectional-formatting character is escaped, so it cannot move the cursor, recolour the screen or
//! reorder the lines around it; and it is rendered inside the service's own template, marked as unverified,
//! below the canonical operation.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const VERSION: u32 = 1;
pub const MAX_LABEL: usize = 128;
pub const MAX_DESCRIPTOR: usize = 4096;
pub const MAX_PARAMETERS: usize = 256;
pub const MAX_NAME: usize = 64;
pub const DIGEST: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
	FirmwareDownload,
	ProbeWrite,
	BmcSelClear,
	BmcChassisControl,
}

impl Action {
	pub fn name(self) -> &'static str {
		match self {
			Action::FirmwareDownload => "firmware-download",
			Action::ProbeWrite => "probe-write",
			Action::BmcSelClear => "bmc-sel-clear",
			Action::BmcChassisControl => "bmc-chassis-control",
		}
	}

	pub fn wire(self) -> u8 {
		match self {
			Action::FirmwareDownload => 1,
			Action::ProbeWrite => 2,
			Action::BmcSelClear => 3,
			Action::BmcChassisControl => 4,
		}
	}
}

/// What a requester asked for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Asked {
	pub action: Action,
	pub target: String,
	pub parameters: Vec<u8>,
	pub payload_length: u32,
	pub label: String,
}

/// The operation as the executor froze it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Descriptor {
	pub version: u32,
	pub action: Action,
	pub executor: String,
	pub executor_epoch: u64,
	pub target: String,
	pub target_generation: u64,
	pub parameters: Vec<u8>,
	pub payload_length: u32,
	pub payload_digest: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Past a bound: the label, the parameters, the encoded descriptor.
	Bounds,
	/// Not the operation that was asked for.
	Mismatch,
	/// A version, a digest or a name this service does not accept.
	Malformed,
	/// Parameters the operation table cannot say in words: a person would be asked to confirm what they cannot read.
	Unrenderable,
}

/// THE OPERATION TABLE: what an action with these parameters does, in the service's words - `None` for an action
/// the table has no row for, whose parameters are shown as bytes alone, and `Err` for parameters its row cannot
/// render.
pub fn operation(action: Action, parameters: &[u8]) -> Result<Option<String>, Refusal> {
	const PAYLOAD: &str = "the payload is the target's name";
	match action {
		Action::FirmwareDownload | Action::ProbeWrite => Ok(None),
		Action::BmcSelClear => match parameters {
			[low, high] => Ok(Some(format!("erase all {} records of the BMC's event log; an event that arrives first cancels it; {PAYLOAD}", u16::from_le_bytes([*low, *high])))),
			_ => Err(Refusal::Unrenderable),
		},
		Action::BmcChassisControl => {
			const HARD: &str = "the machine stops at once: no service is stopped and nothing is flushed";
			let said = match parameters {
				[0] => format!("POWER DOWN - {HARD}"),
				[2] => format!("POWER CYCLE - {HARD}"),
				[3] => format!("HARD RESET - {HARD}"),
				[5] => String::from("SOFT SHUTDOWN: the BMC presses the power button"),
				_ => return Err(Refusal::Unrenderable),
			};
			Ok(Some(format!("{said}; {PAYLOAD}")))
		}
	}
}

/// A request's own bounds, before anything is sent to an executor.
pub fn check_asked(asked: &Asked) -> Result<(), Refusal> {
	if asked.label.len() > MAX_LABEL || asked.parameters.len() > MAX_PARAMETERS || asked.target.len() > MAX_NAME || asked.target.is_empty() {
		return Err(Refusal::Bounds);
	}
	operation(asked.action, &asked.parameters).map(|_| ())
}

/// The executor's answer against the request: the same action, parameters and payload length, a version
/// this service speaks, a whole digest, named executor and target, and an encoding within 4096 bytes.
pub fn check_prepared(asked: &Asked, descriptor: &Descriptor, encoded: usize) -> Result<(), Refusal> {
	if encoded > MAX_DESCRIPTOR {
		return Err(Refusal::Bounds);
	}
	if descriptor.version != VERSION || descriptor.payload_digest.len() != DIGEST || descriptor.executor.is_empty() || descriptor.target.is_empty() || descriptor.executor.len() > MAX_NAME || descriptor.target.len() > MAX_NAME {
		return Err(Refusal::Malformed);
	}
	if descriptor.action != asked.action || descriptor.parameters != asked.parameters || descriptor.payload_length != asked.payload_length {
		return Err(Refusal::Mismatch);
	}
	Ok(())
}

// A character that would change how the text around it is read: C0 and C1 controls, DEL, and the Unicode
// bidirectional embeddings, overrides, isolates and marks.
fn hazardous(character: char) -> bool {
	character.is_control() || matches!(character, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{061c}')
}

/// The label as it may be shown: at most 128 bytes of it, cut at a character, with every hazardous character
/// written out as `\u{..}` and the backslash itself doubled so an escape cannot be forged.
pub fn escape_label(label: &str) -> String {
	let mut cut = label.len().min(MAX_LABEL);
	while !label.is_char_boundary(cut) {
		cut -= 1;
	}
	let mut out = String::new();
	for character in label[..cut].chars() {
		if character == '\\' {
			out.push_str("\\\\");
		} else if hazardous(character) {
			out.push_str(&format!("\\u{{{:x}}}", character as u32));
		} else {
			out.push(character);
		}
	}
	out
}

fn hex(bytes: &[u8]) -> String {
	let mut out = String::new();
	for byte in bytes {
		out.push_str(&format!("{byte:02x}"));
	}
	out
}

/// THE SERVICE'S OWN TEMPLATE, line by line: who asked (as PermissionManager named it), the canonical
/// operation, and last the requester's label, escaped and marked as its own words.
pub fn prompt(descriptor: &Descriptor, requester: &str, label: &str) -> Vec<String> {
	let shown: usize = descriptor.parameters.len().min(32);
	let mut parameters = hex(&descriptor.parameters[..shown]);
	if descriptor.parameters.len() > shown {
		parameters.push_str("...");
	}
	let mut lines = alloc::vec![String::from("ADMINISTRATIVE CONFIRMATION"), String::new(), format!("Requested by:  {requester}"), format!("Action:        {}", descriptor.action.name())];
	// THE OPERATION IN WORDS, where the table has a row: a descriptor that got this far has parameters it renders.
	if let Ok(Some(said)) = operation(descriptor.action, &descriptor.parameters) {
		lines.push(format!("Operation:     {said}"));
	}
	lines.extend([
		format!("Executor:      {} (epoch {})", descriptor.executor, descriptor.executor_epoch),
		format!("Target:        {} (generation {})", descriptor.target, descriptor.target_generation),
		format!("Parameters:    {} bytes {}", descriptor.parameters.len(), parameters),
		format!("Payload:       {} bytes, SHA-256 {}", descriptor.payload_length, hex(&descriptor.payload_digest)),
		format!("Label:         \"{}\" (the requester's words, not verified)", escape_label(label)),
		String::new(),
		String::from("Enter approves this one operation. Escape declines."),
	]);
	lines
}

/// The protected screen with nothing waiting.
pub fn idle_prompt() -> Vec<String> {
	alloc::vec![String::from("ADMINISTRATIVE CONFIRMATION"), String::new(), String::from("No request is waiting."), String::new(), String::from("Escape returns.")]
}

/// Where the template's values begin: `prompt` names each field in this many columns.
pub const VALUE_COLUMN: usize = 15;

/// THE PROMPT AS ROWS OF AT MOST `columns` CHARACTERS, WITH NOTHING CUT. A line that does not fit continues
/// on the next row, indented to where the template's values begin - so a digest or a label is shown whole,
/// and a continuation cannot pass for a field of its own. `None` when a continuation would have no room: a
/// screen that narrow cannot show the operation, and showing part of it is what this exists to prevent.
pub fn wrap(lines: &[String], columns: usize) -> Option<Vec<String>> {
	if columns <= VALUE_COLUMN {
		return None;
	}
	let mut rows = Vec::new();
	for line in lines {
		let mut row = String::new();
		let mut used = 0;
		for character in line.chars() {
			if used == columns {
				rows.push(core::mem::take(&mut row));
				row.push_str(&" ".repeat(VALUE_COLUMN));
				used = VALUE_COLUMN;
			}
			row.push(character);
			used += 1;
		}
		rows.push(row);
	}
	Some(rows)
}

#[cfg(test)]
mod tests;
