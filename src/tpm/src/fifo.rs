// THE FIFO INTERFACE (TCG PC Client Platform TPM Profile, the TIS register set for TPM 2.0), at locality 0.
//
// A COMMAND IS WRITTEN IN BURSTS AND THE TPM SAYS HOW BIG. `burstCount` is how many bytes the FIFO takes before
// it must be asked again, and `Expect` is whether it still wants more: set after every byte but the last and
// clear after the last. A driver that wrote the whole command at once, or never looked at `Expect`, finds out
// that the TPM stopped listening only when the response is garbage. The response is read the same way, header
// first, and `dataAvail` must be clear once the size the header declared has been read.
//
// WHATEVER HAPPENS, THE INTERFACE IS PUT BACK: commandReady returns it to idle and the locality is relinquished,
// on the failure paths too - a TPM left holding a half-read response refuses the next command.

use crate::command::Transport;
use crate::{MAX_MESSAGE, Registers, TIMEOUT_A_MS, TIMEOUT_B_MS, TIMEOUT_C_MS, TIMEOUT_D_MS, TransportError, wait_for};
use alloc::vec::Vec;

pub const ACCESS: usize = 0x00;
pub const STS: usize = 0x18;
pub const DATA_FIFO: usize = 0x24;
pub const INTERFACE_ID: usize = 0x30;
pub const DID_VID: usize = 0xF00;

pub const ACCESS_REQUEST_USE: u8 = 1 << 1;
pub const ACCESS_ACTIVE_LOCALITY: u8 = 1 << 5;
pub const ACCESS_VALID: u8 = 1 << 7;

pub const STS_EXPECT: u32 = 1 << 3;
pub const STS_DATA_AVAIL: u32 = 1 << 4;
pub const STS_GO: u32 = 1 << 5;
pub const STS_COMMAND_READY: u32 = 1 << 6;
pub const STS_VALID: u32 = 1 << 7;
pub const STS_COMMAND_CANCEL: u32 = 1 << 24;

/// The interface type in `TPM_INTERFACE_ID` bits 3:0: the FIFO for TPM 2.0, and the legacy TIS 1.3 whose
/// register does not exist and reads as all ones.
pub const INTERFACE_FIFO: u32 = 0x0;
pub const INTERFACE_CRB: u32 = 0x1;
pub const INTERFACE_LEGACY: u32 = 0xF;

pub struct Fifo<R: Registers> {
	pub registers: R,
}

impl<R: Registers> Fifo<R> {
	/// A FIFO interface - refused if the interface says it is a CRB, or its access register is not valid.
	pub fn new(mut registers: R) -> Result<Fifo<R>, TransportError> {
		let kind = registers.read32(INTERFACE_ID) & 0xF;
		if kind != INTERFACE_FIFO && kind != INTERFACE_LEGACY {
			return Err(TransportError::Fault);
		}
		if registers.read8(ACCESS) & ACCESS_VALID == 0 {
			return Err(TransportError::Fault);
		}
		Ok(Fifo { registers })
	}

	fn request_locality(&mut self) -> Result<(), TransportError> {
		let granted = |registers: &mut R| registers.read8(ACCESS) & (ACCESS_VALID | ACCESS_ACTIVE_LOCALITY) == ACCESS_VALID | ACCESS_ACTIVE_LOCALITY;
		if granted(&mut self.registers) {
			return Ok(());
		}
		self.registers.write8(ACCESS, ACCESS_REQUEST_USE);
		if wait_for(&mut self.registers, TIMEOUT_A_MS, granted) { Ok(()) } else { Err(TransportError::Locality) }
	}

	fn relinquish(&mut self) {
		self.registers.write8(ACCESS, ACCESS_ACTIVE_LOCALITY);
	}

	// The burst count once it is non-zero: how many bytes the FIFO moves before it must be asked again.
	//
	// NOT GATED ON `stsValid`, which says whether Expect and dataAvail mean anything and nothing about the burst
	// count: a TPM that has just gone ready has taken no byte yet, has nothing to say about Expect, and - QEMU's
	// FIFO among them - leaves `stsValid` clear until the first byte arrives. Waiting for it there waits for ever.
	fn burst(&mut self) -> Result<usize, TransportError> {
		let mut count = 0;
		let found = wait_for(&mut self.registers, TIMEOUT_D_MS.max(TIMEOUT_C_MS), |registers| {
			count = ((registers.read32(STS) >> 8) & 0xFFFF) as usize;
			count > 0
		});
		if found { Ok(count) } else { Err(TransportError::NotReady) }
	}

	// Wait for the status register to be valid, and read it.
	fn valid_status(&mut self) -> Result<u32, TransportError> {
		let mut status = 0;
		if wait_for(&mut self.registers, TIMEOUT_C_MS, |registers| {
			status = registers.read32(STS);
			status & STS_VALID != 0
		}) {
			Ok(status)
		} else {
			Err(TransportError::NotReady)
		}
	}

	fn send(&mut self, command: &[u8]) -> Result<(), TransportError> {
		let (last, body) = command.split_last().ok_or(TransportError::Length)?;
		let mut at = 0;
		while at < body.len() {
			let count = self.burst()?.min(body.len() - at);
			for byte in &body[at..at + count] {
				self.registers.write8(DATA_FIFO, *byte);
			}
			at += count;
			// EVERY BYTE BUT THE LAST LEAVES THE TPM EXPECTING MORE; one that stopped expecting took less than
			// the command, and the rest would be read as the next one.
			if self.valid_status()? & STS_EXPECT == 0 {
				return Err(TransportError::Expect);
			}
		}
		self.burst()?;
		self.registers.write8(DATA_FIFO, *last);
		if self.valid_status()? & STS_EXPECT != 0 {
			return Err(TransportError::Expect);
		}
		Ok(())
	}

	fn read_exact(&mut self, count: usize, response: &mut Vec<u8>) -> Result<(), TransportError> {
		let mut left = count;
		while left > 0 {
			let take = self.burst()?.min(left);
			for _ in 0..take {
				response.push(self.registers.read8(DATA_FIFO));
			}
			left -= take;
		}
		Ok(())
	}

	fn receive(&mut self, response: &mut Vec<u8>) -> Result<(), TransportError> {
		self.read_exact(10, response)?;
		let size = u32::from_be_bytes([response[2], response[3], response[4], response[5]]) as usize;
		if !(10..=MAX_MESSAGE).contains(&size) {
			return Err(TransportError::Length);
		}
		self.read_exact(size - 10, response)?;
		// AND NOTHING AFTER IT: a TPM with more to say than its header declared is not answering this command.
		if self.valid_status()? & STS_DATA_AVAIL != 0 {
			return Err(TransportError::Length);
		}
		Ok(())
	}

	fn exchange(&mut self, command: &[u8], response: &mut Vec<u8>, duration_ms: u64) -> Result<(), TransportError> {
		self.registers.write32(STS, STS_COMMAND_READY);
		if !wait_for(&mut self.registers, TIMEOUT_B_MS, |registers| registers.read32(STS) & STS_COMMAND_READY != 0) {
			return Err(TransportError::NotReady);
		}
		self.send(command)?;
		self.registers.write32(STS, STS_GO);
		let done = |registers: &mut R| registers.read32(STS) & (STS_VALID | STS_DATA_AVAIL) == STS_VALID | STS_DATA_AVAIL;
		if !wait_for(&mut self.registers, duration_ms, done) {
			// OUT OF TIME: the command is cancelled, and whatever the TPM answers to the cancel is read and thrown
			// away so the interface is idle again.
			self.registers.write32(STS, STS_COMMAND_CANCEL);
			if wait_for(&mut self.registers, TIMEOUT_B_MS, done) {
				let mut discarded = Vec::new();
				let _ = self.receive(&mut discarded);
			}
			return Err(TransportError::TimedOut);
		}
		self.receive(response)
	}
}

impl<R: Registers> Transport for Fifo<R> {
	fn execute(&mut self, command: &[u8], response: &mut Vec<u8>, duration_ms: u64) -> Result<(), TransportError> {
		if command.len() < 10 || command.len() > MAX_MESSAGE {
			return Err(TransportError::Length);
		}
		self.request_locality()?;
		let result = self.exchange(command, response, duration_ms);
		self.registers.write32(STS, STS_COMMAND_READY);
		self.relinquish();
		result
	}
}
