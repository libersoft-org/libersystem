// Portable flattened-device-tree (FDT / DTB) parser - shared by every arch backend
// that boots via a device tree.
//
// The FDT is a standard, architecture-independent format (big-endian, magic
// 0xd00dfeed, a token stream of node / property records). QEMU's `-machine virt`
// generates one for both aarch64 and riscv64 describing the machine (RAM banks, CPUs,
// the PCIe ECAM). Only two things are arch-specific and stay in each backend's shim:
// how a physical address is read (the higher-half `phys_to_virt`, handed in here) and
// where to look for the blob when no firmware/loader pointer is supplied (the QEMU
// DRAM window). This parser extracts just what early bring-up needs, so the frame
// allocator and the per-CPU pool stop hard-coding the RAM size and CPU count.

// Only the arch backend(s) with a device tree use this; it is dead code on the others.
#![allow(dead_code)]

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

// FDT token + header constants.
const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

// A flattened device tree at a physical base, read through a backend-supplied
// `phys_to_virt` (the kernel runs higher-half, so every FDT byte is reached through
// the direct map rather than a raw physical pointer).
pub struct Fdt {
	base: u64,
	p2v: fn(u64) -> u64,
}

impl Fdt {
	// An FDT at physical `base`, reachable through `phys_to_virt`.
	pub fn new(base: u64, phys_to_virt: fn(u64) -> u64) -> Self {
		Self { base, p2v: phys_to_virt }
	}

	// Read a byte at physical `pa` through the direct map.
	unsafe fn u8_at(&self, pa: u64) -> u8 {
		unsafe { read_volatile((self.p2v)(pa) as *const u8) }
	}

	// Read a big-endian u32 from a (byte-addressed) FDT offset.
	unsafe fn be32(&self, p: u64) -> u32 {
		unsafe { u32::from_be_bytes([self.u8_at(p), self.u8_at(p + 1), self.u8_at(p + 2), self.u8_at(p + 3)]) }
	}

	// A plausible FDT header at `base`? (magic + a sane, self-consistent totalsize).
	pub fn is_valid(&self) -> bool {
		unsafe {
			if self.be32(self.base) != FDT_MAGIC {
				return false;
			}
			let totalsize = self.be32(self.base + 4);
			let off_struct = self.be32(self.base + 8);
			let off_strings = self.be32(self.base + 12);
			let version = self.be32(self.base + 20);
			totalsize >= 64 && totalsize < 0x20_0000 && off_struct < totalsize && off_strings < totalsize && version == 17
		}
	}

	// Compare a null-terminated FDT string at `p` against `s`.
	unsafe fn str_eq(&self, p: u64, s: &str) -> bool {
		unsafe {
			for (i, &c) in s.as_bytes().iter().enumerate() {
				if self.u8_at(p + i as u64) != c {
					return false;
				}
			}
			self.u8_at(p + s.len() as u64) == 0
		}
	}

	// Does the null-terminated FDT string at `p` start with `prefix`?
	unsafe fn str_starts(&self, p: u64, prefix: &str) -> bool {
		unsafe {
			for (i, &c) in prefix.as_bytes().iter().enumerate() {
				if self.u8_at(p + i as u64) != c {
					return false;
				}
			}
			true
		}
	}

	// Length (excluding the terminator) of a null-terminated FDT string at `p`.
	unsafe fn str_len(&self, p: u64) -> u64 {
		let mut n = 0u64;
		while unsafe { self.u8_at(p + n) } != 0 {
			n += 1;
		}
		n
	}

	// Combine `cells` big-endian u32 cells at `p` into a u64 (advancing `p`).
	unsafe fn read_cells(&self, p: &mut u64, cells: u32) -> u64 {
		let mut v = 0u64;
		for _ in 0..cells {
			v = (v << 32) | unsafe { self.be32(*p) } as u64;
			*p += 4;
		}
		v
	}

	// Parse the device tree, returning the RAM geometry, CPU count and PCIe ECAM base,
	// or None if it is not a valid FDT (or has no memory node).
	pub fn parse(&self) -> Option<BootInfo> {
		if !self.is_valid() {
			return None;
		}
		unsafe {
			let off_struct = self.be32(self.base + 8) as u64;
			let off_strings = self.be32(self.base + 12) as u64;
			let strings = self.base + off_strings;

			let mut p = self.base + off_struct;
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
				let token = self.be32(p);
				p += 4;
				match token {
					FDT_BEGIN_NODE => {
						depth += 1;
						let name = p;
						p += (self.str_len(name) + 1 + 3) & !3; // name + NUL, padded to 4
						if depth == 1 {
							d1_memory = self.str_starts(name, "memory");
							d1_cpus = self.str_eq(name, "cpus");
							d1_pcie = self.str_starts(name, "pcie") || self.str_starts(name, "pci@");
						} else if depth == 2 && d1_cpus && self.str_starts(name, "cpu@") {
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
						let len = self.be32(p);
						let nameoff = self.be32(p + 4);
						let val = p + 8;
						p += 8 + ((len as u64 + 3) & !3);
						let pname = strings + nameoff as u64;
						if depth == 0 {
							if self.str_eq(pname, "#address-cells") {
								addr_cells = self.be32(val);
							} else if self.str_eq(pname, "#size-cells") {
								size_cells = self.be32(val);
							}
						} else if depth == 1 && d1_memory && self.str_eq(pname, "reg") {
							let mut q = val;
							let end = val + len as u64;
							let mut first = true;
							while q + 4 * (addr_cells + size_cells) as u64 <= end {
								let a = self.read_cells(&mut q, addr_cells);
								let s = self.read_cells(&mut q, size_cells);
								if first {
									ram_base = a;
									first = false;
								}
								ram_size += s;
							}
						} else if depth == 1 && d1_pcie && self.str_eq(pname, "reg") {
							// The pcie node's reg is <ecam_base ecam_size> in root cells.
							let mut q = val;
							pcie_ecam = self.read_cells(&mut q, addr_cells);
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
}
