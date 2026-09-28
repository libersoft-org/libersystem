//! THE EVALUATOR: AML is parsed as it runs - a table's term list when the table is loaded, a method's body when the
//! method is called - one term at a time, under the bounds in `Limits`. A method past a bound is aborted and the
//! error names the bound; nothing is retried.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::convert;
use crate::error::{Bound, Error, Limits};
use crate::host::Host;
use crate::name::{NameString, Path, Seg};
use crate::namespace::{Namespace, NodeId, ROOT};
use crate::object::{self, Access, BufferField, Connection, Event, Field, FieldKind, Method, Mutex, Native, ObjRef, Object, Reference, Region, Space, Update, int, obj};

/// The interpreter revision `Revision` answers.
pub const REVISION: u64 = 1;

/// What `\_OS` names.
pub const OS_NAME: &str = "Microsoft Windows NT";

/// What `\_REV` answers.
pub const OS_REVISION: u64 = 2;

/// THE STRINGS `\_OSI` ANSWERS TRUE FOR: the Windows versions through the newest this interpreter is tested against,
/// and the feature strings it implements. Firmware is validated against Windows, and machines otherwise hide
/// batteries, touchpads or processor states; "Linux", "Darwin" and anything else are false.
pub const OSI_TRUE: &[&str] = &[
	"Windows 2000",
	"Windows 2001",
	"Windows 2001 SP1",
	"Windows 2001.1",
	"Windows 2001 SP2",
	"Windows 2001.1 SP1",
	"Windows 2006",
	"Windows 2006.1",
	"Windows 2006 SP1",
	"Windows 2006 SP2",
	"Windows 2009",
	"Windows 2012",
	"Windows 2013",
	"Windows 2015",
	"Windows 2016",
	"Windows 2017",
	"Windows 2017.2",
	"Windows 2018",
	"Windows 2018.2",
	"Windows 2019",
	"Windows 2020",
	"Windows 2021",
	"Windows 2022",
	"Module Device",
	"Processor Device",
	"Extended Address Space Descriptor",
];

/// THE NAMESPACE AND WHAT LOADED IT.
pub struct Aml {
	pub ns: Namespace,
	pub limits: Limits,
	/// Integers are 64 bits wide unless the DSDT's revision is below 2.
	pub int64: bool,
	tables: usize,
	next_handle: u64,
	/// What loading reported and let pass: a name declared twice, a table-level term that failed.
	pub warnings: Vec<String>,
}

pub(crate) enum Flow {
	Next,
	Return(ObjRef),
	Break,
	Continue,
}

/// Where a store goes.
#[derive(Clone, Debug)]
pub(crate) enum Target {
	Null,
	Local(usize),
	Arg(usize),
	Node(NodeId),
	Debug,
	Reference(Reference),
}

pub(crate) struct Frame {
	pub code: Rc<[u8]>,
	pub scope: NodeId,
	pub args: Vec<ObjRef>,
	pub locals: Vec<ObjRef>,
	/// Names this method created, removed when it returns.
	pub temps: Vec<NodeId>,
	pub in_method: bool,
}

impl Frame {
	fn new(code: Rc<[u8]>, scope: NodeId, in_method: bool) -> Frame {
		Frame { code, scope, args: (0..7).map(|_| obj(Object::Uninitialized)).collect(), locals: (0..8).map(|_| obj(Object::Uninitialized)).collect(), temps: Vec::new(), in_method }
	}
}

pub(crate) struct Machine<'a> {
	pub aml: &'a mut Aml,
	pub host: &'a mut dyn Host,
	steps: u64,
	started_ms: u64,
	memory: usize,
	calls: usize,
	nesting: usize,
	package_depth: usize,
}

fn seg(text: &[u8; 4]) -> Seg {
	Seg(*text)
}

impl Aml {
	/// An empty namespace with the predefined scopes and objects: `\_GPE`, `\_PR`, `\_SB`, `\_SI`, `\_TZ`, `\_OSI`,
	/// `\_OS`, `\_REV` and the global lock `\_GL`.
	pub fn new(limits: Limits) -> Aml {
		let mut ns = Namespace::new();
		for name in [b"_GPE", b"_PR_", b"_SB_", b"_SI_", b"_TZ_"] {
			let _ = ns.add(ROOT, seg(name), obj(Object::Scope));
		}
		let _ = ns.add(ROOT, seg(b"_OSI"), obj(Object::Method(Method { args: 1, serialized: false, sync_level: 0, code: Rc::from(&[][..]), native: Some(Native::Osi) })));
		let _ = ns.add(ROOT, seg(b"_OS_"), obj(Object::String(String::from(OS_NAME))));
		let _ = ns.add(ROOT, seg(b"_REV"), int(OS_REVISION));
		let _ = ns.add(ROOT, seg(b"_GL_"), obj(Object::Mutex(Mutex { sync_level: 0, depth: 0, global: true })));
		Aml { ns, limits, int64: true, tables: 0, next_handle: 1, warnings: Vec::new() }
	}

	/// The node a name names, from the root.
	pub fn lookup(&self, text: &str) -> Option<NodeId> {
		let name = NameString::parse(text)?;
		self.ns.resolve(&name, ROOT)
	}

	/// The node `name` names relative to `scope`, with the search rule for a bare segment.
	pub fn lookup_from(&self, scope: NodeId, text: &str) -> Option<NodeId> {
		let name = NameString::parse(text)?;
		self.ns.resolve(&name, scope)
	}

	/// How many tables are loaded.
	pub fn tables(&self) -> usize {
		self.tables
	}

	/// LOAD A TABLE - the DSDT first, then each SSDT - into the one namespace: its header checked, its checksum
	/// checked, and its term list run at the root. A term that fails at table level ends that table's load and is
	/// answered; what it declared before stands.
	pub fn load(&mut self, table: &[u8], host: &mut dyn Host) -> Result<u64, Error> {
		let body = check_table(table)?;
		if self.tables >= self.limits.tables {
			return Err(Error::Bound(Bound::Tables));
		}
		if self.tables == 0 {
			// A DSDT BELOW REVISION 2 is a 32-bit namespace: every integer is cut to 32 bits.
			self.int64 = table[8] >= 2;
		}
		self.tables += 1;
		let handle = self.next_handle;
		self.next_handle += 1;
		let code: Rc<[u8]> = Rc::from(body);
		let mut frame = Frame::new(code, ROOT, false);
		let mut machine = Machine::new(self, host);
		let end = frame.code.len();
		let mut pos = 0usize;
		machine.run_list(&mut frame, &mut pos, end)?;
		Ok(handle)
	}

	/// EVALUATE a named object: a method is called with `args`, a field read, anything else answered as it is.
	/// `None` for a method that returned nothing.
	pub fn evaluate(&mut self, node: NodeId, args: &[Object], host: &mut dyn Host) -> Result<Option<Object>, Error> {
		let mut machine = Machine::new(self, host);
		let args: Vec<ObjRef> = args.iter().map(|arg| obj(object::copy(arg))).collect();
		let result = machine.evaluate_node(node, args)?;
		Ok(result.map(|value| machine.detach(&value)))
	}

	/// Evaluate by name from the root; `Ok(None)` when the name is not there.
	pub fn evaluate_path(&mut self, text: &str, args: &[Object], host: &mut dyn Host) -> Result<Option<Object>, Error> {
		match self.lookup(text) {
			Some(node) => self.evaluate(node, args, host),
			None => Ok(None),
		}
	}

	/// Run `_REG(space, connected)` for every device under `scope` that declares a region of `space` - what makes
	/// firmware start using a region once its handler is there.
	pub fn run_reg(&mut self, space: Space, connected: bool, host: &mut dyn Host) -> Vec<(Path, Error)> {
		let mut failed = Vec::new();
		let mut owners: Vec<NodeId> = Vec::new();
		for node in self.ns.descendants(ROOT) {
			let Some(object) = self.ns.object(node) else { continue };
			let region_space = match &*object.borrow() {
				Object::Region(region) => Some((region.space, region.parent)),
				_ => None,
			};
			if let Some((found, parent)) = region_space
				&& found == space
				&& !owners.contains(&parent)
			{
				owners.push(parent);
			}
		}
		let code = match space {
			Space::Other(byte) => byte as u64,
			other => space_code(other),
		};
		for owner in owners {
			let Some(reg) = self.ns.child(owner, seg(b"_REG")) else { continue };
			if let Err(error) = self.evaluate(reg, &[Object::Integer(code), Object::Integer(connected as u64)], host) {
				failed.push((self.ns.path(reg), error));
			}
		}
		failed
	}
}

pub(crate) fn space_code(space: Space) -> u64 {
	match space {
		Space::SystemMemory => 0,
		Space::SystemIo => 1,
		Space::PciConfig => 2,
		Space::EmbeddedControl => 3,
		Space::SmBus => 4,
		Space::SystemCmos => 5,
		Space::PciBarTarget => 6,
		Space::Ipmi => 7,
		Space::GeneralPurposeIo => 8,
		Space::GenericSerialBus => 9,
		Space::Pcc => 10,
		Space::Other(byte) => byte as u64,
	}
}

/// A table's AML body after its header is checked: a signature that holds AML, a length that fits, a checksum that
/// sums to zero.
pub fn check_table(table: &[u8]) -> Result<&[u8], Error> {
	if table.len() < 36 {
		return Err(Error::BadTable("shorter than a table header"));
	}
	let signature = &table[0..4];
	if !matches!(signature, b"DSDT" | b"SSDT" | b"PSDT") && !signature.starts_with(b"OEM") {
		return Err(Error::BadTable("its signature is not one that holds AML"));
	}
	let length = u32::from_le_bytes([table[4], table[5], table[6], table[7]]) as usize;
	if length < 36 || length > table.len() {
		return Err(Error::BadTable("its length is not what it holds"));
	}
	let sum = table[..length].iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
	if sum != 0 {
		return Err(Error::BadTable("its checksum does not sum to zero"));
	}
	Ok(&table[36..length])
}

impl<'a> Machine<'a> {
	pub(crate) fn new(aml: &'a mut Aml, host: &'a mut dyn Host) -> Machine<'a> {
		let started_ms = host.now_ms();
		Machine { aml, host, steps: 0, started_ms, memory: 0, calls: 0, nesting: 0, package_depth: 0 }
	}

	fn int64(&self) -> bool {
		self.aml.int64
	}

	fn ones(&self) -> u64 {
		if self.int64() { u64::MAX } else { 0xFFFF_FFFF }
	}

	fn mask(&self, value: u64) -> u64 {
		convert::mask(value, self.int64())
	}

	// ------------------------------------------------------------------ bounds

	fn tick(&mut self) -> Result<(), Error> {
		self.steps += 1;
		if self.steps > self.aml.limits.steps {
			return Err(Error::Bound(Bound::Steps));
		}
		if self.steps % 1024 == 0 {
			self.check_time()?;
		}
		Ok(())
	}

	fn check_time(&mut self) -> Result<(), Error> {
		if self.host.now_ms().saturating_sub(self.started_ms) > self.aml.limits.time_ms {
			return Err(Error::Bound(Bound::Time));
		}
		Ok(())
	}

	fn allocate(&mut self, bytes: usize) -> Result<(), Error> {
		self.memory = self.memory.saturating_add(bytes);
		if self.memory > self.aml.limits.memory {
			return Err(Error::Bound(Bound::Memory));
		}
		Ok(())
	}

	fn new_object(&mut self, object: Object) -> Result<ObjRef, Error> {
		self.allocate(object::footprint(&object))?;
		Ok(obj(object))
	}

	// ------------------------------------------------------------------ reading the code

	fn byte(&self, f: &Frame, at: usize) -> Result<u8, Error> {
		f.code.get(at).copied().ok_or(Error::Malformed("the code ended inside a term", at))
	}

	fn bytes(&self, f: &Frame, at: usize, count: usize) -> Result<u64, Error> {
		let mut value: u64 = 0;
		for i in 0..count {
			value |= (self.byte(f, at + i)? as u64) << (8 * i);
		}
		Ok(value)
	}

	/// A package length's VALUE, at `pos`, which is advanced past it.
	fn pkg_value(&self, f: &Frame, pos: &mut usize) -> Result<usize, Error> {
		let lead = self.byte(f, *pos)?;
		let count = (lead >> 6) as usize;
		let value = if count == 0 {
			(lead & 0x3F) as usize
		} else {
			let mut value = (lead & 0x0F) as usize;
			for i in 0..count {
				value |= (self.byte(f, *pos + 1 + i)? as usize) << (4 + 8 * i);
			}
			value
		};
		*pos += 1 + count;
		Ok(value)
	}

	/// A package length: the END of what it measures, counted from its own first byte.
	fn pkg_end(&self, f: &Frame, pos: &mut usize) -> Result<usize, Error> {
		let start = *pos;
		let value = self.pkg_value(f, pos)?;
		let end = start + value;
		if end > f.code.len() || end < *pos {
			return Err(Error::Malformed("a package length runs past its code", start));
		}
		Ok(end)
	}

	fn name(&self, f: &Frame, pos: &mut usize) -> Result<NameString, Error> {
		let (name, taken) = NameString::decode(&f.code[*pos..]).ok_or(Error::Malformed("not a name string", *pos))?;
		*pos += taken;
		Ok(name)
	}

	fn seg_at(&self, f: &Frame, pos: &mut usize) -> Result<Seg, Error> {
		let bytes: [u8; 4] = f.code.get(*pos..*pos + 4).and_then(|slice| slice.try_into().ok()).ok_or(Error::Malformed("a name segment runs past its code", *pos))?;
		let seg = Seg::new(bytes).ok_or(Error::Malformed("not a name segment", *pos))?;
		*pos += 4;
		Ok(seg)
	}

	// ------------------------------------------------------------------ the namespace, from here

	fn resolve(&self, f: &Frame, name: &NameString) -> Option<NodeId> {
		self.aml.ns.resolve(name, f.scope)
	}

	fn resolve_or_fail(&self, f: &Frame, name: &NameString) -> Result<NodeId, Error> {
		self.resolve(f, name).ok_or_else(|| Error::NotFound(format!("{name} (from {})", self.aml.ns.path(f.scope))))
	}

	/// DECLARE a named object. A name already declared: an `External` placeholder or a bare scope is replaced; at
	/// table level anything else is reported and the first declaration stands, as firmware tables that declare a
	/// name twice expect; inside a method it is an error.
	fn declare(&mut self, f: &mut Frame, name: &NameString, object: ObjRef) -> Result<NodeId, Error> {
		let (parent, last) = self.aml.ns.declaration_site(name, f.scope).ok_or_else(|| Error::NotFound(format!("the scope of {name}")))?;
		if self.aml.ns.len() >= self.aml.limits.nodes {
			return Err(Error::Bound(Bound::Nodes));
		}
		let (id, created) = self.aml.ns.add(parent, last, object.clone()).ok_or_else(|| Error::NotFound(format!("the scope of {name}")))?;
		if !created {
			let replaceable = matches!(self.aml.ns.object(id).map(|existing| existing.borrow().clone()).unwrap_or(Object::Uninitialized), Object::External { .. } | Object::Scope);
			if replaceable {
				self.aml.ns.set_object(id, object);
			} else if f.in_method {
				return Err(Error::Refused(format!("{} is declared twice", self.aml.ns.path(id))));
			} else {
				let line = format!("{} is declared twice; the first declaration stands", self.aml.ns.path(id));
				self.aml.warnings.push(line);
			}
		} else if f.in_method {
			f.temps.push(id);
		}
		Ok(id)
	}

	// ------------------------------------------------------------------ calling

	pub(crate) fn evaluate_node(&mut self, node: NodeId, args: Vec<ObjRef>) -> Result<Option<ObjRef>, Error> {
		let object = self.aml.ns.object(node).ok_or_else(|| Error::NotFound(format!("node {node}")))?;
		let method = match &*object.borrow() {
			Object::Method(method) => Some(method.clone()),
			_ => None,
		};
		match method {
			Some(method) => self.call(node, &method, args),
			None => Ok(Some(self.value_of_node(node)?)),
		}
	}

	fn call(&mut self, node: NodeId, method: &Method, args: Vec<ObjRef>) -> Result<Option<ObjRef>, Error> {
		if let Some(native) = method.native {
			return self.native(native, &args).map(Some);
		}
		if self.calls >= self.aml.limits.calls {
			return Err(Error::Bound(Bound::Calls));
		}
		self.calls += 1;
		let mut frame = Frame::new(method.code.clone(), node, true);
		for (slot, arg) in args.into_iter().take(7).enumerate() {
			frame.args[slot] = arg;
		}
		let saved_nesting = self.nesting;
		self.nesting = 0;
		let end = frame.code.len();
		let mut pos = 0usize;
		let outcome = self.run_list(&mut frame, &mut pos, end);
		self.nesting = saved_nesting;
		self.calls -= 1;
		// A METHOD'S OWN NAMES GO WITH IT, whatever it answered.
		for temp in frame.temps.iter().rev() {
			self.aml.ns.remove(*temp);
		}
		match outcome? {
			Flow::Return(value) => Ok(Some(value)),
			Flow::Next => Ok(None),
			Flow::Break | Flow::Continue => Err(Error::BadControl("Break or Continue outside a loop")),
		}
	}

	fn native(&mut self, native: Native, args: &[ObjRef]) -> Result<ObjRef, Error> {
		match native {
			Native::Osi => {
				let asked = args.first().map(|arg| self.deref_value(arg)).transpose()?.unwrap_or(Object::Uninitialized);
				let asked = match asked {
					Object::String(text) => text,
					other => return Err(Error::Type(format!("_OSI takes a string, not a {}", other.type_name()))),
				};
				let answer = if OSI_TRUE.contains(&asked.as_str()) { self.ones() } else { 0 };
				Ok(int(answer))
			}
		}
	}

	// ------------------------------------------------------------------ terms

	pub(crate) fn run_list(&mut self, f: &mut Frame, pos: &mut usize, end: usize) -> Result<Flow, Error> {
		while *pos < end {
			match self.term(f, pos, end)? {
				Flow::Next => {}
				other => return Ok(other),
			}
		}
		Ok(Flow::Next)
	}

	fn term(&mut self, f: &mut Frame, pos: &mut usize, end: usize) -> Result<Flow, Error> {
		self.tick()?;
		let at = *pos;
		let op = self.byte(f, at)?;
		match op {
			0x08 => {
				*pos += 1;
				let name = self.name(f, pos)?;
				let value = self.term_arg(f, pos)?;
				let value = self.detach_ref(&value);
				self.declare(f, &name, value)?;
			}
			0x10 => {
				*pos += 1;
				let scope_end = self.pkg_end(f, pos)?;
				let name = self.name(f, pos)?;
				let node = match self.resolve(f, &name) {
					Some(node) => node,
					None => self.declare(f, &name, obj(Object::Scope))?,
				};
				let saved = f.scope;
				f.scope = node;
				let flow = self.run_list(f, pos, scope_end);
				f.scope = saved;
				*pos = scope_end;
				if let Flow::Return(value) = flow? {
					return Ok(Flow::Return(value));
				}
			}
			0x14 => {
				*pos += 1;
				let method_end = self.pkg_end(f, pos)?;
				let name = self.name(f, pos)?;
				let flags = self.byte(f, *pos)?;
				*pos += 1;
				let code: Rc<[u8]> = Rc::from(&f.code[*pos..method_end]);
				self.allocate(code.len())?;
				*pos = method_end;
				let method = Method { args: flags & 0x07, serialized: flags & 0x08 != 0, sync_level: flags >> 4, code, native: None };
				self.declare(f, &name, obj(Object::Method(method)))?;
			}
			0x06 => {
				*pos += 1;
				let source = self.name(f, pos)?;
				let alias = self.name(f, pos)?;
				let target = self.resolve_or_fail(f, &source)?;
				let object = self.aml.ns.object(target).ok_or_else(|| Error::NotFound(format!("{source}")))?;
				self.declare(f, &alias, object)?;
			}
			0x15 => {
				*pos += 1;
				let name = self.name(f, pos)?;
				let kind = self.byte(f, *pos)?;
				let args = self.byte(f, *pos + 1)?;
				*pos += 2;
				if self.resolve(f, &name).is_none()
					&& let Some((parent, last)) = self.aml.ns.declaration_site(&name, f.scope)
				{
					let _ = self.aml.ns.add(parent, last, obj(Object::External { kind, args: args & 0x07 }));
				}
			}
			0x5B => return self.ext_term(f, pos, end),
			0x8A | 0x8B | 0x8C | 0x8D | 0x8F => {
				*pos += 1;
				let source = self.term_arg(f, pos)?;
				let buffer = self.buffer_holder(&source)?;
				let index = self.integer_arg(f, pos)?;
				let name = self.name(f, pos)?;
				let (bit_offset, bit_length) = match op {
					0x8A => (index * 8, 32),
					0x8B => (index * 8, 16),
					0x8C => (index * 8, 8),
					0x8D => (index, 1),
					_ => (index * 8, 64),
				};
				self.declare(f, &name, obj(Object::BufferField(BufferField { buffer, bit_offset, bit_length })))?;
			}
			0xA0 => return self.if_else(f, pos),
			0xA1 => {
				// An `Else` whose `If` was false has already been taken by `if_else`; one found here is skipped.
				*pos += 1;
				*pos = self.pkg_end(f, pos)?;
			}
			0xA2 => return self.while_loop(f, pos),
			0xA5 => {
				*pos += 1;
				return Ok(Flow::Break);
			}
			0x9F => {
				*pos += 1;
				return Ok(Flow::Continue);
			}
			0xA4 => {
				*pos += 1;
				if !f.in_method {
					return Err(Error::BadControl("Return outside a method"));
				}
				let value = self.term_arg(f, pos)?;
				let value = self.detach_ref(&value);
				return Ok(Flow::Return(value));
			}
			0xA3 | 0xCC => *pos += 1,
			0x86 => {
				*pos += 1;
				let target = self.super_name(f, pos)?;
				let value = self.integer_arg(f, pos)?;
				let node = match target {
					Target::Node(node) => node,
					Target::Reference(Reference::Node(node)) => node,
					_ => return Err(Error::Type(String::from("Notify needs a named object"))),
				};
				let path = self.aml.ns.path(node);
				self.host.notify(&path, value);
			}
			_ => {
				self.term_arg(f, pos)?;
			}
		}
		let _ = end;
		Ok(Flow::Next)
	}

	fn ext_term(&mut self, f: &mut Frame, pos: &mut usize, end: usize) -> Result<Flow, Error> {
		let at = *pos;
		let op = self.byte(f, at + 1)?;
		match op {
			0x01 => {
				*pos += 2;
				let name = self.name(f, pos)?;
				let flags = self.byte(f, *pos)?;
				*pos += 1;
				self.declare(f, &name, obj(Object::Mutex(Mutex { sync_level: flags & 0x0F, depth: 0, global: false })))?;
			}
			0x02 => {
				*pos += 2;
				let name = self.name(f, pos)?;
				self.declare(f, &name, obj(Object::Event(Event::default())))?;
			}
			0x80 => {
				*pos += 2;
				let name = self.name(f, pos)?;
				let space = Space::from_byte(self.byte(f, *pos)?);
				*pos += 1;
				let offset = self.integer_arg(f, pos)?;
				let length = self.integer_arg(f, pos)?;
				let parent = self.aml.ns.declaration_site(&name, f.scope).map(|(parent, _)| parent).unwrap_or(f.scope);
				self.declare(f, &name, obj(Object::Region(Region { space, offset, length, parent })))?;
			}
			0x88 => {
				*pos += 2;
				let name = self.name(f, pos)?;
				let signature = self.string_arg(f, pos)?;
				let oem_id = self.string_arg(f, pos)?;
				let oem_table_id = self.string_arg(f, pos)?;
				let mut sig = [0u8; 4];
				for (slot, byte) in sig.iter_mut().zip(signature.bytes()) {
					*slot = byte;
				}
				let (address, length) = self.host.table_address(sig, oem_id.as_bytes(), oem_table_id.as_bytes()).ok_or_else(|| Error::NotFound(format!("the table {signature} for a DataTableRegion")))?;
				let parent = self.aml.ns.declaration_site(&name, f.scope).map(|(parent, _)| parent).unwrap_or(f.scope);
				self.declare(f, &name, obj(Object::Region(Region { space: Space::SystemMemory, offset: address, length, parent })))?;
			}
			0x81 => {
				*pos += 2;
				let field_end = self.pkg_end(f, pos)?;
				let region_name = self.name(f, pos)?;
				let region = self.resolve_or_fail(f, &region_name)?;
				let flags = self.byte(f, *pos)?;
				*pos += 1;
				self.field_list(f, pos, field_end, |_| FieldKind::Region(region), flags)?;
				*pos = field_end;
			}
			0x86 => {
				*pos += 2;
				let field_end = self.pkg_end(f, pos)?;
				let index_name = self.name(f, pos)?;
				let data_name = self.name(f, pos)?;
				let index = self.resolve_or_fail(f, &index_name)?;
				let data = self.resolve_or_fail(f, &data_name)?;
				let flags = self.byte(f, *pos)?;
				*pos += 1;
				self.field_list(f, pos, field_end, |_| FieldKind::Index { index, data }, flags)?;
				*pos = field_end;
			}
			0x87 => {
				*pos += 2;
				let field_end = self.pkg_end(f, pos)?;
				let region_name = self.name(f, pos)?;
				let bank_name = self.name(f, pos)?;
				let region = self.resolve_or_fail(f, &region_name)?;
				let bank = self.resolve_or_fail(f, &bank_name)?;
				let value = self.integer_arg(f, pos)?;
				let flags = self.byte(f, *pos)?;
				*pos += 1;
				self.field_list(f, pos, field_end, |_| FieldKind::Bank { region, bank, value }, flags)?;
				*pos = field_end;
			}
			0x82 | 0x83 | 0x84 | 0x85 => {
				*pos += 2;
				let object_end = self.pkg_end(f, pos)?;
				let name = self.name(f, pos)?;
				let object = match op {
					0x82 => Object::Device,
					0x83 => {
						let id = self.byte(f, *pos)?;
						let block = self.bytes(f, *pos + 1, 4)? as u32;
						let block_length = self.byte(f, *pos + 5)?;
						*pos += 6;
						Object::Processor { id, block, block_length }
					}
					0x84 => {
						let system_level = self.byte(f, *pos)?;
						let order = self.bytes(f, *pos + 1, 2)? as u16;
						*pos += 3;
						Object::PowerResource { system_level, order }
					}
					_ => Object::ThermalZone,
				};
				let node = self.declare(f, &name, obj(object))?;
				let saved = f.scope;
				f.scope = node;
				let flow = self.run_list(f, pos, object_end);
				f.scope = saved;
				*pos = object_end;
				if let Flow::Return(value) = flow? {
					return Ok(Flow::Return(value));
				}
			}
			0x13 => {
				*pos += 2;
				let source = self.term_arg(f, pos)?;
				let buffer = self.buffer_holder(&source)?;
				let bit_offset = self.integer_arg(f, pos)?;
				let bit_length = self.integer_arg(f, pos)?;
				let name = self.name(f, pos)?;
				if bit_length == 0 {
					return Err(Error::Refused(format!("CreateField {name} has no bits")));
				}
				self.declare(f, &name, obj(Object::BufferField(BufferField { buffer, bit_offset, bit_length })))?;
			}
			0x21 => {
				*pos += 2;
				let us = self.integer_arg(f, pos)?;
				self.host.stall(us.min(100));
			}
			0x22 => {
				*pos += 2;
				let ms = self.integer_arg(f, pos)?;
				self.host.sleep(ms);
				self.check_time()?;
			}
			0x24 | 0x26 | 0x27 => {
				*pos += 2;
				let target = self.super_name(f, pos)?;
				let object = self.target_object(f, &target)?;
				let mut held = object.borrow_mut();
				match (op, &mut *held) {
					(0x24, Object::Event(event)) => event.signals = event.signals.saturating_add(1),
					(0x26, Object::Event(event)) => event.signals = 0,
					(0x27, Object::Mutex(mutex)) => {
						if mutex.depth == 0 {
							return Err(Error::Refused(String::from("Release of a mutex that is not held")));
						}
						mutex.depth -= 1;
						if mutex.global && mutex.depth == 0 {
							drop(held);
							self.host.global_lock(false);
						}
					}
					(_, other) => return Err(Error::Type(format!("an event or a mutex was needed, not a {}", other.type_name()))),
				}
			}
			0x32 => {
				*pos += 2;
				let kind = self.byte(f, *pos)?;
				let code = self.bytes(f, *pos + 1, 4)? as u32;
				*pos += 5;
				let argument = self.integer_arg(f, pos)?;
				self.host.fatal(kind, code, argument);
				return Err(Error::Fatal { kind, code, argument });
			}
			0x20 => {
				*pos += 2;
				let source = self.super_name(f, pos)?;
				let target = self.super_name(f, pos)?;
				let bytes = self.table_from(f, &source)?;
				let handle = self.aml.load(&bytes, self.host)?;
				let handle = self.new_object(Object::Integer(handle))?;
				self.store(f, &target, &handle)?;
			}
			0x2A => {
				*pos += 2;
				let _ = self.super_name(f, pos)?;
				return Err(Error::Refused(String::from("Unload is not supported")));
			}
			_ => {
				self.term_arg(f, pos)?;
			}
		}
		let _ = end;
		Ok(Flow::Next)
	}

	fn if_else(&mut self, f: &mut Frame, pos: &mut usize) -> Result<Flow, Error> {
		*pos += 1;
		let if_end = self.pkg_end(f, pos)?;
		let predicate = self.integer_arg(f, pos)?;
		let taken = predicate != 0;
		let mut flow = Flow::Next;
		if taken {
			flow = self.run_list(f, pos, if_end)?;
		}
		*pos = if_end;
		// THE ELSE, WHICH IS A TERM OF ITS OWN in the encoding: taken when the If was not, skipped when it was.
		if *pos < f.code.len() && self.byte(f, *pos)? == 0xA1 {
			*pos += 1;
			let else_end = self.pkg_end(f, pos)?;
			if !taken {
				flow = self.run_list(f, pos, else_end)?;
			}
			*pos = else_end;
		}
		Ok(flow)
	}

	fn while_loop(&mut self, f: &mut Frame, pos: &mut usize) -> Result<Flow, Error> {
		*pos += 1;
		let body_start_marker = *pos;
		let loop_end = self.pkg_end(f, pos)?;
		let predicate_at = *pos;
		let _ = body_start_marker;
		let mut iterations: u64 = 0;
		loop {
			let mut at = predicate_at;
			let predicate = self.integer_arg(f, &mut at)?;
			if predicate == 0 {
				break;
			}
			iterations += 1;
			if iterations > self.aml.limits.loop_iterations {
				return Err(Error::Bound(Bound::Loop));
			}
			match self.run_list(f, &mut at, loop_end)? {
				Flow::Break => break,
				Flow::Return(value) => {
					*pos = loop_end;
					return Ok(Flow::Return(value));
				}
				Flow::Next | Flow::Continue => {}
			}
		}
		*pos = loop_end;
		Ok(Flow::Next)
	}

	// ------------------------------------------------------------------ fields

	fn field_list(&mut self, f: &mut Frame, pos: &mut usize, end: usize, kind: impl Fn(()) -> FieldKind, flags: u8) -> Result<(), Error> {
		let mut bit: u64 = 0;
		let mut access = Access::from_flags(flags);
		let lock = flags & 0x10 != 0;
		let update = match (flags >> 5) & 0x03 {
			1 => Update::WriteAsOnes,
			2 => Update::WriteAsZeros,
			_ => Update::Preserve,
		};
		let mut attrib: u8 = 0;
		let mut attrib_length: u8 = 0;
		let mut connection: Option<Connection> = None;
		while *pos < end {
			match self.byte(f, *pos)? {
				0x00 => {
					*pos += 1;
					bit += self.pkg_value(f, pos)? as u64;
				}
				0x01 => {
					let kind_byte = self.byte(f, *pos + 1)?;
					let attrib_byte = self.byte(f, *pos + 2)?;
					*pos += 3;
					access = Access::from_flags(kind_byte);
					attrib = attrib_byte;
					attrib_length = 0;
				}
				0x03 => {
					let kind_byte = self.byte(f, *pos + 1)?;
					let attrib_byte = self.byte(f, *pos + 2)?;
					let length = self.byte(f, *pos + 3)?;
					*pos += 4;
					access = Access::from_flags(kind_byte);
					attrib = attrib_byte;
					attrib_length = length;
				}
				0x02 => {
					*pos += 1;
					if self.byte(f, *pos)? == 0x11 {
						let buffer = self.term_arg(f, pos)?;
						let bytes = match &*buffer.borrow() {
							Object::Buffer(bytes) => bytes.clone(),
							_ => return Err(Error::Malformed("a Connection's buffer", *pos)),
						};
						connection = Some(Connection(bytes));
					} else {
						let name = self.name(f, pos)?;
						let node = self.resolve_or_fail(f, &name)?;
						let object = self.aml.ns.object(node).ok_or_else(|| Error::NotFound(format!("{name}")))?;
						let bytes = match &*object.borrow() {
							Object::Buffer(bytes) => bytes.clone(),
							other => return Err(Error::Type(format!("a Connection names a {}, not a resource buffer", other.type_name()))),
						};
						connection = Some(Connection(bytes));
					}
				}
				_ => {
					let seg = self.seg_at(f, pos)?;
					let length = self.pkg_value(f, pos)? as u64;
					let field = Field { kind: kind(()), bit_offset: bit, bit_length: length, access, lock, update, attrib, attrib_length, connection: connection.clone() };
					bit += length;
					let name = NameString { root: false, parents: 0, segs: alloc::vec![seg] };
					self.declare(f, &name, obj(Object::Field(field)))?;
				}
			}
		}
		Ok(())
	}

	// ------------------------------------------------------------------ expressions

	fn integer_arg(&mut self, f: &mut Frame, pos: &mut usize) -> Result<u64, Error> {
		let value = self.term_arg(f, pos)?;
		let value = self.deref_value(&value)?;
		convert::to_integer(&value, self.int64())
	}

	fn string_arg(&mut self, f: &mut Frame, pos: &mut usize) -> Result<String, Error> {
		let value = self.term_arg(f, pos)?;
		let value = self.deref_value(&value)?;
		match value {
			Object::Buffer(bytes) => Ok(convert::buffer_to_string(&bytes, None)),
			other => convert::to_string(&other, self.int64()),
		}
	}

	pub(crate) fn term_arg(&mut self, f: &mut Frame, pos: &mut usize) -> Result<ObjRef, Error> {
		self.nesting += 1;
		if self.nesting > self.aml.limits.nesting {
			self.nesting -= 1;
			return Err(Error::Bound(Bound::Nesting));
		}
		let result = self.term_arg_inner(f, pos);
		self.nesting -= 1;
		result
	}

	fn term_arg_inner(&mut self, f: &mut Frame, pos: &mut usize) -> Result<ObjRef, Error> {
		self.tick()?;
		let at = *pos;
		let op = self.byte(f, at)?;
		let ones = self.ones();
		match op {
			0x00 => {
				*pos += 1;
				Ok(int(0))
			}
			0x01 => {
				*pos += 1;
				Ok(int(1))
			}
			0xFF => {
				*pos += 1;
				Ok(int(ones))
			}
			0x0A | 0x0B | 0x0C | 0x0E => {
				let width = match op {
					0x0A => 1,
					0x0B => 2,
					0x0C => 4,
					_ => 8,
				};
				let value = self.bytes(f, at + 1, width)?;
				*pos += 1 + width;
				Ok(int(self.mask(value)))
			}
			0x0D => {
				let start = at + 1;
				let length = f.code[start..].iter().position(|&byte| byte == 0).ok_or(Error::Malformed("a string with no terminator", at))?;
				let text: String = f.code[start..start + length].iter().map(|&byte| byte as char).collect();
				*pos = start + length + 1;
				self.new_object(Object::String(text))
			}
			0x11 => {
				*pos += 1;
				let buffer_end = self.pkg_end(f, pos)?;
				let size = self.integer_arg(f, pos)? as usize;
				if size > self.aml.limits.memory {
					return Err(Error::Bound(Bound::Memory));
				}
				let initial = &f.code[*pos..buffer_end];
				let mut bytes = alloc::vec![0u8; size.max(initial.len())];
				bytes[..initial.len()].copy_from_slice(initial);
				*pos = buffer_end;
				self.new_object(Object::Buffer(bytes))
			}
			0x12 | 0x13 => self.package(f, pos),
			0x60..=0x67 => {
				*pos += 1;
				Ok(f.locals[(op - 0x60) as usize].clone())
			}
			0x68..=0x6E => {
				*pos += 1;
				Ok(f.args[(op - 0x68) as usize].clone())
			}
			0x5B => self.ext_expression(f, pos),
			0x70 => {
				*pos += 1;
				let value = self.term_arg(f, pos)?;
				let target = self.super_name(f, pos)?;
				let value = self.read_through(&value)?;
				self.store(f, &target, &value)?;
				Ok(value)
			}
			0x71 => {
				*pos += 1;
				let target = self.super_name(f, pos)?;
				let reference = self.reference_to(f, &target)?;
				self.new_object(Object::Reference(reference))
			}
			0x72 | 0x74 | 0x77 | 0x79 | 0x7A | 0x7B | 0x7C | 0x7D | 0x7E | 0x7F | 0x85 => {
				*pos += 1;
				let left = self.integer_arg(f, pos)?;
				let right = self.integer_arg(f, pos)?;
				let bits = if self.int64() { 64 } else { 32 };
				let value = match op {
					0x72 => left.wrapping_add(right),
					0x74 => left.wrapping_sub(right),
					0x77 => left.wrapping_mul(right),
					0x79 => {
						if right >= bits {
							0
						} else {
							left << right
						}
					}
					0x7A => {
						if right >= bits {
							0
						} else {
							left >> right
						}
					}
					0x7B => left & right,
					0x7C => !(left & right),
					0x7D => left | right,
					0x7E => !(left | right),
					0x7F => left ^ right,
					_ => {
						if right == 0 {
							return Err(Error::DivideByZero);
						}
						left % right
					}
				};
				let result = int(self.mask(value));
				let target = self.super_name(f, pos)?;
				self.store(f, &target, &result)?;
				Ok(result)
			}
			0x78 => {
				*pos += 1;
				let dividend = self.integer_arg(f, pos)?;
				let divisor = self.integer_arg(f, pos)?;
				if divisor == 0 {
					return Err(Error::DivideByZero);
				}
				let remainder = int(dividend % divisor);
				let quotient = int(dividend / divisor);
				let remainder_target = self.super_name(f, pos)?;
				let quotient_target = self.super_name(f, pos)?;
				self.store(f, &remainder_target, &remainder)?;
				self.store(f, &quotient_target, &quotient)?;
				Ok(quotient)
			}
			0x75 | 0x76 => {
				*pos += 1;
				let target = self.super_name(f, pos)?;
				let current = self.target_value(f, &target)?;
				let current = convert::to_integer(&current, self.int64())?;
				let next = if op == 0x75 { current.wrapping_add(1) } else { current.wrapping_sub(1) };
				let result = int(self.mask(next));
				self.store(f, &target, &result)?;
				Ok(result)
			}
			0x80 | 0x81 | 0x82 => {
				*pos += 1;
				let operand = self.integer_arg(f, pos)?;
				let value = match op {
					0x80 => self.mask(!operand),
					0x81 => {
						if operand == 0 {
							0
						} else {
							64 - operand.leading_zeros() as u64
						}
					}
					_ => {
						if operand == 0 {
							0
						} else {
							operand.trailing_zeros() as u64 + 1
						}
					}
				};
				let result = int(value);
				let target = self.super_name(f, pos)?;
				self.store(f, &target, &result)?;
				Ok(result)
			}
			0x73 | 0x84 => {
				*pos += 1;
				let left = self.term_arg(f, pos)?;
				let left = self.deref_value(&left)?;
				let right = self.term_arg(f, pos)?;
				let right = self.deref_value(&right)?;
				let joined = if op == 0x73 { self.concatenate(&left, &right)? } else { self.concatenate_resources(&left, &right)? };
				let result = self.new_object(joined)?;
				let target = self.super_name(f, pos)?;
				self.store(f, &target, &result)?;
				Ok(result)
			}
			0x83 => {
				*pos += 1;
				let operand = self.term_arg(f, pos)?;
				self.dereference(f, &operand)
			}
			0x87 => {
				*pos += 1;
				let target = self.super_name(f, pos)?;
				let value = self.target_object(f, &target)?;
				let value = self.follow(&value)?;
				let size = match &*value.borrow() {
					Object::String(text) => text.len() as u64,
					Object::Buffer(bytes) => bytes.len() as u64,
					Object::Package(elements) => elements.len() as u64,
					other => return Err(Error::Type(format!("SizeOf a {}", other.type_name()))),
				};
				Ok(int(size))
			}
			0x88 => {
				*pos += 1;
				let source = self.term_arg(f, pos)?;
				let container = self.follow(&source)?;
				let index = self.integer_arg(f, pos)?;
				let length = match &*container.borrow() {
					Object::Package(elements) => elements.len(),
					Object::Buffer(bytes) => bytes.len(),
					Object::String(text) => text.len(),
					other => return Err(Error::Type(format!("Index into a {}", other.type_name()))),
				};
				if index >= length as u64 {
					return Err(Error::Index(index));
				}
				let reference = self.new_object(Object::Reference(Reference::Element { container, index: index as usize }))?;
				let target = self.super_name(f, pos)?;
				self.store(f, &target, &reference)?;
				Ok(reference)
			}
			0x89 => {
				*pos += 1;
				let package = self.term_arg(f, pos)?;
				let package = self.follow(&package)?;
				let first_op = self.byte(f, *pos)?;
				*pos += 1;
				let first = self.term_arg(f, pos)?;
				let first = self.deref_value(&first)?;
				let second_op = self.byte(f, *pos)?;
				*pos += 1;
				let second = self.term_arg(f, pos)?;
				let second = self.deref_value(&second)?;
				let start = self.integer_arg(f, pos)? as usize;
				let elements = match &*package.borrow() {
					Object::Package(elements) => elements.clone(),
					other => return Err(Error::Type(format!("Match over a {}", other.type_name()))),
				};
				for (index, element) in elements.iter().enumerate().skip(start) {
					let value = self.deref_value(element)?;
					if !value.is_data() || matches!(value, Object::Package(_)) {
						continue;
					}
					if self.match_one(first_op, &value, &first)? && self.match_one(second_op, &value, &second)? {
						return Ok(int(index as u64));
					}
				}
				Ok(int(ones))
			}
			0x8E => {
				*pos += 1;
				let target = self.super_name(f, pos)?;
				let object = self.target_object(f, &target)?;
				let object = self.follow(&object)?;
				let code = object.borrow().type_code();
				Ok(int(code))
			}
			0x90 | 0x91 => {
				*pos += 1;
				let left = self.integer_arg(f, pos)?;
				let right = self.integer_arg(f, pos)?;
				let truth = if op == 0x90 { left != 0 && right != 0 } else { left != 0 || right != 0 };
				Ok(int(if truth { ones } else { 0 }))
			}
			0x92 => {
				*pos += 1;
				let operand = self.integer_arg(f, pos)?;
				Ok(int(if operand == 0 { ones } else { 0 }))
			}
			0x93..=0x95 => {
				*pos += 1;
				let left = self.term_arg(f, pos)?;
				let left = self.deref_value(&left)?;
				let right = self.term_arg(f, pos)?;
				let right = self.deref_value(&right)?;
				let order = self.compare(&left, &right)?;
				let truth = match op {
					0x93 => order == core::cmp::Ordering::Equal,
					0x94 => order == core::cmp::Ordering::Greater,
					_ => order == core::cmp::Ordering::Less,
				};
				Ok(int(if truth { ones } else { 0 }))
			}
			0x96..=0x99 | 0x9C => {
				*pos += 1;
				let operand = self.term_arg(f, pos)?;
				let operand = self.deref_value(&operand)?;
				let int64 = self.int64();
				let converted = match op {
					0x96 => Object::Buffer(convert::to_buffer(&operand, int64)?),
					0x97 => Object::String(convert::decimal_string(&operand, int64)?),
					0x98 => Object::String(convert::hex_string(&operand, int64)?),
					0x99 => Object::Integer(convert::explicit_integer(&operand, int64)?),
					_ => {
						let length = self.integer_arg(f, pos)?;
						let bytes = match &operand {
							Object::Buffer(bytes) => bytes.clone(),
							other => return Err(Error::Type(format!("ToString of a {}", other.type_name()))),
						};
						let limit = if length == ones { None } else { Some(length as usize) };
						Object::String(convert::buffer_to_string(&bytes, limit))
					}
				};
				let result = self.new_object(converted)?;
				let target = self.super_name(f, pos)?;
				self.store(f, &target, &result)?;
				Ok(result)
			}
			0x9D => {
				*pos += 1;
				let value = self.term_arg(f, pos)?;
				let value = self.read_through(&value)?;
				let target = self.super_name(f, pos)?;
				self.copy_object(f, &target, &value)?;
				Ok(value)
			}
			0x9E => {
				*pos += 1;
				let source = self.term_arg(f, pos)?;
				let source = self.deref_value(&source)?;
				let index = self.integer_arg(f, pos)? as usize;
				let length = self.integer_arg(f, pos)? as usize;
				let result = match source {
					Object::String(text) => {
						let bytes = text.as_bytes();
						let start = index.min(bytes.len());
						let stop = start.saturating_add(length).min(bytes.len());
						Object::String(bytes[start..stop].iter().map(|&byte| byte as char).collect())
					}
					Object::Buffer(bytes) => {
						let start = index.min(bytes.len());
						let stop = start.saturating_add(length).min(bytes.len());
						Object::Buffer(bytes[start..stop].to_vec())
					}
					other => return Err(Error::Type(format!("Mid of a {}", other.type_name()))),
				};
				let result = self.new_object(result)?;
				let target = self.super_name(f, pos)?;
				self.store(f, &target, &result)?;
				Ok(result)
			}
			_ if NameString::starts(op) => {
				let name = self.name(f, pos)?;
				let node = self.resolve_or_fail(f, &name)?;
				let object = self.aml.ns.object(node).ok_or_else(|| Error::NotFound(format!("{name}")))?;
				let (method, external_args) = match &*object.borrow() {
					Object::Method(method) => (Some(method.clone()), None),
					Object::External { kind: 8, args } => (None, Some(*args)),
					_ => (None, None),
				};
				if let Some(method) = method {
					let mut args = Vec::with_capacity(method.args as usize);
					for _ in 0..method.args {
						let arg = self.term_arg(f, pos)?;
						args.push(self.pass(&arg)?);
					}
					let returned = self.call(node, &method, args)?;
					return Ok(returned.unwrap_or_else(|| int(0)));
				}
				if external_args.is_some() {
					return Err(Error::NotFound(format!("the method {name}, declared External and never defined")));
				}
				self.value_of_node(node)
			}
			_ => Err(Error::UnknownOpcode(op as u16, at)),
		}
	}

	fn ext_expression(&mut self, f: &mut Frame, pos: &mut usize) -> Result<ObjRef, Error> {
		let at = *pos;
		let op = self.byte(f, at + 1)?;
		let ones = self.ones();
		match op {
			0x30 => {
				*pos += 2;
				Ok(int(REVISION))
			}
			0x31 => {
				*pos += 2;
				Ok(obj(Object::Debug))
			}
			0x33 => {
				*pos += 2;
				Ok(int(self.host.timer()))
			}
			0x12 => {
				*pos += 2;
				let (found, reference) = self.cond_super_name(f, pos)?;
				let target = self.super_name(f, pos)?;
				if let (true, Some(reference)) = (found, reference) {
					let value = self.new_object(Object::Reference(reference))?;
					self.store(f, &target, &value)?;
					return Ok(int(ones));
				}
				Ok(int(0))
			}
			0x23 => {
				*pos += 2;
				let target = self.super_name(f, pos)?;
				let timeout = self.bytes(f, *pos, 2)?;
				*pos += 2;
				let object = self.target_object(f, &target)?;
				let global = match &*object.borrow() {
					Object::Mutex(mutex) => mutex.global,
					other => return Err(Error::Type(format!("Acquire of a {}", other.type_name()))),
				};
				if global && object.borrow().clone().type_code() == object::kind::MUTEX {
					let depth = match &*object.borrow() {
						Object::Mutex(mutex) => mutex.depth,
						_ => 0,
					};
					if depth == 0 && !self.acquire_global(timeout)? {
						return Ok(int(ones));
					}
				}
				if let Object::Mutex(mutex) = &mut *object.borrow_mut() {
					mutex.depth += 1;
				}
				Ok(int(0))
			}
			0x25 => {
				*pos += 2;
				let target = self.super_name(f, pos)?;
				let timeout = self.integer_arg(f, pos)?;
				let object = self.target_object(f, &target)?;
				let mut held = object.borrow_mut();
				let Object::Event(event) = &mut *held else { return Err(Error::Type(String::from("Wait needs an event"))) };
				if event.signals > 0 {
					event.signals -= 1;
					return Ok(int(0));
				}
				drop(held);
				// NOTHING ELSE RUNS WHILE THIS WAITS: an event not signalled now will not be, so the wait is its timeout.
				if timeout >= 0xFFFF {
					return Err(Error::Refused(String::from("a Wait with no timeout on an event nothing can signal")));
				}
				self.host.sleep(timeout);
				self.check_time()?;
				Ok(int(ones))
			}
			0x28 | 0x29 => {
				*pos += 2;
				let operand = self.integer_arg(f, pos)?;
				let value = if op == 0x28 { convert::bcd_to_integer(operand) } else { convert::integer_to_bcd(operand) };
				let result = int(self.mask(value));
				let target = self.super_name(f, pos)?;
				self.store(f, &target, &result)?;
				Ok(result)
			}
			0x1F => {
				*pos += 2;
				let signature = self.string_arg(f, pos)?;
				let oem_id = self.string_arg(f, pos)?;
				let oem_table_id = self.string_arg(f, pos)?;
				let root_path = self.string_arg(f, pos)?;
				let parameter_path = self.string_arg(f, pos)?;
				let parameter = self.term_arg(f, pos)?;
				let mut sig = [0u8; 4];
				for (slot, byte) in sig.iter_mut().zip(signature.bytes()) {
					*slot = byte;
				}
				let Some(table) = self.host.table(sig, oem_id.as_bytes(), oem_table_id.as_bytes()) else { return Ok(int(0)) };
				if !root_path.is_empty() && root_path != "\\" {
					return Err(Error::Refused(format!("LoadTable into {root_path}: tables load at the root here")));
				}
				let handle = self.aml.load(&table.bytes, self.host)?;
				if !parameter_path.is_empty()
					&& let Some(name) = NameString::parse(&parameter_path)
					&& let Some(node) = self.aml.ns.resolve(&name, ROOT)
				{
					let value = self.read_through(&parameter)?;
					self.store(f, &Target::Node(node), &value)?;
				}
				Ok(int(handle))
			}
			_ => Err(Error::UnknownOpcode(0x5B00 | op as u16, at)),
		}
	}

	fn acquire_global(&mut self, timeout: u64) -> Result<bool, Error> {
		let mut waited: u64 = 0;
		loop {
			if self.host.global_lock(true) {
				return Ok(true);
			}
			if timeout != 0xFFFF && waited >= timeout {
				return Ok(false);
			}
			self.host.sleep(1);
			waited += 1;
			self.check_time()?;
		}
	}

	fn package(&mut self, f: &mut Frame, pos: &mut usize) -> Result<ObjRef, Error> {
		let variable = self.byte(f, *pos)? == 0x13;
		*pos += 1;
		let package_end = self.pkg_end(f, pos)?;
		let count = if variable {
			self.integer_arg(f, pos)? as usize
		} else {
			let count = self.byte(f, *pos)? as usize;
			*pos += 1;
			count
		};
		if count > self.aml.limits.memory / 16 {
			return Err(Error::Bound(Bound::Memory));
		}
		self.package_depth += 1;
		if self.package_depth > self.aml.limits.package_depth {
			self.package_depth -= 1;
			return Err(Error::Bound(Bound::PackageDepth));
		}
		let mut elements: Vec<ObjRef> = Vec::with_capacity(count);
		let outcome: Result<(), Error> = (|| {
			while *pos < package_end {
				let op = self.byte(f, *pos)?;
				let element = if NameString::starts(op) {
					let name = self.name(f, pos)?;
					obj(Object::Name { name, scope: f.scope })
				} else {
					let value = self.term_arg(f, pos)?;
					obj(object::copy(&value.borrow()))
				};
				elements.push(element);
			}
			Ok(())
		})();
		self.package_depth -= 1;
		outcome?;
		*pos = package_end;
		while elements.len() < count {
			elements.push(obj(Object::Uninitialized));
		}
		self.new_object(Object::Package(elements))
	}

	// ------------------------------------------------------------------ targets

	pub(crate) fn super_name(&mut self, f: &mut Frame, pos: &mut usize) -> Result<Target, Error> {
		let at = *pos;
		let op = self.byte(f, at)?;
		match op {
			0x00 => {
				*pos += 1;
				Ok(Target::Null)
			}
			0x60..=0x67 => {
				*pos += 1;
				Ok(Target::Local((op - 0x60) as usize))
			}
			0x68..=0x6E => {
				*pos += 1;
				Ok(Target::Arg((op - 0x68) as usize))
			}
			0x5B if self.byte(f, at + 1)? == 0x31 => {
				*pos += 2;
				Ok(Target::Debug)
			}
			0x83 => {
				*pos += 1;
				let operand = self.term_arg(f, pos)?;
				let followed = self.follow_once(&operand)?;
				match followed {
					Some(reference) => Ok(Target::Reference(reference)),
					None => {
						let text = match &*operand.borrow() {
							Object::String(text) => text.clone(),
							other => return Err(Error::Type(format!("DerefOf a {} as a target", other.type_name()))),
						};
						let name = NameString::parse(&text).ok_or_else(|| Error::NotFound(text.clone()))?;
						Ok(Target::Node(self.resolve_or_fail(f, &name)?))
					}
				}
			}
			_ if NameString::starts(op) => {
				let name = self.name(f, pos)?;
				Ok(Target::Node(self.resolve_or_fail(f, &name)?))
			}
			_ => {
				let value = self.term_arg(f, pos)?;
				match &*value.borrow() {
					Object::Reference(reference) => Ok(Target::Reference(reference.clone())),
					other => Err(Error::Type(format!("a {} cannot be stored into", other.type_name()))),
				}
			}
		}
	}

	/// `CondRefOf`'s operand: whether it names something, and the reference to it.
	fn cond_super_name(&mut self, f: &mut Frame, pos: &mut usize) -> Result<(bool, Option<Reference>), Error> {
		let op = self.byte(f, *pos)?;
		if NameString::starts(op) {
			let name = self.name(f, pos)?;
			return Ok(match self.resolve(f, &name) {
				Some(node) => {
					let placeholder = matches!(self.aml.ns.object(node).map(|object| object.borrow().clone()), Some(Object::External { .. }));
					if placeholder { (false, None) } else { (true, Some(Reference::Node(node))) }
				}
				None => (false, None),
			});
		}
		let target = self.super_name(f, pos)?;
		let reference = self.reference_to(f, &target)?;
		Ok((true, Some(reference)))
	}

	fn reference_to(&mut self, f: &Frame, target: &Target) -> Result<Reference, Error> {
		Ok(match target {
			Target::Node(node) => Reference::Node(*node),
			Target::Local(slot) => Reference::Object(f.locals[*slot].clone()),
			Target::Arg(slot) => match &*f.args[*slot].borrow() {
				Object::Reference(reference) => reference.clone(),
				_ => Reference::Object(f.args[*slot].clone()),
			},
			Target::Reference(reference) => reference.clone(),
			Target::Debug => Reference::Object(obj(Object::Debug)),
			Target::Null => return Err(Error::Type(String::from("RefOf nothing"))),
		})
	}

	/// The object a target holds, references followed to what they name.
	fn target_object(&mut self, f: &Frame, target: &Target) -> Result<ObjRef, Error> {
		Ok(match target {
			Target::Node(node) => self.aml.ns.object(*node).ok_or_else(|| Error::NotFound(format!("node {node}")))?,
			Target::Local(slot) => f.locals[*slot].clone(),
			Target::Arg(slot) => {
				let arg = f.args[*slot].clone();
				let reference = match &*arg.borrow() {
					Object::Reference(reference) => Some(reference.clone()),
					_ => None,
				};
				match reference {
					Some(reference) => self.reference_object(&reference)?,
					None => arg,
				}
			}
			Target::Reference(reference) => self.reference_object(reference)?,
			Target::Debug => obj(Object::Debug),
			Target::Null => obj(Object::Uninitialized),
		})
	}

	/// A target's VALUE: fields read, references followed.
	fn target_value(&mut self, f: &Frame, target: &Target) -> Result<Object, Error> {
		if let Target::Node(node) = target {
			let value = self.value_of_node(*node)?;
			return Ok(value.borrow().clone());
		}
		let object = self.target_object(f, target)?;
		self.deref_value(&object)
	}

	fn reference_object(&mut self, reference: &Reference) -> Result<ObjRef, Error> {
		Ok(match reference {
			Reference::Node(node) => self.aml.ns.object(*node).ok_or_else(|| Error::NotFound(format!("node {node}")))?,
			Reference::Object(object) => object.clone(),
			Reference::Element { container, index } => {
				let element = match &*container.borrow() {
					Object::Package(elements) => Some(elements.get(*index).cloned().ok_or(Error::Index(*index as u64))?),
					Object::Buffer(bytes) => return Ok(int(*bytes.get(*index).ok_or(Error::Index(*index as u64))? as u64)),
					Object::String(text) => return Ok(int(*text.as_bytes().get(*index).ok_or(Error::Index(*index as u64))? as u64)),
					other => return Err(Error::Type(format!("an element of a {}", other.type_name()))),
				};
				element.unwrap_or_else(|| obj(Object::Uninitialized))
			}
		})
	}

	// ------------------------------------------------------------------ values

	/// The value a named object stands for when it is used: a field read, a package element's name resolved,
	/// anything else itself.
	pub(crate) fn value_of_node(&mut self, node: NodeId) -> Result<ObjRef, Error> {
		let object = self.aml.ns.object(node).ok_or_else(|| Error::NotFound(format!("node {node}")))?;
		let field = match &*object.borrow() {
			Object::Field(field) => Some(Ok(field.clone())),
			Object::BufferField(field) => Some(Err(field.clone())),
			_ => None,
		};
		match field {
			Some(Ok(field)) => {
				let value = self.read_field(&field)?;
				self.new_object(value)
			}
			Some(Err(field)) => {
				let value = self.read_buffer_field(&field)?;
				self.new_object(value)
			}
			None => Ok(object),
		}
	}

	/// Follow a reference - or a package element's name - to the object it names; anything else is itself.
	fn follow(&mut self, value: &ObjRef) -> Result<ObjRef, Error> {
		let mut current = value.clone();
		for _ in 0..16 {
			let next = match &*current.borrow() {
				Object::Reference(reference) => Some(Ok(reference.clone())),
				Object::Name { name, scope } => Some(Err((name.clone(), *scope))),
				_ => None,
			};
			current = match next {
				Some(Ok(reference)) => self.reference_object(&reference)?,
				Some(Err((name, scope))) => {
					let node = self.aml.ns.resolve(&name, scope).ok_or_else(|| Error::NotFound(format!("{name}")))?;
					self.aml.ns.object(node).ok_or_else(|| Error::NotFound(format!("{name}")))?
				}
				None => return Ok(current),
			};
		}
		Err(Error::Type(String::from("a chain of references too long to follow")))
	}

	fn follow_once(&mut self, value: &ObjRef) -> Result<Option<Reference>, Error> {
		Ok(match &*value.borrow() {
			Object::Reference(reference) => Some(reference.clone()),
			_ => None,
		})
	}

	/// An operand's DATA VALUE: references followed, fields read.
	pub(crate) fn deref_value(&mut self, value: &ObjRef) -> Result<Object, Error> {
		let followed = self.follow(value)?;
		let field = match &*followed.borrow() {
			Object::Field(field) => Some(Ok(field.clone())),
			Object::BufferField(field) => Some(Err(field.clone())),
			_ => None,
		};
		match field {
			Some(Ok(field)) => self.read_field(&field),
			Some(Err(field)) => self.read_buffer_field(&field),
			None => Ok(followed.borrow().clone()),
		}
	}

	/// A value about to be stored or copied: a field read, a reference KEPT as a reference.
	fn read_through(&mut self, value: &ObjRef) -> Result<ObjRef, Error> {
		let field = match &*value.borrow() {
			Object::Field(field) => Some(Ok(field.clone())),
			Object::BufferField(field) => Some(Err(field.clone())),
			_ => None,
		};
		match field {
			Some(Ok(field)) => {
				let value = self.read_field(&field)?;
				self.new_object(value)
			}
			Some(Err(field)) => {
				let value = self.read_buffer_field(&field)?;
				self.new_object(value)
			}
			None => Ok(value.clone()),
		}
	}

	/// An argument as a method receives it: integers and strings by value, buffers and packages shared with the
	/// caller, references as they are.
	fn pass(&mut self, value: &ObjRef) -> Result<ObjRef, Error> {
		let value = self.read_through(value)?;
		let shared = matches!(&*value.borrow(), Object::Buffer(_) | Object::Package(_) | Object::Reference(_));
		if shared { Ok(value) } else { Ok(obj(object::copy(&value.borrow()))) }
	}

	/// What a `Name` or a `Return` keeps: its own copy of a data object, a reference as it is.
	fn detach_ref(&self, value: &ObjRef) -> ObjRef {
		let keep = matches!(&*value.borrow(), Object::Reference(_));
		if keep { value.clone() } else { obj(object::copy(&value.borrow())) }
	}

	/// A result handed out of the interpreter: a copy, package elements that are names left as names.
	pub(crate) fn detach(&mut self, value: &ObjRef) -> Object {
		object::copy(&value.borrow())
	}

	fn dereference(&mut self, f: &Frame, operand: &ObjRef) -> Result<ObjRef, Error> {
		let reference = match &*operand.borrow() {
			Object::Reference(reference) => Some(reference.clone()),
			Object::String(_) => None,
			other => return Err(Error::Type(format!("DerefOf a {}", other.type_name()))),
		};
		match reference {
			Some(Reference::Node(node)) => self.value_of_node(node),
			Some(reference) => {
				let object = self.reference_object(&reference)?;
				let followed = self.follow(&object)?;
				Ok(followed)
			}
			None => {
				let text = match &*operand.borrow() {
					Object::String(text) => text.clone(),
					_ => String::new(),
				};
				let name = NameString::parse(&text).ok_or_else(|| Error::NotFound(text.clone()))?;
				let node = self.resolve_or_fail(f, &name)?;
				self.value_of_node(node)
			}
		}
	}

	/// The object a `Create...Field` carves from: the buffer itself, so the field sees its later changes.
	fn buffer_holder(&mut self, source: &ObjRef) -> Result<ObjRef, Error> {
		let followed = self.follow(source)?;
		let is_buffer = matches!(&*followed.borrow(), Object::Buffer(_));
		if is_buffer {
			return Ok(followed);
		}
		let value = self.deref_value(&followed)?;
		match value {
			Object::Buffer(bytes) => Ok(obj(Object::Buffer(bytes))),
			other => Err(Error::Type(format!("a buffer field over a {}", other.type_name()))),
		}
	}

	fn concatenate(&self, left: &Object, right: &Object) -> Result<Object, Error> {
		let int64 = self.int64();
		Ok(match left {
			Object::Integer(_) => {
				let mut bytes = convert::to_buffer(left, int64)?;
				let right = convert::to_integer(right, int64)?;
				bytes.extend_from_slice(&convert::to_buffer(&Object::Integer(right), int64)?);
				Object::Buffer(bytes)
			}
			Object::String(text) => {
				let tail = match right {
					Object::Buffer(bytes) => convert::buffer_to_string(bytes, None),
					other => convert::to_string(other, int64)?,
				};
				Object::String(format!("{text}{tail}"))
			}
			Object::Buffer(bytes) => {
				let mut joined = bytes.clone();
				joined.extend_from_slice(&convert::to_buffer(right, int64)?);
				Object::Buffer(joined)
			}
			other => return Err(Error::Type(format!("Concatenate a {}", other.type_name()))),
		})
	}

	fn concatenate_resources(&self, left: &Object, right: &Object) -> Result<Object, Error> {
		let strip = |object: &Object| -> Result<Vec<u8>, Error> {
			match object {
				Object::Buffer(bytes) => {
					let mut bytes = bytes.clone();
					if bytes.len() >= 2 && bytes[bytes.len() - 2] == 0x79 {
						bytes.truncate(bytes.len() - 2);
					}
					Ok(bytes)
				}
				other => Err(Error::Type(format!("ConcatenateResTemplate of a {}", other.type_name()))),
			}
		};
		let mut joined = strip(left)?;
		joined.extend_from_slice(&strip(right)?);
		joined.extend_from_slice(&[0x79, 0x00]);
		Ok(Object::Buffer(joined))
	}

	fn compare(&self, left: &Object, right: &Object) -> Result<core::cmp::Ordering, Error> {
		let int64 = self.int64();
		Ok(match left {
			Object::Integer(value) => value.cmp(&convert::to_integer(right, int64)?),
			Object::String(text) => {
				let other = match right {
					Object::Buffer(bytes) => convert::buffer_to_string(bytes, None),
					other => convert::to_string(other, int64)?,
				};
				text.as_bytes().cmp(other.as_bytes())
			}
			Object::Buffer(bytes) => bytes.as_slice().cmp(convert::to_buffer(right, int64)?.as_slice()),
			other => return Err(Error::Type(format!("a comparison of a {}", other.type_name()))),
		})
	}

	fn match_one(&self, op: u8, element: &Object, against: &Object) -> Result<bool, Error> {
		if op == 0 {
			return Ok(true);
		}
		let converted = match element {
			Object::Integer(_) => Object::Integer(convert::to_integer(against, self.int64())?),
			Object::String(_) => Object::String(convert::to_string(against, self.int64())?),
			Object::Buffer(_) => Object::Buffer(convert::to_buffer(against, self.int64())?),
			_ => return Ok(false),
		};
		let order = self.compare(element, &converted)?;
		Ok(match op {
			1 => order == core::cmp::Ordering::Equal,
			2 => order != core::cmp::Ordering::Greater,
			3 => order == core::cmp::Ordering::Less,
			4 => order != core::cmp::Ordering::Less,
			5 => order == core::cmp::Ordering::Greater,
			_ => false,
		})
	}

	// ------------------------------------------------------------------ stores

	pub(crate) fn store(&mut self, f: &mut Frame, target: &Target, value: &ObjRef) -> Result<(), Error> {
		match target {
			Target::Null => Ok(()),
			Target::Local(slot) => {
				let copied = self.detach_ref(value);
				f.locals[*slot] = copied;
				Ok(())
			}
			Target::Arg(slot) => {
				let reference = match &*f.args[*slot].borrow() {
					Object::Reference(reference) => Some(reference.clone()),
					_ => None,
				};
				match reference {
					Some(reference) => self.store_reference(&reference, value),
					None => {
						let copied = self.detach_ref(value);
						f.args[*slot] = copied;
						Ok(())
					}
				}
			}
			Target::Debug => {
				let text = self.render(value);
				self.host.debug(&text);
				Ok(())
			}
			Target::Node(node) => self.store_node(*node, value),
			Target::Reference(reference) => self.store_reference(reference, value),
		}
	}

	fn store_reference(&mut self, reference: &Reference, value: &ObjRef) -> Result<(), Error> {
		match reference {
			Reference::Node(node) => self.store_node(*node, value),
			Reference::Object(object) => {
				let copied = object::copy(&value.borrow());
				*object.borrow_mut() = copied;
				Ok(())
			}
			Reference::Element { container, index } => {
				let data = self.deref_value(value)?;
				let int64 = self.int64();
				let mut held = container.borrow_mut();
				match &mut *held {
					Object::Package(elements) => {
						let slot = elements.get(*index).ok_or(Error::Index(*index as u64))?;
						let copied = match &*value.borrow() {
							Object::Reference(_) => value.borrow().clone(),
							_ => object::copy(&data),
						};
						*slot.borrow_mut() = copied;
					}
					Object::Buffer(bytes) => {
						let byte = convert::to_integer(&data, int64)? as u8;
						*bytes.get_mut(*index).ok_or(Error::Index(*index as u64))? = byte;
					}
					Object::String(text) => {
						let byte = convert::to_integer(&data, int64)? as u8;
						let mut bytes = text.as_bytes().to_vec();
						*bytes.get_mut(*index).ok_or(Error::Index(*index as u64))? = byte;
						*text = bytes.iter().map(|&byte| byte as char).collect();
					}
					other => return Err(Error::Type(format!("a store into an element of a {}", other.type_name()))),
				}
				Ok(())
			}
		}
	}

	/// A STORE INTO A NAMED OBJECT: a field written, an integer, string or buffer converted to the type it already
	/// has, anything else replaced by a copy.
	fn store_node(&mut self, node: NodeId, value: &ObjRef) -> Result<(), Error> {
		let target = self.aml.ns.object(node).ok_or_else(|| Error::NotFound(format!("node {node}")))?;
		enum Plan {
			Field(Field),
			BufferField(BufferField),
			Convert,
			Replace,
			Refuse(&'static str),
		}
		let plan = match &*target.borrow() {
			Object::Field(field) => Plan::Field(field.clone()),
			Object::BufferField(field) => Plan::BufferField(field.clone()),
			Object::Integer(_) | Object::String(_) | Object::Buffer(_) => Plan::Convert,
			Object::Package(_) | Object::Uninitialized | Object::Reference(_) | Object::Name { .. } | Object::External { .. } => Plan::Replace,
			other => Plan::Refuse(other.type_name()),
		};
		match plan {
			Plan::Field(field) => {
				let data = self.deref_value(value)?;
				self.write_field(&field, &data)
			}
			Plan::BufferField(field) => {
				let data = self.deref_value(value)?;
				self.write_buffer_field(&field, &data)
			}
			Plan::Convert => {
				let data = self.deref_value(value)?;
				let converted = convert::to_target_type(&target.borrow(), &data, self.int64())?;
				*target.borrow_mut() = converted;
				Ok(())
			}
			Plan::Replace => {
				let copied = match &*value.borrow() {
					Object::Reference(_) => value.borrow().clone(),
					_ => object::copy(&self.deref_value(value)?),
				};
				*target.borrow_mut() = copied;
				Ok(())
			}
			Plan::Refuse(kind) => Err(Error::Type(format!("a store into a {kind} ({})", self.aml.ns.path(node)))),
		}
	}

	/// `CopyObject`: the target REPLACED by a copy, whatever it was - no conversion, a field included.
	fn copy_object(&mut self, f: &mut Frame, target: &Target, value: &ObjRef) -> Result<(), Error> {
		let copied = obj(object::copy(&value.borrow()));
		match target {
			Target::Null | Target::Debug => Ok(()),
			Target::Local(slot) => {
				f.locals[*slot] = copied;
				Ok(())
			}
			Target::Arg(slot) => {
				f.args[*slot] = copied;
				Ok(())
			}
			Target::Node(node) => {
				self.aml.ns.set_object(*node, copied);
				Ok(())
			}
			Target::Reference(reference) => self.store_reference(reference, value),
		}
	}

	fn render(&mut self, value: &ObjRef) -> String {
		match self.deref_value(value) {
			Ok(Object::Integer(value)) => format!("{value:#x}"),
			Ok(Object::String(text)) => text,
			Ok(Object::Buffer(bytes)) => format!("buffer of {} bytes", bytes.len()),
			Ok(Object::Package(elements)) => format!("package of {} elements", elements.len()),
			Ok(other) => String::from(other.type_name()),
			Err(error) => format!("{error}"),
		}
	}

	/// The bytes of a table `Load` reads: from a buffer, or from a region or field through the host.
	fn table_from(&mut self, f: &Frame, source: &Target) -> Result<Vec<u8>, Error> {
		let object = self.target_object(f, source)?;
		let region = match &*object.borrow() {
			Object::Buffer(bytes) => return Ok(bytes.clone()),
			Object::Region(region) => region.clone(),
			Object::Field(_) | Object::BufferField(_) => Region { space: Space::Other(0xFF), offset: 0, length: 0, parent: ROOT },
			other => return Err(Error::Type(format!("Load from a {}", other.type_name()))),
		};
		if region.space == Space::Other(0xFF) {
			return match self.deref_value(&object)? {
				Object::Buffer(bytes) => Ok(bytes),
				_ => Err(Error::Type(String::from("Load from a field that is not a table"))),
			};
		}
		if region.space != Space::SystemMemory {
			return Err(Error::Refused(format!("Load from a {} region", region.space.name())));
		}
		self.announce(&region);
		let mut header = [0u8; 8];
		for (at, slot) in header.iter_mut().enumerate() {
			*slot = self.host.read(crate::host::Access { space: Space::SystemMemory, address: region.offset + at as u64, width: 8, pci: None })? as u8;
		}
		let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as u64;
		if length < 36 || length > region.length || length as usize > self.aml.limits.memory {
			return Err(Error::BadTable("the table in the region is not the length it says"));
		}
		self.allocate(length as usize)?;
		let mut bytes = Vec::with_capacity(length as usize);
		for at in 0..length {
			bytes.push(self.host.read(crate::host::Access { space: Space::SystemMemory, address: region.offset + at, width: 8, pci: None })? as u8);
		}
		Ok(bytes)
	}
}
