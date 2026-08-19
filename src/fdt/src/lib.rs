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
	// The CONTIGUOUS run from the first bank, which is what a caller that wants one range gets.
	pub ram_base: u64,
	pub ram_size: u64,
	// EVERY BANK THE TREE DECLARES, which is what the frame allocator actually wants.
	//
	// `ram_base`/`ram_size` describe a run and stop at the first hole, so a board with discontiguous
	// RAM lost everything past it - safe, because a hole handed out as memory is a store that goes
	// nowhere, and lossy, because the loss is silent to anyone not reading the boot report. The
	// allocator takes a LIST of regions and always has; the reader was the side that collapsed it.
	//
	// Banks arrive in whatever order the tree wrote them and are coalesced when adjacent. More banks
	// than this array holds are dropped and `ram_region_count` says how many were kept - capped
	// rather than refused, because refusing the tree falls back to built-in defaults, which is a
	// worse answer than sixteen banks of a seventeen-bank machine.
	pub ram_regions: [(u64, u64); MAX_RAM_REGIONS],
	pub ram_region_count: usize,
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
// The widest `#address-cells` / `#size-cells` this reader can hold.
//
// `read_cells` folds cells into a `u64`, which is two cells. A tree declaring more describes
// addresses this reader cannot represent, and reading them anyway shifts the high cells out and
// keeps the low bits - a wrong address that looks like a right one. Two is also what every tree in
// the wild declares for a 64-bit machine, so the bound refuses nothing real.
const MAX_CELLS: u32 = 2;

// How many separate RAM banks the reader will carry. Boards in this tree declare one or two; the
// array is generous so the cap is not a limit anyone meets.
pub const MAX_RAM_REGIONS: usize = 16;

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
	//
	// # Safety
	// `base` must be the physical address a bootloader or firmware published for a device tree, and
	// `phys_to_virt` must map every address in `[base, base + totalsize)` to a readable one. EVERY
	// METHOD ON THIS TYPE DEREFERENCES WHAT THIS WAS HANDED: `u8_at` is a `read_volatile` through
	// `p2v`, so `Fdt::new(1, identity).is_valid()` was safe Rust that read address 1 (FDT-007).
	//
	// The header checks below cannot substitute for this. They read the blob to decide whether it is
	// a blob, which means the first four bytes are already dereferenced by the time anything has been
	// validated - a check inside the object cannot make constructing it safe.
	pub unsafe fn new(base: u64, phys_to_virt: fn(u64) -> u64) -> Self {
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
			let last_comp_version = self.be32(self.base + 24);
			// BACKWARD COMPATIBLE WITH 17, NOT EXACTLY 17.
			//
			// This was `version != 17`, and the format carries `last_comp_version` precisely so a
			// client can accept a NEWER tree that remains compatible with the version it implements.
			// Refusing one is a boot failure whose cause reads like a corrupt device tree. It is not
			// a blocker on any machine this runs on - QEMU's `virt` writes version 17 with
			// `last_comp_version` 16 - which is why it is worth fixing before it is one.
			//
			// An OLDER tree is still refused: version 16 has no `size_dt_struct`, which this reader
			// uses to bound the walk.
			if version < 17 || last_comp_version > 17 || !(64..0x20_0000).contains(&totalsize) {
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
	// ONE CELL READ, WITH EVERY CHECK IT NEEDS. `pcie_ecam`, `plic_base` and `fwcfg_base` called
	// `read_cells` without first requiring the property to BE `cells * 4` bytes long, so a short
	// `reg` read the padding after it or the next token as an address - and that address is used for
	// real MMIO writes after `ExitBootServices`.
	//
	// `read_cells` folds cells into a `u64` by shifting, so three or more cells silently drop the
	// high ones; the width is bounded here rather than at each caller, which is how the console path
	// came to accept four.
	//
	// `Option`, so a caller that cannot read its property leaves the previous value alone instead of
	// adopting whatever the bytes happened to be.
	unsafe fn read_cells_property(&self, value: u64, len: u32, cells: u32) -> Option<u64> {
		if cells == 0 || cells > MAX_CELLS {
			return None;
		}
		let want = cells.checked_mul(4)?;
		if len < want {
			return None;
		}
		let mut p = value;
		Some(unsafe { self.read_cells(&mut p, cells) })
	}

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
	// Does EVERY CPU node advertise the ISA extension `want`?
	//
	// Two properties carry this and both are in the wild: the old `riscv,isa` string
	// ("rv64imafdc_svpbmt_...", underscore-separated after the single-letter base) and the
	// newer `riscv,isa-extensions` stringlist (NUL-separated).
	//
	// EVERY CPU, NOT ANY (FDT-004). This returned `true` on the first hart that advertised the
	// name, and the kernel then used the extension on every hart - which on a heterogeneous machine
	// is an illegal instruction on the ones that do not have it. A tree with no CPU node at all
	// answers `false` for the same reason it always did: an undetected extension is one this port
	// does not use.
	//
	// WHOLE NAMES, NOT A SUBSTRING (FDT-004). The search was a byte scan over the property, so
	// `want = "c"` matched inside `zicsr`, `want = "sv"` matched inside `svpbmt`, and any name that
	// is a prefix or infix of another was reported present. Now `riscv,isa-extensions` is compared
	// element by element against its NUL-separated entries, and `riscv,isa` is split the way the
	// specification defines it: a base like `rv64imafdc` whose letters after the width are
	// single-letter extensions, then `_`-separated multi-letter names.
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
			let mut cpus = 0usize;
			let mut cpus_with = 0usize;
			let mut this_cpu_has = false;
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
							cpus += 1;
							this_cpu_has = false;
						}
					}
					FDT_END_NODE => {
						if depth == 2 && in_cpu {
							// DECIDED AT THE NODE'S END. Either property may carry the name and a
							// tree does not promise property order, so the verdict for this hart is
							// taken once everything about it has been seen.
							if this_cpu_has {
								cpus_with += 1;
							}
							in_cpu = false;
						}
						if depth == 1 {
							d1_cpus = false;
						}
						depth -= 1;
						// CLOSING THE ROOT LEAVES -1, WHICH IS THE END AND NOT AN ERROR. This was
						// `depth < 0`, which was harmless while the function answered `true` the
						// moment it found a match: nothing reached the root's own `FDT_END_NODE`.
						// The verdict is taken at `FDT_END` now, and this returned `false` one token
						// before it - so every positive answer became a negative one.
						if depth < -1 {
							return false;
						}
					}
					FDT_PROP => {
						let Some((pname, len, val, next)) = self.prop_in(p, &b) else { return false };
						p = next;
						if !in_cpu {
							continue;
						}
						if self.str_eq(pname, "riscv,isa-extensions") {
							if self.stringlist_contains(val, len, want) {
								this_cpu_has = true;
							}
						} else if self.str_eq(pname, "riscv,isa") && self.isa_string_names(val, len, want) {
							this_cpu_has = true;
						}
					}
					FDT_NOP => {}
					// A TREE THAT ENDS IS NOT A TREE THAT ANSWERED. Reaching the terminator means no
					// `/cpus` closed with a verdict, which is the same "found nothing" as a tree
					// with no CPU nodes.
					FDT_END => return cpus > 0 && cpus_with == cpus,
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
			// `device_type = "memory"` seen, and `status` not `disabled`/`fail`. Both are properties,
			// and a device tree does not promise property order - so the node's `reg` is REMEMBERED
			// and the banks are committed at the node's end, once everything about it is known.
			let mut d1_memory_typed = false;
			let mut d1_memory_ok = true;
			let mut d1_memory_reg: Option<(u64, u32)> = None;
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
			let mut ram_regions = [(0u64, 0u64); MAX_RAM_REGIONS];
			let mut ram_region_count = 0usize;
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
							// A UNIT NAME OF EXACTLY `memory` OR `memory@...`, not a prefix.
							//
							// This was `str_starts(name, "memory")`, which matches
							// `memory-controller@`, `memory-window@` and anything else beginning
							// with those six letters - and if such a node has a `reg`, its MMIO
							// aperture was added to the RAM list and handed to the frame allocator.
							// The specification's rule is the unit name AND `device_type = "memory"`,
							// which this parser did not read at all; `device_type` is checked below,
							// where the property is seen.
							d1_memory = self.str_eq(name, "memory") || self.str_starts(name, "memory@");
							d1_memory_typed = false;
							d1_memory_ok = true;
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
							// THE MEMORY NODE'S BANKS, COMMITTED NOW THAT IT IS FINISHED.
							//
							// `device_type` and `status` decide whether this node is memory at all,
							// and either may appear after `reg`. A node without `device_type =
							// "memory"` is not memory whatever its name; one marked `disabled` or
							// `fail` is memory this kernel may not use.
							if d1_memory
								&& d1_memory_typed && d1_memory_ok
								&& let Some((val, len)) = d1_memory_reg
							{
								if addr_cells == 0 || size_cells == 0 {
									return None;
								}
								let mut q = val;
								let end = val + len as u64;
								while q + 4 * (addr_cells + size_cells) as u64 <= end {
									let a = self.read_cells(&mut q, addr_cells);
									let s = self.read_cells(&mut q, size_cells);
									if s == 0 || a.checked_add(s).is_none() {
										continue;
									}
									if ram_region_count < MAX_RAM_REGIONS {
										ram_regions[ram_region_count] = (a, s);
										ram_region_count += 1;
									} else {
										// AN EXPLICIT REFUSAL RATHER THAN A SILENT DROP. Everything
										// past the sixteenth bank used to disappear, so a board
										// with a fragmented map booted with less memory than it has
										// and nothing said which part was missing.
										return None;
									}
								}
							}
							d1_memory = false;
							d1_memory_typed = false;
							d1_memory_ok = true;
							d1_memory_reg = None;
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
							// AND WHAT THE READER CAN REPRESENT. `read_cells` folds cells into a
							// `u64`, so a tree declaring three or more cells describes addresses
							// wider than this reader can hold - and it read them anyway, shifting
							// the high cells out and silently keeping the low bits. An address
							// this reader cannot represent is a tree it is not for, and saying so
							// is a rule rather than a truncation nobody can see.
							if self.str_eq(pname, "#address-cells") && len == 4 {
								let cells = self.be32(val);
								if cells > MAX_CELLS {
									return None;
								}
								addr_cells = cells;
							} else if self.str_eq(pname, "#size-cells") && len == 4 {
								let cells = self.be32(val);
								if cells > MAX_CELLS {
									return None;
								}
								size_cells = cells;
							}
						} else if depth == 1 && d1_memory && self.str_eq(pname, "reg") {
							// REMEMBERED, NOT PARSED HERE. `device_type` and `status` decide whether
							// this node is memory at all and either may come after `reg`, so the
							// banks are committed at the node's end - see `FDT_END_NODE`.
							//
							// RAM IS NOT ONE RANGE, AND SUMMING THE BANKS PRETENDED IT WAS. The
							// first version took the first bank's base and added up every bank's
							// SIZE, so a board with a hole in its map - two 256 MB banks at
							// 0x4000_0000 and 0x8000_0000 - produced base 0x4000_0000 and size
							// 512 MB, and the allocator handed out frames from the middle of the
							// hole. A store into a hole is not a fault the allocator can attribute:
							// it simply goes nowhere.
							d1_memory_reg = Some((val, len));
						} else if depth == 1 && d1_memory && self.str_eq(pname, "device_type") {
							// The specification's own test for a memory node, which this parser did
							// not read at all: the unit name says where to look and this says what
							// it is. NUL-terminated in the blob.
							d1_memory_typed = len >= 7 && self.str_eq(val, "memory");
						} else if depth == 1 && d1_memory && self.str_eq(pname, "status") {
							// `disabled` and `fail` are memory this kernel may not use; `okay` and
							// an absent property mean it may. Anything else is unrecognised and is
							// treated as unusable, which is the direction that cannot hand out
							// memory the firmware still owns.
							d1_memory_ok = (len >= 5 && self.str_eq(val, "okay")) || (len >= 3 && self.str_eq(val, "ok"));
						} else if in_pcie == depth && self.str_eq(pname, "reg") {
							// The pcie node's reg is <ecam_base ecam_size> in root cells.
							// Through the bounded reader: a short `reg` leaves the previous value
							// rather than adopting the bytes after it.
							if let Some(base) = self.read_cells_property(val, len, addr_cells) {
								pcie_ecam = base;
							}
						} else if in_plic == depth && self.str_eq(pname, "reg") {
							// The plic node's reg is <plic_base plic_size> in root cells.
							// Through the bounded reader: a short `reg` leaves the previous value
							// rather than adopting the bytes after it.
							if let Some(base) = self.read_cells_property(val, len, addr_cells) {
								plic_base = base;
							}
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
							// Through the bounded reader: a short `reg` leaves the previous value
							// rather than adopting the bytes after it.
							if let Some(base) = self.read_cells_property(val, len, addr_cells) {
								fwcfg_base = base;
							}
						}
					}
					FDT_NOP => {}
					// A TREE THAT ENDS WITH NODES STILL OPEN IS NOT A TREE (FDT-008).
					//
					// This was `break` at any depth, so a token stream with more `FDT_BEGIN_NODE`s
					// than `FDT_END_NODE`s was accepted and everything read out of it - node depths,
					// and therefore which node a `reg` belonged to - was decided by a structure the
					// writer never closed. `-1` is the root closed; anything else is a stream this
					// reader was following by luck.
					FDT_END => {
						if depth != -1 {
							return None;
						}
						break;
					}
					_ => return None, // malformed
				}
			}

			// THE BANK LIST'S INVARIANTS, ESTABLISHED ONCE INSTEAD OF HOPED FOR.
			//
			// The list used to be built by coalescing a bank only with the one IMMEDIATELY before
			// it: not sorted, overlaps not merged, `ram_regions[last].0 + ram_regions[last].1` and
			// `+= s` computed unchecked. A tree listing its banks out of order therefore spent one
			// slot each on ranges that touch, and two banks that OVERLAP both entered the list -
			// after which the frame allocator's overlap refusal kept the system safe by discarding
			// legitimate memory.
			//
			// Sorted by base, then merged where they touch or overlap. An insertion sort, because
			// the list is at most sixteen entries and this crate is `no_std` and allocation-free.
			let mut i = 1usize;
			while i < ram_region_count {
				let mut j = i;
				while j > 0 && ram_regions[j - 1].0 > ram_regions[j].0 {
					ram_regions.swap(j - 1, j);
					j -= 1;
				}
				i += 1;
			}
			let mut merged = 0usize;
			let mut k = 0usize;
			while k < ram_region_count {
				let (base, size) = ram_regions[k];
				if merged > 0 {
					let (held_base, held_size) = ram_regions[merged - 1];
					// `checked_add` throughout: every one of these numbers came off the medium, and
					// the end of a bank is exactly where the old arithmetic overflowed.
					let held_end = held_base.checked_add(held_size);
					if let Some(held_end) = held_end
						&& base <= held_end
					{
						// Touching or overlapping: the union, which for an overlap is the larger of
						// the two ends rather than the sum of the sizes.
						let end = base.checked_add(size).unwrap_or(u64::MAX).max(held_end);
						ram_regions[merged - 1] = (held_base, end - held_base);
						k += 1;
						continue;
					}
				}
				ram_regions[merged] = (base, size);
				merged += 1;
				k += 1;
			}
			ram_region_count = merged;
			// `ram_base`/`ram_size` describe the FIRST run, which is what a caller reading the pair
			// has always got - and now it is the first run of a sorted, merged list rather than
			// whichever banks happened to be adjacent in the tree.
			if ram_region_count > 0 {
				ram_base = ram_regions[0].0;
				ram_size = ram_regions[0].1;
			}
			if ram_size == 0 {
				return None;
			}
			Some(BootInfo { ram_base, ram_size, ram_regions, ram_region_count, cpu_count: cpu_count.max(1), pcie_ecam, plic_base, fwcfg_base, modules_start, modules_end })
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
	// One element of a NUL-separated stringlist equals `want`, and none of them merely contains it.
	//
	// The property's own length bounds the walk; a final element without a terminator is compared
	// as it stands, because a device tree writer that omits the last NUL has still said the name.
	unsafe fn stringlist_contains(&self, value: u64, len: u32, want: &[u8]) -> bool {
		let len = len as u64;
		let mut start = 0u64;
		let mut at = 0u64;
		while at <= len {
			let byte = if at == len { 0 } else { unsafe { self.u8_at(value + at) } };
			if byte == 0 {
				if at - start == want.len() as u64 && (0..want.len() as u64).all(|i| unsafe { self.u8_at(value + start + i) } == want[i as usize]) {
					return true;
				}
				start = at + 1;
			}
			at += 1;
		}
		false
	}

	// `riscv,isa` names `want`, read the way the specification defines the string rather than as a
	// bag of bytes: `rv<width><single letters>` followed by `_`-separated multi-letter names.
	//
	// So `c` is present in `rv64imafdc` and absent from `rv64ima_zicsr`, and `zicsr` is present in
	// the second and absent from the first - which a substring scan got backwards in both
	// directions.
	unsafe fn isa_string_names(&self, value: u64, len: u32, want: &[u8]) -> bool {
		let len = len as u64;
		// The string ends at its NUL or at the property's end, whichever comes first.
		let mut end = 0u64;
		while end < len && unsafe { self.u8_at(value + end) } != 0 {
			end += 1;
		}
		let mut start = 0u64;
		let mut segment = 0usize;
		let mut at = 0u64;
		while at <= end {
			let is_break = at == end || unsafe { self.u8_at(value + at) } == b'_';
			if !is_break {
				at += 1;
				continue;
			}
			if segment == 0 {
				// THE BASE. `rv32`/`rv64` and then one letter per extension, so a one-character
				// `want` is looked for among those letters and a longer one cannot be there.
				if want.len() == 1 {
					let mut i = start;
					// Skip `rv` and the width digits; anything else is a base this reader does not
					// recognise, and it is scanned as letters rather than refused.
					if i + 2 <= at && unsafe { self.u8_at(value + i) } == b'r' && unsafe { self.u8_at(value + i + 1) } == b'v' {
						i += 2;
						while i < at && unsafe { self.u8_at(value + i) }.is_ascii_digit() {
							i += 1;
						}
					}
					while i < at {
						if unsafe { self.u8_at(value + i) } == want[0] {
							return true;
						}
						i += 1;
					}
				}
			} else if at - start == want.len() as u64 && (0..want.len() as u64).all(|i| unsafe { self.u8_at(value + start + i) } == want[i as usize]) {
				return true;
			}
			segment += 1;
			start = at + 1;
			at += 1;
		}
		false
	}

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

impl Fdt {
	// THE BLOB'S OWN EXTENT, so a caller can carve it out of the usable memory it hands to the
	// allocator.
	//
	// The specification requires a client not to overwrite the device tree while it is still in use,
	// and this kernel keeps using it after the allocator is up - riscv64 reads `timebase-frequency`
	// from it well after `frame` init. Nothing reserved the pages holding it, so they could be
	// allocated and zeroed while still live.
	pub fn extent(&self) -> Option<(u64, u64)> {
		unsafe {
			if self.be32(self.base) != FDT_MAGIC {
				return None;
			}
			let total = self.be32(self.base + 4) as u64;
			if !(64..0x20_0000).contains(&total) {
				return None;
			}
			Some((self.base, total))
		}
	}

	// Every entry of the memory reservation block, which nothing read.
	//
	// `off_mem_rsvmap` points at a list of `(address, size)` big-endian pairs ending with a zero
	// pair, and the specification says a client must not use those regions - they hold firmware
	// runtime data and whatever a board reserves. `bootmem::loader_reservations` carved out
	// `BootInfo`, the module descriptor array and the modules, and nothing else.
	//
	// Bounded by the blob's own total size and by a hard entry cap: the terminator comes off the
	// same untrusted bytes as everything else, and a list that never terminates must not spin.
	pub fn for_each_reserved_region(&self, mut visit: impl FnMut(u64, u64)) -> bool {
		unsafe {
			if self.be32(self.base) != FDT_MAGIC {
				return false;
			}
			let total = self.be32(self.base + 4) as u64;
			let off = self.be32(self.base + 16) as u64;
			if !(64..0x20_0000).contains(&total) || off >= total {
				return false;
			}
			let mut p = self.base + off;
			let end = self.base + total;
			for _ in 0..MAX_RESERVED_REGIONS {
				if p.checked_add(16).is_none_or(|next| next > end) {
					return false;
				}
				let address = ((self.be32(p) as u64) << 32) | self.be32(p + 4) as u64;
				let size = ((self.be32(p + 8) as u64) << 32) | self.be32(p + 12) as u64;
				p += 16;
				if address == 0 && size == 0 {
					return true;
				}
				// A reservation whose end overflows is the tree contradicting itself; skipped
				// rather than carved, because carving a saturated range would take the whole
				// address space out of the allocator.
				if size == 0 || address.checked_add(size).is_none() {
					continue;
				}
				visit(address, size);
			}
			// The list did not terminate within the cap: refused, because a client that carried on
			// would be using memory the firmware reserved.
			false
		}
	}

	// Every range the `/reserved-memory` subtree declares, which nothing read (FDT-001).
	//
	// A device tree says "do not use this" in two places and only one of them was being read. The
	// header's reservation block above is the older one; `/reserved-memory` is the one boards
	// actually use, for firmware runtime memory, a secure world's carve-out, DMA pools and a
	// framebuffer an earlier stage handed over. Those ranges lie INSIDE a `/memory` bank by
	// construction - that is what makes them worth declaring - so a client that reads `/memory` and
	// stops is told it owns every one of them.
	//
	// EVERY CHILD WITH A FIXED `reg`, INCLUDING `reusable` ONES. The specification says a `reusable`
	// region may be used by the OS until the owning device claims it back, and this kernel has no
	// protocol for giving a frame back to firmware on demand. So the honest reading of that flag
	// here is that the region is not ours; the alternative is handing out memory we cannot return.
	//
	// A child with `size` and no `reg` is asking the CLIENT to place the region. This reader does
	// not place anything, so there is no range to carve and none is carved - which is correct and
	// is also a gap: a client that later starts honouring those must reserve what it placed.
	//
	// Bounded like everything else here: `#address-cells` and `#size-cells` come from
	// `/reserved-memory` when it declares them and from the root otherwise, both refused past
	// `MAX_CELLS`, and the child count is capped. `false` means the walk could not be completed, and
	// the caller must treat everything past that point as unreserved rather than as absent.
	pub fn for_each_reserved_memory_node(&self, mut visit: impl FnMut(u64, u64)) -> bool {
		let Some(b) = self.bounds() else {
			return false;
		};
		unsafe {
			let mut p = b.struct_start;
			let mut depth: i32 = -1;
			let mut root_addr_cells: u32 = 2;
			let mut root_size_cells: u32 = 2;
			let mut node_addr_cells: Option<u32> = None;
			let mut node_size_cells: Option<u32> = None;
			let mut in_reserved = false;
			let mut children = 0usize;
			loop {
				let Some(token) = self.be32_in(p, b.struct_end) else {
					return false;
				};
				p += 4;
				match token {
					FDT_BEGIN_NODE => {
						depth += 1;
						let Some((name, next)) = self.node_name_in(p, &b) else {
							return false;
						};
						p = next;
						if depth == 1 {
							// EXACTLY `reserved-memory`, with no unit address: the specification
							// gives it none, and a prefix match would take `reserved-memory-pool`
							// with it.
							in_reserved = self.str_eq(name, "reserved-memory");
							node_addr_cells = None;
							node_size_cells = None;
						}
						if in_reserved && depth == 2 {
							children += 1;
							if children > MAX_RESERVED_REGIONS {
								return false;
							}
						}
					}
					FDT_END_NODE => {
						if depth == 1 {
							in_reserved = false;
						}
						depth -= 1;
						if depth < -1 {
							return false;
						}
					}
					FDT_PROP => {
						let Some((pname, len, val, next)) = self.prop_in(p, &b) else {
							return false;
						};
						p = next;
						if depth == 0 && len == 4 {
							if self.str_eq(pname, "#address-cells") {
								let cells = self.be32(val);
								if cells > MAX_CELLS {
									return false;
								}
								root_addr_cells = cells;
							} else if self.str_eq(pname, "#size-cells") {
								let cells = self.be32(val);
								if cells > MAX_CELLS {
									return false;
								}
								root_size_cells = cells;
							}
						} else if depth == 1 && in_reserved && len == 4 {
							if self.str_eq(pname, "#address-cells") {
								let cells = self.be32(val);
								if cells > MAX_CELLS {
									return false;
								}
								node_addr_cells = Some(cells);
							} else if self.str_eq(pname, "#size-cells") {
								let cells = self.be32(val);
								if cells > MAX_CELLS {
									return false;
								}
								node_size_cells = Some(cells);
							}
						} else if depth == 2 && in_reserved && self.str_eq(pname, "reg") {
							let addr_cells = node_addr_cells.unwrap_or(root_addr_cells);
							let size_cells = node_size_cells.unwrap_or(root_size_cells);
							if addr_cells == 0 || size_cells == 0 || addr_cells > MAX_CELLS || size_cells > MAX_CELLS {
								return false;
							}
							// A `reg` CARRIES PAIRS, and a child may declare several. Each pair is
							// read whole or not at all: a trailing partial pair is the tree saying
							// something this reader cannot act on, and inventing a size for it would
							// carve a range nobody wrote down.
							let stride = (addr_cells + size_cells) * 4;
							let mut offset = 0u32;
							while offset + stride <= len {
								let mut cursor = val + offset as u64;
								let base = self.read_cells(&mut cursor, addr_cells);
								let size = self.read_cells(&mut cursor, size_cells);
								offset += stride;
								// A zero-length reservation reserves nothing; one whose end wraps is
								// the tree contradicting itself, and carving a saturated range would
								// take the whole address space out of the allocator.
								if size == 0 || base.checked_add(size).is_none() {
									continue;
								}
								visit(base, size);
							}
						}
					}
					FDT_NOP => {}
					FDT_END => return depth == -1,
					_ => return false,
				}
			}
		}
	}
}

// A reservation list longer than this is a device tree this reader will not follow. Boards reserve
// a handful of ranges; a thousand is a blob that has gone wrong or is trying to.
const MAX_RESERVED_REGIONS: usize = 64;
