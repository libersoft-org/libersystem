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

// The most modules a hand-off may declare, and therefore the most buffers that have to be carved out
// of free RAM. Exported so the callers can size their hole arrays from it instead of guessing.
//
// They guessed 16, and `push` below stops silently when the caller's slice is full - so with two
// slots spent on `BootInfo` and the module descriptor array, modules past the fourteenth were simply
// not reserved, and the frame allocator was free to hand out the page an ELF archive was sitting in.
// That is the flaky `BadImage` this milestone already fixed once, reachable again by staging more
// modules, and nothing would have said so.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub const MAX_MODULES: usize = 64;

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
// How many reservations a hole array has to hold, and it is DERIVED now.
//
// It was sixteen, and both ports wrote `16` out by hand rather than using it - so nothing connected
// the array's size to the number of reservations there can be. `loader_reservations` spends two
// slots on `BootInfo` and the module descriptor array and one per module, and it accepts up to
// `MAX_MODULES` of them: with sixteen slots, a hand-off declaring more than fourteen modules had the
// rest dropped in silence, and a module buffer that is not carved out of free RAM is a page the
// allocator may hand out with an ELF archive still in it. That is the flaky `BadImage` this
// milestone already fixed once, reachable again by staging more modules.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub const MAX_HOLES: usize = 2 + MAX_MODULES;

// Carve EVERY RAM bank against the loader's reservations, into one region array.
//
// The device-tree ports used to hand `carve` a single range - the contiguous run from the first bank
// - so a board with discontiguous RAM lost everything past its first hole. The reader carries the
// real bank list now, and this is the other half: the allocator has always taken a list of regions,
// and the boot path was the side that collapsed it.
//
// Not a loop around `carve`, because `carve` CLAMPS its holes to the range it is given, in place. A
// second call would see reservations already trimmed to the first bank and carve nothing out of the
// second. Each bank gets its own copy, which also makes each bank's answer independent of the order
// the tree wrote them in.
//
// `floor` is the first address above the kernel image: a bank below it is memory the kernel is
// standing in, and the part above it is what is free.
//
// `out` needs `banks + holes` entries at most - each reservation splits exactly one bank into at
// most one extra region - and a bank that does not fit is dropped rather than half-written, because
// a region array that looks complete and is missing its tail is the failure this file exists to
// avoid.
// AND A CEILING, because a bank outside the direct map is not memory this kernel can touch.
//
// The allocator used to be seeded from every bank the device tree reported, while aarch64's boot
// stub maps 1 GB blocks over a fixed 0..4 GiB and riscv64's maps a fixed 0..8 GiB. `run.sh --arch
// aarch64 --mem 4G` puts QEMU `virt` RAM at 1 GiB running to ~5 GiB, so the top gigabyte is outside
// the direct map and the first `write_bytes(phys_to_virt(frame), ..)` on a frame from it is a
// translation fault - not attributable to whoever allocated it, and not reproducible without that
// much memory.
//
// This is the same defect as the `DIRECT_MAP_LIMIT` work seen from the ALLOCATOR's side
// rather than the firmware-pointer side, and the rule is the one that milestone states: nothing may
// enter the allocator before `phys_to_virt` on it is guaranteed to translate.
//
// The remainder is REPORTED rather than dropped in silence - losing memory is recoverable by
// somebody who reads the boot log, and it is the only way anyone learns the map wants widening.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn carve_banks(banks: &[(u64, u64)], floor: u64, ceiling: u64, holes: &[Hole], out: &mut [MemRegion]) -> usize {
	let mut written = 0usize;
	for &(base, len) in banks {
		let start = base.max(floor);
		// `checked_add`, not `saturating_add`: a bank declaring `size = u64::MAX` saturated into a
		// usable range covering nearly the whole address space, and the frame allocator's overlap
		// refusal then kept the system safe by discarding legitimate memory. The device-tree reader
		// drops such a bank now; this is the second answer, at the consumer.
		let Some(declared_end) = base.checked_add(len) else {
			crate::serial_println!("bootmem: bank {base:#x} declares a length that leaves the address space - dropped");
			continue;
		};
		let end = declared_end.min(ceiling);
		if declared_end > ceiling {
			crate::serial_println!("bootmem: {} MiB of RAM above {ceiling:#x} is outside the direct map and is NOT usable - widen the boot mapping to reach it", (declared_end - ceiling.max(start)) / (1024 * 1024));
		}
		if end <= start || written >= out.len() {
			continue;
		}
		let mut scratch = [Hole { start: 0, end: 0 }; MAX_HOLES];
		let count = holes.len().min(scratch.len());
		scratch[..count].copy_from_slice(&holes[..count]);
		written += carve(start, end - start, &mut scratch[..count], &mut out[written..]);
	}
	written
}

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
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
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
	// SAYS SO WHEN IT CANNOT FIT ONE. A reservation that does not make it into the list is a region
	// the allocator will hand out, and the old version returned a short list that reads exactly like
	// a complete one. Callers size their arrays from `MAX_HOLES`, so this should never fire - which
	// is precisely why it must be audible if it ever does.
	let push = |start: u64, length: u64, out: &mut [Hole], written: &mut usize| {
		if length == 0 {
			return;
		}
		if *written >= out.len() {
			crate::serial_println!("bootmem: NO ROOM to reserve {start:#x}..{:#x} - the hole list holds {} and the allocator may hand this range out", start + length, out.len());
			return;
		}
		out[*written] = Hole { start, end: start + length };
		*written += 1;
	};
	push(arg, core::mem::size_of::<bootproto::BootInfo>() as u64, out, &mut written);
	if modules_phys == 0 || modules_len == 0 {
		return written;
	}
	push(modules_phys, modules_len * core::mem::size_of::<bootproto::Module>() as u64, out, &mut written);
	// A length the hand-off cannot plausibly have means the pointer is not what it claims, and
	// walking it would read arbitrary memory as descriptors. Refuse rather than guess.
	if modules_len > MAX_MODULES as u64 {
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

// THE DEVICE TREE'S OWN RESERVATIONS, and the blob itself.
//
// `loader_reservations` carves out `BootInfo`, the module descriptor array and the modules, and
// nothing else. The Devicetree Specification requires a client not to use the memory reservation
// block's regions and not to overwrite the tree while it is still in use - and this kernel keeps
// reading the tree after the allocator is up: riscv64 reads `timebase-frequency` from it well after
// `frame` init. So the pages holding the device tree, firmware runtime data and whatever a board
// reserves could be allocated and zeroed while still live.
//
// Appended to whatever the loader reservations already wrote. Returns the new total AND whether
// every reservation the tree declares was carried (FDT-005).
//
// A blob this cannot read reserves nothing and says so; refusing the boot over an unreadable
// reservation list would take a machine down over a list that is usually empty. What the caller
// must NOT do is take the tree's account of RAM while ignoring its account of what is reserved -
// the two come off the same bytes, and half of them is worse than neither. The second half of the
// answer is what lets the caller fall back to the conservative region instead.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub unsafe fn devicetree_reservations(fdt: &fdt::Fdt, out: &mut [Hole], written: usize) -> (usize, bool) {
	let mut written = written;
	// Set by anything that stopped a reservation from being carried, wherever it happened.
	let mut complete = true;
	let mut push = |start: u64, length: u64, written: &mut usize, complete: &mut bool| {
		if length == 0 {
			return;
		}
		if *written >= out.len() {
			crate::serial_println!("bootmem: NO ROOM to reserve {start:#x}..{:#x} - the hole list holds {} and the allocator may hand this range out", start + length, out.len());
			*complete = false;
			return;
		}
		out[*written] = Hole { start, end: start + length };
		*written += 1;
	};
	// The blob's own pages first: it is the one this kernel is certain to keep reading.
	if let Some((base, len)) = fdt.extent() {
		push(base, len, &mut written, &mut complete);
	}
	let mut entries = 0usize;
	// All or nothing on the reader's side now: an unreadable list hands over no entries at all, so
	// this either carves the whole thing or carves none of it.
	if !fdt.for_each_reserved_region(|base, len| {
		entries += 1;
		push(base, len, &mut written, &mut complete);
	}) {
		crate::serial_println!("bootmem: the device tree's reservation block does not read as a list - nothing from it is reserved, and this tree's account of memory is not used");
		complete = false;
	}
	// AND THE OTHER PLACE THE TREE SAYS "DO NOT USE THIS" (FDT-001).
	//
	// The block above is the header's reservation list. `/reserved-memory` is the subtree boards
	// actually use - firmware runtime, a secure world's carve-out, DMA pools, a framebuffer an
	// earlier stage handed over - and nothing read it. Those ranges lie inside a `/memory` bank by
	// construction, which is what makes them worth declaring and is exactly why reading `/memory`
	// and stopping hands them to the allocator.
	//
	// UEFI boots are not exposed: they take the firmware's own map and never reach this. A direct
	// device-tree boot is, and that is the path this covers.
	let mut nodes = 0usize;
	let subtree = fdt.for_each_reserved_memory_node(|base, len| {
		nodes += 1;
		push(base, len, &mut written, &mut complete);
	});
	if !subtree {
		crate::serial_println!("bootmem: the device tree's /reserved-memory subtree could not be read to its end - {nodes} range(s) carved out, and anything past them is NOT reserved");
		complete = false;
	}
	(written, complete)
}

// THE FIRMWARE'S OWN MAP, when the loader handed one over.
//
// These two architectures used to receive `memmap: 0` and fall back to the device tree's `/memory`,
// which under UEFI is not the system memory map: the EFI map carries runtime services code and
// data, ACPI NVS and reclaimable regions, unusable memory, firmware reservations, loader
// allocations and MMIO apertures, none of which a `/memory` node expresses. The loader translates
// it now; this is the side that reads it.
//
// `None` when there is none - a QEMU `-kernel` boot, which has no firmware at all - and the caller
// falls back to the device tree, which is the right source when there is no better one.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub unsafe fn handed_memmap(arg: u64, to_virt: impl Fn(u64) -> u64) -> Option<(*const MemRegion, usize)> {
	if arg == 0 {
		return None;
	}
	let magic = unsafe { core::ptr::read_volatile(to_virt(arg) as *const u64) };
	if magic != bootproto::MAGIC {
		return None;
	}
	let info = to_virt(arg) as *const bootproto::BootInfo;
	let (phys, len) = unsafe { (core::ptr::read_volatile(&raw const (*info).memmap), core::ptr::read_volatile(&raw const (*info).memmap_len)) };
	if phys == 0 || len == 0 {
		return None;
	}
	// The count comes off the same untrusted structure as everything else, and the array it
	// describes was sized by `MAX_REGIONS` in the loader. A larger claim is a BootInfo this kernel
	// will not read rather than one to clamp: clamping would seed the allocator from a map that
	// looks complete and is not.
	// The loader's own ceiling, restated here rather than imported: the `uefi` crate is a
	// loader-side dependency and the kernel does not link it. A number that must match one in
	// another crate is worth a sentence, and this is it.
	const LOADER_MAX_REGIONS: u64 = 512;
	if len > LOADER_MAX_REGIONS {
		crate::serial_println!("bootmem: the loader claims {len} memory regions, past the {LOADER_MAX_REGIONS} the protocol carries - falling back to the device tree");
		return None;
	}
	Some((to_virt(phys) as *const MemRegion, len as usize))
}

// THE BOOT ARCHIVE A DEVICE-TREE MACHINE HANDED OVER, as a physical range.
//
// aarch64 and riscv64 have no bootloader module hand-off on their direct `-kernel` path, so the
// runner passes the init package one of the only two ways the platform offers, and the two machines
// do not offer the same one:
//
//   - riscv64 gets its device tree from the invocation that boots it (OpenSBI passes it in a1), so
//     `-initrd` annotates the tree the kernel reads and `/chosen/linux,initrd-start` / `-end` carry
//     the exact range. That is `fdt_start`/`fdt_end` here, and it is preferred whenever present -
//     it is also what real firmware writes.
//   - aarch64 enters an ELF kernel with x0 = 0 and places no tree for it, so the runner dumps one
//     from a SEPARATE QEMU invocation and loads it at a fixed address. That tree cannot carry an
//     initrd range (the dump crashes qemu-system-aarch64 10.0.11 when `-kernel` is present at all),
//     so the archive is loaded at a fixed address too and `probe` is where.
//
// Only the START has to be agreed on: a PKGARCH1 archive states its own extent, so the length is
// read out of it rather than out of a second constant that two components would have to keep equal.
pub unsafe fn boot_archive_range(fdt_start: u64, fdt_end: u64, probe: u64, to_virt: impl Fn(u64) -> u64) -> Option<(u64, u64)> {
	if fdt_end > fdt_start {
		return Some((fdt_start, fdt_end - fdt_start));
	}
	if probe == 0 {
		return None;
	}
	let len = unsafe { archive_len(to_virt(probe) as *const u8) }?;
	Some((probe, len))
}

// Every scalar in the header and the entry table is read one byte at a time and volatile, because
// this runs against an address nothing has promised holds an archive: on a boot where the runner
// loaded nothing there it is ordinary RAM, and the answer has to be `None` rather than a fault or a
// wild length. The magic and the reserved word are twelve bytes of exact match before anything
// derived from the contents is used.
pub unsafe fn archive_len(at: *const u8) -> Option<u64> {
	unsafe fn byte(at: *const u8, off: usize) -> u8 {
		unsafe { core::ptr::read_volatile(at.add(off)) }
	}
	unsafe fn le32(at: *const u8, off: usize) -> u32 {
		let mut bytes = [0u8; 4];
		for (i, b) in bytes.iter_mut().enumerate() {
			*b = unsafe { byte(at, off + i) };
		}
		u32::from_le_bytes(bytes)
	}
	for (i, want) in abi::PKG_MAGIC.iter().enumerate() {
		if unsafe { byte(at, i) } != *want {
			return None;
		}
	}
	if unsafe { le32(at, 12) } != 0 {
		return None;
	}
	let count = unsafe { le32(at, 8) } as usize;
	// The READER's ceiling, the same one `abi::Package::parse` applies - an archive declaring more
	// entries than that is not one this system produces, and walking its table would be a long read
	// through memory that has not been shown to be an archive at all.
	if count == 0 || count > abi::MAX_PACKAGE_ENTRIES {
		return None;
	}
	let table_end = abi::PKG_HEADER_LEN.checked_add(count.checked_mul(abi::PKG_ENTRY_LEN)?)? as u64;
	let mut end = table_end;
	for i in 0..count {
		let entry = abi::PKG_HEADER_LEN + i * abi::PKG_ENTRY_LEN;
		let offset = unsafe { le32(at, entry + abi::PKG_NAME_LEN) } as u64;
		let size = unsafe { le32(at, entry + abi::PKG_NAME_LEN + 4) } as u64;
		// A blob inside the header or the table is a malformed archive, and taking its extent
		// anyway would understate the range the frame pool has to be carved around.
		if offset < table_end {
			return None;
		}
		end = end.max(offset.checked_add(size)?);
	}
	Some(end)
}
