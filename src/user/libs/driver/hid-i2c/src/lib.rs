//! HID over I2C: the descriptor a device publishes, the register protocol that reaches its reports,
//! and the bounded bus contract both are spoken over.
//!
//! WHAT THIS IS AND IS NOT. It is a PARSER AND A PROTOCOL and it binds nothing. There is no I2C
//! controller driver in this tree and no provider kind for one, so there is no bus to bind to and no
//! device model to bind against - which is exactly what makes this half a shared library rather than
//! a driver, and what lets it close on host tests over a fake bus while the hardware half waits for a
//! controller item with a real fixture.
//!
//! THE BUS CONTRACT IS OWNED HERE BECAUSE THIS IS THE FIRST IMPLEMENTED CONSUMER, which is the rule
//! the milestone states rather than a claim on the abstraction: an IPMI SSIF transport consumes the
//! same bus class and is independently ordered, and whichever of the two is written first states the
//! contract. It is deliberately the smallest thing that serves both - an address, a write, a read,
//! and the write-then-read that every register access is - with no controller vocabulary in it at
//! all: no clock rate, no bus number, no pin, no arbitration. A controller item will implement this;
//! it will not have to be described by it.
//!
//! WHY A DEVICE ON A SLOW SERIAL BUS NEEDS A PARSER THIS DEFENSIVE. Every field below arrives from a
//! peripheral over two wires: the length of a report, the register a report descriptor lives at, the
//! size of the descriptor itself. A touchpad that answers a register read with stale bytes, a device
//! held in reset, and a firmware that reports a maximum input length of zero are all ordinary
//! hardware behaviour rather than an attack - and each of them is a buffer length somebody would
//! otherwise have trusted.
//!
//! WHAT IT DOES NOT DO. It does not parse a report DESCRIPTOR: that is the generic HID parser the USB
//! stack already has, and the descriptor bytes this module fetches are handed to it unchanged. It
//! does not know what a touchpad is, and it never decides when to read - the caller owns the
//! interrupt and the schedule.

#![cfg_attr(not(test), no_std)]

/// The largest single transfer this protocol will ask a bus for.
///
/// A BOUND THE BUS DOES NOT HAVE TO STATE. An I2C transaction has no length field and no intrinsic
/// limit; the limit is whatever the controller's buffer is, and a device that says its input reports
/// are sixty thousand bytes long has said something a driver must refuse rather than allocate for.
/// Two kilobytes is larger than any HID-over-I2C report any real device sends.
pub const MAX_TRANSFER: usize = 2048;

/// The HID descriptor is thirty bytes, and the specification says so as a constant rather than as a
/// range: a device reporting any other length is not speaking this protocol.
pub const HID_DESCRIPTOR_LEN: usize = 30;

/// The only version of this protocol that exists.
pub const HID_VERSION: u16 = 0x0100;

/// The largest report descriptor this module will fetch.
///
/// The specification puts no ceiling on it. A digitiser's descriptor is a few hundred bytes and the
/// largest in the wild are a couple of thousand; a device answering with sixty-five thousand is a
/// device whose register read returned noise, and the difference matters because the caller allocates
/// against this number.
pub const MAX_REPORT_DESCRIPTOR: usize = 4096;

/// Every input and output report carries a two-byte length prefix that INCLUDES ITSELF.
pub const LENGTH_PREFIX: usize = 2;

/// Why a bus transaction did not happen. The bus's own vocabulary, kept to what a caller can act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BusError {
	/// Nothing acknowledged the address.
	NoDevice,
	/// The device acknowledged and then stopped, or the controller lost the bus.
	Interrupted,
	/// The transfer was longer than the controller or this protocol will carry.
	TooLong,
	/// The controller itself failed.
	Controller,
}

/// Why a HID-over-I2C exchange was refused. Separate from `BusError` because these are things the
/// DEVICE said that cannot be true, and a caller retries a bus error and refuses one of these.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	Bus(BusError),
	/// The descriptor was shorter than the thirty bytes it is defined to be.
	DescriptorTruncated,
	/// The descriptor's own length field is not thirty.
	DescriptorLength,
	/// A version this protocol does not define.
	DescriptorVersion,
	/// The descriptor's reserved bytes are not zero, which means the bytes are not a descriptor.
	DescriptorReserved,
	/// A report descriptor of zero length, or longer than this module will fetch.
	ReportDescriptorLength,
	/// A maximum input length that cannot hold its own length prefix.
	InputLengthTooSmall,
	/// A length larger than the bus or this protocol will carry.
	TooLong,
	/// The caller's buffer is smaller than the exchange needs.
	BufferTooSmall,
	/// A report whose declared length disagrees with what arrived, in either direction.
	ReportLength,
}

impl From<BusError> for Error {
	fn from(error: BusError) -> Self {
		Error::Bus(error)
	}
}

/// A seven-bit I2C address, checked once.
///
/// THE RESERVED ADDRESSES ARE NOT ADDRESSES. `0x00` through `0x07` and `0x78` through `0x7f` are
/// reserved by the bus standard for general call, ten-bit addressing and the rest; a driver that
/// addresses one is not talking to a device, it is broadcasting or worse. A newtype is what makes
/// that checked at the one place an address enters rather than at each transaction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SlaveAddress(u8);

impl SlaveAddress {
	pub const fn new(address: u8) -> Option<Self> {
		if !matches!(address, 0x08..=0x77) {
			return None;
		}
		Some(Self(address))
	}

	pub const fn get(&self) -> u8 {
		self.0
	}
}

/// THE BOUNDED I2C BUS CONTRACT.
///
/// Three operations, because a register protocol needs exactly three: write a register and its value,
/// read bytes the device is offering, and the WRITE-THEN-READ that every register read is - a write
/// of the register address followed by a repeated start and a read, with no stop in between. Splitting
/// that last one into a write and a read is not the same transaction: another master, or a device
/// with an internal pointer, can act in the gap.
///
/// EVERY IMPLEMENTATION IS BOUNDED BY `MAX_TRANSFER` AND SAYS SO BY REFUSING. A controller with a
/// smaller buffer refuses earlier; none is required to be able to carry more.
pub trait I2cBus {
	fn write(&mut self, address: SlaveAddress, bytes: &[u8]) -> Result<(), BusError>;

	fn read(&mut self, address: SlaveAddress, into: &mut [u8]) -> Result<usize, BusError>;

	fn write_read(&mut self, address: SlaveAddress, write: &[u8], into: &mut [u8]) -> Result<usize, BusError>;
}

/// What a HID-over-I2C device publishes about itself, at the register ACPI or the device tree named.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Descriptor {
	pub version: u16,
	pub report_descriptor_len: u16,
	pub report_descriptor_register: u16,
	pub input_register: u16,
	pub max_input_len: u16,
	pub output_register: u16,
	pub max_output_len: u16,
	pub command_register: u16,
	pub data_register: u16,
	pub vendor: u16,
	pub product: u16,
	pub version_id: u16,
}

impl Descriptor {
	/// Decode and CHECK the thirty bytes at the HID descriptor register.
	///
	/// FIVE REFUSALS, AND EACH IS A NUMBER SOMEBODY WOULD HAVE ALLOCATED AGAINST. A length that is not
	/// thirty and a version that is not `0x0100` mean the read did not land on a descriptor at all -
	/// which is what a device still in reset answers with. Reserved bytes that are not zero mean the
	/// same. A report-descriptor length of zero or beyond this module's ceiling, and a maximum input
	/// length that cannot hold its own two-byte prefix, are the two that would otherwise become a
	/// zero-sized or a wild buffer.
	pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
		if bytes.len() < HID_DESCRIPTOR_LEN {
			return Err(Error::DescriptorTruncated);
		}
		let at = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
		if at(0) as usize != HID_DESCRIPTOR_LEN {
			return Err(Error::DescriptorLength);
		}
		if at(2) != HID_VERSION {
			return Err(Error::DescriptorVersion);
		}
		// THE RESERVED FIELD IS FOUR ZERO BYTES AND IS CHECKED. It is the cheapest test that the
		// bytes are a descriptor rather than the tail of somebody else's register window, and a
		// device that sets them is speaking a protocol this one does not know.
		if bytes[26..30].iter().any(|byte| *byte != 0) {
			return Err(Error::DescriptorReserved);
		}
		let report_descriptor_len = at(4);
		if report_descriptor_len == 0 || report_descriptor_len as usize > MAX_REPORT_DESCRIPTOR {
			return Err(Error::ReportDescriptorLength);
		}
		let max_input_len = at(10);
		// A REPORT THAT CANNOT HOLD ITS OWN LENGTH PREFIX IS NOT A REPORT. Two is the smallest legal
		// value and it means "this device sends only the reset indication".
		if (max_input_len as usize) < LENGTH_PREFIX {
			return Err(Error::InputLengthTooSmall);
		}
		if max_input_len as usize > MAX_TRANSFER {
			return Err(Error::TooLong);
		}
		Ok(Self { version: at(2), report_descriptor_len, report_descriptor_register: at(6), input_register: at(8), max_input_len, output_register: at(12), max_output_len: at(14), command_register: at(16), data_register: at(18), vendor: at(20), product: at(22), version_id: at(24) })
	}

	/// The largest report BODY this device sends: its maximum input length less the prefix that is
	/// counted inside it.
	///
	/// STATED ONCE because getting it wrong is a two-byte overrun in one direction and a truncated
	/// report in the other, and both look like a device bug rather than a driver bug.
	pub const fn max_input_body(&self) -> usize {
		self.max_input_len as usize - LENGTH_PREFIX
	}
}

/// Which of the three report kinds a command is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReportType {
	Input,
	Output,
	Feature,
}

impl ReportType {
	const fn code(self) -> u8 {
		match self {
			ReportType::Input => 1,
			ReportType::Output => 2,
			ReportType::Feature => 3,
		}
	}
}

/// The power states a device may be put into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PowerState {
	On,
	Sleep,
}

impl PowerState {
	const fn code(self) -> u8 {
		match self {
			PowerState::On => 0,
			PowerState::Sleep => 1,
		}
	}
}

/// The command opcodes this module speaks. The reserved and vendor opcodes are deliberately absent:
/// an enumeration that can express a reserved opcode is one a caller can send.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Opcode {
	Reset,
	GetReport,
	SetReport,
	SetPower,
}

impl Opcode {
	const fn code(self) -> u8 {
		match self {
			Opcode::Reset => 0x01,
			Opcode::GetReport => 0x02,
			Opcode::SetReport => 0x03,
			Opcode::SetPower => 0x08,
		}
	}
}

/// FIFTEEN IS AN ESCAPE AND NOT AN ID. The command's first byte carries the report id in four bits,
/// and the value fifteen means "the id is in the byte after the opcode" - so an id of fifteen or more
/// takes the longer form. A driver that wrote `id & 0x0f` and stopped there would address report 15
/// as report 15 and report 31 as report 15 as well.
const REPORT_ID_ESCAPE: u8 = 0x0f;

/// A command, encoded into a caller's buffer.
///
/// NOTHING HERE ALLOCATES: the caller owns the buffer, which is what lets a driver with no heap and a
/// test with a fixed array use the same code. Each function answers how many bytes it wrote.
pub struct Command;

impl Command {
	/// The RESET command, which is the first thing said to a device and the only one that needs no
	/// data register.
	pub fn reset(descriptor: &Descriptor, into: &mut [u8]) -> Result<usize, Error> {
		Self::encode(descriptor, Opcode::Reset, ReportType::Input, 0, into, false)
	}

	/// SET_POWER, whose power state travels in the field a report id would occupy.
	pub fn set_power(descriptor: &Descriptor, state: PowerState, into: &mut [u8]) -> Result<usize, Error> {
		// THE POWER STATE IS NOT A REPORT ID and it is never escaped: zero and one are the only two
		// values, and they go in the low nibble as themselves.
		Self::encode(descriptor, Opcode::SetPower, ReportType::Input, state.code(), into, false)
	}

	/// GET_REPORT: the command, then the data register the answer will be read from.
	pub fn get_report(descriptor: &Descriptor, kind: ReportType, report_id: u8, into: &mut [u8]) -> Result<usize, Error> {
		Self::encode(descriptor, Opcode::GetReport, kind, report_id, into, true)
	}

	/// SET_REPORT: the command, the data register, and then the report itself with its own length
	/// prefix - written into one buffer because it is one bus transaction.
	pub fn set_report(descriptor: &Descriptor, kind: ReportType, report_id: u8, body: &[u8], into: &mut [u8]) -> Result<usize, Error> {
		let mut written = Self::encode(descriptor, Opcode::SetReport, kind, report_id, into, true)?;
		// THE LENGTH INCLUDES ITSELF AND THE REPORT ID BYTE WHEN THERE IS ONE, which is the rule that
		// is stated once here and would otherwise be restated by every caller that sends a report.
		let id_bytes = usize::from(report_id != 0);
		let total = LENGTH_PREFIX + id_bytes + body.len();
		if total > u16::MAX as usize || total > MAX_TRANSFER {
			return Err(Error::TooLong);
		}
		let end = written + total;
		if end > into.len() || end > MAX_TRANSFER {
			return Err(Error::BufferTooSmall);
		}
		into[written..written + 2].copy_from_slice(&(total as u16).to_le_bytes());
		written += 2;
		if id_bytes == 1 {
			into[written] = report_id;
			written += 1;
		}
		into[written..end].copy_from_slice(body);
		Ok(end)
	}

	// The command register, the two command bytes, the escaped report id when there is one, and the
	// data register when the opcode uses one.
	fn encode(descriptor: &Descriptor, opcode: Opcode, kind: ReportType, report_id: u8, into: &mut [u8], with_data_register: bool) -> Result<usize, Error> {
		let escaped = report_id >= REPORT_ID_ESCAPE;
		let length = 4 + usize::from(escaped) + if with_data_register { 2 } else { 0 };
		if into.len() < length {
			return Err(Error::BufferTooSmall);
		}
		into[0..2].copy_from_slice(&descriptor.command_register.to_le_bytes());
		let id_field = if escaped { REPORT_ID_ESCAPE } else { report_id & 0x0f };
		into[2] = id_field | (kind.code() << 4);
		into[3] = opcode.code();
		let mut written = 4;
		if escaped {
			into[written] = report_id;
			written += 1;
		}
		if with_data_register {
			into[written..written + 2].copy_from_slice(&descriptor.data_register.to_le_bytes());
			written += 2;
		}
		Ok(written)
	}
}

/// What arrived on a read of the input register.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Input<'a> {
	/// A zero-length report, which is how a device says its RESET finished. It is not an empty
	/// report and a caller that treats it as one has missed the only handshake this protocol has.
	ResetComplete,
	/// A report, without its length prefix.
	Report(&'a [u8]),
}

/// Decode one input report from the bytes a read returned.
///
/// THE DECLARED LENGTH IS CHECKED IN BOTH DIRECTIONS, which is the whole of this function. A length
/// longer than what arrived is the device asking the driver to read bytes that are not there; a
/// length shorter than its own prefix is a device that has stopped making sense; and a length longer
/// than the descriptor's own maximum is the device contradicting what it published about itself. The
/// first would have been an out-of-bounds read and the other two a report parsed at the wrong offset.
pub fn decode_input<'a>(descriptor: &Descriptor, bytes: &'a [u8]) -> Result<Input<'a>, Error> {
	if bytes.len() < LENGTH_PREFIX {
		return Err(Error::ReportLength);
	}
	let declared = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
	if declared == 0 {
		return Ok(Input::ResetComplete);
	}
	if declared < LENGTH_PREFIX {
		return Err(Error::ReportLength);
	}
	if declared > bytes.len() || declared > descriptor.max_input_len as usize {
		return Err(Error::ReportLength);
	}
	Ok(Input::Report(&bytes[LENGTH_PREFIX..declared]))
}

/// A device whose descriptor has been read: the address to reach it at and what it said about itself.
///
/// IT HOLDS NO BUS. The bus is passed to each operation instead of owned, because a driver's bus is
/// shared with every other device on it and a type that owned one could not be built twice.
pub struct Device {
	address: SlaveAddress,
	descriptor: Descriptor,
}

impl Device {
	/// Read and check the HID descriptor at the register the platform named.
	///
	/// THE REGISTER COMES FROM THE FIRMWARE AND IS NOT GUESSED. ACPI or a device tree says where the
	/// descriptor is; there is no default and no scan, because a register read to the wrong address on
	/// a shared bus is a transaction another device answers.
	pub fn probe(bus: &mut dyn I2cBus, address: SlaveAddress, descriptor_register: u16) -> Result<Self, Error> {
		let mut bytes = [0u8; HID_DESCRIPTOR_LEN];
		let read = bus.write_read(address, &descriptor_register.to_le_bytes(), &mut bytes)?;
		if read < HID_DESCRIPTOR_LEN {
			return Err(Error::DescriptorTruncated);
		}
		Ok(Self { address, descriptor: Descriptor::decode(&bytes)? })
	}

	pub const fn address(&self) -> SlaveAddress {
		self.address
	}

	pub const fn descriptor(&self) -> &Descriptor {
		&self.descriptor
	}

	/// Fetch the report descriptor into the caller's buffer, answering how many bytes it holds.
	///
	/// THE LENGTH IS THE DESCRIPTOR'S, NOT THE READ'S. A device that returns fewer bytes than it
	/// published has not given a report descriptor, and a parse of the short prefix would produce a
	/// layout whose later fields are at the wrong offsets - which is a keyboard whose keys are
	/// numbered wrongly rather than a failure anybody would trace back to here.
	pub fn report_descriptor(&self, bus: &mut dyn I2cBus, into: &mut [u8]) -> Result<usize, Error> {
		let want = self.descriptor.report_descriptor_len as usize;
		if into.len() < want {
			return Err(Error::BufferTooSmall);
		}
		let read = bus.write_read(self.address, &self.descriptor.report_descriptor_register.to_le_bytes(), &mut into[..want])?;
		if read != want {
			return Err(Error::ReportLength);
		}
		Ok(want)
	}

	/// Send RESET and wait for nothing: the device answers with a zero-length input report, which the
	/// caller reads when its interrupt says so.
	///
	/// NO SLEEP AND NO POLL LOOP HERE, deliberately. This module has no clock and no scheduler; how
	/// long to wait for the reset indication is the driver's decision and belongs where the timeouts
	/// of the rest of that driver are.
	pub fn reset(&self, bus: &mut dyn I2cBus) -> Result<(), Error> {
		let mut command = [0u8; 8];
		let written = Command::reset(&self.descriptor, &mut command)?;
		bus.write(self.address, &command[..written])?;
		Ok(())
	}

	/// Put the device to sleep or wake it.
	pub fn set_power(&self, bus: &mut dyn I2cBus, state: PowerState) -> Result<(), Error> {
		let mut command = [0u8; 8];
		let written = Command::set_power(&self.descriptor, state, &mut command)?;
		bus.write(self.address, &command[..written])?;
		Ok(())
	}

	/// Read one input report into the caller's buffer.
	///
	/// THE BUFFER MUST BE THE DEVICE'S DECLARED MAXIMUM, not what the caller expects a report to be.
	/// A driver that sized its buffer to the reports it has seen would truncate the first longer one,
	/// and a truncated report decodes as a different report rather than as an error.
	pub fn read_input<'a>(&self, bus: &mut dyn I2cBus, into: &'a mut [u8]) -> Result<Input<'a>, Error> {
		let want = self.descriptor.max_input_len as usize;
		if into.len() < want {
			return Err(Error::BufferTooSmall);
		}
		let read = bus.read(self.address, &mut into[..want])?;
		decode_input(&self.descriptor, &into[..read])
	}

	/// Ask for one report by id and type, and decode the answer.
	pub fn get_report<'a>(&self, bus: &mut dyn I2cBus, kind: ReportType, report_id: u8, into: &'a mut [u8]) -> Result<&'a [u8], Error> {
		let mut command = [0u8; 8];
		let written = Command::get_report(&self.descriptor, kind, report_id, &mut command)?;
		let want = self.descriptor.max_input_len as usize;
		if into.len() < want {
			return Err(Error::BufferTooSmall);
		}
		let read = bus.write_read(self.address, &command[..written], &mut into[..want])?;
		// A FETCHED REPORT CARRIES THE SAME LENGTH PREFIX AN UNSOLICITED ONE DOES, and it is checked
		// by the same rule - but a zero length here is not a reset indication, it is an empty answer
		// to a question, which the caller asked and did not get.
		let end = match decode_input(&self.descriptor, &into[..read])? {
			Input::Report(body) => LENGTH_PREFIX + body.len(),
			Input::ResetComplete => return Err(Error::ReportLength),
		};
		Ok(&into[LENGTH_PREFIX..end])
	}

	/// Send one report to the device.
	pub fn set_report(&self, bus: &mut dyn I2cBus, kind: ReportType, report_id: u8, body: &[u8], scratch: &mut [u8]) -> Result<(), Error> {
		let written = Command::set_report(&self.descriptor, kind, report_id, body, scratch)?;
		bus.write(self.address, &scratch[..written])?;
		Ok(())
	}
}

#[cfg(test)]
mod tests;
