// Carving the frame pool around what the boot already put in RAM.
//
// x86_64 gets a memory map from its UEFI loader with every loader allocation marked reserved, and
// the frame allocator is seeded straight from the usable runs. aarch64 and riscv64 get no map at
// all - their loaders pass `memmap: 0` - so their kernels used to fabricate one: a single region
// from `__kernel_end` to the top of RAM, everything in it declared free.
//
// Everything in it was not free. The loader reads the boot packages off the volume and leaves them
// in RAM above the kernel, hands their addresses over in a `BootInfo`, and the kernel reads those
// bytes for the whole life of the boot - it is where every program's ELF image comes from. A pool
// that spans them will eventually hand one out.
//
// It took a long time to see because the old run-table allocator hands out the LOWEST free address
// first and never came back down, so within one test run it never climbed to where the packages
// were. The buddy allocator seeds by framing the pool into blocks of falling order - a big block at
// the bottom, progressively smaller ones toward the top - which puts the tail of RAM into the small
// orders, exactly where a single-page allocation looks first. The archive was zeroed by
// `alloc_frame` a few hundred allocations in, and the failure surfaced as `BadImage`: an ELF header
// of sixteen zero bytes, with the slice length still perfectly correct.
//
// So the fix is not in the allocator. A region list has to say what is actually free.

use bootproto::MemRegion;

// A physical range the frame pool must not contain.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Hole {
	pub start: u64,
	pub end: u64,
}

const PAGE: u64 = 4096;

// Cut `holes` out of `[base, base + len)` and write what survives into `out`, returning how many
// regions were written.
//
// Holes may arrive unsorted, overlapping, empty, or entirely outside the range; all of those are
// ordinary rather than exceptional, because the caller is reporting what a bootloader did and not
// describing a partition. Each is widened to whole pages first: a hole covering one byte of a page
// makes the page unusable, and rounding the other way would hand out the page holding it.
//
// `out` bounds the answer. If the holes would split the range into more pieces than `out` can hold,
// the last region written is TRUNCATED at the start of the next hole rather than extended over it -
// losing memory, never handing out a hole. Deliberately: with room for `n` regions the safe failure
// is a smaller pool.
pub fn carve(base: u64, len: u64, holes: &mut [Hole], out: &mut [MemRegion]) -> usize {
	let end = base + len;
	// Page-align outward, drop what cannot matter, and sort. Insertion sort because there are a
	// handful of holes and this runs once, before there is a heap to sort with.
	let mut count = 0usize;
	for index in 0..holes.len() {
		let hole = holes[index];
		if hole.end <= hole.start {
			continue;
		}
		let start = (hole.start & !(PAGE - 1)).max(base);
		let stop = hole.end.next_multiple_of(PAGE).min(end);
		if stop <= start {
			continue;
		}
		holes[count] = Hole { start, end: stop };
		count += 1;
	}
	let holes = &mut holes[..count];
	for i in 1..holes.len() {
		let mut j = i;
		while j > 0 && holes[j - 1].start > holes[j].start {
			holes.swap(j - 1, j);
			j -= 1;
		}
	}

	let mut written = 0usize;
	let mut at = base;
	let mut index = 0usize;
	while index < holes.len() {
		// Merge every hole that touches or overlaps this one, so the gap between two regions is
		// computed once rather than producing a zero-length region between adjacent holes.
		let mut stop = holes[index].end;
		let start = holes[index].start;
		while index + 1 < holes.len() && holes[index + 1].start <= stop {
			stop = stop.max(holes[index + 1].end);
			index += 1;
		}
		index += 1;
		if start > at {
			if written == out.len() {
				return written;
			}
			out[written] = MemRegion { base: at, length: start - at, kind: bootproto::MEM_USABLE, _pad: 0 };
			written += 1;
		}
		at = at.max(stop);
	}
	if at < end && written < out.len() {
		out[written] = MemRegion { base: at, length: end - at, kind: bootproto::MEM_USABLE, _pad: 0 };
		written += 1;
	}
	written
}

// Every range the loader's hand-off occupies, given the boot argument it was passed.
//
// Three kinds, and all three are read after the frame allocator is up, which is why all three have
// to be holes rather than just the big one:
//
//   - the `BootInfo` structure itself, at `arg`;
//   - the module descriptor array it points at, which `loader_module` walks whenever a later part
//     of the boot asks for a package by name;
//   - each module's bytes, which are the packages - megabytes of them, and the ones that actually
//     got overwritten.
//
// Returns how many were written. `arg` that is not a `BootInfo` yields none, which is the correct
// answer for a machine booted with `-kernel` and a raw device tree.
//
// # Safety
//
// `arg` is either zero or a physical address the boot handed over; `to_virt` must map a physical
// address to somewhere readable.
pub unsafe fn loader_reservations(arg: u64, to_virt: impl Fn(u64) -> u64, out: &mut [Hole]) -> usize {
	if arg == 0 || out.is_empty() {
		return 0;
	}
	let magic = unsafe { core::ptr::read_volatile(to_virt(arg) as *const u64) };
	if magic != bootproto::MAGIC {
		return 0;
	}
	let info = to_virt(arg) as *const bootproto::BootInfo;
	let (modules_phys, modules_len) = unsafe { (core::ptr::read_volatile(&raw const (*info).modules), core::ptr::read_volatile(&raw const (*info).modules_len)) };
	let mut written = 0usize;
	let push = |start: u64, length: u64, out: &mut [Hole], written: &mut usize| {
		if length > 0 && *written < out.len() {
			out[*written] = Hole { start, end: start + length };
			*written += 1;
		}
	};
	push(arg, core::mem::size_of::<bootproto::BootInfo>() as u64, out, &mut written);
	if modules_phys == 0 || modules_len == 0 {
		return written;
	}
	push(modules_phys, modules_len * core::mem::size_of::<bootproto::Module>() as u64, out, &mut written);
	// A length the hand-off cannot plausibly have means the pointer is not what it claims, and
	// walking it would read arbitrary memory as descriptors. Refuse rather than guess.
	if modules_len > 64 {
		return written;
	}
	let modules = unsafe { core::slice::from_raw_parts(to_virt(modules_phys) as *const bootproto::Module, modules_len as usize) };
	for module in modules {
		push(module.addr, module.size, out, &mut written);
	}
	written
}

#[cfg(test)]
mod tests;
