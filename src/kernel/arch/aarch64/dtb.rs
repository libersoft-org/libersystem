// aarch64 device-tree location.
//
// The FDT PARSING is the shared `arch::common::dtb::Fdt` (the format is a standard, the
// same on every device-tree-booted arch). This shim only supplies the two aarch64
// specifics: where to find the blob on QEMU's `virt` machine (the `-kernel` path passes
// x0 = 0, so `boot/qemu-run.sh aarch64` loads the dumped DTB at a fixed address,
// and a low-DRAM scan is the fallback) and how to read physical memory (the higher-half
// direct map, `paging::phys_to_virt`).

pub use crate::arch::common::dtb::BootInfo;
use crate::arch::common::dtb::Fdt;

// Fixed address the runner loads the dumped DTB at.
const QEMU_DTB_ADDR: u64 = 0x4A00_0000;

// The low DRAM window scanned for the FDT header when no pointer is supplied.
const SCAN_START: u64 = 0x4000_0000;
const SCAN_END: u64 = 0x4800_0000;

// An FDT view at `base`, reading physical memory through the aarch64 direct map.
fn at(base: u64) -> Fdt {
	Fdt::new(base, super::paging::phys_to_virt)
}

// Find the FDT: use `hint` if it points at a valid header, else the runner's fixed
// load address, else a scan of low DRAM.
fn locate(hint: u64) -> Option<u64> {
	if hint != 0 && at(hint).is_valid() {
		return Some(hint);
	}
	if at(QEMU_DTB_ADDR).is_valid() {
		return Some(QEMU_DTB_ADDR);
	}
	let mut base = SCAN_START;
	while base < SCAN_END {
		if at(base).is_valid() {
			return Some(base);
		}
		base += 0x1000;
	}
	None
}

// Parse the device tree reachable from `hint`, returning the RAM geometry, CPU count
// and PCIe ECAM base, or None if no valid FDT is found.
pub fn parse(hint: u64) -> Option<BootInfo> {
	at(locate(hint)?).parse()
}
