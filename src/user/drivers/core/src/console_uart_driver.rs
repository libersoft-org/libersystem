// driver.console_uart - the kernel console's UART on the device-tree ports, driven from userspace: aarch64's PL011 and
// riscv64's 16550 in a register window.
//
// WHAT IT BINDS. The console's platform row, which the kernel publishes claimable, with the console flag, when the
// device tree names the UART the kernel writes to as its console (`/chosen/stdout-path`) - matched by the node's
// `compatible`, one rule per string: its register window, its wired line as a claimed interrupt, and the kernel
// console's TAP, because claiming that row is what took the UART from the kernel. The kernel drove the UART from its
// first line until the claim; from the claim on, no kernel path but its terminal-path writer and its sleep entry
// touches the registers, and the kernel's output - its own lines and every debug write - still goes into its transmit
// ring, which this driver empties through the tap. WHICH ENGINE drives the UART is the compatible the row carries;
// WHAT CLOCKS its baud generator, and a 16550's register spacing and width, are the row's property block's - never an
// address or a rate this driver was written with. With no clock described, the divisor the firmware set is kept.
//
// A UART THAT IS NOT THE CONSOLE IS REFUSED: a second PL011 or 16550 has no tap and no kernel console to take over,
// and its byte stream is not the console's - it waits for the serial layer that a non-console UART's publication
// needs, which no row has asked for yet.
//
// WHAT IT DOES - what `uart16550` does for COM1, over `drivers::console_uart` and either engine. It programs the UART
// whole (whatever the kernel left), then serves three things:
//   - THE TAP: the kernel's output, in order, onto the wire, and a marker line where the ring's bound dropped some;
//   - THE LINE: every received byte, handed to the consumer of its `ConsoleBytes` publication - held, bounded, while
//     nobody is attached or the consumer is behind; past the bound the bytes stay in the UART and the far end waits.
//     The line is LEVEL on both ports, so a fired line is drained before it is acknowledged;
//   - THE PUBLICATION, under the kernel console's provider name: ConsoleService attaches to it, its mirror of the
//     foreground terminal arrives as console-stream writes, and the tap is emptied before each one is written.
//
// WHEN IT GOES - killed, stopped, given up on - the release revokes the window, the line and the tap, and the kernel
// takes the UART back and says so. ACROSS A SLEEP the kernel's sleep entry lends the UART to the kernel; this driver
// takes the sleep itself and its RESUME programs the UART again whatever the sleep state.

#![no_std]
#![no_main]

extern crate alloc;

use drivers::common;
use drivers::console_uart::{self, Console, Engine};
use drivers::pl011::{self, Pl011};
use drivers::serial_port::{self, Session};
use drivers::uart::{self, Held, Uart};
use rt::*;

// THE PL011'S REGISTERS, in this process's mapping of its window: thirty-two bits each, at fixed offsets.
struct Words {
	base: u64,
}

impl pl011::Registers for Words {
	fn read(&mut self, offset: u16) -> u32 {
		// SAFETY: a register inside the window this process mapped at `base`, which is the PL011's - its length was
		// checked against the highest register before the mapping.
		unsafe { core::ptr::read_volatile((self.base + u64::from(offset)) as *const u32) }
	}

	fn write(&mut self, offset: u16, value: u32) {
		// SAFETY: as `read`.
		unsafe { core::ptr::write_volatile((self.base + u64::from(offset)) as *mut u32, value) }
	}
}

// A 16550'S REGISTERS IN A WINDOW: register `n` at `n << shift`, each access `width` bytes - the low byte being the
// register's, as a part wired for word access carries it.
struct Spaced {
	base: u64,
	shift: u32,
	width: u32,
}

impl uart::Registers for Spaced {
	fn read(&mut self, offset: u16) -> u8 {
		let at = self.base + (u64::from(offset) << self.shift);
		// SAFETY: a register inside the window this process mapped at `base`, which is the 16550's - its length was
		// checked against the highest register at this spacing before the mapping; `width` is one the description
		// allows, so the access is aligned as the part is wired.
		unsafe {
			match self.width {
				4 => core::ptr::read_volatile(at as *const u32) as u8,
				2 => core::ptr::read_volatile(at as *const u16) as u8,
				_ => core::ptr::read_volatile(at as *const u8),
			}
		}
	}

	fn write(&mut self, offset: u16, value: u8) {
		let at = self.base + (u64::from(offset) << self.shift);
		// SAFETY: as `read`.
		unsafe {
			match self.width {
				4 => core::ptr::write_volatile(at as *mut u32, u32::from(value)),
				2 => core::ptr::write_volatile(at as *mut u16, u16::from(value)),
				_ => core::ptr::write_volatile(at as *mut u8, value),
			}
		}
	}
}

// WHICH UART, as the row's `compatible` says.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
	Pl011,
	Ns16550,
}

fn kind(bind: &common::Bind) -> Option<Kind> {
	bind.info.platform.match_ids().iter().filter(|id| id.kind == MATCH_ID_COMPATIBLE).find_map(|id| match id.text() {
		b"arm,pl011" => Some(Kind::Pl011),
		b"ns16550a" | b"ns16550" => Some(Kind::Ns16550),
		_ => None,
	})
}

// A BIND THAT CANNOT BE SERVED, said in words.
fn refuse(bootstrap: u64, bind: &common::Bind, what: &[u8], code: driver_protocol::DriverFailureCode) -> ! {
	print(b"driver.console_uart: ");
	print(what);
	print(b"\n");
	common::failed(bootstrap, bind, code)
}

// THE DESCRIPTION, read through the window the claim minted. A namespace device's block is in the node channel's
// encoding, not a tree's, and is not read: such a UART keeps what the firmware set, and its layout is a 16550's own.
fn described(bootstrap: u64, bind: &common::Bind, device: u64) -> console_uart::Described {
	if bind.info.platform.source == PLATFORM_SOURCE_ACPI {
		return console_uart::Described::default();
	}
	let mut block = [0u8; MAX_DEVICE_PROPERTIES];
	let len = device_properties(device, &mut block);
	let block = if len > 0 { &block[..(len as usize).min(MAX_DEVICE_PROPERTIES)] } else { &block[..0] };
	match console_uart::describe(block) {
		Ok(described) => described,
		Err(why) => refuse(bootstrap, bind, why.as_bytes(), driver_protocol::DriverFailureCode::UnsupportedDevice),
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let unusable = driver_protocol::DriverFailureCode::ResourceUnusable;
	let unsupported = driver_protocol::DriverFailureCode::UnsupportedDevice;
	let Some(kind) = kind(&bind) else { refuse(bootstrap, &bind, b"its row names no UART this driver drives", unsupported) };
	if bind.info.platform.flags & PLATFORM_FLAG_CONSOLE == 0 {
		refuse(bootstrap, &bind, b"its row is not the kernel console's UART - a UART beside the console is not this driver's", unsupported);
	}
	// ITS THREE RESOURCES: the UART's window, its line, and the kernel console's tap.
	let Some(range) = bind.info.platform.mmio().first().copied() else { refuse(bootstrap, &bind, b"its row names no register window", unusable) };
	if resources.device == 0 || resources.line_count == 0 || resources.console_tap == 0 {
		refuse(bootstrap, &bind, b"it was not handed the UART's window, its line and the console's tap", unusable);
	}
	let described = described(bootstrap, &bind, resources.device);
	// THE WINDOW REACHES THE HIGHEST REGISTER this driver touches: the PL011's DMA control, or a 16550's line status at
	// its spacing and width.
	let reach = match kind {
		Kind::Pl011 => u64::from(pl011::DMACR) + 4,
		Kind::Ns16550 => (u64::from(uart::LSR) << described.reg_shift) + u64::from(described.reg_io_width),
	};
	if range.len < reach {
		refuse(bootstrap, &bind, b"its register window is shorter than its registers", unusable);
	}
	// SAFETY: the claim's own window, minted for this binding and handed to it; the kernel maps it uncached.
	let mapped: u64 = unsafe { syscall(SYS_DEVICE_MEMORY_MAP, resources.device, 0, 0, 0) };
	if sys_is_err(mapped) {
		refuse(bootstrap, &bind, b"the UART's register window could not be mapped", unusable);
	}
	// A LEVEL LINE IS DRAINED BEFORE IT IS ACKNOWLEDGED - see `console_uart`.
	let level = bind.info.platform.lines().first().is_some_and(|line| line.trigger == LINE_TRIGGER_LEVEL);
	let name: &'static [u8] = if kind == Kind::Pl011 { b"PL011" } else { b"16550" };
	let found = Found { name, base: range.base, clock_hz: described.clock_hz, level };
	match kind {
		Kind::Pl011 => serve(bootstrap, bind, resources, Pl011::new(Words { base: mapped }), found),
		Kind::Ns16550 => serve(bootstrap, bind, resources, Uart::new(Spaced { base: mapped, shift: described.reg_shift, width: described.reg_io_width }), found),
	}
}

// WHAT THE BIND FOUND, for the serve loop and its report: the UART's name and base, the clock its description gives,
// and whether its line is level-triggered.
struct Found {
	name: &'static [u8],
	base: u64,
	clock_hz: Option<u64>,
	level: bool,
}

fn serve<E: Engine>(bootstrap: u64, bind: common::Bind, resources: common::Resources, engine: E, found: Found) -> ! {
	let Found { name, base, clock_hz, level } = found;
	// THE DIVISOR FROM THE DESCRIBED CLOCK, or the firmware's kept when none is described. A clock that cannot make the
	// console's rate is a UART this driver would put at a rate the far end cannot read.
	let line = clock_hz.and_then(|hz| E::line(hz, console_uart::CONSOLE_BAUD));
	if clock_hz.is_some() && line.is_none() {
		refuse(bootstrap, &bind, b"its clock cannot make the console's rate", driver_protocol::DriverFailureCode::UnsupportedDevice);
	}
	let mut console = Console { uart: engine, line, irq: resources.lines[0], level, tap: resources.console_tap, held: Held::default(), dropped: 0 };
	console.program();
	// ANYTHING THE KERNEL QUEUED BEFORE THE HANDOFF goes out first - the handoff line among it.
	if console.drain_tap(&bind, bootstrap).is_err() {
		refuse(bootstrap, &bind, b"the console's tap could not be read", driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let Some((bytes, bytes_far)) = channel() else { refuse(bootstrap, &bind, b"no channel for its provider", driver_protocol::DriverFailureCode::OutOfMemory) };
	// THE RETRY FOR A CONSUMER THAT IS BEHIND: armed a tick away while received bytes wait on its full stream.
	let retry: u64 = match timer_create() {
		t if t > 0 => t as u64,
		_ => refuse(bootstrap, &bind, b"no timer for its receive retry", driver_protocol::DriverFailureCode::OutOfMemory),
	};
	let mut report = common::Bounded::<160>::new();
	report.push(b"driver.console_uart: online - ");
	report.push(name);
	report.push(b" at 0x");
	let mut digits = [0u8; 16];
	let n = hex64(base, &mut digits);
	report.push(&digits[..n]);
	report.push(b", the kernel console's UART and its tap, ");
	match clock_hz {
		Some(hz) => {
			report.push(b"its divisor from a ");
			report.decimal(hz);
			report.push(b" Hz clock");
		}
		None => report.push(b"the firmware's divisor kept - no clock described"),
	}
	// THE SLEEP IS THIS DRIVER'S OWN: the UART is programmed again at the resume, never rebound.
	common::takes_sleep();
	if !common::online_named(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::CONSOLE_BYTES, bytes_far, driver_protocol::provider::KERNEL_CONSOLE_NAME)]) {
		exit();
	}
	let mut serving = common::Serving::from_offers(&[(0, bytes)]);
	let mut session = Session::new();
	let mut buffers = serial_port::Buffers::default();
	loop {
		// A housekeeping wait while behind: a consumer that never reads again must not keep a settling scheduler from
		// settling.
		let behind = console_uart::deliver(&mut console, &mut session, &mut buffers);
		timer_set(retry, if behind { clock() + 1 } else { u64::MAX });
		match common::wait_providers_or_sleep(bootstrap, &bind, &mut serving, &[console.irq, console.tap, retry], behind) {
			// A `SUSPEND`: the step taken here, and the loop goes on after the resume - WITH THE SAME CONSUMER, whose
			// connection and receive stream outlive the sleep.
			Some(None) => {
				if !common::take_sleep_step(bootstrap, &bind, &mut console_uart::Sleep { console: &mut console, serving: &mut serving, bind: &bind, bootstrap }) {
					console.uart.quiet();
					if common::stop_requested() {
						common::finish_stop(bootstrap, &bind, 0, true);
					}
					exit();
				}
			}
			None => {
				// A PLANNED STOP LEAVES THE UART QUIET - every interrupt masked - and masters nothing; the kernel
				// programs it again when it takes it back.
				console.uart.quiet();
				if console.uart.errors() != 0 || console.held.dropped() != 0 || console.dropped != 0 {
					let mut tally = common::Bounded::<160>::new();
					tally.push(b"driver.console_uart: stopping - ");
					tally.decimal(console.uart.errors());
					tally.push(b" damaged or lost received byte(s), ");
					tally.decimal(console.held.dropped());
					tally.push(b" received while nobody read past the bound, ");
					tally.decimal(console.dropped);
					tally.push(b" byte(s) of kernel output dropped at the ring's bound\n");
					print(tally.as_bytes());
				}
				if common::stop_requested() {
					common::finish_stop(bootstrap, &bind, 0, true);
				}
				exit();
			}
			Some(Some(common::ProviderReady::Connected(_))) => session.reset(),
			Some(Some(common::ProviderReady::Consumer(index))) => {
				if !session.serve(&mut serving, index, &mut console, &bind, bootstrap, &mut buffers) {
					let token = serving.close_at(index);
					session.reset();
					if !common::disconnected(bootstrap, &bind, token) {
						exit();
					}
				}
			}
			Some(Some(common::ProviderReady::Device(0))) => console.service_line(),
			Some(Some(common::ProviderReady::Device(1))) => {
				if console.drain_tap(&bind, bootstrap).is_err() {
					// The tap was revoked: the release is under way and the manager's stop follows. Nothing more can
					// be written for the kernel, and nothing is waited on here.
					console.uart.quiet();
					exit();
				}
			}
			// The retry: the next pass offers the held bytes again.
			Some(Some(common::ProviderReady::Device(_))) => {}
		}
	}
}

fn hex64(value: u64, out: &mut [u8; 16]) -> usize {
	const DIGITS: &[u8; 16] = b"0123456789abcdef";
	let mut len = 0usize;
	let mut started = false;
	for shift in (0..16).rev().map(|nibble| nibble * 4) {
		let digit = ((value >> shift) & 0xF) as usize;
		if digit != 0 || started || shift == 0 {
			out[len] = DIGITS[digit];
			len += 1;
			started = true;
		}
	}
	len
}
