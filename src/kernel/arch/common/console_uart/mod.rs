// THE CONSOLE UART OF THE DEVICE-TREE PORTS - aarch64's PL011 and riscv64's MMIO 16550 - with an asynchronous
// transmit ring, and the handoff of the UART to a userspace driver.
//
// x86_64's COM1 (`arch::x86_64::serial`) is the model this follows rule for rule, and its notes say why each rule is
// what it is. What is here is that model with the registers taken out: the two ports' UARTs share every rule and
// none of their registers, so the rules are written once and each port supplies a `Model` - which register says
// the transmitter has room, how the receiver is read, what the boot programming writes, how a lent driver's
// settings are put back, and which interrupt line the kernel answers on.
//
// Writes never busy-wait on the UART once the scheduler is up: each byte is enqueued into a software ring and the
// writer returns at once, and the ring drains in the background - on the timer tick and on each core's idle loop.
// These ports' UARTs used to be written synchronously, byte by byte, which a handoff cannot keep: a driver that
// holds the UART is where kernel output goes, and it takes it out of a ring. Early boot (before the timer and the
// idle loop run) writes straight to the wire, so boot logs appear immediately; `enable_async` flips to the ring.
//
// ONE OWNER AT A TIME, AND THE HANDOFF IS THE CLAIM. The UART belongs to the KERNEL from its first line until the
// console's platform row is claimed; the claim flips the owner to DRIVER - under the lock that serialises the
// ring, before the claim's register window, line and tap are minted - and the kernel's receive line is let go. From
// then on no kernel path but the terminal-path writer and the sleep entry touches the registers. Kernel output keeps
// going into the ring, and the claim's CONSOLE TAP moves it out, in order, to the driver that writes it. The release
// hands the UART back: the kernel reads out what the receiver holds, programs the UART again, arms its receive line
// again and drains what queued meanwhile. The TERMINAL owner is the terminal-path writer's and is never left; SLEEP
// is the sleep entry's loan of a driver's UART to the kernel for the window around a sleep.
//
// EVERY ACCESS TO THE REGISTERS GOES THROUGH `read` AND `write`, and those count - and refuse - any access an ordinary
// kernel path makes while a driver holds the UART. Every path checks the owner before it reaches for the registers,
// so the count is zero by construction; it is the backstop the handoff gate reads.
//
// THE KERNEL'S SETTINGS ARE THE FIRMWARE'S. These ports never program a divisor at boot - the firmware left the line
// running at its rate and the kernel's first line goes out on it - so what the kernel writes back at a reacquisition,
// on the terminal path and after a sleep that lost them is what the firmware left, read once the scheduler is up
// (`keep_firmware_settings`) and never earlier: the first lines run before the floating-point unit is on, and a
// settings record is exactly the copy a compiler does with vector registers.
//
// THE CODE IS AN INSTANCE OF A MODEL: each port's console is one, and the suite runs the same rules over a scripted
// UART (`tests`), whose every register access is recorded - on every target, x86_64's among them.

use crate::object::console_tap::ConsoleTap;
use crate::sync::SpinLock;
use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

#[cfg(test)]
mod tests;

// The software transmit ring, as COM1's: 16 kB buffers a screenful of console mirror, and on the rare overflow a
// kernel-owned UART drains synchronously to make room; a byte that still does not fit is dropped and COUNTED.
pub const TX_RING_CAP: usize = 16384;

// The deepest receive FIFO either UART has: what the reacquisition reads out, at most.
pub const RX_READ_OUT: usize = 64;

// How long the terminal-path writer waits for the ring's lock before proceeding without it, and how long any
// synchronous write polls the transmitter before writing anyway, in polls.
const TERMINAL_LOCK_SPINS: u32 = 10_000_000;
pub const TX_POLLS: u32 = 1_000_000;
// A PLANNED END'S WAIT FOR THE DRIVER (`Uart::settle_driver`), as COM1's: the ring empty this long, and never longer
// than the bound in all.
pub const SETTLE_QUIET_NS: u64 = 100_000_000;
pub const SETTLE_BOUND_NS: u64 = 1_000_000_000;

// What answers the kernel's own receive line: the one wired handler the boot tail passes.
pub type Handler = fn(u32);

struct TxRing {
	buf: [u8; TX_RING_CAP],
	head: usize, // next index to fill (producer)
	tail: usize, // next index to drain (consumer)
	len: usize,  // bytes currently queued
}

impl TxRing {
	const fn new() -> Self {
		Self { buf: [0u8; TX_RING_CAP], head: 0, tail: 0, len: 0 }
	}

	fn push(&mut self, byte: u8) {
		self.buf[self.head] = byte;
		self.head = (self.head + 1) % TX_RING_CAP;
		self.len += 1;
	}

	fn pop(&mut self) -> u8 {
		let byte: u8 = self.buf[self.tail];
		self.tail = (self.tail + 1) % TX_RING_CAP;
		self.len -= 1;
		byte
	}
}

// WHO DRIVES THE UART. See the module's note.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Owner {
	Kernel,
	// A driver, through the claim of this generation.
	Driver(u64),
	// The sleep entry's loan of a driver's UART to the kernel, for the claim of this generation.
	Sleep(u64),
	// The terminal-path writer's, never left.
	Terminal,
}

impl Owner {
	// The claim that holds the UART, whether it drives it or has lent it for a sleep.
	fn claim(self) -> Option<u64> {
		match self {
			Owner::Driver(generation) => Some(generation),
			Owner::Sleep(generation) => Some(generation),
			Owner::Kernel | Owner::Terminal => None,
		}
	}
}

// Which path an access is made by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Path {
	// Every ordinary kernel path: the drains, the receive handler and polls, the kernel's own writes.
	Kernel,
	Terminal,
	Sleep,
}

// THE ACCESS PATH, as a model's register sequences see it: every read and write it makes goes through the owner
// check and the stray count, for the path the sequence runs on.
pub struct Io<'a, M: Model> {
	uart: &'a Uart<M>,
	path: Path,
}

impl<M: Model> Io<'_, M> {
	pub fn read(&self, offset: u16) -> u32 {
		self.uart.read(self.path, offset)
	}

	pub fn write(&self, offset: u16, value: u32) {
		self.uart.write(self.path, offset, value);
	}
}

// WHAT ONE UART HAS ITS OWN WORDS FOR. Every register sequence is written against an `Io`, never against the raw
// accessors, so no model can reach its registers past the owner check.
pub trait Model: Sync + Sized + 'static {
	// How the console's lines name it: "the PL011", "the 16550".
	const NAME: &'static str;
	// The settings the kernel keeps: the firmware's, read once, and a lent driver's, read at a sleep's entry.
	type Settings: Copy + Send;
	// Where its registers are, physically: what a claim names it by.
	fn base(&self) -> u64;
	// One register, raw. Reached only through `Uart::read` and `Uart::write`, which admit the access first.
	fn raw_read(&self, offset: u16) -> u32;
	fn raw_write(&self, offset: u16, value: u32);
	// How many bytes the transmitter takes now, without another look at its status: none, one, or a FIFO's load.
	fn tx_room(&self, io: &Io<'_, Self>) -> usize;
	fn tx_put(&self, io: &Io<'_, Self>, byte: u8);
	// Whether everything written has left the wire.
	fn tx_idle(&self, io: &Io<'_, Self>) -> bool;
	// One received byte, or `None` when the receiver holds none - clearing what an empty receiver leaves raised.
	fn rx_take(&self, io: &Io<'_, Self>) -> Option<u8>;
	// The receiver made readable at a reacquisition, whatever a driver left in the way of it, before the read-out.
	fn rx_prepare(&self, io: &Io<'_, Self>, firmware: Option<&Self::Settings>);
	// The firmware's settings, read - and nothing the UART does changed.
	fn read_firmware(&self, io: &Io<'_, Self>) -> Self::Settings;
	// THE BOOT PROGRAMMING, whole: every register a driver could have left in any state written - the firmware's
	// settings where they were read, the receive interrupt last and only when asked.
	fn boot_init(&self, io: &Io<'_, Self>, firmware: Option<&Self::Settings>, receive: bool);
	// The UART told to raise the kernel's receive interrupt, or not to.
	fn receive_enable(&self, io: &Io<'_, Self>, on: bool);
	// A driver's settings, read as a sleep's entry borrows its UART, and written back as the loan ends.
	fn lend_read(&self, io: &Io<'_, Self>) -> Self::Settings;
	fn lend_write(&self, io: &Io<'_, Self>, lent: &Self::Settings);
	// The kernel's receive line, as the handoff lines name it - `None` while none is armed.
	fn line(&self) -> Option<(&'static str, u32)>;
	// The kernel's receive line let go at its controller, so the claim can bind it - run under the ring's lock, with
	// interrupts masked - and taken again at the reacquisition, answered by `handler`, without the lock. Neither
	// touches a UART register, and neither takes a lock of its own.
	fn release_line(&self);
	fn rearm_line(&self, handler: Handler);
}

struct Inner<S> {
	ring: TxRing,
	owner: Owner,
	// Bytes the ring's bound dropped since they were last reported - by the tap to its driver, or by the kernel in a
	// line of its own.
	dropped: u64,
	// The claim's tap, while a driver holds the UART.
	tap: Option<Weak<ConsoleTap>>,
	// The ring went from empty to holding bytes while a driver holds the UART, and the tap has not been told yet.
	// Told from a context that holds no other lock - see `deliver_tap_signal`.
	signal_due: bool,
	// A sleep window on a kernel-owned UART: every line goes to the wire synchronously until it closes.
	window: bool,
	// The UART lost its settings in the sleep that just ended; the next line programs it again first.
	lost: bool,
	// The firmware's settings, once read.
	firmware: Option<S>,
	// THE DRIVER'S SETTINGS WHILE ITS UART IS LENT to a sleep's entry, written back as the loan ends.
	lent: Option<S>,
}

pub struct Uart<M: Model> {
	model: M,
	// Whether this is the console: its read-out feeds the console input, and it prints the handoff lines.
	console: bool,
	inner: SpinLock<Inner<M::Settings>>,
	// Accesses an ordinary kernel path made while a driver held the UART - refused, and counted.
	stray: AtomicU64,
	// Whether a driver holds the UART now: the owner as the access path reads it, lock-free.
	driving: AtomicBool,
	// The receive handler the kernel armed, armed again at a reacquisition (0: none armed).
	handler: AtomicUsize,
	// Whether the kernel enables the receive interrupt when it drives this UART.
	receive: AtomicBool,
	// A terminal-path writer has taken the UART: every line goes to the wire synchronously, without the lock.
	terminal: AtomicBool,
	// False during early boot (synchronous writes), flipped true once the timer and the idle loop service the ring.
	// Monotonic: never flipped back.
	asynchronous: AtomicBool,
	// Bytes the reacquisition read out, kept by an instance that is not the console.
	#[cfg(test)]
	read_out: SpinLock<([u8; RX_READ_OUT], usize)>,
}

impl<M: Model> Uart<M> {
	pub const fn new(model: M, console: bool) -> Self {
		Self {
			model,
			console,
			inner: SpinLock::new(Inner { ring: TxRing::new(), owner: Owner::Kernel, dropped: 0, tap: None, signal_due: false, window: false, lost: false, firmware: None, lent: None }),
			stray: AtomicU64::new(0),
			driving: AtomicBool::new(false),
			handler: AtomicUsize::new(0),
			receive: AtomicBool::new(false),
			terminal: AtomicBool::new(false),
			asynchronous: AtomicBool::new(false),
			#[cfg(test)]
			read_out: SpinLock::new(([0; RX_READ_OUT], 0)),
		}
	}

	pub fn model(&self) -> &M {
		&self.model
	}

	pub fn base(&self) -> u64 {
		self.model.base()
	}

	// ------------------------------------------------------------------ the one access path

	fn io(&self, path: Path) -> Io<'_, M> {
		Io { uart: self, path }
	}

	// Whether `path` may reach the registers now. An ordinary kernel path while a driver holds the UART may not, and
	// is counted. THE OWNER IS READ HERE, from the mirror every change of owner writes - not taken from the caller.
	fn admitted(&self, path: Path) -> bool {
		if self.driving.load(Ordering::Acquire) && path == Path::Kernel {
			self.stray.fetch_add(1, Ordering::Relaxed);
			return false;
		}
		true
	}

	// EVERY CHANGE OF OWNER, under the ring's lock, and the mirror the access path reads with it.
	fn set_owner(&self, inner: &mut Inner<M::Settings>, owner: Owner) {
		inner.owner = owner;
		self.driving.store(matches!(owner, Owner::Driver(_)), Ordering::Release);
	}

	fn read(&self, path: Path, offset: u16) -> u32 {
		if !self.admitted(path) {
			return 0;
		}
		self.model.raw_read(offset)
	}

	fn write(&self, path: Path, offset: u16, value: u32) {
		if !self.admitted(path) {
			return;
		}
		self.model.raw_write(offset, value);
	}

	// Write one byte to the wire, polling the transmitter first - bounded, so a UART that never takes a byte cannot
	// hang a terminal path.
	fn put_sync(&self, path: Path, byte: u8) {
		let io = self.io(path);
		let mut polls = 0u32;
		while self.model.tx_room(&io) == 0 && polls < TX_POLLS {
			core::hint::spin_loop();
			polls += 1;
		}
		self.model.tx_put(&io, byte);
	}

	// Poll until the transmitter has sent everything - bounded.
	fn wait_idle(&self, path: Path) {
		let io = self.io(path);
		let mut polls = 0u32;
		while !self.model.tx_idle(&io) && polls < TX_POLLS {
			core::hint::spin_loop();
			polls += 1;
		}
	}

	fn boot_init(&self, path: Path, firmware: Option<&M::Settings>, receive: bool) {
		self.model.boot_init(&self.io(path), firmware, receive);
	}

	// THE FIRMWARE'S SETTINGS, read once the scheduler is up - see the module's note. Reads, and whatever the model
	// needs to make them; nothing the UART does is changed.
	pub fn keep_firmware_settings(&self) {
		let mut inner = self.inner.lock();
		if inner.owner != Owner::Kernel || inner.firmware.is_some() {
			return;
		}
		inner.firmware = Some(self.model.read_firmware(&self.io(Path::Kernel)));
	}

	// ------------------------------------------------------------------ the ring

	// Push as much of the ring to the UART as the transmitter takes right now, then return. Non-blocking, and the
	// kernel's alone: a driver drains the ring through the tap. The caller holds the lock.
	fn drain_locked(&self, inner: &mut Inner<M::Settings>) {
		if inner.owner != Owner::Kernel {
			return;
		}
		let io = self.io(Path::Kernel);
		while inner.ring.len != 0 {
			let room = self.model.tx_room(&io);
			if room == 0 {
				return;
			}
			for _ in 0..room.min(inner.ring.len) {
				let byte: u8 = inner.ring.pop();
				self.model.tx_put(&io, byte);
			}
		}
	}

	// Queue one byte for a driver's tap, noting that the tap is owed a signal when the ring was empty.
	fn push_for_tap(inner: &mut Inner<M::Settings>, byte: u8) {
		if inner.ring.len == 0 {
			inner.signal_due = true;
		}
		// ALLOC-OK: a fixed-capacity TX ring, not a heap collection - every caller checked the capacity.
		inner.ring.push(byte);
	}

	// A line after a sleep that lost the UART's settings: the boot programming first, before any byte, with the
	// receive interrupt as the owner wants it.
	fn restore_after_sleep(&self, inner: &mut Inner<M::Settings>) {
		if !inner.lost {
			return;
		}
		inner.lost = false;
		let firmware = inner.firmware;
		match inner.owner {
			Owner::Kernel => self.boot_init(Path::Sleep, firmware.as_ref(), self.receive.load(Ordering::Relaxed)),
			Owner::Sleep(_) => self.boot_init(Path::Sleep, firmware.as_ref(), false),
			Owner::Driver(_) | Owner::Terminal => {}
		}
	}

	// Switch transmit to the ring: the timer tick and the idle loop service it from now on.
	pub fn enable_async(&self) {
		self.asynchronous.store(true, Ordering::Release);
	}

	// ONE BYTE OF THE KERNEL'S OWN OUTPUT - lossless while the kernel drives the UART, queued for the tap while a
	// driver does, synchronous in a sleep window and on the terminal path.
	pub fn write_byte(&self, byte: u8) {
		if self.terminal.load(Ordering::Acquire) {
			self.put_sync(Path::Terminal, byte);
			return;
		}
		let mut inner = self.inner.lock();
		match inner.owner {
			Owner::Terminal => self.put_sync(Path::Terminal, byte),
			Owner::Sleep(_) => {
				self.restore_after_sleep(&mut inner);
				self.put_sync(Path::Sleep, byte);
			}
			Owner::Kernel if inner.window => {
				self.restore_after_sleep(&mut inner);
				self.put_sync(Path::Sleep, byte);
			}
			Owner::Kernel if !self.asynchronous.load(Ordering::Acquire) => {
				// Early boot: straight to the wire so logs appear immediately.
				self.put_sync(Path::Kernel, byte);
			}
			Owner::Kernel => {
				if inner.ring.len == TX_RING_CAP {
					// Ring full: push out whatever the transmitter takes right now to make room, then enqueue - and
					// drop this byte, counted, when it takes nothing, rather than wait with the lock held.
					self.drain_locked(&mut inner);
					if inner.ring.len == TX_RING_CAP {
						inner.dropped += 1;
						return;
					}
				}
				// ALLOC-OK: a fixed-capacity TX ring, not a heap collection - the capacity is checked above.
				inner.ring.push(byte);
			}
			Owner::Driver(_) => {
				// THE DRIVER DRAINS THE RING THROUGH THE TAP, and this path may not touch the UART to make room: a
				// full ring drops the byte and counts it, and the tap reports the count.
				if inner.ring.len == TX_RING_CAP {
					inner.dropped += 1;
					return;
				}
				Self::push_for_tap(&mut inner, byte);
			}
		}
	}

	// Best-effort bulk enqueue for the SYS_DEBUG_WRITE path: as much as fits (translating \n to \r\n), under one
	// lock, answering how many of the SOURCE bytes were fully accepted - the caller keeps the rest and may offer it
	// again. Early boot, a sleep window and the terminal path write to the wire and accept everything.
	pub fn write_bytes(&self, bytes: &[u8]) -> usize {
		let synchronous = self.terminal.load(Ordering::Acquire) || {
			let inner = self.inner.lock();
			match inner.owner {
				Owner::Terminal => true,
				Owner::Sleep(_) => true,
				Owner::Kernel => inner.window || !self.asynchronous.load(Ordering::Acquire),
				Owner::Driver(_) => false,
			}
		};
		if synchronous {
			for &byte in bytes {
				if byte == b'\n' {
					self.write_byte(b'\r');
				}
				self.write_byte(byte);
			}
			return bytes.len();
		}
		let mut inner = self.inner.lock();
		let for_tap = matches!(inner.owner, Owner::Driver(_));
		for (i, &byte) in bytes.iter().enumerate() {
			let need: usize = if byte == b'\n' { 2 } else { 1 };
			if TX_RING_CAP - inner.ring.len < need {
				return i;
			}
			if byte == b'\n' {
				if for_tap {
					Self::push_for_tap(&mut inner, b'\r');
				} else {
					// ALLOC-OK: a fixed-capacity TX ring - the capacity is checked above.
					inner.ring.push(b'\r');
				}
			}
			if for_tap {
				Self::push_for_tap(&mut inner, byte);
			} else {
				// ALLOC-OK: a fixed-capacity TX ring - the capacity is checked above.
				inner.ring.push(byte);
			}
		}
		bytes.len()
	}

	// Whether the ring would take all of `bytes` (with its \r\n translation) now, and if so take them.
	pub fn write_whole(&self, bytes: &[u8]) -> bool {
		let need: usize = bytes.len() + bytes.iter().filter(|&&byte| byte == b'\n').count();
		{
			let inner = self.inner.lock();
			let queued = matches!(inner.owner, Owner::Kernel | Owner::Driver(_)) && !inner.window && self.asynchronous.load(Ordering::Acquire) && !self.terminal.load(Ordering::Acquire);
			if queued && TX_RING_CAP - inner.ring.len < need {
				return false;
			}
		}
		self.write_bytes(bytes) == bytes.len()
	}

	// Background drain: from the timer tick and from each core's idle loop. `try_lock` so it never spins in an
	// interrupt handler when another core holds the ring. While a driver holds the UART it drains nothing and
	// delivers the tap's signal instead.
	pub fn drain_tx(&self) {
		if let Some(mut inner) = self.inner.try_lock() {
			self.drain_locked(&mut inner);
		}
		self.deliver_tap_signal();
	}

	// DRAIN THE WHOLE RING SYNCHRONOUSLY while the kernel drives the UART - for a caller that must see the ring empty
	// before it goes on, and that is not ending the machine. Each wait is bounded, so a transmitter that never empties
	// costs the caller time and not the machine. While a driver holds the UART the driver drains it, and this returns.
	pub fn drain_sync(&self) {
		let mut inner = self.inner.lock();
		if inner.owner != Owner::Kernel {
			return;
		}
		let io = self.io(Path::Kernel);
		while inner.ring.len != 0 {
			let mut polls = 0u32;
			while self.model.tx_room(&io) == 0 && polls < TX_POLLS {
				core::hint::spin_loop();
				polls += 1;
			}
			if polls == TX_POLLS {
				return;
			}
			self.drain_locked(&mut inner);
		}
		let mut polls = 0u32;
		while !self.model.tx_idle(&io) && polls < TX_POLLS {
			core::hint::spin_loop();
			polls += 1;
		}
	}

	// MAKE ROOM IN THE RING for a writer that may not lose a byte: drained here while the kernel drives the UART, and
	// by the driver - which this core lets run - while one does. The caller holds no lock.
	pub fn make_room(&self) {
		if matches!(self.inner.lock().owner, Owner::Driver(_)) {
			self.deliver_tap_signal();
			crate::sched::yield_now();
		} else {
			self.drain_sync();
		}
	}

	// WHETHER THE RING HOLDS BYTES THE KERNEL STILL HAS TO MOVE: an idle core then sleeps one tick at most - to drain
	// them while the kernel drives the UART, or to tell a driver's tap they are there.
	pub fn tx_pending(&self) -> bool {
		if !self.asynchronous.load(Ordering::Acquire) {
			return false;
		}
		match self.inner.try_lock() {
			None => true,
			Some(inner) => match inner.owner {
				Owner::Kernel => inner.ring.len != 0,
				Owner::Driver(_) => inner.signal_due,
				Owner::Terminal => false,
				Owner::Sleep(_) => false,
			},
		}
	}

	// ------------------------------------------------------------------ receive

	// Read one received byte without waiting, while the kernel drives the UART; `None` while a driver does - the
	// kernel feeds no byte of a driver's UART to the console.
	pub fn read_byte(&self) -> Option<u8> {
		let inner = self.inner.lock();
		if inner.owner != Owner::Kernel {
			return None;
		}
		self.model.rx_take(&self.io(Path::Kernel))
	}

	// THE KERNEL'S RECEIVE INTERRUPT, ARMED - only while the kernel drives the UART: the UART told to raise it, then
	// `controller` run to take the line at its controller, whose refusal takes the UART's half back. The handler is
	// kept, so a reacquisition after a driver held the UART arms it again.
	pub fn arm_receive(&self, handler: Handler, controller: impl FnOnce() -> Result<(), &'static str>) -> Result<(), &'static str> {
		let inner = self.inner.lock();
		if inner.owner != Owner::Kernel {
			return Err("a driver holds the console UART");
		}
		let io = self.io(Path::Kernel);
		self.model.receive_enable(&io, true);
		if let Err(why) = controller() {
			self.model.receive_enable(&io, false);
			return Err(why);
		}
		self.handler.store(handler as usize, Ordering::Release);
		self.receive.store(true, Ordering::Relaxed);
		drop(inner);
		Ok(())
	}

	// ------------------------------------------------------------------ the handoff

	// THE CLAIM TAKES THE UART: the owner flips to the driver under the ring's lock FIRST, then the kernel's receive
	// line is let go - and only after this returns are the claim's resources minted. Refused unless the kernel drives
	// it now (a terminal path took it, or another claim holds it).
	//
	// THE LINE IS LET GO UNDER THE LOCK, with this core's interrupts masked, and that is not tidiness. A PL011's line is
	// LEVEL: a byte that lands between the flip and the release holds it up, the kernel's handler may no longer read
	// the byte that would lower it, and an SPI routed to this very core would be taken again at every EOI - the core
	// never reaching the release that stops it. Masked, the release is done before the core can take it once.
	pub fn hand_over(&self, generation: u64, row: usize) -> bool {
		let armed = self.handler.load(Ordering::Acquire) != 0;
		{
			let mut inner = self.inner.lock();
			if inner.owner != Owner::Kernel {
				return false;
			}
			self.set_owner(&mut inner, Owner::Driver(generation));
			inner.signal_due = inner.ring.len != 0;
			if armed {
				self.model.release_line();
			}
		}
		if self.console {
			let handler = if armed { ", the kernel's receive handler taken off its line" } else { "" };
			match self.model.line() {
				Some((kind, number)) => crate::serial_println!("console: {} at {:#x} ({kind} {number}) is handed to row {row}, claim generation {generation}{handler} - kernel output now leaves through the claim's console tap", M::NAME, self.base()),
				None => crate::serial_println!("console: {} at {:#x} is handed to row {row}, claim generation {generation} - kernel output now leaves through the claim's console tap", M::NAME, self.base()),
			}
		}
		true
	}

	// Whether the claim of `generation` holds the UART now - what the window's mint and the tap ask.
	pub fn held_by(&self, generation: u64) -> bool {
		self.inner.lock().owner.claim() == Some(generation)
	}

	// THE CLAIM'S TAP IS ATTACHED: signalled when the ring goes from empty to holding bytes, and at once if it holds
	// bytes already.
	pub fn attach_tap(&self, generation: u64, tap: &Arc<ConsoleTap>) -> bool {
		{
			let mut inner = self.inner.lock();
			if !matches!(inner.owner, Owner::Driver(held) if held == generation) {
				return false;
			}
			inner.tap = Some(Arc::downgrade(tap));
			inner.signal_due = inner.ring.len != 0;
		}
		self.deliver_tap_signal();
		true
	}

	// MOVE BYTES OUT OF THE RING for the claim of `generation`, in order, and answer how many, how many the bound
	// dropped since the last read, and how many are left. `None` when that claim no longer holds the UART.
	pub fn tap_read(&self, generation: u64, buf: &mut [u8]) -> Option<(usize, u64, usize)> {
		let mut inner = self.inner.lock();
		if !matches!(inner.owner, Owner::Driver(held) if held == generation) {
			return None;
		}
		let n = inner.ring.len.min(buf.len());
		for slot in buf.iter_mut().take(n) {
			*slot = inner.ring.pop();
		}
		let dropped = core::mem::take(&mut inner.dropped);
		if inner.ring.len == 0 {
			inner.signal_due = false;
		}
		Some((n, dropped, inner.ring.len))
	}

	// TELL THE TAP ITS RING HOLDS BYTES, from a context that holds no other lock - the timer tick, the idle loop, the
	// debug-write syscall - since waking its reader takes the scheduler's locks.
	pub fn deliver_tap_signal(&self) {
		let tap = {
			let Some(mut inner) = self.inner.try_lock() else { return };
			if !inner.signal_due || !matches!(inner.owner, Owner::Driver(_)) {
				return;
			}
			inner.signal_due = false;
			inner.tap.clone()
		};
		if let Some(tap) = tap.and_then(|weak| weak.upgrade()) {
			tap.signal();
		}
	}

	// THE RELEASE HANDS THE UART BACK. The claim's window, line and tap are revoked by then. The kernel takes the
	// ring's lock, becomes the owner, and READS OUT WHAT THE RECEIVER HOLDS BEFORE ANYTHING ELSE - at most a FIFO's
	// depth - because the boot programming that follows empties the receiver. Then the boot programming, the receive
	// line armed again, the bytes read out fed to the console, and one line - with the stray-access count in the
	// development and test builds, and the bound's drops when there were any. A quarantined release returns the UART
	// all the same: the kernel is not a claimant, and a stranded holder can at worst interleave bytes with it.
	pub fn hand_back(&self, quarantined: bool) {
		let mut bytes = [0u8; RX_READ_OUT];
		let mut count = 0usize;
		let dropped;
		let receive = self.receive.load(Ordering::Relaxed);
		{
			let mut inner = self.inner.lock();
			if inner.owner.claim().is_none() {
				return;
			}
			self.set_owner(&mut inner, Owner::Kernel);
			inner.tap = None;
			inner.signal_due = false;
			let firmware = inner.firmware;
			let io = self.io(Path::Kernel);
			self.model.rx_prepare(&io, firmware.as_ref());
			while count < RX_READ_OUT {
				let Some(byte) = self.model.rx_take(&io) else { break };
				bytes[count] = byte;
				count += 1;
			}
			self.model.boot_init(&io, firmware.as_ref(), receive);
			dropped = core::mem::take(&mut inner.dropped);
		}
		let handler = self.handler.load(Ordering::Acquire);
		if handler != 0 {
			// SAFETY: `handler` was stored from a `Handler` by `arm_receive` and nothing else writes it.
			self.model.rearm_line(unsafe { core::mem::transmute::<usize, Handler>(handler) });
		}
		let stray = self.stray.swap(0, Ordering::Relaxed);
		if self.console {
			for &byte in &bytes[..count] {
				crate::console_input::feed_serial(byte);
			}
			let how = if quarantined { " - its release was not confirmed, and the row is not claimed again this boot" } else { "" };
			if cfg!(liber_development) || cfg!(test) {
				crate::serial_println!("console: {} at {:#x} is the kernel's again{how} - {stray} kernel access(es) to its registers while the driver held it, {count} byte(s) read out of its receiver", M::NAME, self.base());
			} else {
				crate::serial_println!("console: {} at {:#x} is the kernel's again{how}", M::NAME, self.base());
			}
			if dropped != 0 {
				crate::serial_println!("console: {dropped} byte(s) of kernel output were dropped at the ring's bound while the driver held {} at {:#x}", M::NAME, self.base());
			}
		}
		#[cfg(test)]
		if !self.console {
			let mut kept = self.read_out.lock();
			kept.0[..count].copy_from_slice(&bytes[..count]);
			kept.1 = count;
		}
	}

	// ------------------------------------------------------------------ the terminal path

	// THE TERMINAL-PATH WRITER, entered BEFORE anything is printed on a path that ends the machine - see COM1's. It
	// takes the ring's lock with a bounded wait and past the bound proceeds without it; runs the UART's whole boot
	// programming - a driver may have left it disabled, another divisor, its FIFOs off or loopback on; writes out the
	// ring's backlog by polling, then the count the bound dropped when it is not zero; and sets the owner to TERMINAL,
	// which is never left. NO `core::fmt` ON THIS PATH: aarch64's bad-stack reporter enters it on a small stack of
	// its own, and the count is written digit by digit.
	pub fn terminal(&self) {
		if self.terminal.load(Ordering::Acquire) {
			// Entered again on the same path: what was written is let out before the machine stops.
			self.wait_idle(Path::Terminal);
			return;
		}
		let mut spins = 0u32;
		let mut guard = loop {
			if let Some(guard) = self.inner.try_lock() {
				break Some(guard);
			}
			if spins >= TERMINAL_LOCK_SPINS {
				break None;
			}
			core::hint::spin_loop();
			spins += 1;
		};
		let inner: &mut Inner<M::Settings> = match guard.as_mut() {
			Some(guard) => guard,
			// SAFETY: past the bound the lock's holder is not coming back in time - it may be this very core,
			// interrupted inside the ring - and the machine is ending: the ring is read without it.
			None => unsafe { self.inner.get_unlocked() },
		};
		// The kernel's own bytes already in the FIFO go out before the programming empties it.
		if inner.owner == Owner::Kernel {
			self.wait_idle(Path::Terminal);
		}
		let firmware = inner.firmware;
		self.boot_init(Path::Terminal, firmware.as_ref(), false);
		while inner.ring.len != 0 {
			let byte = inner.ring.pop();
			self.put_sync(Path::Terminal, byte);
		}
		let dropped = core::mem::take(&mut inner.dropped);
		self.set_owner(inner, Owner::Terminal);
		inner.tap = None;
		inner.signal_due = false;
		self.terminal.store(true, Ordering::Release);
		drop(guard);
		if dropped != 0 {
			self.put_all(Path::Terminal, b"\r\nconsole: ");
			let mut digits = [0u8; 20];
			let mut len = 0usize;
			let mut value = dropped;
			loop {
				digits[len] = b'0' + (value % 10) as u8;
				len += 1;
				value /= 10;
				if value == 0 {
					break;
				}
			}
			digits[..len].reverse();
			self.put_all(Path::Terminal, &digits[..len]);
			self.put_all(Path::Terminal, b" byte(s) of kernel output were dropped at the ring's bound before this point\r\n");
		}
		self.wait_idle(Path::Terminal);
	}

	fn put_all(&self, path: Path, bytes: &[u8]) {
		for &byte in bytes {
			self.put_sync(path, byte);
		}
	}

	// ------------------------------------------------------------------ the sleep entry

	// THE SLEEP ENTRY IS NOT A TERMINAL PATH, because the machine comes back - see COM1's. KERNEL: it drains the ring,
	// and every line until `sleep_end` goes to the wire synchronously. DRIVER: every binding has answered SUSPENDED
	// before the entry, and the entry LENDS THE UART TO THE KERNEL FOR THE SLEEP: the owner becomes SLEEP, the driver's
	// settings are read, the boot programming runs with the receive interrupt left off and the ring is drained. The
	// claim, its window, its line and its tap are untouched throughout, and nothing the entry does is counted against
	// the driver's hold, since the owner is not DRIVER while it is done.
	pub fn sleep_begin(&self) {
		let mut inner = self.inner.lock();
		match inner.owner {
			Owner::Kernel => {
				inner.window = true;
				while inner.ring.len != 0 {
					let byte = inner.ring.pop();
					self.put_sync(Path::Sleep, byte);
				}
			}
			Owner::Driver(generation) => {
				self.set_owner(&mut inner, Owner::Sleep(generation));
				let io = self.io(Path::Sleep);
				let lent = self.model.lend_read(&io);
				let firmware = inner.firmware;
				self.model.boot_init(&io, firmware.as_ref(), false);
				inner.lent = Some(lent);
				while inner.ring.len != 0 {
					let byte = inner.ring.pop();
					self.put_sync(Path::Sleep, byte);
				}
				inner.signal_due = false;
			}
			Owner::Sleep(_) | Owner::Terminal => {}
		}
	}

	// THE WAKE, before any line after it: whether the UART lost its settings. When it did, the boot programming runs
	// again before the next line's first byte - with the receive interrupt while the kernel owns the UART and without it
	// while it is lent.
	pub fn sleep_wake(&self, lost_settings: bool) {
		let mut inner = self.inner.lock();
		if lost_settings && matches!(inner.owner, Owner::Kernel | Owner::Sleep(_)) {
			inner.lost = true;
		}
	}

	// THE ENTRY'S LAST ACT: the window closes, and a lent UART goes back to its driver with the driver's settings -
	// later lines queue for the tap again.
	pub fn sleep_end(&self) {
		let mut inner = self.inner.lock();
		self.restore_after_sleep(&mut inner);
		inner.window = false;
		if let Owner::Sleep(generation) = inner.owner {
			if let Some(lent) = inner.lent.take() {
				self.model.lend_write(&self.io(Path::Sleep), &lent);
			}
			self.set_owner(&mut inner, Owner::Driver(generation));
		}
	}

	// ------------------------------------------------------------------ a planned end

	// A PLANNED END'S LAST LINES - see COM1's. A power-off or a reset a process asked for, while a driver holds the
	// UART: the tap is signalled and this core yielded until the ring has stayed empty for `SETTLE_QUIET_NS`, and at
	// most `SETTLE_BOUND_NS` in all. Never on a panic's or the forced deadline's path.
	pub fn settle_driver(&self) {
		let now = || crate::arch::common::time::CLOCK.nanos(crate::arch::tsc::now());
		let started = now();
		let mut empty_since: Option<u64> = None;
		loop {
			let (driving, empty) = {
				let inner = self.inner.lock();
				(matches!(inner.owner, Owner::Driver(_)), inner.ring.len == 0)
			};
			if !driving {
				return;
			}
			let at = now();
			if empty {
				if at.saturating_sub(*empty_since.get_or_insert(at)) >= SETTLE_QUIET_NS {
					return;
				}
			} else {
				empty_since = None;
				self.deliver_tap_signal();
			}
			if at.saturating_sub(started) >= SETTLE_BOUND_NS {
				return;
			}
			crate::sched::yield_now();
		}
	}

	// Whether the ring has dropped bytes at its bound that nobody has reported yet - for the development request that
	// fills it.
	#[cfg(any(test, liber_development))]
	pub fn dropped_pending(&self) -> bool {
		self.inner.lock().dropped != 0
	}

	// ------------------------------------------------------------------ for the suite

	#[cfg(test)]
	pub fn owner(&self) -> Owner {
		self.inner.lock().owner
	}

	#[cfg(test)]
	pub fn stray(&self) -> u64 {
		self.stray.load(Ordering::Relaxed)
	}

	#[cfg(test)]
	pub fn queued(&self) -> usize {
		self.inner.lock().ring.len
	}

	// ONE ACCESS BY AN ORDINARY KERNEL PATH, whoever holds the UART - what a forgotten drain or poll would do.
	#[cfg(test)]
	pub fn stray_read_for_test(&self, offset: u16) -> u32 {
		self.read(Path::Kernel, offset)
	}

	#[cfg(test)]
	pub fn take_read_out(&self) -> alloc::vec::Vec<u8> {
		let mut kept = self.read_out.lock();
		// ALLOC-OK: `#[cfg(test)]`, and a test that cannot allocate has already failed.
		let bytes = kept.0[..kept.1].to_vec();
		kept.1 = 0;
		bytes
	}
}
