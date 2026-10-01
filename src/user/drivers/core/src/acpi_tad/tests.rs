use super::*;

fn grt(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8, valid: u8, zone: i16) -> [u8; 16] {
	let mut b = [0u8; 16];
	b[..2].copy_from_slice(&year.to_le_bytes());
	b[2..8].copy_from_slice(&[month, day, hour, minute, second, valid]);
	b[10..12].copy_from_slice(&zone.to_le_bytes());
	b
}

#[test]
fn the_capabilities_are_gcp_s_three_bits() {
	assert_eq!(capabilities(0b101), Capabilities { ac_wake: true, dc_wake: false, real_time: true });
	assert_eq!(capabilities(0), Capabilities::default());
}

// A KNOWN INSTANT: 2026-09-30 12:34:56 UTC is 1790771696.
#[test]
fn the_time_is_read_at_its_offsets_and_the_zone_is_added_back() {
	assert_eq!(unix_of_grt(&grt(2026, 9, 30, 12, 34, 56, 1, 2047)), Ok(1_790_771_696), "an unspecified zone is UTC");
	assert_eq!(unix_of_grt(&grt(2026, 9, 30, 14, 34, 56, 1, -120)), Ok(1_790_771_696), "UTC is local plus the zone");
	assert_eq!(unix_of_grt(&grt(1970, 1, 1, 0, 0, 0, 1, 0)), Ok(0));
	assert_eq!(unix_of_grt(&grt(2000, 3, 1, 0, 0, 0, 1, 0)), Ok(951_868_800), "the day after a leap day in a leap century");
}

#[test]
fn a_time_that_is_not_one_is_refused_by_name() {
	assert!(unix_of_grt(&grt(2026, 9, 30, 12, 0, 0, 0, 0)).is_err(), "marked not valid");
	assert!(unix_of_grt(&grt(2026, 13, 1, 0, 0, 0, 1, 0)).is_err(), "a thirteenth month");
	assert!(unix_of_grt(&grt(2026, 1, 1, 24, 0, 0, 1, 0)).is_err(), "a twenty-fifth hour");
	assert!(unix_of_grt(&grt(2026, 1, 1, 0, 0, 0, 1, 1500)).is_err(), "a zone past a day");
	assert!(unix_of_grt(&[0u8; 15]).is_err(), "a short buffer");
}

#[test]
fn a_timed_wake_is_whole_seconds_rounded_up_and_none_programs_nothing() {
	assert_eq!(timer_seconds(0), Ok(None));
	assert_eq!(timer_seconds(1), Ok(Some(1)), "never early");
	assert_eq!(timer_seconds(8000), Ok(Some(8)));
	assert_eq!(timer_seconds(8001), Ok(Some(9)));
	assert!(timer_seconds(u64::MAX).is_err(), "past what the timer holds is refused, not wrapped");
}
