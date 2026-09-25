// THE COMMAND RESPONSE BUFFER INTERFACE (TCG PC Client Platform TPM Profile, CRB), at locality 0.
//
// THE TPM NAMES ITS OWN BUFFERS, and they are physical addresses. A driver that wrote wherever the control
// area pointed would write wherever a TPM - or anything able to change that register - wanted, so an address
// is believed only inside the region this transport was given, and a size only up to what that region holds.
//
// A COMMAND IS ONE WRITE AND ONE START: the command in its buffer, `CTRL_START` set, and the TPM clearing it when
// the response is in the other. A start that does not clear within the command's duration is CANCELLED -
// `CTRL_CANCEL` set until the start clears, then cleared - and the interface put back to idle, as a FIFO's is.

use crate::command::Transport;
use crate::{MAX_MESSAGE, Registers, TIMEOUT_A_MS, TIMEOUT_B_MS, TIMEOUT_C_MS, TransportError, wait_for};
use alloc::vec::Vec;

pub const LOC_STATE: usize = 0x00;
pub const LOC_CTRL: usize = 0x08;
pub const LOC_STS: usize = 0x0C;
pub const INTF_ID: usize = 0x30;
pub const CTRL_REQ: usize = 0x40;
pub const CTRL_STS: usize = 0x44;
pub const CTRL_CANCEL: usize = 0x48;
pub const CTRL_START: usize = 0x4C;
pub const CTRL_CMD_SIZE: usize = 0x58;
pub const CTRL_CMD_LADDR: usize = 0x5C;
pub const CTRL_CMD_HADDR: usize = 0x60;
pub const CTRL_RSP_SIZE: usize = 0x64;
pub const CTRL_RSP_ADDR: usize = 0x68;

pub const LOC_STATE_ASSIGNED: u32 = 1 << 1;
pub const LOC_STATE_VALID: u32 = 1 << 7;
pub const LOC_CTRL_REQUEST: u32 = 1 << 0;
pub const LOC_CTRL_RELINQUISH: u32 = 1 << 1;
pub const LOC_STS_GRANTED: u32 = 1 << 0;
pub const CTRL_REQ_READY: u32 = 1 << 0;
pub const CTRL_REQ_IDLE: u32 = 1 << 1;
pub const CTRL_STS_FATAL: u32 = 1 << 0;
pub const CTRL_STS_IDLE: u32 = 1 << 1;

pub struct Crb<R: Registers> {
	pub registers: R,
	/// The physical base the registers are mapped from, and how much of it this transport may touch.
	base: u64,
	len: usize,
}

impl<R: Registers> Crb<R> {
	/// A CRB interface over the region at physical `base`, `len` bytes long - refused if the interface says it is
	/// not a CRB.
	pub fn new(mut registers: R, base: u64, len: usize) -> Result<Crb<R>, TransportError> {
		if registers.read32(INTF_ID) & 0xF != crate::fifo::INTERFACE_CRB {
			return Err(TransportError::Fault);
		}
		Ok(Crb { registers, base, len })
	}

	// Where a buffer the TPM named is inside this region, if all `size` bytes of it are.
	fn inside(&self, address: u64, size: usize) -> Result<usize, TransportError> {
		let offset = address.checked_sub(self.base).ok_or(TransportError::Buffer)? as usize;
		if offset < CTRL_RSP_ADDR + 8 || offset.checked_add(size).is_none_or(|end| end > self.len) {
			return Err(TransportError::Buffer);
		}
		Ok(offset)
	}

	fn request_locality(&mut self) -> Result<(), TransportError> {
		let granted = |registers: &mut R| {
			let state = registers.read32(LOC_STATE);
			registers.read32(LOC_STS) & LOC_STS_GRANTED != 0 && state & (LOC_STATE_VALID | LOC_STATE_ASSIGNED) == LOC_STATE_VALID | LOC_STATE_ASSIGNED && (state >> 2) & 0x7 == 0
		};
		self.registers.write32(LOC_CTRL, LOC_CTRL_REQUEST);
		if wait_for(&mut self.registers, TIMEOUT_A_MS, granted) { Ok(()) } else { Err(TransportError::Locality) }
	}

	fn exchange(&mut self, command: &[u8], response: &mut Vec<u8>, duration_ms: u64) -> Result<(), TransportError> {
		self.registers.write32(CTRL_REQ, CTRL_REQ_READY);
		if !wait_for(&mut self.registers, TIMEOUT_B_MS, |registers| registers.read32(CTRL_REQ) & CTRL_REQ_READY == 0 && registers.read32(CTRL_STS) & CTRL_STS_IDLE == 0) {
			return Err(TransportError::NotReady);
		}
		if self.registers.read32(CTRL_STS) & CTRL_STS_FATAL != 0 {
			return Err(TransportError::Fault);
		}
		let command_size = self.registers.read32(CTRL_CMD_SIZE) as usize;
		let command_address = (self.registers.read32(CTRL_CMD_HADDR) as u64) << 32 | self.registers.read32(CTRL_CMD_LADDR) as u64;
		let response_size = self.registers.read32(CTRL_RSP_SIZE) as usize;
		let response_address = (self.registers.read32(CTRL_RSP_ADDR + 4) as u64) << 32 | self.registers.read32(CTRL_RSP_ADDR) as u64;
		let command_at = self.inside(command_address, command_size)?;
		let response_at = self.inside(response_address, response_size)?;
		if command.len() > command_size || response_size < 10 {
			return Err(TransportError::Buffer);
		}
		for (at, byte) in command.iter().enumerate() {
			self.registers.write8(command_at + at, *byte);
		}
		self.registers.write32(CTRL_START, 1);
		let finished = |registers: &mut R| registers.read32(CTRL_START) == 0;
		if !wait_for(&mut self.registers, duration_ms, finished) {
			self.registers.write32(CTRL_CANCEL, 1);
			let _ = wait_for(&mut self.registers, TIMEOUT_B_MS, finished);
			self.registers.write32(CTRL_CANCEL, 0);
			return Err(TransportError::TimedOut);
		}
		if self.registers.read32(CTRL_STS) & CTRL_STS_FATAL != 0 {
			return Err(TransportError::Fault);
		}
		for at in 0..10 {
			response.push(self.registers.read8(response_at + at));
		}
		let size = u32::from_be_bytes([response[2], response[3], response[4], response[5]]) as usize;
		if size < 10 || size > response_size || size > MAX_MESSAGE {
			return Err(TransportError::Length);
		}
		for at in 10..size {
			response.push(self.registers.read8(response_at + at));
		}
		Ok(())
	}
}

impl<R: Registers> Transport for Crb<R> {
	fn execute(&mut self, command: &[u8], response: &mut Vec<u8>, duration_ms: u64) -> Result<(), TransportError> {
		if command.len() < 10 || command.len() > MAX_MESSAGE {
			return Err(TransportError::Length);
		}
		self.request_locality()?;
		let result = self.exchange(command, response, duration_ms);
		self.registers.write32(CTRL_REQ, CTRL_REQ_IDLE);
		let _ = wait_for(&mut self.registers, TIMEOUT_C_MS, |registers| registers.read32(CTRL_REQ) & CTRL_REQ_IDLE == 0);
		self.registers.write32(LOC_CTRL, LOC_CTRL_RELINQUISH);
		result
	}
}
