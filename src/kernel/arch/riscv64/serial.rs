// riscv64 serial console: the NS16550 UART at 0x1000_0000 on QEMU's `virt` machine.
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
// A polled transmitter, like the aarch64 PL011 beside it: wait for the holding register to empty,
// write the byte. The receive side raises its interrupt once the boot tail arms it
// (`arm_rx_interrupt`). The kernel runs in the higher half, so the MMIO is reached through the
// physical direct map (`phys_to_virt`) - which the boot stub installs before it branches here, so
// this is usable from the kernel's first line.
//
// The base is QEMU `virt`'s, stated rather than discovered, exactly as the aarch64 port states
// UART0's. The device tree carries it (`/soc/serial@10000000`, `ns16550a`) and reading it from
// there is what a second riscv64 machine would need; nothing else in this port is portable to one
// yet, and a console that lies about its own generality is worse than one that does not claim any.

use super::paging::phys_to_virt;
use core::fmt::{self, Write};

// The NS16550 on QEMU virt. Byte-wide registers, no shift.
pub(crate) const UART_BASE: u64 = 0x1000_0000;
const RBR_THR: u64 = 0x00; // receive buffer (read) / transmit holding (write)
#[cfg(not(test))]
const IER: u64 = 0x01; // interrupt enable
const LSR: u64 = 0x05; // line status
#[cfg(not(test))]
const IER_RX_AVAILABLE: u8 = 1 << 0;
#[cfg(not(test))]
const LSR_DATA_READY: u8 = 1 << 0;
const LSR_THR_EMPTY: u8 = 1 << 5;

#[inline]
fn reg(off: u64) -> *mut u8 {
	phys_to_virt(UART_BASE + off) as *mut u8
}

// Nothing to program: the firmware left the line configured, and this port only ever runs after
// firmware. Divisor and line-control setup belongs with a machine that boots the kernel cold.
pub fn init() {}

pub fn enable_async() {}

pub fn drain_tx() {}

// Transmit is synchronous here, so nothing is ever left for an idle hart to drain.
pub fn tx_pending() -> bool {
	false
}

pub fn flush_sync() {}

fn put_byte(b: u8) {
	unsafe {
		while core::ptr::read_volatile(reg(LSR)) & LSR_THR_EMPTY == 0 {
			core::hint::spin_loop();
		}
		core::ptr::write_volatile(reg(RBR_THR), b);
	}
}

// Write `bytes` to the console, returning the count written. A newline is sent as CR LF, because
// the far end is a terminal and the firmware that printed before this did the same.
pub fn write_bytes(bytes: &[u8]) -> usize {
	for &b in bytes {
		if b == b'\n' {
			put_byte(b'\r');
		}
		put_byte(b);
	}
	bytes.len()
}

// Read one input byte if available (polled).
#[cfg(not(test))]
pub fn read_byte() -> Option<u8> {
	unsafe {
		if core::ptr::read_volatile(reg(LSR)) & LSR_DATA_READY == 0 {
			return None;
		}
		Some(core::ptr::read_volatile(reg(RBR_THR)))
	}
}

// THE CONSOLE UART'S RECEIVE INTERRUPT, armed on the source the device tree names for the 16550 - through
// the APLIC, delivered to this hart's interrupt file as `WIRED_EID` - and answered by `handler`, so a typed
// byte wakes an idle machine that takes no periodic tick to find it by.
//
// ONE IDENTITY FOR EVERY WIRED LINE, AND REGISTERING IT AGAIN REPLACES ITS HANDLER: a hot-plug slot's line
// arrives under the same number, so `handler` has to be the one kernel handler that answers both, and the
// boot tail passes the same function to both armings - in whichever order they run.
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
	unsafe {
		core::ptr::write_volatile(reg(IER), IER_RX_AVAILABLE);
	}
	// SAFETY: `base` is where this machine's own device tree places the controller the route names, and
	// the direct map covers the device window it lies in.
	if !unsafe { super::aplic::arm_source(base, route.spec[0], route.spec[1], hart, eid) } {
		unsafe {
			core::ptr::write_volatile(reg(IER), 0);
		}
		return Err("the controller did not take the source the device tree names");
	}
	Ok(eid)
}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		write_bytes(s.as_bytes());
		Ok(())
	}
}
