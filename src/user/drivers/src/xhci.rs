// driver.xhci - the userspace xHCI USB host controller driver (M62).
//
// DeviceManager launches this program with a "DEVICE" message carrying the
// controller's DeviceInfo and a transferred DeviceMemory capability to its MMIO
// BAR (the whole xHCI register file). The driver maps the BAR, resets the
// controller, builds the device context base array, the command ring and the
// event ring, starts the controller, and enumerates the root-hub ports: each
// connected device is reset, given a device slot, addressed, and has its device
// descriptor read over a control transfer on the default endpoint. It reports the
// number of addressed devices and stands holding the controller; the USB class
// drivers (HID keyboard, mass storage) grow on top of this bring-up in the next
// steps. Synchronous and polled throughout - commands and transfers are one at a
// time, completions are reaped by polling the event ring, matching the polled
// virtio-blk/gpu drivers.

#![no_std]
#![no_main]

use rt::*;

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
const IR_ERSTSZ: u64 = 0x08;
const IR_ERSTBA: u64 = 0x10;
const IR_ERDP: u64 = 0x18;
const ERDP_EHB: u64 = 1 << 3; // event handler busy (RW1C)

// TRB types (control-word bits 15:10).
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_ADDRESS_DEVICE: u32 = 11;
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
const DESC_DEVICE: u16 = 1;

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

// The controller with its register windows resolved and its rings built.
struct Xhci {
	// Operational, runtime-interrupter-0 and doorbell-array register bases.
	op: u64,
	ir0: u64,
	db: u64,
	// 64-byte contexts when set (HCCPARAMS1.CSZ); 32-byte otherwise.
	ctx_size: u64,
	ports: u32,
	// Command ring: virtual base, producer index and cycle state.
	cmd_virt: u64,
	cmd_index: u64,
	cmd_cycle: u32,
	// Event ring: virtual/physical base, consumer index and cycle state.
	evt_virt: u64,
	evt_phys: u64,
	evt_index: u64,
	evt_cycle: u32,
	// Device context base address array (virtual base; entry per slot).
	dcbaa_virt: u64,
}

// One addressed USB device: its slot, root-hub port, speed, and the default
// endpoint's transfer ring (virtual/physical, producer index, cycle state).
struct UsbDevice {
	slot: u32,
	port: u32,
	speed: u32,
	ep0_virt: u64,
	ep0_phys: u64,
	ep0_index: u64,
	ep0_cycle: u32,
	// The device descriptor's identity fields.
	vendor: u16,
	product: u16,
	class: u8,
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
		// map the controller's register file.
		let base: u64 = syscall(SYS_DEVICE_MEMORY_MAP, device_handle, 0, 0, 0);
		if sys_is_err(base) {
			exit();
		}
		let mut hc: Xhci = match bring_up(base) {
			Some(hc) => hc,
			None => exit(),
		};
		// enumerate the root-hub ports and address every connected device.
		let mut devices: u32 = 0;
		let mut port: u32 = 1;
		while port <= hc.ports {
			if let Some(dev) = attach_port(&mut hc, port) {
				report_device(&dev);
				devices += 1;
			}
			port += 1;
		}
		// report in, then stand holding the controller until DeviceManager drops the
		// bootstrap channel.
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
		send_blocking(bootstrap, &report[..n], 0);
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
		let (_ch, cmd_virt, cmd_phys): (u64, u64, u64) = dma_page()?;
		let link: u64 = cmd_virt + (RING_TRBS - 1) * 16;
		(link as *mut u64).write_volatile(cmd_phys);
		((link + 12) as *mut u32).write_volatile(TRB_LINK << 10 | TRB_TOGGLE_CYCLE);
		w64(op + OP_CRCR, cmd_phys | 1);

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

		// enable every device slot and start the controller.
		w32(op + OP_CONFIG, slots);
		w32(op + OP_USBCMD, r32(op + OP_USBCMD) | CMD_RUN);
		wait_clear(op + OP_USBSTS, STS_HCHALTED)?;

		Some(Xhci { op, ir0, db, ctx_size: if csz { 64 } else { 32 }, ports, cmd_virt, cmd_index: 0, cmd_cycle: 1, evt_virt, evt_phys, evt_index: 0, evt_cycle: 1, dcbaa_virt })
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

// Push one TRB onto the command ring (following the link TRB on wrap) and ring the
// command doorbell.
unsafe fn command(hc: &mut Xhci, param: u64, status: u32, control: u32) {
	unsafe {
		let trb: u64 = hc.cmd_virt + hc.cmd_index * 16;
		(trb as *mut u64).write_volatile(param);
		((trb + 8) as *mut u32).write_volatile(status);
		((trb + 12) as *mut u32).write_volatile(control | hc.cmd_cycle);
		hc.cmd_index += 1;
		if hc.cmd_index == RING_TRBS - 1 {
			// consume the link TRB: give it the producer cycle and wrap.
			let link: u64 = hc.cmd_virt + hc.cmd_index * 16;
			let ctl: u32 = ((link + 12) as *const u32).read_volatile() & !TRB_CYCLE;
			((link + 12) as *mut u32).write_volatile(ctl | hc.cmd_cycle);
			hc.cmd_index = 0;
			hc.cmd_cycle ^= 1;
		}
		w32(hc.db, 0);
	}
}

// Poll the event ring until an event of `wanted` type arrives, acknowledging the
// dequeue pointer as events are consumed. Port-status-change events are skipped
// (enumeration reads PORTSC directly). Returns (param, status, control) of the
// matching event, or None on budget exhaustion or an unexpected event type.
unsafe fn wait_event(hc: &mut Xhci, wanted: u32) -> Option<(u64, u32, u32)> {
	unsafe {
		let mut spins: u32 = 0;
		loop {
			let trb: u64 = hc.evt_virt + hc.evt_index * 16;
			let control: u32 = ((trb + 12) as *const u32).read_volatile();
			if control & TRB_CYCLE == hc.evt_cycle {
				let param: u64 = (trb as *const u64).read_volatile();
				let status: u32 = ((trb + 8) as *const u32).read_volatile();
				let kind: u32 = control >> 10 & 0x3f;
				// consume the event and publish the new dequeue pointer.
				hc.evt_index += 1;
				if hc.evt_index == RING_TRBS {
					hc.evt_index = 0;
					hc.evt_cycle ^= 1;
				}
				w64(hc.ir0 + IR_ERDP, hc.evt_phys + hc.evt_index * 16 | ERDP_EHB);
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
		let (_th, ep0_virt, ep0_phys): (u64, u64, u64) = dma_page()?;

		let mut dev: UsbDevice = UsbDevice { slot, port, speed, ep0_virt, ep0_phys, ep0_index: 0, ep0_cycle: 1, vendor: 0, product: 0, class: 0 };

		// address the device: an input context whose slot context names the port and
		// whose endpoint-0 context points at the transfer ring.
		let (_ih, in_virt, in_phys): (u64, u64, u64) = dma_page()?;
		write_address_contexts(hc, &dev, in_virt, initial_packet_size(speed));
		command_and_wait(hc, in_phys, 0, TRB_ADDRESS_DEVICE << 10 | slot << 24)?;

		// read the descriptor head first: its bMaxPacketSize0 field tells the real
		// default-endpoint packet size, which full-speed devices are allowed to vary.
		let (_bh, data_virt, data_phys): (u64, u64, u64) = dma_page()?;
		control_in(hc, &mut dev, DESC_DEVICE, 8, data_phys)?;
		let mps: u32 = r8(data_virt + 7) as u32;
		if mps != initial_packet_size(speed) && mps >= 8 {
			// fix endpoint 0 up with an evaluate-context command, then re-read.
			write_address_contexts(hc, &dev, in_virt, mps);
			// evaluate-context consumes only the endpoint-0 add flag.
			((in_virt + 4) as *mut u32).write_volatile(1 << 1);
			command_and_wait(hc, in_phys, 0, TRB_EVALUATE_CONTEXT << 10 | slot << 24)?;
		}
		control_in(hc, &mut dev, DESC_DEVICE, 18, data_phys)?;
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

// Fill the input context at `in_virt` for an address-device command: the input
// control context adds the slot and endpoint-0 contexts, the slot context names
// the root-hub port and speed, and the endpoint-0 context is a control endpoint
// with max packet size `mps` whose transfer ring is the device's.
unsafe fn write_address_contexts(hc: &Xhci, dev: &UsbDevice, in_virt: u64, mps: u32) {
	unsafe {
		core::ptr::write_bytes(in_virt as *mut u8, 0, 4096);
		// input control context: add slot (A0) + endpoint 0 (A1).
		((in_virt + 4) as *mut u32).write_volatile(0x3);
		// slot context: one context entry, the device's speed and root-hub port.
		let slot_ctx: u64 = in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(1 << 27 | dev.speed << 20);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		// endpoint-0 context: a control endpoint (type 4), error count 3, the ring's
		// physical base with the producer's cycle state, average TRB length 8.
		let ep0_ctx: u64 = in_virt + 2 * hc.ctx_size;
		((ep0_ctx + 4) as *mut u32).write_volatile(mps << 16 | 4 << 3 | 3 << 1);
		((ep0_ctx + 8) as *mut u32).write_volatile((dev.ep0_phys | dev.ep0_cycle as u64) as u32);
		((ep0_ctx + 12) as *mut u32).write_volatile((dev.ep0_phys >> 32) as u32);
		((ep0_ctx + 16) as *mut u32).write_volatile(8);
	}
}

// Push one TRB onto the device's default-endpoint transfer ring. The ring is one
// page and enumeration pushes only a handful of TRBs, so no link TRB is needed.
unsafe fn push_ep0(dev: &mut UsbDevice, param: u64, status: u32, control: u32) {
	unsafe {
		let trb: u64 = dev.ep0_virt + dev.ep0_index * 16;
		(trb as *mut u64).write_volatile(param);
		((trb + 8) as *mut u32).write_volatile(status);
		((trb + 12) as *mut u32).write_volatile(control | dev.ep0_cycle);
		dev.ep0_index += 1;
	}
}

// Read `len` bytes of descriptor `desc` from the device into the DMA page at
// `data_phys` with a GET_DESCRIPTOR control transfer on the default endpoint:
// setup stage (the 8-byte request rides in the TRB itself), IN data stage, OUT
// status stage, then the doorbell and the transfer completion event.
unsafe fn control_in(hc: &mut Xhci, dev: &mut UsbDevice, desc: u16, len: u16, data_phys: u64) -> Option<()> {
	unsafe {
		let request: u64 = 0x80 | (REQ_GET_DESCRIPTOR as u64) << 8 | ((desc << 8) as u64) << 16 | (len as u64) << 48;
		push_ep0(dev, request, 8, TRB_SETUP << 10 | TRB_IDT | TRB_TRT_IN);
		push_ep0(dev, data_phys, len as u32, TRB_DATA << 10 | TRB_DIR_IN);
		push_ep0(dev, 0, 0, TRB_STATUS << 10 | TRB_IOC);
		// ring the device slot's doorbell for the default control endpoint (DCI 1).
		w32(hc.db + dev.slot as u64 * 4, 1);
		let (_p, status, _c): (u64, u32, u32) = wait_event(hc, TRB_EV_TRANSFER)?;
		let code: u32 = status >> 24;
		if code != CC_SUCCESS && code != CC_SHORT_PACKET {
			None
		} else {
			Some(())
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
