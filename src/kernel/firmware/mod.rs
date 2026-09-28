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

use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use platform::policy::{self, Admission, Bar, Claimed, MemoryKind, MemoryView};
use platform::report::{Function, List, Report};

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
	mappings: Vec<Mapping>,
	held: Vec<Function>,
	companions: Vec<Companion>,
	parents: Vec<(usize, Function)>,
	merged: Vec<Merge>,
	// What the running instance's walk reported: identities of rows, reservations, merged descriptions, companions.
	reported: Vec<Vec<u8>>,
	grants: Vec<(u16, u8, u8, u32)>,
}

static STATE: SpinLock<State> = SpinLock::new(State { instance: 0, holder: None, events: None, decoded: Vec::new(), mappings: Vec::new(), held: Vec::new(), companions: Vec::new(), parents: Vec::new(), merged: Vec::new(), reported: Vec::new(), grants: Vec::new() });

// ON AN ACPI MACHINE, native hot-plug and error reporting wait for `_OSC`. Before the first scan; the test kernel
// keeps native control, as its fixtures are the bus's own.
pub fn init() {
	if !cfg!(test) && crate::arch::firmware::available() {
		crate::arch::pci::gate_on_osc();
	}
}

// THE BUS'S BARS AND WINDOWS, from the boot scan - every function's, resolved family or not.
pub fn record_decoded(ranges: Vec<crate::arch::common::pci::DecodedRange>) {
	let mut state = STATE.lock();
	state.decoded = ranges.iter().map(|range| Decoded { base: range.base, len: range.len, function: Function { segment: 0, bus: range.bus, device: range.dev, function: range.func }, window: range.window }).collect();
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

// The companion node `identity` names, as the function it describes - how a connection names a PCI controller.
pub fn companion_function(identity: &[u8]) -> Option<(u8, u8, u8)> {
	STATE.lock().companions.iter().find(|companion| companion.path == identity).map(|companion| (companion.function.bus, companion.function.device, companion.function.function))
}

// ---------------------------------------------------------------------------------------------- the memory view

// The memory map as the policy reads it.
fn regions() -> Vec<(u64, u64, MemoryKind)> {
	(0..crate::mem::memmap_len()).filter_map(crate::mem::memmap_get).filter(|region| region.length != 0).map(|region| (region.base, region.length, MemoryKind::from_memmap(region.kind))).collect()
}

// A claimed row's ranges and the node that owns them: a platform row's own identity, or the companion node of a PCI
// function.
fn claim_ranges(entry: &crate::device::DeviceEntry, state: &State) -> (Vec<(u64, u64)>, Vec<u8>) {
	match entry.platform.as_ref() {
		Some(row) => (row.part.mmio().iter().map(|range| (range.base, range.len)).collect(), row.part.identity().to_vec()),
		None => {
			let own = state.companions.iter().find(|companion| companion.function.bus == entry.bus && companion.function.device == entry.dev && companion.function.function == entry.func).map(|companion| companion.path.clone()).unwrap_or_default();
			let mut ranges: Vec<(u64, u64)> = state.decoded.iter().filter(|range| !range.window && range.function.bus == entry.bus && range.function.device == entry.dev && range.function.function == entry.func).map(|range| (range.base, range.len)).collect();
			if entry.bar_len != 0 && !ranges.contains(&(entry.bar_phys, entry.bar_len)) {
				ranges.push((entry.bar_phys, entry.bar_len));
			}
			(ranges, own)
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
	fn build(tables: &crate::device::Tables<'_>, state: &State) -> View {
		let rows = tables.rows();
		let kernel_held: Vec<(u64, u64)> = rows.iter().filter_map(|entry| entry.platform.as_ref()).filter(|row| row.part.state == abi::PLATFORM_STATE_KERNEL_HELD).flat_map(|row| row.part.mmio().iter().map(|range| (range.base, range.len))).collect();
		let bars = state
			.decoded
			.iter()
			.map(|range| {
				let row = tables.function_row(range.function.bus, range.function.device, range.function.function);
				let fixture = row.is_some_and(|index| rows[index].vendor == 0x1af4 && rows[index].product == 0x1110);
				Bar { base: range.base, len: range.len, function: range.function, window: range.window, driver_held: row.is_some_and(|index| tables.driver_held(index)), firmware_held: state.held.contains(&range.function), fixture }
			})
			.collect();
		let mut claimed = Vec::new();
		for (index, entry) in rows.iter().enumerate() {
			if !tables.driver_held(index) {
				continue;
			}
			let (ranges, node) = claim_ranges(entry, state);
			for (base, len) in ranges {
				claimed.push((base, len, node.clone()));
			}
		}
		View { regions: regions(), kernel_held, bars, claimed }
	}

	fn with<R>(&self, f: impl FnOnce(&MemoryView<'_>) -> R) -> R {
		let claimed: Vec<Claimed<'_>> = self.claimed.iter().map(|(base, len, node)| Claimed { base: *base, len: *len, node }).collect();
		f(&MemoryView { regions: &self.regions, kernel_held: &self.kernel_held, bars: &self.bars, claimed: &claimed, carve_out: cfg!(liber_development) })
	}
}

fn function_text(function: Function) -> String {
	alloc::format!("{:04x}:{:02x}:{:02x}.{}", function.segment, function.bus, function.device, function.function)
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
	let name = String::from_utf8_lossy(node).into_owned();
	crate::device::with_tables(|tables| {
		let mut state = STATE.lock();
		let view = View::build(tables, &state);
		let decided = view.with(|view| policy::system_memory(view, request.base, request.len, node, companion));
		let admission = match decided {
			Ok(admission) => admission,
			Err(refusal) => {
				let why = match refusal {
					policy::Refusal::Empty => String::from("it is empty"),
					policy::Refusal::Ram => String::from("it is RAM"),
					policy::Refusal::Mixed => String::from("it spans write-back and uncached memory"),
					policy::Refusal::KernelHeld => String::from("the kernel holds it"),
					policy::Refusal::Bar(function) => alloc::format!("it is a BAR of {}", function_text(function)),
					policy::Refusal::Window(function) => alloc::format!("it is in the window of bridge {}", function_text(function)),
					policy::Refusal::DriverHeld(function) => alloc::format!("a driver holds {}, whose BAR it is", function_text(function)),
					policy::Refusal::Claimed(at) => alloc::format!("{} holds it through a claim", String::from_utf8_lossy(&view.claimed[at].2)),
				};
				crate::serial_println!("firmware: the region {name} declares at {:#x}..{:#x} is not mapped - {why}", request.base, request.base + request.len - 1);
				return Err(abi::ERR_ACCESS_DENIED);
			}
		};
		let write_back = admission == Admission::WriteBack;
		let object = DeviceMemory::for_firmware(request.base, request.len as usize, write_back).ok_or(abi::ERR_NO_MEMORY)?;
		if let Admission::FirmwareHold(function) = admission
			&& !state.held.contains(&function)
		{
			crate::serial_println!("firmware: {} is now FIRMWARE-HELD - the region {name} declares is in its BAR, and no driver may take it", function_text(function));
			state.held.push(function);
		}
		state.mappings.retain(|mapping| mapping.object.strong_count() != 0);
		state.mappings.push(Mapping { base: request.base, len: request.len, node: node.to_vec(), object: Arc::downgrade(&object) });
		Ok(object)
	})
}

// WHY A CLAIM OF `entry` IS REFUSED, if it is: a function the service's regions hold, or a range another node's live
// region maps - "whichever comes first", so a claim never lands under a region and a region never over a claim.
// Called by `device::claim` under the table's locks.
pub fn claim_refusal(entry: &crate::device::DeviceEntry) -> Option<String> {
	let mut state = STATE.lock();
	if entry.platform.is_none() && state.held.iter().any(|held| held.bus == entry.bus && held.device == entry.dev && held.function == entry.func) {
		return Some(String::from("the ACPI service's regions hold it (firmware-held)"));
	}
	state.mappings.retain(|mapping| mapping.object.strong_count() != 0);
	let (ranges, own) = claim_ranges(entry, &state);
	let mappings: Vec<(u64, u64, &[u8])> = state.mappings.iter().map(|mapping| (mapping.base, mapping.len, mapping.node.as_slice())).collect();
	let at = policy::claim_blocked(&ranges, &own, &mappings)?;
	Some(alloc::format!("a region {} declares maps {:#x}..{:#x} of its ranges", String::from_utf8_lossy(mappings[at].2), mappings[at].0, mappings[at].0 + mappings[at].1 - 1))
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
		let msi = msi_ranges(bus, dev, func);
		let held = tables.function_row(bus, dev, func).is_some_and(|index| tables.driver_held(index));
		policy::config_write(offset, width, &msi, held, crate::arch::firmware::chipset_registers(vendor, device))
	});
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
// looping list is firmware's to get wrong.
fn msi_ranges(bus: u8, dev: u8, func: u8) -> Vec<(u16, u16)> {
	let mut out = Vec::new();
	let status = crate::arch::pci::config_read_exact(bus, dev, func, 0x06, 2).unwrap_or(0);
	if status & 0x10 == 0 {
		return out;
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
				out.push((at, 10 + if wide { 4 } else { 0 } + if masked { 10 } else { 0 }));
			}
			0x11 => out.push((at, 12)),
			_ => {}
		}
		at = (header >> 8) as u16 & 0xFC;
	}
	out
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

fn wired_lines_the_kernel_uses(tables: &crate::device::Tables<'_>) -> Vec<u32> {
	let mut lines = crate::arch::firmware::kernel_lines();
	for row in tables.rows().iter().filter_map(|entry| entry.platform.as_ref()).filter(|row| row.part.state == abi::PLATFORM_STATE_KERNEL_HELD) {
		lines.extend(row.part.lines().iter().map(|line| line.number));
	}
	lines
}

// THE `_CRS` CHECK a namespace row is held to before it becomes a row.
fn admit(tables: &crate::device::Tables<'_>, description: &platform::Description) -> Result<(), String> {
	let state = STATE.lock();
	let view = View::build(tables, &state);
	let io_bars: Vec<(u16, u16)> = tables.rows().iter().filter(|entry| entry.platform.is_none()).flat_map(|entry| entry.ports[..entry.port_count as usize].iter().filter(|port| port.source == abi::PORT_SOURCE_IO_BAR).map(|port| (port.base, port.len))).collect();
	let lines = wired_lines_the_kernel_uses(tables);
	let checked = view.with(|view| policy::crs(description, view, |base, len| crate::object::port_range::grants::recordable(base, len).is_ok(), &io_bars, &lines));
	checked.map_err(|refusal| match refusal {
		policy::CrsRefusal::MmioOverRam => String::from("its MMIO lies over RAM"),
		policy::CrsRefusal::MmioKernelHeld => String::from("its MMIO is a range the kernel holds"),
		policy::CrsRefusal::MmioOverBar(function) => alloc::format!("its MMIO lies over a BAR or window of {}", function_text(function)),
		policy::CrsRefusal::PortsReserved => String::from("its ports are in the reserved set or granted"),
		policy::CrsRefusal::PortsOverBar => String::from("its ports are a PCI function's I/O BAR"),
		policy::CrsRefusal::KernelLine(line) => alloc::format!("its line {line} is one the kernel uses"),
	})
}

fn remember(state: &mut State, identity: &[u8]) {
	if !state.reported.iter().any(|reported| reported == identity) {
		state.reported.push(identity.to_vec());
	}
}

fn list_text(list: &List) -> String {
	let mut out = String::new();
	for (at, value) in list.as_slice().iter().enumerate() {
		if at != 0 {
			out.push(',');
		}
		out.push_str(&alloc::format!("{value:#x}"));
	}
	out
}

// ONE REPORT of the running instance's, decoded: answers the row it is about, or 0.
pub fn report(process: u64, bytes: &[u8]) -> i64 {
	if !is_instance(process) {
		return abi::ERR_UNSUPPORTED;
	}
	let Some(decoded) = platform::report::decode(bytes) else { return abi::ERR_INVALID };
	match decoded {
		Report::Device(device) => {
			let identity = device.description.identity().to_vec();
			let name = String::from_utf8_lossy(&identity).into_owned();
			let targets: Vec<(u8, Vec<u8>)> = device.targets[..device.target_count].iter().map(|(at, target)| (*at, target.to_vec())).collect();
			let published = crate::device::publish_namespace(device.description, device.properties.to_vec(), &targets, admit, companion_function);
			let mut state = STATE.lock();
			remember(&mut state, &identity);
			match published {
				Ok(crate::device::Published::Same(row)) => row as i64,
				Ok(crate::device::Published::Arrived(row)) => {
					state.parents.retain(|(at, _)| *at != row);
					if let Some(parent) = device.parent {
						state.parents.push((row, parent));
					}
					crate::serial_println!("firmware: {name} is row {row}");
					row as i64
				}
				Ok(crate::device::Published::Merged(row, ids)) => {
					if !state.merged.iter().any(|merge| merge.row == row && merge.identity == identity) {
						state.merged.push(Merge { row, identity, ids });
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
			let path = companion.path.to_vec();
			let row = crate::device::with_tables(|tables| {
				let row = tables.function_row(function.bus, function.device, function.function);
				let mut state = STATE.lock();
				remember(&mut state, &path);
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
						crate::serial_println!("firmware: {} is described by {} now, not {} - the companion is replaced", function_text(function), String::from_utf8_lossy(&path), String::from_utf8_lossy(&held.path));
						*held = Companion { function, path: path.clone(), aei: companion.aei_lines, field_lines: companion.field_lines, field_addresses: companion.field_addresses };
					}
					None => {
						crate::serial_println!("firmware: {} is the companion of {} (row {row}){}{}", String::from_utf8_lossy(&path), function_text(function), if companion.aei_lines.count != 0 { alloc::format!(", _AEI lines [{}]", list_text(&companion.aei_lines)) } else { String::new() }, if companion.field_lines.count + companion.field_addresses.count != 0 { alloc::format!(", field lines [{}] addresses [{}]", list_text(&companion.field_lines), list_text(&companion.field_addresses)) } else { String::new() });
						state.companions.push(Companion { function, path: path.clone(), aei: companion.aei_lines, field_lines: companion.field_lines, field_addresses: companion.field_addresses });
					}
				}
				Some(row)
			});
			match row {
				Some(row) => row as i64,
				None => {
					crate::serial_println!("firmware: {} names {}, which is no endpoint on the bus - not joined", String::from_utf8_lossy(&path), function_text(function));
					abi::ERR_INVALID
				}
			}
		}
		Report::Osc { segment, bus_start, bus_end, granted } => {
			{
				let mut state = STATE.lock();
				state.grants.retain(|grant| !(grant.0 == segment && grant.1 == bus_start && grant.2 == bus_end));
				state.grants.push((segment, bus_start, bus_end, granted));
			}
			if segment == 0 && crate::arch::pci::apply_grant(bus_start, bus_end, granted) {
				#[cfg(not(test))]
				crate::arm_hot_plug_interrupts();
			}
			0
		}
		Report::Loaded { instance } => loaded(instance),
	}
}

// WITHDRAW WHAT `identity` NAMES: a namespace row, a merged description, a companion.
fn withdraw(identity: &[u8]) -> i64 {
	let name = String::from_utf8_lossy(identity).into_owned();
	let mut answered = false;
	if let Some(row) = crate::device::withdraw_namespace(identity) {
		crate::serial_println!("firmware: {name} (row {row}) is withdrawn");
		answered = true;
	}
	let (merges, companions): (Vec<Merge>, Vec<Companion>) = {
		let mut state = STATE.lock();
		state.reported.retain(|reported| reported != identity);
		let (merges, kept): (Vec<Merge>, Vec<Merge>) = core::mem::take(&mut state.merged).into_iter().partition(|merge| merge.identity == identity);
		state.merged = kept;
		let (companions, kept): (Vec<Companion>, Vec<Companion>) = core::mem::take(&mut state.companions).into_iter().partition(|companion| companion.path == identity);
		state.companions = kept;
		(merges, companions)
	};
	for merge in merges {
		crate::device::unmerge(merge.row, &merge.ids);
		crate::serial_println!("firmware: {name} is taken out of row {} - the row keeps its place", merge.row);
		answered = true;
	}
	for companion in companions {
		crate::serial_println!("firmware: {name} no longer describes {}", function_text(companion.function));
		answered = true;
	}
	if answered { 0 } else { abi::ERR_INVALID }
}

// "NAMESPACE LOADED": everything the namespace published that this instance's walk did not report again is withdrawn,
// then DeviceManager is told, naming the instance.
fn loaded(instance: u64) -> i64 {
	let (current, reported) = {
		let state = STATE.lock();
		(state.instance, state.reported.clone())
	};
	if instance != current {
		return abi::ERR_INVALID;
	}
	let mut stale: Vec<Vec<u8>> = crate::device::namespace_rows().into_iter().map(|(_, identity)| identity).filter(|identity| !reported.contains(identity)).collect();
	{
		let state = STATE.lock();
		stale.extend(state.merged.iter().map(|merge| merge.identity.clone()).filter(|identity| !reported.contains(identity)));
		stale.extend(state.companions.iter().map(|companion| companion.path.clone()).filter(|identity| !reported.contains(identity)));
	}
	for identity in &stale {
		let _ = withdraw(identity);
	}
	crate::serial_println!("firmware: the ACPI service's instance {instance} has loaded its namespace - {} reported, {} withdrawn", reported.len(), stale.len());
	crate::device::report(abi::DEVICE_EVENT_NAMESPACE_LOADED, instance as usize);
	0
}

// WHAT THE NAMESPACE ATTACHED TO ROW `index`, for DeviceManager: a PCI function's companion and its lists, a
// namespace child's parent function, and whether the function is firmware-held.
pub fn node(index: usize) -> Option<abi::FirmwareNode> {
	let (function, platform) = crate::device::with(index, |entry| (Function { segment: 0, bus: entry.bus, device: entry.dev, function: entry.func }, entry.platform.is_some()))?;
	let state = STATE.lock();
	let mut node = abi::FirmwareNode::default();
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
}
