//! FINDING A BOOT MOUSE ON A PEER, AND TURNING ITS REPORTS ON.
//!
//! Five steps, each driven by the peer's answer to the one before: find the human-interface service,
//! find its protocol-mode and boot-mouse-report characteristics, find the report's configuration
//! descriptor, put the device in boot mode, and turn notifications on. Every answer is walked by
//! `att`, which refuses the three shapes that make a discovery loop hang or bind the wrong attribute
//! - a handle that does not advance, a handle outside the range asked about, and a list whose entries
//! do not divide it.
//!
//! WHAT IS NOT FOUND IS A TYPED ANSWER. A peer with no human-interface service, or one whose service
//! lacks the boot report, is not a mouse this slice can drive - and saying so is the milestone's own
//! rule: typed unsupported results for unsupported modes rather than a claim of full HOGP
//! conformance.

use crate::att::{ATTRIBUTE_NOT_FOUND, Answer, Entries, FIRST_HANDLE, LAST_HANDLE, Refusal, answer, next_range, op};
use crate::hogp::NOTIFICATIONS_ON;

/// The UUIDs this procedure looks for, all sixteen-bit.
pub mod uuid {
	pub const PRIMARY_SERVICE: u16 = 0x2800;
	pub const CHARACTERISTIC: u16 = 0x2803;
	pub const CLIENT_CONFIGURATION: u16 = 0x2902;
	pub const HUMAN_INTERFACE: u16 = 0x1812;
	pub const PROTOCOL_MODE: u16 = 0x2a4e;
	pub const BOOT_MOUSE_INPUT: u16 = 0x2a33;
}

/// Why this peer is not a boot mouse this slice can drive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unsupported {
	/// No human-interface service at all.
	NoHumanInterface,
	/// The service has no boot mouse report - a keyboard, or a device that speaks report mode only.
	NoBootMouseReport,
	/// The boot report cannot be configured to notify.
	NoConfiguration,
	/// There is no protocol-mode characteristic to select boot mode with.
	NoProtocolMode,
}

/// What to do next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Next {
	/// Send this ATT request and feed its answer back.
	Request(alloc::vec::Vec<u8>),
	/// Send this ATT command. A command has no answer; the procedure continues with `Next` below.
	Command(alloc::vec::Vec<u8>, alloc::boxed::Box<Next>),
	/// Notifications from this handle are boot mouse reports.
	Ready { report: u16 },
	/// This peer is not a boot mouse this slice can drive.
	Unsupported(Unsupported),
	/// The peer's answer was not well formed.
	Failed(Refusal),
	/// The peer answered an error this procedure cannot continue past.
	ServerError(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
	Services { from: u16 },
	Characteristics { from: u16 },
	Descriptors { from: u16, to: u16 },
	Configure,
	Done,
}

/// The discovery, as state.
#[derive(Clone, Copy, Debug)]
pub struct Discovery {
	phase: Phase,
	mtu: usize,
	service: Option<(u16, u16)>,
	protocol_mode: Option<u16>,
	report: Option<u16>,
	/// The declaration that follows the report's, which bounds where its descriptors may be.
	after_report: Option<u16>,
	configuration: Option<u16>,
}

fn read_by_group_type(from: u16, to: u16, kind: u16) -> alloc::vec::Vec<u8> {
	let mut out = alloc::vec![op::READ_BY_GROUP_TYPE_REQUEST];
	out.extend_from_slice(&from.to_le_bytes());
	out.extend_from_slice(&to.to_le_bytes());
	out.extend_from_slice(&kind.to_le_bytes());
	out
}

fn read_by_type(from: u16, to: u16, kind: u16) -> alloc::vec::Vec<u8> {
	let mut out = alloc::vec![op::READ_BY_TYPE_REQUEST];
	out.extend_from_slice(&from.to_le_bytes());
	out.extend_from_slice(&to.to_le_bytes());
	out.extend_from_slice(&kind.to_le_bytes());
	out
}

fn find_information(from: u16, to: u16) -> alloc::vec::Vec<u8> {
	let mut out = alloc::vec![op::FIND_INFORMATION_REQUEST];
	out.extend_from_slice(&from.to_le_bytes());
	out.extend_from_slice(&to.to_le_bytes());
	out
}

fn write(opcode: u8, handle: u16, value: &[u8]) -> alloc::vec::Vec<u8> {
	let mut out = alloc::vec![opcode];
	out.extend_from_slice(&handle.to_le_bytes());
	out.extend_from_slice(value);
	out
}

impl Discovery {
	/// Begin, at the MTU the two ends agreed on. The first request asks for the whole handle range.
	pub fn start(mtu: usize) -> (Discovery, Next) {
		let discovery = Discovery { phase: Phase::Services { from: FIRST_HANDLE }, mtu, service: None, protocol_mode: None, report: None, after_report: None, configuration: None };
		(discovery, Next::Request(read_by_group_type(FIRST_HANDLE, LAST_HANDLE, uuid::PRIMARY_SERVICE)))
	}

	/// The report handle, once the procedure has finished.
	pub fn report(&self) -> Option<u16> {
		if self.phase == Phase::Done { self.report } else { None }
	}

	/// Feed the answer to the last request.
	pub fn on_answer(&mut self, pdu: &[u8]) -> Next {
		match self.phase {
			Phase::Services { from } => self.services(pdu, from),
			Phase::Characteristics { from } => self.characteristics(pdu, from),
			Phase::Descriptors { from, to } => self.descriptors(pdu, from, to),
			Phase::Configure => self.configured(pdu),
			Phase::Done => Next::Failed(Refusal::Unexpected { got: pdu.first().copied().unwrap_or(0), want: 0 }),
		}
	}

	fn services(&mut self, pdu: &[u8], from: u16) -> Next {
		let body = match answer(pdu, op::READ_BY_GROUP_TYPE_RESPONSE, self.mtu) {
			Ok(Answer::Response(body)) => body,
			Ok(Answer::Error(error)) if error.code == ATTRIBUTE_NOT_FOUND => return self.after_services(),
			Ok(Answer::Error(error)) => return Next::ServerError(error.code),
			Err(refusal) => return Next::Failed(refusal),
		};
		let mut entries = match Entries::new(body, from, LAST_HANDLE) {
			Ok(entries) => entries,
			Err(refusal) => return Next::Failed(refusal),
		};
		// THE NEXT REQUEST BEGINS PAST THE END OF THE LAST GROUP, not past the last group's START:
		// asking again from the start's successor would re-read the attributes inside that service as
		// though they could be services. So the end of every group is tracked as the walk goes.
		let mut last_end: Option<u16> = None;
		for entry in entries.by_ref() {
			// A group entry's value is the group's end handle and then the service UUID.
			if entry.value.len() < 2 {
				return Next::Failed(Refusal::Truncated { len: entry.value.len(), needed: 2 });
			}
			let end = u16::from_le_bytes([entry.value[0], entry.value[1]]);
			// A GROUP ENDING BEFORE IT BEGINS, or before the one ahead of it ended, is a table whose
			// next request would ask about attributes already read.
			if end < entry.handle || last_end.is_some_and(|previous| entry.handle <= previous) {
				return Next::Failed(Refusal::HandleDidNotAdvance { got: end, from: entry.handle });
			}
			last_end = Some(end);
			// Only the sixteen-bit UUID form is this slice's: a 128-bit service UUID is a vendor one.
			if entry.value.len() == 4 && u16::from_le_bytes([entry.value[2], entry.value[3]]) == uuid::HUMAN_INTERFACE {
				self.service = Some((entry.handle, end));
				return self.after_services();
			}
		}
		if let Some(fault) = entries.fault() {
			return Next::Failed(fault);
		}
		match last_end.and_then(|last| next_range(last, LAST_HANDLE)) {
			Some((next, _)) => {
				self.phase = Phase::Services { from: next };
				Next::Request(read_by_group_type(next, LAST_HANDLE, uuid::PRIMARY_SERVICE))
			}
			None => self.after_services(),
		}
	}

	fn after_services(&mut self) -> Next {
		let Some((start, end)) = self.service else { return Next::Unsupported(Unsupported::NoHumanInterface) };
		self.phase = Phase::Characteristics { from: start };
		Next::Request(read_by_type(start, end, uuid::CHARACTERISTIC))
	}

	fn characteristics(&mut self, pdu: &[u8], from: u16) -> Next {
		let (_, end) = self.service.expect("characteristics are asked for inside a found service");
		let body = match answer(pdu, op::READ_BY_TYPE_RESPONSE, self.mtu) {
			Ok(Answer::Response(body)) => body,
			Ok(Answer::Error(error)) if error.code == ATTRIBUTE_NOT_FOUND => return self.after_characteristics(),
			Ok(Answer::Error(error)) => return Next::ServerError(error.code),
			Err(refusal) => return Next::Failed(refusal),
		};
		let mut entries = match Entries::new(body, from, end) {
			Ok(entries) => entries,
			Err(refusal) => return Next::Failed(refusal),
		};
		for entry in entries.by_ref() {
			// A declaration's value is its properties, its value handle and its UUID.
			if entry.value.len() != 5 {
				continue;
			}
			let value_handle = u16::from_le_bytes([entry.value[1], entry.value[2]]);
			let kind = u16::from_le_bytes([entry.value[3], entry.value[4]]);
			// A VALUE HANDLE AT OR BEFORE ITS OWN DECLARATION, OR OUTSIDE THE SERVICE, is a peer
			// describing an attribute somewhere this discovery did not look - and writing to it would
			// be writing to whatever is actually there.
			if value_handle <= entry.handle || value_handle > end {
				return Next::Failed(Refusal::HandleOutOfRange { got: value_handle, from: entry.handle, to: end });
			}
			if self.report.is_some() && self.after_report.is_none() {
				self.after_report = Some(entry.handle);
			}
			match kind {
				uuid::PROTOCOL_MODE => self.protocol_mode = Some(value_handle),
				uuid::BOOT_MOUSE_INPUT => self.report = Some(value_handle),
				_ => {}
			}
		}
		if let Some(fault) = entries.fault() {
			return Next::Failed(fault);
		}
		match entries.last_handle().and_then(|last| next_range(last, end)) {
			Some((next, _)) => {
				self.phase = Phase::Characteristics { from: next };
				Next::Request(read_by_type(next, end, uuid::CHARACTERISTIC))
			}
			None => self.after_characteristics(),
		}
	}

	fn after_characteristics(&mut self) -> Next {
		let Some(report) = self.report else { return Next::Unsupported(Unsupported::NoBootMouseReport) };
		if self.protocol_mode.is_none() {
			return Next::Unsupported(Unsupported::NoProtocolMode);
		}
		let (_, end) = self.service.expect("inside a found service");
		// THE REPORT'S DESCRIPTORS ARE BETWEEN ITS VALUE AND THE NEXT DECLARATION, and a
		// configuration descriptor past that boundary belongs to a different characteristic -
		// enabling it would turn on some other characteristic's notifications and leave this one's off.
		let to = self.after_report.map_or(end, |next| next.saturating_sub(1));
		if report >= to {
			return Next::Unsupported(Unsupported::NoConfiguration);
		}
		self.phase = Phase::Descriptors { from: report + 1, to };
		Next::Request(find_information(report + 1, to))
	}

	fn descriptors(&mut self, pdu: &[u8], from: u16, to: u16) -> Next {
		let body = match answer(pdu, op::FIND_INFORMATION_RESPONSE, self.mtu) {
			Ok(Answer::Response(body)) => body,
			Ok(Answer::Error(error)) if error.code == ATTRIBUTE_NOT_FOUND => return self.after_descriptors(),
			Ok(Answer::Error(error)) => return Next::ServerError(error.code),
			Err(refusal) => return Next::Failed(refusal),
		};
		let Some((&format, rest)) = body.split_first() else { return Next::Failed(Refusal::Truncated { len: 0, needed: 1 }) };
		let entry = match format {
			1 => 4,
			2 => 18,
			_ => return Next::Failed(Refusal::Ragged { entry: 0, bytes: rest.len() }),
		};
		let mut entries = match Entries::with_entry(rest, entry, from, to) {
			Ok(entries) => entries,
			Err(refusal) => return Next::Failed(refusal),
		};
		for found in entries.by_ref() {
			if found.value.len() == 2 && u16::from_le_bytes([found.value[0], found.value[1]]) == uuid::CLIENT_CONFIGURATION {
				self.configuration = Some(found.handle);
				return self.after_descriptors();
			}
		}
		if let Some(fault) = entries.fault() {
			return Next::Failed(fault);
		}
		match entries.last_handle().and_then(|last| next_range(last, to)) {
			Some((next, _)) => {
				self.phase = Phase::Descriptors { from: next, to };
				Next::Request(find_information(next, to))
			}
			None => self.after_descriptors(),
		}
	}

	fn after_descriptors(&mut self) -> Next {
		let (Some(configuration), Some(mode)) = (self.configuration, self.protocol_mode) else { return Next::Unsupported(Unsupported::NoConfiguration) };
		self.phase = Phase::Configure;
		// BOOT MODE FIRST, AND AS A COMMAND: the protocol-mode characteristic is written without a
		// response, and the reports it governs are not turned on until it has been sent - a device
		// left in report mode would notify in a layout the boot decoder refuses.
		let enable = write(op::WRITE_REQUEST, configuration, &NOTIFICATIONS_ON);
		Next::Command(write(op::WRITE_COMMAND, mode, &[crate::hogp::Mode::Boot.byte()]), alloc::boxed::Box::new(Next::Request(enable)))
	}

	fn configured(&mut self, pdu: &[u8]) -> Next {
		match answer(pdu, op::WRITE_RESPONSE, self.mtu) {
			Ok(Answer::Response(_)) => {
				self.phase = Phase::Done;
				Next::Ready { report: self.report.expect("configured after a report was found") }
			}
			Ok(Answer::Error(error)) => Next::ServerError(error.code),
			Err(refusal) => Next::Failed(refusal),
		}
	}
}

#[cfg(test)]
mod tests;
