//! A flattened device tree (FDT / DTB) reader, shared by everything in this system that boots
//! through one.
//!
//! SEPARATE FROM THE KERNEL SO THE LOADER CAN HAVE IT, AND SO A HOST CAN TEST IT. This was
//! `kernel::arch::common::dtb` and the loader could not reach it, which is why P02M0129 spent two
//! rounds recording that the post-`ExitBootServices` UART is a hard-coded QEMU address on both
//! device-tree targets: taking it from the tree needed this parser, and moving the parser was "a
//! refactor worth doing on its own terms rather than for six diagnostic lines". This is that
//! refactor, done on its own terms - the parse is unchanged, it acquired a console walk, and it can
//! now be run against a real device tree on a host rather than only inside a booted guest.
//!
//! The FDT is a standard, architecture-independent format (big-endian, magic 0xd00dfeed, a token
//! stream of node / property records). QEMU's `-machine virt` generates one for both aarch64 and
//! riscv64 describing the machine (RAM banks, CPUs, the PCIe ECAM, the console). Only two things
//! are arch-specific and stay in each backend's shim: how a physical address is read (the kernel
//! runs higher-half and reaches the blob through its direct map; the loader runs identity-mapped and
//! reads it straight) and where to look for the blob when no firmware pointer is supplied.
//!
//! NOTHING HERE ALLOCATES and nothing here is `&[u8]`-shaped: every read goes through
//! `phys_to_virt` and `read_volatile`, because the two callers reach the same bytes through
//! different mappings and one of them is running before the memory map is settled.

#![cfg_attr(not(test), no_std)]

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

// The structure and strings blocks, as physical spans, checked to lie inside the tree. Every walk
// takes one of these before it reads a token and is bounded by it from there on.
#[derive(Clone, Copy)]
struct Bounds {
	struct_start: u64,
	struct_end: u64,
	strings_start: u64,
	strings_end: u64,
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
		self.bounds().is_some()
	}

	// The two blocks the header declares, CHECKED TO FIT, computed once.
	//
	// `is_valid` used to check that `off_struct` and `off_strings` were each below `totalsize` and
	// stop there - so a header declaring a structure block that STARTS inside the tree and runs past
	// its end passed, and nothing below bounded the walk either: the token loop had no end test, a
	// string was scanned for a terminator with no ceiling, and a property advanced the cursor by its
	// own declared length without checking where that landed. The header carries `size_dt_struct`
	// and `size_dt_strings` for exactly this and neither was read.
	//
	// None of that matters on QEMU's tree or on any tree a real firmware writes. This crate exists
	// because the loader must not assume the machine behaves like QEMU, and it is read by the loader
	// AND the kernel - so it is the one place where bounding it bounds it everywhere.
	fn bounds(&self) -> Option<Bounds> {
		unsafe {
			if self.be32(self.base) != FDT_MAGIC {
				return None;
			}
			let totalsize = self.be32(self.base + 4) as u64;
			let off_struct = self.be32(self.base + 8) as u64;
			let off_strings = self.be32(self.base + 12) as u64;
			let size_struct = self.be32(self.base + 36) as u64;
			let size_strings = self.be32(self.base + 32) as u64;
			let version = self.be32(self.base + 20);
			if version != 17 || !(64..0x20_0000).contains(&totalsize) {
				return None;
			}
			// Each block starts inside the tree AND ends inside it. `checked_add` because both
			// halves come from the same untrusted header.
			let struct_end = off_struct.checked_add(size_struct)?;
			let strings_end = off_strings.checked_add(size_strings)?;
			if struct_end > totalsize || strings_end > totalsize {
				return None;
			}
			// A structure block with no room for a single token is not one this can walk.
			if size_struct < 4 {
				return None;
			}
			Some(Bounds { struct_start: self.base + off_struct, struct_end: self.base + struct_end, strings_start: self.base + off_strings, strings_end: self.base + strings_end })
		}
	}

	// Read a big-endian u32 that must lie WHOLLY inside `[.., end)`.
	unsafe fn be32_in(&self, p: u64, end: u64) -> Option<u32> {
		if p.checked_add(4)? > end {
			return None;
		}
		Some(unsafe { self.be32(p) })
	}

	// Length (excluding the terminator) of a null-terminated string at `p`, BOUNDED by the block it
	// is being read in. None when no terminator lies inside it.
	//
	// The bound is a parameter because the two kinds of caller read in different blocks: a node name
	// is in the structure block and a property name is in the strings block, and a length that walks
	// out of one and into the other is exactly what this is here to refuse.
	unsafe fn str_len_in(&self, p: u64, end: u64) -> Option<u64> {
		let mut n = 0u64;
		while p.checked_add(n)? < end {
			if unsafe { self.u8_at(p + n) } == 0 {
				return Some(n);
			}
			n += 1;
		}
		None
	}

	// One `FDT_PROP` record: its value span and where the next token starts, all inside the block.
	// `None` refuses the whole walk, which is the only safe answer to a record that does not fit.
	unsafe fn prop_in(&self, p: u64, b: &Bounds) -> Option<(u64, u32, u64, u64)> {
		unsafe {
			let len = self.be32_in(p, b.struct_end)? as u64;
			let nameoff = self.be32_in(p + 4, b.struct_end)?;
			let value = p.checked_add(8)?;
			// The value itself must fit, and so must the padding the next token starts after.
			if value.checked_add(len)? > b.struct_end {
				return None;
			}
			let next = value.checked_add((len + 3) & !3)?;
			if next > b.struct_end {
				return None;
			}
			let name = b.strings_start.checked_add(nameoff as u64)?;
			// And the property's NAME has a terminator inside the strings block.
			self.str_len_in(name, b.strings_end)?;
			Some((name, len as u32, value, next))
		}
	}

	// One `FDT_BEGIN_NODE` name: where it starts and where the next token is.
	unsafe fn node_name_in(&self, p: u64, b: &Bounds) -> Option<(u64, u64)> {
		let len = unsafe { self.str_len_in(p, b.struct_end)? };
		let next = p.checked_add((len + 1 + 3) & !3)?;
		if next > b.struct_end {
			return None;
		}
		Some((p, next))
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
		let Some(b) = self.bounds() else { return false };
		unsafe {
			let mut p = b.struct_start;
			let mut depth: i32 = -1;
			let mut d1_cpus = false;
			let mut in_cpu = false;
			loop {
				let Some(token) = self.be32_in(p, b.struct_end) else { return false };
				p += 4;
				match token {
					FDT_BEGIN_NODE => {
						depth += 1;
						let Some((name, next)) = self.node_name_in(p, &b) else { return false };
						p = next;
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
						let Some((pname, len, val, next)) = self.prop_in(p, &b) else { return false };
						let len = len as u64;
						p = next;
						if !in_cpu {
							continue;
						}
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
		let b = self.bounds()?;
		unsafe {
			let mut p = b.struct_start;
			// MINUS ONE, so the root node is depth 0 and `/cpus` is depth 1 - the same convention the
			// two walks beside this one use.
			//
			// It started at zero, which made the ROOT depth 1, so `in_cpus` was tested against the
			// root's name (empty) and never became true: this function returned None on every machine
			// that has ever booted, and riscv64 fell through to the QEMU constant it was written to
			// replace, printing "no /cpus/timebase-frequency in the device tree" over a tree that has
			// one. Found by moving this parser somewhere a real device tree could be handed to it -
			// which is the whole argument for the move.
			let mut depth = -1i32;
			let mut in_cpus = false;
			loop {
				let token = self.be32_in(p, b.struct_end)?;
				p += 4;
				match token {
					FDT_BEGIN_NODE => {
						depth += 1;
						let (name, next) = self.node_name_in(p, &b)?;
						p = next;
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
						let (pname, len, val, next) = self.prop_in(p, &b)?;
						p = next;
						// On `/cpus` itself, which is where the specification puts it. A per-cpu
						// override exists in the binding and is not read here: a machine whose harts
						// tick at different rates needs more than one number anyway, and inventing one
						// from the first hart would be a guess wearing a measurement's clothes.
						if depth == 1 && in_cpus && len == 4 && self.str_eq(pname, "timebase-frequency") {
							return Some(self.be32(val));
						}
					}
					FDT_NOP => {}
					FDT_END => return None,
					_ => return None,
				}
			}
		}
	}

	pub fn parse(&self) -> Option<BootInfo> {
		if !self.is_valid() {
			return None;
		}
		let b = self.bounds()?;
		unsafe {
			let mut p = b.struct_start;
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
				let token = self.be32_in(p, b.struct_end)?;
				p += 4;
				match token {
					FDT_BEGIN_NODE => {
						depth += 1;
						let (name, next) = self.node_name_in(p, &b)?; // name + NUL, padded to 4
						p = next;
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
						let (pname, len, val, next) = self.prop_in(p, &b)?;
						p = next;
						if depth == 0 {
							// FOUR BYTES, BECAUSE THAT IS WHAT THE PROPERTY IS. `prop_in` proves the
							// declared value lies inside the structure block; it does not prove the
							// value is as long as this reader assumes. A one-byte `#address-cells`
							// had `be32` reading three bytes of whatever followed it - inside the
							// blob, so no bounds check fires, and the cell count it produced then
							// decided how every `reg` in the tree was parsed.
							if self.str_eq(pname, "#address-cells") && len == 4 {
								addr_cells = self.be32(val);
							} else if self.str_eq(pname, "#size-cells") && len == 4 {
								size_cells = self.be32(val);
							}
						} else if depth == 1 && d1_memory && self.str_eq(pname, "reg") {
							// RAM IS NOT ONE RANGE, AND SUMMING THE BANKS PRETENDS IT IS.
							//
							// This took the first bank's base and added up every bank's SIZE, so a
							// board with a hole in its map - two 256 MB banks at 0x4000_0000 and
							// 0x8000_0000 - produced base 0x4000_0000 and size 512 MB. The kernel
							// builds its usable frame region from `ram_base .. ram_base + ram_size`,
							// so it would have handed out frames from the middle of the HOLE as
							// though they were memory, and stopped short of the second bank's real
							// end. Writing to a hole is not a fault the allocator can attribute:
							// the store simply goes nowhere and the data is gone.
							//
							// So banks are accumulated only while they are CONTIGUOUS with what has
							// been seen, and a gap ends the run. What this reports is a range that
							// genuinely is memory - which is the property the frame allocator needs
							// and the one the old arithmetic could not promise.
							//
							// The cost is the RAM past the first hole, which is not used. That is a
							// limitation and it is stated: see the milestone task for carrying the
							// real region list, which is the fix that keeps it. Losing memory is
							// recoverable by anyone who reads the boot report; handing out a hole is
							// not recoverable by anybody.
							let mut q = val;
							let end = val + len as u64;
							while q + 4 * (addr_cells + size_cells) as u64 <= end {
								let a = self.read_cells(&mut q, addr_cells);
								let s = self.read_cells(&mut q, size_cells);
								if s == 0 {
									continue;
								}
								if ram_size == 0 {
									ram_base = a;
									ram_size = s;
									continue;
								}
								// Contiguous with the run so far - in either direction, because a
								// device tree does not promise its banks are in address order.
								if a == ram_base + ram_size {
									ram_size += s;
								} else if a + s == ram_base {
									ram_base = a;
									ram_size += s;
								}
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
							// FOUR OR EIGHT AND NOTHING ELSE. The width was taken as "eight if the
							// length says eight, otherwise four", so a property of one, two or three
							// bytes - or of zero - still took a four-byte read past its own end.
							let value = match len {
								8 => ((self.be32(val) as u64) << 32) | self.be32(val + 4) as u64,
								4 => self.be32(val) as u64,
								_ => continue,
							};
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

// ---------------------------------------------------------------------------------------------
// The console the firmware was using, taken from the tree rather than guessed.
// ---------------------------------------------------------------------------------------------

// The UARTs this system has a driver for. A machine whose console is anything else is a machine
// this code says nothing about, which is the whole point: the alternative to "I do not know" here
// was storing bytes to an address somebody wrote down while looking at QEMU.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Uart {
	// `arm,pl011` - a 32-bit data register at +0x00 and a flag register at +0x18.
	Pl011,
	// `ns16550a` / `ns16550` / `snps,dw-apb-uart` - a transmit holding register and a line status
	// register, spaced by `reg-shift` bytes.
	Ns16550,
}

// How a PSCI call reaches its implementation on this platform.
//
// A property of the PLATFORM and not of the exception level: what the caller runs at says nothing
// about which of the two instructions is trapped by something that implements PSCI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PsciConduit {
	// `smc #0` - PSCI in EL3 firmware, which is most server-class AArch64.
	Smc,
	// `hvc #0` - PSCI in a hypervisor at EL2, which is QEMU's `virt`.
	Hvc,
}

// Where the console is and what it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Console {
	pub uart: Uart,
	pub base: u64,
	// Register spacing for the 16550 family: 0 on QEMU's `ns16550a`, 2 on `snps,dw-apb-uart`, and
	// getting it wrong writes the baud divisor where the character should go. Always 0 for PL011,
	// whose registers are at fixed offsets.
	pub reg_shift: u32,
}

// A device-tree path is short and this reader does not allocate, so it goes in a fixed buffer. The
// longest in either QEMU tree is under thirty bytes; a path that does not fit is answered "no
// console" rather than truncated, because a truncated path names a DIFFERENT node.
const MAX_PATH: usize = 128;
// Device trees are shallow - three levels in both QEMU trees - and the cell stack is indexed by
// depth, so this bounds it. Deeper than this answers "no console".
const MAX_DEPTH: usize = 12;

impl Fdt {
	// The console the firmware described, or None when the tree does not name one this system can
	// drive.
	//
	// `/chosen/stdout-path` is the standard answer to "which device is the console", and it is what
	// the firmware itself was writing to - so taking the address from here is taking it from the
	// machine rather than from a machine somebody had.
	//
	// None is a real answer and the callers must treat it as one: a loader that cannot find the
	// console prints NOTHING after `ExitBootServices` rather than storing to an address it made up.
	pub fn console(&self) -> Option<Console> {
		if !self.is_valid() {
			return None;
		}
		// SAFETY: the header was validated, so the struct and strings blocks lie inside the tree -
		// the same contract every other walk in this file relies on.
		unsafe {
			let mut path = [0u8; MAX_PATH];
			let mut len = self.stdout_path(&mut path)?;
			// An alias rather than a path: `stdout-path = "serial0:115200n8"` is how riscv64's virt
			// tree names it, and `/aliases/serial0` is where the path itself lives. One level of
			// indirection, which is all the specification defines.
			if path[0] != b'/' {
				let mut resolved = [0u8; MAX_PATH];
				len = self.alias(&path[..len], &mut resolved)?;
				path = resolved;
			}
			self.console_at(&path[..len])
		}
	}

	// `/chosen/stdout-path`, with the `:options` suffix cut off. Returns how many bytes were
	// written into `out`.
	unsafe fn stdout_path(&self, out: &mut [u8; MAX_PATH]) -> Option<usize> {
		unsafe { self.property_string(&[b"chosen"], b"stdout-path", out).or_else(|| self.property_string(&[b"chosen"], b"linux,stdout-path", out)) }
	}

	// `/aliases/<name>`.
	unsafe fn alias(&self, name: &[u8], out: &mut [u8; MAX_PATH]) -> Option<usize> {
		unsafe { self.property_string(&[b"aliases"], name, out) }
	}

	// Which instruction reaches this platform's PSCI implementation, as the tree states it.
	//
	// TAKEN FROM THE MACHINE, not inferred from the exception level. The loader used to answer
	// `PSCI_HVC` for any boot that did not start at EL2, which is true of QEMU's `virt` and false of
	// most server-class AArch64 - where EL2 belongs to a hypervisor and PSCI lives in EL3 firmware
	// behind `smc`. That is the same class of assumption as the hard-coded UART address this crate
	// was extracted to remove: a value that is right on the machine it was written against.
	//
	// The binding is `/psci` with a required `method` property whose value is `"smc"` or `"hvc"`.
	// `None` means the tree does not describe PSCI at all - no node, no `method`, or a method this
	// system has no instruction for - and a caller must treat that as "no PSCI" rather than
	// guessing, which is the whole point.
	pub fn psci_conduit(&self) -> Option<PsciConduit> {
		if !self.is_valid() {
			return None;
		}
		let mut method = [0u8; MAX_PATH];
		// SAFETY: the header was validated, so every read below is bounded by the blocks it
		// declares - the same contract every other walk in this file relies on.
		let len = unsafe { self.property_string(&[b"psci"], b"method", &mut method)? };
		match &method[..len] {
			b"smc" => Some(PsciConduit::Smc),
			b"hvc" => Some(PsciConduit::Hvc),
			_ => None,
		}
	}

	// Copy the string property `want` of the node at `path` into `out`, stopping at a `:` - the
	// separator between a stdout path and its baud options. Returns the length written.
	unsafe fn property_string(&self, path: &[&[u8]], want: &[u8], out: &mut [u8; MAX_PATH]) -> Option<usize> {
		unsafe {
			let mut found = None;
			self.walk_to(
				path,
				|value, len, _cells| {
					if len == 0 {
						return;
					}
					let mut written = 0usize;
					while written < len as usize && written < MAX_PATH {
						let byte = self.u8_at(value + written as u64);
						if byte == 0 || byte == b':' {
							break;
						}
						out[written] = byte;
						written += 1;
					}
					// A path that filled the buffer is a path that may have been cut, and a cut path
					// names a different node. Refuse it.
					found = (written > 0 && written < MAX_PATH).then_some(written);
				},
				want,
			);
			found
		}
	}

	// Read `compatible`, `reg` and `reg-shift` off the node at `path` and decide what it is.
	unsafe fn console_at(&self, path: &[u8]) -> Option<Console> {
		// The path split into components, without allocating. `/soc/serial@10000000` is two.
		let mut parts: [&[u8]; MAX_DEPTH] = [b""; MAX_DEPTH];
		let mut count = 0usize;
		for component in path.split(|byte| *byte == b'/') {
			if component.is_empty() {
				continue;
			}
			if count == MAX_DEPTH {
				return None;
			}
			parts[count] = component;
			count += 1;
		}
		if count == 0 {
			return None;
		}

		let mut uart: Option<Uart> = None;
		let mut base: Option<u64> = None;
		let mut reg_shift = 0u32;
		unsafe {
			self.walk_node(&parts[..count], |name, value, len, cells| {
				if self.str_eq(name, "compatible") {
					// A `compatible` is a list, most specific first, and a machine may name a chip
					// this system does not know followed by one it does. So every entry is tried
					// rather than only the first.
					if uart.is_none() {
						uart = self.uart_from_compatible(value, len);
					}
				} else if self.str_eq(name, "reg") && base.is_none() {
					// The cell count comes from the tree, so it is bounded before it is used as a
					// read length: four 32-bit cells is a 128-bit address and there is no such
					// machine, while a garbage value would walk this read off the end of the
					// property. A tree that says something impossible gets no console rather than
					// an address assembled out of whatever followed.
					let mut at = value;
					if (1..=4).contains(&cells.0) && len >= 4 * cells.0 {
						base = Some(self.read_cells(&mut at, cells.0));
					}
				} else if self.str_eq(name, "reg-shift") && len == 4 {
					reg_shift = self.be32(value);
				}
			});
		}
		let uart = uart?;
		let base = base.filter(|address| *address != 0)?;
		Some(Console { uart, base, reg_shift: if uart == Uart::Pl011 { 0 } else { reg_shift } })
	}

	// Which of the two drivers, if either, a `compatible` stringlist names.
	unsafe fn uart_from_compatible(&self, value: u64, len: u32) -> Option<Uart> {
		let mut at = value;
		let end = value + len as u64;
		while at < end {
			let entry = at;
			// BOUNDED BY THE PROPERTY, not by the first zero byte anywhere after it. This used the
			// unbounded `str_len`, which walks until it finds a NUL - so a `compatible` whose last
			// entry has no terminator read past the end of its own property and kept going. The
			// bounded reader exists for exactly this class and was already used elsewhere; this was
			// the one call site still on the old one.
			let Some(entry_len) = (unsafe { self.str_len_in(entry, end) }) else {
				return None;
			};
			at += entry_len + 1;
			for (name, uart) in [("arm,pl011", Uart::Pl011), ("ns16550a", Uart::Ns16550), ("ns16550", Uart::Ns16550), ("snps,dw-apb-uart", Uart::Ns16550)] {
				if unsafe { self.str_eq(entry, name) } {
					return Some(uart);
				}
			}
		}
		None
	}

	// Visit every property of the node named by `parts`, with the parent's `(#address-cells,
	// #size-cells)` - which is what `reg` is expressed in, and which is NOT the root's on riscv64,
	// where the console lives under `/soc`.
	unsafe fn walk_node(&self, parts: &[&[u8]], visit: impl FnMut(u64, u64, u32, (u32, u32))) {
		// A malformed record stops the walk, which for this one means "no more properties" - its
		// callers already treat an absent property as the answer.
		let _ = unsafe { self.walk_node_inner(parts, visit) };
	}

	unsafe fn walk_node_inner(&self, parts: &[&[u8]], mut visit: impl FnMut(u64, u64, u32, (u32, u32))) -> Option<()> {
		// The root has no parent to take its `reg` cells from, and every caller here names at least
		// one component. Stated rather than assumed, because the alternative is an index of -1.
		if parts.is_empty() {
			return None;
		}
		let b = self.bounds()?;
		unsafe {
			let mut p = b.struct_start;
			// What each depth declares for its CHILDREN. The specification's fallback when a node
			// says nothing is the parent's value; the root's own default is 2/2 here, which is what
			// every tree this system meets writes explicitly anyway.
			let mut cells = [(2u32, 2u32); MAX_DEPTH + 1];
			let mut depth: i32 = -1;
			// The depth of the deepest node on `parts` we are currently inside; 0 is the root.
			let mut matched: i32 = -1;
			loop {
				let token = self.be32_in(p, b.struct_end)?;
				p += 4;
				match token {
					FDT_BEGIN_NODE => {
						depth += 1;
						let (name, next) = self.node_name_in(p, &b)?;
						p = next;
						if depth as usize > MAX_DEPTH {
							return None;
						}
						if depth == 0 {
							matched = 0;
						} else {
							cells[depth as usize] = cells[depth as usize - 1];
							let index = depth as usize - 1;
							if matched == depth - 1 && index < parts.len() && self.name_matches(name, parts[index]) {
								matched = depth;
							}
						}
					}
					FDT_END_NODE => {
						if matched == depth {
							matched = depth - 1;
						}
						depth -= 1;
						if depth < 0 {
							return None;
						}
					}
					FDT_PROP => {
						let (pname, len, value, next) = self.prop_in(p, &b)?;
						p = next;
						if depth < 0 || depth as usize > MAX_DEPTH {
							return None;
						}
						// `#address-cells` / `#size-cells` govern this node's CHILDREN, and the
						// properties of a node precede its children in the token stream - so by the
						// time a child's `reg` is read, its parent's cells are known.
						if len == 4 && self.str_eq(pname, "#address-cells") {
							cells[depth as usize].0 = self.be32(value);
						} else if len == 4 && self.str_eq(pname, "#size-cells") {
							cells[depth as usize].1 = self.be32(value);
						}
						if matched == depth && depth as usize == parts.len() {
							visit(pname, value, len, cells[depth as usize - 1]);
						}
					}
					FDT_NOP => {}
					FDT_END => return Some(()),
					_ => return None,
				}
			}
		}
	}

	// The same walk, for one named property of a node reached by name components. Kept separate
	// from `walk_node` because the callers want different things: this one wants a value and stops
	// caring, that one wants every property of a node at once.
	unsafe fn walk_to(&self, parts: &[&[u8]], mut found: impl FnMut(u64, u32, (u32, u32)), want: &[u8]) {
		unsafe {
			self.walk_node(parts, |name, value, len, cells| {
				if self.str_eq_bytes(name, want) {
					found(value, len, cells);
				}
			});
		}
	}

	// Does the node name at `p` match this path component? Exactly, or up to the unit address -
	// `soc` matches `soc`, and `serial` matches `serial@10000000`, while `pci` does not match
	// `pcie@...`.
	unsafe fn name_matches(&self, p: u64, want: &[u8]) -> bool {
		unsafe {
			for (index, &byte) in want.iter().enumerate() {
				if self.u8_at(p + index as u64) != byte {
					return false;
				}
			}
			let next = self.u8_at(p + want.len() as u64);
			next == 0 || next == b'@'
		}
	}

	// `str_eq` against bytes rather than a `&str`, for a property name that came out of the tree.
	unsafe fn str_eq_bytes(&self, p: u64, s: &[u8]) -> bool {
		unsafe {
			for (index, &byte) in s.iter().enumerate() {
				if self.u8_at(p + index as u64) != byte {
					return false;
				}
			}
			self.u8_at(p + s.len() as u64) == 0
		}
	}
}

#[cfg(test)]
mod tests;
