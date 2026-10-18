// The mass-storage side of driver.xhci: the xHCI half of SCSI over the Bulk-Only Transport.
//
// A SCSI Bulk-Only interface found during enumeration is configured here - both bulk endpoints brought up in one
// configure-endpoint command - and its transport is the shared one, `drivers::usb_bot`: the CBW and CSW, their
// recovery, the unit's start and the block wire a StorageService instance mounts the stick through as vol://usb,
// the same code every other USB host controller's driver runs. What is xHCI's is below: a bulk transfer is one
// Normal TRB on the endpoint's ring, a stall is recovered with Reset Endpoint and Set TR Dequeue Pointer before the
// device-side CLEAR_FEATURE, and HID events arriving during the synchronous waits are serviced inline in xhci.rs.

use drivers::descriptor;
use drivers::usb_bot::{self, At, Moved, Unit};
use rt::*;

use crate::usb_hid::Hids;
use crate::{CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, DESC_CONFIG, FEATURE_ENDPOINT_HALT, REQ_CLEAR_FEATURE, REQ_SET_CONFIGURATION, RT_ENDPOINT, TRB_CONFIGURE_ENDPOINT, TRB_IOC, TRB_NORMAL};
use crate::{Ring, UsbDevice, Xhci};
use crate::{command_and_wait, control_in, control_nodata, dma_page, reset_endpoint, w32, wait_transfer_len};

// THE BLOCK WIRE COMES FROM THE CRATE THAT OWNS IT, and these two are named here for xhci.rs's own refusals.
pub use driver_protocol::block::{STATUS_ERR, STATUS_INVALID};
pub use drivers::usb_bot::reply_block;

// The longest data stage one transfer carries: the xHCI Normal TRB's 17-bit transfer-length field, 64 kB here as
// one aligned TRB.
const TRB_DATA_MAX: u32 = 64 * 1024;

// A configured USB mass-storage device: its xHCI pipes, and the started unit the shared transport keeps.
pub struct Storage {
	pipes: Pipes,
	unit: Unit,
}

// THE TWO BULK PIPES AND THE DATA BUFFER: the endpoints' device context indices, addresses (for stall recovery) and
// transfer rings, and a growable contiguous DMA buffer for the sector data (the CBW/CSW frames ride in the device's
// scratch page).
struct Pipes {
	dci_in: u32,
	dci_out: u32,
	ep_in_addr: u8,
	ep_out_addr: u8,
	ring_in: Ring,
	ring_out: Ring,
	data_virt: u64,
	data_phys: u64,
	// The data buffer's handle and size: a contiguous DMA span grown to the largest request seen (the old buffer is
	// released when replaced).
	data_handle: u64,
	data_bytes: u64,
}

impl Storage {
	// Give this device's rings and data buffer back when it goes away.
	pub fn release(&mut self) {
		let pipes = &mut self.pipes;
		pipes.ring_in.release();
		pipes.ring_out.release();
		if pipes.data_handle != 0 {
			close(pipes.data_handle);
			pipes.data_handle = 0;
			pipes.data_bytes = 0;
		}
	}
}

impl Pipes {
	// Ensure the data buffer holds `bytes`, reallocating a larger contiguous span when a request outgrows it. False
	// when the allocation fails.
	fn fit_data(&mut self, bytes: u64) -> bool {
		if bytes <= self.data_bytes {
			return true;
		}
		// THE BUFFER IS BOUNDED BY WHAT THE ADMISSION RESERVED. It grows to the largest request anybody makes and
		// never shrinks, inside the controller's Domain, for the life of the process - so without this the bound on
		// it is whatever the largest transfer ever asked for was. The number is the class module's own, charged
		// when this device was admitted.
		if !drivers::usb_class::Budget::buffer_within(drivers::usb_class::ClassKind::Storage, bytes) {
			return false;
		}
		// SAFETY: a DMA buffer for this controller, mapped for this process.
		let Some((handle, virt, phys)) = (unsafe { dma_buffer_for(crate::device(), bytes) }) else { return false };
		if self.data_handle != 0 {
			close(self.data_handle);
		}
		self.data_handle = handle;
		self.data_virt = virt;
		self.data_phys = phys;
		self.data_bytes = bytes;
		true
	}
}

// THE xHCI SIDE OF THE TRANSPORT, for one transaction's span: the controller, the HID devices whose reports a wait
// services inline, the device and its pipes.
struct Bot<'a> {
	hc: &'a mut Xhci,
	hids: &'a mut Hids,
	dev: &'a mut UsbDevice,
	pipes: &'a mut Pipes,
}

impl usb_bot::Transport for Bot<'_> {
	fn bulk(&mut self, dir_in: bool, at: At, len: u32) -> Moved {
		let phys = match at {
			At::Wrapper(offset) => self.dev.data_phys + offset as u64,
			At::Data => self.pipes.data_phys,
		};
		let (ring, dci) = if dir_in { (&mut self.pipes.ring_in, self.pipes.dci_in) } else { (&mut self.pipes.ring_out, self.pipes.dci_out) };
		// SAFETY: the ring and the doorbell are this controller's, and `phys` is a page it was given.
		unsafe {
			ring.push(phys, len, TRB_NORMAL << 10 | TRB_IOC);
			w32(self.hc.db + self.dev.slot as u64 * 4, dci);
		}
		match wait_transfer_len(self.hc, self.hids, self.dev.slot, dci) {
			Some((CC_SUCCESS | CC_SHORT_PACKET, residual)) => Moved::Done(len.saturating_sub(residual)),
			Some((CC_STALL, residual)) => Moved::Stall(len.saturating_sub(residual)),
			_ => Moved::Failed,
		}
	}

	// The controller-side Reset Endpoint + Set TR Dequeue Pointer pair, then the device-side
	// CLEAR_FEATURE(ENDPOINT_HALT) to the endpoint's address, resetting its data toggle.
	fn recover(&mut self, dir_in: bool) {
		let pipes = &*self.pipes;
		let (dci, addr, dequeue): (u32, u8, u64) = if dir_in { (pipes.dci_in, pipes.ep_in_addr, pipes.ring_in.phys + pipes.ring_in.index * 16 | pipes.ring_in.cycle as u64) } else { (pipes.dci_out, pipes.ep_out_addr, pipes.ring_out.phys + pipes.ring_out.index * 16 | pipes.ring_out.cycle as u64) };
		reset_endpoint(self.hc, self.hids, self.dev.slot, dci, dequeue);
		let _ = control_nodata(self.hc, self.hids, self.dev, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, addr as u16);
	}

	fn class_reset(&mut self, iface: u16) {
		let _ = control_nodata(self.hc, self.hids, self.dev, usb_bot::RT_CLASS_INTERFACE, usb_bot::BOT_REQ_RESET, 0, iface);
	}

	fn wrapper(&mut self) -> &mut [u8] {
		// SAFETY: the device's scratch page, a whole page this driver allocated and nothing else holds a reference
		// into while the transaction runs.
		unsafe { core::slice::from_raw_parts_mut(self.dev.data_virt as *mut u8, 64) }
	}

	fn data(&mut self, bytes: u64) -> Option<&mut [u8]> {
		if !self.pipes.fit_data(bytes) {
			return None;
		}
		// SAFETY: the data buffer, `data_bytes` long and mapped for this process.
		Some(unsafe { core::slice::from_raw_parts_mut(self.pipes.data_virt as *mut u8, self.pipes.data_bytes as usize) })
	}

	fn max_transfer(&self) -> u32 {
		TRB_DATA_MAX
	}
}

// Configure the device's mass-storage function, if it has one: find a SCSI Bulk-Only interface and its bulk IN/OUT
// endpoint pair in the configuration descriptor, bring both endpoints up, select the configuration, then start the
// unit through the shared transport. None when the device is not a disk or any step fails.
pub unsafe fn configure_storage(hc: &mut Xhci, dev: &mut UsbDevice) -> Option<Storage> {
	unsafe {
		// no HID device is serving yet, so the control / transport waits see no HID events (report TRBs are only
		// posted once the service loop starts).
		let mut hids: Hids = Hids::new();
		// THE HEAD IS A RECORD LIKE ANY OTHER, and a device that returned four of its nine bytes has not sent the
		// total length the rest of this function is computed from.
		let head = control_in(hc, &mut hids, dev, DESC_CONFIG, 9)?;
		let head = descriptor::config_head(core::slice::from_raw_parts(dev.data_virt as *const u8, head.min(9) as usize)).ok()?;
		let total: u16 = head.total.min(1024);
		let received = control_in(hc, &mut hids, dev, DESC_CONFIG, total)?;
		// AND THE WHOLE DESCRIPTOR HAS TO HAVE ARRIVED before it is walked: the page is reused, so the bytes past a
		// short answer are the previous transfer's.
		let total = descriptor::check_transfer(total, received).ok()?;
		let found = usb_bot::find(core::slice::from_raw_parts(dev.data_virt as *const u8, total as usize))?;
		let dci = |address: u8| (address & 0xf) as u32 * 2 + (address >> 7) as u32;
		let (dci_in, mps_in, ep_in_addr) = (dci(found.ep_in.address), found.ep_in.max_packet as u32, found.ep_in.address);
		let (dci_out, mps_out, ep_out_addr) = (dci(found.ep_out.address), found.ep_out.max_packet as u32, found.ep_out.address);

		// BOTH RINGS AND THE DATA PAGE ARE TAKEN BEFORE THE CONTROLLER IS TOLD ABOUT EITHER RING, and all three are
		// given back on any failure from here on. An attach that failed part way - an empty card reader whose unit
		// never comes ready, a disk whose sector is not 512 bytes, a device that refused its configuration - kept
		// them for the life of the driver, while the class budget the detach gives back had charged for none of
		// them.
		let mut st: Storage = match (Ring::new(), Ring::new(), dma_page()) {
			(Some(ring_in), Some(ring_out), Some((data_handle, data_virt, data_phys))) => Storage { pipes: Pipes { dci_in, dci_out, ep_in_addr, ep_out_addr, ring_in, ring_out, data_virt, data_phys, data_handle, data_bytes: 4096 }, unit: Unit::new(found.iface) },
			(ring_in, ring_out, data) => {
				for mut ring in [ring_in, ring_out].into_iter().flatten() {
					ring.release();
				}
				if let Some((handle, _, _)) = data {
					close(handle);
				}
				return None;
			}
		};
		let usable = 'unit: {
			// bring both bulk endpoints up with one configure-endpoint command.
			core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
			((dev.in_virt + 4) as *mut u32).write_volatile(1 | 1 << dci_in | 1 << dci_out);
			let entries: u32 = dci_in.max(dci_out);
			let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
			(slot_ctx as *mut u32).write_volatile(entries << 27 | dev.speed << 20 | dev.route);
			((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
			// bulk endpoint contexts: IN type 6 / OUT type 2, error count 3, no interval.
			for &(dci, mps, ep_type, ring) in &[(dci_in, mps_in, 6u32, &st.pipes.ring_in), (dci_out, mps_out, 2u32, &st.pipes.ring_out)] {
				let ep_ctx: u64 = dev.in_virt + (1 + dci as u64) * hc.ctx_size;
				((ep_ctx + 4) as *mut u32).write_volatile(mps << 16 | ep_type << 3 | 3 << 1);
				((ep_ctx + 8) as *mut u32).write_volatile((ring.phys | ring.cycle as u64) as u32);
				((ep_ctx + 12) as *mut u32).write_volatile((ring.phys >> 32) as u32);
				((ep_ctx + 16) as *mut u32).write_volatile(mps);
			}
			if command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24).is_none() || control_nodata(hc, &mut hids, dev, 0x00, REQ_SET_CONFIGURATION, head.value as u16, 0).is_none() {
				break 'unit false;
			}
			let Storage { pipes, unit } = &mut st;
			usb_bot::start(&mut Bot { hc, hids: &mut hids, dev, pipes }, unit)
		};
		if !usable {
			st.release();
			return None;
		}
		Some(st)
	}
}

// Serve one block request from the StorageService instance through the shared transport - the same wire contract
// driver.virtio-blk serves.
pub fn serve_block_request(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, st: &mut Storage, blk_server: u64, req: &[u8; 16], handle: u64) {
	let Storage { pipes, unit } = st;
	usb_bot::serve(&mut Bot { hc, hids, dev, pipes }, unit, blk_server, req, handle);
}
