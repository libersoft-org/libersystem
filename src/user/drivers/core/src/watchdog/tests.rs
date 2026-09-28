use super::*;

#[test]
fn the_i6300esb_splits_the_timeout_into_two_preloads_of_its_unit_and_rounds_down() {
	// 60 s: two stages of 30 s, each 30,000,000,000 ns / 983,040 ns = 30517 units.
	let division = i6300esb(60_000).expect("sixty seconds counts");
	assert_eq!(division.count, 30_517);
	assert_eq!(division.effective_ms, 59_998, "rounded down, never up");
	assert!(division.effective_ms <= 60_000);
	assert_eq!(i6300esb(1), None, "below two units nothing counts");
	let (min, max, granularity) = i6300esb_range();
	assert_eq!((min, granularity), (2, 2));
	assert!(i6300esb(min).is_some());
	// Past the 20-bit preload the division stops at the longest the device counts.
	let longest = i6300esb(u32::MAX).expect("a long timeout is cut to the longest");
	assert_eq!(longest.count, ESB_PRELOAD_MAX);
	assert_eq!(longest.effective_ms, max);
}

#[test]
fn the_tco_counts_pairs_of_ticks_from_two_to_1023() {
	assert_eq!(tco(60_000), Some(Division { effective_ms: 60_000, count: 50 }));
	assert_eq!(tco(61_199), Some(Division { effective_ms: 60_000, count: 50 }), "rounded down to 1.2 s");
	assert_eq!(tco(2_399), None, "below 2.4 s the device cannot count");
	assert_eq!(tco(2_400), Some(Division { effective_ms: 2_400, count: 2 }));
	assert_eq!(tco(u32::MAX), Some(Division { effective_ms: 1_227_600, count: 1_023 }));
	assert_eq!(tco_range(), (2_400, 1_227_600, 1_200));
}

#[test]
fn a_wdat_counts_its_table_s_period_within_its_table_s_range() {
	assert_eq!(wdat(60_000, 1_200, 2, 1_023), Some(Division { effective_ms: 60_000, count: 50 }));
	assert_eq!(wdat(1_000, 1_200, 2, 1_023), None, "below the table's minimum");
	assert_eq!(wdat(10_000_000, 1_000, 1, 600), Some(Division { effective_ms: 600_000, count: 600 }), "cut to the table's maximum");
	assert_eq!(wdat(60_000, 0, 1, 10), None, "a period of nothing is no table");
	assert_eq!(wdat(60_000, 1_000, 10, 5), None, "nor a range upside down");
}

#[test]
fn a_bmc_counts_tenths_of_a_second_in_sixteen_bits() {
	assert_eq!(bmc(60_000), Some(Division { effective_ms: 60_000, count: 600 }));
	assert_eq!(bmc(99), None);
	assert_eq!(bmc(u32::MAX), Some(Division { effective_ms: 6_553_500, count: 65_535 }));
}
