//! OBJECTS: what a node, a local, an argument or a package element holds, shared and mutable as AML's are - a
//! `Store` into a named object changes what every holder of it sees, and an `Index` reference writes into the
//! package or buffer it was taken from.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::name::{NameString, Path};
use crate::namespace::NodeId;

pub type ObjRef = Rc<RefCell<Object>>;

pub fn obj(object: Object) -> ObjRef {
	Rc::new(RefCell::new(object))
}

pub fn int(value: u64) -> ObjRef {
	obj(Object::Integer(value))
}

/// The address space of an operation region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Space {
	SystemMemory,
	SystemIo,
	PciConfig,
	EmbeddedControl,
	SmBus,
	SystemCmos,
	PciBarTarget,
	Ipmi,
	GeneralPurposeIo,
	GenericSerialBus,
	Pcc,
	Other(u8),
}

impl Space {
	pub fn from_byte(byte: u8) -> Space {
		match byte {
			0 => Space::SystemMemory,
			1 => Space::SystemIo,
			2 => Space::PciConfig,
			3 => Space::EmbeddedControl,
			4 => Space::SmBus,
			5 => Space::SystemCmos,
			6 => Space::PciBarTarget,
			7 => Space::Ipmi,
			8 => Space::GeneralPurposeIo,
			9 => Space::GenericSerialBus,
			10 => Space::Pcc,
			other => Space::Other(other),
		}
	}

	pub fn name(&self) -> &'static str {
		match self {
			Space::SystemMemory => "SystemMemory",
			Space::SystemIo => "SystemIO",
			Space::PciConfig => "PCI_Config",
			Space::EmbeddedControl => "EmbeddedControl",
			Space::SmBus => "SMBus",
			Space::SystemCmos => "SystemCMOS",
			Space::PciBarTarget => "PciBarTarget",
			Space::Ipmi => "IPMI",
			Space::GeneralPurposeIo => "GeneralPurposeIo",
			Space::GenericSerialBus => "GenericSerialBus",
			Space::Pcc => "PCC",
			Space::Other(_) => "an unknown space",
		}
	}
}

#[derive(Clone, Debug)]
pub struct Region {
	pub space: Space,
	pub offset: u64,
	pub length: u64,
	/// The node the region was declared under: a PCI_Config region's device is found from it.
	pub parent: NodeId,
}

/// How a field is reached: its access width.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
	Any,
	Byte,
	Word,
	DWord,
	QWord,
	Buffer,
}

impl Access {
	pub fn from_flags(flags: u8) -> Access {
		match flags & 0x0F {
			1 => Access::Byte,
			2 => Access::Word,
			3 => Access::DWord,
			4 => Access::QWord,
			5 => Access::Buffer,
			_ => Access::Any,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Update {
	Preserve,
	WriteAsOnes,
	WriteAsZeros,
}

/// What a field's `Connection` names: a resource descriptor - a `GpioIo`, a `GpioInt` or a serial-bus connection
/// - as its raw bytes, decoded when the field is reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection(pub Vec<u8>);

/// How a field reaches its region.
#[derive(Clone, Debug)]
pub enum FieldKind {
	/// Straight into the region.
	Region(NodeId),
	/// Through an index field: the offset written to `index`, the datum through `data`.
	Index { index: NodeId, data: NodeId },
	/// Into the region after `value` is written to the bank field `bank`.
	Bank { region: NodeId, bank: NodeId, value: u64 },
}

#[derive(Clone, Debug)]
pub struct Field {
	pub kind: FieldKind,
	pub bit_offset: u64,
	pub bit_length: u64,
	pub access: Access,
	pub lock: bool,
	pub update: Update,
	/// The access attribute (a serial-bus protocol) and its length, from `AccessAs`.
	pub attrib: u8,
	pub attrib_length: u8,
	pub connection: Option<Connection>,
}

/// A field carved out of a buffer by one of the `Create...Field` operators.
#[derive(Clone, Debug)]
pub struct BufferField {
	pub buffer: ObjRef,
	pub bit_offset: u64,
	pub bit_length: u64,
}

#[derive(Clone, Debug)]
pub struct Method {
	pub args: u8,
	pub serialized: bool,
	pub sync_level: u8,
	pub code: Rc<[u8]>,
	/// A method the interpreter answers itself: `\_OSI`.
	pub native: Option<Native>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Native {
	Osi,
}

#[derive(Clone, Debug, Default)]
pub struct Mutex {
	pub sync_level: u8,
	/// How many times the one thread of this interpreter holds it.
	pub depth: u32,
	/// The firmware global lock's stand-in: `\_GL`.
	pub global: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Event {
	pub signals: u64,
}

/// A REFERENCE: to a named object, to an object (a local's or an argument's), or to one element of a package, one
/// byte of a buffer or one character of a string.
#[derive(Clone, Debug)]
pub enum Reference {
	Node(NodeId),
	Object(ObjRef),
	Element { container: ObjRef, index: usize },
}

#[derive(Clone, Debug)]
pub enum Object {
	Uninitialized,
	Integer(u64),
	String(String),
	Buffer(Vec<u8>),
	Package(Vec<ObjRef>),
	Reference(Reference),
	Method(Method),
	Mutex(Mutex),
	Event(Event),
	Region(Region),
	Field(Field),
	BufferField(BufferField),
	Device,
	Processor {
		id: u8,
		block: u32,
		block_length: u8,
	},
	PowerResource {
		system_level: u8,
		order: u16,
	},
	ThermalZone,
	Debug,
	/// A node that is only a scope: the root, `\_SB`, `\_GPE`, and what `Scope` alone creates.
	Scope,
	/// A name inside a package, resolved when the element is used: a package may name what is declared later.
	Name {
		name: NameString,
		scope: NodeId,
	},
	/// An `External` declaration nothing has defined yet: its object type and argument count.
	External {
		kind: u8,
		args: u8,
	},
}

/// ACPI's object type numbers, as `ObjectType` answers them.
pub mod kind {
	pub const UNINITIALIZED: u64 = 0;
	pub const INTEGER: u64 = 1;
	pub const STRING: u64 = 2;
	pub const BUFFER: u64 = 3;
	pub const PACKAGE: u64 = 4;
	pub const FIELD_UNIT: u64 = 5;
	pub const DEVICE: u64 = 6;
	pub const EVENT: u64 = 7;
	pub const METHOD: u64 = 8;
	pub const MUTEX: u64 = 9;
	pub const REGION: u64 = 10;
	pub const POWER_RESOURCE: u64 = 11;
	pub const PROCESSOR: u64 = 12;
	pub const THERMAL_ZONE: u64 = 13;
	pub const BUFFER_FIELD: u64 = 14;
	pub const DEBUG: u64 = 16;
}

impl Object {
	pub fn type_code(&self) -> u64 {
		match self {
			Object::Uninitialized | Object::Scope | Object::Name { .. } | Object::External { .. } => kind::UNINITIALIZED,
			Object::Integer(_) => kind::INTEGER,
			Object::String(_) => kind::STRING,
			Object::Buffer(_) => kind::BUFFER,
			Object::Package(_) => kind::PACKAGE,
			Object::Field(_) => kind::FIELD_UNIT,
			Object::Device => kind::DEVICE,
			Object::Event(_) => kind::EVENT,
			Object::Method(_) => kind::METHOD,
			Object::Mutex(_) => kind::MUTEX,
			Object::Region(_) => kind::REGION,
			Object::PowerResource { .. } => kind::POWER_RESOURCE,
			Object::Processor { .. } => kind::PROCESSOR,
			Object::ThermalZone => kind::THERMAL_ZONE,
			Object::BufferField(_) => kind::BUFFER_FIELD,
			Object::Debug => kind::DEBUG,
			Object::Reference(_) => kind::UNINITIALIZED,
		}
	}

	/// Whether this is a data object - what `Store` copies and a method may return.
	pub fn is_data(&self) -> bool {
		matches!(self, Object::Integer(_) | Object::String(_) | Object::Buffer(_) | Object::Package(_))
	}

	pub fn type_name(&self) -> &'static str {
		match self {
			Object::Uninitialized => "uninitialized",
			Object::Integer(_) => "integer",
			Object::String(_) => "string",
			Object::Buffer(_) => "buffer",
			Object::Package(_) => "package",
			Object::Reference(_) => "reference",
			Object::Method(_) => "method",
			Object::Mutex(_) => "mutex",
			Object::Event(_) => "event",
			Object::Region(_) => "operation region",
			Object::Field(_) => "field unit",
			Object::BufferField(_) => "buffer field",
			Object::Device => "device",
			Object::Processor { .. } => "processor",
			Object::PowerResource { .. } => "power resource",
			Object::ThermalZone => "thermal zone",
			Object::Debug => "debug object",
			Object::Scope => "scope",
			Object::Name { .. } => "name reference",
			Object::External { .. } => "external",
		}
	}
}

/// A DEEP COPY of a data object: `Store` and `CopyObject` hand the target its own package, never the source's.
pub fn copy(object: &Object) -> Object {
	match object {
		Object::Package(elements) => Object::Package(elements.iter().map(|element| obj(copy(&element.borrow()))).collect()),
		other => other.clone(),
	}
}

/// How many bytes an object holds - what the memory bound counts.
pub fn footprint(object: &Object) -> usize {
	match object {
		Object::String(text) => text.len() + 16,
		Object::Buffer(bytes) => bytes.len() + 16,
		Object::Package(elements) => 16 + elements.iter().map(|element| footprint(&element.borrow())).sum::<usize>(),
		_ => 16,
	}
}

/// A path for display, for a node the caller knows.
pub fn describe(path: &Path) -> String {
	path.text()
}
