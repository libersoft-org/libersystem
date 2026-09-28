//! DEVICES IN THE NAMESPACE: the walk with `_STA` and `_INI` in the specification's order, a device's identity
//! (`_HID`, `_CID`, `_UID`, `_ADR`), its `_CRS`, its `_DSD`, and the `_OSC` and `_DSM` calls.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::convert;
use crate::dsd::{self, Properties, Resolved};
use crate::error::Error;
use crate::host::Host;
use crate::interp::Aml;
use crate::name::{Path, Seg};
use crate::namespace::{NodeId, ROOT};
use crate::object::{Object, Reference};
use crate::resource::{self, Resource};

/// `_STA`'s bits.
pub mod sta {
	pub const PRESENT: u32 = 1 << 0;
	pub const ENABLED: u32 = 1 << 1;
	pub const SHOWN: u32 = 1 << 2;
	pub const FUNCTIONING: u32 = 1 << 3;
	pub const BATTERY: u32 = 1 << 4;
	/// A device with no `_STA` is present, enabled, shown and functioning.
	pub const DEFAULT: u32 = 0x0F;
}

/// What kind of namespace object a node is, for a walk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Device,
	Processor,
	ThermalZone,
	PowerResource,
	Other,
}

/// A device's identity, as its node declares it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Identity {
	pub hid: Option<String>,
	pub cids: Vec<String>,
	pub uid: Option<String>,
	pub adr: Option<u64>,
}

/// One node the walk found present, with its `_STA`.
#[derive(Clone, Debug)]
pub struct Found {
	pub node: NodeId,
	pub path: Path,
	pub kind: Kind,
	pub sta: u32,
}

/// What a walk found and what it could not run.
#[derive(Clone, Debug, Default)]
pub struct Walk {
	pub found: Vec<Found>,
	pub failures: Vec<(Path, Error)>,
}

fn id_text(object: &Object) -> Option<String> {
	match object {
		Object::Integer(value) => Some(resource::eisa_id(*value as u32)),
		Object::String(text) => Some(text.clone()),
		_ => None,
	}
}

impl Aml {
	pub fn kind(&self, node: NodeId) -> Kind {
		match self.ns.object(node).map(|object| object.borrow().clone()) {
			Some(Object::Device) => Kind::Device,
			Some(Object::Processor { .. }) => Kind::Processor,
			Some(Object::ThermalZone) => Kind::ThermalZone,
			Some(Object::PowerResource { .. }) => Kind::PowerResource,
			_ => Kind::Other,
		}
	}

	/// A child named `name` evaluated, `None` when the node has none.
	pub fn child_value(&mut self, node: NodeId, name: &[u8; 4], host: &mut dyn Host) -> Result<Option<Object>, Error> {
		match self.ns.child(node, Seg(*name)) {
			Some(child) => self.evaluate(child, &[], host),
			None => Ok(None),
		}
	}

	fn child_integer(&mut self, node: NodeId, name: &[u8; 4], host: &mut dyn Host) -> Result<Option<u64>, Error> {
		match self.child_value(node, name, host)? {
			Some(value) => Ok(Some(convert::to_integer(&value, self.int64)?)),
			None => Ok(None),
		}
	}

	/// `_STA`, or `DEFAULT` when the node has none.
	pub fn sta(&mut self, node: NodeId, host: &mut dyn Host) -> Result<u32, Error> {
		Ok(self.child_integer(node, b"_STA", host)?.map(|value| value as u32).unwrap_or(sta::DEFAULT))
	}

	/// THE DEVICE WALK, as the specification orders it: `\_SB._INI` first; then each device, processor, thermal zone
	/// and power resource depth first, `_STA` evaluated, `_INI` run where it is present, the children walked when
	/// it is present or functioning, and skipped when it is neither.
	pub fn initialize(&mut self, host: &mut dyn Host) -> Walk {
		let mut walk = Walk::default();
		if let Some(sb) = self.lookup("\\_SB")
			&& let Some(ini) = self.ns.child(sb, Seg(*b"_INI"))
			&& let Err(error) = self.evaluate(ini, &[], host)
		{
			walk.failures.push((self.ns.path(ini), error));
		}
		self.walk_from(ROOT, true, host, &mut walk);
		walk
	}

	/// The same walk over `node`'s subtree without running `_INI` - what a device-check re-walks.
	pub fn rewalk(&mut self, node: NodeId, host: &mut dyn Host) -> Walk {
		let mut walk = Walk::default();
		self.walk_from(node, false, host, &mut walk);
		walk
	}

	fn walk_from(&mut self, scope: NodeId, run_ini: bool, host: &mut dyn Host, walk: &mut Walk) {
		for child in self.ns.children(scope) {
			let kind = self.kind(child);
			if kind == Kind::Other {
				// A scope that is not a device still holds devices: `\_SB` itself, a `Scope` of its own.
				let is_scope = matches!(self.ns.object(child).map(|object| object.borrow().clone()), Some(Object::Scope));
				if is_scope {
					self.walk_from(child, run_ini, host, walk);
				}
				continue;
			}
			let status = match self.sta(child, host) {
				Ok(status) => status,
				Err(error) => {
					walk.failures.push((self.ns.path(child), error));
					continue;
				}
			};
			if status & sta::PRESENT != 0 {
				if run_ini
					&& let Some(ini) = self.ns.child(child, Seg(*b"_INI"))
					&& let Err(error) = self.evaluate(ini, &[], host)
				{
					walk.failures.push((self.ns.path(ini), error));
				}
				walk.found.push(Found { node: child, path: self.ns.path(child), kind, sta: status });
				self.walk_from(child, run_ini, host, walk);
			} else if status & sta::FUNCTIONING != 0 {
				self.walk_from(child, run_ini, host, walk);
			}
		}
	}

	/// `_HID`, `_CID` (one or a package), `_UID` and `_ADR`.
	pub fn identity(&mut self, node: NodeId, host: &mut dyn Host) -> Result<Identity, Error> {
		let hid = self.child_value(node, b"_HID", host)?.as_ref().and_then(id_text);
		let cids = match self.child_value(node, b"_CID", host)? {
			Some(Object::Package(elements)) => elements.iter().filter_map(|element| id_text(&element.borrow())).collect(),
			Some(other) => id_text(&other).into_iter().collect(),
			None => Vec::new(),
		};
		let uid = match self.child_value(node, b"_UID", host)? {
			Some(Object::Integer(value)) => Some(format!("{value}")),
			Some(Object::String(text)) => Some(text),
			_ => None,
		};
		let adr = self.child_integer(node, b"_ADR", host)?;
		Ok(Identity { hid, cids, uid, adr })
	}

	/// `_CRS`, decoded; an empty list for a node with none.
	pub fn resources(&mut self, node: NodeId, host: &mut dyn Host) -> Result<Vec<Resource>, Error> {
		match self.child_value(node, b"_CRS", host)? {
			Some(Object::Buffer(bytes)) => resource::decode(&bytes).map_err(|error| Error::Type(format!("the _CRS of {} does not decode: {error:?}", self.ns.path(node)))),
			Some(other) => Err(Error::Type(format!("the _CRS of {} is a {}", self.ns.path(node), other.type_name()))),
			None => Ok(Vec::new()),
		}
	}

	/// `_DSD`, decoded, hierarchical data followed four levels deep.
	pub fn properties(&mut self, node: NodeId, host: &mut dyn Host) -> Result<Properties, Error> {
		let Some(package) = self.child_value(node, b"_DSD", host)? else { return Ok(Properties::default()) };
		let mut resolve = |object: &Object| -> Option<Resolved> {
			match object {
				Object::Name { name, scope } => {
					let target = self.ns.resolve(name, *scope)?;
					self.resolved(target)
				}
				Object::Reference(Reference::Node(target)) => self.resolved(*target),
				Object::String(text) => {
					let target = self.lookup_from(node, text)?;
					self.resolved(target)
				}
				_ => None,
			}
		};
		Ok(dsd::decode(&package, &mut resolve, 4))
	}

	fn resolved(&self, node: NodeId) -> Option<Resolved> {
		match self.ns.object(node).map(|object| object.borrow().clone()) {
			Some(Object::Package(elements)) => Some(Resolved::Package(Object::Package(elements))),
			Some(_) => Some(Resolved::Path(self.ns.path(node).text())),
			None => None,
		}
	}

	/// `_OSC(uuid, revision, count, capabilities)`: the capabilities buffer as dwords, and the dwords answered.
	pub fn osc(&mut self, node: NodeId, uuid: &str, revision: u64, capabilities: &[u32], host: &mut dyn Host) -> Result<Option<Vec<u32>>, Error> {
		let Some(method) = self.ns.child(node, Seg(*b"_OSC")) else { return Ok(None) };
		let id = dsd::uuid(uuid).ok_or_else(|| Error::Type(format!("{uuid} is not a UUID")))?;
		let buffer: Vec<u8> = capabilities.iter().flat_map(|dword| dword.to_le_bytes()).collect();
		let answered = self.evaluate(method, &[Object::Buffer(id.to_vec()), Object::Integer(revision), Object::Integer(capabilities.len() as u64), Object::Buffer(buffer)], host)?;
		match answered {
			Some(Object::Buffer(bytes)) => Ok(Some(bytes.chunks(4).map(|chunk| chunk.iter().enumerate().fold(0u32, |value, (at, byte)| value | (*byte as u32) << (8 * at))).collect())),
			Some(other) => Err(Error::Type(format!("_OSC answered a {}", other.type_name()))),
			None => Err(Error::Type(String::from("_OSC answered nothing"))),
		}
	}

	/// `_DSM(uuid, revision, function, arguments)`.
	pub fn dsm(&mut self, node: NodeId, uuid: &str, revision: u64, function: u64, arguments: Object, host: &mut dyn Host) -> Result<Option<Object>, Error> {
		let id = dsd::uuid(uuid).ok_or_else(|| Error::Type(format!("{uuid} is not a UUID")))?;
		self.dsm_bytes(node, &id, revision, function, arguments, host)
	}

	/// The same, the UUID as the sixteen bytes `ToUUID` lays out - as a node channel carries it.
	pub fn dsm_bytes(&mut self, node: NodeId, uuid: &[u8; 16], revision: u64, function: u64, arguments: Object, host: &mut dyn Host) -> Result<Option<Object>, Error> {
		let Some(method) = self.ns.child(node, Seg(*b"_DSM")) else { return Ok(None) };
		self.evaluate(method, &[Object::Buffer(uuid.to_vec()), Object::Integer(revision), Object::Integer(function), arguments], host)
	}

	/// The path an element of a returned package names - a name, or a reference to a node.
	pub fn element_path(&self, element: &Object) -> Option<Path> {
		match element {
			Object::Name { name, scope } => self.ns.resolve(name, *scope).map(|node| self.ns.path(node)),
			Object::Reference(Reference::Node(node)) => Some(self.ns.path(*node)),
			_ => None,
		}
	}
}
