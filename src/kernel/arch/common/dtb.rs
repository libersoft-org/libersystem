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
	// PLIC (RISC-V platform interrupt controller) base (0 if none / not RISC-V).
	pub plic_base: u64,
	// QEMU fw-cfg MMIO base (0 if the tree has no fw-cfg node). Drives the ramfb
	// early framebuffer.
	pub fwcfg_base: u64,
	// The boot-module archive handed over as an initrd: `/chosen/linux,initrd-start` and
	// `linux,initrd-end`. Both 0 when none was supplied.
	//
	// This is how a kernel that carries no userspace gets one on a machine with no bootloader
	// module hand-off. x86_64 receives named modules from its own UEFI loader; here QEMU loads
	// the archive and writes its range into the device tree, which is the same idea through
	// the mechanism this platform actually has.
	pub modules_start: u64,
	pub modules_end: u64,
}

// Where the runner places the boot-module archive: high in RAM, below the top, so the frame
// pool can simply stop underneath it. The kernel scans for it rather than agreeing on an exact
// address, which is the same discovery pattern the FDT above uses (hint, then fixed address,
// then scan) - one less constant that two components have to keep equal.
pub const MODULES_SCAN_BYTES: u64 = 64 * 1024 * 1024;

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
	// Does any CPU node advertise the ISA extension `want`?
	//
	// Two properties carry this and both are in the wild: the old `riscv,isa` string
	// ("rv64imafdc_svpbmt_...", underscore-separated after the single-letter base) and the
	// newer `riscv,isa-extensions` stringlist (NUL-separated). The search is a substring scan
	// over the property bytes, which is enough for a name that cannot occur inside another -
	// and deliberately conservative: a name it fails to find leaves the feature off.
	//
	// `want` must be lowercase; device trees are.
	pub fn has_isa_extension(&self, want: &[u8]) -> bool {
		if !self.is_valid() || want.is_empty() {
			return false;
		}
		unsafe {
			let off_struct = self.be32(self.base + 8) as u64;
			let off_strings = self.be32(self.base + 12) as u64;
			let strings = self.base + off_strings;
			let mut p = self.base + off_struct;
			let mut depth: i32 = -1;
			let mut d1_cpus = false;
			let mut in_cpu = false;
			loop {
				let token = self.be32(p);
				p += 4;
				match token {
					FDT_BEGIN_NODE => {
						depth += 1;
						let name = p;
						p += (self.str_len(name) + 1 + 3) & !3;
						if depth == 1 {
							d1_cpus = self.str_eq(name, "cpus");
						} else if depth == 2 && d1_cpus && self.str_starts(name, "cpu@") {
							in_cpu = true;
						}
					}
					FDT_END_NODE => {
						if depth == 2 {
							in_cpu = false;
						}
						if depth == 1 {
							d1_cpus = false;
						}
						depth -= 1;
						if depth < 0 {
							return false;
						}
					}
					FDT_PROP => {
						let len = self.be32(p) as u64;
						let nameoff = self.be32(p + 4);
						let val = p + 8;
						p += 8 + ((len + 3) & !3);
						if !in_cpu {
							continue;
						}
						let pname = strings + nameoff as u64;
						if !(self.str_eq(pname, "riscv,isa") || self.str_eq(pname, "riscv,isa-extensions")) {
							continue;
						}
						// Substring scan over the property's bytes, bounded by its own length.
						if len < want.len() as u64 {
							continue;
						}
						for start in 0..=(len - want.len() as u64) {
							if (0..want.len() as u64).all(|i| self.u8_at(val + start + i) == want[i as usize]) {
								return true;
							}
						}
					}
					FDT_NOP => {}
					FDT_END => return false,
					_ => return false,
				}
			}
		}
	}

	// `/cpus/timebase-frequency`, in Hz, or None when the tree does not carry it.
	//
	// RISC-V's `time` CSR counts at a rate the hardware chooses, and the kernel had it as the
	// constant 10,000,000 with a comment naming QEMU's virt machine. Every timeout, tick
	// conversion, timer and deadline is scaled by it, so on a machine that ticks at a different
	// rate all of them are wrong by that ratio - silently, because nothing compares the two.
	//
	// A separate walk rather than a field on `BootInfo`: this is read at clock init, which happens
	// before the boot info is built, and it is one property.
	pub fn timebase_frequency(&self) -> Option<u32> {
		if !self.is_valid() {
			return None;
		}
		// SAFETY: the header was validated above, so the struct and strings blocks lie inside the
		// tree this `Fdt` was built on - the same contract every other walk in this file relies on.
		unsafe { self.timebase_frequency_inner() }
	}

	unsafe fn timebase_frequency_inner(&self) -> Option<u32> {
		let strings = self.base + self.be32(self.base + 12) as u64;
		let mut p = self.base + self.be32(self.base + 8) as u64;
		let mut depth = 0i32;
		let mut in_cpus = false;
		loop {
			let token = self.be32(p);
			p += 4;
			match token {
				FDT_BEGIN_NODE => {
					depth += 1;
					let name = p;
					p += (self.str_len(name) + 1 + 3) & !3;
					if depth == 1 {
						in_cpus = self.str_eq(name, "cpus");
					}
				}
				FDT_END_NODE => {
					if depth == 1 {
						in_cpus = false;
					}
					depth -= 1;
					if depth < 0 {
						return None;
					}
				}
				FDT_PROP => {
					let len = self.be32(p);
					let nameoff = self.be32(p + 4);
					let val = p + 8;
					p += 8 + ((len as u64 + 3) & !3);
					// On `/cpus` itself, which is where the specification puts it. A per-cpu
					// override exists in the binding and is not read here: a machine whose harts
					// tick at different rates needs more than one number anyway, and inventing one
					// from the first hart would be a guess wearing a measurement's clothes.
					if depth == 1 && in_cpus && len == 4 && self.str_eq(strings + nameoff as u64, "timebase-frequency") {
						return Some(self.be32(val));
					}
				}
				FDT_NOP => {}
				FDT_END => return None,
				_ => return None,
			}
		}
	}

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
			// The pcie/pci and plic nodes sit at the root on aarch64 virt but under /soc
			// (depth 2) on riscv64 virt, so track them by the depth at which we entered.
			let mut in_pcie: i32 = -1;
			let mut in_plic: i32 = -1;
			let mut in_fwcfg: i32 = -1;
			let mut d1_chosen = false;
			let mut addr_cells: u32 = 2;
			let mut size_cells: u32 = 2;
			let mut ram_base: u64 = 0;
			let mut ram_size: u64 = 0;
			let mut cpu_count: u32 = 0;
			let mut pcie_ecam: u64 = 0;
			let mut plic_base: u64 = 0;
			let mut fwcfg_base: u64 = 0;
			let mut modules_start: u64 = 0;
			let mut modules_end: u64 = 0;

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
							d1_chosen = self.str_eq(name, "chosen");
						} else if depth == 2 && d1_cpus && self.str_starts(name, "cpu@") {
							cpu_count += 1;
						}
						if in_pcie < 0 && (self.str_starts(name, "pcie") || self.str_starts(name, "pci@")) {
							in_pcie = depth;
						}
						if in_plic < 0 && self.str_starts(name, "plic") {
							in_plic = depth;
						}
						if in_fwcfg < 0 && self.str_starts(name, "fw-cfg") {
							in_fwcfg = depth;
						}
					}
					FDT_END_NODE => {
						if depth == 1 {
							d1_memory = false;
							d1_cpus = false;
							d1_chosen = false;
						}
						if depth == in_pcie {
							in_pcie = -1;
						}
						if depth == in_plic {
							in_plic = -1;
						}
						if depth == in_fwcfg {
							in_fwcfg = -1;
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
						} else if in_pcie == depth && self.str_eq(pname, "reg") {
							// The pcie node's reg is <ecam_base ecam_size> in root cells.
							let mut q = val;
							pcie_ecam = self.read_cells(&mut q, addr_cells);
						} else if in_plic == depth && self.str_eq(pname, "reg") {
							// The plic node's reg is <plic_base plic_size> in root cells.
							let mut q = val;
							plic_base = self.read_cells(&mut q, addr_cells);
						} else if depth == 1 && d1_chosen && (self.str_eq(pname, "linux,initrd-start") || self.str_eq(pname, "linux,initrd-end")) {
							// QEMU writes these as one or two cells depending on the machine, so
							// the width is taken from the property length rather than assumed -
							// reading a 4-byte property as 8 would produce a wild address.
							let value = if len == 8 { ((self.be32(val) as u64) << 32) | self.be32(val + 4) as u64 } else { self.be32(val) as u64 };
							if self.str_eq(pname, "linux,initrd-start") {
								modules_start = value;
							} else {
								modules_end = value;
							}
						} else if in_fwcfg == depth && self.str_eq(pname, "reg") {
							// The fw-cfg node's reg is <fwcfg_base size> in root cells.
							let mut q = val;
							fwcfg_base = self.read_cells(&mut q, addr_cells);
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
			Some(BootInfo { ram_base, ram_size, cpu_count: cpu_count.max(1), pcie_ecam, plic_base, fwcfg_base, modules_start, modules_end })
		}
	}
}
