// PL011 UART - the console on QEMU's `virt` machine (UART0 at 0x0900_0000).
//
// A minimal driver: transmit a byte (wait while the TX FIFO is full, then write
// UARTDR) and read one (if the RX FIFO is not empty), the receive side raising its
// interrupt once the boot tail arms it (`arm_rx_interrupt`). The kernel runs in the
// higher half, so the device's MMIO is reached through the physical direct map
// (`phys_to_virt`). Transmit stays synchronous: there is no async TX ring here.

use super::paging::phys_to_virt;
use core::fmt::{self, Write};

// UART0 on QEMU virt.
pub(crate) const UART_BASE: u64 = 0x0900_0000;
const UARTDR: u64 = 0x00; // data register
const UARTFR: u64 = 0x18; // flag register
#[cfg(not(test))]
const UARTIMSC: u64 = 0x38; // interrupt mask set/clear
#[cfg(not(test))]
const UARTICR: u64 = 0x44; // interrupt clear
#[cfg(not(test))]
const FR_RXFE: u32 = 1 << 4; // receive FIFO empty
const FR_TXFF: u32 = 1 << 5; // transmit FIFO full
// The two receive interrupts: RX, the FIFO holds bytes at its trigger level, and RT, bytes have sat in
// it below that level for 32 bit periods.
#[cfg(not(test))]
const INT_RX: u32 = 1 << 4;
#[cfg(not(test))]
const INT_RT: u32 = 1 << 6;

#[inline]
fn reg(off: u64) -> *mut u32 {
	phys_to_virt(UART_BASE + off) as *mut u32
}

pub fn init() {
	// QEMU's PL011 is usable out of reset (the firmware/ROM left it enabled); no
	// baud or line-control programming is needed to transmit.
}

fn put_byte(b: u8) {
	unsafe {
		while core::ptr::read_volatile(reg(UARTFR)) & FR_TXFF != 0 {
			core::hint::spin_loop();
		}
		core::ptr::write_volatile(reg(UARTDR), b as u32);
	}
}

pub fn write_bytes(bytes: &[u8]) -> usize {
	for &b in bytes {
		if b == b'\n' {
			put_byte(b'\r');
		}
		put_byte(b);
	}
	bytes.len()
}

// Read one received byte without waiting. AN EMPTY FIFO ALSO ENDS A RECEIVE TIMEOUT, which is cleared
// here: RT stays raised until it is, and the interrupt that reported it has been answered by the read
// that found nothing left.
#[cfg(not(test))]
pub fn read_byte() -> Option<u8> {
	unsafe {
		if core::ptr::read_volatile(reg(UARTFR)) & FR_RXFE != 0 {
			core::ptr::write_volatile(reg(UARTICR), INT_RT);
			return None;
		}
		Some(core::ptr::read_volatile(reg(UARTDR)) as u8)
	}
}

// THE CONSOLE UART'S RECEIVE INTERRUPT, armed on the line the device tree names for the PL011 - an SPI
// through the GIC this kernel drives - and answered by `handler`, so a typed byte wakes an idle machine
// that takes no periodic tick to find it by. Answers the INTID an idle core's record names it by; `Err`
// says why the UART's input stays polled.
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
	// REGISTERED BEFORE THE SOURCE IS ENABLED, as a slot's is: a byte typed during the boot is already
	// waiting, and the enable below would otherwise take its interrupt with no handler behind it.
	if !super::interrupts::register(intid, handler) {
		return Err("this kernel answers as many wired lines as it carries rows for");
	}
	unsafe {
		core::ptr::write_volatile(reg(UARTICR), INT_RX | INT_RT);
		core::ptr::write_volatile(reg(UARTIMSC), INT_RX | INT_RT);
	}
	super::gic::enable_spi(intid, trigger);
	Ok(intid)
}

// The interrupt / async-TX surface (used by the portable console path); these
// become real once the GIC is up. Polled transmit needs none of them.
pub fn enable_async() {}

pub fn drain_tx() {}

// Transmit is synchronous here, so nothing is ever left for an idle core to drain.
pub fn tx_pending() -> bool {
	false
}

pub fn flush_sync() {}

// THE CONSOLE HANDOFF'S SURFACE, which this port does not have yet. Only x86_64's COM1 is handed to a driver
// (its claim-scoped row is the one that carries `PLATFORM_FLAG_CONSOLE`), so no row here ever asks; the
// handoff of this port's own console UART reuses the tap and these rules when it is built, and adds the
// transmit ring this port's synchronous writer does not have. Every answer below is that refusal.
pub fn drain_sync() {}

// Nothing to make room in: writes are synchronous.
pub fn make_room() {}

pub fn write_whole(bytes: &[u8]) -> bool {
	write_bytes(bytes) == bytes.len()
}

pub fn console_hand_over(_base: u64, _row: usize, _generation: u64) -> bool {
	false
}

pub fn console_hand_back(_base: u64, _quarantined: bool) {}

pub fn console_held_by(_base: u64, _generation: u64) -> bool {
	false
}

pub fn console_attach_tap(_base: u64, _generation: u64, _tap: &alloc::sync::Arc<crate::object::console_tap::ConsoleTap>) -> bool {
	false
}

pub fn console_tap_read(_base: u64, _generation: u64, _buf: &mut [u8]) -> Option<(usize, u64, usize)> {
	None
}

pub fn console_deliver() {}

// No transmit ring, so nothing for the development request to fill.
#[cfg(liber_development)]
pub fn console_dropped() -> Option<bool> {
	None
}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		write_bytes(s.as_bytes());
		Ok(())
	}
}
