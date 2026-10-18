// THE MEMORY BALLOON'S DECISIONS: how far one step moves, what the host is told and in what form, and what the
// statistics queue answers - the parts of `driver.virtio-balloon` a host can test.
//
// THE BALLOON HOLDS THE SMALLER OF WHAT THE HOST ASKS AND WHAT THE POLICY ALLOWS, and moves toward it a bounded batch at
// a time, giving back before it takes when the two disagree with what it holds: a host that cancels, or a policy that
// lowers the allowance under pressure, is followed one batch later, never one inflation later.

use rt::MemoryStats;

// `VIRTIO_BALLOON_F_STATS_VQ`: the statistics queue.
pub const FEATURE_STATS_VQ: u32 = 1 << 1;

// THE MOST PAGES ONE STEP MOVES: the kernel's batch, and one page of 4-byte frame numbers four times over.
pub const BATCH: usize = 256;

// One step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Plan {
	Hold,
	Inflate(usize),
	Deflate(usize),
}

// THE NEXT STEP for a balloon holding `held` pages, asked for `target` and allowed `allowed`.
pub fn plan(target: u64, allowed: u64, held: u64) -> Plan {
	let want = target.min(allowed);
	if held < want {
		Plan::Inflate((want - held).min(BATCH as u64) as usize)
	} else if held > want {
		Plan::Deflate((held - want).min(BATCH as u64) as usize)
	} else {
		Plan::Hold
	}
}

// THE FRAMES AS THE DEVICE TAKES THEM: one little-endian 32-bit page frame number each, 4 KiB pages. None when a frame
// is not page-aligned or lies past what 32 bits of frame number reach (16 TiB) - such a frame cannot be named to the host
// at all, and naming another would give away the wrong page.
pub fn pfns(frames: &[u64], out: &mut [u8]) -> Option<usize> {
	if out.len() < frames.len() * 4 {
		return None;
	}
	for (at, frame) in frames.iter().enumerate() {
		if frame % 4096 != 0 {
			return None;
		}
		let pfn = u32::try_from(frame >> 12).ok()?;
		out[at * 4..at * 4 + 4].copy_from_slice(&pfn.to_le_bytes());
	}
	Some(frames.len() * 4)
}

// The statistics tags this driver answers with.
pub const STAT_MEMFREE: u16 = 4;
pub const STAT_MEMTOT: u16 = 5;
pub const STAT_AVAIL: u16 = 6;
// One record is a 16-bit tag and a 64-bit value, packed; three of them.
pub const STATS_LEN: usize = 3 * 10;

// THE STATISTICS RECORD, in bytes: free, total and available memory. Available is free here - nothing this kernel holds
// could be dropped to make room, so free is what a request could have now. Answers the length written.
pub fn stats(memory: &MemoryStats, out: &mut [u8; STATS_LEN]) -> usize {
	let page = 4096u64;
	let records = [
		(STAT_MEMFREE, memory.free_frames.saturating_mul(page)),
		(STAT_MEMTOT, memory.total_frames.saturating_mul(page)),
		(STAT_AVAIL, memory.free_frames.saturating_mul(page)),
	];
	for (at, (tag, value)) in records.iter().enumerate() {
		out[at * 10..at * 10 + 2].copy_from_slice(&tag.to_le_bytes());
		out[at * 10 + 2..at * 10 + 10].copy_from_slice(&value.to_le_bytes());
	}
	STATS_LEN
}

#[cfg(test)]
mod tests;
