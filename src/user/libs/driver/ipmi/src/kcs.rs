//! KCS - the Keyboard Controller Style interface (IPMI 2.0, 9): two registers, data (0) and status/command (1), and a
//! byte-at-a-time handshake on the status register's IBF and OBF flags, with the interface's own state in its top two
//! bits.
//!
//! THE WRITE PHASE: WRITE_START on the command register, every byte but the last on the data register, WRITE_END on
//! the command register, the last byte on the data register - each followed by a wait for IBF to clear, a check that
//! the interface is in its write state, and OBF cleared by a read of the data register. THE READ PHASE: while the
//! interface is in its read state, a byte is read once OBF is set and READ is written back for the next; in the idle
//! state the one dummy byte is read and the response is complete. ANYTHING ELSE - the error state, a state out of
//! place, the deadline - is ended by the ABORT sequence, bounded by `RECOVERY_MS`, and the transaction fails.

use crate::{Failure, MAX_RESPONSE, RECOVERY_MS, Registers, Request, Response, response, wait};
use alloc::vec::Vec;

pub const DATA: u8 = 0;
pub const STATUS: u8 = 1;
pub const COMMAND: u8 = 1;

pub const OBF: u8 = 1 << 0;
pub const IBF: u8 = 1 << 1;
pub const SMS_ATN: u8 = 1 << 2;

pub const GET_STATUS_ABORT: u8 = 0x60;
pub const WRITE_START: u8 = 0x61;
pub const WRITE_END: u8 = 0x62;
pub const READ: u8 = 0x68;

// What the write and read phases say when the interface enters its error state: the abort then reads the code.
const ERROR_STATE: &str = "the interface entered its error state";

/// The interface's state, the status register's top two bits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
	Idle,
	Read,
	Write,
	Error,
}

pub fn state(status: u8) -> State {
	match status >> 6 {
		0 => State::Idle,
		1 => State::Read,
		2 => State::Write,
		_ => State::Error,
	}
}

/// One transaction over a KCS interface: the request written, the response read and held against it. At the deadline,
/// or on the interface's error state, the ABORT sequence returns the interface to idle before the failure is reported.
pub fn transact<R: Registers + ?Sized>(registers: &mut R, request: &Request, deadline_ms: u64) -> Result<Response, Failure> {
	let bytes = request.bytes();
	if bytes.len() > crate::MAX_REQUEST {
		return Err(Failure::RequestTooLong);
	}
	let outcome = exchange(registers, &bytes, deadline_ms);
	match outcome {
		Ok(read) => response(request, &read),
		Err(failure) => {
			let recovered = abort(registers);
			match (failure, recovered) {
				// THE ERROR STATE'S OWN CODE, when the abort read one.
				(Failure::Protocol(why), Ok(code)) if why == ERROR_STATE => Err(Failure::Interface(code)),
				_ => Err(failure),
			}
		}
	}
}

// The write phase and the read phase; the bytes read.
fn exchange<R: Registers + ?Sized>(registers: &mut R, bytes: &[u8], deadline_ms: u64) -> Result<Vec<u8>, Failure> {
	let Some((last, first)) = bytes.split_last() else { return Err(Failure::RequestTooLong) };
	ibf_clear(registers, deadline_ms)?;
	clear_obf(registers);
	registers.write(COMMAND, WRITE_START);
	expect(registers, deadline_ms, State::Write)?;
	for &byte in first {
		registers.write(DATA, byte);
		expect(registers, deadline_ms, State::Write)?;
	}
	registers.write(COMMAND, WRITE_END);
	expect(registers, deadline_ms, State::Write)?;
	registers.write(DATA, *last);
	// THE READ PHASE. IBF stays set while the BMC works on the request, so this first wait is the one the BMC's own
	// time is spent in.
	let mut read = Vec::new();
	loop {
		let status = ibf_clear(registers, deadline_ms)?;
		match state(status) {
			State::Read => {
				obf_set(registers, deadline_ms)?;
				let byte = registers.read(DATA);
				if read.len() == MAX_RESPONSE {
					// PAST THE BOUND: the interface is left mid-response, which the abort in `transact` ends.
					return Err(Failure::TooLong);
				}
				read.push(byte);
				registers.write(DATA, READ);
			}
			State::Idle => {
				obf_set(registers, deadline_ms)?;
				let _ = registers.read(DATA);
				return Ok(read);
			}
			State::Error => return Err(Failure::Protocol(ERROR_STATE)),
			State::Write => return Err(Failure::Protocol("the interface stayed in its write state after the last byte")),
		}
	}
}

// Wait for IBF to clear, then check the interface is in `wanted` and clear OBF.
fn expect<R: Registers + ?Sized>(registers: &mut R, deadline_ms: u64, wanted: State) -> Result<(), Failure> {
	let status = ibf_clear(registers, deadline_ms)?;
	match state(status) {
		found if found == wanted => {
			clear_obf(registers);
			Ok(())
		}
		State::Error => Err(Failure::Protocol(ERROR_STATE)),
		_ => Err(Failure::Protocol("the interface is not in the state the write phase requires")),
	}
}

fn ibf_clear<R: Registers + ?Sized>(registers: &mut R, deadline_ms: u64) -> Result<u8, Failure> {
	wait(registers, STATUS, deadline_ms, |status| status & IBF == 0).ok_or(Failure::Deadline)
}

fn obf_set<R: Registers + ?Sized>(registers: &mut R, deadline_ms: u64) -> Result<u8, Failure> {
	wait(registers, STATUS, deadline_ms, |status| status & OBF != 0).ok_or(Failure::Deadline)
}

fn clear_obf<R: Registers + ?Sized>(registers: &mut R) {
	if registers.read(STATUS) & OBF != 0 {
		let _ = registers.read(DATA);
	}
}

/// How many times the abort sequence is tried within its bound.
pub const ABORT_TRIES: u32 = 3;

/// THE ABORT SEQUENCE (IPMI 2.0, 9.15, error recovery): GET_STATUS/ABORT on the command register, a zero on the data
/// register, the status code read in the read state, READ written back, and the dummy byte read in the idle state -
/// tried `ABORT_TRIES` times within `RECOVERY_MS`. The status code the interface reported, or `Deadline` when it
/// could not be brought back.
pub fn abort<R: Registers + ?Sized>(registers: &mut R) -> Result<u8, Failure> {
	let deadline_ms = registers.now_ms() + RECOVERY_MS;
	for _ in 0..ABORT_TRIES {
		if let Ok(code) = abort_once(registers, deadline_ms) {
			return Ok(code);
		}
		if registers.now_ms() >= deadline_ms {
			break;
		}
	}
	Err(Failure::Deadline)
}

fn abort_once<R: Registers + ?Sized>(registers: &mut R, deadline_ms: u64) -> Result<u8, Failure> {
	ibf_clear(registers, deadline_ms)?;
	registers.write(COMMAND, GET_STATUS_ABORT);
	ibf_clear(registers, deadline_ms)?;
	clear_obf(registers);
	registers.write(DATA, 0);
	let status = ibf_clear(registers, deadline_ms)?;
	if state(status) != State::Read {
		return Err(Failure::Protocol("the abort did not reach the read state"));
	}
	obf_set(registers, deadline_ms)?;
	let code = registers.read(DATA);
	registers.write(DATA, READ);
	let status = ibf_clear(registers, deadline_ms)?;
	if state(status) != State::Idle {
		return Err(Failure::Protocol("the abort did not return the interface to idle"));
	}
	obf_set(registers, deadline_ms)?;
	let _ = registers.read(DATA);
	Ok(code)
}

/// Whether the interface is idle with nothing pending - what a binding checks before its first transaction, and after
/// a resume, to know whether to abort.
pub fn idle<R: Registers + ?Sized>(registers: &mut R) -> bool {
	let status = registers.read(STATUS);
	state(status) == State::Idle && status & (IBF | OBF) == 0
}
