// riscv64 serial console: the NS16550 UART at 0x1000_0000 on QEMU's `virt` machine, with an asynchronous transmit ring,
// and the handoff of the UART to a userspace driver.
//
// THE SAME DEVICE THE FIRMWARE USES, and that is the whole point. This was the SBI legacy console
// (EID 0x01 `console_putchar`), which is the obvious choice - OpenSBI is already there, no driver
// is needed - and which on this machine writes to nothing. Measured: a full test-kernel boot made
// 9256 `console_putchar` ecalls and not one byte reached the serial log, while the loader's own
// output through the same UART did. Every riscv64 line the kernel has ever printed has been lost,
// including the ones a boot failure would print, which is why "riscv64 hangs after
// ExitBootServices with no kernel output" was diagnosed for a day as a hang. It was not: the
// kernel was running, with a console nobody could hear.
//
// THE RULES ARE `arch::common::console_uart`'s: the ring, the owner, the tap, the terminal-path writer, the sleep's
// loan and the stray-access count, shared with aarch64's PL011 and taken from x86_64's COM1. What is here is the
// 16550's part over a register window - its registers, its programming and its interrupt line - and the HAL's entry
// points onto the one instance, `CONSOLE`. The kernel runs in the higher half, so the MMIO is reached through the
// physical direct map (`phys_to_virt`) - which the boot stub installs before it branches here, so this is usable from
// the kernel's first line.
//
// The base is QEMU `virt`'s, stated rather than discovered, exactly as the aarch64 port states UART0's: the kernel
// writes to it from its first line, before a tree is read, with byte-wide registers and no shift. The tree is what
// says whether it is THE CONSOLE - its `/chosen/stdout-path` node at this base, with that register layout - and only
// then is its platform row the one whose claim takes the UART (`platform::describe`).
//
// THE KERNEL NEVER PROGRAMS THE DIVISOR OF ITS OWN ACCORD. The firmware left the line configured and the first line
// goes out on it; the kernel reads the firmware's divisor, line control and modem control once the scheduler is up,
// and writes exactly those back when it takes the UART again - from a driver, on the terminal path, after a sleep.

use super::paging::phys_to_virt;
use crate::arch::common::console_uart::{Handler, Io, Model, Uart};
use core::fmt::{self, Write};
#[cfg(not(test))]
use core::sync::atomic::AtomicU64;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// The NS16550 on QEMU virt. Byte-wide registers, no shift.
pub(crate) const UART_BASE: u64 = 0x1000_0000;
const RBR_THR: u16 = 0x00; // receive buffer (read) / transmit holding (write); the divisor's low byte under DLAB
const IER: u16 = 0x01; // interrupt enable; the divisor's high byte under DLAB
const IIR_FCR: u16 = 0x02; // interrupt identification (read) / FIFO control (write)
const LCR: u16 = 0x03; // line control
const MCR: u16 = 0x04; // modem control
const LSR: u16 = 0x05; // line status

const IER_RX_AVAILABLE: u8 = 1 << 0;
const LSR_DATA_READY: u8 = 1 << 0;
const LSR_THR_EMPTY: u8 = 1 << 5;
const LSR_TX_IDLE: u8 = 1 << 6;
const LCR_DLAB: u8 = 0x80;
// Eight data bits, no parity, one stop bit: the line control when the firmware's was never read - and when it reads
// as five-bit words, which is a register nobody programmed rather than a console anybody chose.
const LCR_8N1: u8 = 0x03;
const LCR_WORD: u8 = 0x03;
// The FIFOs on and both cleared, the receive trigger at one byte.
const FCR_ENABLE_AND_CLEAR: u8 = 0xC7;
// IIR's FIFO bits: both set while the FIFOs are on.
const IIR_FIFOS: u8 = 0xC0;
const MCR_LOOPBACK: u8 = 1 << 4;
// DTR, RTS and OUT2, loopback off: the modem control when the firmware's was never read.
const MCR_CONSOLE: u8 = 0x0B;
// What an empty holding register takes at once with the FIFOs on.
const TX_FIFO: usize = 16;

// WHAT THE KERNEL KEEPS OF A 16550's SETTINGS: the divisor, the line and modem control, and for a driver's loan its
// interrupt enables too.
#[derive(Clone, Copy)]
pub struct Settings {
	dll: u8,
	dlm: u8,
	lcr: u8,
	mcr: u8,
	ier: u8,
}

// THE 16550 AS THE CONSOLE'S MODEL: its base, whether its FIFOs are on, and the kernel's receive line once the boot
// tail armed it.
pub struct Ns16550 {
	base: u64,
	// Whether an empty holding register takes a FIFO's load rather than one byte: read from IIR with the firmware's
	// settings, and set by every boot programming, which turns the FIFOs on.
	fifo: AtomicBool,
	// The APLIC source (0 until armed).
	source: AtomicU32,
	// Its controller's register block, the trigger the tree states and the hart its interrupt file is on.
	#[cfg(not(test))]
	aplic: AtomicU64,
	#[cfg(not(test))]
	trigger: AtomicU32,
	#[cfg(not(test))]
	hart: AtomicU64,
}

impl Ns16550 {
	// The line the boot tail armed, kept for the reacquisition.
	#[cfg(not(test))]
	fn record_line(&self, aplic: u64, source: u32, trigger: u32, hart: u64) {
		self.aplic.store(aplic, Ordering::Relaxed);
		self.trigger.store(trigger, Ordering::Relaxed);
		self.hart.store(hart, Ordering::Relaxed);
		self.source.store(source, Ordering::Release);
	}

	// The divisor, read through the latch and the latch closed again on the line control it found.
	fn read_divisor(io: &Io<'_, Self>, lcr: u8) -> (u8, u8) {
		io.write(LCR, u32::from(lcr | LCR_DLAB));
		let divisor = (io.read(RBR_THR) as u8, io.read(IER) as u8);
		io.write(LCR, u32::from(lcr & !LCR_DLAB));
		divisor
	}

	// A driver's or the firmware's settings, read: the interrupt enables and the line and modem control, then the
	// divisor through the latch.
	fn read_settings(io: &Io<'_, Self>) -> Settings {
		let ier = io.read(IER) as u8;
		let lcr = io.read(LCR) as u8 & !LCR_DLAB;
		let mcr = io.read(MCR) as u8;
		let (dll, dlm) = Self::read_divisor(io, lcr);
		Settings { dll, dlm, lcr, mcr, ier }
	}
}

impl Model for Ns16550 {
	const NAME: &'static str = "the 16550";
	type Settings = Settings;

	fn base(&self) -> u64 {
		self.base
	}

	fn raw_read(&self, offset: u16) -> u32 {
		// SAFETY: one of the 16550's own registers, through the direct map, which covers the device window.
		u32::from(unsafe { core::ptr::read_volatile(phys_to_virt(self.base + u64::from(offset)) as *const u8) })
	}

	fn raw_write(&self, offset: u16, value: u32) {
		// SAFETY: as `raw_read`.
		unsafe { core::ptr::write_volatile(phys_to_virt(self.base + u64::from(offset)) as *mut u8, value as u8) }
	}

	// AN EMPTY HOLDING REGISTER TAKES A FIFO'S LOAD while the FIFOs are on, and one byte while they are off.
	fn tx_room(&self, io: &Io<'_, Self>) -> usize {
		if io.read(LSR) as u8 & LSR_THR_EMPTY == 0 {
			0
		} else if self.fifo.load(Ordering::Relaxed) {
			TX_FIFO
		} else {
			1
		}
	}

	fn tx_put(&self, io: &Io<'_, Self>, byte: u8) {
		io.write(RBR_THR, u32::from(byte));
	}

	fn tx_idle(&self, io: &Io<'_, Self>) -> bool {
		io.read(LSR) as u8 & LSR_TX_IDLE != 0
	}

	fn rx_take(&self, io: &Io<'_, Self>) -> Option<u8> {
		if io.read(LSR) as u8 & LSR_DATA_READY == 0 {
			return None;
		}
		Some(io.read(RBR_THR) as u8)
	}

	// DLAB CLEARED, which a driver may have left set: the receive register is the divisor's low byte while it is.
	fn rx_prepare(&self, io: &Io<'_, Self>, firmware: Option<&Settings>) {
		io.write(LCR, u32::from(firmware.map_or(LCR_8N1, |settings| settings.lcr) & !LCR_DLAB));
	}

	fn read_firmware(&self, io: &Io<'_, Self>) -> Settings {
		let settings = Self::read_settings(io);
		self.fifo.store(io.read(IIR_FCR) as u8 & IIR_FIFOS == IIR_FIFOS, Ordering::Relaxed);
		settings
	}

	// THE BOOT PROGRAMMING, as COM1's with the firmware's numbers in it: interrupt enables off, the firmware's divisor
	// through the latch where it was read and set, the latch closed with the firmware's line control, the FIFOs on and
	// cleared, the firmware's modem control with loopback off - and the receive interrupt last, when asked.
	fn boot_init(&self, io: &Io<'_, Self>, firmware: Option<&Settings>, receive: bool) {
		io.write(IER, 0);
		let (lcr, mcr) = match firmware {
			Some(settings) => {
				if settings.dll != 0 || settings.dlm != 0 {
					io.write(LCR, u32::from(LCR_DLAB));
					io.write(RBR_THR, u32::from(settings.dll));
					io.write(IER, u32::from(settings.dlm));
				}
				(if settings.lcr & LCR_WORD == 0 { LCR_8N1 } else { settings.lcr }, settings.mcr)
			}
			None => (LCR_8N1, MCR_CONSOLE),
		};
		io.write(LCR, u32::from(lcr & !LCR_DLAB));
		io.write(IIR_FCR, u32::from(FCR_ENABLE_AND_CLEAR));
		self.fifo.store(true, Ordering::Relaxed);
		io.write(MCR, u32::from(mcr & !MCR_LOOPBACK));
		if receive {
			io.write(IER, u32::from(IER_RX_AVAILABLE));
		}
	}

	fn receive_enable(&self, io: &Io<'_, Self>, on: bool) {
		io.write(IER, if on { u32::from(IER_RX_AVAILABLE) } else { 0 });
	}

	fn lend_read(&self, io: &Io<'_, Self>) -> Settings {
		Self::read_settings(io)
	}

	// THE DRIVER'S SETTINGS BACK before the UART is: the divisor through the latch, the line, the modem control, and the
	// interrupt enables last.
	fn lend_write(&self, io: &Io<'_, Self>, lent: &Settings) {
		io.write(LCR, u32::from(LCR_DLAB));
		io.write(RBR_THR, u32::from(lent.dll));
		io.write(IER, u32::from(lent.dlm));
		io.write(LCR, u32::from(lent.lcr & !LCR_DLAB));
		io.write(MCR, u32::from(lent.mcr));
		io.write(IER, u32::from(lent.ier));
	}

	fn line(&self) -> Option<(&'static str, u32)> {
		let source = self.source.load(Ordering::Acquire);
		(source != 0).then_some(("APLIC source", source))
	}

	// THE SOURCE LET GO, AND NOTHING ELSE: disarmed at the APLIC and no longer held, so `interrupts::bind_wired` arms it
	// to the claim's own identity. `WIRED_EID` and its handler stay - the hot-plug lines arrive under the same identity.
	fn release_line(&self) {
		#[cfg(not(test))]
		{
			let (aplic, source) = (self.aplic.load(Ordering::Relaxed), self.source.load(Ordering::Acquire));
			if aplic != 0 && source != 0 {
				// SAFETY: the controller this machine's own tree places the console's line behind, inside the direct map.
				unsafe { super::aplic::disarm_source(aplic, source) };
				super::interrupts::release_source(source);
			}
		}
	}

	// AND TAKEN AGAIN: the handler on `WIRED_EID` (registering it again replaces it with itself), the source armed to
	// that identity on the hart whose file enabled it at boot, and held - and a level line asked for once more, since
	// an asserted level line raises no edge. The claim's binding was undone by its release before this runs.
	fn rearm_line(&self, handler: Handler) {
		#[cfg(not(test))]
		{
			let (aplic, source) = (self.aplic.load(Ordering::Relaxed), self.source.load(Ordering::Acquire));
			if aplic == 0 || source == 0 {
				return;
			}
			let eid = super::interrupts::WIRED_EID;
			let trigger = self.trigger.load(Ordering::Relaxed);
			super::interrupts::register(eid, handler);
			// SAFETY: as in `release_line`.
			if unsafe { super::aplic::arm_source(aplic, source, trigger, self.hart.load(Ordering::Relaxed), eid) } {
				super::interrupts::hold_source(source);
				if trigger & 0xc != 0 {
					// SAFETY: as above.
					unsafe { super::aplic::unmask_source(aplic, source, true) };
				}
			}
		}
		#[cfg(test)]
		let _ = handler;
	}
}

// THE CONSOLE.
pub static CONSOLE: Uart<Ns16550> = Uart::new(
	Ns16550 {
		base: UART_BASE,
		fifo: AtomicBool::new(false),
		source: AtomicU32::new(0),
		#[cfg(not(test))]
		aplic: AtomicU64::new(0),
		#[cfg(not(test))]
		trigger: AtomicU32::new(0),
		#[cfg(not(test))]
		hart: AtomicU64::new(0),
	},
	true,
);

// ------------------------------------------------------------------ the console's entry points

// NOTHING AT THE FIRST LINE: the firmware left the line configured, and its settings are read once the scheduler is
// up (`enable_async`).
pub fn init() {}

// THE CONSOLE UART'S RECEIVE INTERRUPT, armed on the source the device tree names for the 16550 - through the APLIC,
// delivered to this hart's interrupt file as `WIRED_EID` - and answered by `handler`, so a typed byte wakes an idle
// machine that takes no periodic tick to find it by.
//
// ONE IDENTITY FOR EVERY WIRED LINE, AND REGISTERING IT AGAIN REPLACES ITS HANDLER: a hot-plug slot's line arrives
// under the same number, so `handler` has to be the one kernel handler that answers both, and the boot tail passes the
// same function to both armings - in whichever order they run. A handoff of the UART lets the SOURCE go and never the
// identity.
//
// Answers the identity an idle core's record names it by; `Err` says why the UART's input stays polled.
#[cfg(not(test))]
pub fn arm_rx_interrupt(handler: super::interrupts::HandlerFn) -> Result<u32, &'static str> {
	let tree = super::device_tree().ok_or("the machine handed over no device tree")?;
	// THE NODE THE LINE IS READ FROM IS THE UART THIS DRIVER WRITES TO, or it is some other device's line.
	if tree.console().map(|console| console.base) != Some(UART_BASE) {
		return Err("the device tree's console is not the UART this kernel writes to");
	}
	let route = tree.console_interrupt().ok_or("the device tree names no line for it")?;
	// TWO CELLS IS THE APLIC BINDING and one a PLIC's, which this kernel does not drive - as for a slot.
	if route.cells != 2 {
		return Err("its line goes to a controller this kernel does not drive");
	}
	let (base, _size) = tree.interrupt_controller_reg(route.controller).ok_or("the device tree does not place its controller")?;
	let hart: u64 = super::percpu::this_cpu().lapic_id();
	if !super::imsic::usable() || !super::imsic::has_file(hart) {
		return Err("this hart has no interrupt file to deliver it to");
	}
	let eid: u32 = super::interrupts::WIRED_EID;
	if !super::interrupts::register(eid, handler) {
		return Err("this kernel answers as many wired lines as it carries rows for");
	}
	super::imsic::enable_eid(eid);
	let (source, trigger) = (route.spec[0], route.spec[1]);
	CONSOLE.model().record_line(base, source, trigger, hart);
	// THE UART TOLD FIRST, THEN THE SOURCE ARMED - and the UART's half taken back when the controller refuses.
	CONSOLE.arm_receive(handler, || {
		// SAFETY: `base` is where this machine's own device tree places the controller the route names, and the direct
		// map covers the device window it lies in.
		if !unsafe { super::aplic::arm_source(base, source, trigger, hart, eid) } {
			return Err("the controller did not take the source the device tree names");
		}
		Ok(())
	})?;
	super::interrupts::hold_source(source);
	Ok(eid)
}

// Switch transmit to the asynchronous ring. Called once the scheduler is up, so the timer tick and the idle loop are
// draining the ring - and the firmware's settings are read here.
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
// trap, and before a reset, a power-off or the test exit acts.
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

// Write `bytes` to the console, answering how many of them were accepted. A newline is sent as CR LF, because the far
// end is a terminal and the firmware that printed before this did the same.
pub fn write_bytes(bytes: &[u8]) -> usize {
	CONSOLE.write_bytes(bytes)
}

pub fn write_whole(bytes: &[u8]) -> bool {
	CONSOLE.write_whole(bytes)
}

// Read one input byte if available, while the kernel drives the UART.
#[cfg(not(test))]
pub fn read_byte() -> Option<u8> {
	CONSOLE.read_byte()
}

// ------------------------------------------------------------------ the handoff, by the UART's base

fn instance(base: u64) -> Option<&'static Uart<Ns16550>> {
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
