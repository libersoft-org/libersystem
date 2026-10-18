use super::*;

#[test]
// THE SMALLER OF THE TWO, A BATCH AT A TIME, AND BACK BEFORE MORE: the host's target never wins over the allowance, and
// a cancelled or lowered request is followed at the next step.
fn a_step_moves_toward_the_smaller_of_target_and_allowance_by_at_most_a_batch() {
	assert_eq!(plan(0, 0, 0), Plan::Hold, "nothing asked, nothing held");
	assert_eq!(plan(65536, 0, 0), Plan::Hold, "a host that asks with no allowance granted is given nothing");
	assert_eq!(plan(65536, 100_000, 0), Plan::Inflate(BATCH), "a batch at a time");
	assert_eq!(plan(65536, 100_000, 65_500), Plan::Inflate(36), "the last of it");
	assert_eq!(plan(65536, 1_000, 900), Plan::Inflate(100), "up to the allowance and no further");
	assert_eq!(plan(65536, 1_000, 1_000), Plan::Hold);
	assert_eq!(plan(65536, 500, 1_000), Plan::Deflate(BATCH), "a lowered allowance gives back first");
	assert_eq!(plan(0, 100_000, 300), Plan::Deflate(BATCH), "a cancelled target gives back");
	assert_eq!(plan(0, 100_000, 44), Plan::Deflate(44));
	assert_eq!(plan(u64::MAX, u64::MAX, 0), Plan::Inflate(BATCH), "no overflow at the ends");
}

#[test]
fn frames_cross_to_the_device_as_little_endian_page_numbers_or_not_at_all() {
	let mut out = [0u8; 12];
	assert_eq!(pfns(&[0x1000, 0x7fff_f000, 0xfff_ffff_f000], &mut out), Some(12));
	assert_eq!(&out[0..4], &1u32.to_le_bytes());
	assert_eq!(&out[4..8], &0x7ffffu32.to_le_bytes());
	assert_eq!(&out[8..12], &0xffff_ffffu32.to_le_bytes(), "the last page 32 bits of frame number reach");
	assert_eq!(pfns(&[0x1000_0000_0000], &mut out), None, "a frame at 16 TiB cannot be named");
	assert_eq!(pfns(&[0x1800], &mut out), None, "nor one inside a page");
	assert_eq!(pfns(&[0x1000, 0x2000, 0x3000, 0x4000], &mut out), None, "nor more than the buffer holds");
}

#[test]
fn the_statistics_are_free_total_and_available_memory_in_bytes() {
	let memory = MemoryStats { total_frames: 1 << 20, free_frames: 1 << 18, heap_total: 0, heap_free: 0, balloon_frames: 7 };
	let mut out = [0u8; STATS_LEN];
	assert_eq!(stats(&memory, &mut out), 30);
	let record = |at: usize| (u16::from_le_bytes([out[at * 10], out[at * 10 + 1]]), u64::from_le_bytes(out[at * 10 + 2..at * 10 + 10].try_into().unwrap()));
	assert_eq!(record(0), (STAT_MEMFREE, 1 << 30));
	assert_eq!(record(1), (STAT_MEMTOT, 1 << 32));
	assert_eq!(record(2), (STAT_AVAIL, 1 << 30));
}
