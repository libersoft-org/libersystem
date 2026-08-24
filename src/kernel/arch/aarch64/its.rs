// The GICv3 Interrupt Translation Service: the other kind of ARM MSI controller.
//
// A GICv2m frame is a register a device writes an SPI NUMBER into - the device names the interrupt
// it wants raised. An ITS is not that. A device writes an EVENT ID to one translation register, and
// the ITS looks up the pair (which device wrote, which event) in TABLES THIS KERNEL OWNS to decide
// which LPI to raise and on which core. So the identity is the controller's rather than the
// device's: a device cannot name an interrupt it was not mapped to, which is why the ITS is the
// modern configuration and why bringing it up is table and queue work rather than a register write.
//
// What is here is the physical LPI path for one collection targeting the boot core - the same
// routing the v2m backend has always had (`ITARGETSR = 0x01`). Virtual LPIs, per-core collections
// and the two-level table forms are not, and a machine that needs one of them is refused by name
// rather than driven wrongly.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::mem::frame;
use crate::sync::SpinLock;

// The control frame.
const GITS_CTLR: u64 = 0x0000; // bit 0 Enabled, bit 31 Quiescent
const GITS_TYPER: u64 = 0x0008;
const GITS_CBASER: u64 = 0x0080;
const GITS_CWRITER: u64 = 0x0088;
const GITS_CREADR: u64 = 0x0090;
const GITS_BASER: u64 = 0x0100; // eight 64-bit registers
// The translation frame: the address a device's MSI-X entry is programmed with.
const GITS_TRANSLATER: u64 = 0x1_0040;

// GITS_BASER table types.
const TABLE_DEVICES: u64 = 1;
const TABLE_COLLECTIONS: u64 = 4;

// Commands, one byte of opcode in the first doubleword of a 32-byte entry.
const CMD_SYNC: u64 = 0x05;
const CMD_MAPD: u64 = 0x08;
const CMD_MAPC: u64 = 0x09;
const CMD_MAPTI: u64 = 0x0a;
const CMD_INV: u64 = 0x0c;
const CMD_INVALL: u64 = 0x0d;
const CMD_DISCARD: u64 = 0x0f;

// The command queue: 64 KiB, which is both the size Linux uses and a size whose natural alignment
// satisfies GITS_CBASER's 64 KiB requirement when it is carved out of a larger run.
const CMD_QUEUE_BYTES: u64 = 0x1_0000;
const CMD_BYTES: u64 = 32;

// The first LPI INTID. Fixed by the architecture: INTIDs below this are SGIs, PPIs and SPIs.
pub const LPI_BASE: u32 = 8192;
// How many LPI identifiers the configuration table covers. The table starts at INTID 8192 and its
// size is `2^(IDbits+1) - 8192`, so thirteen ID bits is the smallest table that holds any LPIs at
// all: 8192 of them, in 8 KiB.
const LPI_ID_BITS: u64 = 13;
const LPI_COUNT: u64 = 8192;
// One byte per LPI: priority in bits 7:2, enable in bit 0.
const LPI_CONFIG_BYTES: u64 = LPI_COUNT;
const LPI_PRIORITY: u8 = 0xa0;
// Bit 1 of a configuration entry, and it is not optional: an LPI is a Group 1 interrupt and the
// architecture requires this bit written as one. Left clear, the controller reads the entry as
// GROUP 0 - delivered as FIQ, which this port does not enable and does not handle - so every
// message pends in the table and none is ever taken. That is a delivery which never happens rather
// than one which is late, and it looks exactly like a device that is not wired up.
const LPI_GROUP1: u8 = 1 << 1;
// The pending table is one bit per LPI, and GICR_PENDBASER holds its address from bit 16 up - so it
// is 64 KiB aligned whatever its size.
const LPI_PENDING_BYTES: u64 = 0x1_0000;

// One collection, targeting the boot core.
const COLLECTION: u64 = 0;

static ITS_BASE: AtomicU64 = AtomicU64::new(0);
static CMD_BASE: AtomicU64 = AtomicU64::new(0); // physical
static CMD_WRITE: AtomicU64 = AtomicU64::new(0); // byte offset of the next free slot
static LPI_CONFIG: AtomicU64 = AtomicU64::new(0); // physical
// EventID bits the ITS supports, and how many devices its table covers - both read from GITS_TYPER
// and both bounds this kernel refuses past rather than truncates into.
static EVENT_BITS: AtomicU32 = AtomicU32::new(0);
static DEVICE_LIMIT: AtomicU32 = AtomicU32::new(0);
// How many bytes one entry of a device's interrupt translation table takes, from GITS_TYPER.
static ITT_ENTRY: AtomicU32 = AtomicU32::new(0);
// The value this controller wants in a command's RDbase field, already shifted into place.
//
// TWO WAYS TO NAME A CORE, AND GITS_TYPER.PTA SAYS WHICH. With PTA set a collection targets a
// redistributor's ADDRESS; with it clear the same field carries the redistributor's own PROCESSOR
// NUMBER - a third numbering, neither the logical cpu id nor the affinity. QEMU's ITS reports PTA
// clear, so the address form was the one that could not be tested by running it.
static TARGET: AtomicU64 = AtomicU64::new(0);

// THE COMMAND QUEUE IS A RING, AND TWO CORES CAN REACH IT AT ONCE. Posting a command is read the
// write offset, write thirty-two bytes there, ring the doorbell, wait for the read pointer - four
// steps, each atomic on its own and the sequence not. An MSI acquire and a teardown are both
// userspace syscalls, so two cores can be inside this at the same time, write the same slot, and
// leave one command never issued while its caller waits for it.
//
// The v2m backend never needed this because it has no queue: it writes distributor registers, and
// each of those is its own operation.
static QUEUE: SpinLock<()> = SpinLock::new(());

fn reg(off: u64) -> *mut u64 {
	super::paging::phys_to_virt(ITS_BASE.load(Ordering::Relaxed) + off) as *mut u64
}

// GITS_CTLR IS THIRTY-TWO BITS WIDE, and the registers around it are sixty-four. A 64-bit store to
// it covers GITS_IIDR as well, which is read-only - and an implementation is free to ignore the
// whole access. This one did: the enable never took, and every command sat in the queue unread.
fn ctlr() -> *mut u32 {
	super::paging::phys_to_virt(ITS_BASE.load(Ordering::Relaxed) + GITS_CTLR) as *mut u32
}

// Everything the ITS reads from memory is written through the direct map, which is cacheable, so a
// write is not visible to it until the store buffer is drained. `dsb ish` before ringing the
// doorbell is what makes the command it is about to read the one that was written.
fn barrier() {
	unsafe { core::arch::asm!("dsb ish", options(nostack, preserves_flags)) };
}

// A contiguous, zeroed physical run of `bytes`, aligned to `align`.
//
// Over-allocates and takes the aligned window inside it, because the frame allocator answers in
// pages and these tables have alignment rules a page does not satisfy - the command queue and the
// LPI pending table are both addressed from bit 16 up. The remainder is not returned: this runs once
// at boot, and handing back the two halves of a split run is how a page ends up freed twice.
fn table(bytes: u64, align: u64) -> Option<u64> {
	let pages = ((bytes + align) / 4096 + 1) as usize;
	let Some(base) = frame::allocate_contiguous(pages) else {
		crate::serial_println!("its: no run of {pages} contiguous pages for a table - no MSI from this controller");
		return None;
	};
	let aligned = (base + align - 1) & !(align - 1);
	unsafe {
		core::ptr::write_bytes(super::paging::phys_to_virt(aligned) as *mut u8, 0, bytes as usize);
	}
	Some(aligned)
}

// Program one GITS_BASER for a table this kernel allocated, and answer whether the ITS took it.
//
// THE FIELDS ARE NEGOTIATED, NOT SET. Cacheability and shareability are writable-or-ignored: an ITS
// that does not snoop this kernel's caches reports back non-cacheable, and every table write would
// then need a cache clean this port does not do. Reading the register back is how that is found out
// rather than assumed, and a controller that answers non-cacheable is refused.
fn program_baser(index: u64, kind: u64, entry_size: u64, pages: u64, phys: u64) -> bool {
	let addr = ITS_BASE.load(Ordering::Relaxed) + GITS_BASER + index * 8;
	let ptr = super::paging::phys_to_virt(addr) as *mut u64;
	// Valid, inner-cacheable read-allocate write-back (5), inner-shareable (1), 4 KiB pages (0).
	let value = 1 << 63 | 5 << 59 | kind << 56 | (entry_size - 1) << 48 | (phys & 0x0000_ffff_ffff_f000) | 1 << 10 | (pages - 1);
	unsafe {
		core::ptr::write_volatile(ptr, value);
		let read = core::ptr::read_volatile(ptr);
		if read >> 63 & 1 == 0 {
			crate::serial_println!("its: the controller would not take a table of type {kind}");
			return false;
		}
		if read >> 59 & 0x7 <= 1 {
			crate::serial_println!("its: the controller keeps its tables non-cacheable, which needs cache maintenance this port does not do - no MSI from it");
			return false;
		}
	}
	true
}

// Post one command and wait for the ITS to consume it.
//
// SERIAL, AND WAITED FOR. The queue is a ring the controller reads at its own pace, and a caller
// that does not wait cannot tell a command that was rejected from one that has not run yet. Every
// caller here is a device setup or teardown, none of them is on a hot path, and the alternative is
// a queue whose errors are discovered by an interrupt that never arrives.
fn command(dw0: u64, dw1: u64, dw2: u64, dw3: u64) -> bool {
	// HELD ACROSS THE POST AND THE WAIT, because the wait reads `GITS_CREADR` against the offset
	// this caller wrote: another core advancing the ring underneath it would make it wait for a
	// pointer that has already gone past.
	let _posting = QUEUE.lock();
	let base = CMD_BASE.load(Ordering::Relaxed);
	let offset = CMD_WRITE.load(Ordering::Relaxed);
	let slot = super::paging::phys_to_virt(base + offset) as *mut u64;
	unsafe {
		core::ptr::write_volatile(slot, dw0);
		core::ptr::write_volatile(slot.add(1), dw1);
		core::ptr::write_volatile(slot.add(2), dw2);
		core::ptr::write_volatile(slot.add(3), dw3);
	}
	let next = (offset + CMD_BYTES) % CMD_QUEUE_BYTES;
	barrier();
	unsafe { core::ptr::write_volatile(reg(GITS_CWRITER), next) };
	CMD_WRITE.store(next, Ordering::Relaxed);
	// The read pointer reaching the write pointer means the queue is drained.
	let mut spins = 0u32;
	while unsafe { core::ptr::read_volatile(reg(GITS_CREADR)) } & !0x1f != next && spins < 1_000_000 {
		core::hint::spin_loop();
		spins += 1;
	}
	if unsafe { core::ptr::read_volatile(reg(GITS_CREADR)) } & !0x1f != next {
		crate::serial_println!("its: a command was not consumed - the queue is stalled and this controller is not delivering");
		return false;
	}
	true
}

// Bring the ITS up: its tables, its command queue, and one collection aimed at this core.
//
// `redistributor` is the physical base of the boot core's redistributor frame, which is what the
// collection targets. Returns false with a reason on the line above whenever the machine is one this
// port does not drive - a caller that gets false has no MSI and says so.
pub fn init(base: u64, size: u64, redistributor: u64) -> bool {
	if base == 0 || size < 0x2_0000 || !crate::mem::within_direct_map(base, size) {
		crate::serial_println!("its: {base:#x}+{size:#x} is not an ITS this kernel can reach");
		return false;
	}
	ITS_BASE.store(base, Ordering::Relaxed);
	let typer = unsafe { core::ptr::read_volatile(reg(GITS_TYPER)) };
	if typer & 1 == 0 {
		crate::serial_println!("its: the controller reports no physical LPI support");
		return false;
	}
	let target = if typer >> 19 & 1 != 0 { redistributor & 0x0007_ffff_ffff_0000 } else { u64::from(super::gic::processor_number(redistributor)) << 16 };
	TARGET.store(target, Ordering::Relaxed);
	// How big one entry of a device's interrupt translation table is. This port gives a device ONE
	// PAGE for its table, so this is the number that decides whether the event window it maps fits -
	// and it was being read and thrown away.
	let itt_entry = (typer >> 4 & 0xf) + 1;
	EVENT_BITS.store((typer >> 8 & 0x1f) as u32 + 1, Ordering::Relaxed);
	ITT_ENTRY.store(itt_entry as u32, Ordering::Relaxed);

	// The tables. Their types are reported per register, so each is programmed where the controller
	// put it rather than at a fixed index.
	let mut devices = false;
	let mut collections = false;
	for index in 0..8u64 {
		let value = unsafe { core::ptr::read_volatile(super::paging::phys_to_virt(base + GITS_BASER + index * 8) as *const u64) };
		let kind = value >> 56 & 0x7;
		let entry_size = (value >> 48 & 0xff) + 1;
		if kind == TABLE_DEVICES && !devices {
			let Some(phys) = table(4096, 4096) else { return false };
			if !program_baser(index, kind, entry_size, 1, phys) {
				return false;
			}
			// One 4 KiB page of entries is what this port maps devices in, so a DeviceID past it is
			// a device it cannot name to the controller - refused at acquire rather than mapped
			// into somebody else's entry.
			DEVICE_LIMIT.store((4096 / entry_size) as u32, Ordering::Relaxed);
			devices = true;
		} else if kind == TABLE_COLLECTIONS && !collections {
			let Some(phys) = table(4096, 4096) else { return false };
			if !program_baser(index, kind, entry_size, 1, phys) {
				return false;
			}
			collections = true;
		}
	}
	if !devices || !collections {
		crate::serial_println!("its: the controller does not ask for the device and collection tables this port programs");
		return false;
	}

	// The command queue, and the LPI configuration table the redistributors read.
	let Some(queue) = table(CMD_QUEUE_BYTES, CMD_QUEUE_BYTES) else { return false };
	let Some(config) = table(LPI_CONFIG_BYTES, 4096) else { return false };
	CMD_BASE.store(queue, Ordering::Relaxed);
	CMD_WRITE.store(0, Ordering::Relaxed);
	LPI_CONFIG.store(config, Ordering::Relaxed);
	unsafe {
		// Valid, inner-cacheable write-back, inner-shareable, size in 4 KiB pages minus one.
		core::ptr::write_volatile(reg(GITS_CBASER), 1 << 63 | 5 << 59 | (queue & 0x0000_ffff_ffff_f000) | 1 << 10 | (CMD_QUEUE_BYTES / 4096 - 1));
		core::ptr::write_volatile(reg(GITS_CWRITER), 0);
	}

	// LPIs on this core, BEFORE the ITS is enabled: a redistributor's LPI tables are read-only once
	// GICR_CTLR.EnableLPIs is set, and an LPI that arrives at a redistributor which has none is
	// discarded rather than queued.
	let Some(pending) = table(LPI_PENDING_BYTES, 0x1_0000) else { return false };
	if !super::gic::enable_lpis(redistributor, config, LPI_ID_BITS, pending) {
		return false;
	}

	unsafe {
		core::ptr::write_volatile(ctlr(), 1);
		if core::ptr::read_volatile(ctlr()) & 1 == 0 {
			crate::serial_println!("its: the controller would not enable - its tables or its queue are not ones it accepts");
			return false;
		}
	}
	// One collection, and it is this core, named the way this controller asks to be told.
	if !command(CMD_MAPC, 0, 1 << 63 | target | COLLECTION, 0) {
		return false;
	}
	crate::serial_println!("its: up - {} event id bits, {} device ids, {} LPIs from INTID {}", EVENT_BITS.load(Ordering::Relaxed), DEVICE_LIMIT.load(Ordering::Relaxed), LPI_COUNT, LPI_BASE);
	true
}

// The address a device's MSI-X entry is programmed with: every device writes its event id here.
pub fn translater() -> u64 {
	ITS_BASE.load(Ordering::Relaxed) + GITS_TRANSLATER
}

// Give `devid` an interrupt translation table, so events from it can be mapped.
//
// `events` is how many event ids this kernel will use for the device; the command takes the number
// of BITS, so the table is sized to the next power of two.
pub fn map_device(devid: u32, events: u32, itt: u64) -> bool {
	if devid >= DEVICE_LIMIT.load(Ordering::Relaxed) {
		crate::serial_println!("its: device id {devid} is past the {} this port's device table holds", DEVICE_LIMIT.load(Ordering::Relaxed));
		return false;
	}
	let bits = events.max(2).next_power_of_two().trailing_zeros() as u64;
	if bits > EVENT_BITS.load(Ordering::Relaxed) as u64 {
		return false;
	}
	// THE TABLE IS ONE PAGE, AND THE CONTROLLER SAYS HOW WIDE AN ENTRY IS. A window that does not
	// fit would have the ITS writing an event's entry past the end of the page this kernel
	// allocated - into whatever is next - so it is refused here by name.
	if (1u64 << bits) * u64::from(ITT_ENTRY.load(Ordering::Relaxed)) > 4096 {
		crate::serial_println!("its: {} events at {} bytes each do not fit the one page this port gives a device", 1u64 << bits, ITT_ENTRY.load(Ordering::Relaxed));
		return false;
	}
	// ITT address from bit 8 up, size in bits minus one, Valid.
	command(CMD_MAPD | (devid as u64) << 32, bits - 1, 1 << 63 | (itt & 0x0000_ffff_ffff_ff00), 0)
}

// Map one of `devid`'s events to `lpi`, aimed at the collection this port targets, and enable it.
pub fn map_event(devid: u32, event: u32, lpi: u32) -> bool {
	set_lpi(lpi, true);
	command(CMD_MAPTI | (devid as u64) << 32, event as u64 | (lpi as u64) << 32, COLLECTION, 0)
		&& command(CMD_INV | (devid as u64) << 32, event as u64, 0, 0)
		// INVALL, NOT ONLY INV. `INV` tells the redistributor that ONE identity's configuration
		// changed; `INVALL` tells it to re-read the configuration of every LPI in the collection -
		// which is also what makes it look again at LPIs that are already pending. Without it a
		// message that arrived while its configuration was still being read stays pending forever,
		// which is a delivery that never happens rather than a delivery that is late.
		&& command(CMD_INVALL, 0, COLLECTION, 0)
		&& command(CMD_SYNC, 0, TARGET.load(Ordering::Relaxed), 0)
}

// Unmap one of `devid`'s events. The LPI is disabled in the configuration table FIRST, so a message
// already in flight finds an identity that raises nothing, and the mapping is discarded after.
pub fn discard_event(devid: u32, event: u32, lpi: u32) -> bool {
	set_lpi(lpi, false);
	command(CMD_INV | (devid as u64) << 32, event as u64, 0, 0) && command(CMD_DISCARD | (devid as u64) << 32, event as u64, 0, 0) && command(CMD_SYNC, 0, TARGET.load(Ordering::Relaxed), 0)
}

// The configuration byte for one LPI: priority and whether it is delivered at all.
fn set_lpi(lpi: u32, on: bool) {
	let config = LPI_CONFIG.load(Ordering::Relaxed);
	if config == 0 || lpi < LPI_BASE || u64::from(lpi - LPI_BASE) >= LPI_CONFIG_BYTES {
		return;
	}
	let entry = super::paging::phys_to_virt(config + u64::from(lpi - LPI_BASE)) as *mut u8;
	unsafe { core::ptr::write_volatile(entry, LPI_PRIORITY | LPI_GROUP1 | u8::from(on)) };
	barrier();
}
