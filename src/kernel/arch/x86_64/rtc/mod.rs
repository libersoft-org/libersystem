// The CMOS / MC146818 real-time clock (the battery-backed wall clock the firmware
// keeps), read through the index/data port pair 0x70/0x71. This is the only
// hardware source of wall-clock time on the box; the kernel exposes it as raw
// mechanism (a Unix epoch read) and the userspace TimeService is the policy that
// disciplines it against NTP and combines it with the monotonic clock.

use super::port::{inb, outb};
use crate::sync::SpinLock;

// THE INDEX/DATA PAIR IS ONE PIECE OF STATE, AND EVERY CPU SHARES IT (KERN-ARCH-023).
//
// A CMOS read is two port accesses - select at 0x70, then read at 0x71 - and nothing held them
// together. Another core selecting its own register in between meant the first core read whatever
// the second had selected: a minute where the hour should be, or a status byte, silently and with
// the right shape. Every access below goes through this.
static CMOS: SpinLock<()> = SpinLock::new(());

// AND EVERY WAIT IS BOUNDED (KERN-ARCH-023).
//
// The update-in-progress wait and the two-agreeing-snapshots loop were both unbounded. An RTC that
// is absent, or wedged with `UIP` stuck, or ticking faster than the reads, spins forever - and
// `SYS_CLOCK_RTC` reaches here from a syscall entry with interrupts masked, so ring 3 could wedge a
// core with no timer left to take it back. The numbers are budgets, not measurements: one update
// takes under 2 ms and the registers change once a second, so a clock behaving as specified is
// nowhere near either.
const UIP_SPINS: u32 = 5_000_000;
const SNAPSHOT_TRIES: u32 = 16;

// CMOS register indices.
const REG_SECONDS: u8 = 0x00;
const REG_MINUTES: u8 = 0x02;
const REG_HOURS: u8 = 0x04;
const REG_DAY: u8 = 0x07;
const REG_MONTH: u8 = 0x08;
const REG_YEAR: u8 = 0x09;
const REG_CENTURY: u8 = 0x32;
const REG_STATUS_A: u8 = 0x0a;
const REG_STATUS_B: u8 = 0x0b;

// Status A bit 7: an RTC update is in progress (the time registers are mid-change).
const STATUS_A_UPDATING: u8 = 0x80;
// Status B bit 1: hours are in 24-hour format. Bit 2: the registers are binary
// (otherwise BCD). Bit 7 (PM) of the hours register, in 12-hour mode.
const STATUS_B_24H: u8 = 0x02;
const STATUS_B_BINARY: u8 = 0x04;
const HOURS_PM: u8 = 0x80;

// Read one CMOS register. The caller holds `CMOS`.
//
// Bit 7 of the index port is the NMI MASK: a one there disables non-maskable interrupts until
// something writes a zero. Every register index here is below 0x80, so the old code left NMI
// enabled by arithmetic rather than by decision. Stated, because a future index with the high bit
// set would silently turn NMI off for the life of the machine.
unsafe fn read_reg(reg: u8) -> u8 {
	unsafe {
		outb(0x70, reg & 0x7f); // high bit clear: NMI stays enabled
		inb(0x71)
	}
}

// ONE CMOS NVRAM BYTE, for the firmware interpreter's `SystemCMOS` regions: under the same lock as every clock read.
// The caller has checked the index is not the clock's.
pub fn nvram_read(index: u8) -> u8 {
	let _held = CMOS.lock();
	// SAFETY: an index below 0x80, selected and read under the lock.
	unsafe { read_reg(index) }
}

pub fn nvram_write(index: u8, value: u8) {
	let _held = CMOS.lock();
	// SAFETY: as above; bit 7 of the index port stays clear, so NMI stays enabled.
	unsafe {
		outb(0x70, index & 0x7f);
		outb(0x71, value);
	}
}

// Decode a BCD byte (each nibble a decimal digit) to binary.
fn bcd_to_bin(v: u8) -> u8 {
	(v & 0x0f) + (v >> 4) * 10
}

// Days since the Unix epoch (1970-01-01) for the given civil date, via Howard
// Hinnant's `days_from_civil` (valid for any proleptic Gregorian date).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
	let y: i64 = if month <= 2 { year - 1 } else { year };
	let era: i64 = if y >= 0 { y } else { y - 399 } / 400;
	let yoe: i64 = y - era * 400;
	let mp: i64 = if month > 2 { month - 3 } else { month + 9 };
	let doy: i64 = (153 * mp + 2) / 5 + day - 1;
	let doe: i64 = yoe * 365 + yoe / 4 - yoe / 100 + doy;
	era * 146097 + doe - 719468
}

// A stable reading of the seven time registers, or None if the clock never settled.
//
// Two consecutive passes must agree, which is what rejects a read straddling an RTC update. Both
// the wait for `UIP` to clear and the number of passes are bounded, so a clock that is absent,
// wedged or changing continuously ends this function instead of owning the core.
//
// `read` is a parameter so the loop can be driven by something other than the ports: the bound is
// the property worth testing, and a test that needed real CMOS hardware could not test it.
pub(crate) fn snapshot(read: impl Fn(u8) -> u8, uip_spins: u32, tries: u32) -> Option<[u8; 7]> {
	let mut prev: Option<[u8; 7]> = None;
	for _ in 0..tries {
		let mut spins: u32 = 0;
		while read(REG_STATUS_A) & STATUS_A_UPDATING != 0 {
			spins += 1;
			if spins >= uip_spins {
				return None;
			}
		}
		let snap: [u8; 7] = [read(REG_SECONDS), read(REG_MINUTES), read(REG_HOURS), read(REG_DAY), read(REG_MONTH), read(REG_YEAR), read(REG_CENTURY)];
		if prev == Some(snap) {
			return Some(snap);
		}
		prev = Some(snap);
	}
	None
}

// Turn a stable snapshot and the status B byte into a Unix timestamp, or 0 for a date the clock
// cannot plausibly be reporting. Split out for the same reason as `snapshot`: BCD, the 12-hour PM
// flag and the century register are decisions worth testing, and none of them needs a port.
pub(crate) fn decode(snap: [u8; 7], status_b: u8) -> u64 {
	{
		let binary: bool = status_b & STATUS_B_BINARY != 0;
		let h24: bool = status_b & STATUS_B_24H != 0;

		let mut second: u8 = snap[0];
		let mut minute: u8 = snap[1];
		let raw_hours: u8 = snap[2];
		let mut day: u8 = snap[3];
		let mut month: u8 = snap[4];
		let mut year: u8 = snap[5];
		let mut century: u8 = snap[6];
		// The PM flag rides bit 7 of the hours register in 12-hour mode; strip it
		// before decoding the hour value, then re-apply after.
		let mut hour: u8 = raw_hours & !HOURS_PM;

		if !binary {
			second = bcd_to_bin(second);
			minute = bcd_to_bin(minute);
			hour = bcd_to_bin(hour);
			day = bcd_to_bin(day);
			month = bcd_to_bin(month);
			year = bcd_to_bin(year);
			century = bcd_to_bin(century);
		}
		if !h24 && raw_hours & HOURS_PM != 0 {
			hour = (hour % 12) + 12;
		} else if !h24 && hour == 12 {
			hour = 0;
		}

		// QEMU exposes the century register; if it is implausible, assume the 2000s
		// (this box runs well past 2000 and the 2-digit year is otherwise ambiguous).
		let full_year: i64 = if (19..=21).contains(&century) { century as i64 * 100 + year as i64 } else { 2000 + year as i64 };

		if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
			return 0;
		}
		let days: i64 = days_from_civil(full_year, month as i64, day as i64);
		let secs: i64 = days * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
		if secs < 0 { 0 } else { secs as u64 }
	}
}

// Read the wall clock as a Unix timestamp (seconds since 1970-01-01 UTC), or 0 if the RTC reports
// an implausible date or never gives a stable reading.
pub fn read_unix() -> u64 {
	// HELD ACROSS THE WHOLE SNAPSHOT, not per register: the point is that no other core selects a
	// CMOS register between this core's select and its read, and that has to hold for the status
	// byte read afterwards too, or the decode is done against somebody else's format bits.
	let _guard = CMOS.lock();
	let read = |reg: u8| unsafe { read_reg(reg) };
	let Some(snap) = snapshot(&read, UIP_SPINS, SNAPSHOT_TRIES) else {
		return 0;
	};
	decode(snap, read(REG_STATUS_B))
}

#[cfg(test)]
mod tests;

// ------------------------------------------------------------------ the alarm an S3 wakes on

// The alarm registers, and Status B's alarm-interrupt enable. Status C is read to clear what fired.
const REG_ALARM_SECONDS: u8 = 0x01;
const REG_ALARM_MINUTES: u8 = 0x03;
const REG_ALARM_HOURS: u8 = 0x05;
const REG_STATUS_C: u8 = 0x0c;
const STATUS_B_ALARM: u8 = 0x20;

fn bin_to_bcd(v: u8) -> u8 {
	(v / 10) << 4 | (v % 10)
}

unsafe fn write_reg(reg: u8, value: u8) {
	unsafe {
		outb(0x70, reg & 0x7f);
		outb(0x71, value);
	}
}

// ARM THE CMOS ALARM `after` SECONDS FROM NOW - the S3 timed wake, which the chipset raises as RTC_STS with RTC_EN
// set. The alarm compares seconds, minutes and hours, so it reaches a day ahead and no further: `false` for more
// than that, or a clock that will not read. The time of day is read and the alarm written in the registers' own
// format - binary or BCD, 24-hour or 12-hour - as Status B says.
pub fn arm_alarm(after: u64) -> bool {
	if after == 0 || after >= 24 * 3600 {
		return false;
	}
	let _cmos = CMOS.lock();
	unsafe {
		let mut spins = 0;
		while read_reg(REG_STATUS_A) & STATUS_A_UPDATING != 0 {
			spins += 1;
			if spins > UIP_SPINS {
				return false;
			}
		}
		let status_b = read_reg(REG_STATUS_B);
		let binary = status_b & STATUS_B_BINARY != 0;
		let twenty_four = status_b & STATUS_B_24H != 0;
		let decode = |v: u8| if binary { v } else { bcd_to_bin(v) };
		let encode = |v: u8| if binary { v } else { bin_to_bcd(v) };
		let hours_raw = read_reg(REG_HOURS);
		let mut hours = decode(hours_raw & 0x7F);
		if !twenty_four {
			hours %= 12;
			if hours_raw & HOURS_PM != 0 {
				hours += 12;
			}
		}
		let now = u64::from(hours) * 3600 + u64::from(decode(read_reg(REG_MINUTES))) * 60 + u64::from(decode(read_reg(REG_SECONDS)));
		let at = (now + after) % (24 * 3600);
		let (hour, minute, second) = ((at / 3600) as u8, ((at / 60) % 60) as u8, (at % 60) as u8);
		let hour_field = if twenty_four {
			encode(hour)
		} else {
			let twelve = match hour % 12 {
				0 => 12,
				h => h,
			};
			encode(twelve) | if hour >= 12 { HOURS_PM } else { 0 }
		};
		write_reg(REG_ALARM_SECONDS, encode(second));
		write_reg(REG_ALARM_MINUTES, encode(minute));
		write_reg(REG_ALARM_HOURS, hour_field);
		let _ = read_reg(REG_STATUS_C);
		write_reg(REG_STATUS_B, status_b | STATUS_B_ALARM);
	}
	true
}

// And disarmed after the wake: the enable cleared and what fired read away.
pub fn disarm_alarm() {
	let _cmos = CMOS.lock();
	unsafe {
		let status_b = read_reg(REG_STATUS_B);
		write_reg(REG_STATUS_B, status_b & !STATUS_B_ALARM);
		let _ = read_reg(REG_STATUS_C);
	}
}
