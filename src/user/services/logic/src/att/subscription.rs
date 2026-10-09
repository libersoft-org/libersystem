//! Subscribe to one value, without crossing into the next characteristic or service.

use super::{ATTRIBUTE_NOT_FOUND, Answer, Entries, MAX_DISCOVERED, answer, next_range, op};
use alloc::vec::Vec;
use base_proto::generated::liber::base::v1::Error;

#[derive(Clone, Copy, Debug)]
enum Phase {
	Properties { from: u16 },
	Boundary,
	Descriptors { from: u16, to: u16 },
	Write { cccd: u16 },
	Done,
}

/// Kept on the link's existing ATT queue. No table grows with the peer's replies.
#[derive(Clone, Debug)]
pub struct Subscription {
	value: u16,
	deadline: u64,
	end: u16,
	setting: u16,
	remaining: usize,
	phase: Phase,
}

impl Subscription {
	pub fn new(start: u16, end: u16, value: u16, deadline: u64) -> Result<Self, Error> {
		// A declaration precedes the value, and its CCCD must follow it.
		if start == 0 || value <= start || value >= end {
			return Err(Error::Unsupported);
		}
		Ok(Self { value, deadline, end, setting: 0, remaining: MAX_DISCOVERED, phase: Phase::Properties { from: start + 1 } })
	}

	pub fn deadline(&self) -> u64 {
		self.deadline
	}

	pub fn expired(&self, now: u64) -> bool {
		now >= self.deadline
	}

	pub fn value(&self) -> u16 {
		self.value
	}

	pub fn request(&self) -> Vec<u8> {
		let (opcode, fields): (u8, &[u16]) = match &self.phase {
			Phase::Properties { from } => (op::READ_BY_TYPE_REQUEST, &[*from, self.value, 0x2803]),
			Phase::Boundary => (op::READ_BY_TYPE_REQUEST, &[self.value + 1, self.end, 0x2803]),
			Phase::Descriptors { from, to } => (op::FIND_INFORMATION_REQUEST, &[*from, *to]),
			Phase::Write { cccd } => (op::WRITE_REQUEST, &[*cccd, self.setting]),
			Phase::Done => return Vec::new(),
		};
		let mut out = alloc::vec![opcode];
		for field in fields {
			out.extend_from_slice(&field.to_le_bytes());
		}
		out
	}

	/// `true` means the peer acknowledged this CCCD write; `false` queues `request()`.
	pub fn on_response(&mut self, pdu: &[u8], now: u64) -> Result<bool, Error> {
		let outcome = if self.expired(now) { Err(Error::TimedOut) } else { self.advance(pdu) };
		if outcome.is_err() || outcome == Ok(true) {
			self.phase = Phase::Done;
		}
		outcome
	}

	fn advance(&mut self, pdu: &[u8]) -> Result<bool, Error> {
		let (sent, expected, from, to) = match self.phase {
			Phase::Properties { from } => (op::READ_BY_TYPE_REQUEST, op::READ_BY_TYPE_RESPONSE, from, self.value),
			Phase::Boundary => (op::READ_BY_TYPE_REQUEST, op::READ_BY_TYPE_RESPONSE, self.value + 1, self.end),
			Phase::Descriptors { from, to } => (op::FIND_INFORMATION_REQUEST, op::FIND_INFORMATION_RESPONSE, from, to),
			Phase::Write { cccd } => (op::WRITE_REQUEST, op::WRITE_RESPONSE, cccd, cccd),
			Phase::Done => return Err(Error::Invalid),
		};
		let body = match answer(pdu, expected, crate::bt_bounds::ATT_MTU).map_err(|_| Error::Io)? {
			Answer::Error(error) => {
				if pdu.len() != 5 || error.request != sent || error.handle < from || error.handle > to {
					return Err(Error::Io);
				}
				if error.code == ATTRIBUTE_NOT_FOUND {
					return match self.phase {
						Phase::Boundary => self.descriptors(self.end),
						Phase::Properties { .. } | Phase::Descriptors { .. } => Err(Error::Unsupported),
						_ => Err(Error::Io),
					};
				}
				return Err(match error.code {
					0x02 | 0x03 | 0x05 | 0x08 | 0x0c | 0x0f => Error::Denied,
					_ => Error::Io,
				});
			}
			Answer::Response(body) => body,
		};
		match self.phase {
			Phase::Properties { .. } | Phase::Boundary => {
				if !matches!(body.first(), Some(7 | 21)) {
					return Err(Error::Io);
				}
				let mut entries = Entries::new(body, from, to).map_err(|_| Error::Io)?;
				let mut last_value = None;
				let mut first = None;
				let mut setting = None;
				for entry in entries.by_ref() {
					let value = u16::from_le_bytes([entry.value[1], entry.value[2]]);
					if value <= entry.handle || value > self.end || last_value.is_some_and(|last| entry.handle <= last) {
						return Err(Error::Io);
					}
					if matches!(self.phase, Phase::Properties { .. }) {
						self.consume()?;
					}
					first.get_or_insert(entry.handle);
					last_value = Some(value);
					if value == self.value {
						setting = Some(if entry.value[0] & 0x10 != 0 {
							1
						} else if entry.value[0] & 0x20 != 0 {
							2
						} else {
							0
						});
					}
				}
				if entries.fault().is_some() {
					return Err(Error::Io);
				}
				if matches!(self.phase, Phase::Boundary) {
					return self.descriptors(first.ok_or(Error::Io)? - 1);
				}
				match setting {
					Some(setting @ (1 | 2)) => {
						self.setting = setting;
						self.phase = Phase::Boundary;
					}
					Some(_) => return Err(Error::Unsupported),
					None => {
						let (from, _) = next_range(last_value.ok_or(Error::Io)?, self.value).ok_or(Error::Unsupported)?;
						if self.remaining == 0 {
							return Err(Error::Exhausted);
						}
						self.phase = Phase::Properties { from };
					}
				}
				Ok(false)
			}
			Phase::Descriptors { .. } => {
				let width = match body.first() {
					Some(1) => 4,
					Some(2) => 18,
					_ => return Err(Error::Io),
				};
				let mut entries = Entries::with_entry(&body[1..], width, from, to).map_err(|_| Error::Io)?;
				let mut cccd = None;
				for entry in entries.by_ref() {
					self.consume()?;
					if entry.value == [0x02, 0x29] || entry.value == [0xfb, 0x34, 0x9b, 0x5f, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x02, 0x29, 0x00, 0x00] {
						if cccd.replace(entry.handle).is_some() {
							return Err(Error::Io);
						}
					}
				}
				if entries.fault().is_some() {
					return Err(Error::Io);
				}
				if let Some(cccd) = cccd {
					self.phase = Phase::Write { cccd };
				} else {
					let (from, _) = next_range(entries.last_handle().ok_or(Error::Io)?, to).ok_or(Error::Unsupported)?;
					if self.remaining == 0 {
						return Err(Error::Exhausted);
					}
					self.phase = Phase::Descriptors { from, to };
				}
				Ok(false)
			}
			Phase::Write { .. } if body.is_empty() => Ok(true),
			_ => Err(Error::Io),
		}
	}

	fn descriptors(&mut self, to: u16) -> Result<bool, Error> {
		if to <= self.value {
			return Err(Error::Unsupported);
		}
		self.remaining = MAX_DISCOVERED;
		self.phase = Phase::Descriptors { from: self.value + 1, to };
		Ok(false)
	}

	fn consume(&mut self) -> Result<(), Error> {
		self.remaining = self.remaining.checked_sub(1).ok_or(Error::Exhausted)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests;
