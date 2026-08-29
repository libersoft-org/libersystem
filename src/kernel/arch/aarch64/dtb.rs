// aarch64 device-tree location.
//
// The FDT PARSING is the shared `fdt::Fdt` crate (the format is a standard, the
// same on every device-tree-booted arch, and the loader reads the same trees this does).
// This shim only supplies the two aarch64
// specifics: where to find the blob on QEMU's `virt` machine (the `-kernel` path passes
// x0 = 0, so `harness/qemu-run.sh aarch64` loads the dumped DTB at a fixed address,
// and a low-DRAM scan is the fallback) and how to read physical memory (the higher-half
// direct map, `paging::phys_to_virt`).

pub use fdt::BootInfo;
use fdt::Fdt;

// Fixed address the runner loads the dumped DTB at.
const QEMU_DTB_ADDR: u64 = 0x4A00_0000;

// The low DRAM window scanned for the FDT header when no pointer is supplied.
const SCAN_START: u64 = 0x4000_0000;
const SCAN_END: u64 = 0x4800_0000;

// An FDT view at `base`, reading physical memory through the aarch64 direct map.
//
// # Safety
// `base` must be inside the direct map and must be where a device tree was published - by firmware,
// by the loader, or by the fixed address this machine's runner uses. `Fdt::new` dereferences it
// (FDT-007), and so does everything reached from the value this returns.
unsafe fn at(base: u64) -> Fdt {
	unsafe { Fdt::new(base, super::paging::phys_to_virt) }
}

// Find the FDT: use `hint` if it points at a valid header, else the runner's fixed
// load address, else a scan of low DRAM.
//
// # Safety
// `hint` must be 0 or an address the boot path published. The fixed address and the scan window are
// compile-time constants inside the direct map, so probing them is this module's own business; the
// caller's number is not.
unsafe fn locate(hint: u64) -> Option<u64> {
	if hint != 0 && unsafe { at(hint) }.is_valid() {
		return Some(hint);
	}
	// A POINTER THAT WAS GIVEN AND IS NOT AN FDT IS AN ERROR, not an absence.
	//
	// Firmware handing over a blob that does not carry an FDT header is firmware this kernel cannot
	// believe about anything, and the fallbacks below are for a boot path that published NO pointer
	// at all - the fixed address this tree's runner loads a dumped tree at, and a scan of low DRAM.
	// Going looking for a tree somewhere else, after being told where one is and finding it is not
	// there, is how a corrupt pointer ended up selecting a static QEMU descriptor: `parse` answered
	// `None`, which is the same value a machine with no tree produces, and the caller could not tell
	// the two apart.
	if hint != 0 {
		crate::serial_println!("dtb: the boot path published a device tree at {hint:#x} and there is no FDT header there - this kernel will not go looking for another one");
		return None;
	}
	if unsafe { at(QEMU_DTB_ADDR) }.is_valid() {
		return Some(QEMU_DTB_ADDR);
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

// WHY THERE IS NO TREE, WHICH IS TWO DIFFERENT FACTS.
//
// `parse` answers `None` for a machine that published no tree AND for one that published a tree this
// reader could not use - and the caller then treats `None` as the no-DT case, so a build that
// authorises the named no-DT profile authorises the CORRUPT-tree case with it. M4 asks for the static
// descriptor to be selected only by a named profile; a corrupt tree is not that profile.
pub enum TreeAbsence {
	// The boot path published no pointer and no tree was found where this port looks.
	NoTree,
	// Something was published or found and this reader could not use it. Never the no-DT profile.
	Unusable,
}

// WHERE THE TREE ACTUALLY IS, which is not always where the boot path said.
//
// This runner's DIRECT path enters with `x0 = 0` and loads the tree at a fixed address, so a caller
// that asks the boot argument gets zero and concludes there is no tree - which is how `psci::conduit`
// came to answer `PSCI_NONE` on a machine whose own tree states `method`, and why a four-core direct
// profile came up on one core. `locate` already knows the answer; nothing outside this file could ask.
//
// # Safety
// `hint` must be 0 or the device-tree pointer the boot path was given.
pub unsafe fn tree_address(hint: u64) -> Option<u64> {
	unsafe { locate(hint) }
}

// # Safety
// `hint` must be 0 or the device-tree pointer the boot path was given.
pub unsafe fn absence(hint: u64) -> TreeAbsence {
	// A pointer that was given and is not an FDT is an error, and `locate` says so; reaching here
	// with a hint at all means something was published.
	if hint != 0 {
		return TreeAbsence::Unusable;
	}
	match unsafe { locate(hint) } {
		// A tree was FOUND and `parse` still refused it: the blob is there and this reader cannot
		// use it, which is the case that must not select a static descriptor.
		Some(_) => TreeAbsence::Unusable,
		None => TreeAbsence::NoTree,
	}
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
