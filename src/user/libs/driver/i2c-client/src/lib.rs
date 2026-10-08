//! THE I2C BUS CONTRACT'S CLIENT SIDE, over one ADDRESS-SCOPED connection to an `i2c-bus` controller.
//!
//! TWO CLIENT TYPES, because two kinds of consumer bind to a bus. A HID-over-I2C device speaks plain I2C
//! registers, so `ScopedBus` is `hid_i2c::I2cBus` - the bus trait that library states, implemented UNCHANGED
//! over the contract, so the protocol library stays free of IPC. An SMBus device - an IPMI SSIF interface -
//! speaks SMBus transactions, so `Smbus` is those.
//!
//! EACH REFUSES AT BIND WHAT THE CONTROLLER DOES NOT DECLARE, instead of failing later: `ScopedBus` a
//! controller that serves no plain I2C, `Smbus` one that lacks a transaction its consumer says it needs.
//!
//! THE ADDRESS IS THE CONNECTION'S. The contract carries none - DeviceManager scoped the connection to one
//! address when it minted it, and the controller serves that address alone - so `hid_i2c::I2cBus`'s address
//! argument can only ever name that one. Any other is refused HERE, before anything is sent: a consumer that
//! asked for a neighbour on the bus has a defect, and the answer is the one a bus gives a missing device.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use hid_i2c::{BusError, I2cBus, MAX_TRANSFER, SlaveAddress};
use i2c_device_proto::codec::Transport;
use i2c_device_proto::generated::liber::i2c_device::v1::{Error as ContractError, I2cFunctionality, I2cReply, I2cStatus, i2c_device::Client};

/// Why a client was not made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// The controller did not answer what it serves, or which address this connection reaches.
	NoAnswer,
	/// The controller does not declare a transaction this consumer needs.
	Unsupported,
}

// What the controller says it serves, and the address the connection reaches.
fn ask<T: Transport>(client: &mut Client<T>) -> Result<(I2cFunctionality, u8), Refusal> {
	let functionality = client.functionality().and_then(Result::ok).ok_or(Refusal::NoAnswer)?;
	let address = client.address().and_then(Result::ok).ok_or(Refusal::NoAnswer)?;
	Ok((functionality, address))
}

/// `hid_i2c::I2cBus` over one address-scoped connection.
pub struct ScopedBus<T: Transport> {
	client: Client<T>,
	address: u8,
	// The longest plain message: the controller's declared maximum, or the protocol's, whichever is less.
	max_transfer: usize,
}

impl<T: Transport> ScopedBus<T> {
	/// Refused when the controller serves no plain I2C.
	pub fn new(mut client: Client<T>) -> Result<Self, Refusal> {
		let (functionality, address) = ask(&mut client)?;
		if !functionality.plain {
			return Err(Refusal::Unsupported);
		}
		Ok(Self { client, address, max_transfer: (functionality.max_transfer as usize).min(MAX_TRANSFER) })
	}

	/// The seven-bit address this connection reaches.
	pub fn address(&self) -> u8 {
		self.address
	}

	pub fn into_client(self) -> Client<T> {
		self.client
	}

	// THE TRANSFER MAY GO: to this connection's own address, within the bound.
	fn check(&self, address: SlaveAddress, lens: &[usize]) -> Result<(), BusError> {
		if address.get() != self.address {
			return Err(BusError::NoDevice);
		}
		if lens.iter().any(|&len| len > self.max_transfer) {
			return Err(BusError::TooLong);
		}
		Ok(())
	}
}

// A plain transfer's answer as the bus trait says it: the status, and the bytes of a read.
fn plain(reply: Option<Result<I2cReply, ContractError>>) -> Result<Vec<u8>, BusError> {
	let reply = reply.and_then(Result::ok).ok_or(BusError::Controller)?;
	match reply.status {
		I2cStatus::Ok => Ok(reply.bytes),
		I2cStatus::NoDevice => Err(BusError::NoDevice),
		I2cStatus::Interrupted | I2cStatus::Pec => Err(BusError::Interrupted),
		I2cStatus::TooLong => Err(BusError::TooLong),
		I2cStatus::Controller | I2cStatus::Unsupported => Err(BusError::Controller),
	}
}

// A read's bytes into the caller's buffer. A controller that answered a different count than was asked
// for has answered something that is not the transfer.
fn fill(bytes: Vec<u8>, into: &mut [u8]) -> Result<usize, BusError> {
	if bytes.len() != into.len() {
		return Err(BusError::Controller);
	}
	into.copy_from_slice(&bytes);
	Ok(bytes.len())
}

impl<T: Transport> I2cBus for ScopedBus<T> {
	fn write(&mut self, address: SlaveAddress, bytes: &[u8]) -> Result<(), BusError> {
		self.check(address, &[bytes.len()])?;
		plain(self.client.write(bytes)).map(|_| ())
	}

	fn read(&mut self, address: SlaveAddress, into: &mut [u8]) -> Result<usize, BusError> {
		self.check(address, &[into.len()])?;
		let bytes = plain(self.client.read(&(into.len() as u16)))?;
		fill(bytes, into)
	}

	fn write_read(&mut self, address: SlaveAddress, write: &[u8], into: &mut [u8]) -> Result<usize, BusError> {
		self.check(address, &[write.len(), into.len()])?;
		let bytes = plain(self.client.write_read(write, &(into.len() as u16)))?;
		fill(bytes, into)
	}
}

/// Why an SMBus transaction failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SmbusError {
	NoDevice,
	Interrupted,
	TooLong,
	Controller,
	/// The packet error code the device sent does not match its bytes.
	Pec,
	/// The controller answered something that is not this transaction's answer.
	Malformed,
}

/// THE SMBUS TRANSACTIONS over one address-scoped connection - the second client type, the one SSIF uses.
///
/// WHAT IT NEEDS IS STATED ONCE, at `new`: the transactions this consumer will ask for, and whether every one
/// of them carries a packet error code. A transaction it did not state is answered `Unsupported`-shaped
/// (`Controller`) without being sent, so a consumer cannot drift past what it was admitted with.
pub struct Smbus<T: Transport> {
	client: Client<T>,
	address: u8,
	needs: I2cFunctionality,
}

impl<T: Transport> Smbus<T> {
	/// Refused when the controller does not declare every transaction in `needs`, or `needs.pec` and no PEC.
	pub fn new(mut client: Client<T>, needs: I2cFunctionality) -> Result<Self, Refusal> {
		let (declared, address) = ask(&mut client)?;
		let wanted = [needs.quick, needs.byte, needs.byte_data, needs.word_data, needs.block_write, needs.block_read, needs.i2c_block_read, needs.pec];
		let offered = [declared.quick, declared.byte, declared.byte_data, declared.word_data, declared.block_write, declared.block_read, declared.i2c_block_read, declared.pec];
		if wanted.iter().zip(offered.iter()).any(|(&want, &has)| want && !has) {
			return Err(Refusal::Unsupported);
		}
		Ok(Self { client, address, needs })
	}

	/// The seven-bit address this connection reaches.
	pub fn address(&self) -> u8 {
		self.address
	}

	pub fn into_client(self) -> Client<T> {
		self.client
	}

	/// Set one absolute tick deadline shared by the following SMBus operations (zero waits forever).
	pub fn set_deadline(&mut self, deadline: u64) {
		self.client.set_deadline(deadline);
	}

	// One transaction's answer, when it was admitted at all: the bytes, of exactly `len` when that is known.
	fn answer(&self, admitted: bool, reply: Option<Result<I2cReply, ContractError>>, len: Option<usize>) -> Result<Vec<u8>, SmbusError> {
		if !admitted {
			return Err(SmbusError::Controller);
		}
		let reply = reply.and_then(Result::ok).ok_or(SmbusError::Controller)?;
		match reply.status {
			I2cStatus::Ok if len.is_none_or(|len| reply.bytes.len() == len) => Ok(reply.bytes),
			I2cStatus::Ok => Err(SmbusError::Malformed),
			I2cStatus::NoDevice => Err(SmbusError::NoDevice),
			I2cStatus::Interrupted => Err(SmbusError::Interrupted),
			I2cStatus::TooLong => Err(SmbusError::TooLong),
			I2cStatus::Pec => Err(SmbusError::Pec),
			I2cStatus::Controller | I2cStatus::Unsupported => Err(SmbusError::Controller),
		}
	}

	pub fn quick(&mut self, read: bool) -> Result<(), SmbusError> {
		let reply = if self.needs.quick { self.client.quick(&read) } else { None };
		self.answer(self.needs.quick, reply, Some(0)).map(|_| ())
	}

	pub fn send_byte(&mut self, value: u8) -> Result<(), SmbusError> {
		let reply = if self.needs.byte { self.client.send_byte(&value, &self.needs.pec) } else { None };
		self.answer(self.needs.byte, reply, Some(0)).map(|_| ())
	}

	pub fn receive_byte(&mut self) -> Result<u8, SmbusError> {
		let reply = if self.needs.byte { self.client.receive_byte(&self.needs.pec) } else { None };
		self.answer(self.needs.byte, reply, Some(1)).map(|bytes| bytes[0])
	}

	pub fn write_byte_data(&mut self, command: u8, value: u8) -> Result<(), SmbusError> {
		let reply = if self.needs.byte_data { self.client.write_byte_data(&command, &value, &self.needs.pec) } else { None };
		self.answer(self.needs.byte_data, reply, Some(0)).map(|_| ())
	}

	pub fn read_byte_data(&mut self, command: u8) -> Result<u8, SmbusError> {
		let reply = if self.needs.byte_data { self.client.read_byte_data(&command, &self.needs.pec) } else { None };
		self.answer(self.needs.byte_data, reply, Some(1)).map(|bytes| bytes[0])
	}

	pub fn write_word_data(&mut self, command: u8, value: u16) -> Result<(), SmbusError> {
		let reply = if self.needs.word_data { self.client.write_word_data(&command, &value, &self.needs.pec) } else { None };
		self.answer(self.needs.word_data, reply, Some(0)).map(|_| ())
	}

	/// The word, low byte first on the bus.
	pub fn read_word_data(&mut self, command: u8) -> Result<u16, SmbusError> {
		let reply = if self.needs.word_data { self.client.read_word_data(&command, &self.needs.pec) } else { None };
		self.answer(self.needs.word_data, reply, Some(2)).map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
	}

	/// At most 32 bytes, which the controller sends with their count.
	pub fn block_write(&mut self, command: u8, data: &[u8]) -> Result<(), SmbusError> {
		if data.is_empty() || data.len() > 32 {
			return Err(SmbusError::TooLong);
		}
		let reply = if self.needs.block_write { self.client.block_write(&command, data, &self.needs.pec) } else { None };
		self.answer(self.needs.block_write, reply, Some(0)).map(|_| ())
	}

	/// The block whose count THE DEVICE sends first: at most 32 bytes.
	pub fn block_read(&mut self, command: u8) -> Result<Vec<u8>, SmbusError> {
		let reply = if self.needs.block_read { self.client.block_read(&command, &self.needs.pec) } else { None };
		let bytes = self.answer(self.needs.block_read, reply, None)?;
		if bytes.is_empty() || bytes.len() > 32 {
			return Err(SmbusError::Malformed);
		}
		Ok(bytes)
	}

	/// `len` bytes after a one-byte command, with no count from the device: at most 32.
	pub fn i2c_block_read(&mut self, command: u8, len: u8) -> Result<Vec<u8>, SmbusError> {
		if len == 0 || len > 32 {
			return Err(SmbusError::TooLong);
		}
		let reply = if self.needs.i2c_block_read { self.client.i2c_block_read(&command, &len, &self.needs.pec) } else { None };
		self.answer(self.needs.i2c_block_read, reply, Some(len as usize))
	}
}

#[cfg(test)]
mod tests;
