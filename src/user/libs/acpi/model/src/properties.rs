//! A NODE'S PROPERTY BLOCK: `_UID` and `_DSD` - device properties and hierarchical data - in the record format every
//! row's block has (`abi::DEVICE_PROPERTY_*`), so a driver reads a namespace device's properties the way it reads a
//! tree node's. Each value is carried in the node channel's value encoding (`aml::wire`). Bounded by the block's
//! size: a block past it is cut at a record and said to be.

use alloc::vec::Vec;

use aml::dsd::{Properties, Value};
use aml::wire;

/// A block being written.
#[derive(Default)]
pub struct Block {
	pub bytes: Vec<u8>,
	/// A record did not fit, or a value did not encode.
	pub cut: bool,
}

impl Block {
	fn record(&mut self, kind: u8, depth: u8, name: &[u8], value: &[u8]) -> bool {
		let padded = (value.len() + 3) & !3;
		let need = 8 + name.len() + padded;
		if self.bytes.len() + need > abi::MAX_DEVICE_PROPERTIES || name.len() > u16::MAX as usize {
			self.cut = true;
			return false;
		}
		self.bytes.push(kind);
		self.bytes.push(depth);
		self.bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
		self.bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
		self.bytes.extend_from_slice(name);
		self.bytes.extend_from_slice(value);
		self.bytes.resize(self.bytes.len() + (padded - value.len()), 0);
		true
	}
}

fn wire_value(value: &Value) -> wire::Value {
	match value {
		Value::Integer(value) => wire::Value::Integer(*value),
		Value::String(text) => wire::Value::String(text.clone()),
		Value::Reference(path) => wire::Value::Reference(path.clone()),
		Value::Buffer(bytes) => wire::Value::Buffer(bytes.clone()),
		Value::Package(elements) => wire::Value::Package(elements.iter().map(wire_value).collect()),
	}
}

fn write(block: &mut Block, properties: &Properties, depth: u8) {
	for (name, value) in &properties.values {
		match wire::encode(&wire_value(value)) {
			Ok(encoded) => {
				if !block.record(abi::DEVICE_PROPERTY_VALUE, depth, name.as_bytes(), &encoded) {
					return;
				}
			}
			Err(_) => block.cut = true,
		}
	}
	for (name, child) in &properties.children {
		if depth == u8::MAX || !block.record(abi::DEVICE_PROPERTY_NODE, depth + 1, name.as_bytes(), &[]) {
			return;
		}
		write(block, child, depth + 1);
	}
}

/// THE BLOCK: `_UID` first, when the node has one, then `_DSD`'s properties and its data nodes in order.
pub fn block(uid: Option<&str>, properties: &Properties) -> Block {
	let mut block = Block::default();
	if let Some(uid) = uid
		&& let Ok(encoded) = wire::encode(&wire::Value::String(alloc::string::String::from(uid)))
	{
		block.record(abi::DEVICE_PROPERTY_VALUE, 0, b"_UID", &encoded);
	}
	write(&mut block, properties, 0);
	block
}
