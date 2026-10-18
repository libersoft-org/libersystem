// SCSI OVER THE USB BULK-ONLY TRANSPORT, above any host controller: the transaction, its recovery, the unit's
// start, and the block wire a StorageService instance mounts the disk through.
//
// IT WAS THE xHCI DRIVER'S, and a second controller would have had to copy it. A controller moves the bytes - one
// bulk transfer at a time on the interface's two endpoints, a stall recovered on its side and the device's - and
// says how many moved; everything a Bulk-Only Transport IS lives here once: the 31-byte Command Block Wrapper and
// the 13-byte Command Status Wrapper, the residue checked against what the transport saw move, the recovery
// sequence (a stalled data stage unhalted and the CSW still read, a stalled CSW read once more, anything worse the
// Mass Storage Reset), the sense read, the unit spun up and its geometry checked, and READ(10)/WRITE(10)/
// SYNCHRONIZE CACHE served as the canonical block wire - the same contract driver.virtio-blk serves.

use driver_protocol::block;
use driver_protocol::block::{OP_CAPACITY, OP_FLUSH, OP_READ, OP_WRITE, STATUS_ERR, STATUS_INVALID, STATUS_OK};
use rt::*;

use crate::blk;
use crate::descriptor;
use crate::scsi;
use crate::usb::{BotFault, CSW_LEN, Csw, FlushOutcome, csw_outcome, flush_outcome};

// The USB mass-storage identity within a configuration: the class with the SCSI transparent command set over the
// Bulk-Only Transport.
pub const CLASS_MASS_STORAGE: u8 = 8;
const SUBCLASS_SCSI: u8 = 6;
const PROTOCOL_BULK_ONLY: u8 = 0x50;
const EP_ATTR_BULK: u8 = 2;

// The Bulk-Only Mass Storage Reset class request (to the interface): the last-resort recovery that returns the
// device's BOT state machine to idle after a transport error a per-endpoint stall recovery cannot fix.
pub const BOT_REQ_RESET: u8 = 0xff;
pub const RT_CLASS_INTERFACE: u8 = 0x21;

// Bulk-Only Transport framing: the 31-byte Command Block Wrapper led by its signature. The CSW rides in the same
// wrapper page as the CBW, at the next 32-byte slot.
const CBW_SIGNATURE: u32 = 0x4342_5355;
const CBW_LEN: u32 = 31;
pub const CSW_OFF: u32 = 32;
const CBW_FLAG_IN: u8 = 0x80;

// One disk sector: the block protocol serves 512-byte sectors, and a unit with another size is refused.
pub const SECTOR: u32 = 512;

/// A Bulk-Only SCSI interface of a configuration: its number and its bulk endpoint pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interface {
	pub iface: u16,
	pub ep_in: Bulk,
	pub ep_out: Bulk,
}

/// A bulk endpoint: its address and its maximum packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bulk {
	pub address: u8,
	pub max_packet: u16,
}

/// THE BULK-ONLY SCSI INTERFACE OF A CONFIGURATION and its endpoint pair - the first IN and the first OUT bulk
/// endpoint of the first interface that is mass storage, SCSI, Bulk-Only. EVERY FIELD IS READ FROM INSIDE ITS OWN
/// RECORD: a record whose `bLength` is two is legal, and the class byte at offset five of it is the next record's
/// header.
pub fn find(config: &[u8]) -> Option<Interface> {
	let mut in_storage = false;
	let mut iface: Option<u16> = None;
	let mut ep_in: Option<Bulk> = None;
	let mut ep_out: Option<Bulk> = None;
	for record in descriptor::Walk::new(config) {
		if record.kind == descriptor::DT_INTERFACE {
			if iface.is_some() && (ep_in.is_some() && ep_out.is_some()) {
				break;
			}
			in_storage = record.field(5) == Ok(CLASS_MASS_STORAGE) && record.field(6) == Ok(SUBCLASS_SCSI) && record.field(7) == Ok(PROTOCOL_BULK_ONLY);
			if in_storage && iface.is_none() {
				iface = Some(record.field(2).unwrap_or(0) as u16);
			}
		}
		if record.kind == descriptor::DT_ENDPOINT && in_storage {
			let (Ok(address), Ok(attrs)) = (record.field(2), record.field(3)) else { continue };
			if attrs & 0x3 == EP_ATTR_BULK {
				let Ok(max_packet) = record.field16(4) else { continue };
				if address & 0x80 != 0 && ep_in.is_none() {
					ep_in = Some(Bulk { address, max_packet });
				} else if address & 0x80 == 0 && ep_out.is_none() {
					ep_out = Some(Bulk { address, max_packet });
				}
			}
		}
	}
	Some(Interface { iface: iface?, ep_in: ep_in?, ep_out: ep_out? })
}

/// Where a bulk transfer's bytes are: the wrapper page at an offset, or the data buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
	Wrapper(u32),
	Data,
}

/// How a bulk transfer ended, as the controller saw it: every byte asked for or fewer (`Done`), a stall after
/// `moved` bytes, or a failure that is no stall - a timeout, a device gone, an error the bus reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
	Done(u32),
	Stall(u32),
	Failed,
}

/// WHAT A HOST CONTROLLER DOES FOR THE TRANSPORT, and all it does: one bulk transfer at a time on the interface's
/// endpoints, a halted endpoint recovered, the class reset sent, and the two buffers the bytes move through.
pub trait Transport {
	/// Move `len` bytes on the bulk IN endpoint (`dir_in`) or the OUT one, between the device and `at`. Bounded:
	/// a transfer that does not complete answers `Failed`.
	fn bulk(&mut self, dir_in: bool, at: At, len: u32) -> Moved;
	/// Unhalt one bulk endpoint after a stall: the controller's side, and the device's CLEAR_FEATURE(ENDPOINT_HALT)
	/// to the endpoint's address, which resets its data toggle.
	fn recover(&mut self, dir_in: bool);
	/// The Mass Storage Reset class request to the interface.
	fn class_reset(&mut self, iface: u16);
	/// The wrapper page the CBW and the CSW ride in - 64 bytes at least.
	fn wrapper(&mut self) -> &mut [u8];
	/// The data buffer, grown to hold `bytes` within what the class budget reserved, or None.
	fn data(&mut self, bytes: u64) -> Option<&mut [u8]>;
	/// The longest data stage one transfer carries.
	fn max_transfer(&self) -> u32;
}

/// A started unit: its interface (for the class reset), the rolling CBW tag, and its size in bytes from READ
/// CAPACITY - answered to OP_CAPACITY queries for the `lsblk` inventory.
pub struct Unit {
	pub iface: u16,
	pub tag: u32,
	pub capacity: u64,
}

impl Unit {
	pub fn new(iface: u16) -> Unit {
		Unit { iface, tag: 1, capacity: 0 }
	}
}

/// SPIN THE UNIT UP AND CHECK ITS GEOMETRY: a freshly attached unit reports a power-on unit attention on its first
/// command, cleared by reading the sense data - so TEST UNIT READY is tried four times with the sense read between
/// - then READ CAPACITY, whose block size must be the 512-byte sector the block protocol serves, and whose last
/// LBA (big-endian) is one less than the sector count. False when the unit never comes ready or is not one this
/// wire can serve.
pub fn start(t: &mut impl Transport, unit: &mut Unit) -> bool {
	let mut ready = false;
	for _ in 0..4 {
		if command(t, unit, &scsi::test_unit_ready(), 0, false) {
			ready = true;
			break;
		}
		read_sense(t, unit);
	}
	if !ready || !command(t, unit, &scsi::read_capacity10(), 8, true) {
		return false;
	}
	let Some(answer) = t.data(8) else { return false };
	let block_size = u32::from_be_bytes([answer[4], answer[5], answer[6], answer[7]]);
	let last_lba = u32::from_be_bytes([answer[0], answer[1], answer[2], answer[3]]);
	if block_size != SECTOR {
		return false;
	}
	unit.capacity = (last_lba as u64 + 1) * SECTOR as u64;
	true
}

/// Run one SCSI command, the whole data stage or nothing - which is what every caller but the sense read wants.
pub fn command(t: &mut impl Transport, unit: &mut Unit, cb: &[u8], data_len: u32, data_in: bool) -> bool {
	transfer(t, unit, cb, data_len, data_in, false).is_ok()
}

/// RUN ONE SCSI COMMAND OVER THE BULK-ONLY TRANSPORT, answering how many bytes actually moved: the CBW on the bulk
/// OUT endpoint, the data stage (into or out of the data buffer), and the CSW on the bulk IN endpoint, whose
/// signature, tag echo, residue and status decide the result.
///
/// `allow_short` is for the one caller that can use a short answer - the sense read asks for eighteen bytes and is
/// content with what it gets - and it is an argument rather than a second function because the rest of the
/// transaction is identical.
pub fn transfer(t: &mut impl Transport, unit: &mut Unit, cb: &[u8], data_len: u32, data_in: bool, allow_short: bool) -> Result<u32, BotFault> {
	let tag = unit.tag;
	unit.tag = unit.tag.wrapping_add(1);
	{
		let wrapper = t.wrapper();
		wrapper[..(CSW_OFF + CSW_LEN) as usize].fill(0);
		wrapper[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
		wrapper[4..8].copy_from_slice(&tag.to_le_bytes());
		wrapper[8..12].copy_from_slice(&data_len.to_le_bytes());
		wrapper[12] = if data_in { CBW_FLAG_IN } else { 0 };
		wrapper[13] = 0; // LUN 0
		wrapper[14] = cb.len() as u8;
		wrapper[15..15 + cb.len()].copy_from_slice(cb);
	}
	match t.bulk(false, At::Wrapper(0), CBW_LEN) {
		Moved::Done(_) => {}
		Moved::Stall(_) => {
			// the device rejected the CBW: unhalt the OUT endpoint and fail the command.
			t.recover(false);
			return Err(BotFault::Framing);
		}
		Moved::Failed => {
			reset(t, unit);
			return Err(BotFault::Framing);
		}
	}
	// THE DATA STAGE, on the direction's endpoint, out of the data buffer. A stall here is routine (the device
	// returned less than asked): unhalt the endpoint and continue to the CSW, which still reports the command's
	// status. AND HOW MANY BYTES THE TRANSPORT SAW MOVE is kept, because it is the account of the data stage that
	// does not come from the device: the CSW's residue is checked against it rather than believed.
	let mut observed: u32 = 0;
	if data_len > 0 {
		match t.bulk(data_in, At::Data, data_len) {
			Moved::Done(moved) => observed = moved,
			Moved::Stall(moved) => {
				observed = moved;
				t.recover(data_in);
			}
			Moved::Failed => {
				reset(t, unit);
				return Err(BotFault::Framing);
			}
		}
	}
	// THE CSW CLOSES THE TRANSACTION; a stalled status stage is unhalted and the read retried once (the Bulk-Only
	// recovery sequence), anything worse resets the transport. A SHORT WRAPPER ARRIVES SHORT, and how short is what
	// `csw_outcome` refuses it by.
	let mut received: Option<u32> = None;
	for _ in 0..2 {
		match t.bulk(true, At::Wrapper(CSW_OFF), CSW_LEN) {
			Moved::Done(moved) => {
				received = Some(moved);
				break;
			}
			Moved::Stall(_) => t.recover(true),
			Moved::Failed => break,
		}
	}
	let Some(received) = received else {
		reset(t, unit);
		return Err(BotFault::Framing);
	};
	// THE WRAPPER HAS FIVE FIELDS AND THREE WERE READ. The residue is the device's own account of how many of the
	// bytes it was asked for did not move, and without it a device that transferred half a sector and said "pass"
	// was a complete success.
	let wrapper = t.wrapper();
	let at = CSW_OFF as usize;
	let word = |offset: usize| u32::from_le_bytes([wrapper[at + offset], wrapper[at + offset + 1], wrapper[at + offset + 2], wrapper[at + offset + 3]]);
	let csw = Csw { signature: word(0), echoed_tag: word(4), residue: word(8), status: wrapper[at + 12], received };
	let outcome = csw_outcome(&csw, tag, data_len, observed, allow_short);
	// A malformed wrapper means the transport lost sync, so reset it; a device that simply refused the command has
	// not, and resetting there would throw away its sense data.
	if matches!(outcome, Err(BotFault::Framing) | Err(BotFault::Residue)) {
		reset(t, unit);
	}
	outcome
}

/// Read (and return) the unit's sense data, clearing the pending condition a failed command left behind so the
/// next command starts clean. A SENSE READ IS THE ONE TRANSFER A SHORT ANSWER IS AN ANSWER TO: a unit that returns
/// twelve bytes of fixed-format sense has told us what happened.
pub fn read_sense(t: &mut impl Transport, unit: &mut Unit) -> [u8; scsi::SENSE_LEN] {
	let mut out = [0u8; scsi::SENSE_LEN];
	let moved = transfer(t, unit, &scsi::request_sense(scsi::SENSE_LEN as u8), scsi::SENSE_LEN as u32, true, true).unwrap_or(0) as usize;
	if let Some(data) = t.data(scsi::SENSE_LEN as u64) {
		let take = moved.min(scsi::SENSE_LEN);
		out[..take].copy_from_slice(&data[..take]);
	}
	out
}

/// The last-resort transport recovery (the Bulk-Only specification's reset sequence): the Mass Storage Reset class
/// request returns the device's BOT state machine to idle, then both bulk endpoints are unhalted, so the next CBW
/// starts a clean transaction.
pub fn reset(t: &mut impl Transport, unit: &Unit) {
	t.class_reset(unit.iface);
	t.recover(true);
	t.recover(false);
}

/// SERVE ONE BLOCK REQUEST from the StorageService instance - `[op u32][lba u64][count u32]`, the canonical block
/// wire: a read replies `[status u32]` and a MemoryObject of the sectors, a write carries one in and replies
/// `[status u32]`, a capacity query replies the capacity and the longest request, and a flush is SYNCHRONIZE CACHE.
///
/// THE COUNT IS REFUSED AND NOT CLAMPED, and the range is checked against the medium. A clamp turns a wrong request
/// into a wrong WRITE: the caller believes its bytes landed where it said, and they landed somewhere smaller. And
/// the ten-byte command carries a THIRTY-TWO-BIT address, so a request past two terabytes is refused rather than
/// truncated into a block near the start of the medium. A request this server will not carry is `STATUS_INVALID`;
/// `STATUS_ERR` is kept for a device that failed a request it was right to be asked.
pub fn serve(t: &mut impl Transport, unit: &mut Unit, blk_server: u64, req: &[u8; 16], handle: u64) {
	let block::Request { op, lba, count } = block::Request::from_bytes(req);
	let max_sectors = (t.max_transfer() / SECTOR) as u64;
	let admitted = if matches!(op, OP_READ | OP_WRITE) {
		let capacity_sectors = unit.capacity / SECTOR as u64;
		blk::request_range(lba, count, capacity_sectors, max_sectors).and_then(|count| blk::command_lba32(lba, count).map(|_| count))
	} else {
		Ok(count)
	};
	let Ok(count) = admitted else {
		if handle != 0 {
			close(handle);
		}
		reply_block(blk_server, STATUS_INVALID, 0);
		return;
	};
	match op {
		OP_READ => serve_read(t, unit, blk_server, lba, count),
		OP_WRITE => serve_write(t, unit, blk_server, lba, count, handle),
		OP_CAPACITY => reply_capacity(blk_server, unit.capacity, max_sectors),
		OP_FLUSH => serve_flush(t, unit, blk_server),
		_ => {
			if handle != 0 {
				close(handle);
			}
			reply_block(blk_server, STATUS_ERR, 0);
		}
	}
}

// Read `count` sectors starting at `lba` with one SCSI READ(10) into a fresh shared buffer and hand it to the
// client, or reply with an error status. A failed command is retried once with the sense data read (and discarded)
// in between - a transient unit attention fails the first command and succeeds the retry.
fn serve_read(t: &mut impl Transport, unit: &mut Unit, blk_server: u64, lba: u64, count: u32) {
	let bytes: u64 = count as u64 * SECTOR as u64;
	if t.data(bytes).is_none() {
		reply_block(blk_server, STATUS_ERR, 0);
		return;
	}
	let cb = scsi::read_write10(false, lba as u32, count as u16);
	let mut ok = command(t, unit, &cb, bytes as u32, true);
	if !ok {
		read_sense(t, unit);
		ok = command(t, unit, &cb, bytes as u32, true);
	}
	if !ok {
		reply_block(blk_server, STATUS_ERR, 0);
		return;
	}
	let obj: u64 = unsafe { syscall(SYS_MEMORY_OBJECT_CREATE, bytes, 0, 0, 0) };
	if sys_is_err(obj) {
		reply_block(blk_server, STATUS_ERR, 0);
		return;
	}
	// SAFETY: the object was created just above, and its mapping is the only one this function uses.
	let Some(dst) = (unsafe { map_object(obj) }) else {
		close(obj);
		reply_block(blk_server, STATUS_ERR, 0);
		return;
	};
	let Some(data) = t.data(bytes) else {
		unmap_object(obj);
		close(obj);
		reply_block(blk_server, STATUS_ERR, 0);
		return;
	};
	// SAFETY: `dst` maps the `bytes`-long object just created, and `data` holds at least `bytes`.
	unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), dst as *mut u8, bytes as usize) };
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

// Write `count` sectors starting at `lba` from the transferred buffer with one SCSI WRITE(10), then reply with the
// status and no buffer. A failed command is retried once with the sense data read in between; the sense read reuses
// the data buffer, so the sectors are copied in again before the retry.
fn serve_write(t: &mut impl Transport, unit: &mut Unit, blk_server: u64, lba: u64, count: u32, src_handle: u64) {
	if src_handle == 0 {
		reply_block(blk_server, STATUS_INVALID, 0);
		return;
	}
	// THE THREE QUESTIONS BEFORE ANYTHING IS MAPPED, from one `object_info`: what the handle is, whether it can be
	// read through, and whether it is as long as the request says.
	let bytes: u64 = count as u64 * SECTOR as u64;
	match object_info(src_handle) {
		Some(info) if blk::write_source(&info, bytes).is_ok() => {}
		_ => {
			close(src_handle);
			reply_block(blk_server, STATUS_INVALID, 0);
			return;
		}
	}
	// SAFETY: `write_source` said the handle is a readable object as long as the request; it is unmapped below.
	let Some(src) = (unsafe { map_object(src_handle) }) else {
		close(src_handle);
		reply_block(blk_server, STATUS_ERR, 0);
		return;
	};
	if t.data(bytes).is_none() {
		unmap_object(src_handle);
		close(src_handle);
		reply_block(blk_server, STATUS_ERR, 0);
		return;
	}
	let cb = scsi::read_write10(true, lba as u32, count as u16);
	let mut ok = copy_in(t, src, bytes) && command(t, unit, &cb, bytes as u32, false);
	if !ok {
		read_sense(t, unit);
		ok = copy_in(t, src, bytes) && command(t, unit, &cb, bytes as u32, false);
	}
	unmap_object(src_handle);
	close(src_handle);
	reply_block(blk_server, if ok { STATUS_OK } else { STATUS_ERR }, 0);
}

// The sectors of a mapped write source, `bytes` long, copied into the data buffer.
fn copy_in(t: &mut impl Transport, src: u64, bytes: u64) -> bool {
	let Some(data) = t.data(bytes) else { return false };
	// SAFETY: `src` maps an object `write_source` said holds `bytes`, and the buffer holds as many.
	unsafe { core::ptr::copy_nonoverlapping(src as *const u8, data.as_mut_ptr(), bytes as usize) };
	true
}

// Flush the unit's write cache with one SCSI SYNCHRONIZE CACHE (10) - LBA and count zero mean the whole medium -
// then reply with the status. The write barrier LiberFS commits rely on. No data stage.
fn serve_flush(t: &mut impl Transport, unit: &mut Unit, blk_server: u64) {
	let cb = scsi::synchronize_cache10();
	let mut outcome = FlushOutcome::Committed;
	if !command(t, unit, &cb, 0, false) {
		// The first failure may be a pending condition the unit was carrying; the sense read clears it and the
		// command is tried once more.
		let _ = read_sense(t, unit);
		if !command(t, unit, &cb, 0, false) {
			// AND THE SECOND FAILURE IS DECIDED FROM THE SENSE DATA rather than assumed. A unit that does not
			// implement the (optional) command has no volatile cache and the barrier holds without it; a unit
			// that could not commit is a FAILED barrier, and reporting it as success is worse than having no
			// barrier at all.
			let sense = read_sense(t, unit);
			outcome = flush_outcome(false, &sense);
		}
	}
	let ok = matches!(outcome, FlushOutcome::Committed | FlushOutcome::NoVolatileCache);
	reply_block(blk_server, if ok { STATUS_OK } else { STATUS_ERR }, 0);
}

/// Send a block reply: `[status u32 LE]` carrying the handle `xfer` (0 = none).
pub fn reply_block(blk_server: u64, status: u32, xfer: u64) {
	send_blocking(blk_server, &block::reply(status), xfer);
}

// Send a capacity reply: [status u32 LE][capacity bytes u64 LE][max sectors u32 LE][class u32 LE], no handle - the
// same wire contract driver.virtio-blk serves.
fn reply_capacity(blk_server: u64, bytes: u64, max_sectors: u64) {
	let reply: [u8; block::CAPACITY_CLASS_LEN] = block::capacity_reply(bytes, max_sectors, block::DeviceClass::UsbMassStorage);
	send_blocking(blk_server, &reply, 0);
}

#[cfg(test)]
mod tests;
