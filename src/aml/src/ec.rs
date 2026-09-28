//! THE EMBEDDED CONTROLLER TRANSPORT: the ACPI EC's command set over its two ports - read, write, burst enable and
//! disable, and the query - every wait on IBF and OBF BOUNDED, so a controller that stops answering costs one
//! failed access and never the service. The handler of `EmbeddedControl` regions, and on `SCI_EVT` the source of
//! `_Qxx`.
//!
//! QEMU emulates no EC, so this is held to a register model: a stuck input buffer, a flood of queries, a controller
//! that vanishes (its status reading all ones).

use alloc::vec::Vec;

/// Status register bits.
pub mod status {
	pub const OBF: u8 = 1 << 0;
	pub const IBF: u8 = 1 << 1;
	pub const CMD: u8 = 1 << 3;
	pub const BURST: u8 = 1 << 4;
	pub const SCI_EVT: u8 = 1 << 5;
	pub const SMI_EVT: u8 = 1 << 6;
}

/// Commands, written to the command port.
pub mod command {
	pub const READ: u8 = 0x80;
	pub const WRITE: u8 = 0x81;
	pub const BURST_ENABLE: u8 = 0x82;
	pub const BURST_DISABLE: u8 = 0x83;
	pub const QUERY: u8 = 0x84;
	/// What the controller answers a burst enable with.
	pub const BURST_ACK: u8 = 0x90;
}

/// The two ports and a clock.
pub trait Ports {
	fn status(&mut self) -> u8;
	fn data(&mut self) -> u8;
	fn command(&mut self, value: u8);
	fn write_data(&mut self, value: u8);
	/// A monotonic microsecond clock.
	fn now_us(&mut self) -> u64;
	/// Give the controller a moment between two polls.
	fn pause(&mut self);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
	/// Waiting for the controller to take a byte (IBF clear).
	InputBuffer,
	/// Waiting for the controller's answer (OBF set).
	OutputBuffer,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EcError {
	/// A wait ran past its bound: which one, and in what command.
	Timeout { stage: Stage, command: u8 },
	/// The status reads all ones: nothing is there.
	Vanished,
	/// A burst enable answered something other than the acknowledgement.
	NoBurst(u8),
}

/// How long one wait may take, in microseconds - the specification's allowance for a slow controller.
pub const WAIT_US: u64 = 50_000;

/// The most queries one `SCI_EVT` is drained with: past it the flood is reported and the rest left for the next.
pub const QUERIES_PER_EVENT: usize = 32;

pub struct Ec<P: Ports> {
	pub ports: P,
	pub wait_us: u64,
	burst: bool,
}

impl<P: Ports> Ec<P> {
	pub fn new(ports: P) -> Ec<P> {
		Ec { ports, wait_us: WAIT_US, burst: false }
	}

	fn wait(&mut self, stage: Stage, command: u8) -> Result<(), EcError> {
		let deadline = self.ports.now_us() + self.wait_us;
		loop {
			let status = self.ports.status();
			if status == 0xFF {
				return Err(EcError::Vanished);
			}
			let ready = match stage {
				Stage::InputBuffer => status & status::IBF == 0,
				Stage::OutputBuffer => status & status::OBF != 0,
			};
			if ready {
				return Ok(());
			}
			if self.ports.now_us() >= deadline {
				return Err(EcError::Timeout { stage, command });
			}
			self.ports.pause();
		}
	}

	/// Read one byte of the controller's address space.
	pub fn read(&mut self, address: u8) -> Result<u8, EcError> {
		self.wait(Stage::InputBuffer, command::READ)?;
		self.ports.command(command::READ);
		self.wait(Stage::InputBuffer, command::READ)?;
		self.ports.write_data(address);
		self.wait(Stage::OutputBuffer, command::READ)?;
		Ok(self.ports.data())
	}

	/// Write one byte of the controller's address space.
	pub fn write(&mut self, address: u8, value: u8) -> Result<(), EcError> {
		self.wait(Stage::InputBuffer, command::WRITE)?;
		self.ports.command(command::WRITE);
		self.wait(Stage::InputBuffer, command::WRITE)?;
		self.ports.write_data(address);
		self.wait(Stage::InputBuffer, command::WRITE)?;
		self.ports.write_data(value);
		self.wait(Stage::InputBuffer, command::WRITE)
	}

	/// BURST MODE for a run of accesses: enabled and acknowledged, then given back.
	pub fn burst_enable(&mut self) -> Result<(), EcError> {
		self.wait(Stage::InputBuffer, command::BURST_ENABLE)?;
		self.ports.command(command::BURST_ENABLE);
		self.wait(Stage::OutputBuffer, command::BURST_ENABLE)?;
		let ack = self.ports.data();
		if ack != command::BURST_ACK {
			return Err(EcError::NoBurst(ack));
		}
		self.burst = true;
		Ok(())
	}

	pub fn burst_disable(&mut self) -> Result<(), EcError> {
		self.wait(Stage::InputBuffer, command::BURST_DISABLE)?;
		self.ports.command(command::BURST_DISABLE);
		self.burst = false;
		self.wait(Stage::InputBuffer, command::BURST_DISABLE)
	}

	pub fn in_burst(&self) -> bool {
		self.burst
	}

	/// Whether the controller has an event to be queried.
	pub fn event_pending(&mut self) -> bool {
		let status = self.ports.status();
		status != 0xFF && status & status::SCI_EVT != 0
	}

	/// QUERY: the next event's number, `None` when the controller says there is none (0).
	pub fn query(&mut self) -> Result<Option<u8>, EcError> {
		self.wait(Stage::InputBuffer, command::QUERY)?;
		self.ports.command(command::QUERY);
		self.wait(Stage::OutputBuffer, command::QUERY)?;
		let value = self.ports.data();
		Ok((value != 0).then_some(value))
	}

	/// DRAIN THE EVENTS one `SCI_EVT` announces: query until the controller answers none, at most
	/// `QUERIES_PER_EVENT` - answering the event numbers, for `_Qxx`, and whether the bound cut the drain short.
	pub fn drain(&mut self) -> Result<(Vec<u8>, bool), EcError> {
		let mut events = Vec::new();
		for _ in 0..QUERIES_PER_EVENT {
			match self.query()? {
				Some(event) => events.push(event),
				None => return Ok((events, false)),
			}
		}
		Ok((events, self.event_pending()))
	}
}

/// `_Qxx`'s name for an event number: `_Q0A`.
pub fn query_method(event: u8) -> [u8; 4] {
	let hex = b"0123456789ABCDEF";
	[b'_', b'Q', hex[(event >> 4) as usize], hex[(event & 0x0F) as usize]]
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::VecDeque;
	use std::vec;
	use std::vec::Vec;

	/// A CONTROLLER MODEL: a 256-byte space, a command state machine, queued events, and faults to switch on.
	#[derive(Default)]
	struct Model {
		space: Vec<u8>,
		state: State,
		output: Option<u8>,
		events: VecDeque<u8>,
		clock: u64,
		stuck_ibf: bool,
		vanished: bool,
		flood: bool,
		commands: Vec<u8>,
	}

	#[derive(Default, Clone, Copy, PartialEq)]
	enum State {
		#[default]
		Idle,
		ReadAddress,
		WriteAddress,
		WriteData(u8),
	}

	impl Model {
		fn new() -> Model {
			Model { space: vec![0; 256], ..Default::default() }
		}
	}

	impl Ports for &mut Model {
		fn status(&mut self) -> u8 {
			if self.vanished {
				return 0xFF;
			}
			let mut status = 0;
			if self.output.is_some() {
				status |= status::OBF;
			}
			if self.stuck_ibf {
				status |= status::IBF;
			}
			if !self.events.is_empty() || self.flood {
				status |= status::SCI_EVT;
			}
			status
		}
		fn data(&mut self) -> u8 {
			self.output.take().unwrap_or(0)
		}
		fn command(&mut self, value: u8) {
			self.commands.push(value);
			match value {
				command::READ => self.state = State::ReadAddress,
				command::WRITE => self.state = State::WriteAddress,
				command::BURST_ENABLE => self.output = Some(command::BURST_ACK),
				command::BURST_DISABLE => {}
				command::QUERY => self.output = Some(if self.flood { 0x42 } else { self.events.pop_front().unwrap_or(0) }),
				_ => {}
			}
		}
		fn write_data(&mut self, value: u8) {
			match self.state {
				State::ReadAddress => {
					self.output = Some(self.space[value as usize]);
					self.state = State::Idle;
				}
				State::WriteAddress => self.state = State::WriteData(value),
				State::WriteData(address) => {
					self.space[address as usize] = value;
					self.state = State::Idle;
				}
				State::Idle => {}
			}
		}
		fn now_us(&mut self) -> u64 {
			self.clock
		}
		fn pause(&mut self) {
			self.clock += 100;
		}
	}

	#[test]
	fn reads_writes_and_burst_follow_the_command_protocol() {
		let mut model = Model::new();
		model.space[0x30] = 0x5A;
		let mut ec = Ec::new(&mut model);
		assert_eq!(ec.read(0x30), Ok(0x5A));
		ec.write(0x31, 0xA5).unwrap();
		ec.burst_enable().unwrap();
		assert!(ec.in_burst());
		assert_eq!(ec.read(0x31), Ok(0xA5));
		ec.burst_disable().unwrap();
		assert!(!ec.in_burst());
		assert_eq!(model.commands, vec![command::READ, command::WRITE, command::BURST_ENABLE, command::READ, command::BURST_DISABLE]);
	}

	#[test]
	fn a_stuck_input_buffer_times_out_in_bounded_time() {
		let mut model = Model::new();
		model.stuck_ibf = true;
		let mut ec = Ec::new(&mut model);
		assert_eq!(ec.read(0x10), Err(EcError::Timeout { stage: Stage::InputBuffer, command: command::READ }));
		assert!(model.clock >= WAIT_US && model.clock <= WAIT_US + 200, "the wait ended at its bound ({} us)", model.clock);
	}

	#[test]
	fn events_are_drained_and_a_flood_is_cut_at_its_bound() {
		let mut model = Model::new();
		model.events.extend([0x0A, 0x1B]);
		let mut ec = Ec::new(&mut model);
		assert!(ec.event_pending());
		assert_eq!(ec.drain(), Ok((vec![0x0A, 0x1B], false)));
		assert!(!ec.event_pending());
		let mut flooding = Model::new();
		flooding.flood = true;
		let mut ec = Ec::new(&mut flooding);
		let (events, cut) = ec.drain().unwrap();
		assert_eq!(events.len(), QUERIES_PER_EVENT);
		assert!(cut, "the flood is reported, not drained for ever");
		assert_eq!(query_method(0x0A), *b"_Q0A");
	}

	#[test]
	fn a_vanished_controller_is_named_at_once() {
		let mut model = Model::new();
		model.vanished = true;
		let mut ec = Ec::new(&mut model);
		assert_eq!(ec.read(0), Err(EcError::Vanished));
		assert!(!ec.event_pending());
		assert_eq!(model.clock, 0, "no wait for a controller that is not there");
	}
}
