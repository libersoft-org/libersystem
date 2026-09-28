//! THE INTERPRETER'S TEST TOOLING, for this crate's suites and for the crates that decide on what it evaluates (the
//! ACPI service's model): an AML encoder (`build`) and a host made of maps (`Model`). Compiled for tests and behind
//! the `testing` feature, never into a service.

pub mod build;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Limits;
use crate::host::{Access, Host, HostError, PciAddress, TableBytes};
use crate::interp::Aml;
use crate::name::Path;
use crate::object::{Object, Space};

/// A MACHINE MADE OF MAPS: memory, ports, PCI configuration, CMOS and the embedded controller as bytes; GPIO lines;
/// an I2C device as registers per address; a clock that `Sleep` advances; and every notify and debug line kept.
#[derive(Default)]
pub struct Model {
	pub memory: BTreeMap<u64, u8>,
	pub io: BTreeMap<u64, u8>,
	pub pci: BTreeMap<(u16, u8, u8, u8, u64), u8>,
	pub cmos: BTreeMap<u64, u8>,
	pub ec: BTreeMap<u64, u8>,
	pub gpio: BTreeMap<u16, bool>,
	pub i2c: BTreeMap<(u16, u64), u8>,
	/// Ranges the policy refuses, by space: an access inside one is refused.
	pub refused: Vec<(Space, u64, u64)>,
	pub accesses: Vec<(Space, u64, u8, bool)>,
	pub notifies: Vec<(String, u64)>,
	pub debug: Vec<String>,
	pub tables: Vec<Vec<u8>>,
	pub clock_ms: u64,
	pub global_lock_held: bool,
	pub global_lock_busy: bool,
	pub fatal: Option<(u8, u32, u64)>,
	/// Every region announced before an access: (the declaring node, space, base, length).
	pub regions: Vec<(String, Space, u64, u64)>,
}

fn bytes_of(map: &BTreeMap<u64, u8>, address: u64, width: u8) -> u64 {
	(0..width as u64 / 8).fold(0u64, |value, at| value | (*map.get(&(address + at)).unwrap_or(&0) as u64) << (8 * at))
}

fn put(map: &mut BTreeMap<u64, u8>, address: u64, width: u8, value: u64) {
	for at in 0..width as u64 / 8 {
		map.insert(address + at, (value >> (8 * at)) as u8);
	}
}

fn i2c_address(connection: &[u8]) -> Option<u16> {
	match crate::resource::decode_one(connection)? {
		crate::resource::Resource::I2c { address, .. } => Some(address),
		_ => None,
	}
}

impl Model {
	fn check(&self, access: &Access) -> Result<(), HostError> {
		for (space, start, end) in &self.refused {
			if *space == access.space && access.address >= *start && access.address < *end {
				return Err(HostError::Refused(format!("{:#x} is outside the policy", access.address)));
			}
		}
		Ok(())
	}

	fn map(&mut self, space: Space) -> Option<&mut BTreeMap<u64, u8>> {
		match space {
			Space::SystemMemory => Some(&mut self.memory),
			Space::SystemIo => Some(&mut self.io),
			Space::SystemCmos => Some(&mut self.cmos),
			Space::EmbeddedControl => Some(&mut self.ec),
			_ => None,
		}
	}
}

impl Host for Model {
	fn region(&mut self, node: &Path, space: Space, base: u64, length: u64) {
		self.regions.push((node.text(), space, base, length));
	}

	fn read(&mut self, access: Access) -> Result<u64, HostError> {
		self.check(&access)?;
		self.accesses.push((access.space, access.address, access.width, false));
		if access.space == Space::PciConfig {
			let PciAddress { segment, bus, device, function } = access.pci.expect("a PCI access names its function");
			return Ok((0..access.width as u64 / 8).fold(0u64, |value, at| value | (*self.pci.get(&(segment, bus, device, function, access.address + at)).unwrap_or(&0) as u64) << (8 * at)));
		}
		let width = access.width;
		let map = self.map(access.space).ok_or_else(|| HostError::Unavailable(String::from("no such space in the model")))?;
		Ok(bytes_of(map, access.address, width))
	}

	fn write(&mut self, access: Access, value: u64) -> Result<(), HostError> {
		self.check(&access)?;
		self.accesses.push((access.space, access.address, access.width, true));
		if access.space == Space::PciConfig {
			let PciAddress { segment, bus, device, function } = access.pci.expect("a PCI access names its function");
			for at in 0..access.width as u64 / 8 {
				self.pci.insert((segment, bus, device, function, access.address + at), (value >> (8 * at)) as u8);
			}
			return Ok(());
		}
		let width = access.width;
		let map = self.map(access.space).ok_or_else(|| HostError::Unavailable(String::from("no such space in the model")))?;
		put(map, access.address, width, value);
		Ok(())
	}

	fn serial_bus(&mut self, connection: &[u8], protocol: u8, length: u8, command: u64, write: Option<&[u8]>) -> Result<Vec<u8>, HostError> {
		let address = i2c_address(connection).ok_or_else(|| HostError::Refused(String::from("not an I2C connection")))?;
		let count = match protocol {
			crate::host::protocol::BYTE => 1,
			crate::host::protocol::WORD => 2,
			crate::host::protocol::BYTES => length as usize,
			_ => return Err(HostError::Refused(format!("protocol {protocol:#x}"))),
		};
		match write {
			Some(bytes) => {
				for (at, byte) in bytes.iter().enumerate().take(count) {
					self.i2c.insert((address, command + at as u64), *byte);
				}
				Ok(Vec::new())
			}
			None => Ok((0..count as u64).map(|at| *self.i2c.get(&(address, command + at)).unwrap_or(&0)).collect()),
		}
	}

	fn gpio_read(&mut self, _connection: &[u8], pin: u16) -> Result<bool, HostError> {
		Ok(*self.gpio.get(&pin).unwrap_or(&false))
	}

	fn table(&mut self, signature: [u8; 4], oem_id: &[u8], oem_table_id: &[u8]) -> Option<TableBytes> {
		self.tables.iter().find(|table| table[0..4] == signature && table[10..16].starts_with(oem_id.trim_ascii_end()) && table[16..24].starts_with(oem_table_id.trim_ascii_end())).map(|table| TableBytes { bytes: table.clone() })
	}

	fn table_address(&mut self, _signature: [u8; 4], _oem_id: &[u8], _oem_table_id: &[u8]) -> Option<(u64, u64)> {
		None
	}

	fn sleep(&mut self, ms: u64) {
		self.clock_ms += ms;
	}

	fn stall(&mut self, _us: u64) {}

	fn timer(&mut self) -> u64 {
		self.clock_ms * 10_000
	}

	fn now_ms(&mut self) -> u64 {
		self.clock_ms
	}

	fn notify(&mut self, node: &Path, value: u64) {
		self.notifies.push((node.text(), value));
	}

	fn debug(&mut self, text: &str) {
		self.debug.push(String::from(text));
	}

	fn fatal(&mut self, kind: u8, code: u32, argument: u64) {
		self.fatal = Some((kind, code, argument));
	}

	fn global_lock(&mut self, acquire: bool) -> bool {
		if acquire {
			if self.global_lock_busy {
				return false;
			}
			self.global_lock_held = true;
			true
		} else {
			self.global_lock_held = false;
			true
		}
	}
}

/// A namespace with one DSDT of `body` loaded.
pub fn load(body: &[u8]) -> (Aml, Model) {
	load_revision(body, 2)
}

pub fn load_revision(body: &[u8], revision: u8) -> (Aml, Model) {
	let mut aml = Aml::new(Limits::default());
	let mut model = Model::default();
	let table = build::table("DSDT", revision, body);
	aml.load(&table, &mut model).expect("the DSDT loads");
	(aml, model)
}

/// Evaluate `path` with integer arguments.
pub fn eval(aml: &mut Aml, model: &mut Model, path: &str, args: &[u64]) -> Result<Option<Object>, crate::error::Error> {
	let args: Vec<Object> = args.iter().map(|value| Object::Integer(*value)).collect();
	let node = aml.lookup(path).unwrap_or_else(|| panic!("{path} is in the namespace"));
	aml.evaluate(node, &args, model)
}
