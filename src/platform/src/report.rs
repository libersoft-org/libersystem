// WHAT THE ACPI SERVICE REPORTS TO THE KERNEL, encoded: a namespace device or reservation with its `_CRS`
// resources, a withdrawal, a companion join, a host bridge's `_OSC` answer, and "namespace loaded". One encoder
// the service writes with and one decoder the kernel reads with - no allocation on either side, every count
// bounded by the row's own bounds, so a report past them is refused whole.
//
// The service DESCRIBES; the kernel decides what is minted.

use abi::{Connection, WiredLine};

use crate::Description;

pub const DEVICE: u8 = 1;
pub const WITHDRAW: u8 = 2;
pub const COMPANION: u8 = 3;
pub const OSC: u8 = 4;
pub const LOADED: u8 = 5;
pub const LISTS: u8 = 6;

/// The most lines or addresses a companion report carries in each list.
pub const MAX_LISTED: usize = 32;
/// The most bytes one report may be.
pub const MAX_REPORT: usize = 8192;

/// A PCI function: segment, bus, device, function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Function {
	pub segment: u16,
	pub bus: u8,
	pub device: u8,
	pub function: u8,
}

/// A namespace device or reservation.
#[derive(Clone, Copy, Debug)]
pub struct DeviceReport<'a> {
	pub description: Description,
	/// Each connection's controller, by the controller's identity - a namespace path or a companion's function
	/// identity - joined to a row by the kernel.
	pub targets: [(u8, &'a [u8]); abi::MAX_PLATFORM_CONNECTIONS],
	pub target_count: usize,
	/// A node below a companion: the parent companion's function, carried in its identity's row.
	pub parent: Option<Function>,
	/// The row's property block (records in `abi::DEVICE_PROPERTY_*` form) - `_DSD` decoded.
	pub properties: &'a [u8],
}

/// A bounded list of numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct List {
	pub values: [u32; MAX_LISTED],
	pub count: usize,
}

impl Default for List {
	fn default() -> Self {
		List { values: [0; MAX_LISTED], count: 0 }
	}
}

impl List {
	pub fn as_slice(&self) -> &[u32] {
		&self.values[..self.count.min(MAX_LISTED)]
	}

	pub fn push(&mut self, value: u32) -> bool {
		if self.count >= MAX_LISTED {
			return false;
		}
		self.values[self.count] = value;
		self.count += 1;
		true
	}
}

/// A companion join: the node, the function it describes, and - for a GPIO or serial-bus controller - the
/// `_AEI` lines and the lines and addresses the service's own fields name.
#[derive(Clone, Copy, Debug)]
pub struct CompanionReport<'a> {
	pub path: &'a [u8],
	pub function: Function,
	pub aei_lines: List,
	pub field_lines: List,
	pub field_addresses: List,
}

/// A namespace row that is itself a GPIO or serial-bus controller: its identity, and the `_AEI` lines and the lines and
/// addresses the service's own fields name through it.
#[derive(Clone, Copy, Debug)]
pub struct ListsReport<'a> {
	pub identity: &'a [u8],
	pub aei_lines: List,
	pub field_lines: List,
	pub field_addresses: List,
}

#[derive(Clone, Copy, Debug)]
pub enum Report<'a> {
	Device(DeviceReport<'a>),
	Withdraw(&'a [u8]),
	Companion(CompanionReport<'a>),
	/// A host bridge's `_OSC` answer: the segment and bus range it roots, and the control it granted.
	Osc {
		segment: u16,
		bus_start: u8,
		bus_end: u8,
		granted: u32,
	},
	/// The walk of instance `instance` is published.
	Loaded {
		instance: u64,
	},
	Lists(ListsReport<'a>),
}

/// `_OSC` control bits for a PCI Express host bridge, as the dword the service passed and firmware answered.
pub mod osc {
	pub const HOT_PLUG: u32 = 1 << 0;
	pub const PME: u32 = 1 << 2;
	pub const AER: u32 = 1 << 3;
	pub const CAPABILITY: u32 = 1 << 4;
	pub const LTR: u32 = 1 << 5;
}

struct Writer<'a> {
	out: &'a mut [u8],
	at: usize,
}

impl Writer<'_> {
	fn put(&mut self, bytes: &[u8]) -> Option<()> {
		let end = self.at.checked_add(bytes.len())?;
		self.out.get_mut(self.at..end)?.copy_from_slice(bytes);
		self.at = end;
		Some(())
	}

	fn u8(&mut self, value: u8) -> Option<()> {
		self.put(&[value])
	}

	fn short(&mut self, bytes: &[u8]) -> Option<()> {
		self.u8(u8::try_from(bytes.len()).ok()?)?;
		self.put(bytes)
	}
}

struct Reader<'a> {
	bytes: &'a [u8],
	at: usize,
}

impl<'a> Reader<'a> {
	fn take(&mut self, count: usize) -> Option<&'a [u8]> {
		let end = self.at.checked_add(count)?;
		let slice = self.bytes.get(self.at..end)?;
		self.at = end;
		Some(slice)
	}

	fn u8(&mut self) -> Option<u8> {
		Some(self.take(1)?[0])
	}

	fn u16(&mut self) -> Option<u16> {
		Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
	}

	fn u32(&mut self) -> Option<u32> {
		Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
	}

	fn u64(&mut self) -> Option<u64> {
		Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
	}

	fn short(&mut self) -> Option<&'a [u8]> {
		let length = self.u8()? as usize;
		self.take(length)
	}

	fn list(&mut self) -> Option<List> {
		let count = self.u8()? as usize;
		if count > MAX_LISTED {
			return None;
		}
		let mut list = List::default();
		for _ in 0..count {
			list.push(self.u32()?);
		}
		Some(list)
	}
}

fn function_bytes(function: &Function) -> [u8; 5] {
	let segment = function.segment.to_le_bytes();
	[segment[0], segment[1], function.bus, function.device, function.function]
}

/// Encode a device report into `out`, answering its length.
pub fn encode_device(report: &DeviceReport<'_>, out: &mut [u8]) -> Option<usize> {
	let mut w = Writer { out, at: 0 };
	let part = &report.description.part;
	w.u8(DEVICE)?;
	w.u8(part.state)?;
	w.u8(part.flags)?;
	w.short(part.identity())?;
	w.u8(part.match_ids().len() as u8)?;
	for id in part.match_ids() {
		w.u8(id.kind)?;
		w.short(id.text())?;
	}
	w.u8(part.mmio().len() as u8)?;
	for range in part.mmio() {
		w.put(&range.base.to_le_bytes())?;
		w.put(&range.len.to_le_bytes())?;
	}
	w.u8(report.description.ports().len() as u8)?;
	for port in report.description.ports() {
		w.put(&port.base.to_le_bytes())?;
		w.put(&port.len.to_le_bytes())?;
	}
	w.u8(part.lines().len() as u8)?;
	for line in part.lines() {
		w.put(&line.number.to_le_bytes())?;
		w.u8(line.trigger)?;
		w.u8(line.polarity)?;
		w.u8(line.controller)?;
	}
	w.u8(part.connections().len() as u8)?;
	for (index, connection) in part.connections().iter().enumerate() {
		w.u8(connection.kind)?;
		w.u8(connection.trigger)?;
		w.u8(connection.polarity)?;
		w.put(&connection.value.to_le_bytes())?;
		w.put(&connection.extra.to_le_bytes())?;
		let target = report.targets[..report.target_count].iter().find(|(at, _)| *at as usize == index).map(|(_, identity)| *identity).unwrap_or(&[]);
		w.short(target)?;
	}
	match &report.parent {
		Some(function) => {
			w.u8(1)?;
			w.put(&function_bytes(function))?;
		}
		None => w.u8(0)?,
	}
	w.put(&u16::try_from(report.properties.len()).ok()?.to_le_bytes())?;
	w.put(report.properties)?;
	Some(w.at)
}

/// Encode a withdrawal of the row `path` names.
pub fn encode_withdraw(path: &[u8], out: &mut [u8]) -> Option<usize> {
	let mut w = Writer { out, at: 0 };
	w.u8(WITHDRAW)?;
	w.short(path)?;
	Some(w.at)
}

pub fn encode_companion(report: &CompanionReport<'_>, out: &mut [u8]) -> Option<usize> {
	let mut w = Writer { out, at: 0 };
	w.u8(COMPANION)?;
	w.short(report.path)?;
	w.put(&function_bytes(&report.function))?;
	for list in [&report.aei_lines, &report.field_lines, &report.field_addresses] {
		w.u8(list.count as u8)?;
		for value in list.as_slice() {
			w.put(&value.to_le_bytes())?;
		}
	}
	Some(w.at)
}

pub fn encode_lists(report: &ListsReport<'_>, out: &mut [u8]) -> Option<usize> {
	let mut w = Writer { out, at: 0 };
	w.u8(LISTS)?;
	w.short(report.identity)?;
	for list in [&report.aei_lines, &report.field_lines, &report.field_addresses] {
		w.u8(list.count as u8)?;
		for value in list.as_slice() {
			w.put(&value.to_le_bytes())?;
		}
	}
	Some(w.at)
}

pub fn encode_osc(segment: u16, bus_start: u8, bus_end: u8, granted: u32, out: &mut [u8]) -> Option<usize> {
	let mut w = Writer { out, at: 0 };
	w.u8(OSC)?;
	w.put(&segment.to_le_bytes())?;
	w.u8(bus_start)?;
	w.u8(bus_end)?;
	w.put(&granted.to_le_bytes())?;
	Some(w.at)
}

pub fn encode_loaded(instance: u64, out: &mut [u8]) -> Option<usize> {
	let mut w = Writer { out, at: 0 };
	w.u8(LOADED)?;
	w.put(&instance.to_le_bytes())?;
	Some(w.at)
}

/// DECODE a report; `None` for anything malformed or past a bound, and for trailing bytes.
pub fn decode(bytes: &[u8]) -> Option<Report<'_>> {
	if bytes.len() > MAX_REPORT {
		return None;
	}
	let mut r = Reader { bytes, at: 0 };
	let report = match r.u8()? {
		DEVICE => {
			let state = r.u8()?;
			let flags = r.u8()?;
			let identity = r.short()?;
			let mut description = Description::new(abi::PLATFORM_SOURCE_ACPI, state, identity)?;
			description.part.flags = flags;
			for _ in 0..r.u8()? {
				let kind = r.u8()?;
				let text = r.short()?;
				if !description.add_match(kind, text) {
					return None;
				}
			}
			for _ in 0..r.u8()? {
				let base = r.u64()?;
				let len = r.u64()?;
				if !description.add_mmio(base, len) {
					return None;
				}
			}
			for _ in 0..r.u8()? {
				let base = r.u16()?;
				let len = r.u16()?;
				if !description.add_port(base, len) {
					return None;
				}
			}
			for _ in 0..r.u8()? {
				let number = r.u32()?;
				let trigger = r.u8()?;
				let polarity = r.u8()?;
				let controller = r.u8()?;
				if !description.add_line(WiredLine { number, trigger, polarity, controller, _pad: 0 }) {
					return None;
				}
			}
			let mut targets: [(u8, &[u8]); abi::MAX_PLATFORM_CONNECTIONS] = [(0, &[]); abi::MAX_PLATFORM_CONNECTIONS];
			let mut target_count = 0usize;
			for index in 0..r.u8()? {
				let kind = r.u8()?;
				let trigger = r.u8()?;
				let polarity = r.u8()?;
				let value = r.u32()?;
				let extra = r.u32()?;
				let target = r.short()?;
				if !description.add_connection(Connection { kind, trigger, polarity, _pad: 0, controller: u32::MAX, value, extra }) {
					return None;
				}
				if !target.is_empty() {
					targets[target_count] = (index, target);
					target_count += 1;
				}
			}
			let parent = match r.u8()? {
				0 => None,
				1 => {
					let segment = r.u16()?;
					Some(Function { segment, bus: r.u8()?, device: r.u8()?, function: r.u8()? })
				}
				_ => return None,
			};
			let length = r.u16()? as usize;
			let properties = r.take(length)?;
			Report::Device(DeviceReport { description, targets, target_count, parent, properties })
		}
		WITHDRAW => Report::Withdraw(r.short()?),
		COMPANION => {
			let path = r.short()?;
			let segment = r.u16()?;
			let function = Function { segment, bus: r.u8()?, device: r.u8()?, function: r.u8()? };
			let aei_lines = r.list()?;
			let field_lines = r.list()?;
			let field_addresses = r.list()?;
			Report::Companion(CompanionReport { path, function, aei_lines, field_lines, field_addresses })
		}
		OSC => {
			let segment = r.u16()?;
			let bus_start = r.u8()?;
			let bus_end = r.u8()?;
			let granted = r.u32()?;
			Report::Osc { segment, bus_start, bus_end, granted }
		}
		LOADED => Report::Loaded { instance: r.u64()? },
		LISTS => {
			let identity = r.short()?;
			let aei_lines = r.list()?;
			let field_lines = r.list()?;
			let field_addresses = r.list()?;
			Report::Lists(ListsReport { identity, aei_lines, field_lines, field_addresses })
		}
		_ => return None,
	};
	(r.at == bytes.len()).then_some(report)
}
