// The CMOS / MC146818 real-time clock (the battery-backed wall clock the firmware
// keeps), read through the index/data port pair 0x70/0x71. This is the only
// hardware source of wall-clock time on the box; the kernel exposes it as raw
// mechanism (a Unix epoch read) and the userspace TimeService is the policy that
// disciplines it against NTP and combines it with the monotonic clock.

use super::port::{inb, outb};

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

// Read one CMOS register.
unsafe fn read_reg(reg: u8) -> u8 {
	unsafe {
		outb(0x70, reg);
		inb(0x71)
	}
}

// Whether the RTC is mid-update (its time registers should not be read now).
unsafe fn updating() -> bool {
	unsafe { read_reg(REG_STATUS_A) & STATUS_A_UPDATING != 0 }
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

// Read the wall clock as a Unix timestamp (seconds since 1970-01-01 UTC), or 0 if
// the RTC reports an implausible date. The fields are read twice and re-read until
// two consecutive snapshots agree, so a read straddling an RTC update is rejected.
pub fn read_unix() -> u64 {
	unsafe {
		// Take a stable snapshot: wait out any in-progress update, read the fields,
		// then re-read until two passes match (the registers did not tick mid-read).
		let mut prev: [u8; 7] = [0xff; 7];
		loop {
			while updating() {}
			let snap: [u8; 7] = [read_reg(REG_SECONDS), read_reg(REG_MINUTES), read_reg(REG_HOURS), read_reg(REG_DAY), read_reg(REG_MONTH), read_reg(REG_YEAR), read_reg(REG_CENTURY)];
			if snap == prev {
				break;
			}
			prev = snap;
		}
		let status_b: u8 = read_reg(REG_STATUS_B);
		let binary: bool = status_b & STATUS_B_BINARY != 0;
		let h24: bool = status_b & STATUS_B_24H != 0;

		let mut second: u8 = prev[0];
		let mut minute: u8 = prev[1];
		let raw_hours: u8 = prev[2];
		let mut day: u8 = prev[3];
		let mut month: u8 = prev[4];
		let mut year: u8 = prev[5];
		let mut century: u8 = prev[6];
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
