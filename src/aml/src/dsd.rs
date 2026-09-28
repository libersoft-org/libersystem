//! `_DSD`: DEVICE PROPERTIES and HIERARCHICAL DATA, decoded. A `_DSD` package is pairs of a UUID and a package; the
//! device-properties UUID's package holds `{ key, value }` pairs, and the hierarchical-data UUID's package holds
//! `{ name, data node }` pairs whose data node is itself a `_DSD`-shaped package.

use alloc::string::String;
use alloc::vec::Vec;

use crate::object::Object;

/// A UUID as `ToUUID` lays it out: the first three fields little-endian, the last eight bytes as written.
pub fn uuid(text: &str) -> Option<[u8; 16]> {
	let hex: Vec<u8> = text.bytes().filter(|byte| *byte != b'-').collect();
	if hex.len() != 32 || text.len() != 36 {
		return None;
	}
	let mut raw = [0u8; 16];
	for (at, slot) in raw.iter_mut().enumerate() {
		let pair = core::str::from_utf8(&hex[2 * at..2 * at + 2]).ok()?;
		*slot = u8::from_str_radix(pair, 16).ok()?;
	}
	let mut out = raw;
	out[0..4].copy_from_slice(&[raw[3], raw[2], raw[1], raw[0]]);
	out[4..6].copy_from_slice(&[raw[5], raw[4]]);
	out[6..8].copy_from_slice(&[raw[7], raw[6]]);
	Some(out)
}

pub const DEVICE_PROPERTIES: &str = "daffd814-6eba-4d8c-8a91-bc9bbf4aa301";
pub const HIERARCHICAL_DATA: &str = "dbb8e3e6-5886-4ba6-8795-1319f52a966b";

/// One property value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Value {
	Integer(u64),
	String(String),
	/// A reference to another node, by its path.
	Reference(String),
	Package(Vec<Value>),
	Buffer(Vec<u8>),
}

/// A node's properties, and its hierarchical data children by name.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Properties {
	pub values: Vec<(String, Value)>,
	pub children: Vec<(String, Properties)>,
}

impl Properties {
	pub fn get(&self, key: &str) -> Option<&Value> {
		self.values.iter().find(|(name, _)| name == key).map(|(_, value)| value)
	}
}

/// What a `_DSD` element resolves to when it is not a plain value: a reference or a name, followed by the caller -
/// to a data node's package (`Package`), or named as a path (`Path`).
pub enum Resolved {
	Package(Object),
	Path(String),
}

/// DECODE a `_DSD` package. `resolve` follows an element that is a reference or a name - the interpreter's to do -
/// and `depth` bounds hierarchical data within data.
pub fn decode(package: &Object, resolve: &mut dyn FnMut(&Object) -> Option<Resolved>, depth: usize) -> Properties {
	let mut out = Properties::default();
	let Object::Package(elements) = package else { return out };
	let properties = uuid(DEVICE_PROPERTIES);
	let hierarchical = uuid(HIERARCHICAL_DATA);
	let mut at = 0;
	while at + 1 < elements.len() {
		let key = elements[at].borrow().clone();
		let body = elements[at + 1].borrow().clone();
		at += 2;
		let Object::Buffer(id) = key else { continue };
		let Object::Package(pairs) = body else { continue };
		if Some(id.as_slice()) == properties.as_ref().map(|uuid| uuid.as_slice()) {
			for pair in pairs {
				let Object::Package(pair) = pair.borrow().clone() else { continue };
				if pair.len() != 2 {
					continue;
				}
				let Object::String(name) = pair[0].borrow().clone() else { continue };
				let value = value_of(&pair[1].borrow(), resolve);
				if let Some(value) = value {
					out.values.push((name, value));
				}
			}
		} else if Some(id.as_slice()) == hierarchical.as_ref().map(|uuid| uuid.as_slice()) && depth > 0 {
			for pair in pairs {
				let Object::Package(pair) = pair.borrow().clone() else { continue };
				if pair.len() != 2 {
					continue;
				}
				let Object::String(name) = pair[0].borrow().clone() else { continue };
				let target = pair[1].borrow().clone();
				let child = match target {
					Object::Package(_) => Some(target),
					other => match resolve(&other) {
						Some(Resolved::Package(package)) => Some(package),
						_ => None,
					},
				};
				if let Some(child) = child {
					out.children.push((name, decode(&child, resolve, depth - 1)));
				}
			}
		}
	}
	out
}

fn value_of(object: &Object, resolve: &mut dyn FnMut(&Object) -> Option<Resolved>) -> Option<Value> {
	Some(match object {
		Object::Integer(value) => Value::Integer(*value),
		Object::String(text) => Value::String(text.clone()),
		Object::Buffer(bytes) => Value::Buffer(bytes.clone()),
		Object::Package(elements) => Value::Package(elements.iter().filter_map(|element| value_of(&element.borrow(), resolve)).collect()),
		other => match resolve(other)? {
			Resolved::Path(path) => Value::Reference(path),
			Resolved::Package(package) => return value_of(&package, resolve),
		},
	})
}
