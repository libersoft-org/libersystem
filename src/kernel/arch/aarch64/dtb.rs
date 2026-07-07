// aarch64 device-tree (FDT/DTB) parsing (M116).
//
// QEMU `-machine virt` generates a flattened device tree describing the machine
// (RAM banks, CPUs, the GIC, peripherals). Its `-kernel` ELF path does not pass
// the blob's address to the kernel (x0 arrives as 0), so the runner loads the
// dumped DTB at a fixed address (`QEMU_DTB_ADDR`) and `locate` checks there (plus
// a low-DRAM scan) for the FDT header. This minimal parser extracts just what the
// bring-up needs - the total RAM size and the CPU count - so the frame allocator
// and the per-CPU pool stop hard-coding them.

use core::ptr::read_volatile;

// What the kernel wants out of the device tree.
#[derive(Clone, Copy)]
pub struct BootInfo {
	pub ram_base: u64,
	pub ram_size: u64,
	pub cpu_count: u32,
	// PCIe ECAM config-space base (0 if the tree has no pcie node).
	pub pcie_ecam: u64,
}

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

// Fixed address the runner (`boot/qemu-aarch64.sh`) loads the dumped DTB at.
const QEMU_DTB_ADDR: u64 = 0x4A00_0000;

// The low DRAM window scanned for the FDT header when no pointer is supplied.
const SCAN_START: u64 = 0x4000_0000;
const SCAN_END: u64 = 0x4800_0000;

// Read a big-endian u32 from a (byte-addressed) FDT offset.
unsafe fn be32(p: u64) -> u32 {
	let b = |o: u64| unsafe { read_volatile((p + o) as *const u8) };
	u32::from_be_bytes([b(0), b(1), b(2), b(3)])
}

// A plausible FDT header at `base`? (magic + a sane, self-consistent totalsize).
unsafe fn is_fdt(base: u64) -> bool {
	unsafe {
		if be32(base) != FDT_MAGIC {
			return false;
		}
		let totalsize = be32(base + 4);
		let off_struct = be32(base + 8);
		let off_strings = be32(base + 12);
		let version = be32(base + 20);
		totalsize >= 64 && totalsize < 0x20_0000 && off_struct < totalsize && off_strings < totalsize && version == 17
	}
}

// Find the FDT: use `hint` if it points at a valid header, else the runner's
// fixed load address, else a scan of low DRAM.
fn locate(hint: u64) -> Option<u64> {
	unsafe {
		if hint != 0 && is_fdt(hint) {
			return Some(hint);
		}
		if is_fdt(QEMU_DTB_ADDR) {
			return Some(QEMU_DTB_ADDR);
		}
		let mut base = SCAN_START;
		while base < SCAN_END {
			if is_fdt(base) {
				return Some(base);
			}
			base += 0x1000;
		}
	}
	None
}

// Compare a null-terminated FDT string at `p` against `s`.
unsafe fn str_eq(p: u64, s: &str) -> bool {
	unsafe {
		for (i, &c) in s.as_bytes().iter().enumerate() {
			if read_volatile((p + i as u64) as *const u8) != c {
				return false;
			}
		}
		read_volatile((p + s.len() as u64) as *const u8) == 0
	}
}

// Does the null-terminated FDT string at `p` start with `prefix`?
unsafe fn str_starts(p: u64, prefix: &str) -> bool {
	unsafe {
		for (i, &c) in prefix.as_bytes().iter().enumerate() {
			if read_volatile((p + i as u64) as *const u8) != c {
				return false;
			}
		}
		true
	}
}

// Length (excluding the terminator) of a null-terminated FDT string at `p`.
unsafe fn str_len(p: u64) -> u64 {
	let mut n = 0u64;
	while unsafe { read_volatile((p + n) as *const u8) } != 0 {
		n += 1;
	}
	n
}

// Combine `cells` big-endian u32 cells at `p` into a u64 (advancing `p`).
unsafe fn read_cells(p: &mut u64, cells: u32) -> u64 {
	let mut v = 0u64;
	for _ in 0..cells {
		v = (v << 32) | unsafe { be32(*p) } as u64;
		*p += 4;
	}
	v
}

// Parse the device tree reachable from `hint`, returning the RAM geometry and CPU
// count, or `None` if no valid FDT is found.
pub fn parse(hint: u64) -> Option<BootInfo> {
	let base = locate(hint)?;
	unsafe {
		let off_struct = be32(base + 8) as u64;
		let off_strings = be32(base + 12) as u64;
		let strings = base + off_strings;

		let mut p = base + off_struct;
		let mut depth: i32 = -1;
		let mut d1_memory = false; // inside a depth-1 "memory" node
		let mut d1_cpus = false; //   inside the depth-1 "cpus" node
		let mut d1_pcie = false; //   inside a depth-1 "pcie"/"pci" node
		let mut addr_cells: u32 = 2;
		let mut size_cells: u32 = 2;
		let mut ram_base: u64 = 0;
		let mut ram_size: u64 = 0;
		let mut cpu_count: u32 = 0;
		let mut pcie_ecam: u64 = 0;

		loop {
			let token = be32(p);
			p += 4;
			match token {
				FDT_BEGIN_NODE => {
					depth += 1;
					let name = p;
					p += (str_len(name) + 1 + 3) & !3; // name + NUL, padded to 4
					if depth == 1 {
						d1_memory = str_starts(name, "memory");
						d1_cpus = str_eq(name, "cpus");
						d1_pcie = str_starts(name, "pcie") || str_starts(name, "pci@");
					} else if depth == 2 && d1_cpus && str_starts(name, "cpu@") {
						cpu_count += 1;
					}
				}
				FDT_END_NODE => {
					if depth == 1 {
						d1_memory = false;
						d1_cpus = false;
						d1_pcie = false;
					}
					depth -= 1;
				}
				FDT_PROP => {
					let len = be32(p);
					let nameoff = be32(p + 4);
					let val = p + 8;
					p += 8 + ((len as u64 + 3) & !3);
					let pname = strings + nameoff as u64;
					if depth == 0 {
						if str_eq(pname, "#address-cells") {
							addr_cells = be32(val);
						} else if str_eq(pname, "#size-cells") {
							size_cells = be32(val);
						}
					} else if depth == 1 && d1_memory && str_eq(pname, "reg") {
						let mut q = val;
						let end = val + len as u64;
						let mut first = true;
						while q + 4 * (addr_cells + size_cells) as u64 <= end {
							let a = read_cells(&mut q, addr_cells);
							let s = read_cells(&mut q, size_cells);
							if first {
								ram_base = a;
								first = false;
							}
							ram_size += s;
						}
					} else if depth == 1 && d1_pcie && str_eq(pname, "reg") {
						// The pcie node's reg is <ecam_base ecam_size> in root cells.
						let mut q = val;
						pcie_ecam = read_cells(&mut q, addr_cells);
					}
				}
				FDT_NOP => {}
				FDT_END => break,
				_ => return None, // malformed
			}
		}

		if ram_size == 0 {
			return None;
		}
		Some(BootInfo { ram_base, ram_size, cpu_count: cpu_count.max(1), pcie_ecam })
	}
}
