// THE PL011 REGISTER ENGINE, apart from where the UART is and what clocks it.
//
// The ARM PrimeCell UART is the 16550's neighbour in every way that matters to a console and in no way that matters
// to its registers: thirty-two-bit registers at fixed offsets, a flag register instead of a line status, a baud
// divisor with a six-bit fraction that only a write of the line control latches, an interrupt mask instead of
// enables, and interrupts cleared by writing them rather than by reading an identification. So it has an engine of
// its own beside `uart`'s, the same shape - HOW a register is reached is the `Registers` a driver supplies, WHAT
// CLOCKS the baud generator is the `Line` a platform description gives - and host-tested the same way, against a
// scripted UART. What a console needs on top of either engine - the held input, the dropped marker, the session -
// is shared, not repeated (`uart::Held`, `uart::dropped_marker`, `serial_port::Session`).

#[cfg(test)]
mod tests;

// The registers (ARM PrimeCell UART PL011 TRM), as offsets from the UART's base.
pub const DR: u16 = 0x00;
pub const RSR_ECR: u16 = 0x04;
pub const FR: u16 = 0x18;
pub const IBRD: u16 = 0x24;
pub const FBRD: u16 = 0x28;
pub const LCR_H: u16 = 0x2C;
pub const CR: u16 = 0x30;
pub const IFLS: u16 = 0x34;
pub const IMSC: u16 = 0x38;
pub const MIS: u16 = 0x40;
pub const ICR: u16 = 0x44;
pub const DMACR: u16 = 0x48;

pub const FR_BUSY: u32 = 1 << 3;
pub const FR_RXFE: u32 = 1 << 4;
pub const FR_TXFF: u32 = 1 << 5;
// The receive error bits a byte carries in the data register: framing, parity, break and overrun.
pub const DR_ERRORS: u32 = 0xF00;
// Eight data bits, one stop bit, no parity, and the FIFOs on.
pub const LCR_H_8N1_FIFO: u32 = (0b11 << 5) | (1 << 4);
pub const CR_UARTEN: u32 = 1 << 0;
pub const CR_TXE: u32 = 1 << 8;
pub const CR_RXE: u32 = 1 << 9;
// Both trigger levels at an eighth: the transmitter asks for more while it still has some to send, and the
// receive interrupt comes early - the timeout interrupt catches the bytes below it.
pub const IFLS_EIGHTHS: u32 = 0;
// The interrupts: receive, transmit and receive timeout, and all eleven for a clear.
pub const INT_RX: u32 = 1 << 4;
pub const INT_TX: u32 = 1 << 5;
pub const INT_RT: u32 = 1 << 6;
pub const INT_ALL: u32 = 0x7FF;

// How long programming waits for the character on the wire before disabling the UART, in polls: a disabled UART
// that never sends must not hang the driver.
pub const BUSY_POLLS: u32 = 100_000;

// HOW A REGISTER IS REACHED: a word in a window, whatever the platform has.
pub trait Registers {
	fn read(&mut self, offset: u16) -> u32;
	fn write(&mut self, offset: u16, value: u32);
}

// WHAT THE PLATFORM DESCRIBES AND WHAT THE LINE WANTS, as the two numbers the engine programs: the integer and the
// six-bit fractional part of the baud divisor, UARTCLK / (16 x baud).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
	pub ibrd: u16,
	pub fbrd: u8,
}

impl Line {
	// The divisor that makes `baud` from a `clock_hz` reference clock, or `None` when none comes within 3% of it - or
	// falls outside what the divisor registers hold (1 to 65535, the fraction zero at the top).
	pub fn new(clock_hz: u64, baud: u32) -> Option<Line> {
		if clock_hz == 0 || baud == 0 {
			return None;
		}
		// 64 x clock / (16 x baud), rounded: the divisor in sixty-fourths.
		let sixty_fourths = (clock_hz.checked_mul(4)? + u64::from(baud) / 2) / u64::from(baud);
		let (ibrd, fbrd) = (sixty_fourths >> 6, sixty_fourths & 0x3F);
		if ibrd == 0 || ibrd > 0xFFFF || (ibrd == 0xFFFF && fbrd != 0) {
			return None;
		}
		let actual = clock_hz * 4 / sixty_fourths;
		if actual.abs_diff(u64::from(baud)) * 100 > 3 * u64::from(baud) {
			return None;
		}
		Some(Line { ibrd: ibrd as u16, fbrd: fbrd as u8 })
	}
}

pub struct Pl011<R: Registers> {
	regs: R,
	// The interrupts this engine left unmasked, so turning the transmit interrupt on and off keeps the rest.
	imsc: u32,
	// Received bytes the data register reported damaged or lost, since this was made.
	errors: u64,
}

impl<R: Registers> Pl011<R> {
	pub fn new(regs: R) -> Self {
		Pl011 { regs, imsc: 0, errors: 0 }
	}

	// PROGRAM THE WHOLE UART, whatever state it was left in, in the order the TRM gives: every interrupt masked; the
	// UART disabled once the character on the wire is out; the FIFOs flushed by clearing their enable; the divisor
	// when a line is given - and the one the UART holds kept when it is not, since nothing described the clock it
	// divides; the line control, whose write is what latches the divisor; the trigger levels, DMA off and every
	// interrupt cleared; enabled with transmit and receive and loopback off - and the receive interrupts last, when
	// asked, so no interrupt is raised half way.
	pub fn program(&mut self, line: Option<Line>, receive_interrupt: bool) {
		self.regs.write(IMSC, 0);
		let mut polls = 0u32;
		while self.regs.read(FR) & FR_BUSY != 0 && polls < BUSY_POLLS {
			polls += 1;
		}
		self.regs.write(CR, 0);
		self.regs.write(LCR_H, 0);
		if let Some(line) = line {
			self.regs.write(IBRD, u32::from(line.ibrd));
			self.regs.write(FBRD, u32::from(line.fbrd));
		}
		self.regs.write(LCR_H, LCR_H_8N1_FIFO);
		self.regs.write(IFLS, IFLS_EIGHTHS);
		self.regs.write(DMACR, 0);
		self.regs.write(ICR, INT_ALL);
		self.regs.write(CR, CR_UARTEN | CR_TXE | CR_RXE);
		self.imsc = if receive_interrupt { INT_RX | INT_RT } else { 0 };
		self.regs.write(IMSC, self.imsc);
	}

	// Every interrupt masked: what a stopping driver leaves.
	pub fn quiet(&mut self) {
		self.imsc = 0;
		self.regs.write(IMSC, 0);
	}

	fn mask(&mut self, bits: u32, on: bool) {
		let imsc = if on { self.imsc | bits } else { self.imsc & !bits };
		if imsc != self.imsc {
			self.imsc = imsc;
			self.regs.write(IMSC, imsc);
		}
	}

	// Whether the transmit interrupt is wanted: unmasked while a write waits for the FIFO to drain to its trigger
	// level, masked otherwise, so an idle UART raises nothing for an empty FIFO.
	pub fn transmit_interrupt(&mut self, on: bool) {
		self.mask(INT_TX, on);
	}

	// Whether the receive interrupts are wanted: masked while what was received has nowhere to go, so the bytes stay
	// in the FIFO and the far end waits for room instead of losing them; unmasked again once there is room. The
	// transmit interrupt is kept either way.
	pub fn receive_interrupt(&mut self, on: bool) {
		self.mask(INT_RX | INT_RT, on);
	}

	// As much of `bytes` as the transmitter takes now: while the FIFO is not full, a byte at a time, and nothing once
	// it is. Answers how many.
	pub fn put(&mut self, bytes: &[u8]) -> usize {
		let mut n = 0usize;
		while n < bytes.len() && self.regs.read(FR) & FR_TXFF == 0 {
			self.regs.write(DR, u32::from(bytes[n]));
			n += 1;
		}
		n
	}

	// EVERY BYTE THE RECEIVER HOLDS, into `out` while it has room - the FIFO emptied, which is what lowers a level
	// line. A byte past `out`'s room stays in the FIFO for the next call. A byte the data register says was damaged
	// or lost is counted in `errors` and its error cleared; and the FIFO found empty ends a receive timeout, which is
	// cleared here - the timeout interrupt stays raised until it is. Answers how many.
	pub fn receive(&mut self, out: &mut [u8]) -> usize {
		let mut n = 0usize;
		while n < out.len() {
			if self.regs.read(FR) & FR_RXFE != 0 {
				self.regs.write(ICR, INT_RT);
				break;
			}
			let word = self.regs.read(DR);
			if word & DR_ERRORS != 0 {
				self.errors += 1;
				self.regs.write(RSR_ECR, 0);
			}
			out[n] = word as u8;
			n += 1;
		}
		n
	}

	// THE TRANSMIT INTERRUPT CLEARED: it is raised as the FIFO drains past its trigger level and stays raised until it
	// is cleared or the FIFO fills again - and a transmitter with nothing more to send does neither. Answers the
	// interrupts that were raised and unmasked as the line fired.
	pub fn acknowledge(&mut self) -> u32 {
		let raised = self.regs.read(MIS);
		self.regs.write(ICR, INT_TX);
		raised
	}

	pub fn errors(&self) -> u64 {
		self.errors
	}
}
