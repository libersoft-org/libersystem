//! UCSI - the USB Type-C Connector System Software Interface - as the operating system's policy manager (the OPM)
//! speaks it to a platform policy manager (the PPM) through a shared mailbox.
//!
//! THE MAILBOX. VERSION is read first and fixes the layout: before 2.0, CCI at 4, CONTROL at 8, a 16-byte MESSAGE_IN
//! at 16 and MESSAGE_OUT at 32; from 2.0, a 256-byte MESSAGE_IN at 16 and MESSAGE_OUT at 272. A range shorter than the
//! layout its version needs, or a major version outside 1 to 3, is refused.
//!
//! THE DISCIPLINE (`execute`, `reset`). One command outstanding at a time. After a completion, CCI's length is checked
//! against MESSAGE_IN's size and against the command's own answer size, and a connector number against the count the
//! capabilities gave, before a byte of the answer is believed. Every completion is acknowledged with `ACK_CC_CI`, and
//! the acknowledgement's completion waited for, before the next command; a connector change is acknowledged in the same
//! `ACK_CC_CI` as the completion of the `GET_CONNECTOR_STATUS` that reads it, never alone. BUSY is waited on; ERROR is
//! the caller's to follow with `GET_ERROR_STATUS`; NOT SUPPORTED is a refusal. Every bound is TEN SECONDS: shipping
//! laptops take eight to ten for the first reset and more than five for the commands after it.
//!
//! WHAT REFRESHES THE MAILBOX. The platform refreshes CCI and MESSAGE_IN before its notification, so after one they are
//! read as they are; `Ppm::refresh` (the `_DSM`'s function 2) is called ONLY where no notification has refreshed them -
//! the VERSION read at bind and the reset's polled completion, while notifications are off - since firmware may spoil
//! its own copy once it has notified.
//!
//! Nothing here reaches the runtime: the caller supplies the PPM as `Ppm`, and a host test supplies a scripted one.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod answer;
pub mod command;

use alloc::vec::Vec;

/// The bound on a command, its acknowledgement and a reset's completion.
pub const COMMAND_MS: u64 = 10_000;
/// Between two reads of a polled reset's completion.
pub const POLL_MS: u64 = 10;

// ------------------------------------------------------------------ the mailbox

pub const VERSION: usize = 0;
pub const CCI: usize = 4;
pub const CONTROL: usize = 8;
pub const MESSAGE_IN: usize = 16;

/// Where a mailbox's fields are, as its VERSION fixes them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
	/// Binary-coded: 0x0210 is 2.1.
	pub version: u16,
	pub message_in: usize,
	pub message_out: usize,
	/// MESSAGE_IN's and MESSAGE_OUT's size.
	pub message_size: usize,
}

/// Why a mailbox is not bound.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutRefusal {
	/// A major version outside 1 to 3.
	Version(u16),
	/// The range is shorter than the layout its version needs.
	Short { needed: usize, have: usize },
}

impl Layout {
	pub fn of(version: u16, range: usize) -> Result<Layout, LayoutRefusal> {
		let major = version >> 8;
		if !(1..=3).contains(&major) {
			return Err(LayoutRefusal::Version(version));
		}
		let layout = if major == 1 { Layout { version, message_in: MESSAGE_IN, message_out: 32, message_size: 16 } } else { Layout { version, message_in: MESSAGE_IN, message_out: 272, message_size: 256 } };
		let needed = layout.message_out + layout.message_size;
		if range < needed {
			return Err(LayoutRefusal::Short { needed, have: range });
		}
		Ok(layout)
	}

	pub fn major(&self) -> u16 {
		self.version >> 8
	}

	/// Whether the version is at least `major`.`minor`.
	pub fn at_least(&self, major: u16, minor: u16) -> bool {
		self.version >= (major << 8) | (minor << 4)
	}
}

// ------------------------------------------------------------------ CCI

/// The Command Status and Connector Change Indication.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Cci(pub u32);

impl Cci {
	pub const NOT_SUPPORTED: u32 = 1 << 25;
	pub const CANCEL_COMPLETE: u32 = 1 << 26;
	pub const RESET_COMPLETE: u32 = 1 << 27;
	pub const BUSY: u32 = 1 << 28;
	pub const ACK_COMPLETE: u32 = 1 << 29;
	pub const ERROR: u32 = 1 << 30;
	pub const COMMAND_COMPLETE: u32 = 1 << 31;

	/// The connector whose state changed, if any.
	pub fn connector(self) -> Option<u8> {
		let number = ((self.0 >> 1) & 0x7F) as u8;
		(number != 0).then_some(number)
	}

	/// The bytes of MESSAGE_IN the answer fills.
	pub fn length(self) -> usize {
		((self.0 >> 8) & 0xFF) as usize
	}

	pub fn has(self, bit: u32) -> bool {
		self.0 & bit != 0
	}
}

// ------------------------------------------------------------------ the policy manager

/// Why an exchange with the PPM failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Failure {
	/// Nothing within the bound: the PPM is silent.
	Silent,
	/// An answer's length is past MESSAGE_IN, or past what the command answers.
	Length { cci: usize, limit: usize },
	/// An answer names a connector outside the count the capabilities gave.
	Connector(u8),
	/// A reserved value, or a completion that is no answer to what was sent.
	Malformed,
	/// The `_DSM` or the mailbox could not be reached.
	Transport,
}

/// The PPM, as the caller reaches it.
pub trait Ppm {
	/// Write CONTROL - and MESSAGE_OUT first, when `message_out` is not empty - and evaluate the `_DSM`'s function 1,
	/// so the platform takes them.
	fn send(&mut self, control: u64, message_out: &[u8]) -> Result<(), Failure>;
	/// Wait for the PPM's notification until `deadline_ms`: false at the deadline.
	fn wait_notify(&mut self, deadline_ms: u64) -> Result<bool, Failure>;
	/// Evaluate the `_DSM`'s function 2, which has the platform refresh CCI and MESSAGE_IN. ONLY where no notification
	/// refreshed them.
	fn refresh(&mut self) -> Result<(), Failure>;
	fn cci(&mut self) -> Cci;
	/// The first `out.len()` bytes of MESSAGE_IN.
	fn message_in(&mut self, out: &mut [u8]);
	fn now_ms(&mut self) -> u64;
	/// Sleep a little between two polls of a reset.
	fn pause(&mut self);
}

/// How a command completed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Completion {
	/// Its answer, `length` bytes as CCI gave them.
	Answered(Vec<u8>),
	/// The PPM does not support the command.
	NotSupported,
	/// The PPM reported an error: the caller asks `GET_ERROR_STATUS` for it.
	Error,
}

/// A completed exchange: how the command completed, and any connector change the PPM indicated while it ran.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Exchange {
	pub completion: Completion,
	pub change: Option<u8>,
}

// Wait for a notification whose CCI satisfies `done`, noting every connector change seen on the way; a BUSY PPM is
// waited on inside the same bound.
fn await_cci(ppm: &mut dyn Ppm, deadline: u64, change: &mut Option<u8>, done: impl Fn(Cci) -> bool) -> Result<Cci, Failure> {
	loop {
		if !ppm.wait_notify(deadline)? {
			return Err(Failure::Silent);
		}
		let cci = ppm.cci();
		if let Some(number) = cci.connector() {
			*change = Some(number);
		}
		if done(cci) {
			return Ok(cci);
		}
	}
}

/// ONE COMMAND, UNDER THE DISCIPLINE: sent, its completion awaited and checked, its answer read, and the completion
/// acknowledged - with the connector change in the same acknowledgement when `change_ack`, which only a
/// `GET_CONNECTOR_STATUS` reading that change asks for.
pub fn execute(ppm: &mut dyn Ppm, layout: &Layout, command: command::Command, message_out: &[u8], connectors: u8, change_ack: bool) -> Result<Exchange, Failure> {
	let mut change = None;
	ppm.send(command.control, message_out)?;
	let deadline = ppm.now_ms() + COMMAND_MS;
	let cci = await_cci(ppm, deadline, &mut change, |cci| cci.has(Cci::COMMAND_COMPLETE) && !cci.has(Cci::BUSY))?;
	if cci.has(Cci::ACK_COMPLETE) || cci.has(Cci::RESET_COMPLETE) {
		// A completion that also says it acknowledged or reset is no answer to a command.
		return Err(Failure::Malformed);
	}
	if let Some(number) = cci.connector()
		&& number > connectors
		&& connectors != 0
	{
		return Err(Failure::Connector(number));
	}
	let completion = if cci.has(Cci::NOT_SUPPORTED) {
		Completion::NotSupported
	} else if cci.has(Cci::ERROR) {
		Completion::Error
	} else {
		let length = cci.length();
		if length > layout.message_size {
			return Err(Failure::Length { cci: length, limit: layout.message_size });
		}
		if length > command.answer {
			return Err(Failure::Length { cci: length, limit: command.answer });
		}
		let mut data = alloc::vec![0u8; length];
		ppm.message_in(&mut data);
		Completion::Answered(data)
	};
	// THE ACKNOWLEDGEMENT, and its own completion, before anything else is sent.
	ppm.send(command::ack(true, change_ack).control, &[])?;
	let deadline = ppm.now_ms() + COMMAND_MS;
	let mut ignored = None;
	await_cci(ppm, deadline, &mut ignored, |cci| cci.has(Cci::ACK_COMPLETE))?;
	if let Some(number) = ignored
		&& change.is_none()
	{
		change = Some(number);
	}
	Ok(Exchange { completion, change })
}

/// PPM_RESET, its completion POLLED through the `_DSM`'s function 2 - a reset leaves notifications off, so nothing else
/// refreshes CCI - for at most the bound.
pub fn reset(ppm: &mut dyn Ppm) -> Result<(), Failure> {
	ppm.send(command::ppm_reset().control, &[])?;
	let deadline = ppm.now_ms() + COMMAND_MS;
	loop {
		ppm.refresh()?;
		if ppm.cci().has(Cci::RESET_COMPLETE) {
			return Ok(());
		}
		if ppm.now_ms() >= deadline {
			return Err(Failure::Silent);
		}
		ppm.pause();
	}
}

#[cfg(test)]
mod tests;
