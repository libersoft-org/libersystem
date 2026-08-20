// KERN-ARCH-023: the CMOS clock's waits are bounded and its decoding is a decision, not a guess.
//
// Driven through `snapshot`'s `read` parameter rather than the ports, which is the only way these
// cases exist at all: an RTC wedged with `UIP` set, or one whose registers never agree, is not
// something QEMU can be asked for.

use super::{SNAPSHOT_TRIES, UIP_SPINS, decode, snapshot};
use core::cell::Cell;

const REG_SECONDS: u8 = 0x00;
const REG_MINUTES: u8 = 0x02;
const REG_HOURS: u8 = 0x04;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
const REG_CENTURY: u8 = 0x32;
const REG_STATUS_A: u8 = 0x0a;

// A clock reading 2024-05-17 13:45:30, in binary, 24-hour.
const BINARY_24H: u8 = 0x04 | 0x02;
fn stable(reg: u8) -> u8 {
	match reg {
		REG_STATUS_A => 0,
		REG_SECONDS => 30,
		REG_MINUTES => 45,
		REG_HOURS => 13,
		REG_DAY => 17,
		REG_MONTH => 5,
		REG_YEAR => 24,
		REG_CENTURY => 20,
		_ => 0,
	}
}

crate::tagged_test!(a_clock_that_never_finishes_updating_is_given_up_on, [Kernel, ArchX86_64], id = "kernel.arch.x86_64.rtc.a_clock_that_never_finishes_updating_is_given_up_on", covers = ["kernel"]);
fn a_clock_that_never_finishes_updating_is_given_up_on() {
	// `while updating() {}` with nothing to end it. An absent or wedged RTC leaves that bit set,
	// and `SYS_CLOCK_RTC` reaches this from a syscall entry with interrupts masked - so ring 3
	// could take a core out of the machine with a clock read and no timer left to recover it.
	let reads = Cell::new(0u32);
	let always_updating = |reg: u8| {
		reads.set(reads.get() + 1);
		if reg == REG_STATUS_A { 0x80 } else { 0 }
	};
	assert_eq!(snapshot(&always_updating, 32, SNAPSHOT_TRIES), None, "a clock stuck mid-update is not a time");
	assert!(reads.get() <= 32, "and the wait stopped at its budget rather than running on: {} reads", reads.get());
}

crate::tagged_test!(a_clock_whose_registers_never_agree_is_not_a_time, [Kernel, ArchX86_64], id = "kernel.arch.x86_64.rtc.a_clock_whose_registers_never_agree_is_not_a_time", covers = ["kernel"]);
fn a_clock_whose_registers_never_agree_is_not_a_time() {
	// The other unbounded loop: two passes had to agree, and a clock ticking faster than the reads
	// - or returning noise - never produces two that do.
	let tick = Cell::new(0u8);
	let reads = Cell::new(0u32);
	let never_settles = |reg: u8| {
		reads.set(reads.get() + 1);
		if reg == REG_STATUS_A {
			return 0;
		}
		tick.set(tick.get().wrapping_add(1));
		tick.get()
	};
	assert_eq!(snapshot(&never_settles, UIP_SPINS, 8), None, "no two passes agreed, so there is no reading");
	// EIGHT PASSES, and a pass is one status read plus seven register reads. The count is what
	// makes the bound a bound: without it "returns None" is also what an unbounded loop does after
	// the test has already hung.
	assert!(reads.get() <= 8 * 8, "the snapshot took {} reads for a budget of 8 passes", reads.get());

	// AND A CLOCK THAT SETTLES IS STILL READ. The bound must not turn a working RTC into a
	// timeout: this one changes once and then holds still, which is what a real tick looks like.
	let pass = Cell::new(0u32);
	let settles_after_one_tick = |reg: u8| {
		if reg == REG_SECONDS {
			pass.set(pass.get() + 1);
		}
		if pass.get() <= 1 { stable(reg).wrapping_add(1) } else { stable(reg) }
	};
	assert_eq!(snapshot(&settles_after_one_tick, UIP_SPINS, SNAPSHOT_TRIES), Some([30, 45, 13, 17, 5, 24, 20]), "the reading is the two passes that agreed");
}

crate::tagged_test!(the_registers_are_decoded_the_way_the_status_byte_says, [Kernel, ArchX86_64], id = "kernel.arch.x86_64.rtc.the_registers_are_decoded_the_way_the_status_byte_says", covers = ["kernel"]);
fn the_registers_are_decoded_the_way_the_status_byte_says() {
	// The decoding was never covered by anything, and it is where a wall clock goes wrong quietly:
	// BCD read as binary is a plausible wrong time, not an obvious one.
	const WANT: u64 = 1_715_953_530; // 2024-05-17 13:45:30 UTC
	assert_eq!(snapshot(&stable, UIP_SPINS, SNAPSHOT_TRIES), Some([30, 45, 13, 17, 5, 24, 20]));
	assert_eq!(decode([30, 45, 13, 17, 5, 24, 20], BINARY_24H), WANT, "binary, 24-hour");
	// The same instant in BCD, which is what the hardware actually reports by default.
	assert_eq!(decode([0x30, 0x45, 0x13, 0x17, 0x05, 0x24, 0x20], 0x02), WANT, "BCD, 24-hour");
	// And in 12-hour form: 1:45:30 PM, the PM flag riding bit 7 of the hours register.
	assert_eq!(decode([0x30, 0x45, 0x80 | 0x01, 0x17, 0x05, 0x24, 0x20], 0x00), WANT, "BCD, 12-hour PM");
	// Midnight in 12-hour form is hour 12 with the PM flag CLEAR, which is the case that reads as
	// noon if the flag is the only thing looked at.
	assert_eq!(decode([0x00, 0x00, 0x12, 0x01, 0x01, 0x00, 0x20], 0x00), 946_684_800, "2000-01-01 00:00:00");
	// A century register the machine does not have: the year is taken as 20xx rather than 00xx.
	assert_eq!(decode([0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0xff], 0x02), 946_684_800, "an implausible century falls back to the 2000s");
	// And a date the clock cannot be reporting is no time at all rather than an arbitrary one.
	assert_eq!(decode([0x00, 0x00, 0x00, 0x01, 0x13, 0x24, 0x20], 0x02), 0, "month 13");
	assert_eq!(decode([0x00, 0x00, 0x00, 0x00, 0x01, 0x24, 0x20], 0x02), 0, "day 0");
	assert_eq!(decode([0x00, 0x00, 0x25, 0x01, 0x01, 0x24, 0x20], BINARY_24H), 0, "hour 25");
}
