// THE POLICY THE KERNEL ENFORCES ON FIRMWARE CODE, as pure functions over what the kernel knows - its memory map,
// the ranges it holds, every PCI BAR and bridge window, the live claims - so every rule is host-tested. The ACPI
// service asks; these decide.
//
//   SYSTEM MEMORY regions map ACPI NVS, ACPI-reclaimable and firmware-reserved memory WRITE-BACK, and MMIO outside
//   RAM UNCACHED, and nothing else: never RAM, never a kernel-held range, a BAR or a window, never a claimed
//   device's range - with two admissions. A claimed namespace device's OWN node may map inside its own `_CRS`
//   range while it is claimed (the driver and the node's methods share that memory); and a BAR of a function no
//   driver holds may be mapped by the node that is that function's companion, which makes the function
//   FIRMWARE-HELD. One memory type per range: a region straddling write-back and uncached memory is refused.
//   A development build adds the fixture carve-out: inside a firmware-held `ivshmem-plain` BAR, a region its own
//   companion node declares, and a region a claimed device's own node declares inside that device's range.
//   SYSTEM I/O regions are refused over the reserved set (the caller's `recordable`) and inside a live claim's
//   ports. PCI CONFIGURATION writes are refused below 0x40, inside MSI and MSI-X capabilities, to a function a
//   driver holds and to the registers the kernel's chipset rows own. `_CRS`-derived resources are checked before a
//   row is published; a reservation mints nothing and a merged description adds nothing, so neither is refused.

use crate::report::Function;

/// What the memory map says a range is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemoryKind {
	Ram,
	AcpiNvs,
	AcpiReclaimable,
	FirmwareReserved,
	/// A hole in the map, or memory the loader reported as MMIO.
	Mmio,
}

impl MemoryKind {
	pub fn from_memmap(kind: u32) -> MemoryKind {
		match kind {
			abi::MEMMAP_ACPI_NVS => MemoryKind::AcpiNvs,
			abi::MEMMAP_ACPI_RECLAIMABLE => MemoryKind::AcpiReclaimable,
			abi::MEMMAP_RESERVED => MemoryKind::FirmwareReserved,
			abi::MEMMAP_MMIO => MemoryKind::Mmio,
			_ => MemoryKind::Ram,
		}
	}
}

/// One BAR or bridge window, with who holds its function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
	pub base: u64,
	pub len: u64,
	pub function: Function,
	/// A bridge's window rather than a function's own BAR.
	pub window: bool,
	pub driver_held: bool,
	pub firmware_held: bool,
	/// The development fixture's region device (`ivshmem-plain`, 1af4:1110) - the carve-out's function.
	pub fixture: bool,
}

/// A live claim's range and the identity of the node that owns it.
#[derive(Clone, Copy, Debug)]
pub struct Claimed<'a> {
	pub base: u64,
	pub len: u64,
	pub node: &'a [u8],
}

/// Everything a system-memory decision reads.
#[derive(Clone, Copy, Debug)]
pub struct MemoryView<'a> {
	/// The memory map: (base, length, kind).
	pub regions: &'a [(u64, u64, MemoryKind)],
	pub kernel_held: &'a [(u64, u64)],
	pub bars: &'a [Bar],
	pub claimed: &'a [Claimed<'a>],
	/// The development build's fixture carve-out is compiled in.
	pub carve_out: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
	WriteBack,
	Uncached,
	/// Uncached, and the function whose BAR it is becomes firmware-held.
	FirmwareHold(Function),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
	Empty,
	Ram,
	/// Write-back and uncached memory in one range.
	Mixed,
	KernelHeld,
	Bar(Function),
	Window(Function),
	/// A driver holds the function whose BAR it is.
	DriverHeld(Function),
	/// Another node's claimed device owns it: the index into `claimed`.
	Claimed(usize),
}

fn overlaps(a_base: u64, a_len: u64, b_base: u64, b_len: u64) -> bool {
	a_base < b_base.saturating_add(b_len) && b_base < a_base.saturating_add(a_len)
}

fn inside(base: u64, len: u64, outer_base: u64, outer_len: u64) -> bool {
	base >= outer_base && base.saturating_add(len) <= outer_base.saturating_add(outer_len)
}

/// THE KIND OF MEMORY a range is: the kinds of every map region it overlaps, a gap in the map being MMIO.
fn kinds(view: &MemoryView<'_>, base: u64, len: u64) -> (bool, bool, bool) {
	let (mut ram, mut write_back, mut mmio) = (false, false, false);
	let mut covered: u64 = 0;
	for &(start, size, kind) in view.regions {
		if !overlaps(base, len, start, size) {
			continue;
		}
		let from = base.max(start);
		let to = base.saturating_add(len).min(start.saturating_add(size));
		covered = covered.saturating_add(to - from);
		match kind {
			MemoryKind::Ram => ram = true,
			MemoryKind::AcpiNvs | MemoryKind::AcpiReclaimable | MemoryKind::FirmwareReserved => write_back = true,
			MemoryKind::Mmio => mmio = true,
		}
	}
	if covered < len {
		mmio = true;
	}
	(ram, write_back, mmio)
}

/// MAY `declaring_node` MAP `base..base+len` of system memory - the node whose `OperationRegion` it is, and the PCI
/// function that node is the companion of, if it is one.
pub fn system_memory(view: &MemoryView<'_>, base: u64, len: u64, declaring_node: &[u8], companion: Option<Function>) -> Result<Admission, Refusal> {
	if len == 0 || base.checked_add(len).is_none() {
		return Err(Refusal::Empty);
	}
	let (ram, write_back, mmio) = kinds(view, base, len);
	if ram {
		return Err(Refusal::Ram);
	}
	if write_back && mmio {
		return Err(Refusal::Mixed);
	}
	if view.kernel_held.iter().any(|&(start, size)| overlaps(base, len, start, size)) {
		return Err(Refusal::KernelHeld);
	}
	// ADMISSION ONE: inside a claimed range its own node owns. Any other node's claim over it refuses.
	let own_claim = view.claimed.iter().any(|claim| claim.node == declaring_node && inside(base, len, claim.base, claim.len));
	for (index, claim) in view.claimed.iter().enumerate() {
		if overlaps(base, len, claim.base, claim.len) && !own_claim {
			return Err(Refusal::Claimed(index));
		}
	}
	let mut hold: Option<Function> = None;
	for bar in view.bars.iter().filter(|bar| overlaps(base, len, bar.base, bar.len)) {
		if bar.window {
			// A window holds every BAR behind its bridge: a region in one is judged by the BAR it lands in, if the
			// BAR is that of the companion; any other region in a window is refused.
			let in_companion_bar = view.bars.iter().any(|inner| !inner.window && Some(inner.function) == companion && inside(base, len, inner.base, inner.len));
			if in_companion_bar || (own_claim && view.carve_out) {
				continue;
			}
			return Err(Refusal::Window(bar.function));
		}
		// THE FIXTURE CARVE-OUT, development builds alone: inside the firmware-held fixture BAR, a claimed device's
		// own node reaches its own range.
		if view.carve_out && bar.fixture && bar.firmware_held && own_claim {
			continue;
		}
		// ADMISSION TWO: the companion's own BAR, of a function no driver holds.
		if Some(bar.function) == companion {
			if bar.driver_held {
				return Err(Refusal::DriverHeld(bar.function));
			}
			hold = Some(bar.function);
			continue;
		}
		return Err(Refusal::Bar(bar.function));
	}
	if let Some(function) = hold {
		return Ok(Admission::FirmwareHold(function));
	}
	if own_claim {
		return Ok(Admission::Uncached);
	}
	Ok(if write_back { Admission::WriteBack } else { Admission::Uncached })
}

/// A CLAIM REFUSED WHILE ANOTHER NODE'S REGION MAPS ANY PART OF ITS RANGES, whichever came first: the region
/// mappings as (base, len, declaring node); answers the first conflicting one.
pub fn claim_blocked(ranges: &[(u64, u64)], own_node: &[u8], mappings: &[(u64, u64, &[u8])]) -> Option<usize> {
	mappings.iter().position(|&(base, len, node)| node != own_node && ranges.iter().any(|&(start, size)| overlaps(base, len, start, size)))
}

/// A SYSTEM I/O REGION: `recordable` is the reserved set and the live grants; a live claim's ports refuse too.
pub fn system_io(base: u16, len: u16, recordable: impl Fn(u16, u16) -> bool, claimed_ports: &[(u16, u16)]) -> bool {
	if len == 0 || u32::from(base) + u32::from(len) > 0x1_0000 || !recordable(base, len) {
		return false;
	}
	!claimed_ports.iter().any(|&(start, size)| overlaps(u64::from(base), u64::from(len), u64::from(start), u64::from(size)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigRefusal {
	/// The standard header - below 0x40.
	Header,
	/// Inside an MSI or MSI-X capability.
	Msi,
	/// A driver holds the function.
	DriverHeld,
	/// A register the kernel's chipset rows own - the ECAM base, the PM and GPE bases.
	Chipset,
	/// Not a naturally aligned byte, word or dword.
	Width,
}

/// A PCI CONFIGURATION WRITE: `msi` the byte ranges of the function's MSI and MSI-X capabilities, `chipset` the
/// kernel-owned registers of its (vendor, device).
pub fn config_write(offset: u16, width: u8, msi: &[(u16, u16)], driver_held: bool, chipset: &[(u16, u16)]) -> Result<(), ConfigRefusal> {
	if !matches!(width, 1 | 2 | 4) || offset % u16::from(width) != 0 {
		return Err(ConfigRefusal::Width);
	}
	if offset < 0x40 {
		return Err(ConfigRefusal::Header);
	}
	let (o, w) = (u64::from(offset), u64::from(width));
	if msi.iter().any(|&(start, size)| overlaps(o, w, u64::from(start), u64::from(size))) {
		return Err(ConfigRefusal::Msi);
	}
	if driver_held {
		return Err(ConfigRefusal::DriverHeld);
	}
	if chipset.iter().any(|&(start, size)| overlaps(o, w, u64::from(start), u64::from(size))) {
		return Err(ConfigRefusal::Chipset);
	}
	Ok(())
}

/// Why a `_CRS`-derived resource refuses a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrsRefusal {
	MmioOverRam,
	MmioKernelHeld,
	MmioOverBar(Function),
	PortsReserved,
	PortsOverBar,
	/// A wired line the kernel uses: the timer, the SCI, a kernel-held console's line.
	KernelLine(u32),
}

/// `_CRS`-DERIVED RESOURCES, checked before a row is published: MMIO not over RAM - ACPI NVS and firmware-reserved
/// memory excepted, where firmware places mailboxes - kernel-held ranges or BARs and windows (the fixture carve-out
/// admitting the firmware-held fixture BAR in a development build); ports not reserved and not a PCI I/O BAR; lines
/// not the kernel's.
pub fn crs(description: &crate::Description, view: &MemoryView<'_>, ports_recordable: impl Fn(u16, u16) -> bool, io_bars: &[(u16, u16)], kernel_lines: &[u32]) -> Result<(), CrsRefusal> {
	for range in description.part.mmio() {
		let (ram, _, _) = kinds(view, range.base, range.len);
		if ram {
			return Err(CrsRefusal::MmioOverRam);
		}
		if view.kernel_held.iter().any(|&(start, size)| overlaps(range.base, range.len, start, size)) {
			return Err(CrsRefusal::MmioKernelHeld);
		}
		for bar in view.bars.iter().filter(|bar| overlaps(range.base, range.len, bar.base, bar.len)) {
			let carved = view.carve_out && (bar.fixture && bar.firmware_held || bar.window && view.bars.iter().any(|inner| inner.fixture && inner.firmware_held && inside(range.base, range.len, inner.base, inner.len)));
			if !carved {
				return Err(CrsRefusal::MmioOverBar(bar.function));
			}
		}
	}
	for port in description.ports() {
		if !ports_recordable(port.base, port.len) {
			return Err(CrsRefusal::PortsReserved);
		}
		if io_bars.iter().any(|&(start, size)| overlaps(u64::from(port.base), u64::from(port.len), u64::from(start), u64::from(size))) {
			return Err(CrsRefusal::PortsOverBar);
		}
	}
	for line in description.part.lines() {
		if kernel_lines.contains(&line.number) {
			return Err(CrsRefusal::KernelLine(line.number));
		}
	}
	Ok(())
}

/// A row as reconciliation reads it.
#[derive(Clone, Copy, Debug)]
pub struct RowState<'a> {
	pub part: &'a abi::PlatformPart,
	/// Withdrawn by a walk: the row and its index stay, and a report of its path refills it.
	pub withdrawn: bool,
}

/// What a walk's report of a path is, against what is published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconcile {
	/// A live row carries the path: the same row, no event. `differs` when the report is not what it was published
	/// with - logged, the first kept.
	Same { row: usize, differs: bool },
	/// A withdrawn row carries it: refilled, with a new generation and an arrival.
	Refill(usize),
	/// Nothing carries it: placed as any description is.
	Place,
}

fn same_resources(a: &abi::PlatformPart, b: &abi::PlatformPart) -> bool {
	a.mmio() == b.mmio() && a.lines() == b.lines() && a.state == b.state
}

/// RECONCILE a report of `description` with the rows, BY IDENTITY: its path as a row's identity, or as an identity
/// merged into a static row.
pub fn reconcile(rows: &[Option<RowState<'_>>], description: &crate::Description) -> Reconcile {
	let identity = description.identity();
	for (row, state) in rows.iter().enumerate() {
		let Some(state) = state else { continue };
		let own = state.part.identity() == identity;
		let merged = state.part.match_ids().iter().any(|id| id.kind == abi::MATCH_ID_IDENTITY && id.text() == identity);
		if !own && !merged {
			continue;
		}
		if state.withdrawn && own {
			return Reconcile::Refill(row);
		}
		return Reconcile::Same { row, differs: own && !same_resources(state.part, &description.part) };
	}
	Reconcile::Place
}

/// Whether `description` - published after `reservation` - lies over the reservation's ranges: a RESERVATION keeps
/// every NEW row out of its ranges, and leaves the rows published before it alone.
pub fn reserved_against(reservation: &abi::PlatformPart, reservation_ports: &[abi::PortResource], description: &crate::Description) -> bool {
	let mmio = description.part.mmio().iter().any(|mine| reservation.mmio().iter().any(|theirs| overlaps(mine.base, mine.len, theirs.base, theirs.len)));
	let ports = description.ports().iter().any(|mine| reservation_ports.iter().any(|theirs| overlaps(u64::from(mine.base), u64::from(mine.len), u64::from(theirs.base), u64::from(theirs.len))));
	mmio || ports
}

/// THE SMBIOS TYPE-38 RECORD an `IPI0001` node agrees with: the same interface type and base address - the
/// records as (interface type, base address), answering the index `smbios:38#n` names.
pub fn smbios_ipmi(records: &[(u8, u64)], interface_type: u8, base: u64) -> Option<usize> {
	records.iter().position(|&(kind, address)| kind == interface_type && address & !1 == base & !1)
}

/// The SMBIOS interface type of SSIF.
pub const SMBIOS_IPMI_SSIF: u8 = 4;

/// THE TYPE-38 RECORD AN SSIF NODE AGREES WITH: an SSIF record whose seven-bit address - the record's base shifted right
/// by one, as the boot recorded it - is the node's `I2cSerialBusV2` address. Every bit of a bus address is part of it:
/// there is no I/O-space flag to ignore here. The record names no controller, so the address is all there is.
pub fn smbios_ssif(records: &[(u8, u64)], address: u16) -> Option<usize> {
	records.iter().position(|&(kind, recorded)| kind == SMBIOS_IPMI_SSIF && recorded == u64::from(address))
}
