// THE 16550 REGISTER ENGINE, apart from where the UART is and what clocks it.
//
// A 16550 is eight registers and the same eight wherever it sits: at a PC's legacy ports, in a
// firmware-described register window, behind a board's bus. What differs is HOW a register is reached (the
// `Registers` a driver supplies) and WHAT CLOCKS the baud generator (the `Line` a platform description
// gives). This module knows the registers and nothing about either, so the driver binary is the platform glue
// and the engine is host-tested against a scripted UART.
//
// And two pieces of the console handoff that are not about registers at all: the bound on what is received
// while nobody is attached to read it, and the line that stands on the wire where kernel output was dropped.

#[cfg(test)]
mod tests;

// The registers, as offsets from the UART's base.
pub const RBR_THR: u16 = 0;
pub const IER: u16 = 1;
pub const IIR_FCR: u16 = 2;
pub const LCR: u16 = 3;
pub const MCR: u16 = 4;
pub const LSR: u16 = 5;

pub const IER_RX_AVAILABLE: u8 = 0x01;
pub const IER_TX_EMPTY: u8 = 0x02;
pub const LCR_DLAB: u8 = 0x80;
pub const LCR_8N1: u8 = 0x03;
// FIFOs on, both cleared, the receive trigger at one byte - a console wants each keystroke at once.
pub const FCR_ENABLE_AND_CLEAR: u8 = 0x07;
// DTR, RTS and OUT2 - on a PC OUT2 gates the UART's interrupt onto its line - with loopback off.
pub const MCR_CONSOLE: u8 = 0x0B;
pub const LSR_DATA_READY: u8 = 0x01;
pub const LSR_THR_EMPTY: u8 = 0x20;
// The line-status bits that report a received byte was lost or damaged: overrun, parity, framing, break.
pub const LSR_ERRORS: u8 = 0x1E;

// The transmit FIFO's depth: what one empty holding register takes at once.
pub const TX_FIFO: usize = 16;

// The input clock of a PC's UARTs, and the rate the kernel console runs at.
pub const PC_CLOCK_HZ: u32 = 1_843_200;
pub const CONSOLE_BAUD: u32 = 38_400;

// HOW A REGISTER IS REACHED: a port, a word in a window, whatever the platform has.
pub trait Registers {
	fn read(&mut self, offset: u16) -> u8;
	fn write(&mut self, offset: u16, value: u8);
}

// WHAT THE PLATFORM DESCRIBES AND WHAT THE LINE WANTS, as the one number the engine programs: the divisor of
// the baud generator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
	pub divisor: u16,
}

impl Line {
	// The divisor that makes `baud` from a `clock_hz` input clock, or `None` when none comes within 3% of it -
	// a rate the receiver at the other end would misread rather than a slightly slow one.
	pub fn new(clock_hz: u32, baud: u32) -> Option<Line> {
		if clock_hz == 0 || baud == 0 {
			return None;
		}
		let step = 16 * baud as u64;
		let divisor = (clock_hz as u64 + step / 2) / step;
		if divisor == 0 || divisor > u16::MAX as u64 {
			return None;
		}
		let actual = clock_hz as u64 / (16 * divisor);
		let error = actual.abs_diff(baud as u64);
		if error * 100 > 3 * baud as u64 {
			return None;
		}
		Some(Line { divisor: divisor as u16 })
	}
}

pub struct Uart<R: Registers> {
	regs: R,
	// The interrupt enables this engine left set, so turning the transmit interrupt on and off keeps the rest.
	ier: u8,
	// Received bytes the line status reported damaged or lost, since this was made.
	errors: u64,
}

impl<R: Registers> Uart<R> {
	pub fn new(regs: R) -> Self {
		Uart { regs, ier: 0, errors: 0 }
	}

	// PROGRAM THE WHOLE UART, whatever state it was left in: interrupt enables off, the divisor through the
	// latch and the latch closed again with 8N1, the FIFOs on and cleared, the console's modem control with
	// loopback off - and the receive interrupt last, when asked, so no interrupt is raised half way.
	pub fn program(&mut self, line: Line, receive_interrupt: bool) {
		self.regs.write(IER, 0);
		self.regs.write(LCR, LCR_DLAB);
		self.regs.write(RBR_THR, line.divisor as u8);
		self.regs.write(IER, (line.divisor >> 8) as u8);
		self.regs.write(LCR, LCR_8N1);
		self.regs.write(IIR_FCR, FCR_ENABLE_AND_CLEAR);
		self.regs.write(MCR, MCR_CONSOLE);
		self.ier = if receive_interrupt { IER_RX_AVAILABLE } else { 0 };
		self.regs.write(IER, self.ier);
	}

	// Every interrupt enable off: what a stopping driver leaves.
	pub fn quiet(&mut self) {
		self.ier = 0;
		self.regs.write(IER, 0);
	}

	// Whether the transmit interrupt is wanted: on while a write waits for the FIFO to empty, off otherwise, so
	// an idle UART raises nothing for an empty holding register.
	pub fn transmit_interrupt(&mut self, on: bool) {
		let ier = if on { self.ier | IER_TX_EMPTY } else { self.ier & !IER_TX_EMPTY };
		if ier != self.ier {
			self.ier = ier;
			self.regs.write(IER, ier);
		}
	}

	// As much of `bytes` as the transmitter takes now - a FIFO load when the holding register is empty, and
	// nothing otherwise. Answers how many.
	pub fn put(&mut self, bytes: &[u8]) -> usize {
		if bytes.is_empty() || self.regs.read(LSR) & LSR_THR_EMPTY == 0 {
			return 0;
		}
		let n = bytes.len().min(TX_FIFO);
		for &byte in &bytes[..n] {
			self.regs.write(RBR_THR, byte);
		}
		n
	}

	// EVERY BYTE THE RECEIVER HOLDS, into `out` while it has room - the FIFO emptied, which on an edge line
	// is what lets the next byte raise the next edge. A byte past `out`'s room stays in the FIFO for the next
	// call. Answers how many; bytes the line status says were damaged or lost are counted in `errors`.
	pub fn receive(&mut self, out: &mut [u8]) -> usize {
		let mut n = 0usize;
		while n < out.len() {
			let status = self.regs.read(LSR);
			if status & LSR_ERRORS != 0 {
				self.errors += 1;
			}
			if status & LSR_DATA_READY == 0 {
				break;
			}
			out[n] = self.regs.read(RBR_THR);
			n += 1;
		}
		n
	}

	// Read the interrupt identification, which clears a transmit-empty interrupt the UART raised.
	pub fn acknowledge(&mut self) -> u8 {
		self.regs.read(IIR_FCR)
	}

	pub fn errors(&self) -> u64 {
		self.errors
	}
}

// WHAT IS RECEIVED WHILE NOBODY READS IT, BOUNDED: a shell that was listening before a handoff loses no
// keystroke to it. Five hundred and twelve bytes - the kernel's own input bound. Past it the NEWEST is dropped
// and counted, not the oldest: what a person typed first is what they typed.
pub const HELD_INPUT: usize = 512;

pub struct Held {
	bytes: [u8; HELD_INPUT],
	len: usize,
	dropped: u64,
}

impl Default for Held {
	fn default() -> Self {
		Held { bytes: [0; HELD_INPUT], len: 0, dropped: 0 }
	}
}

impl Held {
	// Keep what fits; answers how many were kept.
	pub fn push(&mut self, bytes: &[u8]) -> usize {
		let room = HELD_INPUT - self.len;
		let kept = bytes.len().min(room);
		self.bytes[self.len..self.len + kept].copy_from_slice(&bytes[..kept]);
		self.len += kept;
		self.dropped += (bytes.len() - kept) as u64;
		kept
	}

	pub fn is_empty(&self) -> bool {
		self.len == 0
	}

	pub fn bytes(&self) -> &[u8] {
		&self.bytes[..self.len]
	}

	// Everything kept, handed on.
	pub fn clear(&mut self) {
		self.len = 0;
	}

	pub fn dropped(&self) -> u64 {
		self.dropped
	}
}

// THE LINE THAT STANDS WHERE KERNEL OUTPUT WAS DROPPED: the kernel's ring has a bound, a driver slower than
// the kernel's output can let it pass, and the tap reports how many bytes it dropped. Written into `out`;
// answers its length.
pub fn dropped_marker(count: u64, out: &mut [u8; 96]) -> usize {
	let mut at = 0usize;
	let mut put = |bytes: &[u8]| {
		let take = bytes.len().min(out.len() - at);
		out[at..at + take].copy_from_slice(&bytes[..take]);
		at += take;
	};
	put(b"\r\n[console: ");
	let mut digits = [0u8; 20];
	let mut value = count;
	let mut len = 0usize;
	loop {
		digits[len] = b'0' + (value % 10) as u8;
		len += 1;
		value /= 10;
		if value == 0 {
			break;
		}
	}
	digits[..len].reverse();
	put(&digits[..len]);
	put(b" byte(s) of kernel output dropped at the ring's bound]\r\n");
	at
}
