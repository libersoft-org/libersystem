use super::*;
use alloc::vec::Vec;

// A UART AS A SCRIPT: what the line status and the receiver answer, and every write in order.
#[derive(Default)]
struct Script {
	writes: Vec<(u16, u8)>,
	// Answered by each LSR read in turn; `idle` once the list runs out.
	lsr: Vec<u8>,
	idle: u8,
	received: Vec<u8>,
}

impl Registers for Script {
	fn read(&mut self, offset: u16) -> u8 {
		match offset {
			LSR => {
				if self.lsr.is_empty() {
					self.idle
				} else {
					self.lsr.remove(0)
				}
			}
			RBR_THR => {
				if self.received.is_empty() {
					0
				} else {
					self.received.remove(0)
				}
			}
			_ => 0,
		}
	}

	fn write(&mut self, offset: u16, value: u8) {
		self.writes.push((offset, value));
	}
}

#[test]
fn the_console_rate_from_a_pc_clock_is_divisor_three_and_an_unreachable_rate_is_refused() {
	assert_eq!(Line::new(PC_CLOCK_HZ, CONSOLE_BAUD), Some(Line { divisor: 3 }));
	assert_eq!(Line::new(PC_CLOCK_HZ, 115_200), Some(Line { divisor: 1 }));
	assert_eq!(Line::new(PC_CLOCK_HZ, 9_600), Some(Line { divisor: 12 }));
	// 1.8432 MHz cannot make 250 kbaud within 3% (the nearest divisor, 0, is no divisor at all), nor anything
	// from a clock of nothing.
	assert_eq!(Line::new(PC_CLOCK_HZ, 250_000), None);
	assert_eq!(Line::new(0, CONSOLE_BAUD), None);
	assert_eq!(Line::new(PC_CLOCK_HZ, 0), None);
	// A 48 MHz clock at 38400 needs divisor 78 (38461 baud, 0.2% fast), which is within the bound.
	assert_eq!(Line::new(48_000_000, CONSOLE_BAUD), Some(Line { divisor: 78 }));
}

#[test]
fn programming_writes_every_register_a_driver_could_have_left_and_the_receive_interrupt_last() {
	let mut uart = Uart::new(Script::default());
	uart.program(Line { divisor: 0x0103 }, true);
	assert_eq!(uart.regs.writes, [(IER, 0), (LCR, LCR_DLAB), (RBR_THR, 0x03), (IER, 0x01), (LCR, LCR_8N1), (IIR_FCR, FCR_ENABLE_AND_CLEAR), (MCR, MCR_CONSOLE), (IER, IER_RX_AVAILABLE)], "interrupts off, the divisor through the latch, the latch closed with 8N1, the FIFOs, modem control with loopback off, and the receive interrupt last");
	uart.regs.writes.clear();
	uart.quiet();
	assert_eq!(uart.regs.writes, [(IER, 0)], "a stop leaves every interrupt enable off");
}

#[test]
fn the_transmit_interrupt_is_on_only_while_a_write_waits_and_keeps_the_receive_enable() {
	let mut uart = Uart::new(Script::default());
	uart.program(Line { divisor: 3 }, true);
	uart.regs.writes.clear();
	uart.transmit_interrupt(true);
	uart.transmit_interrupt(true);
	uart.transmit_interrupt(false);
	assert_eq!(uart.regs.writes, [(IER, IER_RX_AVAILABLE | IER_TX_EMPTY), (IER, IER_RX_AVAILABLE)], "on once, off once, the receive enable kept");
}

#[test]
fn the_receive_interrupt_goes_off_while_nothing_has_room_and_keeps_the_transmit_enable() {
	let mut uart = Uart::new(Script::default());
	uart.program(Line { divisor: 3 }, true);
	uart.transmit_interrupt(true);
	uart.regs.writes.clear();
	uart.receive_interrupt(false);
	uart.receive_interrupt(false);
	uart.receive_interrupt(true);
	assert_eq!(uart.regs.writes, [(IER, IER_TX_EMPTY), (IER, IER_TX_EMPTY | IER_RX_AVAILABLE)], "off once, on once, the transmit enable kept");
}

#[test]
fn a_full_holding_register_takes_nothing_and_an_empty_one_takes_a_fifo_load() {
	let mut uart = Uart::new(Script { lsr: alloc::vec![0, LSR_THR_EMPTY], ..Script::default() });
	let bytes = [b'x'; 40];
	assert_eq!(uart.put(&bytes), 0, "the transmitter is busy");
	assert!(uart.regs.writes.is_empty(), "and nothing was written");
	assert_eq!(uart.put(&bytes), TX_FIFO, "an empty holding register takes one FIFO load");
	assert_eq!(uart.regs.writes.len(), TX_FIFO);
	assert!(uart.regs.writes.iter().all(|&(offset, value)| offset == RBR_THR && value == b'x'));
	assert_eq!(uart.put(&[]), 0, "and nothing is nothing");
}

#[test]
fn every_received_byte_is_read_while_there_is_room_and_damage_is_counted() {
	let mut uart = Uart::new(Script { lsr: alloc::vec![LSR_DATA_READY, LSR_DATA_READY | 0x02, LSR_DATA_READY, 0], received: alloc::vec![b'l', b's', b'\r'], ..Script::default() });
	let mut out = [0u8; 8];
	assert_eq!(uart.receive(&mut out), 3);
	assert_eq!(&out[..3], b"ls\r");
	assert_eq!(uart.errors(), 1, "the overrun the line status reported is counted");
	// ROOM FOR TWO: the third stays in the FIFO for the next call, and its status is not read yet.
	let mut uart = Uart::new(Script { lsr: alloc::vec![LSR_DATA_READY, LSR_DATA_READY, LSR_DATA_READY], received: alloc::vec![1, 2, 3], ..Script::default() });
	let mut two = [0u8; 2];
	assert_eq!(uart.receive(&mut two), 2);
	assert_eq!(two, [1, 2]);
	assert_eq!(uart.regs.received, [3], "the third byte is still the UART's");
}

#[test]
fn input_held_for_a_consumer_keeps_the_oldest_and_counts_what_the_bound_dropped() {
	let mut held = Held::default();
	assert!(held.is_empty());
	assert_eq!(held.room(), HELD_INPUT);
	assert_eq!(held.push(&[b'a'; 500]), 500);
	assert_eq!(held.push(b"0123456789abcdef"), 12, "twelve more fit the bound");
	assert_eq!(held.bytes().len(), HELD_INPUT);
	assert_eq!(held.room(), 0, "and nothing more has room");
	assert_eq!(&held.bytes()[500..], b"0123456789ab", "the newest are the ones dropped");
	assert_eq!(held.dropped(), 4);
	held.clear();
	assert!(held.is_empty());
	assert_eq!(held.dropped(), 4, "the count outlives what was handed on");
}

#[test]
fn the_dropped_marker_names_the_count_on_a_line_of_its_own() {
	let mut out = [0u8; 96];
	let n = dropped_marker(1234, &mut out);
	assert_eq!(&out[..n], b"\r\n[console: 1234 byte(s) of kernel output dropped at the ring's bound]\r\n");
	let n = dropped_marker(0, &mut out);
	assert!(out[..n].starts_with(b"\r\n[console: 0 byte(s)"));
	let n = dropped_marker(u64::MAX, &mut out);
	assert!(out[..n].ends_with(b"]\r\n"), "the largest count still fits");
}
