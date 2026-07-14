// driver.xhci - the userspace xHCI USB host controller driver.
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
mod usb_hid;
mod usb_storage;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use proto::system::usb;
use proto::system::{Error as UsbError, UsbDevice as UsbEntry};
use rt::*;

use crate::usb_hid::{Hids, PTR_SINK, configure_hid, handle_hid_event, post_reports};
use crate::usb_storage::{STATUS_ERR, Storage, configure_storage, reply_block, serve_block_request};

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

// Descriptor types within a configuration, shared by every class walk: the
// interface and endpoint descriptors the HID and mass-storage bindings scan for.
const DT_INTERFACE: u8 = 4;
const DT_ENDPOINT: u8 = 5;

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
					Polled::Message { len, mut handle } => {
						let mut api: UsbApi = UsbApi { slots };
						let mut reply: [u8; 4096] = [0u8; 4096];
						let mut reply_handle: u64 = 0;
						if let Some(n) = usb::dispatch(&mut api, &qreq[..len], &mut handle, &mut reply, &mut reply_handle) {
							if !send_blocking(usbq, &reply[..n], reply_handle) && reply_handle != 0 {
								close(reply_handle);
							}
						}
						if handle != 0 {
							close(handle);
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
			out.push(UsbEntry { port: rec.port, speed: String::from(speed_name(rec.speed)), vendor: rec.vendor as u32, product: rec.product as u32, class: rec.class as u32, r#type: String::from(kind_name(rec.kind)) });
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
