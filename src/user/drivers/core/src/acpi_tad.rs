// THE ACPI TIME AND ALARM DEVICE (`ACPI000E`) - the parts of the `acpi_tad` driver a host can test: what `_GCP` says the
// device has, what `_GRT`'s buffer says the time is, and what a sleep's timed wake programs into a timer.
//
// `_GCP`: bit 0 the AC wake timer, bit 1 the DC wake timer, bit 2 `_GRT`/`_SRT`. `_GRT`: a sixteen-byte buffer - year
// (u16), month, day, hour, minute, second, valid, milliseconds (u16), time zone (i16, minutes, 2047 unspecified), daylight
// and three bytes of pad - local time, UTC being local plus the zone. `_STV(timer, seconds)`: a timer's value, where
// 0xFFFFFFFF disables it; timer 0 is the AC timer and 1 the DC one.

/// What `_GCP` says the device has.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Capabilities {
	pub ac_wake: bool,
	pub dc_wake: bool,
	pub real_time: bool,
}

pub fn capabilities(gcp: u64) -> Capabilities {
	Capabilities { ac_wake: gcp & 1 != 0, dc_wake: gcp & 2 != 0, real_time: gcp & 4 != 0 }
}

pub const TIMER_AC: u64 = 0;
pub const TIMER_DC: u64 = 1;
/// `_STV`'s value that disables a timer.
pub const DISABLED: u64 = 0xFFFF_FFFF;
/// `_GRT`'s time zone for "unspecified", taken as UTC.
const ZONE_UNSPECIFIED: i16 = 2047;

/// `_GRT`'s buffer as seconds since the Unix epoch, UTC. Refused by name: a short buffer, a time marked not valid, a
/// field out of its range, a zone past a day.
pub fn unix_of_grt(buffer: &[u8]) -> Result<u64, &'static str> {
	if buffer.len() < 16 {
		return Err("_GRT's buffer is shorter than its sixteen bytes");
	}
	let year = u16::from_le_bytes([buffer[0], buffer[1]]) as i64;
	let (month, day, hour, minute, second, valid) = (buffer[2] as i64, buffer[3] as i64, buffer[4] as i64, buffer[5] as i64, buffer[6] as i64, buffer[7]);
	let zone = i16::from_le_bytes([buffer[10], buffer[11]]);
	if valid == 0 {
		return Err("_GRT says its time is not valid");
	}
	if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
		return Err("_GRT's time has a field out of its range");
	}
	let offset: i64 = match zone {
		ZONE_UNSPECIFIED => 0,
		minutes if (-1440..=1440).contains(&minutes) => minutes as i64 * 60,
		_ => return Err("_GRT's time zone is past a day"),
	};
	let local = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
	u64::try_from(local + offset).map_err(|_| "_GRT's time is before the epoch")
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
	let y = if month <= 2 { year - 1 } else { year };
	let era = if y >= 0 { y } else { y - 399 } / 400;
	let yoe = y - era * 400;
	let mp = (month + 9) % 12;
	let doy = (153 * mp + 2) / 5 + day - 1;
	let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
	era * 146_097 + doe - 719_468
}

/// THE TIMER VALUE A SLEEP'S TIMED WAKE PROGRAMS: whole seconds, rounded up so the machine never wakes early; none for
/// no timed wake, and a wake past what the timer holds refused rather than wrapped.
pub fn timer_seconds(timed_wake_ms: u64) -> Result<Option<u64>, &'static str> {
	if timed_wake_ms == 0 {
		return Ok(None);
	}
	let seconds = timed_wake_ms.div_ceil(1000);
	if seconds >= DISABLED { Err("the timed wake is past what the device's timer holds") } else { Ok(Some(seconds)) }
}

#[cfg(test)]
mod tests;
