// riscv64 device-tree location.
//
// The FDT PARSING is the shared `fdt::Fdt` crate (the format is a standard, the same
// on every device-tree-booted arch, and the loader reads the same trees this does).
// This shim only supplies the two
// riscv64 specifics: where to find the blob (OpenSBI passes the DTB pointer in a1, so
// the hint is normally valid; a low-DRAM scan is the fallback) and how to read
// physical memory (the higher-half direct map, `paging::phys_to_virt`).

pub use fdt::BootInfo;
use fdt::Fdt;

// The low DRAM window scanned for the FDT header when no pointer is supplied.
const SCAN_START: u64 = 0x8000_0000;
const SCAN_END: u64 = 0x9000_0000;

// An FDT view at `base`, reading physical memory through the riscv64 direct map.
//
// # Safety
// `base` must be inside the direct map and must be where a device tree was published - by firmware,
// by the loader, or by the fixed address this machine's runner uses. `Fdt::new` dereferences it
// (FDT-007), and so does everything reached from the value this returns.
unsafe fn at(base: u64) -> Fdt {
	unsafe { Fdt::new(base, super::paging::phys_to_virt) }
}

// Find the FDT: use `hint` (the DTB pointer OpenSBI passed in a1) if valid, else scan.
//
// # Safety
// `hint` must be 0 or an address the boot path published. The scan window is a compile-time
// constant inside the direct map, so probing it is this module's own business; the caller's number
// is not.
unsafe fn locate(hint: u64) -> Option<u64> {
	if hint != 0 && unsafe { at(hint) }.is_valid() {
		return Some(hint);
	}
	let mut base = SCAN_START;
	while base < SCAN_END {
		if unsafe { at(base) }.is_valid() {
			return Some(base);
		}
		base += 0x1000;
	}
	None
}

// Parse the device tree reachable from `hint`, returning the RAM geometry, CPU count
// and PCIe ECAM base, or None if no valid FDT is found.
//
// # Safety
// `hint` must be 0 or the device-tree pointer the boot path was given.
pub unsafe fn parse(hint: u64) -> Option<BootInfo> {
	unsafe { at(locate(hint)?).parse() }
}

// The tree itself, at wherever it was actually found.
//
// `parse` locates the blob and throws the location away, so a caller that needs the FDT for anything
// else - carving its reservation block and its own pages out of the usable memory, which nothing
// did - had only the raw hint, which may not be where the tree is.
//
// # Safety
// `hint` must be 0 or the device-tree pointer the boot path was given, and the returned `Fdt`
// carries that contract onward - every method on it dereferences the address.
pub unsafe fn located(hint: u64) -> Option<Fdt> {
	unsafe { Some(at(locate(hint)?)) }
}

// `/cpus/timebase-frequency` in Hz, or None when this tree does not carry it.
//
// # Safety
// `hint` must be 0 or the device-tree pointer the boot path was given.
pub unsafe fn timebase_frequency(hint: u64) -> Option<u32> {
	unsafe { at(locate(hint)?).timebase_frequency() }
}

// Does this machine's device tree advertise `want` (a lowercase ISA extension name) on its CPUs?
// False when no FDT can be found, which is the safe direction: an undetected extension is one
// this port does not use.
//
// # Safety
// `hint` must be 0 or the device-tree pointer the boot path was given.
pub unsafe fn has_isa_extension(hint: u64, want: &[u8]) -> bool {
	unsafe {
		match locate(hint) {
			Some(base) => at(base).has_isa_extension(want),
			None => false,
		}
	}
}
