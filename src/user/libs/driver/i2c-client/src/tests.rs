use super::*;
use i2c_device_proto::codec::{Handles, TransportError};
use i2c_device_proto::generated::liber::i2c_device::v1::{Error as ContractError, i2c_device};

// A CONTROLLER AT THE FAR END OF A LOOPBACK: the generated dispatch over a model that answers from 256
// registers at one address, so every call below crosses the real codec both ways.
struct Model {
	address: u8,
	functionality: I2cFunctionality,
	registers: [u8; 256],
	pointer: u8,
	// A status every transfer answers instead, when set.
	status: Option<I2cStatus>,
	// Answers one byte short, when set.
	short: bool,
	// Every write that reached the controller.
	writes: Vec<Vec<u8>>,
}

impl Model {
	fn new(functionality: I2cFunctionality) -> Model {
		let mut registers = [0u8; 256];
		for (at, value) in registers.iter_mut().enumerate() {
			*value = at as u8 ^ 0x5A;
		}
		Model { address: 0x2C, functionality, registers, pointer: 0, status: None, short: false, writes: Vec::new() }
	}

	fn reply(&self, bytes: Vec<u8>) -> Result<I2cReply, ContractError> {
		match self.status {
			Some(status) => Ok(I2cReply { status, bytes: Vec::new() }),
			None if self.short && !bytes.is_empty() => Ok(I2cReply { status: I2cStatus::Ok, bytes: bytes[1..].to_vec() }),
			None => Ok(I2cReply { status: I2cStatus::Ok, bytes }),
		}
	}

	fn store(&mut self, data: &[u8]) {
		self.writes.push(data.to_vec());
		if let Some((&pointer, values)) = data.split_first() {
			self.pointer = pointer;
			for (at, &value) in values.iter().enumerate() {
				self.registers[pointer.wrapping_add(at as u8) as usize] = value;
			}
		}
	}

	fn load(&self, len: usize) -> Vec<u8> {
		(0..len).map(|at| self.registers[self.pointer.wrapping_add(at as u8) as usize]).collect()
	}
}

impl i2c_device::Service for Model {
	fn functionality(&mut self) -> Result<I2cFunctionality, ContractError> {
		Ok(self.functionality.clone())
	}
	fn address(&mut self) -> Result<u8, ContractError> {
		Ok(self.address)
	}
	fn write(&mut self, data: Vec<u8>) -> Result<I2cReply, ContractError> {
		self.store(&data);
		self.reply(Vec::new())
	}
	fn read(&mut self, len: u16) -> Result<I2cReply, ContractError> {
		self.reply(self.load(len as usize))
	}
	fn write_read(&mut self, data: Vec<u8>, len: u16) -> Result<I2cReply, ContractError> {
		self.store(&data);
		self.reply(self.load(len as usize))
	}
	fn quick(&mut self, _read: bool) -> Result<I2cReply, ContractError> {
		self.reply(Vec::new())
	}
	fn send_byte(&mut self, value: u8, _pec: bool) -> Result<I2cReply, ContractError> {
		self.store(&[value]);
		self.reply(Vec::new())
	}
	fn receive_byte(&mut self, _pec: bool) -> Result<I2cReply, ContractError> {
		self.reply(self.load(1))
	}
	fn write_byte_data(&mut self, command: u8, value: u8, _pec: bool) -> Result<I2cReply, ContractError> {
		self.store(&[command, value]);
		self.reply(Vec::new())
	}
	fn read_byte_data(&mut self, command: u8, _pec: bool) -> Result<I2cReply, ContractError> {
		self.pointer = command;
		self.reply(self.load(1))
	}
	fn write_word_data(&mut self, command: u8, value: u16, _pec: bool) -> Result<I2cReply, ContractError> {
		self.store(&[command, value as u8, (value >> 8) as u8]);
		self.reply(Vec::new())
	}
	fn read_word_data(&mut self, command: u8, _pec: bool) -> Result<I2cReply, ContractError> {
		self.pointer = command;
		self.reply(self.load(2))
	}
	fn block_write(&mut self, command: u8, data: Vec<u8>, _pec: bool) -> Result<I2cReply, ContractError> {
		let mut bytes = alloc::vec![command, data.len() as u8];
		bytes.extend_from_slice(&data);
		self.store(&bytes);
		self.reply(Vec::new())
	}
	fn block_read(&mut self, command: u8, _pec: bool) -> Result<I2cReply, ContractError> {
		self.pointer = command;
		self.reply(b"BLOCK".to_vec())
	}
	fn i2c_block_read(&mut self, command: u8, len: u8, _pec: bool) -> Result<I2cReply, ContractError> {
		self.pointer = command;
		self.reply(self.load(len as usize))
	}
}

struct Loopback {
	model: Model,
	calls: usize,
}

impl Transport for Loopback {
	fn call(&mut self, request: &[u8], _request_handles: &[u64], reply_handles: &mut Handles, _deadline: u64) -> Result<Vec<u8>, TransportError> {
		self.calls += 1;
		let mut handles = Handles::new();
		let mut out = alloc::vec![0u8; 4096];
		let len = i2c_device::dispatch(&mut self.model, request, &mut handles, &mut out, reply_handles).ok_or(TransportError::Malformed)?;
		out.truncate(len);
		Ok(out)
	}

	fn discard_handles(&mut self, _handles: &[u64]) {}
}

// What a virtio-i2c controller declares: plain I2C and the SMBus transactions that compose from it, not the
// block read with the device's count.
fn virtio_like() -> I2cFunctionality {
	I2cFunctionality { plain: true, max_transfer: 2048, quick: true, byte: true, byte_data: true, word_data: true, block_write: true, block_read: false, i2c_block_read: true, pec: true }
}

// What an SMBus-only controller declares: the block transactions SSIF uses and no plain I2C.
fn smbus_only() -> I2cFunctionality {
	I2cFunctionality { plain: false, max_transfer: 0, quick: false, byte: false, byte_data: false, word_data: false, block_write: true, block_read: true, i2c_block_read: false, pec: true }
}

fn none() -> I2cFunctionality {
	I2cFunctionality { plain: false, max_transfer: 0, quick: false, byte: false, byte_data: false, word_data: false, block_write: false, block_read: false, i2c_block_read: false, pec: false }
}

fn bus(functionality: I2cFunctionality) -> Result<ScopedBus<Loopback>, Refusal> {
	ScopedBus::new(Client::new(Loopback { model: Model::new(functionality), calls: 0 }))
}

#[test]
fn the_bus_trait_reaches_the_connections_own_address_and_reads_what_was_written() {
	let mut bus = bus(virtio_like()).expect("a controller serving plain I2C is taken");
	assert_eq!(bus.address(), 0x2C, "the address is the connection's, asked of the controller");
	let own = SlaveAddress::new(0x2C).unwrap();
	bus.write(own, &[0x10, 0xAA, 0xBB]).expect("the write reached the device");
	let mut two = [0u8; 2];
	assert_eq!(bus.write_read(own, &[0x10], &mut two), Ok(2));
	assert_eq!(two, [0xAA, 0xBB], "a register read is a write of the register and a read, as one transfer");
	let mut one = [0u8; 1];
	assert_eq!(bus.read(own, &mut one), Ok(1));
	assert_eq!(one, [0xAA], "and a plain read answers from the pointer the last write left");
}

#[test]
fn an_address_other_than_the_connections_is_refused_before_anything_is_sent() {
	let mut bus = bus(virtio_like()).unwrap();
	let neighbour = SlaveAddress::new(0x2D).unwrap();
	let before = bus.client_calls();
	assert_eq!(bus.write(neighbour, &[0x10, 0xEE]), Err(BusError::NoDevice));
	let mut into = [0u8; 2];
	assert_eq!(bus.read(neighbour, &mut into), Err(BusError::NoDevice));
	assert_eq!(bus.write_read(neighbour, &[0x10], &mut into), Err(BusError::NoDevice));
	assert_eq!(bus.client_calls(), before, "nothing reached the controller");
}

#[test]
fn a_transfer_past_the_bound_is_refused_and_a_status_is_the_bus_traits_error() {
	let mut bus = bus(I2cFunctionality { max_transfer: 16, ..virtio_like() }).unwrap();
	let own = SlaveAddress::new(0x2C).unwrap();
	assert_eq!(bus.write(own, &[0u8; 17]), Err(BusError::TooLong), "the controller's smaller maximum is the bound");
	let mut into = [0u8; 17];
	assert_eq!(bus.read(own, &mut into), Err(BusError::TooLong));
	for (status, error) in [
		(I2cStatus::NoDevice, BusError::NoDevice),
		(I2cStatus::Interrupted, BusError::Interrupted),
		(I2cStatus::TooLong, BusError::TooLong),
		(I2cStatus::Controller, BusError::Controller),
		(I2cStatus::Unsupported, BusError::Controller),
		(I2cStatus::Pec, BusError::Interrupted),
	] {
		let mut bus = bus_with(virtio_like(), |model| model.status = Some(status));
		assert_eq!(bus.write(own, &[0x10]), Err(error), "{status:?}");
	}
	let mut short = bus_with(virtio_like(), |model| model.short = true);
	assert_eq!(short.read(own, &mut [0u8; 4]), Err(BusError::Controller), "an answer of another length is not the transfer");
}

#[test]
fn a_controller_without_plain_i2c_is_refused_at_bind() {
	assert_eq!(bus(smbus_only()).err(), Some(Refusal::Unsupported));
}

#[test]
fn the_smbus_client_refuses_at_bind_a_transaction_the_controller_lacks() {
	let needs = I2cFunctionality { block_write: true, block_read: true, pec: true, ..none() };
	let refused = Smbus::new(Client::new(Loopback { model: Model::new(virtio_like()), calls: 0 }), needs.clone());
	assert_eq!(refused.err(), Some(Refusal::Unsupported), "a virtio-i2c controller cannot carry the block read with the device's count, so SSIF is refused on it");
	let mut ssif = Smbus::new(Client::new(Loopback { model: Model::new(smbus_only()), calls: 0 }), needs).expect("and served by a controller that declares it");
	ssif.block_write(0x02, &[0x18, 0x01]).expect("a block write");
	assert_eq!(ssif.block_read(0x03), Ok(b"BLOCK".to_vec()));
	assert_eq!(ssif.read_byte_data(0x10), Err(SmbusError::Controller), "a transaction it did not state is not sent");
	assert_eq!(ssif.block_write(0x02, &[0u8; 33]), Err(SmbusError::TooLong));
}

#[test]
fn the_smbus_client_answers_words_low_byte_first_and_names_a_pec_failure() {
	let needs = I2cFunctionality { byte: true, byte_data: true, word_data: true, i2c_block_read: true, quick: true, ..none() };
	let mut smbus = Smbus::new(Client::new(Loopback { model: Model::new(virtio_like()), calls: 0 }), needs.clone()).unwrap();
	smbus.write_word_data(0x20, 0x1234).unwrap();
	assert_eq!(smbus.read_word_data(0x20), Ok(0x1234));
	smbus.write_byte_data(0x30, 0x77).unwrap();
	assert_eq!(smbus.read_byte_data(0x30), Ok(0x77));
	assert_eq!(smbus.i2c_block_read(0x20, 2), Ok(alloc::vec![0x34, 0x12]));
	assert_eq!(smbus.quick(false), Ok(()));
	let mut failing = Smbus::new(Client::new(Loopback { model: Model { status: Some(I2cStatus::Pec), ..Model::new(virtio_like()) }, calls: 0 }), needs).unwrap();
	assert_eq!(failing.read_byte_data(0x30), Err(SmbusError::Pec));
}

fn bus_with(functionality: I2cFunctionality, set: impl FnOnce(&mut Model)) -> ScopedBus<Loopback> {
	let mut model = Model::new(functionality);
	set(&mut model);
	ScopedBus::new(Client::new(Loopback { model, calls: 0 })).unwrap()
}

impl ScopedBus<Loopback> {
	fn client_calls(&mut self) -> usize {
		// The client is moved out and back, which is the only way to reach its transport.
		let client = core::mem::replace(&mut self.client, Client::new(Loopback { model: Model::new(none()), calls: 0 }));
		let transport = client.into_transport();
		let calls = transport.calls;
		self.client = Client::new(transport);
		calls
	}
}
