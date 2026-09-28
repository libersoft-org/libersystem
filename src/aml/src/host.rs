//! THE HOST: everything the interpreter cannot do by itself - reach a register, wait, tell anybody anything. The
//! ACPI service implements it over the kernel's firmware calls, under the kernel's policy; the tests implement it
//! over a register model. The interpreter decides nothing about what a region may reach - the host answers
//! `Refused` and the interpreter reports it.

use alloc::string::String;
use alloc::vec::Vec;

use crate::name::Path;
use crate::object::Space;

/// Serial-bus protocols, as `AccessAs` names them.
pub mod protocol {
	pub const QUICK: u8 = 0x02;
	pub const SEND_RECEIVE: u8 = 0x04;
	pub const BYTE: u8 = 0x06;
	pub const WORD: u8 = 0x08;
	pub const BLOCK: u8 = 0x0A;
	pub const BYTES: u8 = 0x0B;
	pub const PROCESS_CALL: u8 = 0x0C;
	pub const BLOCK_PROCESS_CALL: u8 = 0x0D;
	pub const RAW_BYTES: u8 = 0x0E;
	pub const RAW_PROCESS_BYTES: u8 = 0x0F;
}

/// A PCI function, as a `PCI_Config` region names it through its device's `_SEG`, `_BBN` and `_ADR`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PciAddress {
	pub segment: u16,
	pub bus: u8,
	pub device: u8,
	pub function: u8,
}

/// One register access: the space, the address within it (for `PCI_Config`, the configuration offset), and the
/// width in bits - 8, 16, 32 or 64.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Access {
	pub space: Space,
	pub address: u64,
	pub width: u8,
	pub pci: Option<PciAddress>,
}

/// Why a host did not perform an access.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HostError {
	/// The policy refused it: a range it may not map, a register it may not write.
	Refused(String),
	/// Nothing answers there, or the space has no handler yet.
	Unavailable(String),
	/// The access was made and failed.
	Failed(String),
}

/// A table `Load` or `LoadTable` asks for, as the host found it.
pub struct TableBytes {
	pub bytes: Vec<u8>,
}

pub trait Host {
	/// THE REGION THE NEXT ACCESS IS IN: the node that declared it, its space, base and length - so a host that maps a
	/// region whole, and answers to a policy that asks whose region it is, knows before the access. Said before
	/// every access to a SystemMemory or SystemIO region; a host that needs none of it ignores it.
	fn region(&mut self, _node: &Path, _space: Space, _base: u64, _length: u64) {}
	/// Read one register.
	fn read(&mut self, access: Access) -> Result<u64, HostError>;
	/// Write one register.
	fn write(&mut self, access: Access, value: u64) -> Result<(), HostError>;
	/// One serial-bus transaction on the connection a field names (the raw `I2cSerialBusV2` descriptor): the
	/// protocol (`AccessAs`' attribute), its length where it has one, the command (the field's offset in bytes),
	/// and the bytes to write for a write. Answers the bytes read (empty for a write that returns nothing).
	fn serial_bus(&mut self, connection: &[u8], protocol: u8, length: u8, command: u64, write: Option<&[u8]>) -> Result<Vec<u8>, HostError>;
	/// Read one input line of the GPIO connection a field names (the raw `GpioIo` or `GpioInt` descriptor).
	fn gpio_read(&mut self, connection: &[u8], pin: u16) -> Result<bool, HostError>;
	/// A table `LoadTable` names, by signature and OEM ids (the ids blank-padded, compared as the firmware stored
	/// them), or `None`.
	fn table(&mut self, signature: [u8; 4], oem_id: &[u8], oem_table_id: &[u8]) -> Option<TableBytes>;
	/// Where a table sits in memory, for `DataTableRegion`: its physical address and length.
	fn table_address(&mut self, signature: [u8; 4], oem_id: &[u8], oem_table_id: &[u8]) -> Option<(u64, u64)>;
	/// `Sleep`: give up the processor for at least `ms` milliseconds.
	fn sleep(&mut self, ms: u64);
	/// `Stall`: busy-wait `us` microseconds.
	fn stall(&mut self, us: u64);
	/// `Timer`: a monotonic count of 100-nanosecond units.
	fn timer(&mut self) -> u64;
	/// A monotonic millisecond clock, for the evaluation's time bound.
	fn now_ms(&mut self) -> u64;
	/// `Notify(node, value)`.
	fn notify(&mut self, node: &Path, value: u64);
	/// A line for `Store(..., Debug)` and for what the interpreter reports.
	fn debug(&mut self, text: &str);
	/// `Fatal`: the firmware declares an unrecoverable error.
	fn fatal(&mut self, kind: u8, code: u32, argument: u64);
	/// Take (`true`) or give up (`false`) the firmware global lock; answers whether it was taken.
	fn global_lock(&mut self, acquire: bool) -> bool;
}
