// driver.xhci - the userspace xHCI USB host controller driver (M62).
//
// DeviceManager launches this program with a "DEVICE" message carrying the
// controller's DeviceInfo and a transferred DeviceMemory capability to its MMIO
// BAR (the whole xHCI register file), followed by an "IRQ" message carrying the
// controller's MSI-X Interrupt capability. The driver maps the BAR, resets the
// controller, builds the device context base array, the command ring and the
// event ring, starts the controller, and enumerates the root-hub ports: each
// connected device is reset, given a device slot, addressed, and has its device
// descriptor read over a control transfer on the default endpoint. Every HID
// device found among them is configured (its interrupt IN endpoint brought up,
// its report descriptor read and parsed) and served interrupt-driven for the
// life of the system: keyboard and Consumer-page changes feed the interactive
// console through the shared keys module, exactly like the virtio-input
// keyboard, and a pointing device's reports feed InputService as normalized
// pointer events, exactly like the virtio-input pointer. Bring-up itself is
// synchronous and polled - commands and transfers one at a time, completions
// reaped off the event ring - matching the polled virtio-blk/gpu drivers.

#![no_std]
#![no_main]

extern crate alloc;

mod hid;
mod keys;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use proto::system::usb;
use proto::system::{Error as UsbError, UsbDevice as UsbEntry};
use rt::*;

use crate::keys::Mods;

// Capability registers (at the mapped BAR base).
const CAP_CAPLENGTH: u64 = 0x00; // u8: operational-register offset
const CAP_HCSPARAMS1: u64 = 0x04; // slots [7:0], interrupters [18:8], ports [31:24]
const CAP_HCSPARAMS2: u64 = 0x08; // scratchpad count hi [25:21] / lo [31:27]
const CAP_HCCPARAMS1: u64 = 0x10; // CSZ (64-byte contexts) = bit 2
const CAP_DBOFF: u64 = 0x14; // doorbell-array offset (mask ~0x3)
const CAP_RTSOFF: u64 = 0x18; // runtime-register offset (mask ~0x1f)

// Operational registers (at base + CAPLENGTH).
const OP_USBCMD: u64 = 0x00;
const OP_USBSTS: u64 = 0x04;
const OP_CRCR: u64 = 0x18;
const OP_DCBAAP: u64 = 0x30;
const OP_CONFIG: u64 = 0x38;
const OP_PORTSC_BASE: u64 = 0x400; // + (port - 1) * 0x10

// USBCMD bits.
const CMD_RUN: u32 = 1 << 0;
const CMD_HCRST: u32 = 1 << 1;
const CMD_INTE: u32 = 1 << 2; // interrupter enable

// USBSTS bits.
const STS_HCHALTED: u32 = 1 << 0;
const STS_CNR: u32 = 1 << 11; // controller not ready

// PORTSC bits. The register is a minefield of RW1C bits: writes always go through
// portsc_write below, which masks them out so a read-modify-write cannot clear a
// change flag by accident.
const PORTSC_CCS: u32 = 1 << 0; // current connect status
const PORTSC_PED: u32 = 1 << 1; // port enabled (RW1C - writing 1 disables!)
const PORTSC_PR: u32 = 1 << 4; // port reset
const PORTSC_PP: u32 = 1 << 9; // port power
const PORTSC_CSC: u32 = 1 << 17; // connect status change (RW1C)
const PORTSC_PEC: u32 = 1 << 18; // port enabled change (RW1C)
const PORTSC_WRC: u32 = 1 << 19; // warm reset change (RW1C)
const PORTSC_PRC: u32 = 1 << 21; // port reset change (RW1C)
const PORTSC_PLC: u32 = 1 << 22; // port link state change (RW1C)
const PORTSC_CEC: u32 = 1 << 23; // config error change (RW1C)
const PORTSC_RW1C: u32 = PORTSC_PED | PORTSC_CSC | PORTSC_PEC | PORTSC_WRC | PORTSC_PRC | PORTSC_PLC | PORTSC_CEC;

// Port speed ids (PORTSC bits 13:10); full/low speed take the default packet size.
const SPEED_HIGH: u32 = 3;
const SPEED_SUPER: u32 = 4;

// Interrupter 0 registers (at base + RTSOFF + 0x20).
const IR_IMAN: u64 = 0x00;
const IR_IMOD: u64 = 0x04;
const IR_ERSTSZ: u64 = 0x08;
const IR_ERSTBA: u64 = 0x10;
const IR_ERDP: u64 = 0x18;
const ERDP_EHB: u64 = 1 << 3; // event handler busy (RW1C)

// IMAN bits: interrupt pending (RW1C) and interrupt enable.
const IMAN_IP: u32 = 1 << 0;
const IMAN_IE: u32 = 1 << 1;

// TRB types (control-word bits 15:10).
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_ENDPOINT: u32 = 12;
const TRB_EVALUATE_CONTEXT: u32 = 13;
const TRB_RESET_ENDPOINT: u32 = 14;
const TRB_SET_TR_DEQUEUE: u32 = 16;
const TRB_EV_TRANSFER: u32 = 32;
const TRB_EV_CMD_COMPLETE: u32 = 33;
const TRB_EV_PORT_STATUS: u32 = 34;

// TRB control-word bits.
const TRB_CYCLE: u32 = 1 << 0;
const TRB_TOGGLE_CYCLE: u32 = 1 << 1;
const TRB_IOC: u32 = 1 << 5;
const TRB_IDT: u32 = 1 << 6;
const TRB_DIR_IN: u32 = 1 << 16;
const TRB_TRT_IN: u32 = 3 << 16; // setup stage: IN data stage follows

// TRB completion code (event status bits 31:24) for success.
const CC_SUCCESS: u32 = 1;
// A short packet is a successful IN transfer that returned fewer bytes than asked
// for - normal for a descriptor read sized generously.
const CC_SHORT_PACKET: u32 = 13;
// The device stalled the endpoint (it rejected the request, or a Bulk-Only data
// stage ran past what the command returns): the endpoint is halted until the
// stall-recovery dance below clears it.
const CC_STALL: u32 = 6;

// Rings are one DMA page of 256 16-byte TRBs; the command ring's last entry is a
// link TRB back to the start.
const RING_TRBS: u64 = 256;

// The spin budget for one polled completion, with a cooperative yield on the slow
// path (same shape as the virtio queue poll).
const SPIN_BUDGET: u32 = 10_000_000;

// GET_DESCRIPTOR request fields.
const REQ_GET_DESCRIPTOR: u8 = 6;
const REQ_SET_CONFIGURATION: u8 = 9;
const DESC_DEVICE: u16 = 1;
const DESC_CONFIG: u16 = 2;

// CLEAR_FEATURE(ENDPOINT_HALT) to an endpoint (bmRequestType 0x02): resets the
// device side of a stalled endpoint (its data toggle), the USB half of the
// stall-recovery dance whose xHCI half is Reset Endpoint + Set TR Dequeue Pointer.
const REQ_CLEAR_FEATURE: u8 = 1;
const FEATURE_ENDPOINT_HALT: u16 = 0;
const RT_ENDPOINT: u8 = 0x02;

// The Bulk-Only Mass Storage Reset class request (to the interface): the last-resort
// recovery that returns the device's BOT state machine to idle after a transport
// error a per-endpoint stall recovery cannot fix.
const BOT_REQ_RESET: u8 = 0xff;
const RT_CLASS_INTERFACE: u8 = 0x21;

// The HID class SET_PROTOCOL request (to the interface): wValue 0 selects the
// fixed boot-report layout - the fallback for a boot-subclass keyboard whose
// report descriptor cannot be read.
const HID_REQ_SET_PROTOCOL: u8 = 0x0b;

// Descriptor types and the HID identity within a configuration: the HID class
// descriptor (embedded after the interface descriptor) names the report
// descriptor's length, and the report descriptor itself is read with an
// interface-targeted GET_DESCRIPTOR.
const DT_INTERFACE: u8 = 4;
const DT_ENDPOINT: u8 = 5;
const DT_HID: u8 = 0x21;
const DESC_REPORT: u16 = 0x22;
const RT_INTERFACE_IN: u8 = 0x81;
const CLASS_HID: u8 = 3;
const SUBCLASS_BOOT: u8 = 1;
const PROTOCOL_KEYBOARD: u8 = 1;
const EP_ATTR_INTERRUPT: u8 = 3;
const EP_ATTR_BULK: u8 = 2;

// The USB mass-storage identity within a configuration: the class with the SCSI
// transparent command set over the Bulk-Only Transport.
const CLASS_MASS_STORAGE: u8 = 8;
const SUBCLASS_SCSI: u8 = 6;
const PROTOCOL_BULK_ONLY: u8 = 0x50;

// The hub device class (in the device descriptor) and its class descriptor type,
// plus the hub class requests enumeration drives: GET_STATUS on a port (the status
// word's connection / low-speed / high-speed bits), SET_FEATURE for port power and
// reset, and CLEAR_FEATURE for the per-port change flags.
const CLASS_HUB: u8 = 9;
const DESC_HUB: u16 = 0x29;
const REQ_GET_STATUS: u8 = 0;
const REQ_SET_FEATURE: u8 = 3;
const RT_CLASS_DEVICE_IN: u8 = 0xa0;
const RT_CLASS_PORT: u8 = 0x23;
const RT_CLASS_PORT_IN: u8 = 0xa3;
const HUB_FEAT_PORT_RESET: u16 = 4;
const HUB_FEAT_PORT_POWER: u16 = 8;
const HUB_FEAT_C_CONNECTION: u16 = 16;
const HUB_FEAT_C_RESET: u16 = 20;
const PORT_STATUS_CCS: u16 = 1 << 0;
const PORT_STATUS_POWER: u16 = 1 << 8;
const PORT_STATUS_LOW_SPEED: u16 = 1 << 9;
const PORT_STATUS_HIGH_SPEED: u16 = 1 << 10;
const PORT_CHANGE_RESET: u16 = 1 << 4;

// Bulk-Only Transport framing: the 31-byte Command Block Wrapper and the 13-byte
// Command Status Wrapper, each led by its signature; a CSW status of 0 is success.
const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
const CBW_LEN: u32 = 31;
const CSW_LEN: u32 = 13;
// The CSW rides in the same scratch page as the CBW, at the next 32-byte slot.
const CSW_OFF: u64 = 32;
const CBW_FLAG_IN: u8 = 0x80;

// SCSI command opcodes (the transparent command set a USB stick speaks).
const SCSI_TEST_UNIT_READY: u8 = 0x00;
const SCSI_REQUEST_SENSE: u8 = 0x03;
const SCSI_READ_CAPACITY10: u8 = 0x25;
const SCSI_READ10: u8 = 0x28;
const SCSI_WRITE10: u8 = 0x2a;
const SCSI_SYNCHRONIZE_CACHE10: u8 = 0x35;

// One disk sector, and the block-service wire protocol this driver serves to a
// StorageService instance - the same contract driver.virtio-blk serves: a request
// is [op u32][lba u64][count u32], a read replies [status u32] + a MemoryObject of
// the sectors, a write carries a MemoryObject in and replies [status u32], and a
// capacity query (op 2) replies [status u32][capacity bytes u64], and a flush (op 3)
// - the write barrier, served as SCSI SYNCHRONIZE CACHE (10) - replies [status u32].
// The per-request
// sector cap is this driver's own: one SCSI READ(10)/WRITE(10) moves through the
// unit's single 4 kB data page, so 8 sectors is the transfer unit here (a larger
// unit needs a multi-page BOT data buffer - a future throughput step).
const SECTOR: u32 = 512;
const MAX_SECTORS: u32 = 8;
const OP_READ: u32 = 0;
const OP_WRITE: u32 = 1;
const OP_CAPACITY: u32 = 2;
const OP_FLUSH: u32 = 3;
const STATUS_OK: u32 = 0;
const STATUS_ERR: u32 = 1;

unsafe fn r8(addr: u64) -> u8 {
	unsafe { (addr as *const u8).read_volatile() }
}
unsafe fn r32(addr: u64) -> u32 {
	unsafe { (addr as *const u32).read_volatile() }
}
unsafe fn w32(addr: u64, v: u32) {
	unsafe { (addr as *mut u32).write_volatile(v) }
}
// A 64-bit register is written as two 32-bit halves (low then high), the portable
// form; xHCI permits 32-bit accesses to all its registers.
unsafe fn w64(addr: u64, v: u64) {
	unsafe {
		w32(addr, v as u32);
		w32(addr + 4, (v >> 32) as u32);
	}
}

// Allocate one zeroed DMA page. Zeroing matters: a freed page from an earlier
// driver instance is recycled with its old ring contents intact, and a stale TRB
// with the right cycle bit would read as a fresh event.
unsafe fn dma_page() -> Option<(u64, u64, u64)> {
	unsafe {
		let (handle, virt, phys): (u64, u64, u64) = dma_buffer(4096)?;
		core::ptr::write_bytes(virt as *mut u8, 0, 4096);
		Some((handle, virt, phys))
	}
}

// One producer TRB ring (command or transfer): a zeroed DMA page of 256 TRBs whose
// last slot is a link TRB back to the start (toggle cycle), so a ring pushed to
// forever - the keyboard's interrupt endpoint - wraps correctly.
struct Ring {
	virt: u64,
	phys: u64,
	index: u64,
	cycle: u32,
}

impl Ring {
	// Allocate the ring page and plant the wrapping link TRB.
	unsafe fn new() -> Option<Ring> {
		unsafe {
			let (_h, virt, phys): (u64, u64, u64) = dma_page()?;
			let link: u64 = virt + (RING_TRBS - 1) * 16;
			(link as *mut u64).write_volatile(phys);
			((link + 12) as *mut u32).write_volatile(TRB_LINK << 10 | TRB_TOGGLE_CYCLE);
			Some(Ring { virt, phys, index: 0, cycle: 1 })
		}
	}

	// Push one TRB, following the link TRB (and toggling the cycle state) on wrap.
	unsafe fn push(&mut self, param: u64, status: u32, control: u32) {
		unsafe {
			let trb: u64 = self.virt + self.index * 16;
			(trb as *mut u64).write_volatile(param);
			((trb + 8) as *mut u32).write_volatile(status);
			((trb + 12) as *mut u32).write_volatile(control | self.cycle);
			self.index += 1;
			if self.index == RING_TRBS - 1 {
				// consume the link TRB: give it the producer cycle and wrap.
				let link: u64 = self.virt + self.index * 16;
				let ctl: u32 = ((link + 12) as *const u32).read_volatile() & !TRB_CYCLE;
				((link + 12) as *mut u32).write_volatile(ctl | self.cycle);
				self.index = 0;
				self.cycle ^= 1;
			}
		}
	}
}

// The controller with its register windows resolved and its rings built.
struct Xhci {
	// Operational, runtime-interrupter-0 and doorbell-array register bases.
	op: u64,
	ir0: u64,
	db: u64,
	// 64-byte contexts when set (HCCPARAMS1.CSZ); 32-byte otherwise.
	ctx_size: u64,
	ports: u32,
	// The command ring.
	cmd: Ring,
	// Event ring: virtual/physical base, consumer index and cycle state.
	evt_virt: u64,
	evt_phys: u64,
	evt_index: u64,
	evt_cycle: u32,
	// Device context base address array (virtual base; entry per slot).
	dcbaa_virt: u64,
}

// One addressed USB device: its slot, root-hub port, route string (the hub-port
// chain below that root port, one nibble per tier; 0 = directly attached), speed,
// the default endpoint's transfer ring, and the scratch pages enumeration reuses
// (the input context and the control-transfer data page).
struct UsbDevice {
	slot: u32,
	port: u32,
	route: u32,
	speed: u32,
	ep0: Ring,
	in_virt: u64,
	in_phys: u64,
	data_virt: u64,
	data_phys: u64,
	// The device descriptor's identity fields.
	vendor: u16,
	product: u16,
	class: u8,
}

// A configured HID input device: the interrupt IN endpoint's device context index
// and its transfer ring, on which input reports of the descriptor's size are
// posted and reaped; the parsed report layout the reports decode against; the
// previous report body per report id (the state the key diffs run against); the
// tracked modifiers; and the running pointer state (normalized position and
// buttons) a pointing device's reports fold into.
struct Hid {
	dci: u32,
	ring: Ring,
	layout: hid::Layout,
	posted: bool,
	prevs: Vec<(u8, [u8; 64])>,
	mods: Mods,
	x: i32,
	y: i32,
	buttons: u8,
}

// The bound HID devices the service loop reaps reports for. The synchronous
// control / transport waits service their events inline, so typing is never lost
// behind disk traffic; bring-up paths pass an empty set (no report TRB is in
// flight before the service loop posts the first one).
struct Hids {
	entries: Vec<(UsbDevice, Hid)>,
}

impl Hids {
	const fn new() -> Hids {
		Hids { entries: Vec::new() }
	}

	// Whether any bound device reports keyboard-page keys.
	fn any_keyboard(&self) -> bool {
		self.entries.iter().any(|(_, h)| h.layout.has_keyboard())
	}

	// Whether any bound device reports a pointer.
	fn any_pointer(&self) -> bool {
		self.entries.iter().any(|(_, h)| h.layout.has_pointer())
	}
}

// The pointer-event sink: the server end of the channel InputService reads,
// set once at startup. Pointer reports send through it from wherever they are
// reaped - the service loop or a wait deep inside a disk transfer.
static PTR_SINK: AtomicU64 = AtomicU64::new(0);

// One addressed device's inventory record: its root port and slot (the state a
// detach tears down), plus the identity its device descriptor reported and the
// role the driver bound it to - the `usb.list` inventory.
#[derive(Clone, Copy)]
struct SlotRec {
	port: u32,
	slot: u32,
	speed: u32,
	vendor: u16,
	product: u16,
	class: u8,
	kind: u8,
}

// The roles a device may be bound to, reported in the inventory.
const KIND_DEVICE: u8 = 0;
const KIND_HUB: u8 = 1;
const KIND_KEYBOARD: u8 = 2;
const KIND_STORAGE: u8 = 3;
const KIND_POINTER: u8 = 4;

// The addressed devices, by root port - the state hot-plug works against and the
// inventory `usb.list` serves. An attach enumerates a root port only when no slot
// is recorded for it; a detach disables every slot recorded for it (a hub takes
// its downstream devices along). Grows with the bus - the controller's slot count
// is the only bound, never an artificial cap that would silently drop devices.
struct Slots {
	entries: Vec<SlotRec>,
}

impl Slots {
	const fn new() -> Slots {
		Slots { entries: Vec::new() }
	}

	// Record one addressed device's inventory entry.
	fn record(&mut self, rec: SlotRec) {
		self.entries.push(rec);
	}

	// Update the recorded role of the device in `slot` once it is classified.
	fn set_kind(&mut self, slot: u32, kind: u8) {
		if let Some(rec) = self.entries.iter_mut().find(|r| r.slot == slot) {
			rec.kind = kind;
		}
	}

	// Whether any addressed device sits on this root port.
	fn has_port(&self, port: u32) -> bool {
		self.entries.iter().any(|r| r.port == port)
	}

	// Remove and return one slot on this root port (call until None on a detach).
	fn take_port(&mut self, port: u32) -> Option<u32> {
		let i: usize = self.entries.iter().position(|r| r.port == port)?;
		Some(self.entries.swap_remove(i).slot)
	}
}

// A configured USB mass-storage device (Bulk-Only Transport): the bulk IN and OUT
// endpoints' device context indices, addresses (for stall recovery) and transfer
// rings, the interface number (for the BOT reset), a page for the sector data (the
// CBW/CSW frames ride in the device's scratch page), and the rolling CBW tag.
struct Storage {
	dci_in: u32,
	dci_out: u32,
	ep_in_addr: u8,
	ep_out_addr: u8,
	iface: u16,
	ring_in: Ring,
	ring_out: Ring,
	data_virt: u64,
	data_phys: u64,
	tag: u32,
	// The unit's size in bytes, from READ CAPACITY at configuration - answered to
	// OP_CAPACITY queries for the `lsblk` inventory.
	capacity: u64,
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let mut buf: [u8; 96] = [0u8; 96];
		let info_size: usize = core::mem::size_of::<DeviceInfo>();
		// receive "DEVICE" + DeviceInfo + the DeviceMemory capability.
		let (device_handle, _info): (u64, DeviceInfo) = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if handle != 0 && len >= 6 + info_size && &buf[..6] == b"DEVICE" => (handle, (buf.as_ptr().add(6) as *const DeviceInfo).read_unaligned()),
			_ => exit(),
		};
		// receive "IRQ" + the controller's MSI-X Interrupt capability (the keyboard
		// service loop blocks on it; bring-up itself polls).
		let irq: u64 = match recv_blocking(bootstrap, &mut buf) {
			Received::Message { len, handle } if handle != 0 && len >= 3 && &buf[..3] == b"IRQ" => handle,
			_ => exit(),
		};
		// map the controller's register file.
		let base: u64 = syscall(SYS_DEVICE_MEMORY_MAP, device_handle, 0, 0, 0);
		if sys_is_err(base) {
			exit();
		}
		let mut hc: Xhci = match bring_up(base) {
			Some(hc) => hc,
			None => exit(),
		};
		// enumerate the root-hub ports, address every connected device (expanding hubs
		// recursively), and configure every HID device and the first mass-storage
		// device found anywhere on the bus. Every addressed device's slot is recorded
		// by root port, the state runtime attach/detach works against.
		let mut devices: u32 = 0;
		let mut hids: Hids = Hids::new();
		let mut storage: Option<(UsbDevice, Storage)> = None;
		let mut slots: Slots = Slots::new();
		let mut port: u32 = 1;
		while port <= hc.ports {
			if let Some(dev) = attach_port(&mut hc, port) {
				register_device(&mut hc, dev, &mut slots, &mut devices, &mut hids, &mut storage);
			}
			port += 1;
		}
		// the block channel a mass-storage device is served over: the client end rides
		// up with the report (DeviceManager routes it to a StorageService instance).
		// Always created - a stick hot-plugged later serves over the same channel, and
		// with none attached requests are answered with an error status.
		let (blk_server, blk_client): (u64, u64) = channel().unwrap_or_else(|| exit());
		// the USB bus query channel: the client end follows the report under "USBBUS"
		// (DeviceManager routes it on to PermissionManager, which grants it to the
		// `lsusb` command); the driver serves the typed `usb` interface on the server
		// end - the live inventory of the devices it addressed.
		let (usbq_server, usbq_client): (u64, u64) = channel().unwrap_or_else(|| exit());
		// the pointer-event channel: the client end follows under "POINTER"
		// (DeviceManager routes it to InputService), and a pointing device's reports
		// send normalized events over the server end - the same wire format the
		// virtio-input pointer speaks. Always created, so a pointer hot-plugged later
		// serves over the same channel.
		let (ptr_server, ptr_client): (u64, u64) = channel().unwrap_or_else(|| exit());
		PTR_SINK.store(ptr_server, Ordering::Relaxed);
		// report in, then serve the bus for the life of the system: HID reports,
		// block requests, and runtime attach / detach.
		let mut report: [u8; 64] = [0u8; 64];
		let mut n: usize = 0;
		for &b in b"driver.xhci: online (" {
			report[n] = b;
			n += 1;
		}
		n += push_decimal(&mut report[n..], devices as u64);
		for &b in b" device(s))" {
			report[n] = b;
			n += 1;
		}
		if hids.any_keyboard() {
			for &b in b" (keyboard)" {
				report[n] = b;
				n += 1;
			}
		}
		if hids.any_pointer() {
			for &b in b" (pointer)" {
				report[n] = b;
				n += 1;
			}
		}
		if storage.is_some() {
			for &b in b" (storage)" {
				report[n] = b;
				n += 1;
			}
		}
		send_blocking(bootstrap, &report[..n], blk_client);
		send_blocking(bootstrap, b"USBBUS", usbq_client);
		send_blocking(bootstrap, b"POINTER", ptr_client);
		service_loop(&mut hc, &mut slots, hids, storage, blk_server, usbq_server, irq);
	}
}

// Reset the controller and build its data structures: the device context base
// array (with scratchpad buffers when the controller asks for them), the command
// ring, and a one-segment event ring on interrupter 0. Leaves the controller
// running with all device slots enabled. None if any allocation or handshake fails.
unsafe fn bring_up(base: u64) -> Option<Xhci> {
	unsafe {
		let op: u64 = base + r8(base + CAP_CAPLENGTH) as u64;
		let hcs1: u32 = r32(base + CAP_HCSPARAMS1);
		let slots: u32 = hcs1 & 0xff;
		let ports: u32 = hcs1 >> 24;
		let csz: bool = r32(base + CAP_HCCPARAMS1) & (1 << 2) != 0;
		let db: u64 = base + (r32(base + CAP_DBOFF) & !0x3) as u64;
		let ir0: u64 = base + (r32(base + CAP_RTSOFF) & !0x1f) as u64 + 0x20;

		// halt (clear run/stop) and reset the controller, then wait until it is ready.
		w32(op + OP_USBCMD, r32(op + OP_USBCMD) & !CMD_RUN);
		wait_set(op + OP_USBSTS, STS_HCHALTED)?;
		w32(op + OP_USBCMD, r32(op + OP_USBCMD) | CMD_HCRST);
		wait_clear(op + OP_USBCMD, CMD_HCRST)?;
		wait_clear(op + OP_USBSTS, STS_CNR)?;

		// the device context base address array; entry 0 points at the scratchpad
		// pointer array when the controller asks for scratchpad pages.
		let (_h, dcbaa_virt, dcbaa_phys): (u64, u64, u64) = dma_page()?;
		let hcs2: u32 = r32(base + CAP_HCSPARAMS2);
		let scratchpads: u32 = ((hcs2 >> 21) & 0x1f) << 5 | (hcs2 >> 27) & 0x1f;
		if scratchpads > 0 {
			let (_ah, arr_virt, arr_phys): (u64, u64, u64) = dma_page()?;
			let mut i: u32 = 0;
			while i < scratchpads.min(512) {
				let (_ph, _pv, page_phys): (u64, u64, u64) = dma_page()?;
				((arr_virt + i as u64 * 8) as *mut u64).write_volatile(page_phys);
				i += 1;
			}
			((dcbaa_virt) as *mut u64).write_volatile(arr_phys);
		}
		w64(op + OP_DCBAAP, dcbaa_phys);

		// the command ring, with a link TRB in the last slot wrapping it (toggle cycle).
		let cmd: Ring = Ring::new()?;
		w64(op + OP_CRCR, cmd.phys | 1);

		// a one-segment event ring on interrupter 0: the segment table needs one
		// 16-byte entry, carved from the tail of the DCBAA page (64-byte aligned).
		let (_eh, evt_virt, evt_phys): (u64, u64, u64) = dma_page()?;
		let erst_virt: u64 = dcbaa_virt + 2048;
		let erst_phys: u64 = dcbaa_phys + 2048;
		(erst_virt as *mut u64).write_volatile(evt_phys);
		((erst_virt + 8) as *mut u32).write_volatile(RING_TRBS as u32);
		((erst_virt + 12) as *mut u32).write_volatile(0);
		w32(ir0 + IR_ERSTSZ, 1);
		w64(ir0 + IR_ERSTBA, erst_phys);
		w64(ir0 + IR_ERDP, evt_phys);
		// enable interrupter 0 with no moderation: each event raises the controller's
		// MSI-X vector (the keyboard service loop blocks on it; bring-up polls, which
		// interrupts do not disturb).
		w32(ir0 + IR_IMOD, 0);
		w32(ir0 + IR_IMAN, IMAN_IE | IMAN_IP);

		// enable every device slot and start the controller (with interrupts on).
		w32(op + OP_CONFIG, slots);
		w32(op + OP_USBCMD, r32(op + OP_USBCMD) | CMD_RUN | CMD_INTE);
		wait_clear(op + OP_USBSTS, STS_HCHALTED)?;

		Some(Xhci { op, ir0, db, ctx_size: if csz { 64 } else { 32 }, ports, cmd, evt_virt, evt_phys, evt_index: 0, evt_cycle: 1, dcbaa_virt })
	}
}

// Spin until the masked bits at `addr` are all set. None on budget exhaustion.
unsafe fn wait_set(addr: u64, mask: u32) -> Option<()> {
	unsafe {
		let mut spins: u32 = 0;
		while r32(addr) & mask != mask {
			spins += 1;
			if spins > SPIN_BUDGET {
				return None;
			}
			if spins % 4096 == 0 {
				yield_now();
			}
		}
		Some(())
	}
}

// Spin until the masked bits at `addr` are all clear. None on budget exhaustion.
unsafe fn wait_clear(addr: u64, mask: u32) -> Option<()> {
	unsafe {
		let mut spins: u32 = 0;
		while r32(addr) & mask != 0 {
			spins += 1;
			if spins > SPIN_BUDGET {
				return None;
			}
			if spins % 4096 == 0 {
				yield_now();
			}
		}
		Some(())
	}
}

// Write PORTSC preserving its state: the RW1C change bits are masked out (so the
// read-modify-write cannot clear them by accident) and `set` is OR-ed in.
unsafe fn portsc_write(hc: &Xhci, port: u32, set: u32) {
	unsafe {
		let addr: u64 = hc.op + OP_PORTSC_BASE + (port - 1) as u64 * 0x10;
		let value: u32 = r32(addr) & !PORTSC_RW1C;
		w32(addr, value | set);
	}
}

// Push one TRB onto the command ring and ring the command doorbell.
unsafe fn command(hc: &mut Xhci, param: u64, status: u32, control: u32) {
	unsafe {
		hc.cmd.push(param, status, control);
		w32(hc.db, 0);
	}
}

// Take one event off the event ring if one is pending, publishing the new dequeue
// pointer. Returns (param, status, control), or None when the ring is empty.
unsafe fn take_event(hc: &mut Xhci) -> Option<(u64, u32, u32)> {
	unsafe {
		let trb: u64 = hc.evt_virt + hc.evt_index * 16;
		let control: u32 = ((trb + 12) as *const u32).read_volatile();
		if control & TRB_CYCLE != hc.evt_cycle {
			return None;
		}
		let param: u64 = (trb as *const u64).read_volatile();
		let status: u32 = ((trb + 8) as *const u32).read_volatile();
		hc.evt_index += 1;
		if hc.evt_index == RING_TRBS {
			hc.evt_index = 0;
			hc.evt_cycle ^= 1;
		}
		w64(hc.ir0 + IR_ERDP, hc.evt_phys + hc.evt_index * 16 | ERDP_EHB);
		Some((param, status, control))
	}
}

// Poll the event ring until an event of `wanted` type arrives. Port-status-change
// events are skipped (enumeration reads PORTSC directly). Returns (param, status,
// control) of the matching event, or None on budget exhaustion or an unexpected
// event type.
unsafe fn wait_event(hc: &mut Xhci, wanted: u32) -> Option<(u64, u32, u32)> {
	unsafe {
		let mut spins: u32 = 0;
		loop {
			if let Some((param, status, control)) = take_event(hc) {
				let kind: u32 = control >> 10 & 0x3f;
				if kind == wanted {
					return Some((param, status, control));
				}
				if kind != TRB_EV_PORT_STATUS {
					return None;
				}
				continue;
			}
			spins += 1;
			if spins > SPIN_BUDGET {
				return None;
			}
			if spins % 4096 == 0 {
				yield_now();
			}
		}
	}
}

// Issue one command and wait for its completion event. Returns the event's slot id
// (control bits 31:24) on success, None on a non-success completion code.
unsafe fn command_and_wait(hc: &mut Xhci, param: u64, status: u32, control: u32) -> Option<u32> {
	unsafe {
		command(hc, param, status, control);
		let (_p, ev_status, ev_control): (u64, u32, u32) = wait_event(hc, TRB_EV_CMD_COMPLETE)?;
		if ev_status >> 24 != CC_SUCCESS {
			return None;
		}
		Some(ev_control >> 24)
	}
}

// Bring up the device on root-hub port `port`: reset the port if a device is
// connected, then give it a slot, an address and an identity. Returns None when
// the port is empty or any step fails.
unsafe fn attach_port(hc: &mut Xhci, port: u32) -> Option<UsbDevice> {
	unsafe {
		let addr: u64 = hc.op + OP_PORTSC_BASE + (port - 1) as u64 * 0x10;
		if r32(addr) & PORTSC_CCS == 0 {
			return None;
		}
		// a USB2 device needs a port reset to reach the enabled state; a USB3 port
		// enables itself on attach. Reset when not yet enabled, then wait for it.
		if r32(addr) & PORTSC_PED == 0 {
			portsc_write(hc, port, PORTSC_PP | PORTSC_PR);
			wait_set(addr, PORTSC_PRC)?;
		}
		wait_set(addr, PORTSC_PED)?;
		// acknowledge the change bits the attach/reset raised.
		portsc_write(hc, port, PORTSC_CSC | PORTSC_PEC | PORTSC_PRC | PORTSC_WRC | PORTSC_PLC | PORTSC_CEC);
		let speed: u32 = r32(addr) >> 10 & 0xf;
		address_device(hc, port, 0, speed)
	}
}

// Give an attached, freshly reset device a slot and an address, and read its
// identity: enable a slot, hang its device context off the DCBAA, address it (the
// slot context names the root port, the route string below it, and the speed), fix
// endpoint 0's packet size up from the descriptor head, and read the full device
// descriptor. Shared by the root ports and the ports of a hub (whose devices carry
// a non-zero route). Returns None when any step fails.
unsafe fn address_device(hc: &mut Xhci, root_port: u32, route: u32, speed: u32) -> Option<UsbDevice> {
	unsafe {
		// a slot for the device, its device context, and the default endpoint's ring.
		let slot: u32 = command_and_wait(hc, 0, 0, TRB_ENABLE_SLOT << 10)?;
		if slot == 0 || slot > 255 {
			return None;
		}
		let (_dh, _ctx_virt, ctx_phys): (u64, u64, u64) = dma_page()?;
		((hc.dcbaa_virt + slot as u64 * 8) as *mut u64).write_volatile(ctx_phys);

		let (_ih, in_virt, in_phys): (u64, u64, u64) = dma_page()?;
		let (_bh, data_virt, data_phys): (u64, u64, u64) = dma_page()?;
		let mut dev: UsbDevice = UsbDevice { slot, port: root_port, route, speed, ep0: Ring::new()?, in_virt, in_phys, data_virt, data_phys, vendor: 0, product: 0, class: 0 };
		// no HID device can have reports in flight during bring-up (report TRBs are
		// only posted once the service loop starts), so the waits see no HID events.
		let mut pending: Hids = Hids::new();

		// address the device: an input context whose slot context names the port and
		// whose endpoint-0 context points at the transfer ring.
		write_address_contexts(hc, &dev, initial_packet_size(speed));
		command_and_wait(hc, in_phys, 0, TRB_ADDRESS_DEVICE << 10 | slot << 24)?;

		// read the descriptor head first: its bMaxPacketSize0 field tells the real
		// default-endpoint packet size, which full-speed devices are allowed to vary.
		control_in(hc, &mut pending, &mut dev, DESC_DEVICE, 8)?;
		let mps: u32 = r8(data_virt + 7) as u32;
		if mps != initial_packet_size(speed) && mps >= 8 {
			// fix endpoint 0 up with an evaluate-context command, then re-read.
			write_address_contexts(hc, &dev, mps);
			// evaluate-context consumes only the endpoint-0 add flag.
			((in_virt + 4) as *mut u32).write_volatile(1 << 1);
			command_and_wait(hc, in_phys, 0, TRB_EVALUATE_CONTEXT << 10 | slot << 24)?;
		}
		control_in(hc, &mut pending, &mut dev, DESC_DEVICE, 18)?;
		dev.class = r8(data_virt + 4);
		dev.vendor = r8(data_virt + 8) as u16 | (r8(data_virt + 9) as u16) << 8;
		dev.product = r8(data_virt + 10) as u16 | (r8(data_virt + 11) as u16) << 8;
		Some(dev)
	}
}

// The default-endpoint max packet size the port speed implies, used until the
// device descriptor names the real one: 512 for SuperSpeed, 64 for high speed,
// 8 for full/low speed.
fn initial_packet_size(speed: u32) -> u32 {
	match speed {
		SPEED_SUPER => 512,
		SPEED_HIGH => 64,
		_ => 8,
	}
}

// Register one addressed device: print its identity, record its slot by root port
// (the state a later detach tears down), count it, and classify it - a hub is
// expanded (its ports enumerated, each downstream device landing back here
// recursively), every HID device and the first mass-storage device are
// configured and kept for the service loop, anything else is left addressed.
unsafe fn register_device(hc: &mut Xhci, mut dev: UsbDevice, slots: &mut Slots, devices: &mut u32, hids: &mut Hids, storage: &mut Option<(UsbDevice, Storage)>) {
	unsafe {
		report_device(&dev);
		slots.record(SlotRec { port: dev.port, slot: dev.slot, speed: dev.speed, vendor: dev.vendor, product: dev.product, class: dev.class, kind: KIND_DEVICE });
		*devices += 1;
		if dev.class == CLASS_HUB {
			slots.set_kind(dev.slot, KIND_HUB);
			expand_hub(hc, &mut dev, slots, devices, hids, storage);
		} else if let Some(h) = configure_hid(hc, &mut dev) {
			slots.set_kind(dev.slot, if h.layout.has_keyboard() { KIND_KEYBOARD } else { KIND_POINTER });
			hids.entries.push((dev, h));
		} else if storage.is_none()
			&& let Some(st) = configure_storage(hc, &mut dev)
		{
			slots.set_kind(dev.slot, KIND_STORAGE);
			*storage = Some((dev, st));
		}
	}
}

// Configure an addressed hub and enumerate the devices on its ports: select its
// configuration, read the hub class descriptor for the port count, power each port
// up, and bring up whatever is connected. Each addressed downstream device runs
// through `register_device`, so a hub found downstream expands recursively and a
// keyboard or disk behind any tier of hubs is configured like a root one.
unsafe fn expand_hub(hc: &mut Xhci, hub: &mut UsbDevice, slots: &mut Slots, devices: &mut u32, hids: &mut Hids, storage: &mut Option<(UsbDevice, Storage)>) {
	unsafe {
		// no HID device is serving yet, so the control waits see no HID events.
		let mut pending: Hids = Hids::new();
		// select the hub's configuration (the head of its config descriptor names it).
		if control_in(hc, &mut pending, hub, DESC_CONFIG, 9).is_none() {
			return;
		}
		let config_value: u16 = r8(hub.data_virt + 5) as u16;
		if control_nodata(hc, &mut pending, hub, 0x00, REQ_SET_CONFIGURATION, config_value, 0).is_none() {
			return;
		}
		// the hub class descriptor: bNbrPorts rides at offset 2.
		if control_in_req(hc, &mut pending, hub, RT_CLASS_DEVICE_IN, REQ_GET_DESCRIPTOR, DESC_HUB << 8, 0, 9).is_none() {
			return;
		}
		let ports: u32 = r8(hub.data_virt + 2) as u32;
		// the route string tier this hub's ports occupy: one nibble per tier, the
		// first free nibble above the hub's own route.
		let mut shift: u32 = 0;
		while shift < 20 && hub.route >> shift & 0xf != 0 {
			shift += 4;
		}
		let mut port: u32 = 1;
		while port <= ports.min(15) {
			if let Some(dev) = attach_hub_port(hc, hub, port, shift) {
				register_device(hc, dev, slots, devices, hids, storage);
			}
			port += 1;
		}
	}
}

// Bring up the device on one hub port: power the port, check a device is
// connected, reset the port through the hub's SET_FEATURE(PORT_RESET) (waiting on
// the reset-change flag), read the attached speed off the port status, and address
// the device with the hub's route string extended by this port at `shift`. Returns
// None when the port is empty or any step fails.
unsafe fn attach_hub_port(hc: &mut Xhci, hub: &mut UsbDevice, port: u32, shift: u32) -> Option<UsbDevice> {
	unsafe {
		let mut pending: Hids = Hids::new();
		// power the port and wait for the power state to read back.
		control_nodata(hc, &mut pending, hub, RT_CLASS_PORT, REQ_SET_FEATURE, HUB_FEAT_PORT_POWER, port as u16)?;
		let mut spins: u32 = 0;
		while hub_port_status(hc, &mut pending, hub, port)? & PORT_STATUS_POWER == 0 {
			spins += 1;
			if spins > 1000 {
				return None;
			}
			yield_now();
		}
		// a device must be connected; acknowledge the connect-change flag.
		if hub_port_status(hc, &mut pending, hub, port)? & PORT_STATUS_CCS == 0 {
			return None;
		}
		control_nodata(hc, &mut pending, hub, RT_CLASS_PORT, REQ_CLEAR_FEATURE, HUB_FEAT_C_CONNECTION, port as u16)?;
		// reset the port and wait for the reset-change flag (the status word's
		// change half is its high 16 bits), then acknowledge it.
		control_nodata(hc, &mut pending, hub, RT_CLASS_PORT, REQ_SET_FEATURE, HUB_FEAT_PORT_RESET, port as u16)?;
		spins = 0;
		loop {
			let change: u16 = (hub_port_change(hc, &mut pending, hub, port)?) & PORT_CHANGE_RESET;
			if change != 0 {
				break;
			}
			spins += 1;
			if spins > 1000 {
				return None;
			}
			yield_now();
		}
		control_nodata(hc, &mut pending, hub, RT_CLASS_PORT, REQ_CLEAR_FEATURE, HUB_FEAT_C_RESET, port as u16)?;
		// the attached speed, from the port status bits (default full speed).
		let status: u16 = hub_port_status(hc, &mut pending, hub, port)?;
		let speed: u32 = if status & PORT_STATUS_LOW_SPEED != 0 {
			2
		} else if status & PORT_STATUS_HIGH_SPEED != 0 {
			SPEED_HIGH
		} else {
			1
		};
		address_device(hc, hub.port, hub.route | (port & 0xf) << shift, speed)
	}
}

// Read one hub port's status word (the low half of the GET_STATUS reply).
unsafe fn hub_port_status(hc: &mut Xhci, hids: &mut Hids, hub: &mut UsbDevice, port: u32) -> Option<u16> {
	unsafe {
		control_in_req(hc, hids, hub, RT_CLASS_PORT_IN, REQ_GET_STATUS, 0, port as u16, 4)?;
		Some(r8(hub.data_virt) as u16 | (r8(hub.data_virt + 1) as u16) << 8)
	}
}

// Read one hub port's change word (the high half of the GET_STATUS reply).
unsafe fn hub_port_change(hc: &mut Xhci, hids: &mut Hids, hub: &mut UsbDevice, port: u32) -> Option<u16> {
	unsafe {
		control_in_req(hc, hids, hub, RT_CLASS_PORT_IN, REQ_GET_STATUS, 0, port as u16, 4)?;
		Some(r8(hub.data_virt + 2) as u16 | (r8(hub.data_virt + 3) as u16) << 8)
	}
}

// Fill the device's input context for an address-device command: the input
// control context adds the slot and endpoint-0 contexts, the slot context names
// the root-hub port and speed, and the endpoint-0 context is a control endpoint
// with max packet size `mps` whose transfer ring is the device's.
unsafe fn write_address_contexts(hc: &Xhci, dev: &UsbDevice, mps: u32) {
	unsafe {
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		// input control context: add slot (A0) + endpoint 0 (A1).
		((dev.in_virt + 4) as *mut u32).write_volatile(0x3);
		// slot context: one context entry, the device's route string, speed and root port.
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(1 << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		// endpoint-0 context: a control endpoint (type 4), error count 3, the ring's
		// physical base with the producer's cycle state, average TRB length 8.
		let ep0_ctx: u64 = dev.in_virt + 2 * hc.ctx_size;
		((ep0_ctx + 4) as *mut u32).write_volatile(mps << 16 | 4 << 3 | 3 << 1);
		((ep0_ctx + 8) as *mut u32).write_volatile((dev.ep0.phys | dev.ep0.cycle as u64) as u32);
		((ep0_ctx + 12) as *mut u32).write_volatile((dev.ep0.phys >> 32) as u32);
		((ep0_ctx + 16) as *mut u32).write_volatile(8);
	}
}

// Read `len` bytes of descriptor `desc` from the device into its data page with a
// standard GET_DESCRIPTOR control transfer on the default endpoint.
unsafe fn control_in(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, desc: u16, len: u16) -> Option<()> {
	unsafe { control_in_req(hc, hids, dev, 0x80, REQ_GET_DESCRIPTOR, desc << 8, 0, len) }
}

// Run one IN control request on the default endpoint, the data landing in the
// device's data page: setup stage (the 8-byte request rides in the TRB itself), IN
// data stage, OUT status stage, then the doorbell and the transfer completion
// event. The hub class requests (GET_STATUS on a port, the hub descriptor) ride
// through here too. A stall halts endpoint 0; it is recovered before reporting
// failure, so the endpoint stays usable.
unsafe fn control_in_req(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, request_type: u8, request: u8, value: u16, index: u16, len: u16) -> Option<()> {
	unsafe {
		let setup: u64 = request_type as u64 | (request as u64) << 8 | (value as u64) << 16 | (index as u64) << 32 | (len as u64) << 48;
		dev.ep0.push(setup, 8, TRB_SETUP << 10 | TRB_IDT | TRB_TRT_IN);
		dev.ep0.push(dev.data_phys, len as u32, TRB_DATA << 10 | TRB_DIR_IN);
		dev.ep0.push(0, 0, TRB_STATUS << 10 | TRB_IOC);
		// ring the device slot's doorbell for the default control endpoint (DCI 1).
		w32(hc.db + dev.slot as u64 * 4, 1);
		let code: u32 = wait_transfer(hc, hids, dev.slot, 1)?;
		if code == CC_STALL {
			recover_ep0(hc, hids, dev);
			return None;
		}
		if code != CC_SUCCESS && code != CC_SHORT_PACKET { None } else { Some(()) }
	}
}

// Issue a data-less control request (SET_CONFIGURATION, the HID SET_PROTOCOL, the
// stall-recovery CLEAR_FEATURE, the BOT reset) on the default endpoint: a setup
// stage with no data stage, then the IN-direction status stage, the doorbell and
// the completion event. A stall halts endpoint 0; it is recovered before reporting
// failure, so a request the device rejects leaves the endpoint usable.
unsafe fn control_nodata(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, request_type: u8, request: u8, value: u16, index: u16) -> Option<()> {
	unsafe {
		let setup: u64 = request_type as u64 | (request as u64) << 8 | (value as u64) << 16 | (index as u64) << 32;
		dev.ep0.push(setup, 8, TRB_SETUP << 10 | TRB_IDT);
		dev.ep0.push(0, 0, TRB_STATUS << 10 | TRB_DIR_IN | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, 1);
		let code: u32 = wait_transfer(hc, hids, dev.slot, 1)?;
		if code == CC_STALL {
			recover_ep0(hc, hids, dev);
			return None;
		}
		if code != CC_SUCCESS { None } else { Some(()) }
	}
}

// Recover the halted default endpoint after a stall: a Reset Endpoint command
// clears the controller-side halt, and a Set TR Dequeue Pointer repositions the
// transfer ring past the abandoned control transfer (at the producer's current
// position). Endpoint 0 has no device-side halt feature, so no CLEAR_FEATURE.
unsafe fn recover_ep0(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice) {
	unsafe {
		reset_endpoint(hc, hids, dev.slot, 1, dev.ep0.phys + dev.ep0.index * 16 | dev.ep0.cycle as u64);
	}
}

// Recover one halted bulk endpoint of the storage device after a stall: the
// controller-side Reset Endpoint + Set TR Dequeue Pointer pair, then the device-side
// CLEAR_FEATURE(ENDPOINT_HALT) to the endpoint's address, resetting its data toggle.
unsafe fn recover_bulk(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, st: &mut Storage, dir_in: bool) {
	unsafe {
		let (dci, addr, dequeue): (u32, u8, u64) = if dir_in { (st.dci_in, st.ep_in_addr, st.ring_in.phys + st.ring_in.index * 16 | st.ring_in.cycle as u64) } else { (st.dci_out, st.ep_out_addr, st.ring_out.phys + st.ring_out.index * 16 | st.ring_out.cycle as u64) };
		reset_endpoint(hc, hids, dev.slot, dci, dequeue);
		let _ = control_nodata(hc, hids, dev, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, addr as u16);
	}
}

// The controller half of stall recovery: Reset Endpoint clears the endpoint's
// halted state, Set TR Dequeue Pointer repositions its transfer ring to `dequeue`
// (the producer's current position with the cycle state in bit 0), abandoning the
// stalled TD. HID events arriving during the command waits are serviced.
unsafe fn reset_endpoint(hc: &mut Xhci, hids: &mut Hids, slot: u32, dci: u32, dequeue: u64) {
	unsafe {
		command(hc, 0, 0, TRB_RESET_ENDPOINT << 10 | dci << 16 | slot << 24);
		let _ = wait_command(hc, hids);
		command(hc, dequeue, 0, TRB_SET_TR_DEQUEUE << 10 | dci << 16 | slot << 24);
		let _ = wait_command(hc, hids);
	}
}

// Wait for a command completion event, servicing HID events that arrive in the
// meantime inline. Returns the completion code, or None on budget exhaustion.
unsafe fn wait_command(hc: &mut Xhci, hids: &mut Hids) -> Option<u32> {
	unsafe {
		let mut spins: u32 = 0;
		loop {
			if let Some((_p, status, control)) = take_event(hc) {
				if control >> 10 & 0x3f == TRB_EV_CMD_COMPLETE {
					return Some(status >> 24);
				}
				handle_hid_event(hc, hids, status, control);
				continue;
			}
			spins += 1;
			if spins > SPIN_BUDGET {
				return None;
			}
			if spins % 4096 == 0 {
				yield_now();
			}
		}
	}
}

// The last-resort transport recovery (the Bulk-Only spec's reset sequence): the
// Mass Storage Reset class request returns the device's BOT state machine to idle,
// then both bulk endpoints are unhalted, so the next CBW starts a clean transaction.
unsafe fn bot_reset(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, st: &mut Storage) {
	unsafe {
		let _ = control_nodata(hc, hids, dev, RT_CLASS_INTERFACE, BOT_REQ_RESET, 0, st.iface);
		recover_bulk(hc, hids, dev, st, true);
		recover_bulk(hc, hids, dev, st, false);
	}
}

// Configure the device's HID function, if it has one: read the configuration
// descriptor, find a HID interface (any subclass - keyboards, pointing devices
// and multimedia controls alike), its interrupt IN endpoint and its report
// descriptor's length, bring the endpoint up with a configure-endpoint command,
// select the configuration, then read and parse the report descriptor - the
// layout the device's input reports decode against. A boot-subclass keyboard
// whose report descriptor cannot be read falls back to the fixed boot protocol.
// None when the device carries no HID function whose reports the system consumes,
// or any step fails.
unsafe fn configure_hid(hc: &mut Xhci, dev: &mut UsbDevice) -> Option<Hid> {
	unsafe {
		// no HID device is serving yet, so the control waits see no HID events.
		let mut pending: Hids = Hids::new();
		// the configuration descriptor head names the total length; read it whole.
		control_in(hc, &mut pending, dev, DESC_CONFIG, 9)?;
		let total: u16 = (r8(dev.data_virt + 2) as u16 | (r8(dev.data_virt + 3) as u16) << 8).min(1024);
		let config_value: u16 = r8(dev.data_virt + 5) as u16;
		control_in(hc, &mut pending, dev, DESC_CONFIG, total)?;

		// walk the descriptors for a HID interface, its report descriptor's length
		// (the HID class descriptor rides between the interface and its endpoints)
		// and its interrupt IN endpoint.
		let mut offset: u64 = 0;
		let mut in_hid: bool = false;
		let mut boot_keyboard: bool = false;
		let mut iface: u16 = 0;
		let mut desc_len: u16 = 0;
		let mut found: Option<(u32, u32, u32)> = None; // (dci, mps, interval)
		while offset + 2 <= total as u64 {
			let length: u64 = r8(dev.data_virt + offset) as u64;
			let kind: u8 = r8(dev.data_virt + offset + 1);
			if length < 2 {
				break;
			}
			if kind == DT_INTERFACE && found.is_none() {
				in_hid = r8(dev.data_virt + offset + 5) == CLASS_HID;
				if in_hid {
					iface = r8(dev.data_virt + offset + 2) as u16;
					boot_keyboard = r8(dev.data_virt + offset + 6) == SUBCLASS_BOOT && r8(dev.data_virt + offset + 7) == PROTOCOL_KEYBOARD;
				}
			}
			if kind == DT_HID && in_hid && found.is_none() && length >= 9 {
				// wDescriptorLength of the (first) class descriptor, the report one.
				desc_len = r8(dev.data_virt + offset + 7) as u16 | (r8(dev.data_virt + offset + 8) as u16) << 8;
			}
			if kind == DT_ENDPOINT && in_hid && found.is_none() {
				let ep_addr: u8 = r8(dev.data_virt + offset + 2);
				let attrs: u8 = r8(dev.data_virt + offset + 3);
				if ep_addr & 0x80 != 0 && attrs & 0x3 == EP_ATTR_INTERRUPT {
					let mps: u32 = r8(dev.data_virt + offset + 4) as u32 | (r8(dev.data_virt + offset + 5) as u32) << 8;
					let interval: u32 = ep_interval(dev.speed, r8(dev.data_virt + offset + 6) as u32);
					found = Some(((ep_addr & 0xf) as u32 * 2 + 1, mps, interval));
				}
			}
			offset += length;
		}
		let (dci, mps, interval): (u32, u32, u32) = found?;

		// bring the endpoint up: the input context adds the slot context (its context
		// entries grown to cover the new DCI) and the interrupt IN endpoint context.
		let ring: Ring = Ring::new()?;
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		((dev.in_virt + 4) as *mut u32).write_volatile(1 | 1 << dci);
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(dci << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		// endpoint context: interrupt IN (type 7), error count 3, the polling
		// interval, the ring's base, and the report size as the average/ESIT payload.
		let ep_ctx: u64 = dev.in_virt + (1 + dci as u64) * hc.ctx_size;
		(ep_ctx as *mut u32).write_volatile(interval << 16);
		((ep_ctx + 4) as *mut u32).write_volatile(mps << 16 | 7 << 3 | 3 << 1);
		((ep_ctx + 8) as *mut u32).write_volatile((ring.phys | ring.cycle as u64) as u32);
		((ep_ctx + 12) as *mut u32).write_volatile((ring.phys >> 32) as u32);
		((ep_ctx + 16) as *mut u32).write_volatile(8 | mps << 16);
		command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24)?;

		// select the configuration, then read and parse the report descriptor. The
		// device stays in its default report protocol - the layout tells the driver
		// what each report carries, no boot arrangement needed. A boot-subclass
		// keyboard whose descriptor cannot be read is put into the boot protocol
		// instead and decoded with the fixed boot layout.
		control_nodata(hc, &mut pending, dev, 0x00, REQ_SET_CONFIGURATION, config_value, 0)?;
		let layout: hid::Layout = match desc_len {
			0 => hid::Layout::empty(),
			_ => match control_in_req(hc, &mut pending, dev, RT_INTERFACE_IN, REQ_GET_DESCRIPTOR, DESC_REPORT << 8, iface, desc_len.min(1024)) {
				Some(()) => hid::parse(core::slice::from_raw_parts(dev.data_virt as *const u8, desc_len.min(1024) as usize)),
				None => hid::Layout::empty(),
			},
		};
		let layout: hid::Layout = if layout.is_useful() {
			layout
		} else if boot_keyboard {
			control_nodata(hc, &mut pending, dev, 0x21, HID_REQ_SET_PROTOCOL, 0, iface)?;
			hid::boot_keyboard()
		} else {
			return None;
		};
		Some(Hid { dci, ring, layout, posted: false, prevs: Vec::new(), mods: Mods::default(), x: 0, y: 0, buttons: 0 })
	}
}

// The xHCI endpoint-context interval field for an interrupt endpoint: the exponent
// of the service interval in 125 us microframes. High/SuperSpeed descriptors carry
// the exponent + 1 already; a full/low-speed bInterval counts 1 ms frames, so find
// the smallest exponent whose period covers it (bInterval * 8 microframes).
fn ep_interval(speed: u32, b_interval: u32) -> u32 {
	if speed == SPEED_HIGH || speed == SPEED_SUPER {
		return b_interval.clamp(1, 16) - 1;
	}
	let mut exp: u32 = 3;
	while exp < 15 && 1 << (exp - 3) < b_interval {
		exp += 1;
	}
	exp
}

// Serve the bus for the life of the system: every HID device keeps one input
// report TRB posted (the device completes it only when its state changes) and
// its reports feed the console and the pointer sink; the disk serves block
// requests arriving on `blk_server` (answered with an error status while no disk
// is attached); the typed `usb` interface serves the live device inventory on
// `usbq`; and a port-status-change event triggers a root-port reconcile, so a
// device plugged in at runtime enumerates and configures on the fly and an
// unplugged one is torn down. The loop sleeps on the controller's MSI-X
// interrupt and both channels at once, and the synchronous BOT waits service HID
// events inline, so typing is never lost behind disk traffic.
unsafe fn service_loop(hc: &mut Xhci, slots: &mut Slots, mut hids: Hids, mut storage: Option<(UsbDevice, Storage)>, blk_server: u64, usbq: u64, irq: u64) -> ! {
	unsafe {
		post_reports(hc, &mut hids);
		let mut req: [u8; 16] = [0u8; 16];
		loop {
			let waitset: [u64; 3] = [irq, blk_server, usbq];
			wait_any(&waitset, 0);
			// the interrupt: drain the event ring (HID reports feed the console and
			// the pointer sink; a port-status change marks the bus for the reconcile
			// below), acknowledge, and clear the interrupter's pending flag so the
			// next event edge fires.
			let mut rescan: bool = false;
			while let Some((_p, status, control)) = take_event(hc) {
				if control >> 10 & 0x3f == TRB_EV_PORT_STATUS {
					rescan = true;
					continue;
				}
				handle_hid_event(hc, &mut hids, status, control);
			}
			interrupt_ack(irq);
			w32(hc.ir0 + IR_IMAN, IMAN_IE | IMAN_IP);
			if rescan {
				reconcile_ports(hc, slots, &mut hids, &mut storage);
			}
			// the block channel: serve every queued request (with an error status while
			// no mass-storage device is attached, so a client never blocks).
			loop {
				match try_recv(blk_server, &mut req) {
					Polled::Message { len, handle } if len >= 16 => match storage.as_mut() {
						Some((dev, st)) => serve_block_request(hc, &mut hids, dev, st, blk_server, &req, handle),
						None => {
							if handle != 0 {
								close(handle);
							}
							reply_block(blk_server, STATUS_ERR, 0);
						}
					},
					Polled::Message { handle, .. } => {
						if handle != 0 {
							close(handle);
						}
						reply_block(blk_server, STATUS_ERR, 0);
					}
					Polled::Empty => break,
					Polled::Closed => exit(),
				}
			}
			// the query channel: answer every queued `usb.list` request with the live
			// inventory of the addressed devices.
			loop {
				let mut qreq: [u8; 64] = [0u8; 64];
				match try_recv(usbq, &mut qreq) {
					Polled::Message { len, handle } => {
						let mut api: UsbApi = UsbApi { slots };
						let mut reply: [u8; 4096] = [0u8; 4096];
						let mut reply_handle: u64 = 0;
						if let Some(n) = usb::dispatch(&mut api, &qreq[..len], handle, &mut reply, &mut reply_handle) {
							send_blocking(usbq, &reply[..n], reply_handle);
						}
					}
					Polled::Empty => break,
					Polled::Closed => exit(),
				}
			}
		}
	}
}

// The driver's live device inventory, served over the generated `usb` contract.
struct UsbApi<'a> {
	slots: &'a Slots,
}

impl<'a> usb::Service for UsbApi<'a> {
	fn list(&mut self) -> Result<Vec<UsbEntry>, UsbError> {
		let mut out: Vec<UsbEntry> = Vec::new();
		for rec in &self.slots.entries {
			out.push(UsbEntry { port: rec.port, speed: String::from(speed_name(rec.speed)), vendor: rec.vendor as u32, product: rec.product as u32, class: rec.class as u32, kind: String::from(kind_name(rec.kind)) });
		}
		Ok(out)
	}
}

// The name of a PORTSC speed code.
fn speed_name(speed: u32) -> &'static str {
	match speed {
		1 => "full",
		2 => "low",
		3 => "high",
		4 => "super",
		_ => "unknown",
	}
}

// The name of a device's bound role.
fn kind_name(kind: u8) -> &'static str {
	match kind {
		KIND_HUB => "hub",
		KIND_KEYBOARD => "keyboard",
		KIND_STORAGE => "storage",
		KIND_POINTER => "pointer",
		_ => "device",
	}
}

// Reconcile the root ports with the addressed-device state after a port-status
// change: a connected port with no addressed device is a fresh attach - enumerate
// and classify it like at start (a new HID device begins serving reports, a new
// disk serves the block channel); a disconnected port with addressed devices is a
// detach - every slot on that port is disabled (a hub takes its downstream devices
// along) and the HID / storage state dropped, so vol://usb unmounts and a
// replug enumerates cleanly.
unsafe fn reconcile_ports(hc: &mut Xhci, slots: &mut Slots, hids: &mut Hids, storage: &mut Option<(UsbDevice, Storage)>) {
	unsafe {
		let mut port: u32 = 1;
		while port <= hc.ports {
			let addr: u64 = hc.op + OP_PORTSC_BASE + (port - 1) as u64 * 0x10;
			let connected: bool = r32(addr) & PORTSC_CCS != 0;
			let known: bool = slots.has_port(port);
			if connected && !known {
				let mut devices: u32 = 0;
				if let Some(dev) = attach_port(hc, port) {
					register_device(hc, dev, slots, &mut devices, hids, storage);
				}
				// a HID device configured by this attach starts serving: post its first
				// report TRB (the boot-time ones are posted before the service loop).
				post_reports(hc, hids);
			} else if !connected && known {
				// acknowledge the disconnect and tear the port's devices down.
				portsc_write(hc, port, PORTSC_CSC | PORTSC_PEC | PORTSC_PRC | PORTSC_WRC | PORTSC_PLC | PORTSC_CEC);
				while let Some(slot) = slots.take_port(port) {
					command(hc, 0, 0, TRB_DISABLE_SLOT << 10 | slot << 24);
					let mut none: Hids = Hids::new();
					let _ = wait_command(hc, &mut none);
					((hc.dcbaa_virt + slot as u64 * 8) as *mut u64).write_volatile(0);
				}
				hids.entries.retain(|(dev, _)| dev.port != port);
				if storage.as_ref().is_some_and(|(dev, _)| dev.port == port) {
					*storage = None;
				}
				print(b"driver.xhci: port detached\n");
			}
			port += 1;
		}
	}
}

// Post a HID device's next input-report TRB (sized by its layout) and ring its
// doorbell.
unsafe fn post_report(hc: &Xhci, dev: &UsbDevice, h: &mut Hid) {
	unsafe {
		h.ring.push(dev.data_phys, h.layout.report_bytes(), TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, h.dci);
		h.posted = true;
	}
}

// Post the first report TRB of every HID device that is not serving yet (at the
// service loop's start, and after a runtime attach).
unsafe fn post_reports(hc: &Xhci, hids: &mut Hids) {
	unsafe {
		for (dev, h) in hids.entries.iter_mut() {
			if !h.posted {
				post_report(hc, dev, h);
			}
		}
	}
}

// Handle one event ring entry against the bound HID devices: a successful
// transfer event for a device's interrupt endpoint is a fresh input report,
// which is decoded through its layout and the next report TRB posted. A stalled
// report is recovered (the endpoint unhalted, its ring repositioned, the
// device-side halt cleared) and reposted. Every other event is ignored.
unsafe fn handle_hid_event(hc: &mut Xhci, hids: &mut Hids, status: u32, control: u32) {
	unsafe {
		let kind: u32 = control >> 10 & 0x3f;
		let code: u32 = status >> 24;
		if kind != TRB_EV_TRANSFER {
			return;
		}
		let Some(i) = hids.entries.iter().position(|(dev, h)| control >> 24 == dev.slot && (control >> 16 & 0x1f) == h.dci) else {
			return;
		};
		let (dev, h) = &mut hids.entries[i];
		if code == CC_STALL {
			// the endpoint is halted (no reports can be in flight on it), so the
			// recovery's own waits run against the other devices only.
			let dequeue: u64 = h.ring.phys + h.ring.index * 16 | h.ring.cycle as u64;
			let (slot, dci): (u32, u32) = (dev.slot, h.dci);
			let addr: u16 = 0x80 | (dci >> 1) as u16;
			let mut rest: Hids = Hids { entries: core::mem::take(&mut hids.entries) };
			let mut moved: (UsbDevice, Hid) = rest.entries.swap_remove(i);
			reset_endpoint(hc, &mut rest, slot, dci, dequeue);
			let _ = control_nodata(hc, &mut rest, &mut moved.0, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, addr);
			post_report(hc, &moved.0, &mut moved.1);
			rest.entries.push(moved);
			hids.entries = rest.entries;
			return;
		}
		if code != CC_SUCCESS && code != CC_SHORT_PACKET {
			return;
		}
		let len: usize = (h.layout.report_bytes() as usize).saturating_sub((status & 0xff_ffff) as usize).min(64);
		let mut report: [u8; 64] = [0u8; 64];
		for (i, slot) in report.iter_mut().enumerate().take(len) {
			*slot = r8(dev.data_virt + i as u64);
		}
		feed_hid_report(h, &report[..len]);
		post_report(hc, dev, h);
	}
}

// Decode one input report through the device's layout and feed the results on:
// keyboard- and Consumer-page changes (diffed against the previous report of the
// same report id) go to the shared keys module, and pointer fields fold into the
// running pointer state, sent to InputService when it changed - the same
// [x u16 LE][y u16 LE][buttons u8][wheel i8] frame the virtio pointer sends.
unsafe fn feed_hid_report(h: &mut Hid, report: &[u8]) {
	unsafe {
		if report.is_empty() {
			return;
		}
		let (id, body): (u8, &[u8]) = if h.layout.uses_ids() { (report[0], &report[1..]) } else { (0, report) };
		let prev_i: usize = match h.prevs.iter().position(|&(pid, _)| pid == id) {
			Some(i) => i,
			None => {
				h.prevs.push((id, [0u8; 64]));
				h.prevs.len() - 1
			}
		};
		let (layout, prevs, mods): (&hid::Layout, &mut Vec<(u8, [u8; 64])>, &mut Mods) = (&h.layout, &mut h.prevs, &mut h.mods);
		layout.keys_diff(id, &prevs[prev_i].1, body, &mut |usage, down| {
			let code: u16 = usage_keycode(usage);
			if code != 0 {
				keys::feed_key(code, down as u32, mods);
			}
		});
		let (mut x, mut y, mut buttons, mut wheel): (i32, i32, u8, i32) = (h.x, h.y, h.buttons, 0);
		if layout.pointer_fold(id, body, &mut x, &mut y, &mut buttons, &mut wheel) && (x != h.x || y != h.y || buttons != h.buttons || wheel != 0) {
			let mut msg: [u8; 6] = [0u8; 6];
			msg[0..2].copy_from_slice(&(x as u16).to_le_bytes());
			msg[2..4].copy_from_slice(&(y as u16).to_le_bytes());
			msg[4] = buttons;
			msg[5] = wheel.clamp(-127, 127) as i8 as u8;
			let sink: u64 = PTR_SINK.load(Ordering::Relaxed);
			if sink != 0 {
				// non-blocking: with no consumer routed, pointer events just drop.
				let _ = try_send(sink, &msg, 0);
			}
			h.x = x;
			h.y = y;
			h.buttons = buttons;
		}
		let body_len: usize = body.len().min(64);
		prevs[prev_i].1[..body_len].copy_from_slice(&body[..body_len]);
	}
}

// Resolve a page-extended HID usage to its keycode: the keyboard page through
// the boot-usage table (its modifier range through the modifier map), the
// Consumer page through the multimedia map. 0 = unmapped.
fn usage_keycode(usage: u32) -> u16 {
	let (page, u): (u16, u32) = ((usage >> 16) as u16, usage & 0xffff);
	match page {
		0x07 if (0xe0..=0xe7).contains(&u) => keys::HID_MODIFIER_KEYCODES[(u - 0xe0) as usize],
		0x07 if u <= 0xff => keys::hid_keycode(u as u8),
		0x0c => keys::consumer_keycode(u as u16),
		_ => 0,
	}
}

// Wait for a transfer event on the given slot/endpoint, servicing HID events
// that arrive in the meantime inline (a keystroke during a disk transfer). Returns
// the completion code, or None on budget exhaustion.
unsafe fn wait_transfer(hc: &mut Xhci, hids: &mut Hids, slot: u32, dci: u32) -> Option<u32> {
	unsafe {
		let mut spins: u32 = 0;
		loop {
			if let Some((_p, status, control)) = take_event(hc) {
				let kind: u32 = control >> 10 & 0x3f;
				if kind == TRB_EV_TRANSFER && control >> 24 == slot && (control >> 16 & 0x1f) == dci {
					return Some(status >> 24);
				}
				handle_hid_event(hc, hids, status, control);
				continue;
			}
			spins += 1;
			if spins > SPIN_BUDGET {
				return None;
			}
			if spins % 4096 == 0 {
				yield_now();
			}
		}
	}
}

// Configure the device's mass-storage function, if it has one: find a SCSI
// Bulk-Only interface and its bulk IN/OUT endpoint pair in the configuration
// descriptor, bring both endpoints up, select the configuration, then spin the
// unit up (TEST UNIT READY, clearing the power-on sense) and check its block size
// is the 512-byte sector the block protocol serves. None when the device is not a
// disk or any step fails.
unsafe fn configure_storage(hc: &mut Xhci, dev: &mut UsbDevice) -> Option<Storage> {
	unsafe {
		// no HID device is serving yet, so the control / transport waits see no HID
		// events (report TRBs are only posted once the service loop starts).
		let mut hids: Hids = Hids::new();
		control_in(hc, &mut hids, dev, DESC_CONFIG, 9)?;
		let total: u16 = (r8(dev.data_virt + 2) as u16 | (r8(dev.data_virt + 3) as u16) << 8).min(1024);
		let config_value: u16 = r8(dev.data_virt + 5) as u16;
		control_in(hc, &mut hids, dev, DESC_CONFIG, total)?;

		// walk the descriptors for the Bulk-Only SCSI interface and its endpoint pair.
		let mut offset: u64 = 0;
		let mut in_storage: bool = false;
		let mut iface: u16 = 0;
		let mut ep_in: Option<(u32, u32, u8)> = None; // (dci, mps, address)
		let mut ep_out: Option<(u32, u32, u8)> = None;
		while offset + 2 <= total as u64 {
			let length: u64 = r8(dev.data_virt + offset) as u64;
			let kind: u8 = r8(dev.data_virt + offset + 1);
			if length < 2 {
				break;
			}
			if kind == DT_INTERFACE {
				in_storage = r8(dev.data_virt + offset + 5) == CLASS_MASS_STORAGE && r8(dev.data_virt + offset + 6) == SUBCLASS_SCSI && r8(dev.data_virt + offset + 7) == PROTOCOL_BULK_ONLY;
				if in_storage {
					iface = r8(dev.data_virt + offset + 2) as u16;
				}
			}
			if kind == DT_ENDPOINT && in_storage {
				let ep_addr: u8 = r8(dev.data_virt + offset + 2);
				let attrs: u8 = r8(dev.data_virt + offset + 3);
				if attrs & 0x3 == EP_ATTR_BULK {
					let mps: u32 = r8(dev.data_virt + offset + 4) as u32 | (r8(dev.data_virt + offset + 5) as u32) << 8;
					let dci: u32 = (ep_addr & 0xf) as u32 * 2 + if ep_addr & 0x80 != 0 { 1 } else { 0 };
					if ep_addr & 0x80 != 0 && ep_in.is_none() {
						ep_in = Some((dci, mps, ep_addr));
					} else if ep_addr & 0x80 == 0 && ep_out.is_none() {
						ep_out = Some((dci, mps, ep_addr));
					}
				}
			}
			offset += length;
		}
		let (dci_in, mps_in, ep_in_addr): (u32, u32, u8) = ep_in?;
		let (dci_out, mps_out, ep_out_addr): (u32, u32, u8) = ep_out?;

		// bring both bulk endpoints up with one configure-endpoint command.
		let ring_in: Ring = Ring::new()?;
		let ring_out: Ring = Ring::new()?;
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		((dev.in_virt + 4) as *mut u32).write_volatile(1 | 1 << dci_in | 1 << dci_out);
		let entries: u32 = dci_in.max(dci_out);
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(entries << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		// bulk endpoint contexts: IN type 6 / OUT type 2, error count 3, no interval.
		for &(dci, mps, ep_type, ring) in &[(dci_in, mps_in, 6u32, &ring_in), (dci_out, mps_out, 2u32, &ring_out)] {
			let ep_ctx: u64 = dev.in_virt + (1 + dci as u64) * hc.ctx_size;
			((ep_ctx + 4) as *mut u32).write_volatile(mps << 16 | ep_type << 3 | 3 << 1);
			((ep_ctx + 8) as *mut u32).write_volatile((ring.phys | ring.cycle as u64) as u32);
			((ep_ctx + 12) as *mut u32).write_volatile((ring.phys >> 32) as u32);
			((ep_ctx + 16) as *mut u32).write_volatile(mps);
		}
		command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24)?;
		control_nodata(hc, &mut hids, dev, 0x00, REQ_SET_CONFIGURATION, config_value, 0)?;

		let (_sh, data_virt, data_phys): (u64, u64, u64) = dma_page()?;
		let mut st: Storage = Storage { dci_in, dci_out, ep_in_addr, ep_out_addr, iface, ring_in, ring_out, data_virt, data_phys, tag: 1, capacity: 0 };

		// spin the unit up: a freshly attached unit reports a power-on unit attention
		// on its first command, cleared by reading the sense data - retry a few times.
		let mut ready: bool = false;
		let mut attempt: u32 = 0;
		while attempt < 4 {
			let turcb: [u8; 6] = [SCSI_TEST_UNIT_READY, 0, 0, 0, 0, 0];
			if bot_command(hc, &mut hids, dev, &mut st, &turcb, 0, false) {
				ready = true;
				break;
			}
			read_sense(hc, &mut hids, dev, &mut st);
			attempt += 1;
		}
		if !ready {
			return None;
		}
		// the block protocol serves 512-byte sectors; refuse a disk with another size.
		let capcb: [u8; 10] = [SCSI_READ_CAPACITY10, 0, 0, 0, 0, 0, 0, 0, 0, 0];
		if !bot_command(hc, &mut hids, dev, &mut st, &capcb, 8, true) {
			return None;
		}
		let block_size: u32 = (r8(st.data_virt + 4) as u32) << 24 | (r8(st.data_virt + 5) as u32) << 16 | (r8(st.data_virt + 6) as u32) << 8 | r8(st.data_virt + 7) as u32;
		if block_size != SECTOR {
			return None;
		}
		// the unit's size: READ CAPACITY reports the last LBA (big-endian), so the
		// sector count is one more.
		let last_lba: u32 = (r8(st.data_virt) as u32) << 24 | (r8(st.data_virt + 1) as u32) << 16 | (r8(st.data_virt + 2) as u32) << 8 | r8(st.data_virt + 3) as u32;
		st.capacity = (last_lba as u64 + 1) * SECTOR as u64;
		Some(st)
	}
}

// Run one SCSI command over the Bulk-Only Transport: the CBW on the bulk OUT
// endpoint, the data stage (into or out of the storage data page), and the CSW on
// the bulk IN endpoint, whose signature, tag echo and status decide the result.
// HID events arriving during the waits are serviced inline.
unsafe fn bot_command(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, st: &mut Storage, cb: &[u8], data_len: u32, data_in: bool) -> bool {
	unsafe {
		// the CBW rides at the head of the device's scratch page, the CSW after it.
		let cbw: u64 = dev.data_virt;
		core::ptr::write_bytes(cbw as *mut u8, 0, (CSW_OFF + CSW_LEN as u64) as usize);
		(cbw as *mut u32).write_volatile(CBW_SIGNATURE);
		((cbw + 4) as *mut u32).write_volatile(st.tag);
		((cbw + 8) as *mut u32).write_volatile(data_len);
		((cbw + 12) as *mut u8).write_volatile(if data_in { CBW_FLAG_IN } else { 0 });
		((cbw + 13) as *mut u8).write_volatile(0); // LUN 0
		((cbw + 14) as *mut u8).write_volatile(cb.len() as u8);
		for (i, &b) in cb.iter().enumerate() {
			((cbw + 15 + i as u64) as *mut u8).write_volatile(b);
		}
		st.ring_out.push(dev.data_phys, CBW_LEN, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, st.dci_out);
		let mut ok: bool = true;
		match wait_transfer(hc, hids, dev.slot, st.dci_out) {
			Some(CC_SUCCESS) => {}
			Some(CC_STALL) => {
				// the device rejected the CBW: unhalt the OUT endpoint and fail the command.
				recover_bulk(hc, hids, dev, st, false);
				ok = false;
			}
			_ => {
				bot_reset(hc, hids, dev, st);
				ok = false;
			}
		}
		// the data stage, on the direction's endpoint, out of the storage data page. A
		// stall here is routine (the device returned less than asked): unhalt the
		// endpoint and continue to the CSW, which still reports the command's status.
		if ok && data_len > 0 {
			let (ring, dci): (&mut Ring, u32) = if data_in { (&mut st.ring_in, st.dci_in) } else { (&mut st.ring_out, st.dci_out) };
			ring.push(st.data_phys, data_len, TRB_NORMAL << 10 | TRB_IOC);
			w32(hc.db + dev.slot as u64 * 4, dci);
			match wait_transfer(hc, hids, dev.slot, dci) {
				Some(CC_SUCCESS) | Some(CC_SHORT_PACKET) => {}
				Some(CC_STALL) => recover_bulk(hc, hids, dev, st, data_in),
				_ => {
					bot_reset(hc, hids, dev, st);
					ok = false;
				}
			}
		}
		// the CSW closes the transaction; a stalled status stage is unhalted and the
		// read retried once (the Bulk-Only recovery sequence), anything worse resets
		// the transport.
		if ok {
			ok = false;
			let mut attempt: u32 = 0;
			while attempt < 2 {
				st.ring_in.push(dev.data_phys + CSW_OFF, CSW_LEN, TRB_NORMAL << 10 | TRB_IOC);
				w32(hc.db + dev.slot as u64 * 4, st.dci_in);
				match wait_transfer(hc, hids, dev.slot, st.dci_in) {
					Some(CC_SUCCESS) | Some(CC_SHORT_PACKET) => {
						ok = true;
						break;
					}
					Some(CC_STALL) => {
						recover_bulk(hc, hids, dev, st, true);
						attempt += 1;
					}
					_ => break,
				}
			}
			if ok {
				// the wrapper must echo our signature and tag and report status 0 (pass);
				// a malformed wrapper means the transport lost sync, so reset it.
				let csw: u64 = dev.data_virt + CSW_OFF;
				let framed: bool = (csw as *const u32).read_volatile() == CSW_SIGNATURE && ((csw + 4) as *const u32).read_volatile() == st.tag;
				if !framed {
					bot_reset(hc, hids, dev, st);
					ok = false;
				} else {
					ok = ((csw + 12) as *const u8).read_volatile() == 0;
				}
			} else {
				bot_reset(hc, hids, dev, st);
			}
		}
		st.tag = st.tag.wrapping_add(1);
		ok
	}
}

// Serve one block request from the StorageService instance: [op u32][lba u64]
// [count u32], count clamped to one DMA page. A read replies [status u32] + a
// MemoryObject of the sectors; a write carries a MemoryObject in and replies
// [status u32] - the same wire contract driver.virtio-blk serves.
unsafe fn serve_block_request(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, st: &mut Storage, blk_server: u64, req: &[u8; 16], handle: u64) {
	unsafe {
		let op: u32 = u32::from_le_bytes([req[0], req[1], req[2], req[3]]);
		let lba: u64 = u64::from_le_bytes([req[4], req[5], req[6], req[7], req[8], req[9], req[10], req[11]]);
		let count: u32 = u32::from_le_bytes([req[12], req[13], req[14], req[15]]).clamp(1, MAX_SECTORS);
		match op {
			OP_READ => serve_read(hc, hids, dev, st, blk_server, lba, count),
			OP_WRITE => serve_write(hc, hids, dev, st, blk_server, lba, count, handle),
			OP_CAPACITY => reply_capacity(blk_server, st.capacity),
			OP_FLUSH => serve_flush(hc, hids, dev, st, blk_server),
			_ => {
				if handle != 0 {
					close(handle);
				}
				reply_block(blk_server, STATUS_ERR, 0);
			}
		}
	}
}

// Read `count` sectors starting at `lba` with one SCSI READ(10) into a fresh
// shared buffer and hand it to the client, or reply with an error status. A failed
// command is retried once with the sense data read (and discarded) in between - a
// transient unit attention fails the first command and succeeds the retry.
unsafe fn serve_read(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, st: &mut Storage, blk_server: u64, lba: u64, count: u32) {
	unsafe {
		let bytes: u64 = count as u64 * SECTOR as u64;
		let cb: [u8; 10] = read10_cb(SCSI_READ10, lba, count);
		let mut ok: bool = bot_command(hc, hids, dev, st, &cb, bytes as u32, true);
		if !ok {
			read_sense(hc, hids, dev, st);
			ok = bot_command(hc, hids, dev, st, &cb, bytes as u32, true);
		}
		if !ok {
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
		let obj: u64 = syscall(SYS_MEMORY_OBJECT_CREATE, bytes, 0, 0, 0);
		if sys_is_err(obj) {
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
		let dst: u64 = match map_object(obj) {
			Some(base) => base,
			None => {
				close(obj);
				reply_block(blk_server, STATUS_ERR, 0);
				return;
			}
		};
		core::ptr::copy_nonoverlapping(st.data_virt as *const u8, dst as *mut u8, bytes as usize);
		unmap_object(obj);
		// attenuate to read+map plus the transfer right, then hand the buffer over.
		let granted: i64 = duplicate(obj, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(obj);
		if granted < 0 {
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
		reply_block(blk_server, STATUS_OK, granted as u64);
	}
}

// Write `count` sectors starting at `lba` from the transferred buffer with one
// SCSI WRITE(10), then reply with the status and no buffer. A failed command is
// retried once with the sense data read in between; the sense read reuses the data
// page, so the sectors are copied in again before the retry.
unsafe fn serve_write(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, st: &mut Storage, blk_server: u64, lba: u64, count: u32, src_handle: u64) {
	unsafe {
		if src_handle == 0 {
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
		let src: u64 = match map_object(src_handle) {
			Some(base) => base,
			None => {
				close(src_handle);
				reply_block(blk_server, STATUS_ERR, 0);
				return;
			}
		};
		let bytes: u64 = count as u64 * SECTOR as u64;
		let cb: [u8; 10] = read10_cb(SCSI_WRITE10, lba, count);
		core::ptr::copy_nonoverlapping(src as *const u8, st.data_virt as *mut u8, bytes as usize);
		let mut ok: bool = bot_command(hc, hids, dev, st, &cb, bytes as u32, false);
		if !ok {
			read_sense(hc, hids, dev, st);
			core::ptr::copy_nonoverlapping(src as *const u8, st.data_virt as *mut u8, bytes as usize);
			ok = bot_command(hc, hids, dev, st, &cb, bytes as u32, false);
		}
		unmap_object(src_handle);
		close(src_handle);
		reply_block(blk_server, if ok { STATUS_OK } else { STATUS_ERR }, 0);
	}
}

// Read (and discard) the unit's sense data, clearing the pending condition a failed
// command left behind so the next command starts clean.
unsafe fn read_sense(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, st: &mut Storage) {
	unsafe {
		let sense: [u8; 6] = [SCSI_REQUEST_SENSE, 0, 0, 0, 18, 0];
		let _ = bot_command(hc, hids, dev, st, &sense, 18, true);
	}
}

// Flush the unit's write cache with one SCSI SYNCHRONIZE CACHE (10) - LBA and count
// zero mean the whole medium - then reply with the status. The write barrier LiberFS
// commits rely on; a unit that rejects the command (no cache to flush, per the SBC
// spec an optional command) is treated as write-through after the sense read, so the
// barrier still reports success. No data stage.
unsafe fn serve_flush(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, st: &mut Storage, blk_server: u64) {
	unsafe {
		let cb: [u8; 10] = [SCSI_SYNCHRONIZE_CACHE10, 0, 0, 0, 0, 0, 0, 0, 0, 0];
		let mut ok: bool = bot_command(hc, hids, dev, st, &cb, 0, false);
		if !ok {
			read_sense(hc, hids, dev, st);
			ok = bot_command(hc, hids, dev, st, &cb, 0, false);
			// still failing: the unit does not implement the (optional) command, which
			// per SBC means it has no volatile cache to flush - the barrier holds.
			if !ok {
				read_sense(hc, hids, dev, st);
				ok = true;
			}
		}
		reply_block(blk_server, if ok { STATUS_OK } else { STATUS_ERR }, 0);
	}
}

// Build a READ(10)/WRITE(10) command block: big-endian LBA and block count.
fn read10_cb(opcode: u8, lba: u64, count: u32) -> [u8; 10] {
	[opcode, 0, (lba >> 24) as u8, (lba >> 16) as u8, (lba >> 8) as u8, lba as u8, 0, (count >> 8) as u8, count as u8, 0]
}

// Send a block reply: [status u32 LE] carrying the handle `xfer` (0 = none).
unsafe fn reply_block(blk_server: u64, status: u32, xfer: u64) {
	unsafe {
		let reply: [u8; 4] = status.to_le_bytes();
		send_blocking(blk_server, &reply, xfer);
	}
}

// Send a capacity reply: [status u32 LE][capacity bytes u64 LE], no handle - the
// same wire contract driver.virtio-blk serves.
unsafe fn reply_capacity(blk_server: u64, bytes: u64) {
	unsafe {
		let mut reply: [u8; 12] = [0u8; 12];
		reply[..4].copy_from_slice(&STATUS_OK.to_le_bytes());
		reply[4..].copy_from_slice(&bytes.to_le_bytes());
		send_blocking(blk_server, &reply, 0);
	}
}

// Print one addressed device: its port, vendor:product identity and device class.
unsafe fn report_device(dev: &UsbDevice) {
	unsafe {
		let mut line: [u8; 64] = [0u8; 64];
		let mut n: usize = 0;
		for &b in b"driver.xhci: port " {
			line[n] = b;
			n += 1;
		}
		n += push_decimal(&mut line[n..], dev.port as u64);
		for &b in b" device " {
			line[n] = b;
			n += 1;
		}
		n += push_hex16(&mut line[n..], dev.vendor);
		line[n] = b':';
		n += 1;
		n += push_hex16(&mut line[n..], dev.product);
		for &b in b" class " {
			line[n] = b;
			n += 1;
		}
		n += push_decimal(&mut line[n..], dev.class as u64);
		line[n] = b'\n';
		n += 1;
		print(&line[..n]);
	}
}

// Render a small decimal number into `out`, returning the digit count.
fn push_decimal(out: &mut [u8], value: u64) -> usize {
	let mut digits: [u8; 20] = [0u8; 20];
	let mut v: u64 = value;
	let mut n: usize = 0;
	loop {
		digits[n] = b'0' + (v % 10) as u8;
		v /= 10;
		n += 1;
		if v == 0 {
			break;
		}
	}
	for i in 0..n {
		out[i] = digits[n - 1 - i];
	}
	n
}

// Render a 16-bit value as four lowercase hex digits into `out`, returning 4.
fn push_hex16(out: &mut [u8], value: u16) -> usize {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	for i in 0..4 {
		out[i] = HEX[(value >> (12 - i * 4) & 0xf) as usize];
	}
	4
}
