// HOTPLUGGED MEMORY: ranges a virtual machine's host plugs in at run time (virtio-mem), held apart from the pools the
// boot seeded - handed out after them, only for programs' memory objects, and given back before them.
//
// WHY APART. The boot pools are sized once for one extent, with an ownership record over it and a NUMA routing that
// assumes it; a range that appears later fits none of that. So a plugged range is a region of its own here: a bitmap of
// its free frames, consulted only when the boot pools have nothing - which also keeps its frames free for longest, and a
// frame nobody holds is what makes a block unpluggable.
//
// AND ONLY FOR A PROGRAM'S MEMORY (`frame::allocate_movable`): a memory object charged to a Domain, which goes when the
// program lets it go or ends. What the kernel keeps for itself - its heap, page tables, DMA buffers, stacks - never
// comes from here, since nothing would ever give it back and its block would stay plugged for good.
//
// WHAT IS NEVER TAKEN BACK. A block goes offline only when every frame of it is free: a frame a process, a buffer, a
// mapping or a device holds keeps its block plugged, whatever the host asks, until it is freed. Offlining takes the
// block's frames out of the bitmap first, then unmaps them from the direct map and waits for every core to forget the
// translation - only then may the host have the block.
//
// THE DIRECT MAP. A block is mapped into it as it comes online, and only inside the top-level slot that maps the
// machine's RAM: every address space shares the tables under that slot, so the mapping is everyone's at once. A range
// past it is refused rather than mapped into one table. x86_64 only - the device-tree ports' direct maps are fixed
// windows this file cannot extend.

use alloc::vec::Vec;

use crate::sync::SpinLock;

// The most regions, and the highest address one may reach: the first 512 GiB, which the direct map's first top-level
// slot covers.
pub const MAX_REGIONS: usize = 4;
pub const MAX_ADDRESS: u64 = 1 << 39;
const PAGE: u64 = 4096;

// Why a region, or a change to one, is refused. Nothing changes on a refusal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	// A block size that is not a power of two of at least a page, or a base or size that is not whole blocks.
	Unaligned,
	// Over memory the machine already has, a device's window, or another region.
	Overlap,
	// Past what the direct map's shared slot reaches, or wrapping.
	TooHigh,
	TooMany,
	// A block range outside the region.
	Range,
	// A block already online, for an online; one not online, for an offline.
	State,
	// A frame of the range is in use: the block stays.
	Busy,
	// The direct map could not take the range - x86_64's, the one port whose direct map this extends.
	#[cfg(target_arch = "x86_64")]
	Mapping,
	NoMemory,
	// No direct map this kernel can extend on this architecture.
	Unsupported,
}

// A RANGE A REGION MAY BE: whole blocks of a power-of-two size of at least a page, inside the direct map's shared slot,
// and over nothing in `taken` - the memory map, every device window and every other region. Pure, for its test.
pub fn check(base: u64, size: u64, block: u64, taken: &[(u64, u64)]) -> Result<(), Refusal> {
	if block < PAGE || !block.is_power_of_two() || size == 0 || base % block != 0 || size % block != 0 {
		return Err(Refusal::Unaligned);
	}
	let end = base.checked_add(size).ok_or(Refusal::TooHigh)?;
	if end > MAX_ADDRESS {
		return Err(Refusal::TooHigh);
	}
	if taken.iter().any(|&(start, len)| base < start.saturating_add(len) && start < end) {
		return Err(Refusal::Overlap);
	}
	Ok(())
}

struct Region {
	base: u64,
	pages: u64,
	block_pages: u64,
	// Per block: online.
	online: Vec<bool>,
	// Per frame: free, one bit each.
	free: Vec<u64>,
	free_pages: u64,
	online_pages: u64,
	// Where the next search for a free frame starts.
	cursor: u64,
}

impl Region {
	fn contains(&self, phys: u64) -> bool {
		phys >= self.base && phys < self.base + self.pages * PAGE
	}

	fn bit(&self, frame: u64) -> bool {
		self.free[(frame / 64) as usize] & (1 << (frame % 64)) != 0
	}

	fn set(&mut self, frame: u64, free: bool) {
		let word = &mut self.free[(frame / 64) as usize];
		if free {
			*word |= 1 << (frame % 64);
		} else {
			*word &= !(1 << (frame % 64));
		}
	}

	fn block_of(&self, frame: u64) -> usize {
		(frame / self.block_pages) as usize
	}
}

// A FIXED TABLE, so nothing under its lock ever allocates or frees: the heap may need a frame, and the frame allocator
// asks this file.
static REGIONS: SpinLock<[Option<Region>; MAX_REGIONS]> = SpinLock::new([const { None }; MAX_REGIONS]);

// THE REGION FOR [base, base + size) IN BLOCKS OF `block`: its index - the one already registered when the same range is
// asked for again, which is how a restarted driver finds what its predecessor had online. `taken` is what the range may
// not lie over.
pub fn register(base: u64, size: u64, block: u64, taken: &[(u64, u64)]) -> Result<usize, Refusal> {
	if !cfg!(target_arch = "x86_64") {
		return Err(Refusal::Unsupported);
	}
	let mut held = [(0u64, 0u64); MAX_REGIONS];
	{
		let regions = REGIONS.lock();
		for (at, slot) in regions.iter().enumerate() {
			if let Some(region) = slot {
				if region.base == base && region.pages * PAGE == size && region.block_pages * PAGE == block {
					return Ok(at);
				}
				held[at] = (region.base, region.pages * PAGE);
			}
		}
	}
	check(base, size, block, taken)?;
	check(base, size, block, &held)?;
	let pages = size / PAGE;
	let blocks = (size / block) as usize;
	// THE BITMAPS FIRST, with no lock held: the heap may need a frame, and the frame allocator asks this file.
	let mut online = Vec::new();
	online.try_reserve_exact(blocks).map_err(|_| Refusal::NoMemory)?;
	online.resize(blocks, false);
	let mut free = Vec::new();
	free.try_reserve_exact(pages.div_ceil(64) as usize).map_err(|_| Refusal::NoMemory)?;
	free.resize(pages.div_ceil(64) as usize, 0u64);
	let region = Region { base, pages, block_pages: block / PAGE, online, free, free_pages: 0, online_pages: 0, cursor: 0 };
	let mut regions = REGIONS.lock();
	// ANOTHER CALL MAY HAVE PLACED AN OVERLAPPING ONE while the bitmaps were made: checked again under the lock.
	if regions.iter().flatten().any(|other| base < other.base + other.pages * PAGE && other.base < base + size) {
		drop(regions);
		return Err(Refusal::Overlap);
	}
	let Some(at) = regions.iter().position(Option::is_none) else {
		drop(regions);
		return Err(Refusal::TooMany);
	};
	regions[at] = Some(region);
	Ok(at)
}

// Which blocks of the region are online, one bool a block into `out`: the region's block count.
pub fn state(index: usize, out: &mut [bool]) -> Option<usize> {
	let regions = REGIONS.lock();
	let region = regions.get(index)?.as_ref()?;
	for (slot, online) in out.iter_mut().zip(region.online.iter()) {
		*slot = *online;
	}
	Some(region.online.len())
}

// The region's [first, first + count) as (physical base, pages), or the refusal.
fn span(index: usize, first: usize, count: usize) -> Result<(u64, u64), Refusal> {
	let regions = REGIONS.lock();
	let region = regions.get(index).and_then(Option::as_ref).ok_or(Refusal::Range)?;
	if count == 0 || first.checked_add(count).is_none_or(|end| end > region.online.len()) {
		return Err(Refusal::Range);
	}
	Ok((region.base + first as u64 * region.block_pages * PAGE, count as u64 * region.block_pages))
}

#[cfg(target_arch = "x86_64")]
fn map(base: u64, pages: u64) -> Result<(), Refusal> {
	let hhdm = super::hhdm_offset();
	for at in 0..pages {
		let phys = base + at * PAGE;
		if crate::arch::paging::try_map_page(hhdm + phys, phys, crate::arch::paging::WRITABLE | crate::arch::paging::NO_EXECUTE).is_err() {
			unmap(base, at);
			return Err(Refusal::Mapping);
		}
	}
	Ok(())
}

#[cfg(target_arch = "x86_64")]
fn unmap(base: u64, pages: u64) {
	let hhdm = super::hhdm_offset();
	for at in 0..pages {
		let _ = crate::arch::paging::unmap_page(hhdm + base + at * PAGE);
	}
	let _ = super::tlb::shootdown();
}

#[cfg(not(target_arch = "x86_64"))]
fn map(_base: u64, _pages: u64) -> Result<(), Refusal> {
	Err(Refusal::Unsupported)
}

#[cfg(not(target_arch = "x86_64"))]
fn unmap(_base: u64, _pages: u64) {}

// BLOCKS [first, first + count) ONLINE: mapped into the direct map, then their frames free here. Each must be offline.
pub fn online(index: usize, first: usize, count: usize) -> Result<(), Refusal> {
	let (base, pages) = span(index, first, count)?;
	if REGIONS.lock()[index].as_ref().is_some_and(|region| region.online[first..first + count].iter().any(|online| *online)) {
		return Err(Refusal::State);
	}
	map(base, pages)?;
	let mut regions = REGIONS.lock();
	let Some(region) = regions[index].as_mut() else { return Err(Refusal::Range) };
	let start = (base - region.base) / PAGE;
	for frame in start..start + pages {
		region.set(frame, true);
	}
	for block in &mut region.online[first..first + count] {
		*block = true;
	}
	region.free_pages += pages;
	region.online_pages += pages;
	Ok(())
}

// BLOCKS [first, first + count) OFFLINE, if every frame of them is free: taken out of the bitmap, unmapped, and every core
// made to forget them before this returns. `Busy`, and nothing changed, when one frame is held.
pub fn offline(index: usize, first: usize, count: usize) -> Result<(), Refusal> {
	let (base, pages) = span(index, first, count)?;
	{
		let mut regions = REGIONS.lock();
		let Some(region) = regions[index].as_mut() else { return Err(Refusal::Range) };
		if region.online[first..first + count].iter().any(|online| !*online) {
			return Err(Refusal::State);
		}
		let start = (base - region.base) / PAGE;
		if (start..start + pages).any(|frame| !region.bit(frame)) {
			return Err(Refusal::Busy);
		}
		for frame in start..start + pages {
			region.set(frame, false);
		}
		for block in &mut region.online[first..first + count] {
			*block = false;
		}
		region.free_pages -= pages;
		region.online_pages -= pages;
	}
	unmap(base, pages);
	Ok(())
}

// ONE FREE FRAME of an online block, or None.
pub fn take() -> Option<u64> {
	let mut regions = REGIONS.lock();
	for region in regions.iter_mut().flatten() {
		if region.free_pages == 0 {
			continue;
		}
		for step in 0..region.pages {
			let frame = (region.cursor + step) % region.pages;
			if region.bit(frame) {
				region.set(frame, false);
				region.free_pages -= 1;
				region.cursor = frame + 1;
				return Some(region.base + frame * PAGE);
			}
		}
	}
	None
}

// A FRAME OF A REGION GIVEN BACK: true when it is a region's - then it is this file's, freed or refused here, and never
// the boot pools'. A frame of an offline block, or one already free, is a caller's bug: refused and said.
pub fn give(phys: u64) -> bool {
	let mut regions = REGIONS.lock();
	let Some(region) = regions.iter_mut().flatten().find(|region| region.contains(phys)) else { return false };
	let frame = (phys - region.base) / PAGE;
	if !region.online[region.block_of(frame)] || region.bit(frame) {
		drop(regions);
		crate::serial_println!("hotplug: WARNING: a free of {phys:#x} refused - its block is offline or the frame is free already");
		return true;
	}
	region.set(frame, true);
	region.free_pages += 1;
	true
}

// Whether `phys` is a region's, and then whether it is free.
pub fn is_free(phys: u64) -> Option<bool> {
	let regions = REGIONS.lock();
	let region = regions.iter().flatten().find(|region| region.contains(phys))?;
	Some(region.bit((phys - region.base) / PAGE))
}

// Every region's (online, free) frames.
pub fn totals() -> (usize, usize) {
	let regions = REGIONS.lock();
	regions.iter().flatten().fold((0, 0), |(online, free), region| (online + region.online_pages as usize, free + region.free_pages as usize))
}

#[cfg(test)]
mod tests;
