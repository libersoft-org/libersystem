// THE KERNEL CONSOLE'S UART IN A REGISTER WINDOW, as its driver serves it - over either register engine.
//
// The device-tree ports' consoles are a PL011 (aarch64) and a 16550 in a register window (riscv64), and the kernel
// hands each to a driver exactly as x86_64 hands COM1 to `uart16550`: the claim of the console's platform row takes
// the UART, its tap carries the kernel's output out of the kernel's ring, and the release gives it back. What the
// driver does with it is the same whichever UART it is - the tap emptied onto the wire before every consumer write,
// typed input held for the consumer and backpressured past the bound, the transmitter's interrupt waited on when
// its FIFO is full, the UART programmed again at every resume - so it is written here once, over an `Engine`, and
// the two engines (`uart`, `pl011`) keep their own registers. `uart16550` keeps its own copy of these rules for
// COM1, which they were taken from.
//
// ONE THING DIFFERS WITH THE LINE AND NOT THE UART: the order a fired line is answered in. COM1's IRQ 4 is an EDGE,
// acknowledged before the receiver is read so a byte landing during the read raises a new edge. These ports' lines
// are LEVEL - a GIC SPI, an APLIC source - which the kernel masks when they fire and unmasks at the acknowledgement:
// acknowledged first, a level still held up by the bytes about to be read fires again at once, for nothing. So a
// level line is drained, and acknowledged after.
//
// And what a platform description says about the UART - its reference clock, and a 16550's register spacing and
// width - read from the row's property block (`describe`), host-tested with the rest.

use crate::common;
use crate::pl011;
use crate::serial_port::{self, Session};
use crate::uart::{self, Held};
use proto::system::Error;
use rt::*;

#[cfg(test)]
mod tests;

// How long a write waits for the transmitter before it answers `again`, in scheduler ticks: the session's own bound.
const TX_DRAIN_TICKS: u64 = serial_port::TX_DRAIN_TICKS;
// The most the tap hands over in one read, and what one receive pass takes.
const TAP_CHUNK: usize = 1024;
const RX_CHUNK: usize = 64;

// THE RATE THE CONSOLE IS PROGRAMMED AT when the description gives the clock to divide: what the firmware on both
// ports leaves its console at, and what the far end of a serial console expects.
pub const CONSOLE_BAUD: u32 = 115_200;

// WHAT A CONSOLE NEEDS OF A UART, whichever registers it has.
pub trait Engine {
	// The baud generator's setting, as the engine computes it from a clock and a rate.
	type Line: Copy;
	// The setting for `baud` from a `clock_hz` reference clock, or `None` when no setting comes close enough.
	fn line(clock_hz: u64, baud: u32) -> Option<Self::Line>;
	// The whole UART programmed, the divisor kept when no line is given; the receive interrupt last, when asked.
	fn program(&mut self, line: Option<Self::Line>, receive_interrupt: bool);
	fn quiet(&mut self);
	fn transmit_interrupt(&mut self, on: bool);
	fn receive_interrupt(&mut self, on: bool);
	fn put(&mut self, bytes: &[u8]) -> usize;
	fn receive(&mut self, out: &mut [u8]) -> usize;
	// What the line's interrupt leaves to clear at the UART itself.
	fn acknowledge(&mut self);
	fn errors(&self) -> u64;
}

impl<R: uart::Registers> Engine for uart::Uart<R> {
	type Line = uart::Line;

	fn line(clock_hz: u64, baud: u32) -> Option<uart::Line> {
		uart::Line::new(u32::try_from(clock_hz).ok()?, baud)
	}

	fn program(&mut self, line: Option<uart::Line>, receive_interrupt: bool) {
		self.program_line(line, receive_interrupt);
	}

	fn quiet(&mut self) {
		uart::Uart::quiet(self);
	}

	fn transmit_interrupt(&mut self, on: bool) {
		uart::Uart::transmit_interrupt(self, on);
	}

	fn receive_interrupt(&mut self, on: bool) {
		uart::Uart::receive_interrupt(self, on);
	}

	fn put(&mut self, bytes: &[u8]) -> usize {
		uart::Uart::put(self, bytes)
	}

	fn receive(&mut self, out: &mut [u8]) -> usize {
		uart::Uart::receive(self, out)
	}

	// Reading the interrupt identification clears a transmit-empty interrupt the UART raised.
	fn acknowledge(&mut self) {
		let _ = uart::Uart::acknowledge(self);
	}

	fn errors(&self) -> u64 {
		uart::Uart::errors(self)
	}
}

impl<R: pl011::Registers> Engine for pl011::Pl011<R> {
	type Line = pl011::Line;

	fn line(clock_hz: u64, baud: u32) -> Option<pl011::Line> {
		pl011::Line::new(clock_hz, baud)
	}

	fn program(&mut self, line: Option<pl011::Line>, receive_interrupt: bool) {
		pl011::Pl011::program(self, line, receive_interrupt);
	}

	fn quiet(&mut self) {
		pl011::Pl011::quiet(self);
	}

	fn transmit_interrupt(&mut self, on: bool) {
		pl011::Pl011::transmit_interrupt(self, on);
	}

	fn receive_interrupt(&mut self, on: bool) {
		pl011::Pl011::receive_interrupt(self, on);
	}

	fn put(&mut self, bytes: &[u8]) -> usize {
		pl011::Pl011::put(self, bytes)
	}

	fn receive(&mut self, out: &mut [u8]) -> usize {
		pl011::Pl011::receive(self, out)
	}

	// Clearing the transmit interrupt, which stays raised until it is cleared.
	fn acknowledge(&mut self) {
		let _ = pl011::Pl011::acknowledge(self);
	}

	fn errors(&self) -> u64 {
		pl011::Pl011::errors(self)
	}
}

// ------------------------------------------------------------------ the description

// WHAT THE PLATFORM SAYS ABOUT THE UART, from the row's property block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Described {
	// The reference clock the baud generator divides, when the description resolves one: a `fixed-clock` the kernel
	// resolved for `clocks` - the one `clock-names` calls `uartclk` or `baudclk`, the first otherwise - or a 16550's
	// `clock-frequency`.
	pub clock_hz: Option<u64>,
	// A 16550's register spacing - registers `1 << reg_shift` bytes apart - and the width of each access in bytes.
	pub reg_shift: u32,
	pub reg_io_width: u32,
}

impl Default for Described {
	fn default() -> Self {
		Described { clock_hz: None, reg_shift: 0, reg_io_width: 1 }
	}
}

// The most `clocks` entries a description names that this reads.
const MOST_CLOCKS: usize = 8;

// One big-endian cell, or two - a property's value as a tree carries a number.
fn cells(value: &[u8]) -> Option<u64> {
	match value.len() {
		4 => Some(u64::from(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))),
		8 => Some(u64::from_be_bytes([value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7]])),
		_ => None,
	}
}

// THE DESCRIPTION, from a device tree's property block (`abi::DEVICE_PROPERTY_*` records): the device's own records
// only - depth 0 - since a UART's children describe nothing of its registers. An empty block describes nothing, and
// the UART keeps what the firmware left. `Err` is a description this driver cannot honour: registers spaced wider than
// a 16550's ever are, or accessed at a width it cannot issue.
pub fn describe(block: &[u8]) -> Result<Described, &'static str> {
	let mut described = Described::default();
	let mut clocks = [None; MOST_CLOCKS];
	let mut clock_count = 0usize;
	let mut names: &[u8] = &[];
	let mut frequency: Option<u64> = None;
	let mut at = 0usize;
	while at + 8 <= block.len() {
		let (kind, depth) = (block[at], block[at + 1]);
		let name_len = usize::from(u16::from_le_bytes([block[at + 2], block[at + 3]]));
		let value_len = u32::from_le_bytes([block[at + 4], block[at + 5], block[at + 6], block[at + 7]]) as usize;
		let name_at = at + 8;
		let value_at = name_at + name_len;
		let Some(end) = value_at.checked_add(value_len).filter(|end| *end <= block.len()) else { break };
		let (name, value) = (&block[name_at..value_at], &block[value_at..end]);
		at = value_at + ((value_len + 3) & !3);
		if depth != 0 {
			continue;
		}
		match (kind, name) {
			(DEVICE_PROPERTY_CLOCK, b"clocks") | (DEVICE_PROPERTY_UNRESOLVED, b"clocks") => {
				if clock_count < MOST_CLOCKS {
					clocks[clock_count] = if kind == DEVICE_PROPERTY_CLOCK && value.len() == 8 { Some(u64::from_le_bytes([value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7]])) } else { None };
					clock_count += 1;
				}
			}
			(DEVICE_PROPERTY_VALUE, b"clock-names") => names = value,
			(DEVICE_PROPERTY_VALUE, b"clock-frequency") => frequency = cells(value),
			(DEVICE_PROPERTY_VALUE, b"reg-shift") => {
				described.reg_shift = cells(value).filter(|shift| *shift <= 3).ok_or("its registers are spaced wider than a 16550's are")? as u32;
			}
			(DEVICE_PROPERTY_VALUE, b"reg-io-width") => {
				described.reg_io_width = cells(value).filter(|width| matches!(width, 1 | 2 | 4)).ok_or("its registers take an access width this driver cannot issue")? as u32;
			}
			_ => {}
		}
	}
	// THE UART'S OWN CLOCK, by name where the description names its clocks: a PL011's second clock is its bus clock,
	// which divides nothing on the line.
	let named = names.split(|byte| *byte == 0).position(|name| name == b"uartclk" || name == b"baudclk");
	let clock = if clock_count == 0 { None } else { clocks[named.unwrap_or(0).min(clock_count - 1)] };
	described.clock_hz = clock.or(frequency).filter(|hz| *hz != 0);
	Ok(described)
}

// ------------------------------------------------------------------ the console

pub struct Console<E: Engine> {
	pub uart: E,
	// The baud generator's setting from the described clock - `None` keeps the firmware's.
	pub line: Option<E::Line>,
	pub irq: u64,
	// Whether the line is level-triggered: the order a fired line is answered in (see the module's note).
	pub level: bool,
	pub tap: u64,
	// Received bytes nobody has taken yet: while no consumer is attached, while a write is on the wire, and while the
	// consumer's stream is full.
	pub held: Held,
	// Kernel output the tap reported dropped, in total.
	pub dropped: u64,
}

impl<E: Engine> Console<E> {
	// The whole UART programmed, with the receive interrupt: at the bind and at every resume.
	pub fn program(&mut self) {
		self.uart.program(self.line, true);
	}

	// PUT BYTES ON THE WIRE: as much as the transmitter takes whenever it has room, and the transmitter's interrupt
	// waited for when it has none - answering the manager's pings meanwhile, and keeping any byte received while
	// waiting. `Again` when the transmitter did not take it within the bound.
	pub fn transmit(&mut self, mut bytes: &[u8], bind: &common::Bind, bootstrap: u64) -> Result<(), Error> {
		let limit = clock() + TX_DRAIN_TICKS;
		while !bytes.is_empty() {
			let n = self.uart.put(bytes);
			if n != 0 {
				bytes = &bytes[n..];
				continue;
			}
			if !common::answer_ping(bootstrap, bind) {
				return Err(Error::Closed);
			}
			if clock() >= limit {
				return Err(Error::Again);
			}
			self.uart.transmit_interrupt(true);
			// Looked at again with the interrupt on: a transmitter that drained in between raised it already, and what
			// it takes now need not wait for the line.
			let n = self.uart.put(bytes);
			if n != 0 {
				bytes = &bytes[n..];
			} else if wait_any(&[self.irq, bootstrap], limit) == 0 {
				self.service_line();
			}
			self.uart.transmit_interrupt(false);
		}
		Ok(())
	}

	// THE LINE FIRED: the UART's own interrupt state cleared, every received byte read and kept for the consumer, and
	// the line acknowledged - in the order the line's trigger needs (see the module's note). An EDGE line is
	// acknowledged first, so a byte that lands during the read raises a new edge; a LEVEL line last, once what holds it
	// up has been read, so it fires again only for a byte that arrived after.
	pub fn service_line(&mut self) {
		if self.level {
			self.uart.acknowledge();
			self.take_received();
			interrupt_ack(self.irq);
		} else {
			interrupt_ack(self.irq);
			self.uart.acknowledge();
			self.take_received();
		}
	}

	// EVERY RECEIVED BYTE THE HELD BOUND HAS ROOM FOR. What does not fit stays in the FIFO, and the receive interrupt
	// goes off with it, so the UART takes no more and the far end WAITS: typed input is backpressured, never dropped.
	// `deliver` turns it on again once the consumer has taken what is held.
	pub fn take_received(&mut self) {
		let mut chunk = [0u8; RX_CHUNK];
		loop {
			let room = self.held.room().min(RX_CHUNK);
			if room == 0 {
				self.uart.receive_interrupt(false);
				return;
			}
			let n = self.uart.receive(&mut chunk[..room]);
			if n == 0 {
				return;
			}
			self.held.push(&chunk[..n]);
		}
	}

	// EMPTY THE TAP ONTO THE WIRE, oldest first, with the marker where the ring's bound dropped bytes.
	pub fn drain_tap(&mut self, bind: &common::Bind, bootstrap: u64) -> Result<(), Error> {
		let mut chunk = [0u8; TAP_CHUNK];
		loop {
			let (n, dropped) = console_tap_read(self.tap, &mut chunk);
			if n < 0 {
				// The claim no longer holds the UART: the release is on its way, and so is the stop.
				return Err(Error::Closed);
			}
			if dropped != 0 {
				self.dropped += dropped;
				let mut marker = [0u8; 96];
				let len = uart::dropped_marker(dropped, &mut marker);
				self.transmit(&marker[..len], bind, bootstrap)?;
			}
			if n == 0 {
				return Ok(());
			}
			self.transmit(&chunk[..n as usize], bind, bootstrap)?;
		}
	}
}

// THE CONTRACT'S WRITE: the kernel's queued output first, then the consumer's bytes.
impl<E: Engine> serial_port::Wire for Console<E> {
	unsafe fn write(&mut self, payload: &[u8], bind: &common::Bind, bootstrap: u64) -> Result<u32, Error> {
		self.drain_tap(bind, bootstrap)?;
		self.transmit(payload, bind, bootstrap)?;
		Ok(payload.len() as u32)
	}
}

// Hand what was received to the consumer when one is listening; keep it otherwise - and keep it while the consumer's
// stream is full. Answers whether bytes wait on a consumer that has not read yet, which is what the serve loop's retry
// is armed for.
pub fn deliver<E: Engine>(console: &mut Console<E>, session: &mut Session, buffers: &mut serial_port::Buffers) -> bool {
	while session.listening() && !console.held.is_empty() {
		if !session.offer(console.held.bytes(), buffers) {
			return true;
		}
		console.held.clear();
		// ROOM AGAIN: the receive interrupt back on, and what the bound left in the FIFO read now - nothing raises the
		// line again for bytes that were already there while the interrupt was off.
		console.uart.receive_interrupt(true);
		console.take_received();
	}
	false
}

// THE SLEEP: the kernel's queued output put on the wire and the UART left quiet for the kernel's sleep entry, which
// lends the UART to the kernel; at the resume the UART programmed again whole - whatever the sleep left in it - and
// what the kernel queued meanwhile put out. The UART is never rebound for a sleep, so the kernel's console never
// changes hands across one.
pub struct Sleep<'a, E: Engine> {
	pub console: &'a mut Console<E>,
	pub serving: &'a mut common::Serving,
	pub bind: &'a common::Bind,
	pub bootstrap: u64,
}

impl<E: Engine> common::SleepStep for Sleep<'_, E> {
	fn suspend(&mut self, _request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		let _ = self.console.drain_tap(self.bind, self.bootstrap);
		self.console.uart.quiet();
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, _lost_power: bool) -> bool {
		self.console.program();
		let _ = self.console.drain_tap(self.bind, self.bootstrap);
		true
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}
