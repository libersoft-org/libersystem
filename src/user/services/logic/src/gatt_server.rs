//! THE GATT SERVER THIS HOST HOLDS: what every LE device must expose - the GAP service with its name and appearance,
//! and the GATT service with Service Changed - answered to a peer acting as a client. Part of the LE Audio work adds
//! its services to the same table; nothing outside the stack adds to it.
//!
//! A TABLE, READ BY HANDLE. Every request is answered from it: the primary services by group type, the
//! characteristic declarations by type, the descriptors by information, a value by handle; a write is accepted only
//! where the table says so - a client configuration descriptor - and a request this server does not speak is
//! answered "not supported" rather than ignored, so a peer is never left waiting.

use crate::att::op;
use alloc::vec::Vec;

#[cfg(test)]
mod tests;

/// The attribute types the table uses.
pub mod uuid {
	pub const PRIMARY_SERVICE: u16 = 0x2800;
	pub const CHARACTERISTIC: u16 = 0x2803;
	pub const CLIENT_CONFIGURATION: u16 = 0x2902;
	pub const GAP: u16 = 0x1800;
	pub const GATT: u16 = 0x1801;
	pub const DEVICE_NAME: u16 = 0x2a00;
	pub const APPEARANCE: u16 = 0x2a01;
	pub const SERVICE_CHANGED: u16 = 0x2a05;
}

/// The error codes this server answers with.
pub mod error {
	pub const INVALID_HANDLE: u8 = 0x01;
	pub const READ_NOT_PERMITTED: u8 = 0x02;
	pub const WRITE_NOT_PERMITTED: u8 = 0x03;
	pub const REQUEST_NOT_SUPPORTED: u8 = 0x06;
	pub const ATTRIBUTE_NOT_FOUND: u8 = 0x0a;
	pub const INVALID_LENGTH: u8 = 0x0d;
}

/// Characteristic properties, as a declaration carries them.
pub const PROPERTY_READ: u8 = 0x02;
pub const PROPERTY_INDICATE: u8 = 0x20;

/// The appearance this host states: a generic computer.
pub const APPEARANCE_COMPUTER: u16 = 0x0080;

/// The MTU this server agrees to: LE's bound.
pub const MTU: usize = crate::bt_bounds::ATT_MTU;

/// One attribute: its handle, its type, its value, and whether a client may write it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Attribute {
	pub handle: u16,
	pub kind: u16,
	pub value: Vec<u8>,
	pub writable: bool,
	pub readable: bool,
}

/// The table, and the MTU agreed on the link it serves.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Server {
	attributes: Vec<Attribute>,
	mtu: usize,
}

fn attribute(handle: u16, kind: u16, value: Vec<u8>) -> Attribute {
	Attribute { handle, kind, value, writable: false, readable: true }
}

fn declaration(handle: u16, properties: u8, kind: u16) -> Attribute {
	let mut value = alloc::vec![properties];
	value.extend_from_slice(&(handle + 1).to_le_bytes());
	value.extend_from_slice(&kind.to_le_bytes());
	attribute(handle, uuid::CHARACTERISTIC, value)
}

impl Server {
	/// GAP with `name` and the appearance, and GATT with Service Changed and its configuration.
	pub fn new(name: &[u8]) -> Server {
		let mut configuration = attribute(0x0009, uuid::CLIENT_CONFIGURATION, alloc::vec![0, 0]);
		configuration.writable = true;
		let attributes = alloc::vec![
			attribute(0x0001, uuid::PRIMARY_SERVICE, uuid::GAP.to_le_bytes().to_vec()),
			declaration(0x0002, PROPERTY_READ, uuid::DEVICE_NAME),
			attribute(0x0003, uuid::DEVICE_NAME, name.to_vec()),
			declaration(0x0004, PROPERTY_READ, uuid::APPEARANCE),
			attribute(0x0005, uuid::APPEARANCE, APPEARANCE_COMPUTER.to_le_bytes().to_vec()),
			attribute(0x0006, uuid::PRIMARY_SERVICE, uuid::GATT.to_le_bytes().to_vec()),
			declaration(0x0007, PROPERTY_INDICATE, uuid::SERVICE_CHANGED),
			{
				let mut value = attribute(0x0008, uuid::SERVICE_CHANGED, alloc::vec![0x01, 0x00, 0xff, 0xff]);
				value.readable = false;
				value
			},
			configuration,
		];
		Server { attributes, mtu: crate::att::DEFAULT_MTU }
	}

	/// A service the stack adds at its end: its declaration, then `attributes` after it, handles given in order.
	/// Answers the handle the service begins at.
	pub fn add_service(&mut self, service: u16, characteristics: &[(u16, u8, Vec<u8>, bool)]) -> u16 {
		let mut handle = self.attributes.last().map_or(1, |last| last.handle + 1);
		let start = handle;
		self.attributes.push(attribute(handle, uuid::PRIMARY_SERVICE, service.to_le_bytes().to_vec()));
		for (kind, properties, value, writable) in characteristics {
			handle += 1;
			self.attributes.push(declaration(handle, *properties, *kind));
			handle += 1;
			let mut value_attribute = attribute(handle, *kind, value.clone());
			value_attribute.writable = *writable;
			self.attributes.push(value_attribute);
			if properties & 0x30 != 0 {
				handle += 1;
				let mut configuration = attribute(handle, uuid::CLIENT_CONFIGURATION, alloc::vec![0, 0]);
				configuration.writable = true;
				self.attributes.push(configuration);
			}
		}
		start
	}

	/// The value at `handle`, where there is one.
	pub fn value(&self, handle: u16) -> Option<&[u8]> {
		self.attributes.iter().find(|attribute| attribute.handle == handle).map(|attribute| attribute.value.as_slice())
	}

	// The end of the group a service declaration begins: the handle before the next service's, or the table's last.
	fn group_end(&self, start: u16) -> u16 {
		self.attributes.iter().filter(|attribute| attribute.kind == uuid::PRIMARY_SERVICE && attribute.handle > start).map(|attribute| attribute.handle - 1).next().unwrap_or_else(|| self.attributes.last().map_or(start, |last| last.handle))
	}

	fn error(request: u8, handle: u16, code: u8) -> Vec<u8> {
		let mut out = alloc::vec![op::ERROR_RESPONSE, request];
		out.extend_from_slice(&handle.to_le_bytes());
		out.push(code);
		out
	}

	/// ONE REQUEST FROM A PEER, answered - or `None` for a command, which has no answer.
	pub fn answer(&mut self, pdu: &[u8]) -> Option<Vec<u8>> {
		let (&request, body) = pdu.split_first()?;
		let range = || -> Option<(u16, u16)> { Some((u16::from_le_bytes([*body.first()?, *body.get(1)?]), u16::from_le_bytes([*body.get(2)?, *body.get(3)?]))) };
		match request {
			op::EXCHANGE_MTU_REQUEST if body.len() == 2 => {
				let asked = u16::from_le_bytes([body[0], body[1]]) as usize;
				self.mtu = asked.clamp(crate::att::DEFAULT_MTU, MTU);
				let mut out = alloc::vec![op::EXCHANGE_MTU_RESPONSE];
				out.extend_from_slice(&(MTU as u16).to_le_bytes());
				Some(out)
			}
			op::READ_BY_GROUP_TYPE_REQUEST if body.len() == 6 => {
				let (from, to) = range()?;
				let kind = u16::from_le_bytes([body[4], body[5]]);
				if from == 0 || from > to {
					return Some(Self::error(request, from, error::INVALID_HANDLE));
				}
				if kind != uuid::PRIMARY_SERVICE {
					return Some(Self::error(request, from, error::ATTRIBUTE_NOT_FOUND));
				}
				let mut out = alloc::vec![op::READ_BY_GROUP_TYPE_RESPONSE, 6];
				for service in self.attributes.iter().filter(|attribute| attribute.kind == uuid::PRIMARY_SERVICE && attribute.handle >= from && attribute.handle <= to) {
					if out.len() + 6 > self.mtu {
						break;
					}
					out.extend_from_slice(&service.handle.to_le_bytes());
					out.extend_from_slice(&self.group_end(service.handle).to_le_bytes());
					out.extend_from_slice(&service.value);
				}
				Some(if out.len() == 2 { Self::error(request, from, error::ATTRIBUTE_NOT_FOUND) } else { out })
			}
			op::READ_BY_TYPE_REQUEST if body.len() == 6 => {
				let (from, to) = range()?;
				let kind = u16::from_le_bytes([body[4], body[5]]);
				if from == 0 || from > to {
					return Some(Self::error(request, from, error::INVALID_HANDLE));
				}
				let found: Vec<&Attribute> = self.attributes.iter().filter(|attribute| attribute.kind == kind && attribute.handle >= from && attribute.handle <= to).collect();
				let Some(first) = found.first() else { return Some(Self::error(request, from, error::ATTRIBUTE_NOT_FOUND)) };
				if !first.readable {
					return Some(Self::error(request, first.handle, error::READ_NOT_PERMITTED));
				}
				// ONE LENGTH A RESPONSE: the entries that share the first's.
				let size = (first.value.len() + 2).min(self.mtu - 2).min(255);
				let mut out = alloc::vec![op::READ_BY_TYPE_RESPONSE, size as u8];
				for attribute in found.iter().filter(|attribute| attribute.value.len() + 2 == size || (attribute.handle == first.handle)) {
					if out.len() + size > self.mtu || !attribute.readable {
						break;
					}
					out.extend_from_slice(&attribute.handle.to_le_bytes());
					out.extend_from_slice(&attribute.value[..size - 2]);
				}
				Some(out)
			}
			op::FIND_INFORMATION_REQUEST if body.len() == 4 => {
				let (from, to) = range()?;
				if from == 0 || from > to {
					return Some(Self::error(request, from, error::INVALID_HANDLE));
				}
				let mut out = alloc::vec![op::FIND_INFORMATION_RESPONSE, 1];
				for attribute in self.attributes.iter().filter(|attribute| attribute.handle >= from && attribute.handle <= to) {
					if out.len() + 4 > self.mtu {
						break;
					}
					out.extend_from_slice(&attribute.handle.to_le_bytes());
					out.extend_from_slice(&attribute.kind.to_le_bytes());
				}
				Some(if out.len() == 2 { Self::error(request, from, error::ATTRIBUTE_NOT_FOUND) } else { out })
			}
			op::READ_REQUEST | op::READ_BLOB_REQUEST if body.len() >= 2 => {
				let handle = u16::from_le_bytes([body[0], body[1]]);
				let offset = if request == op::READ_BLOB_REQUEST && body.len() == 4 { u16::from_le_bytes([body[2], body[3]]) as usize } else { 0 };
				let Some(attribute) = self.attributes.iter().find(|attribute| attribute.handle == handle) else { return Some(Self::error(request, handle, error::INVALID_HANDLE)) };
				if !attribute.readable {
					return Some(Self::error(request, handle, error::READ_NOT_PERMITTED));
				}
				if offset > attribute.value.len() {
					return Some(Self::error(request, handle, 0x07));
				}
				let mut out = alloc::vec![if request == op::READ_REQUEST { op::READ_RESPONSE } else { op::READ_BLOB_RESPONSE }];
				let end = (offset + self.mtu - 1).min(attribute.value.len());
				out.extend_from_slice(&attribute.value[offset..end]);
				Some(out)
			}
			op::WRITE_REQUEST | op::WRITE_COMMAND if body.len() >= 2 => {
				let handle = u16::from_le_bytes([body[0], body[1]]);
				let value = &body[2..];
				let answer = |code: Option<u8>| if request == op::WRITE_COMMAND { None } else { Some(code.map_or_else(|| alloc::vec![op::WRITE_RESPONSE], |code| Self::error(request, handle, code))) };
				let Some(attribute) = self.attributes.iter_mut().find(|attribute| attribute.handle == handle) else { return answer(Some(error::INVALID_HANDLE)) };
				if !attribute.writable {
					return answer(Some(error::WRITE_NOT_PERMITTED));
				}
				if attribute.kind == uuid::CLIENT_CONFIGURATION && value.len() != 2 {
					return answer(Some(error::INVALID_LENGTH));
				}
				attribute.value = value.to_vec();
				answer(None)
			}
			// A command this server does not speak has no answer; a request is told so.
			_ if request & 0x40 != 0 => None,
			_ => Some(Self::error(request, 0, error::REQUEST_NOT_SUPPORTED)),
		}
	}
}
