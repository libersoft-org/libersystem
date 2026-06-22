// Hand-written rendering for the generated `Timestamp` wire type: the canonical
// time object renders itself as ISO-8601 (one representation of the same seconds),
// the way `Ipv4Addr` renders its octets. The wall-clock policy (the offset, NTP)
// lives in the userspace TimeService; this is purely how the value is displayed.

use crate::system::Timestamp;

impl Timestamp {
	// Render this instant as ISO-8601 UTC "YYYY-MM-DDTHH:MM:SSZ" into `out` (which
	// must hold at least 20 bytes), returning the number of bytes written.
	pub fn render(&self, out: &mut [u8]) -> usize {
		let secs: i64 = self.unix_secs as i64;
		let days: i64 = secs.div_euclid(86400);
		let tod: i64 = secs.rem_euclid(86400);
		let (year, month, day): (i64, u32, u32) = civil_from_days(days);
		let hour: u32 = (tod / 3600) as u32;
		let minute: u32 = (tod % 3600 / 60) as u32;
		let second: u32 = (tod % 60) as u32;
		let mut p: usize = 0;
		p += write_u4(year, &mut out[p..]);
		out[p] = b'-';
		p += 1;
		p += write_u2(month, &mut out[p..]);
		out[p] = b'-';
		p += 1;
		p += write_u2(day, &mut out[p..]);
		out[p] = b'T';
		p += 1;
		p += write_u2(hour, &mut out[p..]);
		out[p] = b':';
		p += 1;
		p += write_u2(minute, &mut out[p..]);
		out[p] = b':';
		p += 1;
		p += write_u2(second, &mut out[p..]);
		out[p] = b'Z';
		p += 1;
		p
	}
}

// The civil date (year, month, day) for a count of days since the Unix epoch, via
// Howard Hinnant's `civil_from_days` (the inverse of `days_from_civil`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
	let z: i64 = days + 719468;
	let era: i64 = if z >= 0 { z } else { z - 146096 } / 146097;
	let doe: i64 = z - era * 146097;
	let yoe: i64 = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
	let y: i64 = yoe + era * 400;
	let doy: i64 = doe - (365 * yoe + yoe / 4 - yoe / 100);
	let mp: i64 = (5 * doy + 2) / 153;
	let day: i64 = doy - (153 * mp + 2) / 5 + 1;
	let month: i64 = if mp < 10 { mp + 3 } else { mp - 9 };
	let year: i64 = if month <= 2 { y + 1 } else { y };
	(year, month as u32, day as u32)
}

// Write a zero-padded 2-digit field into `out`, returning 2.
fn write_u2(v: u32, out: &mut [u8]) -> usize {
	out[0] = b'0' + (v / 10 % 10) as u8;
	out[1] = b'0' + (v % 10) as u8;
	2
}

// Write a zero-padded 4-digit field into `out`, returning 4 (years past 9999 wrap).
fn write_u4(v: i64, out: &mut [u8]) -> usize {
	let v: u32 = (v.rem_euclid(10000)) as u32;
	out[0] = b'0' + (v / 1000 % 10) as u8;
	out[1] = b'0' + (v / 100 % 10) as u8;
	out[2] = b'0' + (v / 10 % 10) as u8;
	out[3] = b'0' + (v % 10) as u8;
	4
}
