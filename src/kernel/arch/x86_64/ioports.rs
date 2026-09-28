// Port I/O as a capability, the x86_64 half: the permission bitmap each core loads, the sources a row's
// port resources come from, and the FADT's part of the reserved set.
//
// ENFORCEMENT IS THE PROCESSOR'S. Every core's TSS carries an 8 KiB I/O permission bitmap (`gdt`), and
// ring 3 may execute `in` or `out` only for ports whose bits are clear there. IOPL stays zero by
// construction - ring 3 is entered only with the kernel's fixed initial flags or the flags the processor
// saved on entry, and `popf` at CPL 3 cannot change IOPL - so the bitmap is the whole of it.
//
// ON EVERY SWITCH TO A THREAD the core compares the incoming process and that process's generation with
// what it loaded last. A process with no mapped range gets the map base past the TSS limit, which refuses
// every port and copies nothing; otherwise the words up to the process's highest one are copied (two for
// COM1) and every word past them that an earlier copy wrote is set back to all ones.
//
// REVOCATION IS PUSHED: `port_range` clears the bits and runs one round of the TLB shootdown's request and
// acknowledgement, and each core's `service` step - which reads ONLY this core's record, never
// `current_thread`, so it takes no lock - copies again when the running process's generation moved.
// GRANTS ARE PULLED: a sibling thread already running on another core has not loaded a range its process
// just mapped, so a ring-3 #GP in a process whose generation moved copies again and retries the
// instruction; only a #GP against a current bitmap ends the process.

use super::{gdt, pci, percpu};
use crate::object::process::Process;

// How many times one copy retries a change that ran under it. Past this the switch and the service step
// load the refuse-everything state and record nothing loaded, and the #GP path returns without copying -
// all safe, because a grant not yet loaded costs one more #GP, whose path copies again.
const COPY_ATTEMPTS: usize = 16;

pub fn supported() -> bool {
	true
}

// The running core's record. Every caller runs with interrupts masked on this core.
#[inline(always)]
fn record() -> &'static mut percpu::IoCore {
	// SAFETY: this core's own block, reached with interrupts masked, so nothing else on this core can hold
	// a reference into it - and no other core ever touches it.
	unsafe { &mut *percpu::io_core() }
}

#[inline(always)]
fn stale(io: &percpu::IoCore, process: &Process) -> bool {
	let ports = process.io_ports();
	io.koid != ports.owner() || io.generation != ports.generation()
}

#[inline(always)]
fn set_map_base(io: &percpu::IoCore, value: u16) {
	// SAFETY: `base` is this core's TSS I/O map base field, recorded at bring-up.
	unsafe { (io.base as *mut u16).write_volatile(value) };
}

// COPY `process`'s bitmap into this core's TSS, consistently with one generation of it. Answers whether a
// copy was loaded.
fn load(io: &mut percpu::IoCore, process: &Process) -> bool {
	let ports = process.io_ports();
	let map = io.map as *mut u64;
	for _ in 0..COPY_ATTEMPTS {
		let Some((generation, words)) = ports.begin_read() else {
			core::hint::spin_loop();
			continue;
		};
		if words == 0 {
			// NO PORT ALLOWED: the map base past the limit, and nothing copied.
			if !ports.end_read(generation) {
				continue;
			}
			set_map_base(io, gdt::IOMAP_REFUSE);
			io.koid = ports.owner();
			io.generation = generation;
			return true;
		}
		// Counted before the words are written, so an attempt abandoned half way still leaves the next
		// copy knowing how far it has to restore.
		io.dirty = io.dirty.max(words as u64);
		for at in 0..words {
			// SAFETY: `map` is this core's bitmap, `IO_BITMAP_BYTES` long and eight-byte aligned, and
			// `words` is at most its length in words.
			unsafe { map.add(at).write_volatile(ports.word(at)) };
		}
		if !ports.end_read(generation) {
			continue;
		}
		for at in words..io.dirty as usize {
			// SAFETY: as above; `dirty` never exceeds the bitmap's length in words.
			unsafe { map.add(at).write_volatile(u64::MAX) };
		}
		io.dirty = words as u64;
		set_map_base(io, gdt::IOMAP_OFFSET);
		io.koid = ports.owner();
		io.generation = generation;
		return true;
	}
	set_map_base(io, gdt::IOMAP_REFUSE);
	io.koid = 0;
	io.generation = 0;
	false
}

// THE SWITCH: the incoming thread's process becomes this core's running process, and its bitmap is loaded
// unless it already is. Called by the scheduler with interrupts masked.
#[inline(always)]
pub fn switch_in(process: &Process) {
	let io = record();
	io.running = process as *const Process as u64;
	if stale(io, process) {
		load(io, process);
	}
}

// The idle context runs nothing in ring 3: the running process is forgotten. What was loaded stays
// recorded, so a switch straight back to the same process copies nothing.
#[inline(always)]
pub fn switch_to_idle() {
	record().running = 0;
}

// THE SERVICE STEP of a revocation round, from `tlb::service_pending` with interrupts masked: copy again
// when the running process's generation moved. It reads only this core's record.
pub fn service() {
	// Masked here as well: the idle loops call the service step with interrupts on, and a switch on this
	// core must not land between reading the record and writing it.
	let was_enabled = super::interrupts_enabled();
	super::disable_interrupts();
	let io = record();
	if io.running != 0 {
		// SAFETY: `running` is set only while one of this process's threads is current on this core, and
		// a thread holds its process alive.
		let process = unsafe { &*(io.running as *const Process) };
		if stale(io, process) {
			load(io, process);
		}
	}
	if was_enabled {
		super::enable_interrupts();
	}
}

// After a change on this core's own behalf: copy again at once if this core is running the process, so
// the round that follows only has the other cores to ask.
pub fn reload_if_running(process: &Process) {
	let was_enabled = super::interrupts_enabled();
	super::disable_interrupts();
	let io = record();
	if io.running == process as *const Process as u64 && stale(io, process) {
		load(io, process);
	}
	if was_enabled {
		super::enable_interrupts();
	}
}

// THE PULL, from a ring-3 #GP: when this core is behind the running process's generation - a range mapped
// after this core loaded, by a sibling thread elsewhere - copy again and let the instruction retry.
// Answers true when the fault should be retried; false when the bitmap is current, and the fault stands.
pub fn pull_on_gp() -> bool {
	let io = record();
	if io.running == 0 {
		return false;
	}
	// SAFETY: as in `service` - the fault came from this process's thread, current on this core.
	let process = unsafe { &*(io.running as *const Process) };
	if !stale(io, process) {
		return false;
	}
	load(io, process);
	true
}

// ---------------------------------------------------------------- where a row's ports come from

// THE FADT'S PART OF THE RESERVED SET, recorded before the boot scan in both builds: every fixed-hardware
// block in system I/O at its declared length - the PM1 event and control blocks, PM2 control, the PM
// timer, both GPE blocks - and the SMI command port. Into `out`, answering how many.
pub fn firmware_blocks(out: &mut [(u16, u16); 16]) -> usize {
	let Some(fadt) = fadt() else { return 0 };
	let mut count = 0usize;
	let mut push = |port: Option<u16>, len: u16| {
		if let Some(port) = port
			&& len != 0
			&& count < out.len()
		{
			out[count] = (port, len);
			count += 1;
		}
	};
	for block in [fadt.pm1a_event(), fadt.pm1b_event(), fadt.pm1a_control(), fadt.pm1b_control(), fadt.pm2_control()].into_iter().flatten() {
		push(block.io_port(), block.declared_bytes as u16);
	}
	if let Some(timer) = fadt.timer() {
		push(timer.block.io_port(), timer.block.declared_bytes as u16);
	}
	for gpe in [fadt.gpe0(), fadt.gpe1()].into_iter().flatten() {
		push(gpe.block.io_port(), gpe.block.declared_bytes as u16);
	}
	push(fadt.smi_command().and_then(|port| u16::try_from(port).ok()), 1);
	count
}

fn fadt() -> Option<acpi::Fadt<'static>> {
	let bytes = crate::smp::acpi_table(crate::boot_info().rsdp, b"FACP")?;
	acpi::Fadt::new(bytes).ok()
}

// A CHIPSET SUB-RANGE DERIVATION: for the function `vendor:device`, the configuration register holding a
// block's base (offset and mask), the block's length, the sub-ranges (offset, length) that go with the
// function's claim, and when the row applies - a decode-enable bit that must be set, a FADT block the
// base must agree with (at an offset into the block), and an ACPI table whose presence suppresses the
// row. A sub-range NEVER toggles the function's decode: the block is the chipset's, and the kernel's own
// registers live in it.
struct Derivation {
	name: &'static str,
	vendor: u16,
	device: u16,
	base_register: u16,
	base_mask: u32,
	block_len: u16,
	sub_ranges: &'static [(u16, u16)],
	decode_enable: Option<(u16, u32)>,
	// Which FADT block, read by its accessor, and at what offset into the block it sits.
	agrees_with: Option<(fn(&acpi::Fadt<'static>) -> Option<acpi::Block>, u16)>,
	suppressed_by: Option<[u8; 4]>,
}

// THE ICH9 LPC BRIDGE'S TCO WATCHDOG - the table's first production row, the watchdog milestone's. Over q35's ICH9
// LPC bridge and its ACPI PM block (PMBASE, config 0x40, bits 15:7; ACPI_EN, config 0x44 bit 7; the block agreeing
// with the FADT's PM1a event block at its start), EXACTLY PM base + 0x60..0x7F and nothing else of the block: PM1,
// the PM timer, GPE0 and SMI_EN stay the kernel's. WHEN A WDAT IS PRESENT THE ROW IS NOT APPLIED - the ACPI
// watchdog wins, as it does in Linux's `lpc_ich` - and the function's claim is refused (`crate::declared`).
const ICH9_TCO: Derivation = Derivation { name: "ich9-tco", vendor: 0x8086, device: 0x2918, base_register: 0x40, base_mask: 0xFF80, block_len: 128, sub_ranges: &[(0x60, 32)], decode_enable: Some((0x44, 0x80)), agrees_with: Some((acpi::Fadt::pm1a_event, 0)), suppressed_by: Some(*b"WDAT") };

#[cfg(not(test))]
const DERIVATIONS: &[Derivation] = &[ICH9_TCO];

// The test build adds one row over the same block whose sub-range is PM1's, which the reserved set refuses at the
// boot scan.
#[cfg(test)]
const DERIVATIONS: &[Derivation] = &[
	ICH9_TCO,
	Derivation { name: "test:ich9-pm1", vendor: 0x8086, device: 0x2918, base_register: 0x40, base_mask: 0xFF80, block_len: 128, sub_ranges: &[(0x00, 4)], decode_enable: Some((0x44, 0x80)), agrees_with: Some((acpi::Fadt::pm1a_event, 0)), suppressed_by: None },
];

// EVERY PORT RESOURCE A PCI FUNCTION'S ROW WOULD RECORD: its I/O BARs, then the sub-ranges every
// derivation row for it yields. Candidates only - the caller checks each against the reserved set and the
// live grants when it records it, and refuses with a line. Into `out`, answering how many.
pub fn function_ports(bus: u8, dev: u8, func: u8, vendor: u16, product: u16, out: &mut [abi::PortResource; abi::MAX_PORT_RESOURCES]) -> usize {
	let mut count = 0usize;
	let mut bars = [(0u8, 0u16, 0u16); 6];
	let found = pci::io_bars(bus, dev, func, &mut bars);
	for &(index, base, len) in &bars[..found] {
		if count == out.len() {
			break;
		}
		out[count] = abi::PortResource { base, len, source: abi::PORT_SOURCE_IO_BAR, index, _pad: [0; 2] };
		count += 1;
	}
	for row in DERIVATIONS.iter().filter(|row| row.vendor == vendor && row.device == product) {
		let Some(base) = derive_base(row, bus, dev, func) else { continue };
		for (at, &(offset, len)) in row.sub_ranges.iter().enumerate() {
			if offset as u32 + len as u32 > row.block_len as u32 || base as u32 + offset as u32 + len as u32 > 0x1_0000 {
				crate::serial_println!("ports: derivation {} for {bus:02x}:{dev:02x}.{func} has a sub-range outside its block - not recorded", row.name);
				continue;
			}
			if count == out.len() {
				crate::serial_println!("ports: {bus:02x}:{dev:02x}.{func} has more port resources than a row holds - {} sub-range {at} not recorded", row.name);
				break;
			}
			out[count] = abi::PortResource { base: base + offset, len, source: abi::PORT_SOURCE_DERIVED, index: at as u8, _pad: [0; 2] };
			count += 1;
		}
	}
	count
}

// A derivation row's block base, or `None` - with a line - when a condition refuses the row.
fn derive_base(row: &Derivation, bus: u8, dev: u8, func: u8) -> Option<u16> {
	if let Some(signature) = row.suppressed_by
		&& crate::smp::acpi_table(crate::boot_info().rsdp, &signature).is_some()
	{
		crate::serial_println!("ports: derivation {} for {bus:02x}:{dev:02x}.{func} is suppressed by this machine's {} table", row.name, core::str::from_utf8(&signature).unwrap_or("?"));
		return None;
	}
	if let Some((offset, mask)) = row.decode_enable
		&& pci::config_read32(bus, dev, func, offset) & mask == 0
	{
		crate::serial_println!("ports: derivation {} for {bus:02x}:{dev:02x}.{func} refused - the block's decode is not enabled", row.name);
		return None;
	}
	let raw = pci::config_read32(bus, dev, func, row.base_register) & row.base_mask;
	let Ok(base) = u16::try_from(raw) else {
		crate::serial_println!("ports: derivation {} for {bus:02x}:{dev:02x}.{func} refused - its base {raw:#x} is no port", row.name);
		return None;
	};
	if base == 0 {
		crate::serial_println!("ports: derivation {} for {bus:02x}:{dev:02x}.{func} refused - its block has no base", row.name);
		return None;
	}
	if let Some((block, offset)) = row.agrees_with {
		let firmware = fadt().and_then(|fadt| block(&fadt));
		let agrees = firmware.and_then(|block| block.io_port()).is_some_and(|port| port as u32 == base as u32 + offset as u32);
		if !agrees {
			crate::serial_println!("ports: derivation {} for {bus:02x}:{dev:02x}.{func} refused - its base {base:#06x} does not agree with the FADT", row.name);
			return None;
		}
	}
	Some(base)
}

// A function's I/O decode: on for a claim of a row with an I/O BAR, off at its release.
pub fn set_io_decode(bus: u8, dev: u8, func: u8, on: bool) {
	pci::set_io_decode(bus, dev, func, on);
}
