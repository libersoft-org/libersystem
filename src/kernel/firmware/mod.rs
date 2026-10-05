// THE ACPI SERVICE'S KERNEL SIDE: what the kernel keeps for the firmware interpreter it does not run.
//
// AML is firmware code and it can be wrong, so it runs in a userspace service holding the `FirmwareInterpreter`
// privilege, and every authority that service has is minted here, under `platform::policy`: the ranges the bus
// decodes, recorded at the boot scan; the service's SystemMemory mappings and the functions its regions made
// FIRMWARE-HELD; what its walk published - companions, merged descriptions, the parent functions of nodes below a
// companion - and the identities the running instance has reported; the running instance and its event channel;
// and every host bridge's `_OSC` answer. The syscalls are `syscall::firmware`; the table operations `device`'s.
//
// WHAT OUTLIVES AN INSTANCE. A crashed service is restarted with a fresh namespace: its mappings go with its handles,
// every general-purpose event it had enabled is disabled, its event channel is dropped. What it PUBLISHED stays -
// rows, reservations, companions, firmware-held functions - and the next instance's walk is reconciled with it by
// identity; that instance's "namespace loaded" report withdraws what its walk did not report again, and only then
// reaches DeviceManager.
//
// LOCK ORDER: the device table, then the claims, then this module's state - every path that needs more than one takes
// them in that order.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::fmt;

use platform::policy::{self, Admission, Bar, Claimed, MemoryKind, MemoryView};
use platform::report::{Function, List, Report};

use crate::mem::heap;
use crate::object::channel::Channel;
use crate::object::device_memory::DeviceMemory;
use crate::sync::SpinLock;

#[cfg(test)]
mod tests;

// One SystemMemory region the service mapped: its range, the node whose region it is, and the object - live while the
// service holds it, which is what "while a region maps it" means to a claim.
struct Mapping {
	base: u64,
	len: u64,
	node: Vec<u8>,
	object: Weak<DeviceMemory>,
}

// A companion: the node that describes a PCI function, attached to the function's row, and - for a GPIO or
// serial-bus controller - the lines and addresses the service holds through it.
struct Companion {
	function: Function,
	path: Vec<u8>,
	aei: List,
	field_lines: List,
	field_addresses: List,
}

// A namespace description merged into an earlier row, and the ids it added.
struct Merge {
	row: usize,
	identity: Vec<u8>,
	ids: Vec<abi::MatchId>,
}

// One BAR or window of the bus, recorded at the boot scan.
#[derive(Clone, Copy)]
struct Decoded {
	base: u64,
	len: u64,
	function: Function,
	window: bool,
}

struct State {
	// The number of the running instance, or of the last one; 0 before the first.
	instance: u64,
	// The running instance's process, by koid.
	holder: Option<u64>,
	events: Option<Arc<Channel>>,
	decoded: Vec<Decoded>,
	// Every bridge's I/O window, from the boot scan.
	io_windows: Vec<(u16, u16)>,
	mappings: Vec<Mapping>,
	held: Vec<Function>,
	companions: Vec<Companion>,
	parents: Vec<(usize, Function)>,
	merged: Vec<Merge>,
	// What the running instance's walk reported: identities of rows, reservations, merged descriptions, companions.
	reported: Vec<Vec<u8>>,
	grants: Vec<(u16, u8, u8, u32)>,
	// A namespace row that is itself a controller: its identity and the lines and addresses the service holds through it.
	lists: Vec<(Vec<u8>, List, List, List)>,
	// SMBIOS's IPMI records (type 38), by instance: the interface type and the base address.
	ipmi: Vec<(u8, u64)>,
	// THE MEMORY PROCESSOR POWER HOLDS - a register a processor table names, admitted by `admit_processor_memory` - once
	// for each admission: no region of the service's maps over it while a table names it.
	processor_held: Vec<(u64, u64)>,
	// THE NAMESPACE DEVICES A DMAR PUTS BEHIND A HARDWARE UNIT: each one's identity (`acpi:` and its path, every
	// segment four characters) and the requester id its DMA carries.
	streams: Vec<(Vec<u8>, u32)>,
}

static STATE: SpinLock<State> = SpinLock::new(State { instance: 0, holder: None, events: None, decoded: Vec::new(), io_windows: Vec::new(), mappings: Vec::new(), held: Vec::new(), companions: Vec::new(), parents: Vec::new(), merged: Vec::new(), reported: Vec::new(), grants: Vec::new(), lists: Vec::new(), ipmi: Vec::new(), processor_held: Vec::new(), streams: Vec::new() });

// A NAMESPACE DEVICE THAT MASTERS THE BUS, as the DMAR names it (`\_SB.PCI0.I2C1`): kept under the identity the ACPI
// service will publish it with - `acpi:` and the path with every segment padded to four characters, as the namespace
// holds names - and its requester id.
#[cfg(any(not(test), target_arch = "x86_64"))]
pub fn note_namespace_stream(name: &[u8], source_id: u16) {
	let Some(identity) = namespace_identity(name) else { return };
	let mut state = STATE.lock();
	if state.streams.try_reserve(1).is_ok() {
		state.streams.push((identity, u32::from(source_id)));
	}
}

// `\_SB.PCI0.I2C1` as an identity: `acpi:\_SB_.PCI0.I2C1`.
pub fn namespace_identity(name: &[u8]) -> Option<Vec<u8>> {
	let mut identity: Vec<u8> = Vec::new();
	identity.try_reserve(5 + name.len() + 8).ok()?;
	identity.extend_from_slice(b"acpi:");
	let path = name.strip_prefix(b"\\").unwrap_or(name);
	identity.push(b'\\');
	for (at, segment) in path.split(|byte| *byte == b'.').enumerate() {
		if segment.is_empty() || segment.len() > 4 {
			return None;
		}
		if at > 0 {
			identity.push(b'.');
		}
		identity.extend_from_slice(segment);
		for _ in segment.len()..4 {
			identity.push(b'_');
		}
	}
	Some(identity)
}

// THE STREAM A DMAR NAMED FOR THIS ROW, attached: the row's claim is then refused by name, since no IOMMU driver of this
// kernel serves a DMAR unit - never a non-mastering claim of a device that masters the bus.
fn attach_stream(description: &mut platform::Description) {
	let state = STATE.lock();
	if let Some((_, stream)) = state.streams.iter().find(|(identity, _)| identity.as_slice() == description.identity()) {
		description.part.flags |= abi::PLATFORM_FLAG_DMA_STREAM;
		description.part.dma_stream = *stream;
	}
}

// SMBIOS'S IPMI RECORD `instance`, as the boot read it: what an `IPI0001` node is checked against.
#[cfg(any(not(test), target_arch = "x86_64"))]
pub fn note_ipmi_record(instance: usize, interface: u8, address: u64) {
	let mut state = STATE.lock();
	if state.ipmi.len() <= instance {
		state.ipmi.resize(instance + 1, (0, 0));
	}
	state.ipmi[instance] = (interface, address);
}

// THE IPMI NODE'S SMBIOS RECORD: an `IPI0001` description whose `_IFT` (in its property block) and base address - its
// first port or memory range - agree with a type-38 record is given that record's id, `smbios:38#n`.
fn attach_smbios(description: &mut platform::Description, properties: &[u8]) {
	if !description.part.match_ids().iter().any(|id| id.kind == abi::MATCH_ID_HID && id.text() == b"IPI0001") {
		return;
	}
	let Some(interface) = property_integer(properties, b"_IFT") else { return };
	// SSIF: the SMBus address its `I2cSerialBusV2` names, held against the SSIF records' seven-bit addresses.
	if interface == u64::from(policy::SMBIOS_IPMI_SSIF) {
		let Some(address) = description.part.connections().iter().find(|connection| connection.kind == abi::CONNECTION_I2C).map(|connection| connection.value as u16) else { return };
		let found = policy::smbios_ssif(&STATE.lock().ipmi, address);
		match found {
			Some(at) => smbios_match(description, at),
			None => crate::serial_println!("firmware: {} names an SSIF interface at SMBus address {address:#04x}, which no SMBIOS record describes", Text(description.identity())),
		}
		return;
	}
	let base = description.ports().first().map(|port| port.base as u64).or_else(|| description.part.mmio().first().map(|range| range.base));
	let Some(base) = base else { return };
	let found = policy::smbios_ipmi(&STATE.lock().ipmi, interface as u8, base);
	match found {
		Some(at) => smbios_match(description, at),
		None => crate::serial_println!("firmware: {} names IPMI interface {interface} at {base:#x}, which no SMBIOS record describes", Text(description.identity())),
	}
}

// THE RECORD'S ID ON THE DESCRIPTION, `smbios:38#n`, formatted fallibly: on a report's path a short heap leaves the node
// without it rather than halting.
fn smbios_match(description: &mut platform::Description, at: usize) {
	let Some(text) = heap::try_format(format_args!("smbios:38#{at}")) else { return };
	if description.add_match(abi::MATCH_ID_SMBIOS, text.as_bytes()) {
		crate::serial_println!("firmware: {} agrees with {text}", Text(description.identity()));
	}
}

// An integer property of a depth-0 record in a property block, in the node channel's value encoding.
fn property_integer(block: &[u8], wanted: &[u8]) -> Option<u64> {
	let mut at = 0usize;
	while at + 8 <= block.len() {
		let kind = block[at];
		let depth = block[at + 1];
		let name_len = u16::from_le_bytes([block[at + 2], block[at + 3]]) as usize;
		let value_len = u32::from_le_bytes(block[at + 4..at + 8].try_into().ok()?) as usize;
		let name = block.get(at + 8..at + 8 + name_len)?;
		let value = block.get(at + 8 + name_len..at + 8 + name_len + value_len)?;
		if kind == abi::DEVICE_PROPERTY_VALUE && depth == 0 && name == wanted && value.len() == 9 && value[0] == 0x01 {
			return Some(u64::from_le_bytes(value[1..9].try_into().ok()?));
		}
		at += 8 + name_len + ((value_len + 3) & !3);
	}
	None
}

// ON AN ACPI MACHINE, native hot-plug and error reporting wait for `_OSC`. Before the first scan; the test kernel
// keeps native control, as its fixtures are the bus's own.
// THE SLEEP-TYPE REGISTRATION REFUSED, on a development boot whose switch says so - soft-off's fallback half.
pub fn sleep_registration_refused() -> bool {
	crate::arch::absent_named(b"sleep-types")
}

pub fn init() {
	if !cfg!(test) && crate::arch::firmware::available() {
		crate::arch::pci::gate_on_osc();
	}
}

// THE BUS'S BARS AND WINDOWS, from the boot scan - every function's, resolved family or not - and every bridge's I/O
// window.
pub fn record_decoded(ranges: Vec<crate::arch::common::pci::DecodedRange>, io_windows: Vec<(u16, u16)>) {
	let mut state = STATE.lock();
	// ALLOC-OK: boot, the bus's ranges recorded once from the boot scan, before userspace exists.
	state.decoded = ranges.iter().map(|range| Decoded { base: range.base, len: range.len, function: Function { segment: 0, bus: range.bus, device: range.dev, function: range.func }, window: range.window }).collect();
	state.io_windows = io_windows;
}

// THE FUNCTION WHOSE MEMORY BAR HOLDS `address` - the boot framebuffer's decoder - from the boot scan's record. A
// bridge's window is not a decoder: the function behind it is.
pub fn decoder_of(address: u64) -> Option<Function> {
	decoder_in(STATE.lock().decoded.iter().map(|range| (range.base, range.len, range.function, range.window)), address)
}

// The containment decision itself, over any list of (base, length, function, window).
pub fn decoder_in(ranges: impl Iterator<Item = (u64, u64, Function, bool)>, address: u64) -> Option<Function> {
	let mut found = None;
	for (base, len, function, window) in ranges {
		if window || len == 0 || address < base || address - base >= len {
			continue;
		}
		found = Some(function);
		break;
	}
	found
}

pub fn decoded_ranges(out: &mut dyn FnMut(u64, u64)) {
	for range in STATE.lock().decoded.iter() {
		out(range.base, range.len);
	}
}

fn function_of(address: u64) -> Option<Function> {
	if address == abi::FIRMWARE_NO_FUNCTION {
		return None;
	}
	Some(Function { segment: (address >> 32) as u16, bus: (address >> 16) as u8, device: (address >> 8) as u8 & 0x1f, function: address as u8 & 7 })
}

// Whether the service's regions hold the function at `bus:dev.func`.
pub fn firmware_held(bus: u8, dev: u8, func: u8) -> bool {
	STATE.lock().held.iter().any(|held| held.bus == bus && held.device == dev && held.function == func)
}

// The companion node `identity` names, as the function it describes - how a connection names a PCI controller. A
// device tree's node BELOW a function's node names that function too: its `virtio,device29` child is the virtio-gpio
// function's GPIO controller.
pub fn companion_function(identity: &[u8]) -> Option<(u8, u8, u8)> {
	let state = STATE.lock();
	let within = |companion: &&Companion| companion.path == identity || (identity.starts_with(b"dt:") && identity.len() > companion.path.len() && identity.starts_with(&companion.path) && identity[companion.path.len()] == b'/');
	state.companions.iter().find(within).map(|companion| (companion.function.bus, companion.function.device, companion.function.function))
}

// A DEVICE TREE'S PCI CHILD NODE, joined at the boot scan to the function its `reg` names - as the ACPI service joins a
// node with `_ADR`. Its path is the companion node every reader sees.
#[cfg(all(not(test), any(target_arch = "aarch64", target_arch = "riscv64")))]
pub fn tree_companion(identity: &[u8], bus: u8, dev: u8, func: u8) {
	let function = Function { segment: 0, bus, device: dev, function: func };
	crate::serial_println!("firmware: {} is the companion of {}", Text(identity), Bdf(function));
	let mut state = STATE.lock();
	state.companions.retain(|companion| companion.function != function);
	// ALLOC-OK: boot, a device-tree node joined to its function at the boot scan, before userspace exists.
	state.companions.push(Companion { function, path: identity.to_vec(), aei: List::default(), field_lines: List::default(), field_addresses: List::default() });
}

// ---------------------------------------------------------------------------------------------- the memory view

// The memory map as the policy reads it - None where the heap cannot hold it.
fn regions() -> Option<Vec<(u64, u64, MemoryKind)>> {
	heap::try_collect((0..crate::mem::memmap_len()).filter_map(crate::mem::memmap_get).filter(|region| region.length != 0).map(|region| (region.base, region.length, MemoryKind::from_memmap(region.kind))))
}

// A claimed row's ranges and the node that owns them: a platform row's own identity, or the companion node of a PCI
// function. None where the heap cannot hold them.
fn claim_ranges(entry: &crate::device::DeviceEntry, state: &State) -> Option<(Vec<(u64, u64)>, Vec<u8>)> {
	match entry.platform.as_ref() {
		Some(row) => Some((heap::try_collect(row.part.mmio().iter().map(|range| (range.base, range.len)))?, heap::try_to_vec(row.part.identity())?)),
		None => {
			let own = match state.companions.iter().find(|companion| companion.function.bus == entry.bus && companion.function.device == entry.dev && companion.function.function == entry.func) {
				Some(companion) => heap::try_to_vec(&companion.path)?,
				None => Vec::new(),
			};
			let mut ranges = heap::try_collect(state.decoded.iter().filter(|range| !range.window && range.function.bus == entry.bus && range.function.device == entry.dev && range.function.function == entry.func).map(|range| (range.base, range.len)))?;
			if entry.bar_len != 0 && !ranges.contains(&(entry.bar_phys, entry.bar_len)) {
				heap::try_push(&mut ranges, (entry.bar_phys, entry.bar_len)).ok()?;
			}
			Some((ranges, own))
		}
	}
}

// What a decision reads, built under the table's locks and this state's.
struct View {
	regions: Vec<(u64, u64, MemoryKind)>,
	kernel_held: Vec<(u64, u64)>,
	bars: Vec<Bar>,
	claimed: Vec<(u64, u64, Vec<u8>)>,
}

impl View {
	// None where the heap cannot hold what the decision reads - which every caller refuses, never decides without.
	fn build(tables: &crate::device::Tables<'_>, state: &State) -> Option<View> {
		let rows = tables.rows();
		let kernel_held = heap::try_collect(rows.iter().filter_map(|entry| entry.platform.as_ref()).filter(|row| row.part.state == abi::PLATFORM_STATE_KERNEL_HELD).flat_map(|row| row.part.mmio().iter().map(|range| (range.base, range.len))))?;
		let bars = heap::try_collect(state.decoded.iter().map(|range| {
			let row = tables.function_row(range.function.bus, range.function.device, range.function.function);
			let fixture = row.is_some_and(|index| rows[index].vendor == 0x1af4 && rows[index].product == 0x1110);
			Bar { base: range.base, len: range.len, function: range.function, window: range.window, driver_held: row.is_some_and(|index| tables.driver_held(index)), firmware_held: state.held.contains(&range.function), fixture }
		}))?;
		let mut claimed = Vec::new();
		for (index, entry) in rows.iter().enumerate() {
			if !tables.driver_held(index) {
				continue;
			}
			let (ranges, node) = claim_ranges(entry, state)?;
			for (base, len) in ranges {
				heap::try_push(&mut claimed, (base, len, heap::try_to_vec(&node)?)).ok()?;
			}
		}
		Some(View { regions: regions()?, kernel_held, bars, claimed })
	}

	fn with<R>(&self, f: impl FnOnce(&MemoryView<'_>) -> R) -> Option<R> {
		let claimed = heap::try_collect(self.claimed.iter().map(|(base, len, node)| Claimed { base: *base, len: *len, node }))?;
		Some(f(&MemoryView { regions: &self.regions, kernel_held: &self.kernel_held, bars: &self.bars, claimed: &claimed, carve_out: cfg!(liber_development) }))
	}
}

// ---------------------------------------------------------------------------------------------- the words

// WHAT THE CONSOLE IS TOLD, written with no allocation: these are said on the service's own calls, where a short heap
// must not turn a refusal into a halt.

// A PCI FUNCTION by its address - `segment:bus:device.function`.
pub(crate) struct Bdf(pub(crate) Function);

impl fmt::Display for Bdf {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{:04x}:{:02x}:{:02x}.{}", self.0.segment, self.0.bus, self.0.device, self.0.function)
	}
}

// A NODE'S NAME as its bytes say it: the valid UTF-8 as text, anything else as U+FFFD.
#[derive(Clone, Copy)]
pub(crate) struct Text<'a>(pub(crate) &'a [u8]);

impl fmt::Display for Text<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		for chunk in self.0.utf8_chunks() {
			f.write_str(chunk.valid())?;
			if !chunk.invalid().is_empty() {
				f.write_str("\u{FFFD}")?;
			}
		}
		Ok(())
	}
}

// A LIST'S VALUES, comma-separated, in hexadecimal.
struct ListText<'a>(&'a List);

impl fmt::Display for ListText<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		for (at, value) in self.0.as_slice().iter().enumerate() {
			if at != 0 {
				f.write_str(",")?;
			}
			write!(f, "{value:#x}")?;
		}
		Ok(())
	}
}

// WHY A REGION IS NOT MAPPED.
struct MapRefusal<'a>(policy::Refusal, &'a View);

impl fmt::Display for MapRefusal<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self.0 {
			policy::Refusal::Empty => f.write_str("it is empty"),
			policy::Refusal::Ram => f.write_str("it is RAM"),
			policy::Refusal::Mixed => f.write_str("it spans write-back and uncached memory"),
			policy::Refusal::KernelHeld => f.write_str("the kernel holds it"),
			policy::Refusal::Bar(function) => write!(f, "it is a BAR of {}", Bdf(function)),
			policy::Refusal::Window(function) => write!(f, "it is in the window of bridge {}", Bdf(function)),
			policy::Refusal::DriverHeld(function) => write!(f, "a driver holds {}, whose BAR it is", Bdf(function)),
			policy::Refusal::Claimed(at) => write!(f, "{} holds it through a claim", Text(&self.1.claimed[at].2)),
		}
	}
}

// WHY A `_CRS` CHECK REFUSES A ROW.
pub(crate) struct CrsWhy(pub(crate) policy::CrsRefusal);

impl fmt::Display for CrsWhy {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self.0 {
			policy::CrsRefusal::MmioOverRam => f.write_str("its MMIO lies over RAM"),
			policy::CrsRefusal::MmioKernelHeld => f.write_str("its MMIO is a range the kernel holds"),
			policy::CrsRefusal::MmioOverBar(function) => write!(f, "its MMIO lies over a BAR or window of {}", Bdf(function)),
			policy::CrsRefusal::PortsReserved => f.write_str("its ports are in the reserved set or granted"),
			policy::CrsRefusal::PortsOverBar => f.write_str("its ports are a PCI function's I/O BAR or a bridge's I/O window"),
			policy::CrsRefusal::KernelLine(line) => write!(f, "its line {line} is one the kernel uses"),
		}
	}
}

// WHY A CLAIM IS REFUSED: the node whose region maps the range is copied, as far as a platform name goes, so the answer
// outlives this module's lock with nothing allocated.
pub enum ClaimRefusal {
	FirmwareHeld,
	Mapped { node: [u8; abi::PLATFORM_NAME_LEN], node_len: usize, base: u64, last: u64 },
	// THE HEAP COULD NOT HOLD WHAT THE DECISION READS: refused, since a claim must never land under a region.
	NoMemory,
}

impl fmt::Display for ClaimRefusal {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			ClaimRefusal::FirmwareHeld => f.write_str("the ACPI service's regions hold it (firmware-held)"),
			ClaimRefusal::Mapped { node, node_len, base, last } => write!(f, "a region {} declares maps {base:#x}..{last:#x} of its ranges", Text(&node[..*node_len])),
			ClaimRefusal::NoMemory => f.write_str("there is no memory to check it against the ACPI service's regions"),
		}
	}
}

// ---------------------------------------------------------------------------------------------- processor power

// WHETHER PROCESSOR POWER MAY TAKE THE REGISTER at `base` of `len` bytes - see `procpower::admission`: not RAM, not a
// range the kernel holds for anything else, not inside a claim or a device's BAR - but for the fixture's BAR in a
// development build. Admitted, it is held: no region of the service's maps over it, and the caller maps it uncached.
pub fn admit_processor_memory(base: u64, len: u64) -> Result<(), &'static str> {
	crate::device::with_tables(|tables| {
		let mut state = STATE.lock();
		let Some(view) = View::build(tables, &state) else { return Err("there is no memory to check it") };
		let ram = heap::try_collect(view.regions.iter().filter(|(_, _, kind)| *kind == MemoryKind::Ram).map(|&(base, len, _)| (base, len)));
		let claimed = heap::try_collect(view.claimed.iter().map(|(base, len, _)| (*base, *len)));
		let bars = heap::try_collect(view.bars.iter().filter(|bar| !bar.window).map(|bar| procpower::admission::Bar { base: bar.base, len: bar.len, fixture: bar.fixture && bar.firmware_held }));
		let (Some(ram), Some(claimed), Some(bars)) = (ram, claimed, bars) else { return Err("there is no memory to check it") };
		if state.processor_held.try_reserve(1).is_err() {
			return Err("there is no memory to hold it");
		}
		let world = procpower::admission::World { reserved_ports: &[], granted_ports: &[], ram: &ram, kernel_held: &view.kernel_held, claimed: &claimed, bars: &bars, fixture_exception: cfg!(liber_development) };
		let register = procpower::Register { space: procpower::Space::SystemMemory, bits: (len * 8).min(64) as u8, address: base };
		match procpower::admission::admit(&register, &world) {
			Ok(admitted) => {
				if admitted == procpower::admission::Admitted::FixtureMemory {
					crate::serial_println!("firmware: processor power holds {base:#x}+{len} inside the fixture's BAR, under the development build's exception");
				}
				state.processor_held.push((base, len));
				Ok(())
			}
			Err(procpower::admission::Refused::Ram) => Err("it is RAM"),
			Err(procpower::admission::Refused::KernelHeld) => Err("the kernel holds it for something else"),
			Err(procpower::admission::Refused::Claimed) => Err("it is inside a claim"),
			Err(procpower::admission::Refused::Bar) => Err("it is inside a device's BAR"),
			Err(_) => Err("it is no memory register"),
		}
	})
}

// The matching release: one admission of `base` given back.
pub fn release_processor_memory(base: u64, len: u64) {
	let mut state = STATE.lock();
	if let Some(at) = state.processor_held.iter().position(|&held| held == (base, len)) {
		state.processor_held.swap_remove(at);
	}
}

// ---------------------------------------------------------------------------------------------- the calls

// MAP A SYSTEMMEMORY REGION `node` declares, under the policy: write-back for ACPI NVS, ACPI-reclaimable and
// firmware-reserved memory, uncached for MMIO; a BAR of the companion's function admitted makes that function
// firmware-held. The refusal is said on the console, naming the node, the range and the reason.
pub fn map(request: &abi::FirmwareMapRequest) -> Result<Arc<DeviceMemory>, i64> {
	let node = request.node.get(..request.node_len as usize).ok_or(abi::ERR_INVALID)?;
	if node.is_empty() || request.len == 0 || request.base.checked_add(request.len).is_none() || usize::try_from(request.len).is_err() {
		return Err(abi::ERR_INVALID);
	}
	let companion = function_of(request.companion);
	let name = Text(node);
	crate::device::with_tables(|tables| {
		let mut state = STATE.lock();
		let view = View::build(tables, &state).ok_or(abi::ERR_NO_MEMORY)?;
		// CARVED OUT FOR PROCESSOR POWER: a register a processor table names is never the service's too.
		if state.processor_held.iter().any(|&(base, len)| request.base < base.saturating_add(len) && base < request.base.saturating_add(request.len)) {
			crate::serial_println!("firmware: the region {name} declares at {:#x}..{:#x} is not mapped - processor power holds a register in it", request.base, request.base + request.len - 1);
			return Err(abi::ERR_ACCESS_DENIED);
		}
		let decided = view.with(|view| policy::system_memory(view, request.base, request.len, node, companion)).ok_or(abi::ERR_NO_MEMORY)?;
		let admission = match decided {
			Ok(admission) => admission,
			Err(refusal) => {
				crate::serial_println!("firmware: the region {name} declares at {:#x}..{:#x} is not mapped - {}", request.base, request.base + request.len - 1, MapRefusal(refusal, &view));
				return Err(abi::ERR_ACCESS_DENIED);
			}
		};
		// THE ROOM FIRST: what this mapping records is reserved before the object exists, so a short heap refuses it whole.
		let node_copy = heap::try_to_vec(node).ok_or(abi::ERR_NO_MEMORY)?;
		if state.held.try_reserve(1).is_err() || state.mappings.try_reserve(1).is_err() {
			return Err(abi::ERR_NO_MEMORY);
		}
		let write_back = admission == Admission::WriteBack;
		let object = DeviceMemory::for_firmware(request.base, request.len as usize, write_back).ok_or(abi::ERR_NO_MEMORY)?;
		if let Admission::FirmwareHold(function) = admission
			&& !state.held.contains(&function)
		{
			crate::serial_println!("firmware: {} is now FIRMWARE-HELD - the region {name} declares is in its BAR, and no driver may take it", Bdf(function));
			state.held.push(function);
			// ITS MEMORY DECODED, and nothing else: firmware that owns a function reaches its BAR, and the function never
			// masters the bus for it.
			let (bus, dev, func) = (function.bus, function.device, function.function);
			if let Some(command) = crate::arch::pci::config_read_exact(bus, dev, func, 0x04, 2)
				&& command & 0x2 == 0
			{
				let _ = crate::arch::pci::config_write_exact(bus, dev, func, 0x04, 2, command | 0x2);
			}
		}
		state.mappings.retain(|mapping| mapping.object.strong_count() != 0);
		state.mappings.push(Mapping { base: request.base, len: request.len, node: node_copy, object: Arc::downgrade(&object) });
		Ok(object)
	})
}

// WHY A CLAIM OF `entry` IS REFUSED, if it is: a function the service's regions hold, or a range another node's live
// region maps - "whichever comes first", so a claim never lands under a region and a region never over a claim.
// Called by `device::claim` under the table's locks.
pub fn claim_refusal(entry: &crate::device::DeviceEntry) -> Option<ClaimRefusal> {
	let mut state = STATE.lock();
	if entry.platform.is_none() && state.held.iter().any(|held| held.bus == entry.bus && held.device == entry.dev && held.function == entry.func) {
		return Some(ClaimRefusal::FirmwareHeld);
	}
	state.mappings.retain(|mapping| mapping.object.strong_count() != 0);
	let Some((ranges, own)) = claim_ranges(entry, &state) else { return Some(ClaimRefusal::NoMemory) };
	let Some(mappings) = heap::try_collect(state.mappings.iter().map(|mapping| (mapping.base, mapping.len, mapping.node.as_slice()))) else { return Some(ClaimRefusal::NoMemory) };
	let at = policy::claim_blocked(&ranges, &own, &mappings)?;
	let (base, len, held) = mappings[at];
	let mut node = [0u8; abi::PLATFORM_NAME_LEN];
	let node_len = held.len().min(node.len());
	node[..node_len].copy_from_slice(&held[..node_len]);
	Some(ClaimRefusal::Mapped { node, node_len, base, last: base + len - 1 })
}

// A PCI CONFIGURATION ACCESS the kernel performs for the service: any read; a write refused below 0x40, inside MSI and
// MSI-X, to a function a driver holds and to a register the chipset rows own.
pub fn pci(address: u64, access: u64, value: u64) -> i64 {
	let Some(function) = function_of(address) else { return abi::ERR_INVALID };
	if function.segment != 0 {
		return abi::ERR_UNSUPPORTED;
	}
	let offset = access as u16;
	let width = (access >> 16) as u8;
	let write = access & abi::FIRMWARE_PCI_WRITE != 0;
	let (bus, dev, func) = (function.bus, function.device, function.function);
	if !write {
		return match crate::arch::pci::config_read_exact(bus, dev, func, offset, width) {
			Some(read) => read as i64,
			None => abi::ERR_INVALID,
		};
	}
	let Ok(value) = u32::try_from(value) else { return abi::ERR_INVALID };
	let decided = crate::device::with_tables(|tables| {
		let identity = crate::arch::pci::config_read_exact(bus, dev, func, 0, 4).unwrap_or(u32::MAX);
		let (vendor, device) = (identity as u16, (identity >> 16) as u16);
		let msi = msi_ranges(bus, dev, func)?;
		let held = tables.function_row(bus, dev, func).is_some_and(|index| tables.driver_held(index));
		Some(policy::config_write(offset, width, &msi, held, crate::arch::firmware::chipset_registers(vendor, device)))
	});
	// NO ROOM TO READ THE CAPABILITIES: no write, since the one it might be is an MSI's.
	let Some(decided) = decided else { return abi::ERR_NO_MEMORY };
	match decided {
		Ok(()) => {
			if crate::arch::pci::config_write_exact(bus, dev, func, offset, width, value) {
				0
			} else {
				abi::ERR_INVALID
			}
		}
		Err(policy::ConfigRefusal::Width) => abi::ERR_INVALID,
		Err(refusal) => {
			crate::serial_println!("firmware: a configuration write to {bus:02x}:{dev:02x}.{func} at {offset:#x} is refused - {refusal:?}");
			abi::ERR_ACCESS_DENIED
		}
	}
}

// The byte ranges of a function's MSI and MSI-X capabilities, walked from its capability pointer - bounded, since a
// looping list is firmware's to get wrong. None where the heap cannot hold them.
fn msi_ranges(bus: u8, dev: u8, func: u8) -> Option<Vec<(u16, u16)>> {
	let mut out = Vec::new();
	let status = crate::arch::pci::config_read_exact(bus, dev, func, 0x06, 2).unwrap_or(0);
	if status & 0x10 == 0 {
		return Some(out);
	}
	let mut at = crate::arch::pci::config_read_exact(bus, dev, func, 0x34, 1).unwrap_or(0) as u16 & 0xFC;
	for _ in 0..48 {
		if at < 0x40 {
			break;
		}
		let header = crate::arch::pci::config_read_exact(bus, dev, func, at, 2).unwrap_or(0);
		match header & 0xFF {
			0x05 => {
				let control = crate::arch::pci::config_read_exact(bus, dev, func, at + 2, 2).unwrap_or(0);
				let wide = control & 0x80 != 0;
				let masked = control & 0x100 != 0;
				heap::try_push(&mut out, (at, 10 + if wide { 4 } else { 0 } + if masked { 10 } else { 0 })).ok()?;
			}
			0x11 => heap::try_push(&mut out, (at, 12)).ok()?,
			_ => {}
		}
		at = (header >> 8) as u16 & 0xFC;
	}
	Some(out)
}

// ATTACH THE CALLER AS THE RUNNING INSTANCE, its events on `channel`: a new number, the previous instance's reported
// set forgotten - its walk is the new instance's to report again.
pub fn attach(process: u64, channel: Arc<Channel>) -> u64 {
	let mut state = STATE.lock();
	state.instance += 1;
	state.holder = Some(process);
	state.events = Some(channel);
	state.reported.clear();
	crate::serial_println!("firmware: the ACPI service's instance {} is attached", state.instance);
	state.instance
}

// Whether `process` is the running instance - every report must come from it.
pub fn is_instance(process: u64) -> bool {
	STATE.lock().holder == Some(process)
}

// A PROCESS ENDED: if it was the running instance, every general-purpose event it enabled is disabled and its event
// channel dropped. Its mappings went with its handles; what it published stays.
pub fn process_ended(koid: u64) {
	let ended = {
		let mut state = STATE.lock();
		if state.holder != Some(koid) {
			return;
		}
		state.holder = None;
		state.events = None;
		state.mappings.retain(|mapping| mapping.object.strong_count() != 0);
		state.instance
	};
	crate::arch::firmware::gpe_instance_ended();
	crate::serial_println!("firmware: the ACPI service's instance {ended} ended - its general-purpose events are disabled; what it published stays");
}

// THE IDLE PASS'S DELIVERY - the shipping kernel's idle hook; a test build has neither the hook nor the SCI: every event the SCI handler latched, sent on the running instance's channel. Nothing is
// sent with no instance attached; the latch is dropped at the instance's end, and a new one enables its own.
#[cfg(not(test))]
pub fn deliver() {
	let Some(channel) = STATE.lock().events.clone() else { return };
	crate::arch::firmware::take_events(&mut |kind, gpe| {
		let mut bytes: Vec<u8> = Vec::new();
		if bytes.try_reserve_exact(3).is_err() {
			return;
		}
		bytes.push(kind);
		bytes.extend_from_slice(&gpe.to_le_bytes());
		if let Err(error) = channel.send(crate::object::channel::Message::new(bytes, Vec::new())) {
			crate::serial_println!("firmware: event {kind} ({gpe:#04x}) could not be delivered to the ACPI service ({error:?})");
		}
	});
}

// ---------------------------------------------------------------------------------------------- the reports

// The wired lines the kernel uses: its own and every kernel-held row's. None where the heap cannot hold them.
fn wired_lines_the_kernel_uses(tables: &crate::device::Tables<'_>) -> Option<Vec<u32>> {
	let (own, count) = crate::arch::firmware::kernel_lines();
	let mut lines = heap::try_to_vec(&own[..count])?;
	for row in tables.rows().iter().filter_map(|entry| entry.platform.as_ref()).filter(|row| row.part.state == abi::PLATFORM_STATE_KERNEL_HELD) {
		lines.try_reserve(row.part.lines().len()).ok()?;
		lines.extend(row.part.lines().iter().map(|line| line.number));
	}
	Some(lines)
}

// THE `_CRS` CHECK a namespace row is held to before it becomes a row.
fn admit(tables: &crate::device::Tables<'_>, description: &platform::Description) -> Result<(), crate::device::Unpublished> {
	let state = STATE.lock();
	let view = View::build(tables, &state).ok_or(crate::device::Unpublished::NoMemory)?;
	// EVERY FUNCTION'S I/O BARS AND EVERY BRIDGE'S I/O WINDOW.
	let mut io_bars = heap::try_collect(tables.rows().iter().filter(|entry| entry.platform.is_none()).flat_map(|entry| entry.ports[..entry.port_count as usize].iter().filter(|port| port.source == abi::PORT_SOURCE_IO_BAR).map(|port| (port.base, port.len)))).ok_or(crate::device::Unpublished::NoMemory)?;
	io_bars.try_reserve(state.io_windows.len()).map_err(|_| crate::device::Unpublished::NoMemory)?;
	io_bars.extend(state.io_windows.iter().copied());
	let lines = wired_lines_the_kernel_uses(tables).ok_or(crate::device::Unpublished::NoMemory)?;
	let checked = view.with(|view| policy::crs(description, view, |base, len| crate::object::port_range::grants::recordable(base, len).is_ok(), &io_bars, &lines)).ok_or(crate::device::Unpublished::NoMemory)?;
	checked.map_err(crate::device::Unpublished::Crs)
}

// The running instance's walk reported `identity`. False where the heap cannot hold it.
fn remember(state: &mut State, identity: &[u8]) -> bool {
	if state.reported.iter().any(|reported| reported == identity) {
		return true;
	}
	let Some(copy) = heap::try_to_vec(identity) else { return false };
	heap::try_push(&mut state.reported, copy).is_ok()
}

// A COMPANION'S LISTS, said after its join when it has any.
struct CompanionLists<'a>(&'a platform::report::CompanionReport<'a>);

impl fmt::Display for CompanionLists<'_> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		if self.0.aei_lines.count != 0 {
			write!(f, ", _AEI lines [{}]", ListText(&self.0.aei_lines))?;
		}
		if self.0.field_lines.count + self.0.field_addresses.count != 0 {
			write!(f, ", field lines [{}] addresses [{}]", ListText(&self.0.field_lines), ListText(&self.0.field_addresses))?;
		}
		Ok(())
	}
}

// ONE REPORT of the running instance's, decoded: answers the row it is about, or 0.
pub fn report(process: u64, bytes: &[u8]) -> i64 {
	if !is_instance(process) {
		return abi::ERR_UNSUPPORTED;
	}
	let Some(decoded) = platform::report::decode(bytes) else { return abi::ERR_INVALID };
	match decoded {
		Report::Device(mut device) => {
			attach_smbios(&mut device.description, device.properties);
			attach_stream(&mut device.description);
			// EVERYTHING THIS REPORT KEEPS, HELD FIRST: a short heap refuses the report whole, before anything is published.
			let Some(identity) = heap::try_to_vec(device.description.identity()) else { return abi::ERR_NO_MEMORY };
			let Some(properties) = heap::try_to_vec(device.properties) else { return abi::ERR_NO_MEMORY };
			let mut targets: Vec<(u8, Vec<u8>)> = Vec::new();
			if targets.try_reserve_exact(device.target_count).is_err() {
				return abi::ERR_NO_MEMORY;
			}
			for (at, target) in &device.targets[..device.target_count] {
				let Some(target) = heap::try_to_vec(target) else { return abi::ERR_NO_MEMORY };
				targets.push((*at, target));
			}
			{
				let mut state = STATE.lock();
				if !remember(&mut state, &identity) || state.parents.try_reserve(1).is_err() || state.merged.try_reserve(1).is_err() {
					return abi::ERR_NO_MEMORY;
				}
			}
			let name = Text(&identity);
			let published = crate::device::publish_namespace(device.description, properties, &targets, admit, companion_function);
			let mut state = STATE.lock();
			match published {
				Ok(crate::device::Published::Same(row)) => row as i64,
				Ok(crate::device::Published::Arrived(row)) => {
					state.parents.retain(|(at, _)| *at != row);
					if let Some(parent) = device.parent {
						// Reserved above - the one push this report makes to the list.
						heap::try_push(&mut state.parents, (row, parent)).ok();
					}
					crate::serial_println!("firmware: {name} is row {row}");
					row as i64
				}
				Ok(crate::device::Published::Merged(row, ids)) => {
					if !state.merged.iter().any(|merge| merge.row == row && merge.identity == identity) {
						let Some(copy) = heap::try_to_vec(&identity) else { return abi::ERR_NO_MEMORY };
						heap::try_push(&mut state.merged, Merge { row, identity: copy, ids }).ok();
					}
					row as i64
				}
				Err(why) => {
					crate::serial_println!("firmware: {name} is not published - {why}");
					abi::ERR_ACCESS_DENIED
				}
			}
		}
		Report::Withdraw(identity) => withdraw(identity),
		Report::Companion(companion) => {
			let function = companion.function;
			let Some(path) = heap::try_to_vec(companion.path) else { return abi::ERR_NO_MEMORY };
			let row = crate::device::with_tables(|tables| {
				let row = tables.function_row(function.bus, function.device, function.function);
				let mut state = STATE.lock();
				if !remember(&mut state, &path) || state.companions.try_reserve(1).is_err() {
					return Some(Err(abi::ERR_NO_MEMORY));
				}
				let row = row?;
				if function.segment != 0 {
					return None;
				}
				match state.companions.iter_mut().find(|held| held.function == function) {
					// THE SAME NODE JOINED TO THE SAME FUNCTION IS THAT COMPANION: no second attach and no event. Its
					// lines follow the latest walk.
					Some(held) if held.path == path => {
						held.aei = companion.aei_lines;
						held.field_lines = companion.field_lines;
						held.field_addresses = companion.field_addresses;
					}
					Some(held) => {
						crate::serial_println!("firmware: {} is described by {} now, not {} - the companion is replaced", Bdf(function), Text(&path), Text(&held.path));
						let Some(copy) = heap::try_to_vec(&path) else { return Some(Err(abi::ERR_NO_MEMORY)) };
						*held = Companion { function, path: copy, aei: companion.aei_lines, field_lines: companion.field_lines, field_addresses: companion.field_addresses };
					}
					None => {
						crate::serial_println!("firmware: {} is the companion of {} (row {row}){}", Text(&path), Bdf(function), CompanionLists(&companion));
						let Some(copy) = heap::try_to_vec(&path) else { return Some(Err(abi::ERR_NO_MEMORY)) };
						state.companions.push(Companion { function, path: copy, aei: companion.aei_lines, field_lines: companion.field_lines, field_addresses: companion.field_addresses });
					}
				}
				Some(Ok(row))
			});
			match row {
				Some(Ok(row)) => row as i64,
				Some(Err(error)) => error,
				None => {
					crate::serial_println!("firmware: {} names {}, which is no endpoint on the bus - not joined", Text(&path), Bdf(function));
					abi::ERR_INVALID
				}
			}
		}
		Report::Osc { segment, bus_start, bus_end, granted } => {
			{
				let mut state = STATE.lock();
				state.grants.retain(|grant| !(grant.0 == segment && grant.1 == bus_start && grant.2 == bus_end));
				if state.grants.try_reserve(1).is_err() {
					return abi::ERR_NO_MEMORY;
				}
				state.grants.push((segment, bus_start, bus_end, granted));
			}
			if segment == 0 && crate::arch::pci::apply_grant(bus_start, bus_end, granted) {
				#[cfg(not(test))]
				crate::arm_hot_plug_interrupts();
			}
			0
		}
		Report::Loaded { instance } => loaded(instance),
		Report::Lists(lists) => {
			let Some(identity) = heap::try_to_vec(lists.identity) else { return abi::ERR_NO_MEMORY };
			let mut state = STATE.lock();
			state.lists.retain(|(held, ..)| *held != identity);
			if state.lists.try_reserve(1).is_err() {
				return abi::ERR_NO_MEMORY;
			}
			crate::serial_println!("firmware: {} is a controller the service holds _AEI lines [{}], field lines [{}] and addresses [{}] through", Text(&identity), ListText(&lists.aei_lines), ListText(&lists.field_lines), ListText(&lists.field_addresses));
			state.lists.push((identity, lists.aei_lines, lists.field_lines, lists.field_addresses));
			0
		}
	}
}

// WITHDRAW WHAT `identity` NAMES: a namespace row, a merged description, a companion.
fn withdraw(identity: &[u8]) -> i64 {
	let name = Text(identity);
	let mut answered = false;
	if let Some(row) = crate::device::withdraw_namespace(identity) {
		crate::serial_println!("firmware: {name} (row {row}) is withdrawn");
		answered = true;
	}
	STATE.lock().reported.retain(|reported| reported != identity);
	// EACH MERGE AND EACH COMPANION IT NAMES, TAKEN OUT ONE AT A TIME - nothing is built to hold them, and the table's lock,
	// which `unmerge` takes, is never taken under this module's.
	loop {
		let merge = {
			let mut state = STATE.lock();
			let at = state.merged.iter().position(|merge| merge.identity == identity);
			at.map(|at| state.merged.remove(at))
		};
		let Some(merge) = merge else { break };
		crate::device::unmerge(merge.row, &merge.ids);
		crate::serial_println!("firmware: {name} is taken out of row {} - the row keeps its place", merge.row);
		answered = true;
	}
	loop {
		let companion = {
			let mut state = STATE.lock();
			let at = state.companions.iter().position(|companion| companion.path == identity);
			at.map(|at| state.companions.remove(at))
		};
		let Some(companion) = companion else { break };
		crate::serial_println!("firmware: {name} no longer describes {}", Bdf(companion.function));
		answered = true;
	}
	if answered { 0 } else { abi::ERR_INVALID }
}

// "NAMESPACE LOADED": everything the namespace published that this instance's walk did not report again is withdrawn,
// then DeviceManager is told, naming the instance.
fn loaded(instance: u64) -> i64 {
	if instance != STATE.lock().instance {
		return abi::ERR_INVALID;
	}
	// THE ROWS FIRST, without this module's lock - the table's is taken before it, never under it. Only the running
	// instance reports, and it is here, so what it reported does not move meanwhile.
	let Some(rows) = crate::device::namespace_rows() else { return abi::ERR_NO_MEMORY };
	let mut stale: Vec<Vec<u8>> = Vec::new();
	let reported = {
		let state = STATE.lock();
		if stale.try_reserve(rows.len() + state.merged.len() + state.companions.len()).is_err() {
			return abi::ERR_NO_MEMORY;
		}
		for (_, identity) in rows {
			if !state.reported.contains(&identity) {
				stale.push(identity);
			}
		}
		for held in state.merged.iter().map(|merge| &merge.identity).chain(state.companions.iter().map(|companion| &companion.path)) {
			if state.reported.contains(held) {
				continue;
			}
			let Some(copy) = heap::try_to_vec(held) else { return abi::ERR_NO_MEMORY };
			stale.push(copy);
		}
		state.reported.len()
	};
	for identity in &stale {
		let _ = withdraw(identity);
	}
	crate::serial_println!("firmware: the ACPI service's instance {instance} has loaded its namespace - {reported} reported, {} withdrawn", stale.len());
	crate::device::report(abi::DEVICE_EVENT_NAMESPACE_LOADED, instance as usize);
	0
}

// WHAT THE NAMESPACE ATTACHED TO ROW `index`, for DeviceManager: a PCI function's companion and its lists, a
// namespace child's parent function, and whether the function is firmware-held.
pub fn node(index: usize) -> Option<abi::FirmwareNode> {
	// THE ROW'S IDENTITY COPIED OUT, as long as a platform name goes - nothing allocated on the service's call.
	let mut name = [0u8; abi::PLATFORM_NAME_LEN];
	let (function, platform, name_len) = crate::device::with(index, |entry| {
		let name_len = entry.platform.as_ref().map(|row| {
			let identity = row.part.identity();
			let len = identity.len().min(name.len());
			name[..len].copy_from_slice(&identity[..len]);
			len
		});
		(Function { segment: 0, bus: entry.bus, device: entry.dev, function: entry.func }, entry.platform.is_some(), name_len)
	})?;
	let identity = name_len.map(|len| &name[..len]);
	let state = STATE.lock();
	let mut node = abi::FirmwareNode::default();
	// A CONTROLLER ROW'S LISTS, under its own identity.
	if let Some(identity) = identity
		&& let Some((_, aei, lines, addresses)) = state.lists.iter().find(|(held, ..)| held.as_slice() == identity)
	{
		let len = identity.len().min(abi::PLATFORM_NAME_LEN);
		node.flags |= abi::FIRMWARE_NODE_LISTS;
		node.path_len = len as u8;
		node.path[..len].copy_from_slice(&identity[..len]);
		for (list, (out, count)) in [
			(aei, (&mut node.aei, &mut node.aei_count)),
			(lines, (&mut node.field_lines, &mut node.field_line_count)),
			(addresses, (&mut node.field_addresses, &mut node.field_address_count)),
		] {
			let values = list.as_slice();
			let take = values.len().min(abi::FIRMWARE_NODE_LISTED);
			out[..take].copy_from_slice(&values[..take]);
			*count = take as u8;
		}
	}
	if !platform {
		if let Some(companion) = state.companions.iter().find(|companion| companion.function == function) {
			let len = companion.path.len().min(abi::PLATFORM_NAME_LEN);
			node.flags |= abi::FIRMWARE_NODE_COMPANION;
			node.path_len = len as u8;
			node.path[..len].copy_from_slice(&companion.path[..len]);
			for (list, (out, count)) in [
				(&companion.aei, (&mut node.aei, &mut node.aei_count)),
				(&companion.field_lines, (&mut node.field_lines, &mut node.field_line_count)),
				(&companion.field_addresses, (&mut node.field_addresses, &mut node.field_address_count)),
			] {
				let values = list.as_slice();
				let take = values.len().min(abi::FIRMWARE_NODE_LISTED);
				out[..take].copy_from_slice(&values[..take]);
				*count = take as u8;
			}
		}
		if state.held.contains(&function) {
			node.flags |= abi::FIRMWARE_NODE_FIRMWARE_HELD;
		}
	}
	if let Some((_, parent)) = state.parents.iter().find(|(row, _)| *row == index) {
		node.flags |= abi::FIRMWARE_NODE_PARENT;
		node.parent_segment = parent.segment;
		node.parent_bus = parent.bus;
		node.parent_dev = parent.device;
		node.parent_func = parent.function;
	}
	Some(node)
}

// THE SUITE'S RESET: no instance, nothing held, joined, merged or mapped - the rows the suite published stay, as every
// suite's do.
#[cfg(test)]
pub fn forget_for_test() {
	let mut state = STATE.lock();
	state.holder = None;
	state.events = None;
	state.mappings.clear();
	state.held.clear();
	state.companions.clear();
	state.parents.clear();
	state.merged.clear();
	state.reported.clear();
	state.lists.clear();
}
