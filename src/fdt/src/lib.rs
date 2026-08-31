//! A flattened device tree (FDT / DTB) reader, shared by everything in this system that boots
//! through one.
//!
//! SEPARATE FROM THE KERNEL SO THE LOADER CAN HAVE IT, AND SO A HOST CAN TEST IT. This was
//! `kernel::arch::common::dtb` and the loader could not reach it, which is why the audit spent two
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
	// WHICH NUMA NODE EACH BANK BELONGS TO, parallel to `ram_regions`. `NUMA_NODE_UNKNOWN` where the
	// tree said nothing - which is not node zero: a bank with no `numa-node-id` is a bank whose
	// affinity firmware did not state, and inventing one steers allocations at a guess.
	pub ram_region_nodes: [u32; MAX_RAM_REGIONS],
	// CPUs THE TREE DECLARES USABLE, and their HARDWARE ids - not a count to iterate from zero.
	//
	// `cpu_count` was every `cpu@` node the walk saw, disabled ones included, and the `reg` that
	// says WHICH core each one is was discarded. Both backends then started ids `0..cpu_count`:
	// dense, zero-based and in tree order, none of which a device tree promises. A machine whose
	// harts are 0, 1, 4, 5 - or whose middle CPU is marked `disabled` - had cores addressed that do
	// not exist and cores that do never started.
	//
	// `cpu_count` is now the number of entries in `cpu_ids`, which are the hardware ids of the
	// nodes that are enabled AND carry a readable `reg`, in tree order. `cpu_nodes` is every
	// `cpu@` node seen, so a caller can report the difference rather than leaving it invisible.
	pub cpu_count: u32,
	pub cpu_ids: [u64; MAX_CPUS],
	// The same for processors, parallel to `cpu_ids`.
	pub cpu_node_ids: [u32; MAX_CPUS],
	// The `distance-map` node's matrix, as (from, to, distance) triples in the tree's own node
	// numbering. Empty on a machine with one node or none.
	pub numa_distances: [(u32, u32, u8); MAX_NUMA_CELLS],
	pub numa_distance_count: usize,
	// THE MATRIX WAS NOT ONE, and the reader could not say so.
	//
	// The triple loop stopped at the first incomplete triple, stopped at `MAX_NUMA_CELLS`, and
	// `continue`d past a distance above 255 - all three silently, and the comment beside the last one
	// claimed it was "refused rather than truncated". So a malformed or oversized matrix arrived as a
	// SHORTER VALID ONE, and a machine describing itself wrongly was read as a machine describing
	// itself partially. A prefix of a false table is not a table. Set here and refused above; the
	// affinity is kept, which is the same split the ACPI reader makes for a bad SLIT.
	pub numa_distance_malformed: bool,
	pub cpu_nodes: u32,
	// PCIe ECAM config-space base (0 if the tree has no pcie node).
	pub pcie_ecam: u64,
	// PLIC (RISC-V platform interrupt controller) base (0 if none / not RISC-V).
	pub plic_base: u64,
	// THE ARM INTERRUPT CONTROLLER, AS THE MACHINE DESCRIBES IT, not as one machine happened to map
	// it. The GIC's `reg` carries two ranges: the distributor first, then the CPU interface on a
	// GICv2 or the redistributor region on a GICv3. Both are needed before the first controller
	// access, which is why they are read here rather than compiled in - a fixed address is a claim
	// about one QEMU machine and says nothing about the tree in front of it.
	//
	// Zero means the tree named no controller this reader knows. A caller that finds zero here has
	// no GIC and cannot claim a timer interrupt, which is a refusal rather than a default.
	pub gic_dist: u64,
	pub gic_dist_size: u64,
	// THE TIMER'S OWN INTERRUPT, READ FROM THE TREE RATHER THAN COMPILED IN.
	//
	// The backend held `const TIMER_INTID: u32 = 30` with a comment naming QEMU `virt`, and nothing
	// decoded the ARM timer node at all - so a tree naming another PPI, or omitting the interrupt
	// this kernel uses, was ACCEPTED and the kernel enabled 30 regardless. `/timer` carries four
	// triples (secure, non-secure EL1 physical, virtual, hypervisor); this reader takes the SECOND,
	// which is the EL1 physical timer the kernel programs.
	//
	// Zero means the tree named no timer interrupt this reader could decode, which is a refusal for
	// the caller to make rather than a number to default.
	pub timer_intid: u32,
	pub _pad_timer: u32,
	pub gic_cpu: u64,
	pub gic_cpu_size: u64,
	// 2 for a GICv2 (`arm,cortex-a15-gic` / `arm,gic-400`), 3 for a GICv3, 0 for neither. The
	// version decides which core profile drives it, and it comes from `compatible` rather than from
	// the shape of `reg`, because two versions can describe two ranges.
	pub gic_version: u8,
	// The GICv2m frame, if the tree declares one as a child of the controller: a device writes an
	// SPI number into it to signal an MSI. Zero when the tree names none, which is a machine
	// without message-signalled interrupts rather than an error.
	pub gic_msi: u64,
	pub gic_msi_size: u64,
	// THE GICv3 ITS, if the tree declares one as a child of the controller. It is the OTHER kind of
	// MSI controller ARM defines and nothing like a v2m frame: a device writes an EVENT id to one
	// translation register, and the ITS turns the pair (device, event) into an LPI aimed at a core -
	// through tables in memory this kernel owns. Zero when the machine has none.
	pub gic_its: u64,
	pub gic_its_size: u64,
	// HOW A PCI DEVICE'S REQUESTER ID BECOMES AN ITS DEVICE ID, from the host bridge's `msi-map`:
	// a device whose RID is in `[rid_base, rid_base + length)` has ITS DeviceID `devid_base + RID -
	// rid_base`. A machine that states no mapping leaves `length` zero, and this kernel then has no
	// way to name its devices to the ITS - which is a refusal rather than an assumed identity.
	pub pci_msi_rid_base: u32,
	pub pci_msi_devid_base: u32,
	pub pci_msi_length: u32,
	// THE RISC-V S-MODE IMSIC, as the machine describes it. With `aia=aplic-imsic` a device's MSI
	// write lands in a per-hart 4 KiB interrupt file, and the base of that array was a constant
	// naming one QEMU machine even though the tree describes the controller.
	//
	// THE S-MODE ONE, NOT THE FIRST ONE FOUND. An AIA machine describes two IMSICs - M-mode and
	// S-mode - and they are told apart by which CPU interrupt each one is wired to:
	// `interrupts-extended` names IRQ 9 (supervisor external) for the file this kernel owns and 11
	// (machine external) for the one the firmware owns. Taking the first node would take the
	// firmware's controller on every machine that lists it first, which QEMU does.
	pub imsic_base: u64,
	pub imsic_size: u64,
	// HOW THE FILES ARE ADDRESSED, which decides whether this kernel can address them at all. The
	// AIA binding lets a machine index its files by guest and by group as well as by hart, and the
	// address of hart H's file is then `base | group << shift | H << (12 + guest_bits)` rather than
	// `base + H * 4096`. Zero for both - which is what QEMU's `virt` emits - is the flat layout this
	// kernel computes; anything else is a machine it must refuse rather than compute a wrong
	// address for. Absent properties default to zero, which the binding also says.
	pub imsic_guest_index_bits: u32,
	pub imsic_group_index_bits: u32,
	// How many interrupt identities the controller carries (`riscv,num-ids`), 0 when it says
	// nothing. A window of EIDs larger than this arms identities the hardware will not deliver.
	pub imsic_num_ids: u32,
	// THE HART EACH INTERRUPT FILE BELONGS TO, IN FILE ORDER, and `u64::MAX` for a file whose
	// `interrupts-extended` entry names no cpu node in this tree.
	//
	// A file's address comes from its INDEX in this list, not from the hart id - the two coincide
	// only when the machine's harts are dense and listed in order, which is what QEMU emits and
	// what a kernel computing `base + hart * stride` silently assumes. Recording the association is
	// what lets that assumption be checked instead of relied on.
	pub imsic_harts: [u64; MAX_CPUS],
	pub imsic_hart_count: u32,
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

// "The tree did not say." Not a node id, and deliberately not zero - see `ram_region_nodes`.
pub const NUMA_NODE_UNKNOWN: u32 = u32::MAX;

// The distance matrix is one triple per ordered pair, so eight nodes is sixty-four of them. Bounded
// here because the count comes from the tree.
pub const MAX_NUMA_CELLS: usize = 64;

// How many CPUs the logical-to-hardware id table holds (KERN-ARCH-008).
//
// Eight, because that is the aarch64 per-CPU pool - the smallest backend limit in the tree, and the
// one a larger table would only postpone hitting. A machine with more cores than this boots on the
// first eight and SAYS so; refusing the tree the way an over-long `/memory` does would turn a
// sixteen-core machine into no machine at all, which is the worse of the two answers here.
pub const MAX_CPUS: usize = 8;

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

	// The `index`-th (address, size) pair of a `reg` property, with every check the single-range
	// reader has plus the ones a list needs.
	//
	// A GIC's `reg` is two ranges - distributor then CPU interface or redistributor - and a reader
	// that takes only the first cannot describe the controller. Each pair is `addr_cells +
	// size_cells` cells wide, so the property must be at least that many cells long for the pair
	// being asked for; a short one is a tree that does not say what the caller thinks it says, and
	// reading past it would take the padding after the property or the next token as an address.
	//
	// A ZERO SIZE IS REFUSED, because a range of no bytes is not a region that can be mapped, and a
	// caller that accepted one would compute an end equal to its start and check nothing.
	unsafe fn read_reg_range(&self, value: u64, len: u32, addr_cells: u32, size_cells: u32, index: u32) -> Option<(u64, u64)> {
		if addr_cells == 0 || addr_cells > MAX_CELLS || size_cells == 0 || size_cells > MAX_CELLS {
			return None;
		}
		let pair_cells = addr_cells.checked_add(size_cells)?;
		let pair_bytes = pair_cells.checked_mul(4)?;
		let skip = pair_bytes.checked_mul(index)?;
		let want = skip.checked_add(pair_bytes)?;
		if len < want {
			return None;
		}
		let mut p = value.checked_add(skip as u64)?;
		let address = unsafe { self.read_cells(&mut p, addr_cells) };
		let size = unsafe { self.read_cells(&mut p, size_cells) };
		if size == 0 || address.checked_add(size).is_none() {
			return None;
		}
		Some((address, size))
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
			// (depth 2) on riscv64 virt, so each is tracked by the depth at which we entered.
			let mut pcie = Device::new();
			let mut plic = Device::new();
			// The ARM interrupt controller and, inside it, the GICv2m frame. The frame is a CHILD of
			// the controller node on QEMU's virt machine, so it is tracked separately and only
			// accepted while the controller node is open.
			let mut gic = Device::new();
			let mut v2m = Device::new();
			let mut its = Device::new();
			let mut gic_its: u64 = 0;
			let mut gic_its_size: u64 = 0;
			let mut pci_msi_rid_base: u32 = 0;
			let mut pci_msi_devid_base: u32 = 0;
			let mut pci_msi_length: u32 = 0;
			// The RISC-V IMSIC, plus the one property that says whether it is the supervisor's.
			let mut imsic = Device::new();
			let mut imsic_supervisor = false;
			let mut fwcfg = Device::new();
			// What each depth declares for its children, and its `ranges` - so a device's `reg` is
			// read in its PARENT's cells and translated up the buses it sits behind (FDT-003). The
			// root's own defaults are the specification's.
			let mut cells_at = [(2u32, 2u32); MAX_DEPTH + 1];
			let mut buses = [Bus::root(); MAX_DEPTH + 1];
			let mut d1_chosen = false;
			let mut addr_cells: u32 = 2;
			let mut size_cells: u32 = 2;
			let mut ram_base: u64 = 0;
			let mut ram_size: u64 = 0;
			let mut ram_regions = [(0u64, 0u64); MAX_RAM_REGIONS];
			let mut ram_region_count = 0usize;
			let mut ram_region_nodes = [NUMA_NODE_UNKNOWN; MAX_RAM_REGIONS];
			// The `numa-node-id` of the memory node being walked, and of the cpu node being walked.
			// Read where they appear and committed at the node's end, like `reg` is - the property
			// may come before or after the ones that decide whether the node counts at all.
			let mut d1_memory_node = NUMA_NODE_UNKNOWN;
			let mut cpu_node_id = NUMA_NODE_UNKNOWN;
			let mut cpu_node_ids = [NUMA_NODE_UNKNOWN; MAX_CPUS];
			// The distance map, which is a node of its own rather than a property of the others.
			let mut numa_distances = [(0u32, 0u32, 0u8); MAX_NUMA_CELLS];
			let mut numa_distance_count = 0usize;
			let mut numa_distance_malformed = false;
			let mut in_timer = false;
			// THE SELECTED MAIN GIC'S OWN PHANDLE, AND WHAT THE TIMER SAYS IT IS ROUTED TO.
			//
			// The timer PPI's kind, number and sense were all checked and its ROUTING was not, so a
			// timer whose `interrupt-parent` names a different controller was accepted and its INTID
			// enabled on the selected GIC - a PPI programmed on a controller the tree does not say it
			// belongs to. M2 asks for the routing to be checked; these two numbers are what the check
			// compares.
			//
			// Zero means "not stated", and for the timer that is the ordinary case: `interrupt-parent`
			// is INHERITED, and on both machines this reader boots the root node carries it and the
			// timer does not repeat it. So an unstated parent is accepted and a STATED one that
			// disagrees is refused - which is the only form of the check that does not require this
			// reader to implement inheritance for one property.
			let mut gic_phandle: u32 = 0;
			let mut timer_parent: u32 = 0;
			// THE ROOT'S `interrupt-parent`, WHICH IS THE ONE THE TIMER INHERITS.
			//
			// `interrupt-parent` is an inherited property, and the previous version treated an
			// unstated one as "not checkable" and skipped the comparison - so a timer inheriting a
			// DIFFERENT controller from its ancestor was accepted and its INTID enabled on the
			// selected GIC, which is the case the check exists for. It was justified by not wanting
			// to implement inheritance for one property, and for THIS node that argument does not
			// hold: the ARM generic timer is a root CHILD on every machine this reader boots, so its
			// only ancestor is the root and the whole of the inheritance is this one value.
			//
			// A tree that nests the timer deeper is one this reader does not claim to read: the
			// resolution below applies only at depth 1, and anything else keeps the old
			// unstated-is-accepted behaviour rather than resolving an ancestor it did not track.
			let mut root_interrupt_parent: u32 = 0;
			let mut timer_intid: u32 = 0;
			let mut timer_compatible = false;
			// The versioned binding, which this reader never checked: entering distance-map mode on
			// the node NAME alone accepts any future or foreign map as if it were this one.
			let mut distance_map_versioned = false;
			let mut in_distance_map = false;
			let mut cpu_count: u32 = 0;
			let mut cpu_ids = [0u64; MAX_CPUS];
			let mut cpu_nodes: u32 = 0;
			// `/cpus` carries its own `#address-cells` (one on both QEMU machines, two where an
			// MPIDR needs it); the root's cell counts do not describe a hart id.
			let mut cpus_addr_cells: u32 = 1;
			let mut in_cpu = false;
			// Decided at the node's END, because a tree does not promise property order: `status`
			// may follow `reg`, and both decide whether this hart is a bring-up target.
			let mut cpu_ok = true;
			let mut cpu_reg: Option<(u64, u32)> = None;
			let mut pcie_ecam: u64 = 0;
			let mut plic_base: u64 = 0;
			let mut gic_dist: u64 = 0;
			let mut gic_dist_size: u64 = 0;
			let mut gic_cpu: u64 = 0;
			let mut gic_cpu_size: u64 = 0;
			let mut gic_version: u8 = 0;
			// WHICH GIC CANDIDATE A CHILD BELONGED TO, and which one won.
			//
			// The v2m frame and the ITS are children of a GIC node and are committed when THEIR node
			// closes; the main controller is chosen when ITS node closes, which is later. Only the
			// first usable GIC wins, but a child under a LATER candidate - an unusable one, a
			// duplicate - still found the global MSI field empty and filled it, and the machine was
			// then handed a frame belonging to a controller it is not running. Counting the
			// candidates makes the question answerable: a child records the number of the node it
			// was under, and anything not matching the winner is dropped after the walk.
			let mut gic_seen: u32 = 0;
			let mut selected_gic: u32 = 0;
			let mut msi_owner: u32 = 0;
			let mut its_owner: u32 = 0;
			let mut gic_msi: u64 = 0;
			let mut gic_msi_size: u64 = 0;
			let mut imsic_base: u64 = 0;
			let mut imsic_size: u64 = 0;
			// Read on whichever IMSIC node is being walked, committed only for the supervisor one.
			let mut imsic_guest_bits_seen: u32 = 0;
			let mut imsic_group_bits_seen: u32 = 0;
			let mut imsic_num_ids_seen: u32 = 0;
			let mut imsic_ext_seen: Option<(u64, u32)> = None;
			let mut imsic_guest_index_bits: u32 = 0;
			let mut imsic_group_index_bits: u32 = 0;
			let mut imsic_num_ids: u32 = 0;
			// The supervisor IMSIC's `interrupts-extended`, resolved after the walk: the cpu nodes
			// it names may be walked before or after it, so the table it is resolved against is only
			// complete at the end.
			let mut imsic_ext: Option<(u64, u32)> = None;
			// Each recorded cpu's interrupt-controller phandle, parallel to `cpu_ids`. Zero for a
			// cpu node with no such child, which is a cpu nothing can be wired to.
			let mut cpu_phandles = [0u32; MAX_CPUS];
			let mut cpu_phandle: u32 = 0;
			let mut in_cpu_intc = false;
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
						if depth as usize > MAX_DEPTH {
							return None;
						}
						if depth == 0 {
							buses[0] = Bus::root();
						} else {
							cells_at[depth as usize] = cells_at[depth as usize - 1];
							// A node that declares no `ranges` does not make its children reachable
							// from its parent; inheriting the parent's would invent a mapping.
							buses[depth as usize] = Bus { cells: cells_at[depth as usize], ranges: None };
						}
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
							// The NUMA distance map, which is a node of the root rather than a
							// property of anything. QEMU emits it as `/distance-map` when the
							// machine was given distances.
							in_distance_map = self.str_eq(name, "distance-map");
							// The ARM generic timer, a root child on every machine that has one.
							in_timer = self.str_eq(name, "timer");
							if in_distance_map {
								distance_map_versioned = false;
							}
							d1_cpus = self.str_eq(name, "cpus");
							d1_chosen = self.str_eq(name, "chosen");
						} else if depth == 2 && d1_cpus && self.str_starts(name, "cpu@") {
							in_cpu = true;
							cpu_nodes += 1;
							cpu_ok = true;
							cpu_reg = None;
						}
						if pcie.depth < 0 && (self.str_starts(name, "pcie") || self.str_starts(name, "pci@")) {
							pcie.enter(depth);
						}
						if plic.depth < 0 && self.str_starts(name, "plic") {
							plic.enter(depth);
						}
						// `intc` is what QEMU's virt machine calls it; `interrupt-controller` is the
						// generic name a hand-written tree uses. Either way `compatible` decides
						// whether this reader knows the controller, and the name only decides where
						// to look.
						if gic.depth < 0 && (self.str_starts(name, "intc") || self.str_starts(name, "interrupt-controller")) {
							gic.enter(depth);
							// EACH CANDIDATE GETS A NUMBER, so a child can say which one it was
							// under - see `msi_owner`.
							gic_seen += 1;
						}
						if gic.depth >= 0 && v2m.depth < 0 && self.str_starts(name, "v2m") {
							v2m.enter(depth);
						}
						// The ITS, also a child of the controller. QEMU names the node
						// `its@...` and Linux's own trees name it `msi-controller@...`, so the
						// `compatible` below is what decides - the name only narrows the search.
						if gic.depth >= 0 && its.depth < 0 && (self.str_starts(name, "its") || self.str_starts(name, "msi-controller")) {
							its.enter(depth);
						}
						// The same node names as the GIC above - an AIA machine calls its controllers
						// `interrupt-controller@...` too - and `compatible` is again what decides
						// which reader this node belongs to.
						if imsic.depth < 0 && (self.str_starts(name, "imsic") || self.str_starts(name, "interrupt-controller")) {
							imsic.enter(depth);
							imsic_supervisor = false;
							imsic_guest_bits_seen = 0;
							imsic_group_bits_seen = 0;
							imsic_num_ids_seen = 0;
							imsic_ext_seen = None;
						}
						// A cpu's own interrupt controller, which is what an IMSIC's
						// `interrupts-extended` names. Its phandle is how a file is tied to a hart.
						if in_cpu && depth == 3 && self.str_starts(name, "interrupt-controller") {
							in_cpu_intc = true;
						}
						if fwcfg.depth < 0 && self.str_starts(name, "fw-cfg") {
							fwcfg.enter(depth);
						}
					}
					FDT_END_NODE => {
						if depth == 3 && in_cpu_intc {
							in_cpu_intc = false;
						}
						if depth == 2 && in_cpu {
							// THIS HART'S VERDICT, NOW THAT THE WHOLE NODE HAS BEEN SEEN. A node
							// without a readable `reg` names no core - the specification requires
							// one, and inventing an id from the enumeration order is the dense
							// assumption this finding is about - so it is counted and not recorded.
							if cpu_ok
								&& let Some((val, len)) = cpu_reg
								&& let Some(id) = self.read_cells_property(val, len, cpus_addr_cells)
							{
								if (cpu_count as usize) < MAX_CPUS {
									cpu_ids[cpu_count as usize] = id;
									cpu_node_ids[cpu_count as usize] = cpu_node_id;
									cpu_phandles[cpu_count as usize] = cpu_phandle;
									cpu_count += 1;
								}
							}
							in_cpu = false;
							cpu_ok = true;
							cpu_reg = None;
							cpu_phandle = 0;
							cpu_node_id = NUMA_NODE_UNKNOWN;
						}
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
										ram_region_nodes[ram_region_count] = d1_memory_node;
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
							d1_memory_node = NUMA_NODE_UNKNOWN;
							in_distance_map = false;
							d1_cpus = false;
							d1_chosen = false;
						}
						// EACH PLATFORM DEVICE'S VERDICT, NOW THAT ITS NODE IS FINISHED. `reg`,
						// `compatible` and `status` may appear in any order, and all three decide
						// whether this address may be used. The FIRST usable node of a kind wins,
						// so an unrelated later one cannot overwrite it (FDT-003).
						if depth == pcie.depth {
							if pcie_ecam == 0
								&& pcie.usable() && let Some((val, len)) = pcie.reg
								&& let Some(child) = self.read_cells_property(val, len, cells_at[depth as usize - 1].0)
								&& let Some(physical) = self.translate_to_root(&buses, depth as usize - 1, child)
							{
								pcie_ecam = physical;
							}
							pcie.leave();
						}
						// THE CONTROLLER'S TWO RANGES, COMMITTED WHEN ITS NODE IS FINISHED, for the
						// reason every other device here is: `reg`, `compatible` and `status` may
						// come in any order and all three decide whether these addresses may be
						// used. Both ranges must read and both must translate to the root bus; a
						// controller whose second range is missing is a controller this reader
						// cannot drive, and taking the first alone would leave the CPU interface at
						// whatever a compiled-in constant said.
						if depth == v2m.depth {
							if gic_msi == 0
								&& v2m.usable() && let Some((val, len)) = v2m.reg
								&& let (parent_addr, parent_size) = cells_at[depth as usize - 1]
								&& let Some((child, size)) = self.read_reg_range(val, len, parent_addr, parent_size, 0)
								&& let Some(physical) = self.translate_to_root(&buses, depth as usize - 1, child)
							{
								// BIG ENOUGH FOR THE REGISTERS ITS DRIVER WRITES. The frame was
								// committed with no minimum at all, so a one-byte window reached
								// MMIO - and the v2m backend writes `MSI_TYPER` at 0x008 and
								// `MSI_SETSPI_NS` at 0x040, which are stores outside the range the
								// machine declared.
								//
								// WHETHER IT ALIASES THE CONTROLLER IS ASKED LATER, not here: this
								// node is the GIC's CHILD, so it ends BEFORE its parent and the core
								// ranges are still zero at this point. Comparing them here compares
								// against nothing, which is a check that passes for the wrong reason.
								if size >= MIN_GIC_V2M_SIZE {
									gic_msi = physical;
									gic_msi_size = size;
									// WHICH GIC THIS CHILD WAS UNDER - see `msi_owner`.
									msi_owner = gic_seen;
								}
							}
							v2m.leave();
						}
						if depth == its.depth {
							if gic_its == 0
								&& its.usable() && let Some((val, len)) = its.reg
								&& let (parent_addr, parent_size) = cells_at[depth as usize - 1]
								&& let Some((child, size)) = self.read_reg_range(val, len, parent_addr, parent_size, 0)
								&& let Some(physical) = self.translate_to_root(&buses, depth as usize - 1, child)
								// BIG ENOUGH FOR THE REGISTERS ITS DRIVER AND ITS DEVICES WRITE, which
								// the v2m frame beside it has always been checked for and this was
								// not. See `MIN_GIC_ITS_SIZE`.
								&& size >= MIN_GIC_ITS_SIZE
							{
								gic_its = physical;
								gic_its_size = size;
								// WHICH GIC THIS CHILD WAS UNDER - see `msi_owner`.
								its_owner = gic_seen;
							}
							its.leave();
						}
						if depth == gic.depth {
							if gic_dist == 0
								&& gic.usable() && let Some((val, len)) = gic.reg
								&& let (parent_addr, parent_size) = cells_at[depth as usize - 1]
								&& let Some((dist, dist_size)) = self.read_reg_range(val, len, parent_addr, parent_size, 0)
								&& let Some((cpu, cpu_size)) = self.read_reg_range(val, len, parent_addr, parent_size, 1)
								&& let Some(dist_phys) = self.translate_to_root(&buses, depth as usize - 1, dist)
								&& let Some(cpu_phys) = self.translate_to_root(&buses, depth as usize - 1, cpu)
								// TWO REGIONS THAT DO NOT OVERLAP. A tree that describes the same
								// bytes twice describes a controller nobody can drive: writing the
								// distributor would write the CPU interface. Checked here, where
								// both are known, rather than at the first MMIO write.
								&& !ranges_overlap(dist_phys, dist_size, cpu_phys, cpu_size)
								// AND BIG ENOUGH TO HOLD THE REGISTERS THE DRIVER WRITES.
								//
								// Non-zero was the whole size check, so a ONE-BYTE distributor was
								// accepted - and the driver then writes `GICD_IROUTER` at offset
								// 0x6000 and the GICv2 CPU interface at 0x10, far outside the window
								// the machine declared. A range that cannot hold the registers is a
								// machine description that is wrong, and the first MMIO write is a
								// bad place to find that out.
								&& dist_size >= MIN_GIC_DIST_SIZE
								// AND THE SECOND RANGE IS MEASURED AGAINST WHAT IT IS.
								//
								// One 0x1000 was applied to both a GICv2 CPU interface and a GICv3
								// REDISTRIBUTOR range, and those are not the same object: a
								// redistributor frame is 0x20000, and `this_redistributor` cannot
								// inspect even one unless a whole stride fits. An undersized v3
								// range was accepted as a main controller and the backend then
								// logged that this core had no redistributor and carried on with no
								// interrupts at all. The version is known here - it is taken from
								// the same node's `compatible`, before this point.
								&& cpu_size >= if gic_version == 3 { MIN_GICR_STRIDE } else { MIN_GIC_CPU_SIZE }
							{
								gic_dist = dist_phys;
								gic_dist_size = dist_size;
								gic_cpu = cpu_phys;
								gic_cpu_size = cpu_size;
								// AND WHICH CANDIDATE WON, so a child committed under another one
								// cannot be handed to it - see `msi_owner`.
								selected_gic = gic_seen;
							}
							gic.leave();
						}
						if depth == imsic.depth {
							if imsic_base == 0
								&& imsic_supervisor && imsic.usable()
								&& let Some((val, len)) = imsic.reg
								&& let (parent_addr, parent_size) = cells_at[depth as usize - 1]
								&& let Some((base, size)) = self.read_reg_range(val, len, parent_addr, parent_size, 0)
								&& let Some(physical) = self.translate_to_root(&buses, depth as usize - 1, base)
							{
								imsic_base = physical;
								imsic_size = size;
								imsic_guest_index_bits = imsic_guest_bits_seen;
								imsic_group_index_bits = imsic_group_bits_seen;
								imsic_num_ids = imsic_num_ids_seen;
								imsic_ext = imsic_ext_seen;
							}
							imsic.leave();
						}
						if depth == plic.depth {
							if plic_base == 0
								&& plic.usable() && let Some((val, len)) = plic.reg
								&& let Some(child) = self.read_cells_property(val, len, cells_at[depth as usize - 1].0)
								&& let Some(physical) = self.translate_to_root(&buses, depth as usize - 1, child)
							{
								plic_base = physical;
							}
							plic.leave();
						}
						if depth == fwcfg.depth {
							if fwcfg_base == 0
								&& fwcfg.usable() && let Some((val, len)) = fwcfg.reg
								&& let Some(child) = self.read_cells_property(val, len, cells_at[depth as usize - 1].0)
								&& let Some(physical) = self.translate_to_root(&buses, depth as usize - 1, child)
							{
								fwcfg_base = physical;
							}
							fwcfg.leave();
						}
						depth -= 1;
					}
					FDT_PROP => {
						let (pname, len, val, next) = self.prop_in(p, &b)?;
						p = next;
						// What this node declares for its CHILDREN, at every depth - recorded as
						// declared and bounded where it is used, so a PCIe node's three address
						// cells do not refuse a tree the rest of which is readable (FDT-006).
						if depth >= 0 && (depth as usize) <= MAX_DEPTH {
							if len == 4 && self.str_eq(pname, "#address-cells") {
								cells_at[depth as usize].0 = self.be32(val);
								buses[depth as usize].cells.0 = cells_at[depth as usize].0;
							} else if len == 4 && self.str_eq(pname, "#size-cells") {
								cells_at[depth as usize].1 = self.be32(val);
								buses[depth as usize].cells.1 = cells_at[depth as usize].1;
							} else if self.str_eq(pname, "ranges") {
								buses[depth as usize].ranges = Some((val, len));
							}
						}
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
							// THE ROOT'S `interrupt-parent`, WHICH IS WHAT THE TIMER INHERITS. Read here
							// because this is the block a depth-0 property reaches: the chain below
							// is this block's `else`, so a `depth == 0` arm added to it can never be
							// taken - which is how the first attempt at this check compiled, ran and
							// changed nothing.
							if self.str_eq(pname, "interrupt-parent") && len == 4 {
								root_interrupt_parent = self.be32(val);
							} else if self.str_eq(pname, "#address-cells") && len == 4 {
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
						} else if depth == 1 && d1_cpus && self.str_eq(pname, "#address-cells") && len == 4 {
							let cells = self.be32(val);
							if cells > MAX_CELLS {
								return None;
							}
							cpus_addr_cells = cells;
						} else if depth == 2 && in_cpu && self.str_eq(pname, "reg") {
							cpu_reg = Some((val, len));
						} else if depth == 2 && in_cpu && self.str_eq(pname, "status") {
							// Same rule as `/memory`: `okay` or absent means usable, `disabled` and
							// `fail` mean not, and anything unrecognised is treated as not - the
							// direction that cannot send a `CPU_ON` to a core the firmware owns.
							cpu_ok = (len >= 5 && self.str_eq(val, "okay")) || (len >= 3 && self.str_eq(val, "ok"));
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
						} else if depth == 1 && d1_memory && len == 4 && self.str_eq(pname, "numa-node-id") {
							d1_memory_node = self.be32(val);
						} else if depth == 2 && in_cpu && len == 4 && self.str_eq(pname, "numa-node-id") {
							cpu_node_id = self.be32(val);
						} else if in_timer && self.str_eq(pname, "compatible") {
							// `arm,armv8-timer` (or the v7 name) is the node this reader decodes.
							timer_compatible = self.stringlist_contains(val, len, b"arm,armv8-timer") || self.stringlist_contains(val, len, b"arm,armv7-timer");
						} else if in_timer && self.str_eq(pname, "interrupt-parent") && len == 4 {
							timer_parent = self.be32(val);
						} else if in_timer && self.str_eq(pname, "interrupts") && len >= 24 {
							// FOUR TRIPLES: secure EL1, NON-SECURE EL1, virtual, hypervisor. Each is
							// (kind, number, flags) with kind 1 = PPI, and a PPI's INTID is its number
							// plus 16. The kernel programs the non-secure EL1 PHYSICAL timer, which is
							// the second triple - taking the first would arm the secure one, which is
							// not this kernel's to arm.
							let kind = self.be32(val + 12);
							let number = self.be32(val + 16);
							// A PPI, AND A PPI NUMBER. `kind == 1` says the cell is tagged a PPI; it
							// does not say the NUMBER is one. The architecture gives PPIs sixteen of
							// them, 0..15, which occupy INTIDs 16..31 - and nothing checked that, so
							// a tree naming PPI 20 was published as INTID 36. GICv3 then shifts a
							// 32-bit enable word by that, and GICv2 reads it as a distributor SPI;
							// neither is the timer, and both are a machine whose scheduler tick goes
							// somewhere else.
							//
							// A specifier outside the range is one this reader cannot decode, and
							// leaving `timer_intid` unset is what lets the caller refuse - see the
							// aarch64 backend, where a boot with no timer is now fatal rather than
							// reported.
							// AND THE THIRD CELL IS READ, which it was not.
							//
							// The specifier is (kind, number, flags) and the flags say what SENSE the
							// interrupt has: exactly one of edge-rising, edge-falling, level-high,
							// level-low. Zero says nothing and two of them say two contradictory
							// things, and neither is an interrupt this reader can program - but the
							// cell was skipped entirely, so a tree carrying either published its
							// number as the timer's INTID and the machine armed a timer whose own
							// description did not agree on how it fires.
							let sense = self.be32(val + 20) & IRQ_SENSE_MASK;
							if kind == PPI_KIND && number < PPI_COUNT && sense.count_ones() == 1 {
								timer_intid = number + PPI_INTID_BASE;
							}
						} else if in_distance_map && self.str_eq(pname, "compatible") {
							// THE VERSIONED BINDING. `numa-distance-map-v1` is what this reader
							// implements; a node named `distance-map` carrying something else is a
							// map in a format nobody here has read, and entering distance-map mode on
							// the NAME alone accepted it as if it were this one.
							distance_map_versioned = self.str_eq(val, "numa-distance-map-v1");
						} else if in_distance_map && self.str_eq(pname, "distance-matrix") {
							// TRIPLES OF CELLS: from, to, distance. The distance is a u32 in the
							// tree and a byte here, because a distance above 255 is not a distance
							// any of this reasons about - it is refused rather than truncated.
							let mut q = val;
							let end = val + len as u64;
							// A LENGTH THAT IS NOT A WHOLE NUMBER OF TRIPLES is a malformed matrix,
							// not a matrix with something after it.
							if len % 12 != 0 {
								numa_distance_malformed = true;
							}
							while q + 12 <= end {
								let from = self.be32(q);
								let to = self.be32(q + 4);
								let distance = self.be32(q + 8);
								q += 12;
								if distance > u8::MAX as u32 {
									// Refused, which is what the comment always said and what the
									// `continue` never did.
									numa_distance_malformed = true;
									break;
								}
								if numa_distance_count >= MAX_NUMA_CELLS {
									// More cells than this kernel bounds at. Truncating leaves a
									// square that is not the machine's.
									numa_distance_malformed = true;
									break;
								}
								numa_distances[numa_distance_count] = (from, to, distance as u8);
								numa_distance_count += 1;
							}
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
						} else if pcie.depth == depth && self.record_device(&mut pcie, pname, val, len, &["pci-host-ecam-generic"]) {
						} else if plic.depth == depth && self.record_device(&mut plic, pname, val, len, &["sifive,plic-1.0.0", "riscv,plic0"]) {
						} else if imsic.depth == depth && self.str_eq(pname, "interrupts-extended") {
							// WHICH PRIVILEGE LEVEL'S FILE THIS IS. Each entry is a phandle and an
							// interrupt number; 9 is the supervisor external interrupt and 11 the
							// machine one. One entry is enough to tell the two controllers apart,
							// and a property too short to hold one says nothing - which leaves this
							// node unclaimed rather than guessed at.
							if len >= 8 {
								imsic_supervisor = self.be32(val + 4) == 9;
							}
							// AND WHICH HART EACH FILE IS, kept raw: every pair names one hart's
							// interrupt controller, in the order the files are laid out.
							imsic_ext_seen = Some((val, len));
						} else if imsic.depth == depth && len == 4 && self.str_eq(pname, "riscv,guest-index-bits") {
							imsic_guest_bits_seen = self.be32(val);
						} else if imsic.depth == depth && len == 4 && self.str_eq(pname, "riscv,group-index-bits") {
							imsic_group_bits_seen = self.be32(val);
						} else if imsic.depth == depth && len == 4 && self.str_eq(pname, "riscv,num-ids") {
							imsic_num_ids_seen = self.be32(val);
						} else if in_cpu_intc && depth == 3 && len == 4 && self.str_eq(pname, "phandle") {
							cpu_phandle = self.be32(val);
						} else if its.depth == depth && self.record_device(&mut its, pname, val, len, &["arm,gic-v3-its"]) {
						} else if pcie.depth == depth && len >= 16 && self.str_eq(pname, "msi-map") {
							// One entry: rid-base, the controller's phandle, its devid-base, length.
							// A tree may hold several; this reader takes the FIRST, and a machine
							// that splits its bus across two MSI controllers is one it does not
							// claim - the second entry is visible in the length it does not cover.
							pci_msi_rid_base = self.be32(val);
							pci_msi_devid_base = self.be32(val + 8);
							pci_msi_length = self.be32(val + 12);
						} else if v2m.depth == depth && self.record_device(&mut v2m, pname, val, len, &["arm,gic-v2m-frame"]) {
						} else if imsic.depth == depth && self.record_device(&mut imsic, pname, val, len, &["riscv,imsics"]) {
						} else if gic.depth == depth && gic_dist == 0 && self.str_eq(pname, "phandle") && len == 4 {
							// THE SELECTED GIC'S OWN PHANDLE, taken from the same node its addresses
							// are, so the timer's routing is compared against the controller this
							// reader actually selected rather than against whichever GIC node came
							// last. Its own branch because `record_device` handles `reg`, `status`
							// and `compatible` and refuses everything else - a `phandle` handed to it
							// falls through the chain and is never seen.
							gic_phandle = self.be32(val);
						} else if gic.depth == depth && self.record_device(&mut gic, pname, val, len, &["arm,cortex-a15-gic", "arm,gic-400", "arm,arm11mp-gic", "arm,gic-v3"]) {
							// The version, from the same `compatible` the reader just matched. A
							// GICv3 describes a distributor and a REDISTRIBUTOR region where a
							// GICv2 describes a distributor and a CPU interface, and the two are
							// driven differently - so which one this is has to come from the
							// machine rather than from the shape of `reg`.
							//
							// AND ONLY FOR THE NODE WHOSE ADDRESSES WERE TAKEN. The addresses are
							// committed once, while `gic_dist == 0`, and this was rewritten for EVERY
							// recognised GIC node afterwards - so a usable GICv2 followed by a
							// recognised GICv3 produced the FIRST node's GICv2 cpu-interface address
							// with version 3, which the kernel then drives as a redistributor region.
							// A version that describes a different node than the addresses do is worse
							// than no version at all.
							if gic_dist == 0 && self.str_eq(pname, "compatible") {
								if self.stringlist_contains(val, len, b"arm,gic-v3") {
									gic_version = 3;
								} else if gic.known {
									gic_version = 2;
								}
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
						} else if fwcfg.depth == depth && self.record_device(&mut fwcfg, pname, val, len, &["qemu,fw-cfg-mmio"]) {
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
					// THE NODE TRAVELS WITH ITS BANK. The two arrays are parallel, and sorting one
					// without the other would attach every bank's affinity to whichever bank landed
					// in its slot - a NUMA topology that is exactly as wrong as it is plausible.
					ram_region_nodes.swap(j - 1, j);
					j -= 1;
				}
				i += 1;
			}
			let mut merged = 0usize;
			let mut k = 0usize;
			while k < ram_region_count {
				let (base, size) = ram_regions[k];
				// TWO BANKS THAT TOUCH ARE ONE BANK ONLY IF THEY BELONG TO THE SAME NODE. On a
				// two-node machine QEMU emits banks that are exactly adjacent - 0x4000_0000 and
				// 0x5000_0000, one node each - and merging them produces a single range whose
				// affinity is whichever half was written first. The topology then says half the
				// machine's memory is local to a node it is not on, and every allocation steered by
				// it is wrong in a way nothing reports.
				let same_node = merged == 0 || ram_region_nodes[merged - 1] == ram_region_nodes[k];
				if merged > 0 && same_node {
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
				ram_region_nodes[merged] = ram_region_nodes[k];
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
			// THE FILES' HARTS, RESOLVED AGAINST THE WHOLE TREE. Deferred to here because a tree may
			// put its interrupt controller before or after the cpu nodes it names, and only one of
			// those orders lets the table be read as it is built. `u64::MAX` marks a file whose
			// entry names no cpu node in this tree - which is not a hart this kernel can find.
			let mut imsic_harts = [u64::MAX; MAX_CPUS];
			let mut imsic_hart_count: u32 = 0;
			if let Some((val, len)) = imsic_ext {
				let mut off: u32 = 0;
				while off + 8 <= len && (imsic_hart_count as usize) < MAX_CPUS {
					let phandle = self.be32(val + off as u64);
					if phandle != 0
						&& let Some(index) = cpu_phandles[..cpu_count as usize].iter().position(|&p| p == phandle)
					{
						imsic_harts[imsic_hart_count as usize] = cpu_ids[index];
					}
					imsic_hart_count += 1;
					off += 8;
				}
			}
			// A MATRIX WITHOUT ITS VERSION IS NOT THIS FORMAT. Checked here rather than at the node,
			// because `compatible` and `distance-matrix` may appear in either order.
			if numa_distance_count > 0 && !distance_map_versioned {
				numa_distance_malformed = true;
			}
			// A TIMER NODE THIS READER DID NOT RECOGNISE NAMES NO INTERRUPT. Zero is "the tree said
			// nothing I could decode", which the caller refuses on rather than defaulting.
			if !timer_compatible {
				timer_intid = 0;
			}
			// AND A TIMER ROUTED SOMEWHERE ELSE NAMES NO INTERRUPT THIS KERNEL MAY PROGRAM.
			//
			// The PPI's kind, number and sense were checked and its ROUTING was not. A timer node
			// whose `interrupt-parent` names a controller other than the main GIC this reader
			// selected describes a PPI on THAT controller - and enabling its INTID on the selected
			// GIC arms a per-core interrupt the tree does not say belongs there.
			//
			// THE EFFECTIVE PARENT IS THE STATED ONE OR THE INHERITED ONE, and checking only the
			// stated one was the hole (corrected 2026-08-30). `interrupt-parent` is inherited, and
			// the ordinary shape on both machines this reader boots is a root that states it and a
			// timer that does not - so "unstated is accepted" skipped the check on exactly the trees
			// it was written for, and a timer inheriting a different controller was enabled on the
			// selected GIC anyway. The timer is a root child, so the inherited value is the root's
			// and resolving it is one lookup rather than an inheritance implementation.
			//
			// A tree in which the GIC declares no phandle still has nothing to compare against and is
			// still accepted; that is a tree this reader cannot check rather than one it has checked.
			// Zeroing rather than flagging, because the caller already refuses a boot with no timer:
			// this is the same absence, arrived at for a stated reason.
			let effective_parent = if timer_parent != 0 { timer_parent } else { root_interrupt_parent };
			if effective_parent != 0 && gic_phandle != 0 && effective_parent != gic_phandle {
				timer_intid = 0;
			}
			// AND NO CHILD REGION MAY SHARE BYTES WITH THE CONTROLLER OR WITH THE OTHER CHILD.
			//
			// Asked HERE, where every range is known. The v2m frame and the ITS are the GIC node's
			// children, so each ends before its parent does - at the moment one is committed the
			// core addresses are still zero, and a comparison there compares against nothing.
			//
			// A frame that overlaps the distributor sends its MSI writes into the controller's own
			// registers; an ITS that overlaps either sends its commands there. Dropped rather than
			// refused whole: a machine with a bad child frame still has a working controller, and
			// the backend already treats a zero MSI base as "no message-signalled interrupts".
			// AND A CHILD BELONGING TO ANOTHER CANDIDATE IS NOT THIS CONTROLLER'S.
			//
			// Checked before the overlap rules below, because a frame under a different GIC is not
			// this machine's MSI frame whatever addresses it happens to hold - and comparing it with
			// the selected controller's ranges would be asking the wrong question about it.
			if gic_msi != 0 && msi_owner != selected_gic {
				gic_msi = 0;
				gic_msi_size = 0;
			}
			if gic_its != 0 && its_owner != selected_gic {
				gic_its = 0;
				gic_its_size = 0;
			}
			// AND A GICv3 REDISTRIBUTOR RANGE HAS TO COVER THE CORES THIS TREE DESCRIBES.
			//
			// One stride was the whole check, and it is only the floor for a one-core machine.
			// `this_redistributor` walks the frames looking for a core's affinity, so a four-core
			// tree with one frame leaves three cores whose redistributor is not in the range: the
			// backend logs that and returns, and those secondaries come up with no timer and no
			// wake interrupt at all. A range that cannot hold one frame per described core is a
			// machine description that contradicts itself, and this is where that is visible - the
			// cores are counted by the time the walk is over.
			if gic_version == 3 && gic_cpu != 0 && (cpu_count as u64) > 1 && gic_cpu_size < MIN_GICR_STRIDE * cpu_count as u64 {
				gic_dist = 0;
				gic_dist_size = 0;
				gic_cpu = 0;
				gic_cpu_size = 0;
				gic_msi = 0;
				gic_msi_size = 0;
				gic_its = 0;
				gic_its_size = 0;
			}
			if gic_msi != 0 && (ranges_overlap(gic_msi, gic_msi_size, gic_dist, gic_dist_size) || ranges_overlap(gic_msi, gic_msi_size, gic_cpu, gic_cpu_size)) {
				gic_msi = 0;
				gic_msi_size = 0;
			}
			if gic_its != 0 && (ranges_overlap(gic_its, gic_its_size, gic_dist, gic_dist_size) || ranges_overlap(gic_its, gic_its_size, gic_cpu, gic_cpu_size) || (gic_msi != 0 && ranges_overlap(gic_its, gic_its_size, gic_msi, gic_msi_size))) {
				gic_its = 0;
				gic_its_size = 0;
			}
			Some(BootInfo { ram_base, ram_size, ram_regions, ram_region_count, ram_region_nodes, cpu_count: cpu_count.max(1), cpu_ids, cpu_node_ids, numa_distances, numa_distance_count, numa_distance_malformed, timer_intid, _pad_timer: 0, cpu_nodes, pcie_ecam, plic_base, gic_dist, gic_dist_size, gic_cpu, gic_cpu_size, gic_version, gic_msi, gic_msi_size, gic_its, gic_its_size, pci_msi_rid_base, pci_msi_devid_base, pci_msi_length, imsic_base, imsic_size, imsic_guest_index_bits, imsic_group_index_bits, imsic_num_ids, imsic_harts, imsic_hart_count, fwcfg_base, modules_start, modules_end })
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
	// MMIO transaction width in bytes, from `reg-io-width` (FDT-006). One when the tree says
	// nothing, which is the 16550's own default; a part wired for 32-bit access rejects or faults
	// on byte transactions, and the loader's access layer already takes a width - it was simply
	// never told this one.
	pub reg_io_width: u32,
}

// A device-tree path is short and this reader does not allocate, so it goes in a fixed buffer. The
// longest in either QEMU tree is under thirty bytes; a path that does not fit is answered "no
// console" rather than truncated, because a truncated path names a DIFFERENT node.
const MAX_PATH: usize = 128;
// Device trees are shallow - three levels in both QEMU trees - and the cell stack is indexed by
// depth, so this bounds it. Deeper than this answers "no console".
const MAX_DEPTH: usize = 12;

// The register spacing this reader will accept (FDT-006). `reg-shift` names a power-of-two byte
// spacing between a 16550's registers; 3 is already eight bytes each, and every part in the wild
// declares 0, 1 or 2. It was taken as any `u32` and handed to the loader, which evaluates
// `5 << reg_shift` for the line-status register - a panic under checked arithmetic, or a masked
// offset into an unrelated register in release.
const MAX_REG_SHIFT: u32 = 3;

// One node on the path from the root, as far as addressing is concerned (FDT-003).
#[derive(Clone, Copy)]
struct Bus {
	// What this node declares for its CHILDREN.
	cells: (u32, u32),
	// This node's own `ranges`, as (value, len) into the blob. A MISSING `ranges` is not an empty
	// one: empty means the child address space IS the parent's, and missing means the child address
	// space is not reachable from the parent at all - which is what the specification says and what
	// this reader used to answer with the untranslated child address.
	ranges: Option<(u64, u32)>,
}

// One of the three platform devices `parse` resolves, remembered until its node CLOSES (FDT-003).
//
// The old walk entered a node on its unit-name prefix alone, at any depth, read its `reg` with the
// ROOT's cell counts, and committed the address there and then. So a node called `pcie-phy`, or one
// the firmware had marked `disabled`, could overwrite a real one; a node under a bus with different
// cell counts was read at the wrong width; and no address was ever translated through the bus it
// sits behind. The verdict is taken at the node's end now, with everything about it in hand.
// Do two physical ranges share a byte? Written as one function because two callers - the
// controller's own pair, and a caller checking a frame against them - must answer it the same way,
// and because an end computed with `+` wraps where this cannot.
// THE SMALLEST WINDOWS THE DRIVERS ACTUALLY REACH INTO.
//
// Taken from the offsets the aarch64 backend writes, not from the specification's maximum: a machine
// may legitimately declare a smaller distributor than the architecture allows, and refusing that would
// refuse a real controller. What cannot be legitimate is a window smaller than the registers this
// kernel writes into it - `GICD_IROUTER` sits at 0x6000, and a GICv2 CPU interface's `GICC_EOIR` at
// 0x10 - because those are stores outside the range the machine declared.
const MIN_GIC_DIST_SIZE: u64 = 0x7000;
const MIN_GIC_CPU_SIZE: u64 = 0x1000;

// AND A GICv3 REDISTRIBUTOR IS NOT A GICv2 CPU INTERFACE, however alike the two look in the tree.
//
// One `MIN_GIC_CPU_SIZE` of 0x1000 was applied to both, and a v3 redistributor FRAME is 0x20000 -
// two 64 KiB pages, RD_base and SGI_base. `this_redistributor` cannot inspect even one unless the
// declared size covers a whole stride, so an undersized v3 range was accepted as a main controller
// and the backend then logged that the core had no redistributor and carried on with no interrupts.
// One stride is the floor because a machine may declare exactly one core's worth.
const MIN_GICR_STRIDE: u64 = 0x20000;

// The registers the v2m frame's driver writes: `MSI_TYPER` at 0x008 and `MSI_SETSPI_NS` at 0x040. A
// frame smaller than that is one whose declared window does not contain the stores this kernel makes
// into it - the same rule the distributor minimum is derived from, applied to the child.
const MIN_GIC_V2M_SIZE: u64 = 0x1000;

// AN ITS IS A 128 KiB WINDOW, and it had no minimum at all.
//
// `GITS_CTLR` is at 0x0000 and `GITS_TRANSLATER` - the register a device writes to raise an
// interrupt - is at 0x10000, so a window that does not reach 0x20000 does not contain the registers
// this kernel and its devices write. Committed with no size check, a one-byte ITS was published and
// the first command wrote outside the range the machine declared. The same rule the distributor and
// the v2m frame already carry, applied to the third child.
const MIN_GIC_ITS_SIZE: u64 = 0x20000;

// A PRIVATE PERIPHERAL INTERRUPT, as the device tree's `interrupts` cells encode one.
const PPI_KIND: u32 = 1;
const PPI_COUNT: u32 = 16;
const PPI_INTID_BASE: u32 = 16;

// THE SENSE BITS OF AN INTERRUPT SPECIFIER'S THIRD CELL, and the four values that name one.
//
// The GIC binding puts the trigger type in the low nibble: 1 edge-rising, 2 edge-falling, 4
// level-high, 8 level-low. Exactly one of them is a sense; zero is a specifier that says nothing,
// and two of them is a specifier that says two contradictory things. Neither is an interrupt this
// reader can program, and the flags cell was not being looked at at all - so a tree carrying an
// unsupported or contradictory trigger published its number as the timer's INTID and the machine
// then armed a timer whose sense the description did not agree on.
const IRQ_SENSE_MASK: u32 = 0xf;

fn ranges_overlap(a: u64, a_len: u64, b: u64, b_len: u64) -> bool {
	let (a_end, b_end) = match (a.checked_add(a_len), b.checked_add(b_len)) {
		(Some(x), Some(y)) => (x, y),
		// An end that overflows is not a range this reader can compare, and calling that
		// "no overlap" would let it through.
		_ => return true,
	};
	a < b_end && b < a_end
}

#[derive(Clone, Copy)]
struct Device {
	// The depth the node was entered at, or -1 when not inside one.
	depth: i32,
	reg: Option<(u64, u32)>,
	// `status`, and whether `compatible` named something this reader knows. Both default to the
	// permissive answer for `status` and the refusing one for `compatible`, which is what each
	// property's absence means.
	enabled: bool,
	known: bool,
}

impl Device {
	const fn new() -> Self {
		Device { depth: -1, reg: None, enabled: true, known: false }
	}
	fn enter(&mut self, depth: i32) {
		*self = Device { depth, reg: None, enabled: true, known: false };
	}
	fn leave(&mut self) {
		self.depth = -1;
	}
	// Whether this node's address may be used.
	fn usable(&self) -> bool {
		self.enabled && self.known
	}
}

impl Bus {
	const fn root() -> Self {
		// The specification's own defaults for a node that declares nothing, and what every tree
		// this system meets writes explicitly anyway. The root needs no `ranges`: nothing is above
		// it to translate into.
		Bus { cells: (2, 2), ranges: Some((0, 0)) }
	}
}

impl Fdt {
	// Walk `address` - expressed in the address space of the node at `depth` - up to a root physical
	// address, through each ancestor's `ranges` (FDT-003).
	//
	// A `ranges` entry is `<child-address> <parent-address> <length>`, in the child bus's address
	// cells, the parent's address cells, and the child bus's size cells respectively. An empty
	// `ranges` is the identity. A missing one means the address does not translate, and an address
	// that falls in no entry does not translate either - both are refusals here rather than the
	// untranslated number, which is an address on a different bus.
	//
	// # Safety
	//
	// `buses[..=depth]` must hold values read out of this tree's own struct block.
	unsafe fn translate_to_root(&self, buses: &[Bus; MAX_DEPTH + 1], depth: usize, address: u64) -> Option<u64> {
		let mut address = address;
		let mut at = depth;
		while at > 0 {
			let bus = buses[at];
			let parent_cells = buses[at - 1].cells.0;
			let (value, len) = bus.ranges?;
			if len == 0 {
				at -= 1;
				continue; // an empty `ranges` is the identity mapping
			}
			// The counts this walk is about to fold into a `u64`, bounded here because they were
			// recorded as the tree declared them (FDT-006).
			if bus.cells.0 == 0 || bus.cells.0 > MAX_CELLS || parent_cells == 0 || parent_cells > MAX_CELLS || bus.cells.1 > MAX_CELLS {
				return None;
			}
			let entry = 4 * (bus.cells.0 as u64 + parent_cells as u64 + bus.cells.1 as u64);
			let mut q = value;
			let end = value + len as u64;
			let mut mapped: Option<u64> = None;
			while q + entry <= end {
				// SAFETY: inside the property this walk read out of the struct block.
				let (child, parent, size) = unsafe { (self.read_cells(&mut q, bus.cells.0), self.read_cells(&mut q, parent_cells), self.read_cells(&mut q, bus.cells.1)) };
				if address >= child && address - child < size {
					mapped = parent.checked_add(address - child);
					break;
				}
			}
			address = mapped?;
			at -= 1;
		}
		Some(address)
	}

	// Record one property of a platform device node, and answer whether it was one of the three
	// this reader cares about (FDT-003).
	//
	// `compatible` is checked against the bindings this reader actually knows rather than trusted
	// from the unit name: `pcie-phy`, `plic-sw` and anything else starting with those letters is a
	// node, and reading its `reg` as a controller's base address programs the wrong device.
	//
	// # Safety
	//
	// `name` and `value` must point into this tree's struct block, with `len` bytes of value.
	unsafe fn record_device(&self, device: &mut Device, name: u64, value: u64, len: u32, compatible: &[&str]) -> bool {
		unsafe {
			if self.str_eq(name, "reg") {
				device.reg = Some((value, len));
				return true;
			}
			if self.str_eq(name, "status") {
				device.enabled = (len >= 5 && self.str_eq(value, "okay")) || (len >= 3 && self.str_eq(value, "ok"));
				return true;
			}
			if self.str_eq(name, "compatible") {
				for want in compatible {
					if self.stringlist_contains(value, len, want.as_bytes()) {
						device.known = true;
					}
				}
				return true;
			}
		}
		false
	}

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
		let mut reg_io_width = 1u32;
		// The node's own verdict on whether it may be used at all. Absent means enabled.
		let mut enabled = true;
		// Whether anything the node said was outside what this reader can represent. A tree that
		// says something impossible gets NO console rather than an address assembled out of
		// whatever followed the property (FDT-006).
		let mut refused = false;
		unsafe {
			self.walk_node(&parts[..count], |name, value, len, cells, buses, depth| {
				if self.str_eq(name, "compatible") {
					// A `compatible` is a list, most specific first, and a machine may name a chip
					// this system does not know followed by one it does. So every entry is tried
					// rather than only the first.
					if uart.is_none() {
						uart = self.uart_from_compatible(value, len);
					}
				} else if self.str_eq(name, "reg") && base.is_none() {
					// ONE RULE FOR CELL COUNTS, EVERYWHERE (FDT-006). This path allowed one through
					// FOUR while the reader folds cells into a `u64`, so a bus declaring three cells
					// produced a low 64-bit suffix of a 96-bit address - a number that looks like a
					// valid physical address and is not one. `MAX_CELLS` is the rule the root parser
					// already applied.
					let mut at = value;
					if !(1..=MAX_CELLS).contains(&cells.0) || len < 4 * cells.0 {
						refused = true;
						return;
					}
					let child = self.read_cells(&mut at, cells.0);
					// AND TRANSLATED UP THE BUSES IT SITS BEHIND (FDT-003). `reg` is an address on
					// the parent bus, not a physical one; the console used to be reached at the raw
					// child address, which is the right offset on the wrong bus.
					match self.translate_to_root(buses, depth - 1, child) {
						Some(physical) => base = Some(physical),
						None => refused = true,
					}
				} else if self.str_eq(name, "reg-shift") && len == 4 {
					let shift = self.be32(value);
					if shift > MAX_REG_SHIFT {
						refused = true;
						return;
					}
					reg_shift = shift;
				} else if self.str_eq(name, "reg-io-width") && len == 4 {
					// Only the widths the loader's access layer implements. A part that needs a
					// width this system cannot issue is a console it cannot drive, and saying so is
					// better than byte-poking a register file that will not answer.
					let width = self.be32(value);
					if !matches!(width, 1 | 2 | 4) {
						refused = true;
						return;
					}
					reg_io_width = width;
				} else if self.str_eq(name, "status") {
					// The same rule as `/memory` and the cpu nodes: `okay` or absent means usable.
					enabled = (len >= 5 && self.str_eq(value, "okay")) || (len >= 3 && self.str_eq(value, "ok"));
				}
			});
		}
		if refused || !enabled {
			return None;
		}
		let uart = uart?;
		let base = base.filter(|address| *address != 0)?;
		Some(Console { uart, base, reg_shift: if uart == Uart::Pl011 { 0 } else { reg_shift }, reg_io_width })
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
	unsafe fn walk_node(&self, parts: &[&[u8]], visit: impl FnMut(u64, u64, u32, (u32, u32), &[Bus; MAX_DEPTH + 1], usize)) {
		// A malformed record stops the walk, which for this one means "no more properties" - its
		// callers already treat an absent property as the answer.
		let _ = unsafe { self.walk_node_inner(parts, visit) };
	}

	unsafe fn walk_node_inner(&self, parts: &[&[u8]], mut visit: impl FnMut(u64, u64, u32, (u32, u32), &[Bus; MAX_DEPTH + 1], usize)) -> Option<()> {
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
			// Each node on the current path, so a matched node's `reg` can be walked up through the
			// buses it sits behind (FDT-003).
			let mut buses = [Bus::root(); MAX_DEPTH + 1];
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
							buses[0] = Bus::root();
						} else {
							cells[depth as usize] = cells[depth as usize - 1];
							// A node that declares no `ranges` is one whose children are not
							// addressable from its parent; inheriting the parent's would be
							// inventing a mapping the tree does not state.
							buses[depth as usize] = Bus { cells: cells[depth as usize], ranges: None };
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
							// RECORDED AS DECLARED, and bounded where it is USED (FDT-006).
							//
							// Refusing the whole walk here would be wrong: a PCIe node declares
							// three address cells on every machine, and it is a sibling of the
							// console rather than an ancestor of it. What must not happen is a cell
							// count wider than a `u64` reaching the folding reader, so `console_at`
							// and `translate_to_root` each check the counts they are about to use.
							cells[depth as usize].0 = self.be32(value);
							buses[depth as usize].cells.0 = cells[depth as usize].0;
						} else if len == 4 && self.str_eq(pname, "#size-cells") {
							cells[depth as usize].1 = self.be32(value);
							buses[depth as usize].cells.1 = cells[depth as usize].1;
						} else if self.str_eq(pname, "ranges") {
							buses[depth as usize].ranges = Some((value, len));
						}
						if matched == depth && depth as usize == parts.len() {
							visit(pname, value, len, cells[depth as usize - 1], &buses, depth as usize);
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
			self.walk_node(parts, |name, value, len, cells, _buses, _depth| {
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
	// ALL OR NOTHING, AND NOTHING IS COMMITTED UNTIL THE WHOLE LIST HAS VALIDATED (FDT-005).
	//
	// This used to hand each entry to the caller as it read it, and answer `false` afterwards if the
	// list turned out to be unterminated or over the cap - so a caller that treated the answer as a
	// diagnostic (which the one in this tree did) had already carved a PREFIX of a list it could not
	// read, and went on using the rest of RAM as though the list had been empty. A zero-size or
	// end-overflowing entry was skipped without even marking the list incomplete.
	//
	// The entries are collected into a fixed array first. If anything about the list is wrong - a
	// truncated pair, a zero-size non-terminator, an end that overflows, no terminator within the
	// cap - the caller sees NO entries and a `false`, and can decide about the whole tree rather
	// than about a prefix it has already committed.
	pub fn for_each_reserved_region(&self, mut visit: impl FnMut(u64, u64)) -> bool {
		let mut entries = [(0u64, 0u64); MAX_RESERVED_REGIONS];
		let mut count = 0usize;
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
			let mut terminated = false;
			// ONE MORE ITERATION THAN THE ARRAY HOLDS, so a FULL list followed by its terminator is
			// a list that terminated rather than one that ran out.
			for _ in 0..=MAX_RESERVED_REGIONS {
				if p.checked_add(16).is_none_or(|next| next > end) {
					return false;
				}
				let address = ((self.be32(p) as u64) << 32) | self.be32(p + 4) as u64;
				let size = ((self.be32(p + 8) as u64) << 32) | self.be32(p + 12) as u64;
				p += 16;
				if address == 0 && size == 0 {
					terminated = true;
					break;
				}
				// A zero-size entry, or one whose end overflows, is the tree contradicting itself.
				// Carving a saturated range would take the whole address space out of the
				// allocator, and skipping it quietly is how a reservation stops being one - so the
				// list is refused instead, and with it the tree's account of memory.
				if size == 0 || address.checked_add(size).is_none() {
					return false;
				}
				if count == MAX_RESERVED_REGIONS {
					return false; // more reservations than this reader carries
				}
				entries[count] = (address, size);
				count += 1;
			}
			if !terminated {
				// The list did not terminate within the cap: refused, because a client that carried
				// on would be using memory the firmware reserved.
				return false;
			}
		}
		for &(address, size) in &entries[..count] {
			visit(address, size);
		}
		true
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
