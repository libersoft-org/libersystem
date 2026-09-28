//! FIELDS: the bits of a region, an index/data pair or a buffer a named field stands for, read and written in
//! units of its access width - a partial unit merged by the field's update rule - through the host.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::convert;
use crate::error::Error;
use crate::host::{Access as HostAccess, PciAddress, protocol};
use crate::interp::Machine;
use crate::name::Seg;
use crate::namespace::NodeId;
use crate::object::{Access, BufferField, Field, FieldKind, Object, Region, Space, Update};
use crate::resource;

/// Bits, least significant first, as bytes.
fn bits_of(object: &Object, bit_length: u64, int64: bool) -> Result<Vec<u8>, Error> {
	let mut bytes = match object {
		Object::Integer(value) => value.to_le_bytes().to_vec(),
		Object::Buffer(bytes) => bytes.clone(),
		Object::String(text) => text.as_bytes().to_vec(),
		other => return Err(Error::Type(format!("a {} written to a field", other.type_name()))),
	};
	let _ = int64;
	bytes.resize(bit_length.div_ceil(8) as usize, 0);
	Ok(bytes)
}

fn get_bit(bytes: &[u8], bit: u64) -> bool {
	bytes.get((bit / 8) as usize).is_some_and(|byte| byte >> (bit % 8) & 1 != 0)
}

fn set_bit(bytes: &mut [u8], bit: u64, on: bool) {
	if let Some(byte) = bytes.get_mut((bit / 8) as usize) {
		if on {
			*byte |= 1 << (bit % 8);
		} else {
			*byte &= !(1 << (bit % 8));
		}
	}
}

/// The value a field of `bit_length` bits reads as: an integer if it fits the integer width, else a buffer.
fn as_value(bytes: Vec<u8>, bit_length: u64, int64: bool) -> Object {
	let width = if int64 { 64 } else { 32 };
	if bit_length <= width {
		let mut value: u64 = 0;
		for (at, byte) in bytes.iter().take(8).enumerate() {
			value |= (*byte as u64) << (8 * at);
		}
		Object::Integer(value)
	} else {
		Object::Buffer(bytes)
	}
}

/// The unit a field is reached in, in bits.
fn unit_bits(access: Access, space: Space, bit_offset: u64, bit_length: u64) -> u64 {
	let natural = match access {
		Access::Byte | Access::Buffer => 8,
		Access::Word => 16,
		Access::DWord => 32,
		Access::QWord => 64,
		// ANY: the smallest unit that holds the whole field aligned, else bytes.
		Access::Any => [8u64, 16, 32, 64].into_iter().find(|unit| bit_offset / unit == (bit_offset + bit_length.max(1) - 1) / unit).unwrap_or(8),
	};
	match space {
		// The embedded controller and CMOS are byte-wide, and a port is at most a dword.
		Space::EmbeddedControl | Space::SystemCmos => 8,
		Space::SystemIo => natural.min(32),
		_ => natural,
	}
}

impl Machine<'_> {
	fn region_of(&self, node: NodeId) -> Result<Region, Error> {
		let object = self.aml.ns.object(node).ok_or_else(|| Error::NotFound(format!("node {node}")))?;
		let region = match &*object.borrow() {
			Object::Region(region) => region.clone(),
			other => return Err(Error::Type(format!("a field over a {}, not a region", other.type_name()))),
		};
		Ok(region)
	}

	fn field_node(&self, node: NodeId) -> Result<Field, Error> {
		let object = self.aml.ns.object(node).ok_or_else(|| Error::NotFound(format!("node {node}")))?;
		let field = match &*object.borrow() {
			Object::Field(field) => field.clone(),
			other => return Err(Error::Type(format!("an index or bank field that is a {}", other.type_name()))),
		};
		Ok(field)
	}

	/// THE PCI FUNCTION a `PCI_Config` region reaches: its device's `_ADR`, the host bridge's `_SEG` and `_BBN`, and
	/// the secondary bus of every bridge between them, read from the bridges themselves.
	fn pci_address(&mut self, device: NodeId) -> Result<PciAddress, Error> {
		let adr = self.named_integer(device, b"_ADR")?.unwrap_or(0);
		let mut chain: Vec<NodeId> = Vec::new();
		let mut at = self.aml.ns.parent(device);
		let mut root: Option<NodeId> = None;
		while let Some(node) = at {
			if self.is_host_bridge(node)? {
				root = Some(node);
				break;
			}
			chain.push(node);
			at = self.aml.ns.parent(node);
		}
		let (segment, mut bus) = match root {
			Some(root) => (self.named_integer(root, b"_SEG")?.unwrap_or(0) as u16, self.named_integer(root, b"_BBN")?.unwrap_or(0) as u8),
			// A region directly under a host bridge's own node: the bridge itself is function 0.0 of its bus.
			None => (0, 0),
		};
		// THE BRIDGES BETWEEN, from the root down: each one's secondary bus is where its children are.
		for bridge in chain.iter().rev() {
			let bridge_adr = self.named_integer(*bridge, b"_ADR")?.unwrap_or(0);
			let address = PciAddress { segment, bus, device: (bridge_adr >> 16) as u8, function: bridge_adr as u8 };
			bus = self.host.read(HostAccess { space: Space::PciConfig, address: 0x19, width: 8, pci: Some(address) })? as u8;
		}
		if root.is_none() && self.is_host_bridge(device)? {
			let segment = self.named_integer(device, b"_SEG")?.unwrap_or(0) as u16;
			let bus = self.named_integer(device, b"_BBN")?.unwrap_or(0) as u8;
			return Ok(PciAddress { segment, bus, device: (adr >> 16) as u8, function: adr as u8 });
		}
		Ok(PciAddress { segment, bus, device: (adr >> 16) as u8, function: adr as u8 })
	}

	/// A child object's integer value - `_ADR`, `_SEG`, `_BBN` - evaluated if it is a method; `None` when absent.
	pub(crate) fn named_integer(&mut self, node: NodeId, name: &[u8; 4]) -> Result<Option<u64>, Error> {
		let Some(child) = self.aml.ns.child(node, Seg(*name)) else { return Ok(None) };
		let value = self.evaluate_node(child, Vec::new())?;
		match value {
			Some(value) => {
				let value = self.deref_value(&value)?;
				Ok(Some(convert::to_integer(&value, self.aml.int64)?))
			}
			None => Ok(None),
		}
	}

	fn is_host_bridge(&mut self, node: NodeId) -> Result<bool, Error> {
		for name in [b"_HID", b"_CID"] {
			let Some(child) = self.aml.ns.child(node, Seg(*name)) else { continue };
			let value = self.evaluate_node(child, Vec::new())?;
			let Some(value) = value else { continue };
			let value = self.deref_value(&value)?;
			let ids: Vec<Object> = match value {
				Object::Package(elements) => elements.iter().map(|element| element.borrow().clone()).collect(),
				other => alloc::vec![other],
			};
			for id in ids {
				let text = match id {
					Object::Integer(eisa) => resource::eisa_id(eisa as u32),
					Object::String(text) => text,
					_ => continue,
				};
				if text == "PNP0A03" || text == "PNP0A08" {
					return Ok(true);
				}
			}
		}
		Ok(false)
	}

	fn region_read(&mut self, region: &Region, pci: Option<PciAddress>, byte_offset: u64, width: u64) -> Result<u64, Error> {
		if byte_offset + width / 8 > region.length && region.space != Space::PciConfig {
			return Err(Error::Refused(format!("a field reaches past its {}-byte {} region", region.length, region.space.name())));
		}
		self.region_space_allowed(region.space)?;
		self.announce(region);
		let value = self.host.read(HostAccess { space: region.space, address: region.offset + byte_offset, width: width as u8, pci })?;
		Ok(value)
	}

	fn region_write(&mut self, region: &Region, pci: Option<PciAddress>, byte_offset: u64, width: u64, value: u64) -> Result<(), Error> {
		if byte_offset + width / 8 > region.length && region.space != Space::PciConfig {
			return Err(Error::Refused(format!("a field reaches past its {}-byte {} region", region.length, region.space.name())));
		}
		self.region_space_allowed(region.space)?;
		self.announce(region);
		self.host.write(HostAccess { space: region.space, address: region.offset + byte_offset, width: width as u8, pci }, value)?;
		Ok(())
	}

	/// Tell the host which region the next access is in - see `Host::region`.
	pub(crate) fn announce(&mut self, region: &Region) {
		if matches!(region.space, Space::SystemMemory | Space::SystemIo) {
			let path = self.aml.ns.path(region.parent);
			self.host.region(&path, region.space, region.offset, region.length);
		}
	}

	/// THE SPACES THIS INTERPRETER REACHES; any other is refused at access and reported.
	fn region_space_allowed(&self, space: Space) -> Result<(), Error> {
		match space {
			Space::SystemMemory | Space::SystemIo | Space::PciConfig | Space::EmbeddedControl | Space::SystemCmos | Space::GeneralPurposeIo | Space::GenericSerialBus => Ok(()),
			other => Err(Error::Refused(format!("a {} region is not served", other.name()))),
		}
	}

	// ------------------------------------------------------------------ reading

	pub(crate) fn read_field(&mut self, field: &Field) -> Result<Object, Error> {
		if field.lock {
			self.lock_global()?;
		}
		let result = self.read_field_unlocked(field);
		if field.lock {
			self.host.global_lock(false);
		}
		result
	}

	fn read_field_unlocked(&mut self, field: &Field) -> Result<Object, Error> {
		let int64 = self.aml.int64;
		let region_node = match &field.kind {
			FieldKind::Region(region) => Some(*region),
			FieldKind::Bank { region, bank, value } => {
				let bank_field = self.field_node(*bank)?;
				self.write_field(&bank_field, &Object::Integer(*value))?;
				Some(*region)
			}
			FieldKind::Index { .. } => None,
		};
		if let Some(region_node) = region_node {
			let region = self.region_of(region_node)?;
			match region.space {
				Space::GeneralPurposeIo => return self.gpio_read(field),
				Space::GenericSerialBus => return self.serial_bus(field, None),
				_ => {}
			}
			let pci = if region.space == Space::PciConfig { Some(self.pci_address(region.parent)?) } else { None };
			let unit = unit_bits(field.access, region.space, field.bit_offset, field.bit_length);
			let mut out = alloc::vec![0u8; field.bit_length.div_ceil(8) as usize];
			let first = field.bit_offset / unit;
			let last = (field.bit_offset + field.bit_length.max(1) - 1) / unit;
			for index in first..=last {
				let datum = self.region_read(&region, pci, index * unit / 8, unit)?;
				let unit_start = index * unit;
				for bit in 0..unit {
					let absolute = unit_start + bit;
					if absolute < field.bit_offset || absolute >= field.bit_offset + field.bit_length {
						continue;
					}
					set_bit(&mut out, absolute - field.bit_offset, datum >> bit & 1 != 0);
				}
			}
			return Ok(as_value(out, field.bit_length, int64));
		}
		// AN INDEX FIELD: each unit's offset written to the index field, the unit read through the data field.
		let FieldKind::Index { index, data } = field.kind else { unreachable!() };
		let index_field = self.field_node(index)?;
		let data_field = self.field_node(data)?;
		let unit = unit_bits(field.access, Space::SystemMemory, field.bit_offset, field.bit_length).min(data_field.bit_length.max(8));
		let mut out = alloc::vec![0u8; field.bit_length.div_ceil(8) as usize];
		let first = field.bit_offset / unit;
		let last = (field.bit_offset + field.bit_length.max(1) - 1) / unit;
		for position in first..=last {
			self.write_field(&index_field, &Object::Integer(position * unit / 8))?;
			let datum = convert::to_integer(&self.read_field(&data_field)?, true)?;
			for bit in 0..unit {
				let absolute = position * unit + bit;
				if absolute < field.bit_offset || absolute >= field.bit_offset + field.bit_length {
					continue;
				}
				set_bit(&mut out, absolute - field.bit_offset, datum >> bit & 1 != 0);
			}
		}
		Ok(as_value(out, field.bit_length, int64))
	}

	fn lock_global(&mut self) -> Result<(), Error> {
		for _ in 0..1000 {
			if self.host.global_lock(true) {
				return Ok(());
			}
			self.host.sleep(1);
		}
		Err(Error::Refused(String::from("the firmware global lock was not given up within a second")))
	}

	pub(crate) fn read_buffer_field(&mut self, field: &BufferField) -> Result<Object, Error> {
		let bytes = match &*field.buffer.borrow() {
			Object::Buffer(bytes) => bytes.clone(),
			other => return Err(Error::Type(format!("a buffer field over a {}", other.type_name()))),
		};
		if field.bit_offset + field.bit_length > bytes.len() as u64 * 8 {
			return Err(Error::Index(field.bit_offset + field.bit_length));
		}
		let mut out = alloc::vec![0u8; field.bit_length.div_ceil(8) as usize];
		for bit in 0..field.bit_length {
			set_bit(&mut out, bit, get_bit(&bytes, field.bit_offset + bit));
		}
		Ok(as_value(out, field.bit_length, self.aml.int64))
	}

	// ------------------------------------------------------------------ writing

	pub(crate) fn write_field(&mut self, field: &Field, value: &Object) -> Result<(), Error> {
		if field.lock {
			self.lock_global()?;
		}
		let result = self.write_field_unlocked(field, value);
		if field.lock {
			self.host.global_lock(false);
		}
		result
	}

	fn write_field_unlocked(&mut self, field: &Field, value: &Object) -> Result<(), Error> {
		let region_node = match &field.kind {
			FieldKind::Region(region) => Some(*region),
			FieldKind::Bank { region, bank, value: bank_value } => {
				let bank_field = self.field_node(*bank)?;
				self.write_field(&bank_field, &Object::Integer(*bank_value))?;
				Some(*region)
			}
			FieldKind::Index { .. } => None,
		};
		if let Some(region_node) = region_node {
			let region = self.region_of(region_node)?;
			match region.space {
				Space::GeneralPurposeIo => return Err(Error::Refused(String::from("a write to a GeneralPurposeIo field: GPIO output lines are not driven"))),
				Space::GenericSerialBus => {
					self.serial_bus(field, Some(value))?;
					return Ok(());
				}
				_ => {}
			}
			let pci = if region.space == Space::PciConfig { Some(self.pci_address(region.parent)?) } else { None };
			let bits = bits_of(value, field.bit_length, self.aml.int64)?;
			let unit = unit_bits(field.access, region.space, field.bit_offset, field.bit_length);
			let first = field.bit_offset / unit;
			let last = (field.bit_offset + field.bit_length.max(1) - 1) / unit;
			for index in first..=last {
				let unit_start = index * unit;
				let mut mask: u64 = 0;
				let mut datum: u64 = 0;
				for bit in 0..unit {
					let absolute = unit_start + bit;
					if absolute < field.bit_offset || absolute >= field.bit_offset + field.bit_length {
						continue;
					}
					mask |= 1 << bit;
					if get_bit(&bits, absolute - field.bit_offset) {
						datum |= 1 << bit;
					}
				}
				let full = if unit == 64 { u64::MAX } else { (1u64 << unit) - 1 };
				if mask != full {
					let rest = match field.update {
						Update::Preserve => self.region_read(&region, pci, unit_start / 8, unit)?,
						Update::WriteAsOnes => full,
						Update::WriteAsZeros => 0,
					};
					datum |= rest & !mask & full;
				}
				self.region_write(&region, pci, unit_start / 8, unit, datum)?;
			}
			return Ok(());
		}
		let FieldKind::Index { index, data } = field.kind else { unreachable!() };
		let index_field = self.field_node(index)?;
		let data_field = self.field_node(data)?;
		let bits = bits_of(value, field.bit_length, self.aml.int64)?;
		let unit = unit_bits(field.access, Space::SystemMemory, field.bit_offset, field.bit_length).min(data_field.bit_length.max(8));
		let first = field.bit_offset / unit;
		let last = (field.bit_offset + field.bit_length.max(1) - 1) / unit;
		for position in first..=last {
			let unit_start = position * unit;
			let mut mask: u64 = 0;
			let mut datum: u64 = 0;
			for bit in 0..unit {
				let absolute = unit_start + bit;
				if absolute < field.bit_offset || absolute >= field.bit_offset + field.bit_length {
					continue;
				}
				mask |= 1 << bit;
				if get_bit(&bits, absolute - field.bit_offset) {
					datum |= 1 << bit;
				}
			}
			let full = if unit == 64 { u64::MAX } else { (1u64 << unit) - 1 };
			self.write_field(&index_field, &Object::Integer(unit_start / 8))?;
			if mask != full {
				let rest = match field.update {
					Update::Preserve => convert::to_integer(&self.read_field(&data_field)?, true)?,
					Update::WriteAsOnes => full,
					Update::WriteAsZeros => 0,
				};
				datum |= rest & !mask & full;
				self.write_field(&index_field, &Object::Integer(unit_start / 8))?;
			}
			self.write_field(&data_field, &Object::Integer(datum))?;
		}
		Ok(())
	}

	pub(crate) fn write_buffer_field(&mut self, field: &BufferField, value: &Object) -> Result<(), Error> {
		let bits = bits_of(value, field.bit_length, self.aml.int64)?;
		let mut held = field.buffer.borrow_mut();
		let Object::Buffer(bytes) = &mut *held else { return Err(Error::Type(String::from("a buffer field over something that is no longer a buffer"))) };
		if field.bit_offset + field.bit_length > bytes.len() as u64 * 8 {
			return Err(Error::Index(field.bit_offset + field.bit_length));
		}
		for bit in 0..field.bit_length {
			set_bit(bytes, field.bit_offset + bit, get_bit(&bits, bit));
		}
		Ok(())
	}

	// ------------------------------------------------------------------ connections

	/// A GeneralPurposeIo field READ: one input line per bit, the lines the field's connection lists in order.
	fn gpio_read(&mut self, field: &Field) -> Result<Object, Error> {
		let connection = field.connection.clone().ok_or_else(|| Error::Refused(String::from("a GeneralPurposeIo field with no Connection")))?;
		let pins = resource::gpio_pins(&connection.0).ok_or_else(|| Error::Refused(String::from("a GeneralPurposeIo field whose Connection is not a GPIO descriptor")))?;
		let mut out = alloc::vec![0u8; field.bit_length.div_ceil(8) as usize];
		for bit in 0..field.bit_length {
			let pin = *pins.get((field.bit_offset + bit) as usize).ok_or_else(|| Error::Refused(String::from("a GeneralPurposeIo field past its connection's pins")))?;
			let high = self.host.gpio_read(&connection.0, pin)?;
			set_bit(&mut out, bit, high);
		}
		Ok(as_value(out, field.bit_length, self.aml.int64))
	}

	/// A GenericSerialBus field: one transaction in the field's protocol at the field's command, answering the data
	/// buffer - status, length, then the bytes - that a read returns and a write stores back.
	fn serial_bus(&mut self, field: &Field, value: Option<&Object>) -> Result<Object, Error> {
		let connection = field.connection.clone().ok_or_else(|| Error::Refused(String::from("a GenericSerialBus field with no Connection")))?;
		let command = field.bit_offset / 8;
		let protocol = if field.attrib_length != 0 && field.attrib == 0 { protocol::BYTES } else { field.attrib };
		let write: Option<Vec<u8>> = match value {
			None => None,
			Some(Object::Buffer(bytes)) => {
				// THE DATA BUFFER: a status byte, a length byte, then the data.
				let length = bytes.get(1).copied().unwrap_or(0) as usize;
				let data = bytes.get(2..).unwrap_or(&[]);
				let take = match protocol {
					protocol::BYTE | protocol::SEND_RECEIVE => 1,
					protocol::WORD | protocol::PROCESS_CALL => 2,
					protocol::BYTES | protocol::RAW_BYTES | protocol::RAW_PROCESS_BYTES => field.attrib_length as usize,
					_ => length,
				};
				Some(data[..take.min(data.len())].to_vec())
			}
			Some(Object::Integer(value)) => Some(value.to_le_bytes()[..2].to_vec()),
			Some(other) => return Err(Error::Type(format!("a {} written to a GenericSerialBus field", other.type_name()))),
		};
		let answered = self.host.serial_bus(&connection.0, protocol, field.attrib_length, command, write.as_deref())?;
		let mut out = alloc::vec![0u8, answered.len() as u8];
		out.extend_from_slice(&answered);
		Ok(Object::Buffer(out))
	}
}
