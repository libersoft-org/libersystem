// The mass-storage side of driver.xhci: SCSI over the Bulk-Only Transport.
//
// A SCSI Bulk-Only interface found during enumeration is configured here (both
// bulk endpoints brought up, the unit spun up and its geometry checked) and
// served: block requests arriving on the driver's block channel run as
// READ(10)/WRITE(10)/SYNCHRONIZE CACHE commands - the same wire contract
// driver.virtio-blk serves, so a StorageService instance mounts the stick as
// vol://usb. The controller plumbing (rings, transfers, endpoint recovery)
// stays in xhci.rs; HID events arriving during the synchronous waits are
// serviced inline there.

use rt::*;

use crate::usb_hid::Hids;
use crate::{CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, DESC_CONFIG, DT_ENDPOINT, DT_INTERFACE, FEATURE_ENDPOINT_HALT, REQ_CLEAR_FEATURE, REQ_SET_CONFIGURATION, RT_ENDPOINT, TRB_CONFIGURE_ENDPOINT, TRB_IOC, TRB_NORMAL};
use crate::{Ring, UsbDevice, Xhci};
use crate::{command_and_wait, control_in, control_nodata, dma_page, r8, reset_endpoint, w32, wait_transfer};

// The USB mass-storage identity within a configuration: the class with the SCSI
// transparent command set over the Bulk-Only Transport.
const CLASS_MASS_STORAGE: u8 = 8;
const SUBCLASS_SCSI: u8 = 6;
const PROTOCOL_BULK_ONLY: u8 = 0x50;
const EP_ATTR_BULK: u8 = 2;

// The Bulk-Only Mass Storage Reset class request (to the interface): the last-resort
// recovery that returns the device's BOT state machine to idle after a transport
// error a per-endpoint stall recovery cannot fix.
const BOT_REQ_RESET: u8 = 0xff;
const RT_CLASS_INTERFACE: u8 = 0x21;

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
// capacity query (op 2) replies [status u32][capacity bytes u64][max sectors u32],
// and a flush (op 3)
// - the write barrier, served as SCSI SYNCHRONIZE CACHE (10) - replies [status u32].
// The data stage rides a contiguous DMA buffer grown to the request, so one
// READ(10)/WRITE(10) moves the whole span; the only per-request bound is the
// xHCI Normal TRB's 17-bit transfer-length field (the protocol's own limit, 64 kB
// here as one aligned TRB), not a page unit.
const SECTOR: u32 = 512;
const TRB_DATA_MAX: u32 = 64 * 1024;
const OP_READ: u32 = 0;
const OP_WRITE: u32 = 1;
const OP_CAPACITY: u32 = 2;
const OP_FLUSH: u32 = 3;
const STATUS_OK: u32 = 0;
pub const STATUS_ERR: u32 = 1;

// A configured USB mass-storage device (Bulk-Only Transport): the bulk IN and OUT
// endpoints' device context indices, addresses (for stall recovery) and transfer
// rings, the interface number (for the BOT reset), a growable contiguous DMA buffer
// for the sector data (the CBW/CSW frames ride in the device's scratch page), and
// the rolling CBW tag.
pub struct Storage {
	dci_in: u32,
	dci_out: u32,
	ep_in_addr: u8,
	ep_out_addr: u8,
	iface: u16,
	ring_in: Ring,
	ring_out: Ring,
	data_virt: u64,
	data_phys: u64,
	// The data buffer's handle and size: a contiguous DMA span grown to the
	// largest request seen (the old buffer is released when replaced).
	data_handle: u64,
	data_bytes: u64,
	tag: u32,
	// The unit's size in bytes, from READ CAPACITY at configuration - answered to
	// OP_CAPACITY queries for the `lsblk` inventory.
	capacity: u64,
}

impl Storage {
	// Ensure the data buffer holds `bytes`, reallocating a larger contiguous span
	// when a request outgrows it. False when the allocation fails.
	unsafe fn fit_data(&mut self, bytes: u64) -> bool {
		unsafe {
			if bytes <= self.data_bytes {
				return true;
			}
			let (handle, virt, phys): (u64, u64, u64) = match dma_buffer(bytes) {
				Some(t) => t,
				None => return false,
			};
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
}

// Configure the device's mass-storage function, if it has one: find a SCSI
// Bulk-Only interface and its bulk IN/OUT endpoint pair in the configuration
// descriptor, bring both endpoints up, select the configuration, then spin the
// unit up (TEST UNIT READY, clearing the power-on sense) and check its block size
// is the 512-byte sector the block protocol serves. None when the device is not a
// disk or any step fails.
pub unsafe fn configure_storage(hc: &mut Xhci, dev: &mut UsbDevice) -> Option<Storage> {
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

		let (data_handle, data_virt, data_phys): (u64, u64, u64) = dma_page()?;
		let mut st: Storage = Storage { dci_in, dci_out, ep_in_addr, ep_out_addr, iface, ring_in, ring_out, data_virt, data_phys, data_handle, data_bytes: 4096, tag: 1, capacity: 0 };

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
// endpoint, the data stage (into or out of the storage data buffer), and the CSW on
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
		// the data stage, on the direction's endpoint, out of the storage data buffer. A
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
// [count u32], count clamped to one TRB's data stage. A read replies [status u32] + a
// MemoryObject of the sectors; a write carries a MemoryObject in and replies
// [status u32] - the same wire contract driver.virtio-blk serves.
pub unsafe fn serve_block_request(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, st: &mut Storage, blk_server: u64, req: &[u8; 16], handle: u64) {
	unsafe {
		let op: u32 = u32::from_le_bytes([req[0], req[1], req[2], req[3]]);
		let lba: u64 = u64::from_le_bytes([req[4], req[5], req[6], req[7], req[8], req[9], req[10], req[11]]);
		let count: u32 = u32::from_le_bytes([req[12], req[13], req[14], req[15]]).clamp(1, TRB_DATA_MAX / SECTOR);
		match op {
			OP_READ => serve_read(hc, hids, dev, st, blk_server, lba, count),
			OP_WRITE => serve_write(hc, hids, dev, st, blk_server, lba, count, handle),
			OP_CAPACITY => reply_capacity(blk_server, st.capacity, (TRB_DATA_MAX / SECTOR) as u64),
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
		if !st.fit_data(bytes) {
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
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
// buffer, so the sectors are copied in again before the retry.
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
		if !st.fit_data(bytes) {
			unmap_object(src_handle);
			close(src_handle);
			reply_block(blk_server, STATUS_ERR, 0);
			return;
		}
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
pub unsafe fn reply_block(blk_server: u64, status: u32, xfer: u64) {
	unsafe {
		let reply: [u8; 4] = status.to_le_bytes();
		send_blocking(blk_server, &reply, xfer);
	}
}

// Send a capacity reply: [status u32 LE][capacity bytes u64 LE][max sectors u32 LE],
// no handle - the same wire contract driver.virtio-blk serves; the cap here is the
// TRB data-stage bound.
unsafe fn reply_capacity(blk_server: u64, bytes: u64, max_sectors: u64) {
	unsafe {
		let mut reply: [u8; 16] = [0u8; 16];
		reply[..4].copy_from_slice(&STATUS_OK.to_le_bytes());
		reply[4..12].copy_from_slice(&bytes.to_le_bytes());
		reply[12..16].copy_from_slice(&(max_sectors.min(u32::MAX as u64) as u32).to_le_bytes());
		send_blocking(blk_server, &reply, 0);
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
