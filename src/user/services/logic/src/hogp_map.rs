//! HID OVER GATT WITH ARBITRARY REPORT MAPS: find the human-interface service, its report map, its input reports
//! and the report id each carries; select report protocol; and turn the input reports' notifications on.
//!
//! WHAT IT ADDS TO THE BOOT MOUSE WALK. `gatt_mouse` finds one boot report and nothing else; a keyboard, a mouse
//! with a wheel and extra buttons, a gamepad or a remote speaks report mode, and what its reports mean is in its
//! report map - a HID report descriptor, read here with Read Blob past one ATT packet and parsed by the same
//! report-descriptor parser a USB device's goes through. A report's id is not in its notification: the report
//! reference descriptor beside each report says it, and the map is told what each handle carries.
//!
//! The same discipline as `gatt_mouse`: every answer is walked by `att`, which refuses a handle that does not
//! advance, one outside the range asked about and a list that does not divide; what the peer lacks is a typed
//! answer; and every count is bounded - reports, the map's bytes - before anything is kept.

use crate::att::{ATTRIBUTE_NOT_FOUND, Answer, Entries, FIRST_HANDLE, LAST_HANDLE, Refusal, answer, next_range, op};
use crate::hogp::NOTIFICATIONS_ON;
use alloc::boxed::Box;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

/// The UUIDs this procedure looks for, all sixteen-bit.
pub mod uuid {
	pub const PRIMARY_SERVICE: u16 = 0x2800;
	pub const CHARACTERISTIC: u16 = 0x2803;
	pub const CLIENT_CONFIGURATION: u16 = 0x2902;
	pub const REPORT_REFERENCE: u16 = 0x2908;
	pub const HUMAN_INTERFACE: u16 = 0x1812;
	pub const REPORT_MAP: u16 = 0x2a4b;
	pub const REPORT: u16 = 0x2a4d;
	pub const PROTOCOL_MODE: u16 = 0x2a4e;
}

/// The longest report map HOGP allows.
pub const MAX_REPORT_MAP: usize = 512;
/// The reports one device may have, of every type.
pub const MAX_REPORTS: usize = 16;

/// A report reference's type: input, output or feature.
pub const REPORT_INPUT: u8 = 1;

/// The report protocol, as the protocol mode characteristic takes it.
const PROTOCOL_REPORT: u8 = 1;

/// Why this peer is not one this walk can drive in report mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unsupported {
	NoHumanInterface,
	/// A human-interface service without a report map: a boot-only device, which the boot walk drives.
	NoReportMap,
	/// No input report this host could turn on.
	NoInputReport,
	/// More reports than this host keeps, or a map longer than HOGP allows.
	TooLarge,
}

/// One input report: the handle its notifications come from, and the report id they carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Input {
	pub handle: u16,
	pub id: u8,
}

/// What a finished walk found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Map {
	pub descriptor: Vec<u8>,
	pub inputs: Vec<Input>,
}

impl Map {
	/// The report id a notification from `handle` carries, or `None` for a handle that is not an input report.
	pub fn id_of(&self, handle: u16) -> Option<u8> {
		self.inputs.iter().find(|input| input.handle == handle).map(|input| input.id)
	}
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Next {
	Request(Vec<u8>),
	/// An ATT command with no answer, and what follows it.
	Command(Vec<u8>, Box<Next>),
	Ready(Map),
	Unsupported(Unsupported),
	Failed(Refusal),
	ServerError(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
	Services { from: u16 },
	Characteristics { from: u16 },
	Descriptors { report: usize, from: u16, to: u16 },
	Reference { report: usize },
	ReportMap,
	Configure { report: usize },
	Done,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Report {
	value: u16,
	/// The declaration after this report's, which bounds where its descriptors may be.
	next_declaration: Option<u16>,
	reference: Option<u16>,
	configuration: Option<u16>,
	id: u8,
	kind: u8,
}

#[derive(Clone, Debug)]
pub struct Discovery {
	phase: Phase,
	mtu: usize,
	service: Option<(u16, u16)>,
	map_handle: Option<u16>,
	protocol_mode: Option<u16>,
	reports: Vec<Report>,
	descriptor: Vec<u8>,
}

fn request(opcode: u8, fields: &[u16]) -> Vec<u8> {
	let mut out = alloc::vec![opcode];
	for field in fields {
		out.extend_from_slice(&field.to_le_bytes());
	}
	out
}

fn write(opcode: u8, handle: u16, value: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![opcode];
	out.extend_from_slice(&handle.to_le_bytes());
	out.extend_from_slice(value);
	out
}

impl Discovery {
	pub fn start(mtu: usize) -> (Discovery, Next) {
		let discovery = Discovery { phase: Phase::Services { from: FIRST_HANDLE }, mtu, service: None, map_handle: None, protocol_mode: None, reports: Vec::new(), descriptor: Vec::new() };
		(discovery, Next::Request(request(op::READ_BY_GROUP_TYPE_REQUEST, &[FIRST_HANDLE, LAST_HANDLE, uuid::PRIMARY_SERVICE])))
	}

	/// Feed the answer to the last request.
	pub fn on_answer(&mut self, pdu: &[u8]) -> Next {
		match self.phase {
			Phase::Services { from } => self.services(pdu, from),
			Phase::Characteristics { from } => self.characteristics(pdu, from),
			Phase::Descriptors { report, from, to } => self.descriptors(pdu, report, from, to),
			Phase::Reference { report } => self.reference(pdu, report),
			Phase::ReportMap => self.report_map(pdu),
			Phase::Configure { report } => self.configured(pdu, report),
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
		let mut last_end: Option<u16> = None;
		for entry in entries.by_ref() {
			if entry.value.len() < 2 {
				return Next::Failed(Refusal::Truncated { len: entry.value.len(), needed: 2 });
			}
			let end = u16::from_le_bytes([entry.value[0], entry.value[1]]);
			if end < entry.handle || last_end.is_some_and(|previous| entry.handle <= previous) {
				return Next::Failed(Refusal::HandleDidNotAdvance { got: end, from: entry.handle });
			}
			last_end = Some(end);
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
				Next::Request(request(op::READ_BY_GROUP_TYPE_REQUEST, &[next, LAST_HANDLE, uuid::PRIMARY_SERVICE]))
			}
			None => self.after_services(),
		}
	}

	fn after_services(&mut self) -> Next {
		let Some((start, end)) = self.service else { return Next::Unsupported(Unsupported::NoHumanInterface) };
		self.phase = Phase::Characteristics { from: start };
		Next::Request(request(op::READ_BY_TYPE_REQUEST, &[start, end, uuid::CHARACTERISTIC]))
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
			if entry.value.len() != 5 {
				continue;
			}
			let value_handle = u16::from_le_bytes([entry.value[1], entry.value[2]]);
			let kind = u16::from_le_bytes([entry.value[3], entry.value[4]]);
			if value_handle <= entry.handle || value_handle > end {
				return Next::Failed(Refusal::HandleOutOfRange { got: value_handle, from: entry.handle, to: end });
			}
			// THIS DECLARATION BOUNDS THE ONE BEFORE IT: a report's descriptors end where the next declaration begins.
			if let Some(last) = self.reports.last_mut()
				&& last.next_declaration.is_none()
			{
				last.next_declaration = Some(entry.handle);
			}
			match kind {
				uuid::REPORT_MAP => self.map_handle = Some(value_handle),
				uuid::PROTOCOL_MODE => self.protocol_mode = Some(value_handle),
				uuid::REPORT => {
					if self.reports.len() >= MAX_REPORTS {
						return Next::Unsupported(Unsupported::TooLarge);
					}
					self.reports.push(Report { value: value_handle, next_declaration: None, reference: None, configuration: None, id: 0, kind: 0 });
				}
				_ => {}
			}
		}
		if let Some(fault) = entries.fault() {
			return Next::Failed(fault);
		}
		match entries.last_handle().and_then(|last| next_range(last, end)) {
			Some((next, _)) => {
				self.phase = Phase::Characteristics { from: next };
				Next::Request(request(op::READ_BY_TYPE_REQUEST, &[next, end, uuid::CHARACTERISTIC]))
			}
			None => self.after_characteristics(),
		}
	}

	fn after_characteristics(&mut self) -> Next {
		if self.map_handle.is_none() {
			return Next::Unsupported(Unsupported::NoReportMap);
		}
		if self.reports.is_empty() {
			return Next::Unsupported(Unsupported::NoInputReport);
		}
		self.descriptors_of(0)
	}

	// The descriptors of report `index`, between its value and the next declaration; then the next report's.
	fn descriptors_of(&mut self, index: usize) -> Next {
		let (_, end) = self.service.expect("inside a found service");
		let Some(report) = self.reports.get(index).copied() else { return self.reference_of(0) };
		let to = report.next_declaration.map_or(end, |next| next.saturating_sub(1));
		if report.value >= to {
			return self.descriptors_of(index + 1);
		}
		self.phase = Phase::Descriptors { report: index, from: report.value + 1, to };
		Next::Request(request(op::FIND_INFORMATION_REQUEST, &[report.value + 1, to]))
	}

	fn descriptors(&mut self, pdu: &[u8], index: usize, from: u16, to: u16) -> Next {
		let body = match answer(pdu, op::FIND_INFORMATION_RESPONSE, self.mtu) {
			Ok(Answer::Response(body)) => body,
			Ok(Answer::Error(error)) if error.code == ATTRIBUTE_NOT_FOUND => return self.descriptors_of(index + 1),
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
			if found.value.len() != 2 {
				continue;
			}
			match u16::from_le_bytes([found.value[0], found.value[1]]) {
				uuid::REPORT_REFERENCE => self.reports[index].reference = Some(found.handle),
				uuid::CLIENT_CONFIGURATION => self.reports[index].configuration = Some(found.handle),
				_ => {}
			}
		}
		if let Some(fault) = entries.fault() {
			return Next::Failed(fault);
		}
		match entries.last_handle().and_then(|last| next_range(last, to)) {
			Some((next, _)) => {
				self.phase = Phase::Descriptors { report: index, from: next, to };
				Next::Request(request(op::FIND_INFORMATION_REQUEST, &[next, to]))
			}
			None => self.descriptors_of(index + 1),
		}
	}

	// Read report `index`'s reference - its id and type; a report without one is an input report with id 0.
	fn reference_of(&mut self, index: usize) -> Next {
		let Some(report) = self.reports.get(index).copied() else {
			self.phase = Phase::ReportMap;
			let map = self.map_handle.expect("a map was found before references are read");
			return Next::Request(request(op::READ_REQUEST, &[map]));
		};
		match report.reference {
			Some(handle) => {
				self.phase = Phase::Reference { report: index };
				Next::Request(request(op::READ_REQUEST, &[handle]))
			}
			None => {
				self.reports[index].kind = REPORT_INPUT;
				self.reference_of(index + 1)
			}
		}
	}

	fn reference(&mut self, pdu: &[u8], index: usize) -> Next {
		let body = match answer(pdu, op::READ_RESPONSE, self.mtu) {
			Ok(Answer::Response(body)) => body,
			Ok(Answer::Error(error)) => return Next::ServerError(error.code),
			Err(refusal) => return Next::Failed(refusal),
		};
		if body.len() < 2 {
			return Next::Failed(Refusal::Truncated { len: body.len(), needed: 2 });
		}
		self.reports[index].id = body[0];
		self.reports[index].kind = body[1];
		self.reference_of(index + 1)
	}

	// THE REPORT MAP, read past one packet with Read Blob: a response as long as a packet can be is followed by
	// another read from where it stopped; a shorter one ends it.
	fn report_map(&mut self, pdu: &[u8]) -> Next {
		let want = if self.descriptor.is_empty() { op::READ_RESPONSE } else { op::READ_BLOB_RESPONSE };
		let body = match answer(pdu, want, self.mtu) {
			Ok(Answer::Response(body)) => body,
			// A blob read past the end is answered with "invalid offset": the map ended on a packet boundary.
			Ok(Answer::Error(error)) if error.code == INVALID_OFFSET && !self.descriptor.is_empty() => return self.after_map(),
			Ok(Answer::Error(error)) => return Next::ServerError(error.code),
			Err(refusal) => return Next::Failed(refusal),
		};
		if self.descriptor.len() + body.len() > MAX_REPORT_MAP {
			return Next::Unsupported(Unsupported::TooLarge);
		}
		self.descriptor.extend_from_slice(body);
		if body.len() == self.mtu - 1 {
			let map = self.map_handle.expect("the map is being read");
			return Next::Request(request(op::READ_BLOB_REQUEST, &[map, self.descriptor.len() as u16]));
		}
		self.after_map()
	}

	// REPORT PROTOCOL, then the input reports' notifications on, one write at a time.
	fn after_map(&mut self) -> Next {
		let first = self.configure_from(0);
		match self.protocol_mode {
			Some(handle) => Next::Command(write(op::WRITE_COMMAND, handle, &[PROTOCOL_REPORT]), Box::new(first)),
			None => first,
		}
	}

	fn configure_from(&mut self, index: usize) -> Next {
		let next = self.reports.iter().enumerate().skip(index).find(|(_, report)| report.kind == REPORT_INPUT && report.configuration.is_some()).map(|(at, report)| (at, report.configuration.unwrap_or(0)));
		match next {
			Some((at, configuration)) => {
				self.phase = Phase::Configure { report: at };
				Next::Request(write(op::WRITE_REQUEST, configuration, &NOTIFICATIONS_ON))
			}
			None => self.finish(),
		}
	}

	fn configured(&mut self, pdu: &[u8], index: usize) -> Next {
		match answer(pdu, op::WRITE_RESPONSE, self.mtu) {
			Ok(Answer::Response(_)) => self.configure_from(index + 1),
			Ok(Answer::Error(error)) => Next::ServerError(error.code),
			Err(refusal) => Next::Failed(refusal),
		}
	}

	fn finish(&mut self) -> Next {
		self.phase = Phase::Done;
		let inputs: Vec<Input> = self.reports.iter().filter(|report| report.kind == REPORT_INPUT && report.configuration.is_some()).map(|report| Input { handle: report.value, id: report.id }).collect();
		if inputs.is_empty() {
			return Next::Unsupported(Unsupported::NoInputReport);
		}
		Next::Ready(Map { descriptor: core::mem::take(&mut self.descriptor), inputs })
	}
}

/// The error a blob read past the attribute's end gets.
const INVALID_OFFSET: u8 = 0x07;
