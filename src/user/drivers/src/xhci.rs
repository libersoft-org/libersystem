// driver.xhci - the userspace xHCI USB host controller driver (M62).
//
// DeviceManager launches this program with a "DEVICE" message carrying the
// controller's DeviceInfo and a transferred DeviceMemory capability to its MMIO
// BAR (the whole xHCI register file), followed by an "IRQ" message carrying the
// controller's MSI-X Interrupt capability. The driver maps the BAR, resets the
// controller, builds the device context base array, the command ring and the
// event ring, starts the controller, and enumerates the root-hub ports: each
// connected device is reset, given a device slot, addressed, and has its device
// descriptor read over a control transfer on the default endpoint. A HID boot
// keyboard found among them is configured (its interrupt IN endpoint brought up,
// the boot protocol selected) and served interrupt-driven for the life of the
// system: each report's key changes feed the interactive console through the
// shared keys module, exactly like the virtio-input keyboard. Bring-up itself is
// synchronous and polled - commands and transfers one at a time, completions
// reaped off the event ring - matching the polled virtio-blk/gpu drivers.

#![no_std]
#![no_main]

mod keys;

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
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_ENDPOINT: u32 = 12;
const TRB_EVALUATE_CONTEXT: u32 = 13;
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

// The HID class SET_PROTOCOL request (to the interface): wValue 0 selects the
// fixed boot-report layout, so a keyboard's reports need no report descriptor.
const HID_REQ_SET_PROTOCOL: u8 = 0x0b;

// Descriptor types and the HID boot-keyboard identity within a configuration.
const DT_INTERFACE: u8 = 4;
const DT_ENDPOINT: u8 = 5;
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

// One disk sector, and the block-service wire protocol this driver serves to a
// StorageService instance - the same contract driver.virtio-blk serves: a request
// is [op u32][lba u64][count u32] (count clamped to one DMA page = 8 sectors), a
// read replies [status u32] + a MemoryObject of the sectors, a write carries a
// MemoryObject in and replies [status u32].
const SECTOR: u32 = 512;
const MAX_SECTORS: u32 = 8;
const OP_READ: u32 = 0;
const OP_WRITE: u32 = 1;
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

// One addressed USB device: its slot, root-hub port, speed, the default endpoint's
// transfer ring, and the scratch pages enumeration reuses (the input context and
// the control-transfer data page).
struct UsbDevice {
	slot: u32,
	port: u32,
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

// A configured HID boot keyboard: the interrupt IN endpoint's device context index
// and its transfer ring, on which 8-byte boot reports are posted and reaped, plus
// the report state the service loop diffs against (the previous report and the
// tracked modifiers).
struct Keyboard {
	dci: u32,
	ring: Ring,
	prev: [u8; 8],
	mods: Mods,
}

// A configured USB mass-storage device (Bulk-Only Transport): the bulk IN and OUT
// endpoints' device context indices and transfer rings, a page for the sector data
// (the CBW/CSW frames ride in the device's scratch page), and the rolling CBW tag.
struct Storage {
	dci_in: u32,
	dci_out: u32,
	ring_in: Ring,
	ring_out: Ring,
	data_virt: u64,
	data_phys: u64,
	tag: u32,
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
		// enumerate the root-hub ports, address every connected device, and configure
		// the first HID boot keyboard and the first mass-storage device found.
		let mut devices: u32 = 0;
		let mut keyboard: Option<(UsbDevice, Keyboard)> = None;
		let mut storage: Option<(UsbDevice, Storage)> = None;
		let mut port: u32 = 1;
		while port <= hc.ports {
			if let Some(mut dev) = attach_port(&mut hc, port) {
				report_device(&dev);
				devices += 1;
				if keyboard.is_none()
					&& let Some(kb) = configure_keyboard(&mut hc, &mut dev)
				{
					keyboard = Some((dev, kb));
				} else if storage.is_none()
					&& let Some(st) = configure_storage(&mut hc, &mut dev)
				{
					storage = Some((dev, st));
				}
			}
			port += 1;
		}
		// a mass-storage device is served over a block channel: the client end rides
		// up with the report (DeviceManager routes it to a StorageService instance).
		let (blk_server, blk_client): (u64, u64) = if storage.is_some() { channel().unwrap_or_else(|| exit()) } else { (0, 0) };
		// report in, then serve the keyboard and the disk for the life of the system
		// - or, with neither, stand holding the controller until DeviceManager drops
		// the bootstrap channel.
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
		if keyboard.is_some() {
			for &b in b" (keyboard)" {
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
		if keyboard.is_some() || storage.is_some() {
			service_loop(&mut hc, keyboard, storage, blk_server, irq);
		}
		let _ = recv_blocking(bootstrap, &mut buf);
	}
	exit();
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
// connected, enable a slot, address the device, and read its device descriptor.
// Returns None when the port is empty or any step fails.
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

		// a slot for the device, its device context, and the default endpoint's ring.
		let slot: u32 = command_and_wait(hc, 0, 0, TRB_ENABLE_SLOT << 10)?;
		if slot == 0 || slot > 255 {
			return None;
		}
		let (_dh, _ctx_virt, ctx_phys): (u64, u64, u64) = dma_page()?;
		((hc.dcbaa_virt + slot as u64 * 8) as *mut u64).write_volatile(ctx_phys);

		let (_ih, in_virt, in_phys): (u64, u64, u64) = dma_page()?;
		let (_bh, data_virt, data_phys): (u64, u64, u64) = dma_page()?;
		let mut dev: UsbDevice = UsbDevice { slot, port, speed, ep0: Ring::new()?, in_virt, in_phys, data_virt, data_phys, vendor: 0, product: 0, class: 0 };

		// address the device: an input context whose slot context names the port and
		// whose endpoint-0 context points at the transfer ring.
		write_address_contexts(hc, &dev, initial_packet_size(speed));
		command_and_wait(hc, in_phys, 0, TRB_ADDRESS_DEVICE << 10 | slot << 24)?;

		// read the descriptor head first: its bMaxPacketSize0 field tells the real
		// default-endpoint packet size, which full-speed devices are allowed to vary.
		control_in(hc, &mut dev, DESC_DEVICE, 8)?;
		let mps: u32 = r8(data_virt + 7) as u32;
		if mps != initial_packet_size(speed) && mps >= 8 {
			// fix endpoint 0 up with an evaluate-context command, then re-read.
			write_address_contexts(hc, &dev, mps);
			// evaluate-context consumes only the endpoint-0 add flag.
			((in_virt + 4) as *mut u32).write_volatile(1 << 1);
			command_and_wait(hc, in_phys, 0, TRB_EVALUATE_CONTEXT << 10 | slot << 24)?;
		}
		control_in(hc, &mut dev, DESC_DEVICE, 18)?;
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

// Fill the device's input context for an address-device command: the input
// control context adds the slot and endpoint-0 contexts, the slot context names
// the root-hub port and speed, and the endpoint-0 context is a control endpoint
// with max packet size `mps` whose transfer ring is the device's.
unsafe fn write_address_contexts(hc: &Xhci, dev: &UsbDevice, mps: u32) {
	unsafe {
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		// input control context: add slot (A0) + endpoint 0 (A1).
		((dev.in_virt + 4) as *mut u32).write_volatile(0x3);
		// slot context: one context entry, the device's speed and root-hub port.
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(1 << 27 | dev.speed << 20);
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
// GET_DESCRIPTOR control transfer on the default endpoint: setup stage (the 8-byte
// request rides in the TRB itself), IN data stage, OUT status stage, then the
// doorbell and the transfer completion event.
unsafe fn control_in(hc: &mut Xhci, dev: &mut UsbDevice, desc: u16, len: u16) -> Option<()> {
	unsafe {
		let request: u64 = 0x80 | (REQ_GET_DESCRIPTOR as u64) << 8 | ((desc << 8) as u64) << 16 | (len as u64) << 48;
		dev.ep0.push(request, 8, TRB_SETUP << 10 | TRB_IDT | TRB_TRT_IN);
		dev.ep0.push(dev.data_phys, len as u32, TRB_DATA << 10 | TRB_DIR_IN);
		dev.ep0.push(0, 0, TRB_STATUS << 10 | TRB_IOC);
		// ring the device slot's doorbell for the default control endpoint (DCI 1).
		w32(hc.db + dev.slot as u64 * 4, 1);
		let (_p, status, _c): (u64, u32, u32) = wait_event(hc, TRB_EV_TRANSFER)?;
		let code: u32 = status >> 24;
		if code != CC_SUCCESS && code != CC_SHORT_PACKET { None } else { Some(()) }
	}
}

// Issue a data-less control request (SET_CONFIGURATION, the HID SET_PROTOCOL) on
// the default endpoint: a setup stage with no data stage, then the IN-direction
// status stage, the doorbell and the completion event.
unsafe fn control_nodata(hc: &mut Xhci, dev: &mut UsbDevice, request_type: u8, request: u8, value: u16, index: u16) -> Option<()> {
	unsafe {
		let setup: u64 = request_type as u64 | (request as u64) << 8 | (value as u64) << 16 | (index as u64) << 32;
		dev.ep0.push(setup, 8, TRB_SETUP << 10 | TRB_IDT);
		dev.ep0.push(0, 0, TRB_STATUS << 10 | TRB_DIR_IN | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, 1);
		let (_p, status, _c): (u64, u32, u32) = wait_event(hc, TRB_EV_TRANSFER)?;
		if status >> 24 != CC_SUCCESS { None } else { Some(()) }
	}
}

// Configure the device's HID boot keyboard, if it has one: read the configuration
// descriptor, find a boot-keyboard interface (HID class, boot subclass, keyboard
// protocol) and its interrupt IN endpoint, bring that endpoint up with a
// configure-endpoint command, select the configuration on the device, and put the
// keyboard into the fixed boot-report protocol. None when the device carries no
// boot keyboard or any step fails.
unsafe fn configure_keyboard(hc: &mut Xhci, dev: &mut UsbDevice) -> Option<Keyboard> {
	unsafe {
		// the configuration descriptor head names the total length; read it whole.
		control_in(hc, dev, DESC_CONFIG, 9)?;
		let total: u16 = (r8(dev.data_virt + 2) as u16 | (r8(dev.data_virt + 3) as u16) << 8).min(1024);
		let config_value: u16 = r8(dev.data_virt + 5) as u16;
		control_in(hc, dev, DESC_CONFIG, total)?;

		// walk the descriptors for a boot-keyboard interface, then its interrupt IN
		// endpoint (the descriptors that follow the interface until the next one).
		let mut offset: u64 = 0;
		let mut in_keyboard: bool = false;
		let mut iface: u16 = 0;
		let mut found: Option<(u32, u32, u32)> = None; // (dci, mps, interval)
		while offset + 2 <= total as u64 {
			let length: u64 = r8(dev.data_virt + offset) as u64;
			let kind: u8 = r8(dev.data_virt + offset + 1);
			if length < 2 {
				break;
			}
			if kind == DT_INTERFACE {
				in_keyboard = r8(dev.data_virt + offset + 5) == CLASS_HID && r8(dev.data_virt + offset + 6) == SUBCLASS_BOOT && r8(dev.data_virt + offset + 7) == PROTOCOL_KEYBOARD;
				if in_keyboard {
					iface = r8(dev.data_virt + offset + 2) as u16;
				}
			}
			if kind == DT_ENDPOINT && in_keyboard && found.is_none() {
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
		(slot_ctx as *mut u32).write_volatile(dci << 27 | dev.speed << 20);
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

		// select the configuration, then the boot protocol (the fixed 8-byte report).
		control_nodata(hc, dev, 0x00, REQ_SET_CONFIGURATION, config_value, 0)?;
		control_nodata(hc, dev, 0x21, HID_REQ_SET_PROTOCOL, 0, iface)?;
		Some(Keyboard { dci, ring, prev: [0u8; 8], mods: Mods::default() })
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

// Serve the configured keyboard and mass-storage device for the life of the
// system: the keyboard keeps one 8-byte report TRB posted (the device completes it
// only when the key state changes) and its reports feed the console; the disk
// serves block requests arriving on `blk_server`. The loop sleeps on the
// controller's MSI-X interrupt and the block channel at once, and the synchronous
// BOT waits service keyboard events inline, so typing is never lost behind disk
// traffic.
unsafe fn service_loop(hc: &mut Xhci, mut keyboard: Option<(UsbDevice, Keyboard)>, mut storage: Option<(UsbDevice, Storage)>, blk_server: u64, irq: u64) -> ! {
	unsafe {
		if let Some((dev, kb)) = keyboard.as_mut() {
			post_report(hc, dev, kb);
		}
		let mut req: [u8; 16] = [0u8; 16];
		loop {
			let waitset: [u64; 2] = [irq, blk_server];
			let handles: &[u64] = if blk_server != 0 { &waitset } else { &waitset[..1] };
			wait_any(handles, 0);
			// the interrupt: drain the event ring (keyboard reports feed the console),
			// acknowledge, and clear the interrupter's pending flag so the next event
			// edge fires.
			while let Some((_p, status, control)) = take_event(hc) {
				handle_keyboard_event(hc, &mut keyboard, status, control);
			}
			interrupt_ack(irq);
			w32(hc.ir0 + IR_IMAN, IMAN_IE | IMAN_IP);
			// the block channel: serve every queued request.
			if let Some((dev, st)) = storage.as_mut() {
				loop {
					match try_recv(blk_server, &mut req) {
						Polled::Message { len, handle } if len >= 16 => serve_block_request(hc, &mut keyboard, dev, st, blk_server, &req, handle),
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
			}
		}
	}
}

// Post the keyboard's next 8-byte boot-report TRB and ring its doorbell.
unsafe fn post_report(hc: &Xhci, dev: &UsbDevice, kb: &mut Keyboard) {
	unsafe {
		kb.ring.push(dev.data_phys, 8, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, kb.dci);
	}
}

// Handle one event ring entry against the keyboard: a successful transfer event
// for its interrupt endpoint is a fresh boot report, which is diffed into the
// console and the next report TRB posted. Every other event is ignored.
unsafe fn handle_keyboard_event(hc: &mut Xhci, keyboard: &mut Option<(UsbDevice, Keyboard)>, status: u32, control: u32) {
	unsafe {
		let Some((dev, kb)) = keyboard.as_mut() else {
			return;
		};
		let kind: u32 = control >> 10 & 0x3f;
		let code: u32 = status >> 24;
		if kind != TRB_EV_TRANSFER || control >> 24 != dev.slot || (control >> 16 & 0x1f) != kb.dci || (code != CC_SUCCESS && code != CC_SHORT_PACKET) {
			return;
		}
		let mut report: [u8; 8] = [0u8; 8];
		for (i, slot) in report.iter_mut().enumerate() {
			*slot = r8(dev.data_virt + i as u64);
		}
		let prev: [u8; 8] = kb.prev;
		feed_report(&prev, &report, &mut kb.mods);
		kb.prev = report;
		post_report(hc, dev, kb);
	}
}

// Wait for a transfer event on the given slot/endpoint, servicing keyboard events
// that arrive in the meantime inline (a keystroke during a disk transfer). Returns
// the completion code, or None on budget exhaustion.
unsafe fn wait_transfer(hc: &mut Xhci, keyboard: &mut Option<(UsbDevice, Keyboard)>, slot: u32, dci: u32) -> Option<u32> {
	unsafe {
		let mut spins: u32 = 0;
		loop {
			if let Some((_p, status, control)) = take_event(hc) {
				let kind: u32 = control >> 10 & 0x3f;
				if kind == TRB_EV_TRANSFER && control >> 24 == slot && (control >> 16 & 0x1f) == dci {
					return Some(status >> 24);
				}
				handle_keyboard_event(hc, keyboard, status, control);
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
		control_in(hc, dev, DESC_CONFIG, 9)?;
		let total: u16 = (r8(dev.data_virt + 2) as u16 | (r8(dev.data_virt + 3) as u16) << 8).min(1024);
		let config_value: u16 = r8(dev.data_virt + 5) as u16;
		control_in(hc, dev, DESC_CONFIG, total)?;

		// walk the descriptors for the Bulk-Only SCSI interface and its endpoint pair.
		let mut offset: u64 = 0;
		let mut in_storage: bool = false;
		let mut ep_in: Option<(u32, u32)> = None; // (dci, mps)
		let mut ep_out: Option<(u32, u32)> = None;
		while offset + 2 <= total as u64 {
			let length: u64 = r8(dev.data_virt + offset) as u64;
			let kind: u8 = r8(dev.data_virt + offset + 1);
			if length < 2 {
				break;
			}
			if kind == DT_INTERFACE {
				in_storage = r8(dev.data_virt + offset + 5) == CLASS_MASS_STORAGE && r8(dev.data_virt + offset + 6) == SUBCLASS_SCSI && r8(dev.data_virt + offset + 7) == PROTOCOL_BULK_ONLY;
			}
			if kind == DT_ENDPOINT && in_storage {
				let ep_addr: u8 = r8(dev.data_virt + offset + 2);
				let attrs: u8 = r8(dev.data_virt + offset + 3);
				if attrs & 0x3 == EP_ATTR_BULK {
					let mps: u32 = r8(dev.data_virt + offset + 4) as u32 | (r8(dev.data_virt + offset + 5) as u32) << 8;
					let dci: u32 = (ep_addr & 0xf) as u32 * 2 + if ep_addr & 0x80 != 0 { 1 } else { 0 };
					if ep_addr & 0x80 != 0 && ep_in.is_none() {
						ep_in = Some((dci, mps));
					} else if ep_addr & 0x80 == 0 && ep_out.is_none() {
						ep_out = Some((dci, mps));
					}
				}
			}
			offset += length;
		}
		let (dci_in, mps_in): (u32, u32) = ep_in?;
		let (dci_out, mps_out): (u32, u32) = ep_out?;

		// bring both bulk endpoints up with one configure-endpoint command.
		let ring_in: Ring = Ring::new()?;
		let ring_out: Ring = Ring::new()?;
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		((dev.in_virt + 4) as *mut u32).write_volatile(1 | 1 << dci_in | 1 << dci_out);
		let entries: u32 = dci_in.max(dci_out);
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(entries << 27 | dev.speed << 20);
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
		control_nodata(hc, dev, 0x00, REQ_SET_CONFIGURATION, config_value, 0)?;

		let (_sh, data_virt, data_phys): (u64, u64, u64) = dma_page()?;
		let mut st: Storage = Storage { dci_in, dci_out, ring_in, ring_out, data_virt, data_phys, tag: 1 };
		let mut keyboard: Option<(UsbDevice, Keyboard)> = None;

		// spin the unit up: a freshly attached unit reports a power-on unit attention
		// on its first command, cleared by reading the sense data - retry a few times.
		let mut ready: bool = false;
		let mut attempt: u32 = 0;
		while attempt < 4 {
			let turcb: [u8; 6] = [SCSI_TEST_UNIT_READY, 0, 0, 0, 0, 0];
			if bot_command(hc, &mut keyboard, dev, &mut st, &turcb, 0, false) {
				ready = true;
				break;
			}
			let sense: [u8; 6] = [SCSI_REQUEST_SENSE, 0, 0, 0, 18, 0];
			let _ = bot_command(hc, &mut keyboard, dev, &mut st, &sense, 18, true);
			attempt += 1;
		}
		if !ready {
			return None;
		}
		// the block protocol serves 512-byte sectors; refuse a disk with another size.
		let capcb: [u8; 10] = [SCSI_READ_CAPACITY10, 0, 0, 0, 0, 0, 0, 0, 0, 0];
		if !bot_command(hc, &mut keyboard, dev, &mut st, &capcb, 8, true) {
			return None;
		}
		let block_size: u32 = (r8(st.data_virt + 4) as u32) << 24 | (r8(st.data_virt + 5) as u32) << 16 | (r8(st.data_virt + 6) as u32) << 8 | r8(st.data_virt + 7) as u32;
		if block_size != SECTOR {
			return None;
		}
		Some(st)
	}
}

// Run one SCSI command over the Bulk-Only Transport: the CBW on the bulk OUT
// endpoint, the data stage (into or out of the storage data page), and the CSW on
// the bulk IN endpoint, whose signature, tag echo and status decide the result.
// Keyboard events arriving during the waits are serviced inline.
unsafe fn bot_command(hc: &mut Xhci, keyboard: &mut Option<(UsbDevice, Keyboard)>, dev: &mut UsbDevice, st: &mut Storage, cb: &[u8], data_len: u32, data_in: bool) -> bool {
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
		if wait_transfer(hc, keyboard, dev.slot, st.dci_out) != Some(CC_SUCCESS) {
			return false;
		}
		// the data stage, on the direction's endpoint, out of the storage data page.
		if data_len > 0 {
			let (ring, dci): (&mut Ring, u32) = if data_in { (&mut st.ring_in, st.dci_in) } else { (&mut st.ring_out, st.dci_out) };
			ring.push(st.data_phys, data_len, TRB_NORMAL << 10 | TRB_IOC);
			w32(hc.db + dev.slot as u64 * 4, dci);
			let code: u32 = match wait_transfer(hc, keyboard, dev.slot, dci) {
				Some(c) => c,
				None => return false,
			};
			if code != CC_SUCCESS && code != CC_SHORT_PACKET {
				return false;
			}
		}
		// the CSW closes the transaction.
		st.ring_in.push(dev.data_phys + CSW_OFF, CSW_LEN, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, st.dci_in);
		let code: u32 = match wait_transfer(hc, keyboard, dev.slot, st.dci_in) {
			Some(c) => c,
			None => return false,
		};
		if code != CC_SUCCESS && code != CC_SHORT_PACKET {
			return false;
		}
		let csw: u64 = dev.data_virt + CSW_OFF;
		let ok: bool = (csw as *const u32).read_volatile() == CSW_SIGNATURE && ((csw + 4) as *const u32).read_volatile() == st.tag && ((csw + 12) as *const u8).read_volatile() == 0;
		st.tag = st.tag.wrapping_add(1);
		ok
	}
}

// Serve one block request from the StorageService instance: [op u32][lba u64]
// [count u32], count clamped to one DMA page. A read replies [status u32] + a
// MemoryObject of the sectors; a write carries a MemoryObject in and replies
// [status u32] - the same wire contract driver.virtio-blk serves.
unsafe fn serve_block_request(hc: &mut Xhci, keyboard: &mut Option<(UsbDevice, Keyboard)>, dev: &mut UsbDevice, st: &mut Storage, blk_server: u64, req: &[u8; 16], handle: u64) {
	unsafe {
		let op: u32 = u32::from_le_bytes([req[0], req[1], req[2], req[3]]);
		let lba: u64 = u64::from_le_bytes([req[4], req[5], req[6], req[7], req[8], req[9], req[10], req[11]]);
		let count: u32 = u32::from_le_bytes([req[12], req[13], req[14], req[15]]).clamp(1, MAX_SECTORS);
		match op {
			OP_READ => serve_read(hc, keyboard, dev, st, blk_server, lba, count),
			OP_WRITE => serve_write(hc, keyboard, dev, st, blk_server, lba, count, handle),
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
// shared buffer and hand it to the client, or reply with an error status.
unsafe fn serve_read(hc: &mut Xhci, keyboard: &mut Option<(UsbDevice, Keyboard)>, dev: &mut UsbDevice, st: &mut Storage, blk_server: u64, lba: u64, count: u32) {
	unsafe {
		let bytes: u64 = count as u64 * SECTOR as u64;
		let cb: [u8; 10] = read10_cb(SCSI_READ10, lba, count);
		if !bot_command(hc, keyboard, dev, st, &cb, bytes as u32, true) {
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
// SCSI WRITE(10), then reply with the status and no buffer.
unsafe fn serve_write(hc: &mut Xhci, keyboard: &mut Option<(UsbDevice, Keyboard)>, dev: &mut UsbDevice, st: &mut Storage, blk_server: u64, lba: u64, count: u32, src_handle: u64) {
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
		core::ptr::copy_nonoverlapping(src as *const u8, st.data_virt as *mut u8, bytes as usize);
		unmap_object(src_handle);
		close(src_handle);
		let cb: [u8; 10] = read10_cb(SCSI_WRITE10, lba, count);
		let ok: bool = bot_command(hc, keyboard, dev, st, &cb, bytes as u32, false);
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

// Diff one HID boot report against the previous one and feed the changes as key
// events: byte 0 carries the modifier bitmask, bytes 2..8 the pressed keys' usage
// ids (1..3 are the keyboard's error codes, skipped). Releases are fed first so a
// fast key swap cannot double-modify.
unsafe fn feed_report(prev: &[u8; 8], report: &[u8; 8], mods: &mut Mods) {
	unsafe {
		for bit in 0..8u8 {
			let mask: u8 = 1 << bit;
			if prev[0] & mask != report[0] & mask {
				let code: u16 = keys::HID_MODIFIER_KEYCODES[bit as usize];
				if code != 0 {
					keys::feed_key(code, (report[0] & mask != 0) as u32, mods);
				}
			}
		}
		for &usage in &prev[2..8] {
			if usage > 3 && !report[2..8].contains(&usage) {
				let code: u16 = keys::hid_keycode(usage);
				if code != 0 {
					keys::feed_key(code, 0, mods);
				}
			}
		}
		for &usage in &report[2..8] {
			if usage > 3 && !prev[2..8].contains(&usage) {
				let code: u16 = keys::hid_keycode(usage);
				if code != 0 {
					keys::feed_key(code, 1, mods);
				}
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
