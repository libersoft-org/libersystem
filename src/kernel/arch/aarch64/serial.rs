// PL011 UART - the console on QEMU's `virt` machine (UART0 at 0x0900_0000) - with an asynchronous transmit ring, and
// the handoff of the UART to a userspace driver.
//
// THE RULES ARE `arch::common::console_uart`'s: the ring, the owner, the tap, the terminal-path writer, the sleep's
// loan and the stray-access count, shared with riscv64's 16550 and taken from x86_64's COM1. What is here is the
// PL011's part - its registers, its programming and its interrupt line - and the HAL's entry points onto the one
// instance, `CONSOLE`.
//
// The kernel runs in the higher half, so the device's MMIO is reached through the physical direct map
// (`phys_to_virt`). The base is QEMU `virt`'s, stated rather than discovered: the kernel writes to it from its first
// line, before a tree is read. The tree is what says whether it is THE CONSOLE - its `/chosen/stdout-path` node at this
// base - and only then is its platform row the one whose claim takes the UART (`platform::describe`).
//
// THE KERNEL NEVER PROGRAMS THE DIVISOR OF ITS OWN ACCORD. The firmware left the PL011 enabled at its rate and the
// first line goes out on it; the kernel reads the firmware's divisor and line control once the scheduler is up, and
// writes exactly those back when it takes the UART again - from a driver, on the terminal path, after a sleep.

use super::paging::phys_to_virt;
use crate::arch::common::console_uart::{self, Handler, Io, Model, Uart};
use core::fmt::{self, Write};
#[cfg(not(test))]
use core::sync::atomic::AtomicBool;
use core::sync::atomic::{AtomicU32, Ordering};

// UART0 on QEMU virt.
pub(crate) const UART_BASE: u64 = 0x0900_0000;

// The registers (ARM PrimeCell UART PL011 TRM), as offsets from the base. Every one is 32 bits wide.
const DR: u16 = 0x00; // data
const FR: u16 = 0x18; // flags
const IBRD: u16 = 0x24; // integer baud divisor
const FBRD: u16 = 0x28; // fractional baud divisor
const LCR_H: u16 = 0x2C; // line control - its write latches the divisor
const CR: u16 = 0x30; // control
const IFLS: u16 = 0x34; // FIFO trigger levels
const IMSC: u16 = 0x38; // interrupt mask set/clear
const ICR: u16 = 0x44; // interrupt clear
const DMACR: u16 = 0x48; // DMA control

const FR_BUSY: u32 = 1 << 3; // a character is on the wire, or the transmit FIFO is not empty
const FR_RXFE: u32 = 1 << 4; // receive FIFO empty
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full
const FR_TXFE: u32 = 1 << 7; // transmit FIFO empty
const CR_UARTEN: u32 = 1 << 0;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;
// Word length 8 and the FIFOs on: what the kernel's line control is when the firmware's was never read - and when it
// reads as five-bit words, which is a register nobody programmed rather than a console anybody chose.
const LCR_H_WLEN: u32 = 0b11 << 5;
const LCR_H_8_FIFO: u32 = (0b11 << 5) | (1 << 4);
// Send break.
const LCR_H_BRK: u32 = 1 << 0;
// The trigger levels as the PL011 resets them: half full each way.
const IFLS_RESET: u32 = 0x12;
// The two receive interrupts: RX, the FIFO holds bytes at its trigger level, and RT, bytes have sat in it below that
// level for 32 bit periods.
const INT_RX: u32 = 1 << 4;
const INT_RT: u32 = 1 << 6;
// Every interrupt the PL011 raises, for a clear.
const INT_ALL: u32 = 0x7FF;

// WHAT THE KERNEL KEEPS OF A PL011's SETTINGS: the divisor, the line control and the trigger levels - and for a driver's
// loan, its control and interrupt mask too.
#[derive(Clone, Copy)]
pub struct Settings {
	ibrd: u32,
	fbrd: u32,
	lcr_h: u32,
	ifls: u32,
	cr: u32,
	imsc: u32,
}

// THE PL011 AS THE CONSOLE'S MODEL: its base, and the kernel's receive line once the boot tail armed it.
pub struct Pl011 {
	base: u64,
	// The SPI's INTID (0 until armed).
	intid: AtomicU32,
	// Whether the tree says the line is level-triggered, as a PL011's is.
	#[cfg(not(test))]
	level: AtomicBool,
}

impl Pl011 {
	// The line the boot tail armed, kept for the reacquisition.
	#[cfg(not(test))]
	fn record_line(&self, intid: u32, level: bool) {
		self.level.store(level, Ordering::Relaxed);
		self.intid.store(intid, Ordering::Release);
	}

	// Wait for the character on the wire and the transmit FIFO behind it - bounded, so a disabled UART that never
	// sends cannot hang the terminal path.
	fn wait_not_busy(io: &Io<'_, Self>) {
		let mut polls = 0u32;
		while io.read(FR) & FR_BUSY != 0 && polls < console_uart::TX_POLLS {
			core::hint::spin_loop();
			polls += 1;
		}
	}
}

impl Model for Pl011 {
	const NAME: &'static str = "the PL011";
	type Settings = Settings;

	fn base(&self) -> u64 {
		self.base
	}

	fn raw_read(&self, offset: u16) -> u32 {
		// SAFETY: one of the PL011's own registers, through the direct map, which covers the device window.
		unsafe { core::ptr::read_volatile(phys_to_virt(self.base + u64::from(offset)) as *const u32) }
	}

	fn raw_write(&self, offset: u16, value: u32) {
		// SAFETY: as `raw_read`.
		unsafe { core::ptr::write_volatile(phys_to_virt(self.base + u64::from(offset)) as *mut u32, value) }
	}

	// ONE BYTE AT A TIME: the transmit FIFO's full flag is exact, so it is asked before every byte.
	fn tx_room(&self, io: &Io<'_, Self>) -> usize {
		if io.read(FR) & FR_TXFF == 0 { 1 } else { 0 }
	}

	fn tx_put(&self, io: &Io<'_, Self>, byte: u8) {
		io.write(DR, u32::from(byte));
	}

	fn tx_idle(&self, io: &Io<'_, Self>) -> bool {
		let flags = io.read(FR);
		flags & FR_BUSY == 0 && flags & FR_TXFE != 0
	}

	// AN EMPTY FIFO ALSO ENDS A RECEIVE TIMEOUT, which is cleared here: RT stays raised until it is, and the interrupt
	// that reported it has been answered by the read that found nothing left.
	fn rx_take(&self, io: &Io<'_, Self>) -> Option<u8> {
		if io.read(FR) & FR_RXFE != 0 {
			io.write(ICR, INT_RT);
			return None;
		}
		Some(io.read(DR) as u8)
	}

	// Nothing stands between the kernel and a PL011's receive FIFO: the data register reads it whatever the control
	// register says.
	fn rx_prepare(&self, _io: &Io<'_, Self>, _firmware: Option<&Settings>) {}

	fn read_firmware(&self, io: &Io<'_, Self>) -> Settings {
		Settings { ibrd: io.read(IBRD), fbrd: io.read(FBRD), lcr_h: io.read(LCR_H), ifls: io.read(IFLS), cr: io.read(CR), imsc: io.read(IMSC) }
	}

	// THE BOOT PROGRAMMING, in the order the TRM gives: every interrupt masked; the UART disabled once the character
	// on the wire is out; the FIFOs flushed by clearing their enable; the firmware's divisor where it was read and set,
	// then the line control - whose write is what latches the divisor - the trigger levels, DMA off and every
	// interrupt cleared; enabled with transmit and receive and loopback off; and the receive interrupts last.
	fn boot_init(&self, io: &Io<'_, Self>, firmware: Option<&Settings>, receive: bool) {
		io.write(IMSC, 0);
		Self::wait_not_busy(io);
		io.write(CR, 0);
		io.write(LCR_H, 0);
		let (lcr_h, ifls) = match firmware {
			Some(settings) => {
				if settings.ibrd != 0 {
					io.write(IBRD, settings.ibrd);
					io.write(FBRD, settings.fbrd);
				}
				(if settings.lcr_h & LCR_H_WLEN == 0 { LCR_H_8_FIFO } else { settings.lcr_h }, settings.ifls)
			}
			None => (LCR_H_8_FIFO, IFLS_RESET),
		};
		io.write(LCR_H, lcr_h & !LCR_H_BRK);
		io.write(IFLS, ifls);
		io.write(DMACR, 0);
		io.write(ICR, INT_ALL);
		io.write(CR, CR_UARTEN | CR_TXE | CR_RXE);
		if receive {
			io.write(IMSC, INT_RX | INT_RT);
		}
	}

	fn receive_enable(&self, io: &Io<'_, Self>, on: bool) {
		io.write(ICR, INT_RX | INT_RT);
		io.write(IMSC, if on { INT_RX | INT_RT } else { 0 });
	}

	fn lend_read(&self, io: &Io<'_, Self>) -> Settings {
		self.read_firmware(io)
	}

	// THE DRIVER'S SETTINGS BACK, in the boot programming's order: disabled while the divisor and the line control
	// are written, then its control, and its interrupt mask last.
	fn lend_write(&self, io: &Io<'_, Self>, lent: &Settings) {
		io.write(IMSC, 0);
		Self::wait_not_busy(io);
		io.write(CR, 0);
		io.write(IBRD, lent.ibrd);
		io.write(FBRD, lent.fbrd);
		io.write(LCR_H, lent.lcr_h);
		io.write(IFLS, lent.ifls);
		io.write(ICR, INT_ALL);
		io.write(CR, lent.cr);
		io.write(IMSC, lent.imsc);
	}

	fn line(&self) -> Option<(&'static str, u32)> {
		let intid = self.intid.load(Ordering::Acquire);
		(intid != 0).then_some(("INTID", intid))
	}

	// THE SPI LET GO: disabled at the distributor - a level line a driver has not bound yet would otherwise be taken
	// again at every EOI - and the kernel's handler off it, so `interrupts::bind_wired` gives it to the claim.
	fn release_line(&self) {
		#[cfg(not(test))]
		{
			let intid = self.intid.load(Ordering::Acquire);
			if intid != 0 {
				super::gic::disable_spi(intid);
				super::interrupts::unregister(intid);
			}
		}
	}

	// AND TAKEN AGAIN: the handler first, then the SPI enabled as the tree configures it. The claim's binding was
	// undone by its release before this runs.
	fn rearm_line(&self, handler: Handler) {
		#[cfg(not(test))]
		{
			let intid = self.intid.load(Ordering::Acquire);
			if intid != 0 && super::interrupts::register(intid, handler) {
				super::gic::enable_spi(intid, if self.level.load(Ordering::Relaxed) { super::gic::Trigger::Level } else { super::gic::Trigger::Edge });
			}
		}
		#[cfg(test)]
		let _ = handler;
	}
}

// THE CONSOLE.
pub static CONSOLE: Uart<Pl011> = Uart::new(
	Pl011 {
		base: UART_BASE,
		intid: AtomicU32::new(0),
		#[cfg(not(test))]
		level: AtomicBool::new(false),
	},
	true,
);

// ------------------------------------------------------------------ the console's entry points

// NOTHING AT THE FIRST LINE: the firmware left the PL011 running, and its settings are read once the scheduler is up
// (`enable_async`) - this runs before the floating-point unit is on.
pub fn init() {}

// THE CONSOLE UART'S RECEIVE INTERRUPT, armed on the line the device tree names for the PL011 - an SPI through the GIC
// this kernel drives - and answered by `handler`, so a typed byte wakes an idle machine that takes no periodic tick to
// find it by. Answers the INTID an idle core's record names it by; `Err` says why the UART's input stays polled. The
// handler is kept, so a reacquisition after a driver held the UART arms it again.
#[cfg(not(test))]
pub fn arm_rx_interrupt(handler: super::interrupts::HandlerFn) -> Result<u32, &'static str> {
	let tree = super::device_tree().ok_or("the machine handed over no device tree")?;
	// THE NODE THE LINE IS READ FROM IS THE UART THIS DRIVER WRITES TO, or it is some other device's line.
	if tree.console().map(|console| console.base) != Some(UART_BASE) {
		return Err("the device tree's console is not the UART this kernel writes to");
	}
	let route = tree.console_interrupt().ok_or("the device tree names no line for it")?;
	// Three cells whose first is zero is the GIC binding saying "an SPI", as for a hot-plug port's line.
	if route.cells != 3 || route.spec[0] != 0 {
		return Err("its line goes to a controller this kernel does not drive");
	}
	let intid: u32 = route.spec[1].checked_add(32).ok_or("the device tree names an SPI past the controller")?;
	// The binding's flags: 1 and 2 are edges, 4 and 8 levels.
	let trigger = if route.spec[2] & 0x3 != 0 { super::gic::Trigger::Edge } else { super::gic::Trigger::Level };
	CONSOLE.model().record_line(intid, trigger == super::gic::Trigger::Level);
	// THE UART TOLD FIRST, THEN THE HANDLER REGISTERED, THEN THE SOURCE ENABLED: a byte typed during the boot is
	// already waiting, and the enable would otherwise take its interrupt with no handler behind it.
	CONSOLE.arm_receive(handler, || {
		if !super::interrupts::register(intid, handler) {
			return Err("this kernel answers as many wired lines as it carries rows for");
		}
		super::gic::enable_spi(intid, trigger);
		Ok(())
	})?;
	Ok(intid)
}

// Switch transmit to the asynchronous ring. Called once the scheduler is up, so the timer tick and the idle loop are
// draining the ring - and the floating-point unit is on, so the firmware's settings are read here.
pub fn enable_async() {
	CONSOLE.keep_firmware_settings();
	CONSOLE.enable_async();
}

pub fn drain_tx() {
	CONSOLE.drain_tx();
}

// THE SLEEP ENTRY'S WINDOW ON THE CONSOLE UART - see `console_uart::Uart::sleep_begin`.
pub fn sleep_begin() {
	CONSOLE.sleep_begin();
}

pub fn sleep_wake(lost_settings: bool) {
	CONSOLE.sleep_wake(lost_settings);
}

pub fn sleep_end() {
	CONSOLE.sleep_end();
}

pub fn tx_pending() -> bool {
	CONSOLE.tx_pending()
}

// THE TERMINAL-PATH WRITER - see `console_uart::Uart::terminal`. Entered before the first line of a panic or a fatal
// exception, and before a reset, a power-off or the test exit acts.
pub fn flush_sync() {
	CONSOLE.terminal();
}

pub fn drain_sync() {
	CONSOLE.drain_sync();
}

pub fn make_room() {
	CONSOLE.make_room();
}

// A PLANNED END'S LAST LINES - see `console_uart::Uart::settle_driver`. For a power-off or a reset a process asked for.
pub fn settle_driver() {
	CONSOLE.settle_driver();
}

pub fn write_bytes(bytes: &[u8]) -> usize {
	CONSOLE.write_bytes(bytes)
}

pub fn write_whole(bytes: &[u8]) -> bool {
	CONSOLE.write_whole(bytes)
}

#[cfg(not(test))]
pub fn read_byte() -> Option<u8> {
	CONSOLE.read_byte()
}

// ------------------------------------------------------------------ the handoff, by the UART's base

fn instance(base: u64) -> Option<&'static Uart<Pl011>> {
	(base == CONSOLE.base()).then_some(&CONSOLE)
}

pub fn console_hand_over(base: u64, row: usize, generation: u64) -> bool {
	instance(base).is_some_and(|uart| uart.hand_over(generation, row))
}

pub fn console_hand_back(base: u64, quarantined: bool) {
	if let Some(uart) = instance(base) {
		uart.hand_back(quarantined);
	}
}

pub fn console_held_by(base: u64, generation: u64) -> bool {
	instance(base).is_some_and(|uart| uart.held_by(generation))
}

pub fn console_attach_tap(base: u64, generation: u64, tap: &alloc::sync::Arc<crate::object::console_tap::ConsoleTap>) -> bool {
	instance(base).is_some_and(|uart| uart.attach_tap(generation, tap))
}

pub fn console_tap_read(base: u64, generation: u64, buf: &mut [u8]) -> Option<(usize, u64, usize)> {
	instance(base)?.tap_read(generation, buf)
}

// The debug-write syscall's half of telling a tap: it holds no lock when it asks.
pub fn console_deliver() {
	CONSOLE.deliver_tap_signal();
}

// Whether the console's ring has dropped bytes at its bound that nobody has reported yet - for the development request
// that fills it.
#[cfg(liber_development)]
pub fn console_dropped() -> Option<bool> {
	Some(CONSOLE.dropped_pending())
}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		for byte in s.bytes() {
			if byte == b'\n' {
				CONSOLE.write_byte(b'\r');
			}
			CONSOLE.write_byte(byte);
		}
		Ok(())
	}
}
