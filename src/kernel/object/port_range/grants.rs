// WHO MAY BE GRANTED WHICH PORT: the reserved set, the live grants, and the ports a failed revocation
// took out of circulation for the rest of the boot.
//
// ONE TABLE UNDER ONE LOCK, because every question here is "is this port free" and every answer has to
// be taken together with the act it permits. A mint checks the reserved set and the live grants and
// inserts its own grant in one step; a run-time install checks the live grants and joins the reserved
// set in one step. Two locks would be two moments, and the port would be granted in between.
//
// THE RESERVED SET HAS THREE PARTS:
//   - FIXED: every port the kernel drives, and every port that would let its holder start a transfer
//     nothing translates. The PIC, the PIT and its gate at 0x61, the CMOS index and data pair (the
//     index port also holds the NMI mask), every byte of the PCI configuration mechanism but 0xCF9,
//     the whole fw_cfg interface (its DMA address registers would let a holder make the device write
//     any physical memory), the ISA DMA controllers' channel and control registers - both of them,
//     the first one's chipset alias at 0x10 included - because they master the bus untranslated into
//     the first 16 MiB for any device on a channel. Their page registers from 0x81 cannot start a
//     transfer and stay mintable, as port 0x80 does. In the test build, the exit port as well.
//   - FROM THE FADT, recorded before the boot scan: the PM1 event and control blocks, PM2 control,
//     the PM timer, both GPE blocks and the SMI command port, each at its declared length.
//   - TAKEN AT RUN TIME: a kernel item that starts driving a port installs it here, and is refused if
//     the port is granted; the same item may install a port it already holds again, and the port
//     leaves the set only with its last uninstall. COM1 is in this part from the kernel's first line,
//     held by the kernel console.
//
// NOT IN THE SET, AND MINTABLE TO A DESCRIBED DEVICE: the ports the kernel writes only on its terminal
// paths - the 8042's reset pulse at 0x64, the reset control byte at 0xCF9, the FADT's reset register
// where it is a port, and the power-off path's fallback ports - which it keeps writing on those paths
// without asking the holder, because the path ends the machine. See `TERMINAL`.
//
// FIRMWARE RESERVATIONS NEVER JOIN THE SET: a `PNP0C01`/`PNP0C02` range routinely covers ports nothing
// here drives, and a mint inside one is governed by this table like any other.

use alloc::vec::Vec;

use crate::sync::SpinLock;

// One past the last port.
pub const PORT_SPACE: u32 = 0x1_0000;

// Why a range was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	// No ports at all, or a range running past 0xFFFF.
	OutOfRange,
	// A port the kernel keeps - which part of the set it is in says why.
	Reserved(Part),
	// A port already in a live grant.
	Granted,
	// A port whose revocation could not be confirmed earlier this boot.
	Retired,
	// The table could not grow.
	NoMemory,
}

// Which part of the reserved set refused a port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
	Fixed,
	Firmware,
	Installed,
}

// The fixed part: (first port, count, what it is).
pub const FIXED: &[(u16, u16, &str)] = &[
	(0x00, 0x20, "the first ISA DMA controller, with its alias at 0x10"),
	(0x20, 2, "the master PIC"),
	(0x40, 4, "the PIT"),
	(0x61, 1, "the PIT's gate and the speaker"),
	(0x70, 2, "the CMOS index (and the NMI mask) and data"),
	(0xA0, 2, "the slave PIC"),
	(0xC0, 0x20, "the second ISA DMA controller"),
	(0xCF8, 1, "the PCI configuration address"),
	(0xCFA, 6, "the PCI configuration address and data, but the reset control byte"),
	(0x510, 12, "fw_cfg, whose DMA registers reach any physical memory"),
];

// The test build's own: the exit device, which ends the run.
#[cfg(test)]
const TEST_EXIT: (u16, u16) = (0xF4, 4);

// COM1, which the kernel console drives from its first line.
pub const COM1: (u16, u16) = (0x3F8, 8);

// The kernel items that install ports at run time. The console holds COM1 from before the boot scan until
// the COM1 handoff moves its ports into the claim's grant.
pub const KERNEL_CONSOLE: u32 = 1;

// The ports the kernel writes on its terminal paths alone: MINTABLE, and written on those paths without
// asking the holder. The FADT's reset register joins them where it is a port. Nothing in the kernel
// reads this list - a terminal path writes its port whoever holds it - so it is the tests' statement of
// the category: every one of these must stay out of the reserved set.
#[cfg(test)]
pub const TERMINAL: &[(u16, u16, &str)] = &[
	(0x64, 1, "the 8042's reset pulse"),
	(0xCF9, 1, "the reset control byte"),
	(0x600, 2, "a power-off fallback"),
	(0x604, 2, "a power-off fallback"),
	(0xB004, 2, "a power-off fallback"),
];

// A kernel item's own install: `count` holds of `len` ports from `base`.
struct Install {
	item: u32,
	base: u16,
	end: u32,
	count: u32,
}

// A span no other grant may take: a live grant, or one retired for the boot.
struct Span {
	base: u16,
	end: u32,
	// The grant's owner, or `RETIRED`.
	owner: u64,
}

const RETIRED: u64 = 0;

struct Table {
	firmware: Vec<(u16, u32)>,
	installs: Vec<Install>,
	spans: Vec<Span>,
}

static TABLE: SpinLock<Table> = SpinLock::new(Table { firmware: Vec::new(), installs: Vec::new(), spans: Vec::new() });

fn end_of(base: u16, len: u16) -> Option<u32> {
	if len == 0 {
		return None;
	}
	let end = base as u32 + len as u32;
	if end > PORT_SPACE { None } else { Some(end) }
}

fn overlaps(base: u16, end: u32, other_base: u16, other_end: u32) -> bool {
	(base as u32) < other_end && (other_base as u32) < end
}

impl Table {
	fn reserved(&self, base: u16, end: u32) -> Option<Part> {
		if FIXED.iter().any(|&(first, count, _)| overlaps(base, end, first, first as u32 + count as u32)) {
			return Some(Part::Fixed);
		}
		#[cfg(test)]
		if overlaps(base, end, TEST_EXIT.0, TEST_EXIT.0 as u32 + TEST_EXIT.1 as u32) {
			return Some(Part::Fixed);
		}
		if self.firmware.iter().any(|&(first, stop)| overlaps(base, end, first, stop)) {
			return Some(Part::Firmware);
		}
		if self.installs.iter().any(|install| overlaps(base, end, install.base, install.end)) {
			return Some(Part::Installed);
		}
		None
	}

	fn taken(&self, base: u16, end: u32) -> Option<Refusal> {
		self.spans.iter().find(|span| overlaps(base, end, span.base, span.end)).map(|span| if span.owner == RETIRED { Refusal::Retired } else { Refusal::Granted })
	}
}

// THE FADT'S PART, recorded once before the boot scan: one system-I/O block at its declared length.
// Answers false when the block runs past the port space or the table could not grow, which the caller
// reports - a block that is not recorded is a port a mint could take.
pub fn reserve_firmware(base: u16, len: u16) -> bool {
	let Some(end) = end_of(base, len) else { return false };
	let mut table = TABLE.lock();
	if table.firmware.try_reserve(1).is_err() {
		return false;
	}
	table.firmware.push((base, end));
	true
}

// WHETHER A ROW MAY RECORD `len` PORTS FROM `base`: the check made when a row is recorded, against the
// reserved set and every live grant, and made again at every mint.
pub fn recordable(base: u16, len: u16) -> Result<(), Refusal> {
	let Some(end) = end_of(base, len) else { return Err(Refusal::OutOfRange) };
	let table = TABLE.lock();
	if let Some(part) = table.reserved(base, end) {
		return Err(Refusal::Reserved(part));
	}
	match table.taken(base, end) {
		Some(refusal) => Err(refusal),
		None => Ok(()),
	}
}

// GRANT `len` ports from `base` to `owner` - the grant's object id - or refuse, in one step against the
// reserved set, every live grant and every retired span.
pub fn grant(base: u16, len: u16, owner: u64) -> Result<(), Refusal> {
	let Some(end) = end_of(base, len) else { return Err(Refusal::OutOfRange) };
	debug_assert!(owner != RETIRED, "an object id is never zero");
	let mut table = TABLE.lock();
	if let Some(part) = table.reserved(base, end) {
		return Err(Refusal::Reserved(part));
	}
	if let Some(refusal) = table.taken(base, end) {
		return Err(refusal);
	}
	if table.spans.try_reserve(1).is_err() {
		return Err(Refusal::NoMemory);
	}
	table.spans.push(Span { base, end, owner });
	Ok(())
}

// END `owner`'s grant: its ports go back into circulation when every core confirmed it lets nobody use
// them, and stay out of every later grant this boot when one did not. Retiring rewrites the span in
// place, so it can never fail for want of memory.
pub fn end(owner: u64, confirmed: bool) {
	let mut table = TABLE.lock();
	let Some(at) = table.spans.iter().position(|span| span.owner == owner) else { return };
	if confirmed {
		table.spans.swap_remove(at);
	} else {
		table.spans[at].owner = RETIRED;
		let span = &table.spans[at];
		crate::serial_println!("ports: {:#06x}..{:#06x} are retired for this boot - a revocation of them was not confirmed on every core", span.base, span.end - 1);
	}
}

// A RUN-TIME INSTALL: kernel item `item` starts driving `len` ports from `base`.
//
// Refused while any of them is granted or retired, or reserved by anything but this same item; admitted
// again, and counted, when this item already holds exactly this range - firmware names one register in
// every core's table, and a replaced table installs its own again.
pub fn install(item: u32, base: u16, len: u16) -> Result<(), Refusal> {
	let Some(end) = end_of(base, len) else { return Err(Refusal::OutOfRange) };
	let mut table = TABLE.lock();
	if let Some(held) = table.installs.iter_mut().find(|install| install.item == item && install.base == base && install.end == end) {
		held.count = held.count.saturating_add(1);
		return Ok(());
	}
	if let Some(refusal) = table.taken(base, end) {
		return Err(refusal);
	}
	if let Some(part) = table.reserved(base, end) {
		return Err(Refusal::Reserved(part));
	}
	if table.installs.try_reserve(1).is_err() {
		return Err(Refusal::NoMemory);
	}
	table.installs.push(Install { item, base, end, count: 1 });
	Ok(())
}

// The matching uninstall. The ports leave the set only with this item's last hold of them; answers
// whether the item held them at all. Its first kernel caller is the COM1 handoff; until then the tests
// are.
#[cfg(test)]
pub fn uninstall(item: u32, base: u16, len: u16) -> bool {
	let Some(end) = end_of(base, len) else { return false };
	let mut table = TABLE.lock();
	let Some(at) = table.installs.iter().position(|install| install.item == item && install.base == base && install.end == end) else { return false };
	table.installs[at].count -= 1;
	if table.installs[at].count == 0 {
		table.installs.swap_remove(at);
	}
	true
}

// How many spans are retired for the boot - for the tests, which must see a failed revocation retire
// its ports and a confirmed one not.
#[cfg(test)]
pub fn retired_covering(port: u16) -> bool {
	TABLE.lock().spans.iter().any(|span| span.owner == RETIRED && (span.base as u32) <= port as u32 && (port as u32) < span.end)
}
