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
// life of the system: keyboard changes feed both canonical HID transitions to
// InputService and cooked text to the console through the shared keys module,
// Consumer-page changes feed the cooked path, and pointing reports feed normalized
// pointer events, exactly like the virtio-input pointer. Bring-up itself is
// synchronous and polled - commands and transfers one at a time, completions
// reaped off the event ring - matching the polled virtio-blk/gpu drivers.

#![no_std]
#![no_main]

extern crate alloc;

mod usb_audio;
mod usb_hid;
mod usb_net;
mod usb_storage;
mod usb_uas;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use proto::system::usb;
use proto::system::{Error as UsbError, UsbDevice as UsbEntry};
use rt::*;

use crate::usb_audio::{Audio, configure_audio};
use crate::usb_hid::{Hids, KEY_SINK, PTR_SINK, configure_hid, handle_hid_event, post_reports};
use crate::usb_net::{NOTIFY_WINDOW, Net, configure_network, handle_net_event, handle_notification, post_notification, post_receive, transmit};
use crate::usb_storage::{STATUS_ERR, Storage, configure_storage, reply_block, serve_block_request};
use crate::usb_uas::{Uas, configure_uas};
use driver_protocol::audio;
use drivers::usb_class::ClassKind;
use drivers::{common, keys};

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
fn w64(addr: u64, v: u64) {
	unsafe {
		w32(addr, v as u32);
		w32(addr + 4, (v >> 32) as u32);
	}
}

// This controller's DeviceMemory capability, so every DMA page can name the device it is for.
//
// A STATIC because this process drives exactly one controller - DeviceManager launches one driver
// per device - and threading it through the eight allocation sites below would say nothing the
// program does not already guarantee. It is set once, before the first allocation.
static DEVICE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// This controller's capability, for the storage half of this program (`usb_storage`), whose data
// span is DMA the controller writes into exactly as the rings are.
pub fn device() -> u64 {
	DEVICE.load(core::sync::atomic::Ordering::Relaxed)
}

// Allocate one zeroed DMA page. Zeroing matters: a freed page from an earlier
// driver instance is recycled with its old ring contents intact, and a stale TRB
// with the right cycle bit would read as a fresh event.
//
// The zeroing is also why the kernel holding these frames matters. "A freed page from an earlier
// driver instance" is exactly the case: this driver can zero what it is given, and it cannot stop
// the previous instance's controller from writing into it afterwards - only a reset can, which is
// what `device_quiesced` below reports.
unsafe fn dma_page() -> Option<(u64, u64, u64)> {
	unsafe {
		let (handle, virt, phys): (u64, u64, u64) = dma_buffer_for(DEVICE.load(core::sync::atomic::Ordering::Relaxed), 4096)?;
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
	// THE PAGE'S HANDLE, kept so the ring can be released. It used to be dropped at the end of the
	// constructor, which pinned the page for ever - correct while the device is there and a leak of
	// one page per attach once it is not.
	handle: u64,
}

impl Ring {
	// Allocate the ring page and plant the wrapping link TRB.
	unsafe fn new() -> Option<Ring> {
		unsafe {
			let (handle, virt, phys): (u64, u64, u64) = dma_page()?;
			let link: u64 = virt + (RING_TRBS - 1) * 16;
			(link as *mut u64).write_volatile(phys);
			((link + 12) as *mut u32).write_volatile(TRB_LINK << 10 | TRB_TOGGLE_CYCLE);
			Some(Ring { virt, phys, index: 0, cycle: 1, handle })
		}
	}

	// Give the ring's page back. A ring is released when its device goes away, and a driver that
	// released nothing leaked a page per attach - which on a port somebody plugs and unplugs is a
	// leak with a person operating it.
	fn release(&mut self) {
		if self.handle != 0 {
			close(self.handle);
			self.handle = 0;
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

// ONE BULK ENDPOINT'S STREAM CONTEXT ARRAY, and what it is for.
//
// A bulk endpoint can carry SEVERAL INDEPENDENT TRANSFER RINGS, selected by a stream id the driver
// writes into the doorbell beside the endpoint. The endpoint context then points at an ARRAY of
// stream contexts rather than at a ring, and each entry names one ring. That is the whole mechanism,
// and it exists here for one reason: a UAS device's status and data pipes DEMAND it - they advertise
// streams in their endpoint companions, and a host that ignores them has no way to tell which
// command an answer belongs to.
//
// ONE STREAM IS USED AND THE ARRAY IS FULL SIZE, which is not a shortcut but what the hardware
// requires: the array's length is fixed by `MaxPStreams` in the endpoint context, and the device
// chooses which entries it uses. This driver issues one command at a time under one tag, so it uses
// stream 1 and leaves the rest of the array zeroed - an entry a device never selects is never read.
struct Streams {
	// The array's page, kept so it can be given back.
	handle: u64,
	virt: u64,
	phys: u64,
	// The one stream this driver drives.
	ring: Ring,
}

// The stream context's type field: a primary transfer ring.
const STREAM_CONTEXT_TYPE_PRIMARY: u64 = 1 << 1;
// The stream this driver uses. Zero is reserved by the specification - it means "no stream" - for
// the same reason the UAS tag reserves zero and NVMe reserves command id zero.
const STREAM_ID: u32 = 1;

impl Streams {
	// Build the array and the one ring in it. `count` is the number of entries, which must be a
	// power of two and is what `MaxPStreams` encodes.
	unsafe fn new(count: u32) -> Option<Streams> {
		unsafe {
			let (handle, virt, phys) = dma_page()?;
			// Sixteen bytes an entry, and the page bounds how many there can be.
			if count as u64 * 16 > 4096 {
				close(handle);
				return None;
			}
			let ring = Ring::new()?;
			let entry = virt + STREAM_ID as u64 * 16;
			(entry as *mut u64).write_volatile(ring.phys | STREAM_CONTEXT_TYPE_PRIMARY | ring.cycle as u64);
			((entry + 8) as *mut u64).write_volatile(0);
			Some(Streams { handle, virt, phys, ring })
		}
	}

	fn release(&mut self) {
		self.ring.release();
		if self.handle != 0 {
			close(self.handle);
			self.handle = 0;
			self.virt = 0;
		}
	}
}

// `MaxPStreams` for an array of `count` entries: the field holds log2(count) - 1.
fn max_primary_streams(count: u32) -> u32 {
	(31 - count.leading_zeros()).saturating_sub(1)
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
	// THE PORT CHANGE NOTHING MAY LOSE. Every event passes through `take_event`, so a synchronous
	// wait that is not interested in a port-status event still cannot drop it - which is what left a
	// device plugged in during a block read invisible until something unrelated woke the loop.
	ports_changed: drivers::port::PortSignal,
	// WHAT THE CLASS MODULES INSIDE THIS PROCESS ARE HOLDING. They live in this Domain and share its
	// endpoints, its DMA pages and its in-flight transfers, so one of them can starve the other and
	// both can starve the controller. The controller drives attach and detach, so the controller is
	// where their cost is counted - a module keeping its own count is a module that can forget to
	// give something back.
	budget: drivers::usb_class::Budget,
	// THE NETWORK COMPLETION NOTHING MAY LOSE, which is the same shape as the port change above and
	// was found the same way. A synchronous wait - a block transfer, a control request, a HID
	// recovery - drains the event ring looking for ONE completion and offers everything else to the
	// HID path, which does not answer for a network endpoint. So a frame that arrived during a disk
	// read was dropped AND its standing receive transfer was never re-posted, which stops the link
	// for good rather than losing one packet. The adapter's endpoint is recorded here when it binds,
	// and a completion for it is stashed rather than discarded.
	//
	// ONE SLOT IS ENOUGH BY CONSTRUCTION: the network module's budget is one transfer in flight, and
	// nothing re-posts until the stashed one is drained.
	net_rx: Option<(u32, u32)>,
	net_pending: Option<(u32, u32)>,
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
	// THE HANDLES THIS DEVICE OWNS: its device context, its input context and its control data page.
	// They used to be dropped at the end of `address_device`, which is correct for as long as the
	// device is attached and a leak of three pages the moment it is not - and on a partial failure
	// during enumeration, a leak of three pages and a slot that stays enabled for ever.
	ctx_handle: u64,
	in_handle: u64,
	data_handle: u64,
	// The device descriptor's identity fields.
	vendor: u16,
	product: u16,
	class: u8,
}

impl UsbDevice {
	// Give everything this device owns back: its pages, its default endpoint's ring, its place in the
	// context array and its slot.
	//
	// ONE PLACE THAT UNDOES WHAT ENUMERATION DID, called from the partial-failure path and from the
	// detach path - because two places that undo half of it each is how a slot stays enabled on one
	// path and a page stays mapped on the other.
	fn release(&mut self, hc: &mut Xhci) {
		unsafe {
			if self.slot != 0 {
				command(hc, 0, 0, TRB_DISABLE_SLOT << 10 | self.slot << 24);
				let mut none: Hids = Hids::new();
				let _ = wait_command(hc, &mut none);
				((hc.dcbaa_virt + self.slot as u64 * 8) as *mut u64).write_volatile(0);
				self.slot = 0;
			}
		}
		self.ep0.release();
		for handle in [&mut self.ctx_handle, &mut self.in_handle, &mut self.data_handle] {
			if *handle != 0 {
				close(*handle);
				*handle = 0;
			}
		}
	}
}

// One addressed device's inventory record: its root port and slot (the state a
// detach tears down), plus the identity its device descriptor reported and the
// role the driver bound it to - the `usb.list` inventory.
struct SlotRec {
	port: u32,
	slot: u32,
	speed: u32,
	vendor: u16,
	product: u16,
	class: u8,
	kind: u8,
	// THE DEVICE ITSELF, for the ones nothing else holds: a hub, and anything the driver left
	// addressed without binding. A device whose resources nobody owns is a slot that stays enabled
	// and three pages that stay pinned when its port is unplugged - which is the leak the debt names.
	// HID and storage devices live in their own structures and this is `None` for them.
	device: Option<UsbDevice>,
}

// The roles a device may be bound to, reported in the inventory.
const KIND_DEVICE: u8 = 0;
const KIND_HUB: u8 = 1;
const KIND_KEYBOARD: u8 = 2;
const KIND_STORAGE: u8 = 3;
const KIND_POINTER: u8 = 4;
const KIND_NETWORK: u8 = 5;
const KIND_UAS: u8 = 6;
const KIND_AUDIO: u8 = 7;

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

	// Attach the device to its record, for the ones no other structure holds.
	fn keep(&mut self, slot: u32, device: UsbDevice) {
		if let Some(rec) = self.entries.iter_mut().find(|r| r.slot == slot) {
			rec.device = Some(device);
		}
	}

	// Remove and return one slot on this root port (call until None on a detach).
	fn take_port(&mut self, port: u32) -> Option<SlotRec> {
		let i: usize = self.entries.iter().position(|r| r.port == port)?;
		Some(self.entries.swap_remove(i))
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		// ONE HANDSHAKE, and it is the same one every other driver speaks: a `BIND` naming the
		// device and how many resources follow, then each resource saying which kind it is. This was
		// five named byte strings read one at a time, in an order this driver had to know without
		// being told - and this driver is the one that reads the most of them.
		let (bind, resources) = common::handshake(bootstrap);
		let device_handle: u64 = resources.device;
		if device_handle == 0 {
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
		}
		DEVICE.store(device_handle, core::sync::atomic::Ordering::Relaxed);
		// The controller's MSI-X Interrupt: the keyboard service loop blocks on it, bring-up polls.
		let irq: u64 = resources.irq;
		if irq == 0 {
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
		}
		let key_sink: u64 = resources.keys;
		// A driver handed no power or console capability finds those keys inert rather than halting
		// the machine on a right it does not hold, so a zero here is a state and not a failure.
		keys::set_power(resources.syspower);
		keys::set_console_input(resources.console);
		KEY_SINK.store(key_sink, Ordering::Relaxed);
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
		// THE ONE NETWORK ADAPTER THIS CONTROLLER SERVES, held the way the one disk is and for the
		// same reason: it is a budget of one that REFUSES a second and says so, rather than a
		// structure that silently cannot hold it.
		let mut network: Option<(UsbDevice, Net)> = None;
		// AND THE ONE UAS TARGET, which is a SECOND block provider and not a second disk behind the
		// first: a Bulk-Only stick and a UAS disk are two devices with two transports, and a
		// controller carrying both publishes two.
		let mut uas: Option<(UsbDevice, Uas)> = None;
		// AND ONE AUDIO SINK, which is the only isochronous thing this controller drives.
		let mut audio: Option<(UsbDevice, usb_audio::Audio)> = None;
		let mut slots: Slots = Slots::new();
		let mut port: u32 = 1;
		while port <= hc.ports {
			if let Some(dev) = attach_port(&mut hc, port) {
				register_device(&mut hc, dev, &mut slots, &mut devices, &mut hids, &mut storage, &mut network, &mut uas, &mut audio);
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
		// the frame channel a CDC Ethernet adapter is served over: the SAME wire `virtio-net`
		// publishes, so NetworkService consumes a USB NIC with no knowledge that it is one. Always
		// created, like the block one, so an adapter plugged in later serves over it.
		let (net_server, net_client): (u64, u64) = channel().unwrap_or_else(|| exit());
		// the UAS target's block channel: a SECOND provider of the same kind, published under its own
		// token so a consumer that asks for one is not handed the other's.
		let (uas_server, uas_client): (u64, u64) = channel().unwrap_or_else(|| exit());
		// AND THE AUDIO SINK'S, which is offered only when one is bound - see the publication list.
		let (audio_server, audio_client): (u64, u64) = channel().unwrap_or_else(|| exit());
		// report in, then serve the bus for the life of the system: HID reports,
		// block requests, and runtime attach / detach.
		// Assembled through a writer that cannot overrun rather than by indexing a fixed
		// buffer by hand. The buffer was 64 bytes and the longest report came to exactly 64
		// with a one-digit count, so a machine presenting ten devices wrote one byte past the
		// end and panicked on the index - it fit on the byte, which is not a margin, and the
		// widening that followed only moved where the same edge is. What matters is that the
		// bound is checked in one place instead of trusted at every append: an overlong report
		// is a truncated line, never a dead driver, and the driver's job is the bus.
		let mut report: common::Bounded<96> = common::Bounded::new();
		// ADDRESSED, like every other driver's report. Two controllers produced two identical online
		// lines, and xHCI is an ordinary registry driver - a machine may have more than one.
		report.push(b"driver.xhci: online (");
		report.push(&common::hex2(bind.info.bus));
		report.push(b":");
		report.push(&common::hex2(bind.info.dev));
		report.push(b".");
		report.push(&[b'0' + (bind.info.func % 10)]);
		report.push(b", ");
		report.decimal(devices as u64);
		report.push(b" device(s))");
		if hids.any_keyboard() {
			report.push(b" (keyboard)");
		}
		if hids.any_pointer() {
			report.push(b" (pointer)");
		}
		if storage.is_some() {
			report.push(b" (storage)");
		}
		if network.is_some() {
			report.push(b" (network)");
		}
		if uas.is_some() {
			report.push(b" (uas)");
		}
		if audio.is_some() {
			report.push(b" (audio)");
		}
		// THREE PROVIDERS, ONE HANDSHAKE. They used to be three messages the manager told apart by
		// their text - a report, then the literal `USBBUS`, then `POINTER` - which meant the
		// manager was parsing strings to decide what a capability was for.
		// A PROVIDER IS PUBLISHED WHEN ITS DEVICE IS THERE, AND THE TWO NEW ONES ARE NOT LIKE THE
		// STICK'S. The mass-storage channel is published unconditionally and has been since it was
		// written, because the block contract ANSWERS: a request with no disk behind it gets an
		// error status, so an empty publication costs a consumer one refused request.
		//
		// A NETWORK LINK DOES NOT ANSWER. Its contract begins with the DRIVER speaking - the MAC and
		// the link MTU, which NetworkService cannot build a stack without - so a `net` provider with
		// no adapter behind it is a service that waits for a message nobody will send. That is not a
		// theory: publishing it unconditionally wedged the aarch64 boot at seventeen of twenty-four
		// services, with NetworkService first in the missing list, on a machine whose xHCI carries a
		// keyboard and a disk and no adapter at all.
		//
		// The UAS target's block channel is conditional for a smaller reason and the same shape: an
		// always-present second block provider is a second thing StorageService probes and assigns a
		// boot role by, on every machine that has no UAS device.
		// A ZERO HANDLE IS AN ENTRY THAT KEEPS ITS TOKEN AND IS NOT SENT - `online_named` skips it -
		// which is what lets the absent ones be published LATER under the same token when a device
		// arrives. The token is the position in this list, and a position that moved would rename a
		// publication the manager is already holding.
		common::online(
			bootstrap,
			&bind,
			report.as_bytes(),
			&[
				(driver_protocol::provider::BLOCK, blk_client),
				(driver_protocol::provider::USB_BUS, usbq_client),
				(driver_protocol::provider::POINTER, ptr_client),
				(driver_protocol::provider::NET, if network.is_some() { net_client } else { 0 }),
				(driver_protocol::provider::BLOCK, if uas.is_some() { uas_client } else { 0 }),
				// THE AUDIO SINK, WHICH IS PUBLISHED ONLY WHEN ONE IS BOUND. A provider offered by
				// a controller with no audio device is a provider AudioService opens and then
				// cannot drive - and this wire's refusal is an empty reply, not a status, so there
				// is no way for the service to tell "no device" from "the device said no".
				(driver_protocol::provider::AUDIO, if audio.is_some() { audio_client } else { 0 }),
			],
		);
		let pending = Unpublished { net: if network.is_some() { 0 } else { net_client }, uas: if uas.is_some() { 0 } else { uas_client } };
		service_loop(bootstrap, &bind, &mut hc, &mut slots, hids, storage, network, uas, audio, blk_server, usbq_server, ptr_server, net_server, uas_server, audio_server, pending, irq);
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
		// The controller is stopped and has cleared its own reset: nothing a previous instance of
		// this driver pointed it at is in flight any more, which is what releases the DMA frames the
		// kernel has been holding for it. The xHCI half of what `virtio::negotiate_for` says.
		let device: u64 = DEVICE.load(core::sync::atomic::Ordering::Relaxed);
		if device != 0 {
			device_quiesced(device);
		}
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

		Some(Xhci { op, ir0, db, ctx_size: if csz { 64 } else { 32 }, ports, cmd, evt_virt, evt_phys, evt_index: 0, evt_cycle: 1, ports_changed: drivers::port::PortSignal::new(), dcbaa_virt, budget: drivers::usb_class::Budget::new(), net_rx: None, net_pending: None })
	}
}

// Spin until the masked bits at `addr` are all set. None on budget exhaustion.
impl Xhci {
	// STOP THE CONTROLLER AND WAIT FOR IT TO SAY SO.
	//
	// Clear Run/Stop and wait for USBSTS.HCHalted: the specification's own handshake, and the same
	// pair `reset` performs before it resets. A halted controller has stopped fetching from the
	// command, event and transfer rings, which is what a planned stop has to establish before it can
	// be acknowledged as clean. Returns whether the controller confirmed within the spin budget - a
	// controller that does not is one whose rings may still be live, and its driver must not report a
	// clean stop.
	fn halt(&self) -> bool {
		unsafe {
			w32(self.op + OP_USBCMD, r32(self.op + OP_USBCMD) & !CMD_RUN);
			wait_set(self.op + OP_USBSTS, STS_HCHALTED).is_some()
		}
	}
}

fn wait_set(addr: u64, mask: u32) -> Option<()> {
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
fn wait_clear(addr: u64, mask: u32) -> Option<()> {
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
fn portsc_write(hc: &Xhci, port: u32, set: u32) {
	unsafe {
		let addr: u64 = hc.op + OP_PORTSC_BASE + (port - 1) as u64 * 0x10;
		let value: u32 = r32(addr) & !PORTSC_RW1C;
		w32(addr, value | set);
	}
}

// Push one TRB onto the command ring and ring the command doorbell.
fn command(hc: &mut Xhci, param: u64, status: u32, control: u32) {
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
		// THE CYCLE BIT SAYS THIS TRB IS THIS PASS'S; IT DOES NOT SAY THE REST OF IT HAS ARRIVED.
		//
		// A sixteen-byte write from a controller is not atomic to this reader, and `read_volatile`
		// says nothing about that - it stops the COMPILER reordering these three reads and orders
		// nothing about a device write still in flight while they run. So the cycle bit can be
		// visible while `param` and `status` are still the previous pass's contents, or the zeroes
		// this ring was created with, and the event is then acted on with somebody else's slot id,
		// somebody else's completion code, or a port number that is not the port that changed.
		//
		// FOUND WHILE WRITING THE NVMe DRIVER, where the same shape produced an intermittent
		// bring-up failure roughly one boot in three: its phase bit arrived first and the driver
		// believed a completion whose command id was still zero. `virtio.rs` has had the right shape
		// all along - it reads `used.idx`, checks it, fences, and only then reads the element - and
		// this is that fence, which was the one missing of the three ring consumers in this tree.
		//
		// NOTHING HERE CLAIMS TO HAVE SEEN THIS FAIL ON xHCI. It is the same defect in the same shape
		// in a driver that ships, fixed because it is wrong rather than because it was caught.
		core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
		let param: u64 = (trb as *const u64).read_volatile();
		let status: u32 = ((trb + 8) as *const u32).read_volatile();
		if control >> 10 & 0x3f == TRB_EV_PORT_STATUS {
			hc.ports_changed.record();
		}
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
fn wait_event(hc: &mut Xhci, wanted: u32) -> Option<(u64, u32, u32)> {
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
fn command_and_wait(hc: &mut Xhci, param: u64, status: u32, control: u32) -> Option<u32> {
	command(hc, param, status, control);
	let (_p, ev_status, ev_control): (u64, u32, u32) = wait_event(hc, TRB_EV_CMD_COMPLETE)?;
	if ev_status >> 24 != CC_SUCCESS {
		return None;
	}
	Some(ev_control >> 24)
}

// Bring up the device on root-hub port `port`: reset the port if a device is
// connected, then give it a slot, an address and an identity. Returns None when
// the port is empty or any step fails.
fn attach_port(hc: &mut Xhci, port: u32) -> Option<UsbDevice> {
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
		// EVERY ALLOCATION FROM HERE IS OWNED BY THE DEVICE, so a failure part way through releases
		// what it got rather than leaving a slot enabled and three pages pinned to a device that
		// never came up.
		let mut dev = UsbDevice { slot, port: root_port, route, speed, ep0: Ring { virt: 0, phys: 0, index: 0, cycle: 1, handle: 0 }, in_virt: 0, in_phys: 0, data_virt: 0, data_phys: 0, ctx_handle: 0, in_handle: 0, data_handle: 0, vendor: 0, product: 0, class: 0 };
		let built = (|| {
			let (ctx_handle, _ctx_virt, ctx_phys): (u64, u64, u64) = dma_page()?;
			dev.ctx_handle = ctx_handle;
			((hc.dcbaa_virt + slot as u64 * 8) as *mut u64).write_volatile(ctx_phys);
			let (in_handle, in_virt, in_phys): (u64, u64, u64) = dma_page()?;
			dev.in_handle = in_handle;
			dev.in_virt = in_virt;
			dev.in_phys = in_phys;
			let (data_handle, data_virt, data_phys): (u64, u64, u64) = dma_page()?;
			dev.data_handle = data_handle;
			dev.data_virt = data_virt;
			dev.data_phys = data_phys;
			dev.ep0 = Ring::new()?;
			Some(())
		})();
		if built.is_none() {
			dev.release(hc);
			return None;
		}
		let (in_virt, in_phys, data_virt) = (dev.in_virt, dev.in_phys, dev.data_virt);
		// no HID device can have reports in flight during bring-up (report TRBs are
		// only posted once the service loop starts), so the waits see no HID events.
		let mut pending: Hids = Hids::new();

		// address the device: an input context whose slot context names the port and
		// whose endpoint-0 context points at the transfer ring.
		write_address_contexts(hc, &dev, initial_packet_size(speed));
		// THE SAME RULE FOR THE REST OF ENUMERATION: every step that can fail is inside one closure,
		// and a failure anywhere in it releases the slot and the pages rather than returning `None`
		// past them. A device unplugged MID-ENUMERATION takes this path, which is the case the debt
		// names - and it used to leave its slot enabled and its three pages pinned for ever.
		let identified = (|dev: &mut UsbDevice| {
			command_and_wait(hc, in_phys, 0, TRB_ADDRESS_DEVICE << 10 | slot << 24)?;
			// read the descriptor head first: its bMaxPacketSize0 field tells the real
			// default-endpoint packet size, which full-speed devices are allowed to vary.
			// EIGHT BYTES, AND THE MAXIMUM PACKET SIZE IS THE EIGHTH. A device that returned four has
			// not sent it, and the byte read from the page is the previous transfer's.
			if control_in(hc, &mut pending, dev, DESC_DEVICE, 8)? < 8 {
				return None;
			}
			let mps: u32 = r8(data_virt + 7) as u32;
			if mps != initial_packet_size(speed) && mps >= 8 {
				// fix endpoint 0 up with an evaluate-context command, then re-read.
				write_address_contexts(hc, dev, mps);
				// evaluate-context consumes only the endpoint-0 add flag.
				((in_virt + 4) as *mut u32).write_volatile(1 << 1);
				command_and_wait(hc, in_phys, 0, TRB_EVALUATE_CONTEXT << 10 | slot << 24)?;
			}
			// The identity fields ride at offsets four to eleven, so a short answer is not an
			// identity.
			if control_in(hc, &mut pending, dev, DESC_DEVICE, 18)? < 12 {
				return None;
			}
			dev.class = r8(data_virt + 4);
			dev.vendor = r8(data_virt + 8) as u16 | (r8(data_virt + 9) as u16) << 8;
			dev.product = r8(data_virt + 10) as u16 | (r8(data_virt + 11) as u16) << 8;
			Some(())
		})(&mut dev);
		if identified.is_none() {
			dev.release(hc);
			return None;
		}
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
fn register_device(hc: &mut Xhci, mut dev: UsbDevice, slots: &mut Slots, devices: &mut u32, hids: &mut Hids, storage: &mut Option<(UsbDevice, Storage)>, network: &mut Option<(UsbDevice, Net)>, uas: &mut Option<(UsbDevice, Uas)>, audio: &mut Option<(UsbDevice, Audio)>) {
	unsafe {
		report_device(&dev);
		slots.record(SlotRec { port: dev.port, slot: dev.slot, speed: dev.speed, vendor: dev.vendor, product: dev.product, class: dev.class, kind: KIND_DEVICE, device: None });
		*devices += 1;
		if dev.class == CLASS_HUB {
			slots.set_kind(dev.slot, KIND_HUB);
			expand_hub(hc, &mut dev, slots, devices, hids, storage, network, uas, audio);
			// A HUB STAYS ADDRESSED because its downstream devices reach the bus through it, so the
			// inventory holds it until the port it is on goes away.
			let slot = dev.slot;
			slots.keep(slot, dev);
		} else if admits(hc, ClassKind::Hid, dev.class)
			&& let Some(h) = configure_hid(hc, &mut dev)
		{
			// THE CHARGE IS TAKEN WHEN THE MODULE TAKES THE DEVICE and not when the admission was
			// asked for: `configure_hid` answers `None` for a device that is not a HID at all, and
			// charging that one would spend the keyboard budget on every mouse-shaped thing that is
			// neither.
			let _ = hc.budget.admit(ClassKind::Hid);
			slots.set_kind(dev.slot, if h.layout.has_keyboard() { KIND_KEYBOARD } else { KIND_POINTER });
			hids.entries.push((dev, h));
		} else if storage.is_none()
			&& admits(hc, ClassKind::Storage, dev.class)
			&& let Some(st) = configure_storage(hc, &mut dev)
		{
			let _ = hc.budget.admit(ClassKind::Storage);
			slots.set_kind(dev.slot, KIND_STORAGE);
			*storage = Some((dev, st));
		} else if network.is_none()
			&& admits(hc, ClassKind::Network, dev.class)
			&& let Some(net) = configure_network(hc, &mut dev)
		{
			let _ = hc.budget.admit(ClassKind::Network);
			slots.set_kind(dev.slot, KIND_NETWORK);
			hc.net_rx = Some((dev.slot, net.receive_endpoint()));
			*network = Some((dev, net));
		} else if uas.is_none()
			&& admits(hc, ClassKind::Uas, dev.class)
			&& let Some(target) = configure_uas(hc, &mut dev)
		{
			let _ = hc.budget.admit(ClassKind::Uas);
			slots.set_kind(dev.slot, KIND_UAS);
			*uas = Some((dev, target));
		} else if audio.is_none()
			&& admits(hc, ClassKind::Audio, dev.class)
			&& let Some(sink) = configure_audio(hc, &mut dev)
		{
			let _ = hc.budget.admit(ClassKind::Audio);
			slots.set_kind(dev.slot, KIND_AUDIO);
			// WHAT IT IS, SAID ONCE. A sink this driver bound at a format it refuses to convert is
			// worth naming: the next machine's device may offer a different one and be refused.
			let (channels, bits, packet) = sink.describes();
			let mut line: common::Bounded<96> = common::Bounded::new();
			line.push(b"driver.xhci: audio sink bound - ");
			line.decimal(channels as u64);
			line.push(b" channel(s), ");
			line.decimal(bits as u64);
			line.push(b"-bit, ");
			line.decimal(packet as u64);
			line.push(b" byte packets\n");
			print(line.as_bytes());
			*audio = Some((dev, sink));
		} else {
			// A DEVICE THIS CONTROLLER DOES NOT BIND IS STILL WORTH READING, and for UAS that
			// reading is what the item asked for before anybody wrote its transport: whether the
			// device demands bulk STREAMS. The probe binds nothing, it stays in the tree after the
			// transport exists, and it is how a machine whose shape this driver cannot drive says so
			// in one boot rather than in one implementation.
			usb_uas::probe(hc, &mut dev);
			// NOTHING ELSE HOLDS IT, so the inventory does. A device left addressed without being
			// bound still owns a slot and three pages, and dropping it here is what leaked them.
			let slot = dev.slot;
			slots.keep(slot, dev);
		}
	}
}

// Whether a class module has room for another device, reported when it has not.
//
// THE REFUSAL IS PRINTED HERE AND NOWHERE ELSE, so a controller that will not take a ninth keyboard
// says which ceiling it reached rather than leaving a device addressed and silent. The device is not
// released by a refusal: it stays in the inventory like every other unbound device, which is what
// lets a later detach give its pages back.
fn admits(hc: &Xhci, kind: ClassKind, class: u8) -> bool {
	let mut trial = hc.budget;
	match trial.admit(kind) {
		Ok(()) => true,
		Err(refusal) => {
			// A device that is not of this class would be refused by its own configure step anyway,
			// and saying "the keyboard budget is full" about a printer would be a lie.
			if class_is_plausible(kind, class) {
				print(b"driver.xhci: ");
				print(match kind {
					ClassKind::Hid => b"HID".as_slice(),
					ClassKind::Storage => b"storage".as_slice(),
					ClassKind::Network => b"network".as_slice(),
					ClassKind::Uas => b"UAS".as_slice(),
					ClassKind::Serial => b"serial".as_slice(),
					ClassKind::Audio => b"audio".as_slice(),
				});
				print(b" module is full (");
				print(refusal.describe());
				print(b"), device left unbound\n");
			}
			false
		}
	}
}

// Whether a device's class byte could belong to this module at all. A composite device declares zero
// at the device level and names its classes per interface, so zero is plausible for both.
fn class_is_plausible(kind: ClassKind, class: u8) -> bool {
	match kind {
		ClassKind::Hid => class == 0 || class == 0x03,
		ClassKind::Storage => class == 0 || class == 0x08,
		// A CDC adapter declares the communications class at the DEVICE level - unlike HID and mass
		// storage, which declare it per interface and leave zero here.
		ClassKind::Network => class == 0 || class == drivers::cdc::CLASS_COMMUNICATIONS,
		ClassKind::Uas => class == 0 || class == usb_uas::CLASS_MASS_STORAGE,
		// A CDC-ACM adapter declares the communications class at the device level like the network
		// models do. NOTHING BINDS THIS KIND YET - the decisions are in `drivers::cdc` and gated, and
		// no CDC-ACM device model exists in this harness to bind against - so what this arm does is
		// keep the refusal honest if one ever appears.
		ClassKind::Serial => class == 0 || class == drivers::cdc::CLASS_COMMUNICATIONS,
		// An audio device declares its class PER INTERFACE and leaves zero at the device level, like
		// HID and mass storage - the audio-control and audio-streaming interfaces are what carry it.
		ClassKind::Audio => class == 0 || class == drivers::uac::CLASS_AUDIO,
	}
}

// Configure an addressed hub and enumerate the devices on its ports: select its
// configuration, read the hub class descriptor for the port count, power each port
// up, and bring up whatever is connected. Each addressed downstream device runs
// through `register_device`, so a hub found downstream expands recursively and a
// keyboard or disk behind any tier of hubs is configured like a root one.
fn expand_hub(hc: &mut Xhci, hub: &mut UsbDevice, slots: &mut Slots, devices: &mut u32, hids: &mut Hids, storage: &mut Option<(UsbDevice, Storage)>, network: &mut Option<(UsbDevice, Net)>, uas: &mut Option<(UsbDevice, Uas)>, audio: &mut Option<(UsbDevice, usb_audio::Audio)>) {
	unsafe {
		// no HID device is serving yet, so the control waits see no HID events.
		let mut pending: Hids = Hids::new();
		// select the hub's configuration (the head of its config descriptor names it).
		if control_in(hc, &mut pending, hub, DESC_CONFIG, 9).unwrap_or(0) < 9 {
			return;
		}
		let config_value: u16 = r8(hub.data_virt + 5) as u16;
		if control_nodata(hc, &mut pending, hub, 0x00, REQ_SET_CONFIGURATION, config_value, 0).is_none() {
			return;
		}
		// the hub class descriptor: bNbrPorts rides at offset 2.
		// bNbrPorts rides at offset two, so anything shorter than three bytes is not a hub descriptor.
		if control_in_req(hc, &mut pending, hub, RT_CLASS_DEVICE_IN, REQ_GET_DESCRIPTOR, DESC_HUB << 8, 0, 9).unwrap_or(0) < 3 {
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
				register_device(hc, dev, slots, devices, hids, storage, network, uas, audio);
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
fn attach_hub_port(hc: &mut Xhci, hub: &mut UsbDevice, port: u32, shift: u32) -> Option<UsbDevice> {
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
fn hub_port_status(hc: &mut Xhci, hids: &mut Hids, hub: &mut UsbDevice, port: u32) -> Option<u16> {
	unsafe {
		control_in_req(hc, hids, hub, RT_CLASS_PORT_IN, REQ_GET_STATUS, 0, port as u16, 4)?;
		Some(r8(hub.data_virt) as u16 | (r8(hub.data_virt + 1) as u16) << 8)
	}
}

// Read one hub port's change word (the high half of the GET_STATUS reply).
fn hub_port_change(hc: &mut Xhci, hids: &mut Hids, hub: &mut UsbDevice, port: u32) -> Option<u16> {
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
fn control_in(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, desc: u16, len: u16) -> Option<u32> {
	control_in_req(hc, hids, dev, 0x80, REQ_GET_DESCRIPTOR, desc << 8, 0, len)
}

// Run one IN control request on the default endpoint, the data landing in the
// device's data page: setup stage (the 8-byte request rides in the TRB itself), IN
// data stage, OUT status stage, then the doorbell and the transfer completion
// event. The hub class requests (GET_STATUS on a port, the hub descriptor) ride
// through here too. A stall halts endpoint 0; it is recovered before reporting
// failure, so the endpoint stays usable.
// THE ANSWER IS HOW MANY BYTES ARRIVED, not merely that the transfer did not fail. The data page is
// reused between transfers, so a caller that cannot tell a short answer from a complete one reads the
// PREVIOUS transfer's bytes past the end of this one.
fn control_in_req(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, request_type: u8, request: u8, value: u16, index: u16, len: u16) -> Option<u32> {
	unsafe {
		let setup: u64 = request_type as u64 | (request as u64) << 8 | (value as u64) << 16 | (index as u64) << 32 | (len as u64) << 48;
		dev.ep0.push(setup, 8, TRB_SETUP << 10 | TRB_IDT | TRB_TRT_IN);
		dev.ep0.push(dev.data_phys, len as u32, TRB_DATA << 10 | TRB_DIR_IN);
		dev.ep0.push(0, 0, TRB_STATUS << 10 | TRB_IOC);
		// ring the device slot's doorbell for the default control endpoint (DCI 1).
		w32(hc.db + dev.slot as u64 * 4, 1);
		let (code, residual) = wait_transfer_len(hc, hids, dev.slot, 1)?;
		if code == CC_STALL {
			recover_ep0(hc, hids, dev);
			return None;
		}
		if code != CC_SUCCESS && code != CC_SHORT_PACKET {
			return None;
		}
		// The transfer event's length field is what was NOT transferred, so what arrived is the
		// difference - and a residual larger than the request is a device inventing one.
		Some(len as u32 - residual.min(len as u32))
	}
}

// Issue a data-less control request (SET_CONFIGURATION, the HID SET_PROTOCOL, the
// stall-recovery CLEAR_FEATURE, the BOT reset) on the default endpoint: a setup
// stage with no data stage, then the IN-direction status stage, the doorbell and
// the completion event. A stall halts endpoint 0; it is recovered before reporting
// failure, so a request the device rejects leaves the endpoint usable.
fn control_nodata(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, request_type: u8, request: u8, value: u16, index: u16) -> Option<()> {
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
fn recover_ep0(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice) {
	reset_endpoint(hc, hids, dev.slot, 1, dev.ep0.phys + dev.ep0.index * 16 | dev.ep0.cycle as u64);
}

// The controller half of stall recovery: Reset Endpoint clears the endpoint's
// halted state, Set TR Dequeue Pointer repositions its transfer ring to `dequeue`
// (the producer's current position with the cycle state in bit 0), abandoning the
// stalled TD. HID events arriving during the command waits are serviced.
fn reset_endpoint(hc: &mut Xhci, hids: &mut Hids, slot: u32, dci: u32, dequeue: u64) {
	command(hc, 0, 0, TRB_RESET_ENDPOINT << 10 | dci << 16 | slot << 24);
	let _ = wait_command(hc, hids);
	command(hc, dequeue, 0, TRB_SET_TR_DEQUEUE << 10 | dci << 16 | slot << 24);
	let _ = wait_command(hc, hids);
}

// Wait for a command completion event, servicing HID events that arrive in the
// meantime inline. Returns the completion code, or None on budget exhaustion.
fn wait_command(hc: &mut Xhci, hids: &mut Hids) -> Option<u32> {
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
#[allow(clippy::too_many_arguments)]
// THE PUBLICATIONS THIS DRIVER STILL OWES, because their device was not there when it reported in.
//
// A `net` link and the UAS target's block channel are published only when their device exists - see
// the note at the handshake - so a device plugged in later has a publication waiting to be made. A
// zero means it has been published already, or that there was never anything to publish.
struct Unpublished {
	net: u64,
	uas: u64,
}

#[allow(clippy::too_many_arguments)]
fn service_loop(bootstrap: u64, bind: &common::Bind, hc: &mut Xhci, slots: &mut Slots, mut hids: Hids, mut storage: Option<(UsbDevice, Storage)>, mut network: Option<(UsbDevice, Net)>, mut uas: Option<(UsbDevice, Uas)>, mut audio: Option<(UsbDevice, Audio)>, blk_server: u64, usbq: u64, pointer: u64, net_server: u64, uas_server: u64, audio_server: u64, mut pending: Unpublished, irq: u64) -> ! {
	unsafe {
		post_reports(hc, &mut hids);
		if let Some((dev, net)) = network.as_mut() {
			post_receive(hc, dev, net);
			post_notification(hc, dev, net);
		}
		let mut req: [u8; 16] = [0u8; 16];
		// A RECEIVED FRAME'S BUFFER, HELD ACROSS THE LOOP rather than allocated per frame: this runs
		// once per arriving packet on a busy link.
		let mut frame: Vec<u8> = Vec::new();
		// And the transmit side's, which a consumer's message is read into.
		let mut outgoing: Vec<u8> = alloc::vec![0u8; 2048];
		// These tokens are the positions in this binding's OFFER list. Keep the factory even
		// with no consumers, so a later storage service or lsusb process can connect again.
		let mut serving = common::Serving::from_offers(&[(0, blk_server), (1, usbq), (2, pointer), (3, net_server), (4, uas_server), (5, audio_server)]);
		loop {
			let Some(ready) = common::wait_providers_or_answer(bootstrap, bind, &mut serving, &[irq]) else {
				common::finish_stop(bootstrap, bind, device(), hc.halt());
				exit();
			};
			PTR_SINK.store(serving.first_for(2), Ordering::Relaxed);
			if let common::ProviderReady::Connected(at) = ready {
				// EVERY CONNECTION STARTS WITH THE MAC AND THE MTU, including one made after a
				// consumer restarted: NetworkService builds its stack from that message and refuses
				// to start without it. A connection made while no adapter is bound gets nothing and
				// is served when one arrives.
				if serving.token_at(at) == 3
					&& let Some((_, net)) = network.as_ref()
					&& !send_blocking(serving.at(at), &net.hello(), 0)
				{
					let token = serving.close_at(at);
					common::disconnected(bootstrap, bind, token);
				}
				continue;
			}
			// Drain hardware events whenever this loop wakes. Synchronous block operations still
			// service HID events inline; consumer closure does not stop the controller.
			let mut rescan: bool = false;
			// THE STASHED COMPLETION FIRST, because it is older than anything still on the ring.
			if let Some((status, control)) = hc.net_pending.take()
				&& let Some((dev, net)) = network.as_mut()
				&& handle_net_event(hc, dev, net, status, control, &mut frame)
			{
				for at in (0..serving.as_slice().len()).rev() {
					if serving.token_at(at) != 3 {
						continue;
					}
					if !send_blocking(serving.at(at), &frame, 0) {
						let token = serving.close_at(at);
						common::disconnected(bootstrap, bind, token);
					}
				}
			}
			while let Some((_p, status, control)) = take_event(hc) {
				if control >> 10 & 0x3f == TRB_EV_PORT_STATUS {
					rescan = true;
					continue;
				}
				// THE ADAPTER'S NOTIFICATION ENDPOINT, BEFORE ITS FRAME ONE. An interrupt endpoint
				// the driver configured and never read from is one the device eventually cannot
				// write to, and a link transition is the one thing the frame pipe cannot say: from
				// there, a cable nobody plugged in and a quiet network are the same silence.
				if control >> 10 & 0x3f == TRB_EV_TRANSFER
					&& let Some((dev, net)) = network.as_mut()
					&& net.notify_endpoint() == Some(control >> 16 & 0x1f)
					&& control >> 24 == dev.slot
				{
					let arrived = NOTIFY_WINDOW.saturating_sub((status & 0x00ff_ffff) as usize);
					if let Some(said) = handle_notification(net, arrived) {
						match said {
							drivers::cdc::Notification::Link { up } => {
								print(if up {
									b"driver.xhci: the CDC adapter reports its link is up
"
									.as_slice()
								} else {
									b"driver.xhci: the CDC adapter reports its link is down
"
									.as_slice()
								});
							}
							// SAID AND NOT ACTED ON. What a speed change means to a stack is a
							// policy this driver does not own, and a notification it does not know
							// is not a link change - reporting either as one would be a log that
							// reads like a flapping cable.
							drivers::cdc::Notification::Speed { .. } => print(
								b"driver.xhci: the CDC adapter reports a link speed change
",
							),
							_ => {}
						}
					}
					post_notification(hc, dev, net);
					continue;
				}
				// THE NETWORK EVENT IS OFFERED THE EVENT FIRST AND THE HID PATH SECOND, and each
				// answers only for its own slot and endpoint - so an event for neither falls through
				// both, which is what the HID path already did for a block transfer's completion.
				if let Some((dev, net)) = network.as_mut()
					&& handle_net_event(hc, dev, net, status, control, &mut frame)
				{
					for at in (0..serving.as_slice().len()).rev() {
						if serving.token_at(at) != 3 {
							continue;
						}
						if !send_blocking(serving.at(at), &frame, 0) {
							let token = serving.close_at(at);
							common::disconnected(bootstrap, bind, token);
						}
					}
					continue;
				}
				handle_hid_event(hc, &mut hids, status, control);
			}
			interrupt_ack(irq);
			w32(hc.ir0 + IR_IMAN, IMAN_IE | IMAN_IP);
			// THE FLAG AND THE DRAIN ARE THE SAME QUESTION, and the flag is the half that survives a
			// synchronous wait - a block transfer that ran while a device was plugged in took the
			// event off the ring, and without this the loop never learned of it.
			if rescan || hc.ports_changed.take() {
				reconcile_ports(hc, slots, &mut hids, &mut storage, &mut network, &mut uas, &mut audio);
				// AND A DEVICE THAT ARRIVED NOW GETS ITS PUBLICATION, under the token this driver's
				// offer list reserved for it. `offer` is the post-READY half of the same handshake:
				// the manager holds it live rather than waiting for a terminal frame.
				if pending.net != 0 && network.is_some() {
					if common::offer(bootstrap, bind, driver_protocol::provider::NET, 3, pending.net) {
						pending.net = 0;
					}
				}
				if pending.uas != 0 && uas.is_some() && common::offer(bootstrap, bind, driver_protocol::provider::BLOCK, 4, pending.uas) {
					pending.uas = 0;
				}
			}
			let common::ProviderReady::Consumer(at) = ready else { continue };
			let server = serving.at(at);
			let closed = match serving.token_at(at) {
				0 => match try_recv(server, &mut req) {
					Polled::Message { len, handle } if len >= 16 => {
						match storage.as_mut() {
							Some((dev, st)) => serve_block_request(hc, &mut hids, dev, st, server, &req, handle),
							None => {
								if handle != 0 {
									close(handle);
								}
								reply_block(server, STATUS_ERR, 0);
							}
						}
						false
					}
					Polled::Message { handle, .. } => {
						if handle != 0 {
							close(handle);
						}
						reply_block(server, STATUS_ERR, 0);
						false
					}
					Polled::Empty => false,
					Polled::Closed => true,
				},
				1 => {
					let mut qreq: [u8; 64] = [0u8; 64];
					match try_recv_caps(server, &mut qreq) {
						PolledCaps::Message { len, handles } => {
							let mut api: UsbApi = UsbApi { slots };
							let mut reply: [u8; 4096] = [0u8; 4096];
							let mut reply_handle = proto::codec::Handles::new();
							let mut handle = handles;
							if let Some(n) = usb::dispatch(&mut api, &qreq[..len], &mut handle, &mut reply, &mut reply_handle) {
								if !send_caps_blocking(server, &reply[..n], reply_handle.as_slice()) {
									for &leftover in reply_handle.as_slice() {
										close(leftover);
									}
								}
							}
							for &unclaimed in handle.as_slice() {
								close(unclaimed);
							}
							false
						}
						PolledCaps::Empty => false,
						PolledCaps::Closed => true,
					}
				}
				3 => match try_recv(server, &mut outgoing) {
					Polled::Message { len, handle } => {
						if handle != 0 {
							close(handle);
						}
						if let Some((dev, net)) = network.as_mut() {
							transmit(hc, &mut hids, dev, net, &outgoing[..len]);
						}
						false
					}
					Polled::Empty => false,
					Polled::Closed => true,
				},
				4 => match try_recv(server, &mut req) {
					Polled::Message { len, handle } if len >= 16 => {
						match uas.as_mut() {
							Some((dev, target)) => usb_uas::serve_block_request(hc, &mut hids, dev, target, server, &req, handle),
							None => {
								if handle != 0 {
									close(handle);
								}
								reply_block(server, STATUS_ERR, 0);
							}
						}
						false
					}
					Polled::Message { handle, .. } => {
						if handle != 0 {
							close(handle);
						}
						reply_block(server, STATUS_ERR, 0);
						false
					}
					Polled::Empty => false,
					Polled::Closed => true,
				},
				// THE AUDIO SINK, over `driver_protocol::audio` - the same wire `virtio-snd` and
				// `hda` serve, which is what the item means by "the same PCM transport contract".
				5 => {
					let mut period = [0u8; audio::PERIOD_BYTES as usize];
					match try_recv(server, &mut period) {
						Polled::Message { len, handle } => {
							if handle != 0 {
								close(handle);
							}
							match audio::message(&period[..len]) {
								audio::Message::Play => {
									let played = match audio.as_mut() {
										Some((dev, sink)) => usb_audio::play(hc, &mut hids, dev, sink, &period[..len]),
										None => false,
									};
									send_blocking(server, if played { audio::OK } else { audio::REFUSED }, 0);
								}
								// AN ISOCHRONOUS SINK HAS NOTHING TO STOP. There is no stream to
								// release: the endpoint is scheduled when a transfer is posted and
								// idle when none is, so the end of a stream is the absence of the
								// next period.
								audio::Message::EndPlayback => {
									send_blocking(server, audio::OK, 0);
								}
								// CAPTURE IS REFUSED AND THE REFUSAL IS THE WIRE'S OWN: an empty
								// reply, which cannot be mistaken for samples because a period is
								// never empty. This device model has no input.
								_ => {
									send_blocking(server, audio::REFUSED, 0);
								}
							}
							false
						}
						Polled::Empty => false,
						Polled::Closed => true,
					}
				}
				// Pointer consumers receive events and send no requests. Drain unexpected input
				// with its capabilities, and return the allowance when the peer closes.
				_ => match try_recv_caps(server, &mut req) {
					PolledCaps::Message { handles, .. } => {
						for &handle in handles.as_slice() {
							close(handle);
						}
						false
					}
					PolledCaps::Empty => false,
					PolledCaps::Closed => true,
				},
			};
			if closed {
				let token = serving.close_at(at);
				PTR_SINK.store(serving.first_for(2), Ordering::Relaxed);
				common::disconnected(bootstrap, bind, token);
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
		KIND_NETWORK => "network",
		KIND_UAS => "uas",
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
unsafe fn reconcile_ports(hc: &mut Xhci, slots: &mut Slots, hids: &mut Hids, storage: &mut Option<(UsbDevice, Storage)>, network: &mut Option<(UsbDevice, Net)>, uas: &mut Option<(UsbDevice, Uas)>, audio: &mut Option<(UsbDevice, Audio)>) {
	unsafe {
		let mut port: u32 = 1;
		while port <= hc.ports {
			let addr: u64 = hc.op + OP_PORTSC_BASE + (port - 1) as u64 * 0x10;
			let connected: bool = r32(addr) & PORTSC_CCS != 0;
			let known: bool = slots.has_port(port);
			// THE ACTION IS DECIDED FROM THE PORT'S OWN REGISTER and not from a list of events, so a
			// connect and a disconnect in one window are one attach and one detach rather than two
			// replays of whichever arrived twice.
			if drivers::port::port_action(connected, known) == drivers::port::PortAction::Attach {
				let mut devices: u32 = 0;
				if let Some(dev) = attach_port(hc, port) {
					register_device(hc, dev, slots, &mut devices, hids, storage, network, uas, audio);
				}
				// a HID device configured by this attach starts serving: post its first
				// report TRB (the boot-time ones are posted before the service loop).
				post_reports(hc, hids);
				// AND AN ADAPTER STARTS RECEIVING, for the same reason and with the same shape: its
				// receive transfer is standing, so one has to be posted before anything arrives.
				if let Some((dev, net)) = network.as_mut() {
					post_receive(hc, dev, net);
					post_notification(hc, dev, net);
				}
			} else if drivers::port::port_action(connected, known) == drivers::port::PortAction::Detach {
				// acknowledge the disconnect and tear the port's devices down.
				portsc_write(hc, port, PORTSC_CSC | PORTSC_PEC | PORTSC_PRC | PORTSC_WRC | PORTSC_PLC | PORTSC_CEC);
				// EVERYTHING THIS PORT OWNED IS GIVEN BACK, and each device is released by whoever
				// holds it: the ones bound to a role live in their own structures, and the rest live
				// in the inventory. Disabling the slot without closing the pages - which is what this
				// did - leaks three pages per attach on a port somebody plugs and unplugs.
				for (dev, hid) in hids.entries.iter_mut().filter(|(dev, _)| dev.port == port) {
					hid.release();
					dev.release(hc);
					// AND THE CHARGE GOES BACK WITH THE PAGES. A release that gave the memory back and
					// kept the budget would turn a port somebody plugs and unplugs into a controller
					// that stops accepting keyboards.
					hc.budget.release(ClassKind::Hid);
				}
				hids.entries.retain(|(dev, _)| dev.port != port);
				if let Some((dev, st)) = storage.as_mut()
					&& dev.port == port
				{
					st.release();
					dev.release(hc);
					hc.budget.release(ClassKind::Storage);
					*storage = None;
				}
				if let Some((dev, target)) = uas.as_mut()
					&& dev.port == port
				{
					target.release();
					dev.release(hc);
					hc.budget.release(ClassKind::Uas);
					*uas = None;
				}
				if let Some((dev, net)) = network.as_mut()
					&& dev.port == port
				{
					net.release();
					dev.release(hc);
					hc.budget.release(ClassKind::Network);
					hc.net_rx = None;
					hc.net_pending = None;
					*network = None;
				}
				// AND THE AUDIO SINK, on the same terms: its ring and its slot go back, and the
				// class budget with them, or a device unplugged and plugged in again is refused by
				// a budget still holding the first one's place.
				if let Some((dev, sink)) = audio.as_mut()
					&& dev.port == port
				{
					sink.release();
					dev.release(hc);
					hc.budget.release(ClassKind::Audio);
					*audio = None;
				}
				while let Some(mut rec) = slots.take_port(port) {
					match rec.device.take() {
						Some(mut dev) => dev.release(hc),
						None => {
							// A record whose device another structure held: its pages went with it,
							// and the slot is disabled here.
							command(hc, 0, 0, TRB_DISABLE_SLOT << 10 | rec.slot << 24);
							let mut none: Hids = Hids::new();
							let _ = wait_command(hc, &mut none);
							((hc.dcbaa_virt + rec.slot as u64 * 8) as *mut u64).write_volatile(0);
						}
					}
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
fn wait_transfer(hc: &mut Xhci, hids: &mut Hids, slot: u32, dci: u32) -> Option<u32> {
	wait_transfer_len(hc, hids, slot, dci).map(|(code, _)| code)
}

// The same wait, answering the completion code AND the transfer event's residual length.
fn wait_transfer_len(hc: &mut Xhci, hids: &mut Hids, slot: u32, dci: u32) -> Option<(u32, u32)> {
	unsafe {
		let mut spins: u32 = 0;
		loop {
			if let Some((_p, status, control)) = take_event(hc) {
				let kind: u32 = control >> 10 & 0x3f;
				if kind == TRB_EV_TRANSFER && control >> 24 == slot && (control >> 16 & 0x1f) == dci {
					return Some((status >> 24, status & 0x00ff_ffff));
				}
				// A COMPLETION FOR THE ADAPTER'S RECEIVE ENDPOINT IS KEPT AND NOT OFFERED TO THE HID
				// PATH, which does not answer for it. See `Xhci::net_pending`.
				if kind == TRB_EV_TRANSFER && hc.net_rx == Some((control >> 24, control >> 16 & 0x1f)) && hc.net_pending.is_none() {
					hc.net_pending = Some((status, control));
					continue;
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
fn report_device(dev: &UsbDevice) {
	let mut line: [u8; 64] = [0u8; 64];
	let mut n: usize = 0;
	for &b in b"driver.xhci: port " {
		line[n] = b;
		n += 1;
	}
	n += common::push_decimal(&mut line[n..], dev.port as u64);
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
	n += common::push_decimal(&mut line[n..], dev.class as u64);
	line[n] = b'\n';
	n += 1;
	print(&line[..n]);
}

// Render a small decimal number into `out`, returning the digit count.

// Render a 16-bit value as four lowercase hex digits into `out`, returning 4.
fn push_hex16(out: &mut [u8], value: u16) -> usize {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	for i in 0..4 {
		out[i] = HEX[(value >> (12 - i * 4) & 0xf) as usize];
	}
	4
}
