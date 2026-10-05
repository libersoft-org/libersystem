// driver.uart16550 - the kernel console's UART, driven from userspace.
//
// WHAT IT BINDS. The kernel-declared COM1 row: its eight ports, its ISA IRQ 4 as a claimed line, and the
// kernel console's TAP - because claiming that row is what took the UART from the kernel. The kernel drove
// COM1 from its first line until the claim; from the claim on, no kernel path but its terminal-path writer and
// its sleep entry touches the registers, and the kernel's output - its own lines and every debug write - still
// goes into its transmit ring, which this driver empties through the tap.
//
// WHAT IT DOES. It programs the UART whole (whatever the kernel left), then serves three things:
//   - THE TAP: the kernel's output, in order, onto the wire, and a marker line where the ring's bound dropped
//     some of it;
//   - IRQ 4: every received byte, handed to the consumer of its `ConsoleBytes` publication - held, bounded, while
//     nobody is attached or the consumer is behind, so a shell that was listening before the handoff loses no
//     keystroke to it; past the bound the bytes stay in the UART and the far end waits;
//   - THE PUBLICATION, under the kernel console's provider name: ConsoleService attaches to it, its mirror of the
//     foreground terminal arrives as console-stream writes, and the tap is emptied before each one is written,
//     so a kernel line queued before a mirror chunk reaches the wire before it.
// Transmit waits for the transmitter's interrupt when the FIFO is full, rather than spinning on the line status.
//
// WHEN IT GOES - killed, stopped, given up on - the release revokes the port range, the line and the tap, and
// the kernel takes COM1 back and says so. ACROSS A SLEEP the kernel's sleep entry lends COM1 to the kernel and
// leaves its own settings in it; this driver takes the sleep itself (`console::Sleep`) and its RESUME reprograms the
// UART whatever the sleep state, so the console is never rebound - and never changes hands - across a sleep.

#![no_std]
#![no_main]

extern crate alloc;

use drivers::common;
use rt::*;

#[cfg(target_arch = "x86_64")]
mod console {
	use drivers::common;
	use drivers::serial_port::{self, Session};
	use drivers::uart::{self, Held, Line, Registers, Uart};
	use proto::system::Error;
	use rt::*;

	// How long a write waits for the transmitter before it answers `again`, in scheduler ticks: the session's
	// own bound, as the virtio port's is.
	const TX_DRAIN_TICKS: u64 = serial_port::TX_DRAIN_TICKS;
	// The most the tap hands over in one read, and what one receive pass takes.
	const TAP_CHUNK: usize = 1024;
	const RX_CHUNK: usize = 64;

	// THE UART'S EIGHT PORTS, in this process's permission bitmap once the range is mapped.
	pub struct Ports {
		pub base: u16,
	}

	impl Registers for Ports {
		fn read(&mut self, offset: u16) -> u8 {
			rt::port::inb(self.base + offset)
		}

		fn write(&mut self, offset: u16, value: u8) {
			rt::port::outb(self.base + offset, value);
		}
	}

	pub struct Console {
		pub uart: Uart<Ports>,
		pub irq: u64,
		pub tap: u64,
		// Received bytes nobody has taken yet: while no consumer is attached, while a write is on the wire, and while
		// the consumer's stream is full.
		pub held: Held,
		// Kernel output the tap reported dropped, in total.
		pub dropped: u64,
	}

	impl Console {
		// PUT BYTES ON THE WIRE: a FIFO load whenever the holding register is empty, and the transmitter's
		// interrupt waited for when it is not - answering the manager's pings meanwhile, and keeping any byte
		// received while waiting. `Again` when the transmitter did not empty within the bound.
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
				// Looked at again with the interrupt on: a holding register that emptied in between raised it
				// already, and what it takes now need not wait for the edge.
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

		// THE LINE FIRED: acknowledged FIRST, then the interrupt identified (which clears a transmit-empty one)
		// and every received byte read and kept for the consumer.
		//
		// THE ORDER IS THE WHOLE OF IT. The line is an ISA edge and the UART's output stays high while a byte waits:
		// a byte that arrives after the receiver's last read raises a new edge, and an acknowledgement made AFTER
		// the read clears the notice of that edge - leaving the byte unread, the output high and no edge ever again.
		// Measured: `lab sh` typed "echo fir" of "echo first" and the console took nothing more for the rest of
		// the boot. Acknowledged first, a byte that lands during the read is either read by it or signals again.
		pub fn service_line(&mut self) {
			interrupt_ack(self.irq);
			let _ = self.uart.acknowledge();
			self.take_received();
		}

		// EVERY RECEIVED BYTE THE HELD BOUND HAS ROOM FOR. What does not fit stays in the FIFO, and the receive
		// interrupt goes off with it - a FIFO this cannot empty would hold the line up and raise no further edge -
		// so the UART takes no more and the far end WAITS, as it waited while this driver blocked on a full
		// stream: typed input is backpressured, never dropped. `deliver` turns it on again once the consumer has
		// taken what is held.
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
	impl serial_port::Wire for Console {
		unsafe fn write(&mut self, payload: &[u8], bind: &common::Bind, bootstrap: u64) -> Result<u32, Error> {
			self.drain_tap(bind, bootstrap)?;
			self.transmit(payload, bind, bootstrap)?;
			Ok(payload.len() as u32)
		}
	}

	// Hand what was received to the consumer when one is listening; keep it otherwise - and keep it while the
	// consumer's stream is full, serving its writes meanwhile (`Session::offer` says why). Answers whether bytes
	// wait on a consumer that has not read yet, which is what the serve loop's retry is armed for.
	pub fn deliver(console: &mut Console, session: &mut Session, buffers: &mut serial_port::Buffers) -> bool {
		while session.listening() && !console.held.is_empty() {
			if !session.offer(console.held.bytes(), buffers) {
				return true;
			}
			console.held.clear();
			// ROOM AGAIN: the receive interrupt back on, and what the bound left in the FIFO read now - no edge
			// is coming for bytes that were already there.
			console.uart.receive_interrupt(true);
			console.take_received();
		}
		false
	}

	pub fn program(uart: &mut Uart<Ports>) -> bool {
		let Some(line) = Line::new(uart::PC_CLOCK_HZ, uart::CONSOLE_BAUD) else { return false };
		uart.program(line, true);
		true
	}

	// THE SLEEP: the kernel's queued output put on the wire and the UART left quiet for the kernel's sleep entry, which
	// lends COM1 to the kernel; at the resume the UART programmed again whole - whatever the sleep state left in it,
	// power lost or not - and what the kernel queued meanwhile put out. The UART is never rebound for a sleep, so the
	// kernel's console never changes hands across one.
	pub struct Sleep<'a> {
		pub console: &'a mut Console,
		pub serving: &'a mut common::Serving,
		pub bind: &'a common::Bind,
		pub bootstrap: u64,
	}

	impl common::SleepStep for Sleep<'_> {
		fn suspend(&mut self, _request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
			let _ = self.console.drain_tap(self.bind, self.bootstrap);
			self.console.uart.quiet();
			driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
		}

		fn resume(&mut self, _lost_power: bool) -> bool {
			if !program(&mut self.console.uart) {
				return false;
			}
			let _ = self.console.drain_tap(self.bind, self.bootstrap);
			true
		}

		fn serving(&mut self) -> Option<&mut common::Serving> {
			Some(self.serving)
		}
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	serve(bootstrap, bind, resources)
}

// A BIND THAT CANNOT BE SERVED, said in words.
fn refuse(bootstrap: u64, bind: &common::Bind, what: &[u8], code: driver_protocol::DriverFailureCode) -> ! {
	print(b"driver.uart16550: ");
	print(what);
	print(b"\n");
	common::failed(bootstrap, bind, code)
}

#[cfg(target_arch = "x86_64")]
fn serve(bootstrap: u64, bind: common::Bind, resources: common::Resources) -> ! {
	use drivers::serial_port::{self, Session};
	use drivers::uart::{Held, Uart};
	let unusable = driver_protocol::DriverFailureCode::ResourceUnusable;
	// ITS THREE RESOURCES: the UART's ports, its line, and the kernel console's tap.
	let ports = bind.info.ports[0];
	if bind.info.port_count == 0 {
		refuse(bootstrap, &bind, b"its row names no ports", unusable);
	}
	if resources.port_range_count == 0 || resources.line_count == 0 || resources.console_tap == 0 {
		refuse(bootstrap, &bind, b"it was not handed the UART's ports, its line and the console's tap", unusable);
	}
	if ports.len < 8 {
		refuse(bootstrap, &bind, b"its port range is shorter than a 16550's eight registers", unusable);
	}
	if port_range_map(resources.port_ranges[0]) < 0 {
		refuse(bootstrap, &bind, b"the UART's ports could not be mapped", unusable);
	}
	let mut console = console::Console { uart: Uart::new(console::Ports { base: ports.base }), irq: resources.lines[0], tap: resources.console_tap, held: Held::default(), dropped: 0 };
	if !console::program(&mut console.uart) {
		refuse(bootstrap, &bind, b"its clock cannot make the console's rate", driver_protocol::DriverFailureCode::UnsupportedDevice);
	}
	// ANYTHING THE KERNEL QUEUED BEFORE THE HANDOFF goes out first - the handoff line among it.
	if console.drain_tap(&bind, bootstrap).is_err() {
		refuse(bootstrap, &bind, b"the console's tap could not be read", unusable);
	}
	let Some((bytes, bytes_far)) = channel() else { refuse(bootstrap, &bind, b"no channel for its provider", driver_protocol::DriverFailureCode::OutOfMemory) };
	// THE RETRY FOR A CONSUMER THAT IS BEHIND: armed a tick away while received bytes wait on its full stream.
	let retry: u64 = match timer_create() {
		t if t > 0 => t as u64,
		_ => refuse(bootstrap, &bind, b"no timer for its receive retry", driver_protocol::DriverFailureCode::OutOfMemory),
	};
	let mut digits = [0u8; 4];
	let mut report = common::Bounded::<96>::new();
	report.push(b"driver.uart16550: online - 16550 at port 0x");
	let n = hex16(ports.base, &mut digits);
	report.push(&digits[..n]);
	report.push(b", the kernel console's UART and its tap");
	// THE SLEEP IS THIS DRIVER'S OWN: the UART is programmed again at the resume, never rebound.
	common::takes_sleep();
	if !common::online_named(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::CONSOLE_BYTES, bytes_far, driver_protocol::provider::KERNEL_CONSOLE_NAME)]) {
		exit();
	}
	let mut serving = common::Serving::from_offers(&[(0, bytes)]);
	let mut session = Session::new();
	let mut buffers = serial_port::Buffers::default();
	loop {
		// A housekeeping wait while behind: a consumer that never reads again must not keep a settling scheduler
		// from settling.
		let behind = console::deliver(&mut console, &mut session, &mut buffers);
		timer_set(retry, if behind { clock() + 1 } else { u64::MAX });
		match common::wait_providers_or_sleep(bootstrap, &bind, &mut serving, &[console.irq, console.tap, retry], behind) {
			// A `SUSPEND`: the step taken here, and the loop goes on after the resume - WITH THE SAME CONSUMER. Its
			// connection and its receive stream outlive the sleep, so the session is kept: resetting it closed the stream
			// ConsoleService reads typed input from, and the console stopped answering after a wake.
			Some(None) => {
				if !common::take_sleep_step(bootstrap, &bind, &mut console::Sleep { console: &mut console, serving: &mut serving, bind: &bind, bootstrap }) {
					console.uart.quiet();
					if common::stop_requested() {
						common::finish_stop(bootstrap, &bind, 0, true);
					}
					exit();
				}
			}
			None => {
				// A PLANNED STOP LEAVES THE UART QUIET - every interrupt enable off - and masters nothing; the
				// kernel programs it again when it takes it back.
				console.uart.quiet();
				if console.uart.errors() != 0 || console.held.dropped() != 0 || console.dropped != 0 {
					let mut tally = common::Bounded::<160>::new();
					tally.push(b"driver.uart16550: stopping - ");
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
					// The tap was revoked: the release is under way and the manager's stop follows. Nothing
					// more can be written for the kernel, and nothing is waited on here.
					console.uart.quiet();
					exit();
				}
			}
			// The retry: the next pass offers the held bytes again.
			Some(Some(common::ProviderReady::Device(_))) => {}
		}
	}
}

// A MACHINE WITH NO PORT SPACE has no legacy 16550 to take: the kernel-declared console row that carries one
// is x86_64's alone. A 16550 in a register window is bound by the firmware-described item that brings its
// window.
#[cfg(not(target_arch = "x86_64"))]
fn serve(bootstrap: u64, bind: common::Bind, _resources: common::Resources) -> ! {
	refuse(bootstrap, &bind, b"this machine has no port space - a 16550 in a register window is not this driver's", driver_protocol::DriverFailureCode::UnsupportedDevice)
}

#[cfg(target_arch = "x86_64")]
fn hex16(value: u16, out: &mut [u8]) -> usize {
	const DIGITS: &[u8; 16] = b"0123456789abcdef";
	let mut len = 0usize;
	let mut started = false;
	for shift in [12u32, 8, 4, 0] {
		let digit = ((value >> shift) & 0xF) as usize;
		if digit != 0 || started || shift == 0 {
			out[len] = DIGITS[digit];
			len += 1;
			started = true;
		}
	}
	len
}
