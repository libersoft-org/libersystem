// THE CONSOLE UART'S RULES, OVER A SCRIPTED UART.
//
// The two ports' consoles are instances of these rules over their own registers, and neither test machine has a
// second PL011 or a second MMIO 16550 to run them on without touching the wire the suite is judged by. So the rules
// run here over a UART that is a script: every register access is recorded, a write to its data register is a byte
// on its wire, and its receiver holds what the test put there. Each test has an instance of its own, so the order of
// the suite decides nothing - and since the script needs no device, the rules run on every target.

use super::*;
use alloc::vec::Vec;

// The scripted UART's registers.
const DATA: u16 = 0;
const STATUS: u16 = 1;
// The boot programming's mark: 1, or 3 when it enables the receive interrupt.
const BOOT: u16 = 2;
const RECEIVE: u16 = 3;
const LENT: u16 = 4;
const PREPARE: u16 = 5;
const FIRMWARE: u16 = 6;

// What the scripted firmware left, and what a scripted driver leaves.
const FIRMWARE_SETTINGS: u32 = 0x5A;
const DRIVER_SETTINGS: u32 = 0x77;

struct Script {
	// Every access, as (write, offset, value).
	accesses: Vec<(bool, u16, u32)>,
	wire: Vec<u8>,
	received: Vec<u8>,
	released: u32,
	rearmed: u32,
}

struct Fake {
	base: u64,
	script: SpinLock<Script>,
}

impl Fake {
	const fn new(base: u64) -> Self {
		Fake { base, script: SpinLock::new(Script { accesses: Vec::new(), wire: Vec::new(), received: Vec::new(), released: 0, rearmed: 0 }) }
	}

	fn accesses(&self) -> usize {
		self.script.lock().accesses.len()
	}

	fn accesses_from(&self, from: usize) -> Vec<(bool, u16, u32)> {
		self.script.lock().accesses[from..].to_vec()
	}

	fn wire(&self) -> Vec<u8> {
		core::mem::take(&mut self.script.lock().wire)
	}

	fn receive(&self, bytes: &[u8]) {
		self.script.lock().received.extend_from_slice(bytes);
	}
}

impl Model for Fake {
	const NAME: &'static str = "the scripted UART";
	type Settings = u32;

	fn base(&self) -> u64 {
		self.base
	}

	fn raw_read(&self, offset: u16) -> u32 {
		let mut script = self.script.lock();
		let value = match offset {
			DATA if !script.received.is_empty() => u32::from(script.received.remove(0)),
			STATUS => u32::from(!script.received.is_empty()),
			LENT => DRIVER_SETTINGS,
			_ => 0,
		};
		script.accesses.push((false, offset, value));
		value
	}

	fn raw_write(&self, offset: u16, value: u32) {
		let mut script = self.script.lock();
		script.accesses.push((true, offset, value));
		if offset == DATA {
			script.wire.push(value as u8);
		}
	}

	fn tx_room(&self, _io: &Io<'_, Self>) -> usize {
		4
	}

	fn tx_put(&self, io: &Io<'_, Self>, byte: u8) {
		io.write(DATA, u32::from(byte));
	}

	fn tx_idle(&self, _io: &Io<'_, Self>) -> bool {
		true
	}

	fn rx_take(&self, io: &Io<'_, Self>) -> Option<u8> {
		(io.read(STATUS) != 0).then(|| io.read(DATA) as u8)
	}

	fn rx_prepare(&self, io: &Io<'_, Self>, _firmware: Option<&u32>) {
		io.write(PREPARE, 1);
	}

	fn read_firmware(&self, _io: &Io<'_, Self>) -> u32 {
		FIRMWARE_SETTINGS
	}

	fn boot_init(&self, io: &Io<'_, Self>, firmware: Option<&u32>, receive: bool) {
		io.write(BOOT, if receive { 3 } else { 1 });
		io.write(FIRMWARE, firmware.copied().unwrap_or(0));
	}

	fn receive_enable(&self, io: &Io<'_, Self>, on: bool) {
		io.write(RECEIVE, u32::from(on));
	}

	fn lend_read(&self, io: &Io<'_, Self>) -> u32 {
		io.read(LENT)
	}

	fn lend_write(&self, io: &Io<'_, Self>, lent: &u32) {
		io.write(LENT, *lent);
	}

	fn line(&self) -> Option<(&'static str, u32)> {
		Some(("line", 9))
	}

	fn release_line(&self) {
		self.script.lock().released += 1;
	}

	fn rearm_line(&self, _handler: Handler) {
		self.script.lock().rearmed += 1;
	}
}

fn handler(_number: u32) {}

// Everything the tap holds, read out in the driver's chunks: the bytes and the dropped count.
fn tap_all(uart: &Uart<Fake>, generation: u64) -> (Vec<u8>, u64) {
	let mut out = Vec::new();
	let mut dropped = 0u64;
	let mut chunk = [0u8; 1024];
	loop {
		let (n, lost, left) = uart.tap_read(generation, &mut chunk).expect("the claim holds the UART");
		out.extend_from_slice(&chunk[..n]);
		dropped += lost;
		if left == 0 {
			return (out, dropped);
		}
	}
}

crate::tagged_test!(the_kernel_s_own_output_reaches_the_wire_in_order_synchronously_at_first_and_then_through_the_ring, [Kernel, Console], id = "kernel.arch.common.console_uart.the_kernel_s_own_output_reaches_the_wire_in_order", covers = ["kernel"]);
fn the_kernel_s_own_output_reaches_the_wire_in_order_synchronously_at_first_and_then_through_the_ring() {
	static UART: Uart<Fake> = Uart::new(Fake::new(0xF100_0000), false);
	assert_eq!(UART.base(), 0xF100_0000);
	assert_eq!(UART.write_bytes(b"boot\n"), 5, "early boot writes the wire and accepts everything");
	assert_eq!(UART.model().wire(), b"boot\r\n", "a newline goes out as CR LF");
	assert!(!UART.tx_pending(), "and nothing waits while writes are synchronous");
	UART.enable_async();
	UART.keep_firmware_settings();
	assert!(UART.write_whole(b"one\n"));
	assert_eq!(UART.write_bytes(b"two"), 3);
	assert!(UART.model().wire().is_empty(), "the ring holds the bytes until it is drained");
	assert!(UART.tx_pending(), "and an idle core is told to come back");
	UART.drain_tx();
	assert_eq!(UART.model().wire(), b"one\r\ntwo", "the timer's or the idle loop's drain puts them out in order");
	UART.write_byte(b'!');
	UART.make_room();
	assert_eq!(UART.model().wire(), b"!", "a writer that may not lose a byte drains the ring while the kernel drives it");
	assert_eq!(UART.write_bytes(b"x"), 1);
	UART.drain_sync();
	assert_eq!(UART.model().wire(), b"x");
	assert_eq!(UART.read_byte(), None, "an empty receiver answers nothing");
	UART.model().receive(b"k");
	assert_eq!(UART.read_byte(), Some(b'k'), "and a received byte is the kernel's while it drives the UART");
}

crate::tagged_test!(the_tap_moves_the_ring_out_in_order_and_counts_what_its_bound_dropped, [Kernel, Console], id = "kernel.arch.common.console_uart.the_tap_moves_the_ring_out_in_order_and_counts_what_its_bound_dropped", covers = ["kernel"]);
fn the_tap_moves_the_ring_out_in_order_and_counts_what_its_bound_dropped() {
	static UART: Uart<Fake> = Uart::new(Fake::new(0xF200_0000), false);
	UART.enable_async();
	assert!(UART.write_bytes(b"queued before the claim\n") != 0);
	assert!(UART.hand_over(5, 1), "the kernel hands its UART to the claim");
	assert_eq!(UART.owner(), Owner::Driver(5));
	assert!(!UART.hand_over(6, 1), "and a second claim is refused while the first holds it");
	assert!(UART.held_by(5) && !UART.held_by(6), "the holding claim is the one that asked");
	let tap = ConsoleTap::new(UART.base(), 5).expect("a tap");
	assert!(!UART.attach_tap(6, &tap), "a tap of another claim is refused");
	assert!(UART.attach_tap(5, &tap), "and the claim's own is attached");
	assert!(tap.is_pending(), "signalled at once, since the ring held the kernel's line already");
	assert_eq!(tap_all(&UART, 5), (b"queued before the claim\r\n".to_vec(), 0), "the line queued before the handoff comes out first, whole");
	assert!(UART.tap_read(6, &mut [0u8; 8]).is_none(), "another claim's tap reads nothing");
	assert_eq!(UART.write_bytes(b"a\nb"), 3);
	assert_eq!(tap_all(&UART, 5).0, b"a\r\nb", "kernel output through the tap, in order, with its CR LF");
	// PAST THE BOUND: every byte that does not fit is dropped and counted, and the tap says how many.
	for _ in 0..TX_RING_CAP + 10 {
		UART.write_byte(b'f');
	}
	assert!(UART.dropped_pending(), "the bound dropped bytes nobody has reported");
	assert_eq!(UART.queued(), TX_RING_CAP);
	let (bytes, dropped) = tap_all(&UART, 5);
	assert_eq!((bytes.len(), dropped), (TX_RING_CAP, 10), "the ring's worth comes out, and the ten past it are counted");
	assert!(!UART.dropped_pending(), "and the count was reported once");
	UART.hand_back(false);
	assert_eq!(UART.owner(), Owner::Kernel);
	assert!(UART.tap_read(5, &mut [0u8; 8]).is_none(), "a released claim's tap reads nothing");
}

crate::tagged_test!(while_a_driver_holds_the_uart_no_kernel_path_touches_it_and_a_stray_access_is_counted, [Kernel, Console], id = "kernel.arch.common.console_uart.while_a_driver_holds_the_uart_no_kernel_path_touches_it", covers = ["kernel"]);
fn while_a_driver_holds_the_uart_no_kernel_path_touches_it_and_a_stray_access_is_counted() {
	static UART: Uart<Fake> = Uart::new(Fake::new(0xF300_0000), false);
	UART.enable_async();
	assert!(UART.hand_over(7, 2));
	UART.model().receive(b"typed");
	let before = UART.model().accesses();
	// EVERY ORDINARY PATH, as the kernel runs them while a driver holds the UART.
	UART.write_byte(b'a');
	assert_eq!(UART.write_bytes(b"bc\n"), 3);
	assert!(UART.tx_pending(), "an idle core comes back to tell the tap");
	UART.drain_tx();
	UART.drain_sync();
	assert_eq!(UART.read_byte(), None, "the kernel feeds no byte of a driver's UART to the console");
	let _ = tap_all(&UART, 7);
	UART.make_room();
	UART.settle_driver();
	assert_eq!(UART.model().accesses(), before, "not one register access while the driver holds the UART");
	assert_eq!(UART.stray(), 0, "and nothing was refused, because nothing asked");
	// A PATH THAT FORGOT: refused at the access path, and counted.
	assert_eq!(UART.stray_read_for_test(STATUS), 0);
	assert_eq!(UART.stray(), 1, "the stray read is counted");
	assert_eq!(UART.model().accesses(), before, "and it never reached the UART");
	UART.hand_back(false);
	assert_eq!(UART.stray(), 0, "the reacquisition reports the count and starts it again");
}

crate::tagged_test!(the_release_reads_out_the_receiver_before_the_boot_programming_and_arms_the_line_again, [Kernel, Console], id = "kernel.arch.common.console_uart.the_release_reads_out_the_receiver_before_the_boot_programming", covers = ["kernel"]);
fn the_release_reads_out_the_receiver_before_the_boot_programming_and_arms_the_line_again() {
	static UART: Uart<Fake> = Uart::new(Fake::new(0xF400_0000), false);
	UART.enable_async();
	UART.keep_firmware_settings();
	// A CONTROLLER THAT REFUSES leaves the UART's half undone, and no handler kept.
	let from = UART.model().accesses();
	assert_eq!(UART.arm_receive(handler, || Err("refused")), Err("refused"));
	assert_eq!(UART.model().accesses_from(from), [(true, RECEIVE, 1), (true, RECEIVE, 0)], "the receive interrupt enabled, then taken back");
	assert!(UART.arm_receive(handler, || Ok(())).is_ok(), "the kernel arms its receive line");
	assert!(UART.hand_over(9, 3));
	assert_eq!(UART.model().script.lock().released, 1, "the hand-over lets the kernel's line go, so the claim can bind it");
	assert!(UART.arm_receive(handler, || Ok(())).is_err(), "and nothing arms it while the driver holds the UART");
	assert!(UART.write_bytes(b"while held\n") != 0);
	UART.model().receive(b"ls\r");
	let from = UART.model().accesses();
	UART.hand_back(false);
	let accesses = UART.model().accesses_from(from);
	let prepared = accesses.iter().position(|access| *access == (true, PREPARE, 1)).expect("the receiver made readable");
	let programmed = accesses.iter().position(|access| *access == (true, BOOT, 3)).expect("the boot programming, with the receive interrupt");
	let last_read = accesses.iter().rposition(|access| !access.0 && access.1 == DATA).expect("the receiver read out");
	assert!(prepared < last_read && last_read < programmed, "made readable, read out, and only then programmed: {accesses:?}");
	assert!(accesses.contains(&(true, FIRMWARE, FIRMWARE_SETTINGS)), "with the firmware's settings, read once the scheduler was up");
	assert_eq!(UART.take_read_out(), b"ls\r", "what the receiver held is kept");
	assert_eq!(UART.model().script.lock().rearmed, 1, "and the kernel's line is armed again");
	assert_eq!(UART.owner(), Owner::Kernel);
	UART.drain_tx();
	assert_eq!(UART.model().wire(), b"while held\r\n", "what queued while the driver held the UART goes out once the kernel has it");
	// A QUARANTINED RELEASE RETURNS THE UART ALL THE SAME.
	assert!(UART.hand_over(10, 3));
	UART.hand_back(true);
	assert_eq!(UART.owner(), Owner::Kernel);
	UART.hand_back(false);
	assert_eq!(UART.model().script.lock().rearmed, 2, "a release with no claim holding the UART does nothing");
}

crate::tagged_test!(the_sleep_lends_a_held_uart_to_the_kernel_and_gives_it_back_with_the_driver_s_settings, [Kernel, Console], id = "kernel.arch.common.console_uart.the_sleep_lends_a_held_uart_to_the_kernel", covers = ["kernel"]);
fn the_sleep_lends_a_held_uart_to_the_kernel_and_gives_it_back_with_the_driver_s_settings() {
	static UART: Uart<Fake> = Uart::new(Fake::new(0xF500_0000), false);
	UART.enable_async();
	UART.keep_firmware_settings();
	// THE KERNEL'S OWN UART: the window drains the ring and every line goes out synchronously.
	assert!(UART.write_bytes(b"queued") != 0);
	UART.sleep_begin();
	assert_eq!(UART.model().wire(), b"queued");
	assert_eq!(UART.write_bytes(b"in the window\n"), 14);
	assert_eq!(UART.model().wire(), b"in the window\r\n", "synchronously, inside the window");
	UART.sleep_end();
	assert_eq!(UART.owner(), Owner::Kernel);
	// A DRIVER'S UART: lent for the window, programmed with the receive interrupt off, and given back.
	assert!(UART.hand_over(11, 4));
	assert!(UART.write_bytes(b"for the tap\n") != 0);
	let from = UART.model().accesses();
	UART.sleep_begin();
	assert_eq!(UART.owner(), Owner::Sleep(11));
	assert!(UART.held_by(11), "the claim still holds the UART it lent");
	let accesses = UART.model().accesses_from(from);
	assert_eq!(accesses[0], (false, LENT, DRIVER_SETTINGS), "the driver's settings read first");
	assert_eq!(accesses[1], (true, BOOT, 1), "then the boot programming, the receive interrupt left off");
	assert_eq!(UART.model().wire(), b"for the tap\r\n", "and the ring drained to the wire");
	assert_eq!(UART.write_bytes(b"sleep: entered\n"), 15);
	assert_eq!(UART.model().wire(), b"sleep: entered\r\n");
	// THE WAKE FROM A SLEEP THAT LOST THE SETTINGS: programmed again before the next line's first byte.
	UART.sleep_wake(true);
	let from = UART.model().accesses();
	UART.write_byte(b'r');
	let accesses = UART.model().accesses_from(from);
	assert_eq!(accesses[0], (true, BOOT, 1), "the programming comes first, still without the receive interrupt");
	assert_eq!(*accesses.last().expect("the byte"), (true, DATA, u32::from(b'r')));
	let from = UART.model().accesses();
	UART.sleep_end();
	assert_eq!(UART.model().accesses_from(from), [(true, LENT, DRIVER_SETTINGS)], "the driver's settings written back");
	assert_eq!(UART.owner(), Owner::Driver(11));
	assert_eq!(UART.stray(), 0, "nothing the entry did is counted against the driver's hold");
	UART.hand_back(false);
}

crate::tagged_test!(the_terminal_writer_takes_the_uart_from_a_driver_and_puts_the_dropped_count_on_the_wire, [Kernel, Console], id = "kernel.arch.common.console_uart.the_terminal_writer_takes_the_uart_from_a_driver", covers = ["kernel"]);
fn the_terminal_writer_takes_the_uart_from_a_driver_and_puts_the_dropped_count_on_the_wire() {
	static UART: Uart<Fake> = Uart::new(Fake::new(0xF600_0000), false);
	UART.enable_async();
	UART.keep_firmware_settings();
	assert!(UART.hand_over(12, 5));
	// A DRIVER THAT STOPPED DRAINING, and a ring past its bound.
	for _ in 0..TX_RING_CAP + 3 {
		UART.write_byte(b'z');
	}
	let from = UART.model().accesses();
	UART.terminal();
	let accesses = UART.model().accesses_from(from);
	assert_eq!(accesses[0], (true, BOOT, 1), "the whole boot programming first, whatever the driver left");
	assert_eq!(accesses[1], (true, FIRMWARE, FIRMWARE_SETTINGS));
	assert_eq!(UART.owner(), Owner::Terminal);
	let wire = UART.model().wire();
	assert!(wire[..TX_RING_CAP].iter().all(|&byte| byte == b'z'), "the backlog first");
	assert_eq!(&wire[TX_RING_CAP..], b"\r\nconsole: 3 byte(s) of kernel output were dropped at the ring's bound before this point\r\n", "then the count the bound dropped");
	assert!(!UART.hand_over(13, 5), "the terminal owner is never left");
	assert_eq!(UART.write_bytes(b"panic\n"), 6);
	assert_eq!(UART.model().wire(), b"panic\r\n", "and every line after it goes out synchronously");
	UART.terminal();
	assert!(UART.model().wire().is_empty(), "entered again, it writes nothing of its own");
}
