use super::*;
use alloc::vec::Vec;

// A PL011 AS A SCRIPT: what the flag register and the receiver answer, and every write in order.
#[derive(Default)]
struct Script {
	writes: Vec<(u16, u32)>,
	// Answered by each FR read in turn; `idle` once the list runs out.
	flags: Vec<u32>,
	idle: u32,
	// Each DR read takes the next word: a byte, with its error bits.
	received: Vec<u32>,
	// What MIS answers.
	raised: u32,
}

impl Registers for Script {
	fn read(&mut self, offset: u16) -> u32 {
		match offset {
			FR => {
				if self.flags.is_empty() {
					self.idle
				} else {
					self.flags.remove(0)
				}
			}
			DR => {
				if self.received.is_empty() {
					0
				} else {
					self.received.remove(0)
				}
			}
			MIS => self.raised,
			_ => 0,
		}
	}

	fn write(&mut self, offset: u16, value: u32) {
		self.writes.push((offset, value));
	}
}

#[test]
fn the_divisor_comes_from_the_supplied_clock_with_its_fraction_and_an_unreachable_rate_is_refused() {
	// QEMU virt's 24 MHz `apb-pclk` at 115200: 13.0208 is 13 and 1/64 (13.0156), 0.04% fast.
	assert_eq!(Line::new(24_000_000, 115_200), Some(Line { ibrd: 13, fbrd: 1 }));
	// At 38400 it divides exactly: 39.0625 is 39 and 4/64.
	assert_eq!(Line::new(24_000_000, 38_400), Some(Line { ibrd: 39, fbrd: 4 }));
	// A 48 MHz clock at 115200: 26.0417, rounded to 26 and 3/64.
	assert_eq!(Line::new(48_000_000, 115_200), Some(Line { ibrd: 26, fbrd: 3 }));
	// A 3.6864 MHz clock makes 115200 exactly with a divisor of 2.
	assert_eq!(Line::new(3_686_400, 115_200), Some(Line { ibrd: 2, fbrd: 0 }));
	// Nothing from a clock of nothing or a rate of nothing; no divisor below one - a clock slower than sixteen times
	// the rate - and none past the integer register's sixteen bits.
	assert_eq!(Line::new(0, 115_200), None);
	assert_eq!(Line::new(24_000_000, 0), None);
	assert_eq!(Line::new(1_000_000, 115_200), None);
	assert_eq!(Line::new(16 * 65_536 * 110, 110), None, "a divisor of 65536 does not fit");
	assert_eq!(Line::new(16 * 65_535 * 110, 110), Some(Line { ibrd: 0xFFFF, fbrd: 0 }), "and 65535 with no fraction does");
}

#[test]
fn programming_follows_the_trm_order_keeps_an_undescribed_divisor_and_unmasks_receive_last() {
	let mut uart = Pl011::new(Script { flags: alloc::vec![FR_BUSY, FR_BUSY, 0], ..Script::default() });
	uart.program(Some(Line { ibrd: 13, fbrd: 1 }), true);
	assert_eq!(
		uart.regs.writes,
		[
			(IMSC, 0),
			(CR, 0),
			(LCR_H, 0),
			(IBRD, 13),
			(FBRD, 1),
			(LCR_H, LCR_H_8N1_FIFO),
			(IFLS, IFLS_EIGHTHS),
			(DMACR, 0),
			(ICR, INT_ALL),
			(CR, CR_UARTEN | CR_TXE | CR_RXE),
			(IMSC, INT_RX | INT_RT)
		],
		"masked, disabled once the character on the wire was out, the FIFOs flushed, the divisor, the line control that latches it, the trigger levels, DMA off, everything cleared, enabled - and the receive interrupts last"
	);
	assert!(uart.regs.flags.is_empty(), "the busy flag was waited out before the UART was disabled");
	// NO CLOCK DESCRIBED: the divisor the UART holds is kept - the line control still latches it.
	let mut uart = Pl011::new(Script::default());
	uart.program(None, false);
	assert!(!uart.regs.writes.iter().any(|&(offset, _)| offset == IBRD || offset == FBRD), "no divisor is written without a clock to divide");
	assert_eq!(uart.regs.writes.last(), Some(&(IMSC, 0)), "and nothing is unmasked when the receive interrupt is not asked for");
	uart.regs.writes.clear();
	uart.quiet();
	assert_eq!(uart.regs.writes, [(IMSC, 0)], "a stop leaves every interrupt masked");
}

#[test]
fn a_uart_that_never_stops_being_busy_is_disabled_after_the_bound_rather_than_waited_on_forever() {
	let mut uart = Pl011::new(Script { idle: FR_BUSY, ..Script::default() });
	uart.program(None, true);
	assert_eq!(uart.regs.writes[1], (CR, 0), "disabled once the bound passed");
}

#[test]
fn the_transmit_interrupt_is_unmasked_only_while_a_write_waits_and_keeps_the_receive_mask() {
	let mut uart = Pl011::new(Script::default());
	uart.program(None, true);
	uart.regs.writes.clear();
	uart.transmit_interrupt(true);
	uart.transmit_interrupt(true);
	uart.transmit_interrupt(false);
	assert_eq!(uart.regs.writes, [(IMSC, INT_RX | INT_RT | INT_TX), (IMSC, INT_RX | INT_RT)], "on once, off once, the receive interrupts kept");
	uart.regs.writes.clear();
	uart.transmit_interrupt(true);
	uart.receive_interrupt(false);
	uart.receive_interrupt(false);
	uart.receive_interrupt(true);
	assert_eq!(uart.regs.writes, [(IMSC, INT_RX | INT_RT | INT_TX), (IMSC, INT_TX), (IMSC, INT_TX | INT_RX | INT_RT)], "receive off once and on once, the transmit interrupt kept");
}

#[test]
fn the_transmitter_takes_bytes_until_its_fifo_is_full_and_nothing_once_it_is() {
	let mut uart = Pl011::new(Script { flags: alloc::vec![0, 0, 0, FR_TXFF], ..Script::default() });
	assert_eq!(uart.put(b"console"), 3, "three bytes went in before the FIFO reported full");
	assert_eq!(uart.regs.writes, [(DR, u32::from(b'c')), (DR, u32::from(b'o')), (DR, u32::from(b'n'))]);
	let mut uart = Pl011::new(Script { idle: FR_TXFF, ..Script::default() });
	assert_eq!(uart.put(b"x"), 0, "a full FIFO takes nothing");
	assert!(uart.regs.writes.is_empty());
	let mut uart = Pl011::new(Script::default());
	assert_eq!(uart.put(&[]), 0, "and nothing is nothing");
	assert_eq!(uart.put(b"ok\r\n"), 4, "a FIFO with room takes all of it");
}

#[test]
fn every_received_byte_is_read_while_there_is_room_damage_is_counted_and_the_timeout_cleared_when_empty() {
	// Three bytes, the second carrying an overrun, then the FIFO empty.
	let mut uart = Pl011::new(Script { flags: alloc::vec![0, 0, 0, FR_RXFE], received: alloc::vec![u32::from(b'l'), u32::from(b's') | 0x800, u32::from(b'\r')], ..Script::default() });
	let mut out = [0u8; 8];
	assert_eq!(uart.receive(&mut out), 3);
	assert_eq!(&out[..3], b"ls\r", "the byte itself is kept even when its error bits are set");
	assert_eq!(uart.errors(), 1, "the overrun the data register reported is counted");
	assert_eq!(uart.regs.writes, [(RSR_ECR, 0), (ICR, INT_RT)], "its error cleared, and the timeout cleared once the FIFO was found empty");
	// ROOM FOR TWO: the third stays in the FIFO for the next call, and the timeout is not cleared under it.
	let mut uart = Pl011::new(Script { received: alloc::vec![1, 2, 3], ..Script::default() });
	let mut two = [0u8; 2];
	assert_eq!(uart.receive(&mut two), 2);
	assert_eq!(two, [1, 2]);
	assert_eq!(uart.regs.received, [3], "the third byte is still the UART's");
	assert!(uart.regs.writes.is_empty(), "and nothing was cleared while it waits");
}

#[test]
fn acknowledging_clears_the_transmit_interrupt_and_answers_what_was_raised() {
	let mut uart = Pl011::new(Script { raised: INT_TX | INT_RX, ..Script::default() });
	assert_eq!(uart.acknowledge(), INT_TX | INT_RX);
	assert_eq!(uart.regs.writes, [(ICR, INT_TX)], "only the transmit interrupt: the receive ones clear as the FIFO is read");
}
