// COM1 serial driver (16550 UART) with an asynchronous transmit ring, and the handoff of COM1 to a
// userspace driver.
//
// Writes never busy-wait on the UART: each byte is enqueued into a software ring
// and the writer returns at once, so a caller (above all the console service
// mirroring its output via SYS_DEBUG_WRITE) is never throttled by the emulated
// UART's transmit pacing. Under KVM every THRE poll is a VM-exit metered by QEMU's
// main loop, so the old per-byte `while !transmit_empty()` spin blocked the caller
// for hundreds of milliseconds on a screenful of output - which, on the console
// render thread, stalled the framebuffer behind the debug console. Now the ring
// drains in the background: on the timer tick and on each core's idle loop, pushing
// a FIFO's worth of bytes whenever the holding register is empty. The terminal-path
// writer serves the panic, fatal-exception, reset, power-off and test-exit paths,
// where the message must reach the wire before the machine stops.
//
// Early boot (before the timer and idle loop that drive the drain are running)
// writes straight to the wire, so boot logs appear immediately; `enable_async` flips
// to the ring once the scheduler is up.
//
// ONE OWNER AT A TIME, AND THE HANDOFF IS THE CLAIM. The UART belongs to the KERNEL from its first line
// until the kernel-declared COM1 row is claimed; the claim flips the owner to DRIVER - under the lock that
// serialises the ring, before the claim's resources are minted - and from then on no kernel path but the
// terminal-path writer and the sleep entry touches the registers. Kernel output keeps going into the ring,
// and the claim's CONSOLE TAP moves it out, in order, to the driver that writes it. The release hands the
// UART back: the kernel reads out what the receiver holds, re-initialises it and drains what queued
// meanwhile. The TERMINAL owner is the terminal-path writer's and is never left; SLEEP is the sleep entry's
// loan of a driver's UART to the kernel for the window around a sleep.
//
// EVERY ACCESS TO THE REGISTERS GOES THROUGH `read` AND `write`, and those count - and refuse - any access
// an ordinary kernel path makes while a driver holds the port. Every path checks the owner before it
// reaches for the port, so the count is zero by construction; it is the backstop the handoff gate reads,
// so a drain, an interrupt handler or a poll left running cannot hide behind a driver that also works.
//
// THE CODE IS AN INSTANCE WITH ITS BASE AS A PARAMETER: `COM1` is the console, and the test build has a
// second instance over the test machine's second UART, whose every register access is recorded, so the
// handoff machinery is tested without touching the wire the suite is judged by.

use super::port::{inb, outb};
use crate::object::console_tap::ConsoleTap;
use crate::sync::SpinLock;
use alloc::sync::{Arc, Weak};
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

// The 16550 transmit FIFO holds 16 bytes: when THRE is set the holding register
// (and FIFO) is empty, so a full FIFO load may be pushed before polling again.
const FIFO_DEPTH: usize = 16;

// Software transmit ring. 16 kB comfortably buffers a screenful of console mirror
// (a `help` listing is ~2 kB), so the producer never blocks in practice; on the
// rare overflow a kernel-owned UART drains synchronously to make room, and a byte
// that still does not fit is dropped and COUNTED - the count is printed where the
// bytes would have been.
const TX_RING_CAP: usize = 16384;

// The deepest receive FIFO a 16550-family part has: what the reacquisition reads out, at most.
const RX_READ_OUT: usize = 64;

// How long the terminal-path writer waits for the ring's lock before proceeding without it, and how long
// any synchronous write polls the holding register before writing anyway, in polls.
const TERMINAL_LOCK_SPINS: u32 = 10_000_000;
const THRE_POLLS: u32 = 1_000_000;
// A PLANNED END'S WAIT FOR THE DRIVER (`Uart::settle_driver`): the ring empty this long - the driver's largest tap read,
// 1024 bytes, takes 89 ms on the wire at the console's 115200 baud - and never longer than the bound in all.
pub const SETTLE_QUIET_NS: u64 = 100_000_000;
pub const SETTLE_BOUND_NS: u64 = 1_000_000_000;

// The registers, as offsets from the base.
const RBR_THR: u16 = 0;
const IER: u16 = 1;
const FCR: u16 = 2;
const LCR: u16 = 3;
const MCR: u16 = 4;
const LSR: u16 = 5;

const LSR_DATA_READY: u8 = 0x01;
const LSR_THR_EMPTY: u8 = 0x20;
const LSR_TX_IDLE: u8 = 0x40;
const LCR_DLAB: u8 = 0x80;
const LCR_8N1: u8 = 0x03;
const IER_RX_AVAILABLE: u8 = 0x01;

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
	// The sleep entry's loan of a driver's UART to the kernel, for the claim of this generation - see `sleep_begin`.
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

struct Inner {
	ring: TxRing,
	owner: Owner,
	// Bytes the ring's bound dropped since they were last reported - by the tap to its driver, or by the
	// kernel in a line of its own.
	dropped: u64,
	// The claim's tap, while a driver holds the UART.
	tap: Option<Weak<ConsoleTap>>,
	// The ring went from empty to holding bytes while a driver holds the UART, and the tap has not been
	// told yet. Told from a context that holds no other lock - see `deliver_tap_signal`.
	signal_due: bool,
	// A sleep window on a kernel-owned UART: every line goes to the wire synchronously until it closes.
	window: bool,
	// The UART lost its settings in the sleep that just ended; the next line re-initialises it first.
	lost: bool,
	// THE DRIVER'S SETTINGS WHILE ITS UART IS LENT to a sleep's entry - IER, LCR, MCR and the divisor's two bytes, read
	// as the loan began - written back as it ends. The loan programs the UART for the kernel's lines, and a UART handed
	// back with the receive interrupt off is a console that answers nothing typed after the sleep.
	lent: Option<[u8; 5]>,
}

// One register access, as the test build records it.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Access {
	pub write: bool,
	pub offset: u16,
	pub value: u8,
	pub path: Path,
}

// THE LAST ACCESSES to the suite's instance, kept in a ring, and how many there have been in all. The console
// records nothing: its registers are the wire the suite is judged by, and no test reads them back.
#[cfg(test)]
const RECORD_CAP: usize = 8192;

#[cfg(test)]
struct Record {
	entries: [Access; RECORD_CAP],
	len: usize,
}

#[cfg(test)]
static RECORD: SpinLock<Record> = SpinLock::new(Record { entries: [Access { write: false, offset: 0, value: 0, path: Path::Kernel }; RECORD_CAP], len: 0 });

pub struct Uart {
	base: u16,
	// The legacy IRQ its receive interrupt arrives on.
	irq: u8,
	// Whether this is the console: its read-out feeds the console input, and it prints the handoff lines.
	console: bool,
	inner: SpinLock<Inner>,
	// Accesses an ordinary kernel path made while a driver held the port - refused, and counted.
	stray: AtomicU64,
	// Whether a driver holds the UART now: the owner as the access path reads it, lock-free.
	driving: AtomicBool,
	// The receive handler the kernel armed, re-registered at a reacquisition (0: none armed).
	handler: AtomicUsize,
	// Whether the kernel enables the receive interrupt when it drives this UART.
	receive: AtomicBool,
	// A terminal-path writer has taken the UART: every line goes to the wire synchronously, without the lock.
	terminal: AtomicBool,
	// Bytes the reacquisition read out, kept by an instance that is not the console.
	#[cfg(test)]
	read_out: SpinLock<([u8; RX_READ_OUT], usize)>,
	// A UART AT ITS BAUD RATE, for the test that needs one. QEMU's 16550 hands every byte to its backend the
	// moment it is written, so THRE is set again at once and one drain empties the whole ring: no burst ever
	// waits for a later drain, and whether an idle core still comes back for the rest cannot be seen. Paced,
	// a drain pushes one FIFO load and stops, as it does on a UART whose FIFO is still going out.
	#[cfg(test)]
	paced: AtomicBool,
}

// THE CONSOLE.
pub static COM1: Uart = Uart::new(0x3F8, 4, true);

// THE TEST MACHINE'S SECOND UART, a second instance of this code whose every access is recorded.
#[cfg(test)]
pub static COM2: Uart = Uart::new(0x2F8, 3, false);

// False during early boot (synchronous writes, so logs appear before the drainers
// run), flipped true by `enable_async` once the timer and idle loop are servicing
// the ring. Monotonic: never flipped back.
static ASYNC: AtomicBool = AtomicBool::new(false);

impl Uart {
	pub const fn new(base: u16, irq: u8, console: bool) -> Self {
		Self {
			base,
			irq,
			console,
			inner: SpinLock::new(Inner { ring: TxRing::new(), owner: Owner::Kernel, dropped: 0, tap: None, signal_due: false, window: false, lost: false, lent: None }),
			stray: AtomicU64::new(0),
			driving: AtomicBool::new(false),
			handler: AtomicUsize::new(0),
			receive: AtomicBool::new(false),
			terminal: AtomicBool::new(false),
			#[cfg(test)]
			read_out: SpinLock::new(([0; RX_READ_OUT], 0)),
			#[cfg(test)]
			paced: AtomicBool::new(false),
		}
	}

	// ------------------------------------------------------------------ the one access path

	// Whether `path` may reach the registers now. An ordinary kernel path while a driver holds the UART may not,
	// and is counted. THE OWNER IS READ HERE, from the mirror every change of owner writes - not taken from the
	// caller - so a path that believed it owned the UART and did not is caught rather than trusted.
	fn admitted(&self, path: Path) -> bool {
		if self.driving.load(Ordering::Acquire) && path == Path::Kernel {
			self.stray.fetch_add(1, Ordering::Relaxed);
			return false;
		}
		true
	}

	// EVERY CHANGE OF OWNER, under the ring's lock, and the mirror the access path reads with it.
	fn set_owner(&self, inner: &mut Inner, owner: Owner) {
		inner.owner = owner;
		self.driving.store(matches!(owner, Owner::Driver(_)), Ordering::Release);
	}

	fn read(&self, path: Path, offset: u16) -> u8 {
		if !self.admitted(path) {
			return 0;
		}
		// SAFETY: this instance's own registers; every caller holds the owner it passes.
		let value = unsafe { inb(self.base + offset) };
		#[cfg(test)]
		self.note(Access { write: false, offset, value, path });
		value
	}

	fn write(&self, path: Path, offset: u16, value: u8) {
		if !self.admitted(path) {
			return;
		}
		// SAFETY: as `read`.
		unsafe { outb(self.base + offset, value) };
		#[cfg(test)]
		self.note(Access { write: true, offset, value, path });
	}

	#[cfg(test)]
	fn note(&self, access: Access) {
		if self.console {
			return;
		}
		let mut record = RECORD.lock();
		let at = record.len;
		record.entries[at % RECORD_CAP] = access;
		record.len = at + 1;
	}

	fn transmit_empty(&self, path: Path) -> bool {
		self.read(path, LSR) & LSR_THR_EMPTY != 0
	}

	// Write one byte to the wire, polling the holding register first - bounded, so a UART that never
	// empties cannot hang a terminal path.
	fn put_sync(&self, path: Path, byte: u8) {
		let mut polls = 0u32;
		while !self.transmit_empty(path) && polls < THRE_POLLS {
			core::hint::spin_loop();
			polls += 1;
		}
		self.write(path, RBR_THR, byte);
	}

	// THE BOOT INITIALISATION, whole: interrupt enables off, 38400 baud and 8N1 with the divisor latch
	// closed again, the FIFOs enabled and cleared, modem control with the interrupt output on and loopback
	// off - and the receive interrupt after it, when asked. Every register a driver could have left in any
	// state is written.
	fn boot_init(&self, path: Path, receive_interrupt: bool) {
		self.boot_init_reading(path, receive_interrupt, &mut [0u8; 2]);
	}

	// THE SAME, with the divisor it replaces read into `divisor` while the latch is open - reads only, so the writes are
	// the boot sequence's and nothing else.
	fn boot_init_reading(&self, path: Path, receive_interrupt: bool, divisor: &mut [u8; 2]) {
		self.write(path, IER, 0x00);
		self.write(path, LCR, LCR_DLAB);
		divisor[0] = self.read(path, RBR_THR);
		divisor[1] = self.read(path, IER);
		self.write(path, RBR_THR, 0x03);
		self.write(path, IER, 0x00);
		self.write(path, LCR, LCR_8N1);
		self.write(path, FCR, 0xC7);
		self.write(path, MCR, 0x0B);
		if receive_interrupt {
			self.write(path, IER, IER_RX_AVAILABLE);
		}
	}

	// ------------------------------------------------------------------ the ring

	// Push as much of the ring to the UART as the holding register will take right now: one FIFO load if
	// THRE is set, then return. Non-blocking, and the kernel's alone: a driver drains the ring through the
	// tap. The caller holds the lock.
	fn drain_locked(&self, inner: &mut Inner) {
		if inner.owner != Owner::Kernel {
			return;
		}
		while inner.ring.len != 0 && self.transmit_empty(Path::Kernel) {
			let n: usize = inner.ring.len.min(FIFO_DEPTH);
			for _ in 0..n {
				let byte: u8 = inner.ring.pop();
				self.write(Path::Kernel, RBR_THR, byte);
			}
			#[cfg(test)]
			if self.paced.load(Ordering::Relaxed) {
				return;
			}
		}
	}

	// Queue one byte for a driver's tap, noting that the tap is owed a signal when the ring was empty.
	fn push_for_tap(inner: &mut Inner, byte: u8) {
		if inner.ring.len == 0 {
			inner.signal_due = true;
		}
		// ALLOC-OK: a fixed-capacity TX ring, not a heap collection - every caller checked the capacity.
		inner.ring.push(byte);
	}

	// A line after a sleep that lost the UART's settings: the boot initialisation first, before any byte,
	// with the receive interrupt as the owner wants it.
	fn restore_after_sleep(&self, inner: &mut Inner) {
		if !inner.lost {
			return;
		}
		inner.lost = false;
		match inner.owner {
			Owner::Kernel => self.boot_init(Path::Sleep, self.receive.load(Ordering::Relaxed)),
			Owner::Sleep(_) => self.boot_init(Path::Sleep, false),
			Owner::Driver(_) | Owner::Terminal => {}
		}
	}

	// ONE BYTE OF THE KERNEL'S OWN OUTPUT - lossless while the kernel drives the UART, queued for the tap
	// while a driver does, synchronous in a sleep window and on the terminal path.
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
			Owner::Kernel if !ASYNC.load(Ordering::Acquire) => {
				// Early boot: straight to the wire so logs appear immediately, before the timer and idle
				// loop that service the ring are running.
				self.put_sync(Path::Kernel, byte);
			}
			Owner::Kernel => {
				if inner.ring.len == TX_RING_CAP {
					// Ring full: a flood outpacing the UART. Push out whatever the holding register will take
					// right now (non-blocking) to make room, then enqueue. If the UART is not ready this byte
					// is dropped, and counted, rather than busy-waited on with the lock - and so interrupts -
					// held, which would stall this core's timer tick.
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
				// THE DRIVER DRAINS THE RING THROUGH THE TAP, and this path may not touch the UART to make
				// room: a full ring drops the byte and counts it, and the tap reports the count.
				if inner.ring.len == TX_RING_CAP {
					inner.dropped += 1;
					return;
				}
				Self::push_for_tap(&mut inner, byte);
			}
		}
	}

	// Best-effort bulk enqueue for the SYS_DEBUG_WRITE path: as much as fits (translating \n to \r\n), under
	// one lock, answering how many of the SOURCE bytes were fully accepted - the caller keeps the rest and
	// may offer it again. Unlike `write_byte` it never drains synchronously on a full ring, so a userspace
	// caller - above all the console service mirroring a screenful while the boot log is still draining the
	// baud-paced UART - is never throttled. Early boot, a sleep window and the terminal path write to the
	// wire and accept everything.
	pub fn write_bytes(&self, bytes: &[u8]) -> usize {
		let synchronous = self.terminal.load(Ordering::Acquire) || {
			let inner = self.inner.lock();
			match inner.owner {
				Owner::Terminal => true,
				Owner::Sleep(_) => true,
				Owner::Kernel => inner.window || !ASYNC.load(Ordering::Acquire),
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
			let queued = matches!(inner.owner, Owner::Kernel | Owner::Driver(_)) && !inner.window && ASYNC.load(Ordering::Acquire) && !self.terminal.load(Ordering::Acquire);
			if queued && TX_RING_CAP - inner.ring.len < need {
				return false;
			}
		}
		// A producer between the check and the write can take the room; what did not fit is then offered
		// again by the caller, which is what `write_bytes` answering short means.
		self.write_bytes(bytes) == bytes.len()
	}

	// Background drain: from a busy core's timer tick and from each core's idle loop. `try_lock` so it never
	// spins in an interrupt handler when another core holds the ring; it tries again on the next tick. While
	// a driver holds the UART it drains nothing and delivers the tap's signal instead.
	pub fn drain_tx(&self) {
		if let Some(mut inner) = self.inner.try_lock() {
			self.drain_locked(&mut inner);
		}
		self.deliver_tap_signal();
	}

	// DRAIN THE WHOLE RING SYNCHRONOUSLY while the kernel drives the UART - for a caller that must see the
	// ring empty before it goes on, and that is not ending the machine. While a driver holds the UART the
	// driver drains it, and this returns at once.
	pub fn drain_sync(&self) {
		let mut inner = self.inner.lock();
		if inner.owner != Owner::Kernel {
			return;
		}
		while inner.ring.len != 0 {
			while !self.transmit_empty(Path::Kernel) {
				core::hint::spin_loop();
			}
			self.drain_locked(&mut inner);
		}
		while !self.transmit_empty(Path::Kernel) {
			core::hint::spin_loop();
		}
	}

	// WHETHER THE RING HOLDS BYTES THE KERNEL STILL HAS TO MOVE: an idle core then sleeps one tick at most -
	// to drain them while the kernel drives the UART, or to tell a driver's tap they are there. A ring
	// another core is working on right now counts as holding bytes.
	pub fn tx_pending(&self) -> bool {
		if !ASYNC.load(Ordering::Acquire) {
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

	// Read one received byte without waiting, while the kernel drives the UART; `None` while a driver does -
	// the kernel feeds no byte of a driver's UART to the console.
	pub fn read_byte(&self) -> Option<u8> {
		let inner = self.inner.lock();
		if inner.owner != Owner::Kernel {
			return None;
		}
		if self.read(Path::Kernel, LSR) & LSR_DATA_READY != 0 { Some(self.read(Path::Kernel, RBR_THR)) } else { None }
	}

	// ------------------------------------------------------------------ the handoff

	// THE CLAIM TAKES THE UART: the owner flips to the driver under the ring's lock FIRST, then the kernel's
	// receive handler is unregistered - and only after this returns are the claim's resources minted.
	// Refused unless the kernel drives it now (a terminal path took it, or another claim holds it).
	pub fn hand_over(&self, generation: u64, row: usize) -> bool {
		{
			let mut inner = self.inner.lock();
			if inner.owner != Owner::Kernel {
				return false;
			}
			self.set_owner(&mut inner, Owner::Driver(generation));
			inner.signal_due = inner.ring.len != 0;
		}
		// The kernel's receive handler goes, so the claim can bind the line; the suite arms none.
		let armed = self.handler.load(Ordering::Acquire) != 0;
		#[cfg(not(test))]
		if armed {
			super::interrupts::unregister(super::interrupts::IRQ_BASE as u32 + self.irq as u32);
		}
		if self.console {
			let handler = if armed { ", the kernel's receive handler taken off its line" } else { "" };
			crate::serial_println!("console: COM1 (IRQ {}) is handed to row {row}, claim generation {generation}{handler} - kernel output now leaves through the claim's console tap", self.irq);
		}
		true
	}

	// Whether the claim of `generation` holds the UART now - what the port range's mint and the tap ask.
	pub fn held_by(&self, generation: u64) -> bool {
		self.inner.lock().owner.claim() == Some(generation)
	}

	// THE CLAIM'S TAP IS ATTACHED: signalled when the ring goes from empty to holding bytes, and at once if
	// it holds bytes already.
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

	// MOVE BYTES OUT OF THE RING for the claim of `generation`, in order, and answer how many, how many the
	// bound dropped since the last read, and how many are left. `None` when that claim no longer holds the UART.
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

	// TELL THE TAP ITS RING HOLDS BYTES, from a context that holds no other lock - the timer tick, the idle
	// loop, the debug-write syscall - since waking its reader takes the scheduler's locks, which a line
	// printed under some other lock must not wait for.
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

	// THE RELEASE HANDS THE UART BACK. The claim's port range, line and tap are revoked by then, and its
	// ports are back in the reserved set. The kernel takes the ring's lock, becomes the owner, and READS OUT
	// WHAT THE RECEIVER HOLDS BEFORE ANYTHING ELSE - DLAB cleared first, which the driver may have left set,
	// and at most a FIFO's depth - because the boot sequence that follows resets the receive FIFO. Then the
	// boot initialisation, the receive handler re-registered and re-routed, the bytes read out fed to the
	// console, and one line - with the stray-access count in the development and test builds, and the
	// bound's drops when there were any. A quarantined release returns the UART all the same: the kernel is
	// not a claimant, and a stranded holder can at worst interleave bytes with it.
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
			self.write(Path::Kernel, LCR, LCR_8N1);
			while count < RX_READ_OUT && self.read(Path::Kernel, LSR) & LSR_DATA_READY != 0 {
				bytes[count] = self.read(Path::Kernel, RBR_THR);
				count += 1;
			}
			self.boot_init(Path::Kernel, receive);
			dropped = core::mem::take(&mut inner.dropped);
		}
		#[cfg(not(test))]
		{
			let handler = self.handler.load(Ordering::Acquire);
			if handler != 0 {
				let vector = super::interrupts::IRQ_BASE + self.irq;
				// SAFETY: `handler` was stored from a `HandlerFn` by `arm_rx_interrupt` and nothing else writes it.
				super::interrupts::register(vector as u32, unsafe { core::mem::transmute::<usize, super::interrupts::HandlerFn>(handler) });
				super::ioapic::route(self.irq as u32, vector, crate::smp::lapic_id(0), super::ioapic::Kind::IsaEdge);
			}
		}
		let stray = self.stray.swap(0, Ordering::Relaxed);
		if self.console {
			for &byte in &bytes[..count] {
				crate::console_input::feed_serial(byte);
			}
			let how = if quarantined { " - its release was not confirmed, and the row is not claimed again this boot" } else { "" };
			if cfg!(liber_development) || cfg!(test) {
				crate::serial_println!("console: COM1 is the kernel's again{how} - {stray} kernel access(es) to its registers while the driver held it, {count} byte(s) read out of its receiver");
			} else {
				crate::serial_println!("console: COM1 is the kernel's again{how}");
			}
			if dropped != 0 {
				crate::serial_println!("console: {dropped} byte(s) of kernel output were dropped at the ring's bound while the driver held COM1");
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

	// THE TERMINAL-PATH WRITER, entered BEFORE anything is printed on a path that ends the machine. It takes
	// the ring's lock with a bounded wait and past the bound proceeds without it; re-runs the UART's whole
	// boot initialisation - a driver may have left DLAB set, another divisor, the FIFO off or loopback on,
	// in which every byte sent returns to the receiver and none reaches the wire; writes out the ring's
	// backlog by polling, then the count the bound dropped when it is not zero; and sets the owner to
	// TERMINAL, which is never left. From then on every line goes to the wire synchronously and nothing
	// enters the ring. A driver still running on another core can interleave bytes with it; it cannot
	// withhold the text, because nothing here waits for it or passes through the ring it drains.
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
		let inner: &mut Inner = match guard.as_mut() {
			Some(guard) => guard,
			// SAFETY: past the bound the lock's holder is not coming back in time - it may be this very core,
			// interrupted inside the ring - and the machine is ending: the ring is read without it.
			None => unsafe { self.inner.get_unlocked() },
		};
		// The kernel's own bytes already in the FIFO go out before the initialisation clears it.
		if inner.owner == Owner::Kernel {
			self.wait_idle(Path::Terminal);
		}
		self.boot_init(Path::Terminal, false);
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
			let mut line = LineBuf::new();
			let _ = write!(line, "\r\nconsole: {dropped} byte(s) of kernel output were dropped at the ring's bound before this point\r\n");
			for &byte in line.bytes() {
				self.put_sync(Path::Terminal, byte);
			}
		}
		self.wait_idle(Path::Terminal);
	}

	// Poll until the transmitter has sent everything - bounded.
	fn wait_idle(&self, path: Path) {
		let mut polls = 0u32;
		while self.read(path, LSR) & LSR_TX_IDLE == 0 && polls < THRE_POLLS {
			core::hint::spin_loop();
			polls += 1;
		}
	}

	// ------------------------------------------------------------------ the sleep entry

	// THE SLEEP ENTRY IS NOT A TERMINAL PATH, because the machine comes back. It takes the ring's lock and
	// follows the owner it finds. KERNEL: it drains the ring, and every line until `sleep_end` goes to the
	// wire synchronously - the owner stays KERNEL. DRIVER: every binding has answered SUSPENDED before the
	// entry, so the driver has stopped driving the UART, and the entry LENDS IT TO THE KERNEL FOR THE SLEEP:
	// the owner becomes SLEEP, the boot initialisation runs with the receive interrupt left off - the line
	// stays the claim's and nothing is read while the machine sleeps - and the ring is drained. The claim,
	// its port range, its line and its tap are untouched throughout, and nothing the entry does is counted
	// against the driver's hold, since the owner is not DRIVER while it is done.
	//
	// The kernel's sleep entry is the production caller (`crate::sleep`), and the suite the other.
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
				let (ier, lcr, mcr) = (self.read(Path::Sleep, IER), self.read(Path::Sleep, LCR), self.read(Path::Sleep, MCR));
				let mut divisor = [0u8; 2];
				self.boot_init_reading(Path::Sleep, false, &mut divisor);
				inner.lent = Some([ier, lcr, mcr, divisor[0], divisor[1]]);
				while inner.ring.len != 0 {
					let byte = inner.ring.pop();
					self.put_sync(Path::Sleep, byte);
				}
				inner.signal_due = false;
			}
			Owner::Sleep(_) | Owner::Terminal => {}
		}
	}

	// THE WAKE, before any line after it: whether the UART lost its settings - an S3 wake resets it with the
	// machine, a suspend to idle resets nothing. When it did, the boot initialisation runs again before the
	// next line's first byte, with the receive interrupt enabled while the kernel owns the UART and left off
	// while it is lent.
	pub fn sleep_wake(&self, lost_settings: bool) {
		let mut inner = self.inner.lock();
		if lost_settings && matches!(inner.owner, Owner::Kernel | Owner::Sleep(_)) {
			inner.lost = true;
		}
	}

	// THE ENTRY'S LAST ACT: the window closes, and a lent UART goes back to its driver - later lines queue
	// for the tap again.
	pub fn sleep_end(&self) {
		let mut inner = self.inner.lock();
		self.restore_after_sleep(&mut inner);
		inner.window = false;
		if let Owner::Sleep(generation) = inner.owner {
			// THE DRIVER'S SETTINGS BACK before the UART is: the divisor through the latch, the line, the modem control,
			// and the interrupt enables last.
			if let Some([ier, lcr, mcr, dll, dlm]) = inner.lent.take() {
				self.write(Path::Sleep, LCR, LCR_DLAB);
				self.write(Path::Sleep, RBR_THR, dll);
				self.write(Path::Sleep, IER, dlm);
				self.write(Path::Sleep, LCR, lcr & !LCR_DLAB);
				self.write(Path::Sleep, MCR, mcr);
				self.write(Path::Sleep, IER, ier);
			}
			self.set_owner(&mut inner, Owner::Driver(generation));
		}
	}

	// ------------------------------------------------------------------ a planned end

	// A PLANNED END'S LAST LINES. A power-off or a reset a process asked for, while a driver holds the UART: the lines
	// before it - the request's own, a button driver's "pressed" - are in the ring or already in the driver's hands, and
	// the terminal-path writer that follows takes the UART whatever it finds, so what the driver had read and not yet
	// put out was lost with the machine. So the tap is signalled and this core yielded until the ring has stayed empty
	// for `SETTLE_QUIET_NS` - long enough for the driver's largest read to reach the wire - and at most `SETTLE_BOUND_NS`
	// in all: a driver that takes nothing does not hold the machine up. Never on a panic's or the forced deadline's
	// path, where nothing may wait; the kernel's own lines need none of it.
	pub fn settle_driver(&self) {
		let now = || crate::arch::common::time::CLOCK.nanos(super::tsc::now());
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

	// ------------------------------------------------------------------ for the suite

	#[cfg(test)]
	pub fn owner(&self) -> Owner {
		self.inner.lock().owner
	}

	// How many accesses the suite's instance has had in all.
	#[cfg(test)]
	pub fn record_len(&self) -> usize {
		RECORD.lock().len
	}

	// The accesses recorded from the `from`th on - as many of them as the ring still holds, which is the last
	// `RECORD_CAP`.
	#[cfg(test)]
	pub fn record_from(&self, from: usize) -> alloc::vec::Vec<Access> {
		let record = RECORD.lock();
		let first = from.max(record.len.saturating_sub(RECORD_CAP));
		// ALLOC-OK: `#[cfg(test)]`, and a test that cannot allocate has already failed.
		(first..record.len).map(|at| record.entries[at % RECORD_CAP]).collect()
	}

	// ONE ACCESS BY AN ORDINARY KERNEL PATH, whoever holds the UART - what a forgotten drain or poll would do.
	// The access path refuses it and counts it while a driver holds the port, which is the count the handoff
	// gate reads.
	#[cfg(test)]
	pub fn stray_read_for_test(&self, offset: u16) -> u8 {
		self.read(Path::Kernel, offset)
	}

	// The instance driven by the kernel from scratch, as the console is at boot: every setting programmed, the
	// receive interrupt as `set_receive` left it.
	#[cfg(test)]
	pub fn init_for_test(&self) {
		self.boot_init(Path::Kernel, self.receive.load(Ordering::Relaxed));
	}

	#[cfg(test)]
	pub fn stray(&self) -> u64 {
		self.stray.load(Ordering::Relaxed)
	}

	#[cfg(test)]
	pub fn queued(&self) -> usize {
		self.inner.lock().ring.len
	}

	#[cfg(test)]
	pub fn take_read_out(&self) -> alloc::vec::Vec<u8> {
		let mut kept = self.read_out.lock();
		// ALLOC-OK: `#[cfg(test)]`, as above.
		let bytes = kept.0[..kept.1].to_vec();
		kept.1 = 0;
		bytes
	}

	// Whether the kernel enables the receive interrupt when it drives this instance.
	#[cfg(test)]
	pub fn set_receive(&self, receive: bool) {
		self.receive.store(receive, Ordering::Relaxed);
	}

	// A terminal-path writer's instance back to the kernel, for the next test: the terminal owner is never
	// left on the console, and the suite's instance is not the console.
	#[cfg(test)]
	pub fn reset_for_test(&self) {
		assert!(!self.console, "the console's terminal owner is never left");
		let mut inner = self.inner.lock();
		self.set_owner(&mut inner, Owner::Kernel);
		inner.ring = TxRing::new();
		inner.dropped = 0;
		inner.tap = None;
		inner.signal_due = false;
		inner.window = false;
		inner.lost = false;
		self.terminal.store(false, Ordering::Release);
		self.stray.store(0, Ordering::Relaxed);
	}
}

// A line built on the stack, for the terminal path, which may not allocate.
struct LineBuf {
	bytes: [u8; 160],
	len: usize,
}

impl LineBuf {
	fn new() -> Self {
		Self { bytes: [0; 160], len: 0 }
	}

	fn bytes(&self) -> &[u8] {
		&self.bytes[..self.len]
	}
}

impl Write for LineBuf {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		let take = s.len().min(self.bytes.len() - self.len);
		self.bytes[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
		self.len += take;
		Ok(())
	}
}

// ------------------------------------------------------------------ the console's entry points

// UART init: 38400 baud, 8N1, FIFO enabled.
pub fn init() {
	COM1.boot_init(Path::Kernel, false);
}

// THE CONSOLE UART'S RECEIVE INTERRUPT, answered by `handler`: COM1's legacy IRQ 4, routed to the boot
// processor through the I/O APIC, and the UART told to raise it - so a typed byte reaches the shell at
// once, and wakes an idle machine that takes no periodic tick to find it by. Answers the number an idle
// core's record names it by, which on this port is the IDT vector. The handler is kept, so a reacquisition
// after a driver held COM1 arms it again.
#[cfg(not(test))]
pub fn arm_rx_interrupt(handler: super::interrupts::HandlerFn) -> Result<u32, &'static str> {
	let vector = super::interrupts::IRQ_BASE + COM1.irq;
	let inner = COM1.inner.lock();
	if inner.owner != Owner::Kernel {
		return Err("a driver holds COM1");
	}
	COM1.handler.store(handler as usize, Ordering::Release);
	COM1.receive.store(true, Ordering::Relaxed);
	super::interrupts::register(vector as u32, handler);
	super::ioapic::route(COM1.irq as u32, vector, crate::smp::lapic_id(0), super::ioapic::Kind::IsaEdge);
	COM1.write(Path::Kernel, IER, IER_RX_AVAILABLE);
	drop(inner);
	Ok(vector as u32)
}

// Switch transmit to the asynchronous ring. Called once the scheduler is up, so the
// timer tick and idle loop are draining the ring; until then writes go straight to
// the wire (see `write_byte`).
pub fn enable_async() {
	ASYNC.store(true, Ordering::Release);
}

#[cfg(test)]
pub fn pace(paced: bool) {
	COM1.paced.store(paced, Ordering::SeqCst);
}

pub fn drain_tx() {
	COM1.drain_tx();
}

// THE TERMINAL-PATH WRITER - see `Uart::terminal`. Entered before the first line of a panic or a fatal
// exception, and before a reset, a power-off or the test exit acts.
pub fn flush_sync() {
	COM1.terminal();
}

pub fn drain_sync() {
	COM1.drain_sync();
}

// A PLANNED END'S LAST LINES - see `Uart::settle_driver`. For a power-off or a reset a process asked for.
pub fn settle_driver() {
	COM1.settle_driver();
}

// MAKE ROOM IN THE RING for a writer that may not lose a byte: drained here while the kernel drives the
// UART, and by the driver - which this core lets run - while one does. The caller holds no lock.
pub fn make_room() {
	if matches!(COM1.inner.lock().owner, Owner::Driver(_)) {
		COM1.deliver_tap_signal();
		crate::sched::yield_now();
	} else {
		COM1.drain_sync();
	}
}

pub fn write_bytes(bytes: &[u8]) -> usize {
	COM1.write_bytes(bytes)
}

pub fn write_whole(bytes: &[u8]) -> bool {
	COM1.write_whole(bytes)
}

pub fn tx_pending() -> bool {
	COM1.tx_pending()
}

#[cfg(not(test))]
pub fn read_byte() -> Option<u8> {
	COM1.read_byte()
}

// ------------------------------------------------------------------ the handoff, by the UART's base

// The instance at `base`: COM1, and in the test build the suite's second UART.
fn instance(base: u64) -> Option<&'static Uart> {
	if base == COM1.base as u64 {
		return Some(&COM1);
	}
	#[cfg(test)]
	if base == COM2.base as u64 {
		return Some(&COM2);
	}
	None
}

// THE SLEEP ENTRY'S WINDOW ON THE CONSOLE UART - see `Uart::sleep_begin`.
pub fn sleep_begin() {
	COM1.sleep_begin();
}

pub fn sleep_wake(lost_settings: bool) {
	COM1.sleep_wake(lost_settings);
}

pub fn sleep_end() {
	COM1.sleep_end();
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

pub fn console_attach_tap(base: u64, generation: u64, tap: &Arc<ConsoleTap>) -> bool {
	instance(base).is_some_and(|uart| uart.attach_tap(generation, tap))
}

pub fn console_tap_read(base: u64, generation: u64, buf: &mut [u8]) -> Option<(usize, u64, usize)> {
	instance(base)?.tap_read(generation, buf)
}

// The debug-write syscall's half of telling a tap: it holds no lock when it asks.
pub fn console_deliver() {
	COM1.deliver_tap_signal();
}

// Whether the console's ring has dropped bytes at its bound that nobody has reported yet - for the development
// request that fills it. `None` where the console has no ring.
#[cfg(liber_development)]
pub fn console_dropped() -> Option<bool> {
	Some(COM1.inner.lock().dropped != 0)
}

pub struct SerialWriter;

impl Write for SerialWriter {
	fn write_str(&mut self, s: &str) -> fmt::Result {
		for byte in s.bytes() {
			if byte == b'\n' {
				COM1.write_byte(b'\r');
			}
			COM1.write_byte(byte);
		}
		Ok(())
	}
}
