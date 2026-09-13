//! Host tests over a FAKE BUS AND FIXTURES THAT ARE WRONG ON PURPOSE.
//!
//! There is no I2C controller in this tree, and this suite is the reason that does not stop the
//! protocol half from being finished and proved: every exchange below runs against a device made of
//! bytes, in the same process, in microseconds. What it holds is the half that would otherwise only
//! ever be exercised by a touchpad on somebody's laptop - a device that answers a register read while
//! it is still in reset, one whose declared report length is longer than what it sent, one that
//! publishes a maximum input length of zero, and a report id that needs the escape form.

use super::*;

/// The registers the fixture device publishes. Arbitrary, and deliberately not consecutive: a
/// protocol that worked only because the registers happened to be in order would pass a test that
/// used 1, 2, 3.
const REPORT_DESC_REGISTER: u16 = 0x0002;
const INPUT_REGISTER: u16 = 0x0003;
const OUTPUT_REGISTER: u16 = 0x0004;
const COMMAND_REGISTER: u16 = 0x0005;
const DATA_REGISTER: u16 = 0x0006;
const HID_DESC_REGISTER: u16 = 0x0020;

fn address() -> SlaveAddress {
	SlaveAddress::new(0x2c).expect("a touchpad's usual address")
}

/// A well-formed HID descriptor, which every fixture starts from and then breaks one field of.
fn descriptor_bytes(max_input_len: u16, report_descriptor_len: u16) -> [u8; HID_DESCRIPTOR_LEN] {
	let mut bytes = [0u8; HID_DESCRIPTOR_LEN];
	let mut put = |offset: usize, value: u16| bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
	put(0, HID_DESCRIPTOR_LEN as u16);
	put(2, HID_VERSION);
	put(4, report_descriptor_len);
	put(6, REPORT_DESC_REGISTER);
	put(8, INPUT_REGISTER);
	put(10, max_input_len);
	put(12, OUTPUT_REGISTER);
	put(14, OUTPUT_REGISTER + 1);
	put(16, COMMAND_REGISTER);
	put(18, DATA_REGISTER);
	put(20, 0x04f3);
	put(22, 0x3067);
	put(24, 0x0100);
	bytes
}

fn descriptor() -> Descriptor {
	Descriptor::decode(&descriptor_bytes(64, 128)).expect("a well-formed descriptor")
}

/// What the fake device did, so a test can assert on the BYTES THAT WENT OUT rather than only on the
/// answer that came back. A command whose encoding is wrong and whose answer is faked would pass a
/// test that only looked at the answer.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Transaction {
	Write(Vec<u8>),
	Read(usize),
	WriteRead(Vec<u8>, usize),
}

/// A device made of bytes: a register window it answers reads from, an input report it is holding,
/// and a log of what was asked of it.
struct FakeDevice {
	address: SlaveAddress,
	hid_descriptor: Vec<u8>,
	report_descriptor: Vec<u8>,
	/// What the next read of the input register returns, already framed.
	pending_input: Vec<u8>,
	/// What the next `write_read` returns when the write was a command rather than a register.
	pending_answer: Vec<u8>,
	log: Vec<Transaction>,
	/// A bus that is not there, for the error path.
	absent: bool,
	/// Answer every read with fewer bytes than asked for, which is a device that stopped talking.
	short_by: usize,
}

impl FakeDevice {
	fn new() -> Self {
		Self { address: address(), hid_descriptor: descriptor_bytes(64, 128).to_vec(), report_descriptor: (0..128u16).map(|index| index as u8).collect(), pending_input: Vec::new(), pending_answer: Vec::new(), log: Vec::new(), absent: false, short_by: 0 }
	}

	/// Frame a report body the way the device does: a two-byte length that counts itself.
	fn framed(body: &[u8]) -> Vec<u8> {
		let mut bytes = ((body.len() + LENGTH_PREFIX) as u16).to_le_bytes().to_vec();
		bytes.extend_from_slice(body);
		bytes
	}

	fn answering(&self, register: u16) -> Option<&[u8]> {
		if register == HID_DESC_REGISTER {
			return Some(&self.hid_descriptor);
		}
		if register == REPORT_DESC_REGISTER {
			return Some(&self.report_descriptor);
		}
		None
	}
}

impl I2cBus for FakeDevice {
	fn write(&mut self, address: SlaveAddress, bytes: &[u8]) -> Result<(), BusError> {
		if self.absent || address != self.address {
			return Err(BusError::NoDevice);
		}
		if bytes.len() > MAX_TRANSFER {
			return Err(BusError::TooLong);
		}
		self.log.push(Transaction::Write(bytes.to_vec()));
		Ok(())
	}

	fn read(&mut self, address: SlaveAddress, into: &mut [u8]) -> Result<usize, BusError> {
		if self.absent || address != self.address {
			return Err(BusError::NoDevice);
		}
		self.log.push(Transaction::Read(into.len()));
		let source = self.pending_input.clone();
		let len = source.len().min(into.len()).saturating_sub(self.short_by);
		into[..len].copy_from_slice(&source[..len]);
		Ok(len)
	}

	fn write_read(&mut self, address: SlaveAddress, write: &[u8], into: &mut [u8]) -> Result<usize, BusError> {
		if self.absent || address != self.address {
			return Err(BusError::NoDevice);
		}
		self.log.push(Transaction::WriteRead(write.to_vec(), into.len()));
		// A TWO-BYTE WRITE IS A REGISTER ADDRESS; anything longer is a command, and its answer comes
		// from the data register rather than from a register window.
		let source = if write.len() == 2 {
			let register = u16::from_le_bytes([write[0], write[1]]);
			self.answering(register).map(<[u8]>::to_vec).unwrap_or_default()
		} else {
			self.pending_answer.clone()
		};
		let len = source.len().min(into.len()).saturating_sub(self.short_by);
		into[..len].copy_from_slice(&source[..len]);
		Ok(len)
	}
}

#[test]
// The address is checked where an address enters, once, rather than at each transaction - and the
// reserved ranges at both ends of the bus are not addresses at all.
fn a_reserved_address_is_not_an_address() {
	assert!(SlaveAddress::new(0x08).is_some(), "the first general address");
	assert!(SlaveAddress::new(0x2c).is_some());
	assert!(SlaveAddress::new(0x77).is_some(), "the last one");
	for reserved in [0x00u8, 0x01, 0x07, 0x78, 0x7c, 0x7f] {
		assert!(SlaveAddress::new(reserved).is_none(), "{reserved:#04x} is reserved by the bus standard");
	}
	// And the eighth bit is not part of a seven-bit address: a caller that shifted one in gets a
	// refusal rather than a device it did not mean to talk to.
	for beyond in [0x80u8, 0xd8, 0xff] {
		assert!(SlaveAddress::new(beyond).is_none());
	}
	assert_eq!(address().get(), 0x2c);
}

#[test]
// The easy half: a well-formed descriptor comes back as what it says, including the derived body
// length that would otherwise be computed at every call site.
fn a_well_formed_descriptor_reads_as_what_it_says() {
	let descriptor = Descriptor::decode(&descriptor_bytes(64, 128)).expect("a descriptor");
	assert_eq!(descriptor.version, HID_VERSION);
	assert_eq!(descriptor.report_descriptor_len, 128);
	assert_eq!(descriptor.report_descriptor_register, REPORT_DESC_REGISTER);
	assert_eq!(descriptor.input_register, INPUT_REGISTER);
	assert_eq!(descriptor.max_input_len, 64);
	assert_eq!(descriptor.command_register, COMMAND_REGISTER);
	assert_eq!(descriptor.data_register, DATA_REGISTER);
	assert_eq!((descriptor.vendor, descriptor.product), (0x04f3, 0x3067));
	assert_eq!(descriptor.max_input_body(), 62, "the length prefix is counted inside the maximum");
}

#[test]
// Each of these is a device that is not answering with a descriptor, and each was a buffer length
// somebody would otherwise have believed.
fn a_descriptor_that_cannot_be_true_is_refused_and_says_why() {
	assert_eq!(Descriptor::decode(&[0u8; 12]).err(), Some(Error::DescriptorTruncated));

	// A DEVICE STILL IN RESET ANSWERS WITH ZEROES, which is the commonest failure of all and would
	// otherwise decode as a descriptor promising nothing.
	assert_eq!(Descriptor::decode(&[0u8; HID_DESCRIPTOR_LEN]).err(), Some(Error::DescriptorLength));

	let mut wrong_length = descriptor_bytes(64, 128);
	wrong_length[0] = 32;
	assert_eq!(Descriptor::decode(&wrong_length).err(), Some(Error::DescriptorLength));

	let mut wrong_version = descriptor_bytes(64, 128);
	wrong_version[2..4].copy_from_slice(&0x0200u16.to_le_bytes());
	assert_eq!(Descriptor::decode(&wrong_version).err(), Some(Error::DescriptorVersion));

	let mut dirty_reserved = descriptor_bytes(64, 128);
	dirty_reserved[28] = 1;
	assert_eq!(Descriptor::decode(&dirty_reserved).err(), Some(Error::DescriptorReserved));

	assert_eq!(Descriptor::decode(&descriptor_bytes(64, 0)).err(), Some(Error::ReportDescriptorLength));
	assert_eq!(Descriptor::decode(&descriptor_bytes(64, MAX_REPORT_DESCRIPTOR as u16 + 1)).err(), Some(Error::ReportDescriptorLength));

	// A maximum input length of zero or one cannot hold the prefix that is counted inside it.
	assert_eq!(Descriptor::decode(&descriptor_bytes(0, 128)).err(), Some(Error::InputLengthTooSmall));
	assert_eq!(Descriptor::decode(&descriptor_bytes(1, 128)).err(), Some(Error::InputLengthTooSmall));
	// Two is legal and means the device sends nothing but the reset indication.
	assert_eq!(Descriptor::decode(&descriptor_bytes(2, 128)).map(|descriptor| descriptor.max_input_body()), Ok(0));
	// And a device promising more than the bus will carry is refused before anything is allocated.
	assert_eq!(Descriptor::decode(&descriptor_bytes(u16::MAX, 128)).err(), Some(Error::TooLong));
}

#[test]
// The whole bring-up, in order, against the fake device: probe, fetch the report descriptor, reset,
// and read the indication that says the reset finished.
fn a_device_is_brought_up_by_reading_what_it_publishes() {
	let mut bus = FakeDevice::new();
	let device = Device::probe(&mut bus, address(), HID_DESC_REGISTER).expect("a device");
	assert_eq!(device.descriptor().report_descriptor_len, 128);
	assert_eq!(bus.log[0], Transaction::WriteRead(HID_DESC_REGISTER.to_le_bytes().to_vec(), HID_DESCRIPTOR_LEN), "the descriptor register is written and the answer read in ONE transaction");

	let mut buffer = [0u8; 256];
	let read = device.report_descriptor(&mut bus, &mut buffer).expect("a report descriptor");
	assert_eq!(read, 128);
	assert_eq!(&buffer[..4], &[0, 1, 2, 3], "the descriptor's own bytes, handed to the generic parser unchanged");

	device.reset(&mut bus).expect("a reset");
	let Transaction::Write(bytes) = &bus.log[2] else { panic!("a reset is a write") };
	assert_eq!(bytes, &[0x05, 0x00, 0x10, 0x01], "the command register, then report type 1 with id 0, then opcode RESET");

	// THE DEVICE ANSWERS A RESET WITH A ZERO-LENGTH REPORT and that is a handshake rather than an
	// empty report. A driver that treated it as one would wait for ever for a completion it had
	// already been given.
	bus.pending_input = vec![0, 0];
	let mut input = [0u8; 64];
	assert_eq!(device.read_input(&mut bus, &mut input), Ok(Input::ResetComplete));
}

#[test]
// The command encoding, byte by byte, because a command whose bytes are wrong and whose answer is
// faked would pass every test that looked only at the answer.
fn a_command_is_the_register_the_type_the_id_and_the_opcode() {
	let descriptor = descriptor();
	let mut buffer = [0u8; 16];

	// GET_REPORT of feature report 3: type 3 in the high nibble, id 3 in the low one, then the
	// opcode, then the data register the answer is read from.
	let written = Command::get_report(&descriptor, ReportType::Feature, 3, &mut buffer).expect("a command");
	assert_eq!(&buffer[..written], &[0x05, 0x00, 0x33, 0x02, 0x06, 0x00]);

	// An INPUT report of id 1.
	let written = Command::get_report(&descriptor, ReportType::Input, 1, &mut buffer).expect("a command");
	assert_eq!(&buffer[..written], &[0x05, 0x00, 0x11, 0x02, 0x06, 0x00]);

	// AN ID OF FIFTEEN OR MORE TAKES THE ESCAPE FORM, which is the one place this encoding is not a
	// straight bit field: the low nibble carries 0x0f and the real id follows the opcode.
	let written = Command::get_report(&descriptor, ReportType::Input, 15, &mut buffer).expect("a command");
	assert_eq!(&buffer[..written], &[0x05, 0x00, 0x1f, 0x02, 0x0f, 0x06, 0x00]);
	let written = Command::get_report(&descriptor, ReportType::Output, 200, &mut buffer).expect("a command");
	assert_eq!(&buffer[..written], &[0x05, 0x00, 0x2f, 0x02, 0xc8, 0x06, 0x00], "report 200 is not report 8");

	// SET_POWER puts the state where a report id would go, and needs no data register.
	let written = Command::set_power(&descriptor, PowerState::Sleep, &mut buffer).expect("a command");
	assert_eq!(&buffer[..written], &[0x05, 0x00, 0x11, 0x08]);
	let written = Command::set_power(&descriptor, PowerState::On, &mut buffer).expect("a command");
	assert_eq!(&buffer[..written], &[0x05, 0x00, 0x10, 0x08]);

	// AND A BUFFER THAT CANNOT HOLD THE COMMAND IS A REFUSAL, not a truncated command on the wire.
	let mut tiny = [0u8; 5];
	assert_eq!(Command::get_report(&descriptor, ReportType::Input, 1, &mut tiny).err(), Some(Error::BufferTooSmall));
}

#[test]
// A report sent to the device carries its own length prefix, and the prefix counts itself and the
// report id byte. Stating that once here is what stops every caller restating it differently.
fn a_report_sent_to_the_device_counts_its_own_prefix() {
	let descriptor = descriptor();
	let mut buffer = [0u8; 32];

	// With a report id: command, data register, then length 5 = two prefix bytes, one id byte, two
	// body bytes.
	let written = Command::set_report(&descriptor, ReportType::Output, 2, &[0xaa, 0xbb], &mut buffer).expect("a report");
	assert_eq!(&buffer[..written], &[0x05, 0x00, 0x22, 0x03, 0x06, 0x00, 0x05, 0x00, 0x02, 0xaa, 0xbb]);

	// Without one: length 4, and no id byte in the body.
	let written = Command::set_report(&descriptor, ReportType::Feature, 0, &[0xaa, 0xbb], &mut buffer).expect("a report");
	assert_eq!(&buffer[..written], &[0x05, 0x00, 0x30, 0x03, 0x06, 0x00, 0x04, 0x00, 0xaa, 0xbb]);

	// A body that does not fit the caller's buffer is refused rather than half-written.
	let mut small = [0u8; 10];
	assert_eq!(Command::set_report(&descriptor, ReportType::Output, 2, &[0; 8], &mut small).err(), Some(Error::BufferTooSmall));
}

#[test]
// THE ONE RULE OF THE INPUT PATH: the declared length is checked in both directions. A length longer
// than what arrived is the device asking for a read past the buffer; a length shorter than its own
// prefix, or longer than the device's own published maximum, is a device contradicting itself.
fn an_input_report_is_believed_only_as_far_as_it_arrived() {
	let descriptor = descriptor();

	let framed = FakeDevice::framed(&[1, 2, 3, 4]);
	assert_eq!(decode_input(&descriptor, &framed), Ok(Input::Report(&[1, 2, 3, 4][..])));

	// Zero is the reset indication and is not a report.
	assert_eq!(decode_input(&descriptor, &[0, 0]), Ok(Input::ResetComplete));

	// One byte is not even a length.
	assert_eq!(decode_input(&descriptor, &[4]).err(), Some(Error::ReportLength));

	// A length of one cannot include its own two-byte prefix.
	assert_eq!(decode_input(&descriptor, &[1, 0, 9, 9]).err(), Some(Error::ReportLength));

	// A length longer than what arrived: the out-of-bounds read this check exists for.
	assert_eq!(decode_input(&descriptor, &[16, 0, 9, 9]).err(), Some(Error::ReportLength));

	// A length longer than the device's own published maximum, even though the bytes are there.
	let long = FakeDevice::framed(&[0u8; 80]);
	assert_eq!(decode_input(&descriptor, &long).err(), Some(Error::ReportLength), "the device published 64 and sent 82");

	// AND A REPORT SHORTER THAN WHAT ARRIVED IS NOT AN ERROR: a bus read of the maximum length
	// returns the maximum, and the report inside it says how much of that is the report.
	let mut buffer = FakeDevice::framed(&[7, 7]);
	buffer.resize(64, 0xee);
	assert_eq!(decode_input(&descriptor, &buffer), Ok(Input::Report(&[7, 7][..])));
}

#[test]
// The same rule through the device path, where the bytes come off a bus that can also stop mid-read.
fn a_device_that_stops_talking_mid_read_is_refused_rather_than_parsed() {
	let mut bus = FakeDevice::new();
	let device = Device::probe(&mut bus, address(), HID_DESC_REGISTER).expect("a device");
	let mut input = [0u8; 64];

	bus.pending_input = FakeDevice::framed(&[1, 2, 3]);
	assert_eq!(device.read_input(&mut bus, &mut input), Ok(Input::Report(&[1, 2, 3][..])));

	// The device declares five bytes and the bus returns four of them.
	bus.short_by = 1;
	assert_eq!(device.read_input(&mut bus, &mut input).err(), Some(Error::ReportLength));

	// A buffer smaller than the device's declared maximum is refused: sizing a buffer to the reports
	// seen so far is how the first longer one is silently truncated.
	bus.short_by = 0;
	let mut small = [0u8; 8];
	assert_eq!(device.read_input(&mut bus, &mut small).err(), Some(Error::BufferTooSmall));
}

#[test]
// A fetched report goes through the same framing check as an unsolicited one, and a zero-length
// answer to a question is not a reset indication.
fn a_fetched_report_is_framed_like_any_other() {
	let mut bus = FakeDevice::new();
	let device = Device::probe(&mut bus, address(), HID_DESC_REGISTER).expect("a device");
	let mut buffer = [0u8; 64];

	bus.pending_answer = FakeDevice::framed(&[0xde, 0xad]);
	assert_eq!(device.get_report(&mut bus, ReportType::Feature, 3, &mut buffer), Ok(&[0xde, 0xad][..]));
	let Transaction::WriteRead(bytes, _) = &bus.log[1] else { panic!("a fetch is a write-then-read") };
	assert_eq!(bytes, &[0x05, 0x00, 0x33, 0x02, 0x06, 0x00]);

	bus.pending_answer = vec![0, 0];
	assert_eq!(device.get_report(&mut bus, ReportType::Feature, 3, &mut buffer).err(), Some(Error::ReportLength), "an empty answer to a question is not a reset");
}

#[test]
// A bus that does not answer is a bus error and not a protocol error, because a caller retries one
// and refuses the other.
fn a_bus_that_does_not_answer_says_so_in_its_own_vocabulary() {
	let mut bus = FakeDevice::new();
	bus.absent = true;
	assert_eq!(Device::probe(&mut bus, address(), HID_DESC_REGISTER).err(), Some(Error::Bus(BusError::NoDevice)));

	// And a device at another address on the same bus is not this one.
	let mut bus = FakeDevice::new();
	let elsewhere = SlaveAddress::new(0x15).expect("another address");
	assert_eq!(Device::probe(&mut bus, elsewhere, HID_DESC_REGISTER).err(), Some(Error::Bus(BusError::NoDevice)));
}

#[test]
// A device whose report descriptor read comes back short has not given a report descriptor, and a
// parse of the prefix would produce a layout whose later fields are at the wrong offsets.
fn a_short_report_descriptor_is_not_a_report_descriptor() {
	let mut bus = FakeDevice::new();
	let device = Device::probe(&mut bus, address(), HID_DESC_REGISTER).expect("a device");
	let mut buffer = [0u8; 256];

	bus.short_by = 4;
	assert_eq!(device.report_descriptor(&mut bus, &mut buffer).err(), Some(Error::ReportLength));

	// And a buffer that cannot hold what the device published is refused before the read.
	bus.short_by = 0;
	let mut small = [0u8; 64];
	assert_eq!(device.report_descriptor(&mut bus, &mut small).err(), Some(Error::BufferTooSmall));
}

#[test]
// Power transitions are the only commands a driver sends without being asked, and a sleep that
// encodes as a wake is the failure nobody sees until a battery is flat.
fn a_power_transition_says_which_way_it_goes() {
	let mut bus = FakeDevice::new();
	let device = Device::probe(&mut bus, address(), HID_DESC_REGISTER).expect("a device");

	device.set_power(&mut bus, PowerState::Sleep).expect("a sleep");
	device.set_power(&mut bus, PowerState::On).expect("a wake");
	let Transaction::Write(sleep) = &bus.log[1] else { panic!("a power command is a write") };
	let Transaction::Write(wake) = &bus.log[2] else { panic!("a power command is a write") };
	assert_eq!(sleep[3], 0x08, "the SET_POWER opcode");
	assert_eq!(sleep[2] & 0x0f, 1, "sleep is state one");
	assert_eq!(wake[2] & 0x0f, 0, "on is state zero");
	assert_ne!(sleep, wake);
}

#[test]
// The report descriptor this module fetches is handed to the generic HID parser unchanged, and the
// two ceilings that bound it are the ones a caller allocates against.
fn the_ceilings_are_the_ones_a_caller_allocates_against() {
	assert!(MAX_REPORT_DESCRIPTOR <= MAX_TRANSFER * 2, "a descriptor is fetched in transfers the bus can carry");
	assert!(HID_DESCRIPTOR_LEN < MAX_TRANSFER);
	// A device may publish a maximum input length right at the bus ceiling and be accepted; one byte
	// beyond it is refused, and that boundary is what the driver's buffer is sized by.
	assert!(Descriptor::decode(&descriptor_bytes(MAX_TRANSFER as u16, 128)).is_ok());
	assert_eq!(Descriptor::decode(&descriptor_bytes(MAX_TRANSFER as u16 + 1, 128)).err(), Some(Error::TooLong));
}
