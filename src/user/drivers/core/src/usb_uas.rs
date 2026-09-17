// The USB Attached SCSI side of driver.xhci: the four-pipe transport and the probe that decided it
// could be written.
//
// UAS CARRIES THE SAME SCSI COMMAND SET AS THE BULK-ONLY PATH over FOUR pipes instead of two - a
// command pipe, a status pipe and a data pipe each way - joined by a TAG rather than by strict
// order. `drivers::scsi` is unchanged and shared, `drivers::uas` is the information units and the
// tag rules, and this file is the controller work: the endpoints, their streams, and the waiting.
//
// AND THE STREAMS ARE WHY THIS TOOK A MEASUREMENT FIRST. The item that owns UAS says the cheap check
// is to attach the device and READ ITS PIPE USAGE AND ENDPOINT COMPANION DESCRIPTORS before writing
// a transfer path, because a device that demands bulk streams cannot be driven by a controller that
// has none. The probe below is that check, it stays in the tree, and its answer on this machine was
// that the three answering pipes each advertise sixteen streams - so the controller grew them.
//
// ONE COMMAND AT A TIME, UNDER ONE TAG, ON ONE STREAM. That is the slice the plan names, and it is
// what makes a single stream enough: UAS requires the stream id to be the task tag, so one
// outstanding command is one stream. The array is full size because the hardware fixes its length;
// the entries nothing selects are never read.

use rt::*;

use crate::usb_hid::Hids;
use crate::{CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, DESC_CONFIG, FEATURE_ENDPOINT_HALT, REQ_CLEAR_FEATURE, REQ_GET_DESCRIPTOR, REQ_SET_CONFIGURATION, RT_ENDPOINT, SPIN_BUDGET, STREAM_ID, TRB_CONFIGURE_ENDPOINT, TRB_EV_TRANSFER, TRB_IOC, TRB_NORMAL};
use crate::{Ring, Streams, UsbDevice, Xhci};
use crate::{command_and_wait, control_in_req, control_nodata, dma_page, handle_hid_event, max_primary_streams, r8, take_event, w32};
use driver_protocol::block;
use drivers::blk;
use drivers::common;
use drivers::descriptor;
use drivers::scsi;
use drivers::uas;

/// The UAS interface's identity: mass storage, SCSI command set, UAS protocol.
pub const CLASS_MASS_STORAGE: u8 = 0x08;
pub const SUBCLASS_SCSI: u8 = 0x06;
pub const PROTOCOL_UAS: u8 = 0x62;

/// The class-specific descriptor that says which of the four pipes an endpoint is.
const DT_PIPE_USAGE: u8 = 0x24;
/// And the SuperSpeed endpoint companion, whose `bmAttributes` carries the stream count.
const DT_SS_COMPANION: u8 = 0x30;

const PIPE_COMMAND: u8 = 1;
const PIPE_STATUS: u8 = 2;
const PIPE_DATA_IN: u8 = 3;
const PIPE_DATA_OUT: u8 = 4;

/// The one tag this driver issues. One command at a time means one tag, and the stream id is the
/// tag, so this is also the stream. Never zero - that value is reserved.
const TAG: u16 = STREAM_ID as u16;

/// Where the command and the status information units sit in the control page.
const COMMAND_OFF: u64 = 0;
const STATUS_OFF: u64 = 256;
/// The largest status information unit this driver reads back: the header plus its sense data.
const STATUS_LEN: u32 = 96;

/// THE BRING-UP'S WHOLE BUDGET, in the 100 Hz ticks READY is measured in. Every driver on this
/// machine binds inside 44 of the 200 the window allows, so a UAS target that answers nothing costs
/// this and the controller still reports in time to say so.
const BRINGUP_TICKS: u64 = 20;
/// And one request's budget once the driver is serving, where nothing is waiting on a bind window.
const REQUEST_TICKS: u64 = 200;

/// The block size this transport serves. A medium with another one is refused rather than served
/// with the wrong arithmetic.
const SECTOR: u32 = 512;

/// One bound UAS device.
pub struct Uas {
	dci_command: u32,
	dci_status: u32,
	dci_data_in: u32,
	dci_data_out: u32,
	/// The command pipe carries no streams - commands are serialised by the tag, and streams are
	/// what multiplex the ANSWERS.
	ring_command: Ring,
	status: Streams,
	data_in: Streams,
	data_out: Streams,
	control_handle: u64,
	control_virt: u64,
	control_phys: u64,
	data_handle: u64,
	data_virt: u64,
	data_phys: u64,
	data_bytes: u64,
	pub capacity: u64,
}

impl Uas {
	pub fn release(&mut self) {
		self.ring_command.release();
		self.status.release();
		self.data_in.release();
		self.data_out.release();
		for handle in [&mut self.control_handle, &mut self.data_handle] {
			if *handle != 0 {
				close(*handle);
				*handle = 0;
			}
		}
	}
}

/// What one configuration's UAS interface looks like.
struct Found {
	config_value: u8,
	interface: u8,
	alternate: u8,
	/// (endpoint address, max packet, stream count) per pipe.
	command: Option<(u8, u16, u32)>,
	status: Option<(u8, u16, u32)>,
	data_in: Option<(u8, u16, u32)>,
	data_out: Option<(u8, u16, u32)>,
}

impl Found {
	fn complete(&self) -> bool {
		self.command.is_some() && self.status.is_some() && self.data_in.is_some() && self.data_out.is_some()
	}
}

// Walk one configuration for a UAS interface and its four pipes.
//
// THE PIPE USAGE DESCRIPTOR IS WHAT NAMES THEM and the endpoint direction is not enough: a UAS
// interface has two IN endpoints, and which of them is the status pipe and which the data pipe is
// written in a class descriptor that FOLLOWS each endpoint.
fn walk(config: &[u8], config_value: u8) -> Option<Found> {
	let mut found = Found { config_value, interface: 0, alternate: 0, command: None, status: None, data_in: None, data_out: None };
	let mut in_uas = false;
	let mut endpoint: Option<(u8, u16)> = None;
	let mut streams: u32 = 0;
	for record in descriptor::Walk::new(config) {
		match record.kind {
			descriptor::DT_INTERFACE => {
				in_uas = record.field(5) == Ok(CLASS_MASS_STORAGE) && record.field(6) == Ok(SUBCLASS_SCSI) && record.field(7) == Ok(PROTOCOL_UAS);
				endpoint = None;
				streams = 0;
				if in_uas {
					found.interface = record.field(2).unwrap_or(0);
					found.alternate = record.field(3).unwrap_or(0);
				}
			}
			descriptor::DT_ENDPOINT if in_uas => {
				let (Ok(address), Ok(packet)) = (record.field(2), record.field16(4)) else { continue };
				endpoint = Some((address, packet));
				streams = 0;
			}
			DT_SS_COMPANION if in_uas => {
				// `bmAttributes` bits 4:0 are log2 of the number of streams this endpoint supports.
				// Zero means none, which is the high-speed shape.
				let Ok(attributes) = record.field(3) else { continue };
				let exponent = attributes & 0x1f;
				streams = if exponent == 0 { 0 } else { 1u32 << exponent };
			}
			DT_PIPE_USAGE if in_uas => {
				let Ok(usage) = record.field(2) else { continue };
				let Some((address, packet)) = endpoint else { continue };
				let pipe = Some((address, packet, streams));
				match usage {
					PIPE_COMMAND => found.command = pipe,
					PIPE_STATUS => found.status = pipe,
					PIPE_DATA_IN => found.data_in = pipe,
					PIPE_DATA_OUT => found.data_out = pipe,
					_ => {}
				}
			}
			_ => {}
		}
	}
	found.complete().then_some(found)
}

// The device-context index of an endpoint address.
fn dci(address: u8) -> u32 {
	(address & 0x0f) as u32 * 2 + if address & 0x80 != 0 { 1 } else { 0 }
}

/// Read one device's configurations and report whether a UAS interface is there and what its
/// endpoints demand. Answers true when one was found.
///
/// IT REPORTS AND BINDS NOTHING. The item that owns UAS asked for exactly this measurement before a
/// transfer path was written, and it stays in the tree afterwards: it costs one boot and it is how
/// the next machine's shape is read in one run rather than in one implementation.
pub unsafe fn probe(hc: &mut Xhci, dev: &mut UsbDevice) -> bool {
	unsafe {
		let Some(found) = read_configurations(hc, dev, &mut |config, value| walk(config, value)) else {
			return false;
		};
		let mut line: common::Bounded<96> = common::Bounded::new();
		line.push(b"driver.xhci: UAS interface ");
		line.decimal(found.interface as u64);
		line.push(b" alternate ");
		line.decimal(found.alternate as u64);
		line.push(b" in configuration ");
		line.decimal(found.config_value as u64);
		line.push(b"\n");
		print(line.as_bytes());
		for (name, pipe) in [(b"command".as_slice(), found.command), (b"status", found.status), (b"data-in", found.data_in), (b"data-out", found.data_out)] {
			let Some((address, packet, streams)) = pipe else { continue };
			let mut line: common::Bounded<96> = common::Bounded::new();
			line.push(b"driver.xhci:   pipe ");
			line.push(name);
			line.push(b" endpoint ");
			line.push(&common::hex2(address));
			line.push(b" packet ");
			line.decimal(packet as u64);
			line.push(b" streams ");
			line.decimal(streams as u64);
			line.push(b"\n");
			print(line.as_bytes());
		}
		true
	}
}

// Read every configuration this device declares and hand each one to `take`, answering the first
// non-empty result.
//
// EVERY CONFIGURATION AND NOT INDEX ZERO, which the CDC path learned the expensive way: a device may
// publish several, and the one a driver speaks is not always the first.
unsafe fn read_configurations<T>(hc: &mut Xhci, dev: &mut UsbDevice, take: &mut dyn FnMut(&[u8], u8) -> Option<T>) -> Option<T> {
	unsafe {
		let mut hids: Hids = Hids::new();
		let configurations = match control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, (descriptor::DT_DEVICE as u16) << 8, 0, 18) {
			Some(received) if received >= 18 => r8(dev.data_virt + 17).max(1),
			_ => 1,
		};
		for index in 0..configurations.min(8) {
			let Some(head) = control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, DESC_CONFIG << 8 | index as u16, 0, 9) else {
				continue;
			};
			let head_bytes = core::slice::from_raw_parts(dev.data_virt as *const u8, head.min(9) as usize);
			let Some(head_record) = descriptor::Walk::new(head_bytes).next() else { continue };
			if descriptor::check_type(descriptor::DT_CONFIG, head_record.kind).is_err() {
				continue;
			}
			let (Ok(total), Ok(value)) = (head_record.field16(2), head_record.field(5)) else { continue };
			let total = total.min(1024);
			let Some(received) = control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, DESC_CONFIG << 8 | index as u16, 0, total) else {
				continue;
			};
			let Ok(total) = descriptor::check_transfer(total, received) else { continue };
			let config_bytes = core::slice::from_raw_parts(dev.data_virt as *const u8, total as usize);
			if let Some(taken) = take(config_bytes, value) {
				return Some(taken);
			}
		}
		None
	}
}

/// Configure an addressed device as a UAS target, or answer None when it is not one - or when its
/// pipes demand something this controller cannot give them.
pub unsafe fn configure_uas(hc: &mut Xhci, dev: &mut UsbDevice) -> Option<Uas> {
	unsafe {
		let found = read_configurations(hc, dev, &mut |config, value| walk(config, value))?;
		let (command_addr, command_packet, _) = found.command?;
		let (status_addr, status_packet, status_streams) = found.status?;
		let (in_addr, in_packet, in_streams) = found.data_in?;
		let (out_addr, out_packet, out_streams) = found.data_out?;
		// THE THREE ANSWERING PIPES MUST AGREE ABOUT STREAMS, because one array size is configured
		// for each and a device that advertised different counts is describing a shape this driver
		// has not been written against.
		if status_streams != in_streams || in_streams != out_streams {
			print(b"driver.xhci: the UAS device's answering pipes disagree about streams - it is left unbound\n");
			return None;
		}
		// STREAM 1 HAS TO BE INSIDE THE ARRAY. A device advertising two streams has entries 0 and 1,
		// which is enough; one advertising none is the high-speed shape and is driven without them.
		if status_streams == 1 {
			print(b"driver.xhci: the UAS device advertises a single stream, which has no entry for the tag - it is left unbound\n");
			return None;
		}

		let mut hids: Hids = Hids::new();
		let ring_command = Ring::new()?;
		let mut status = streams_or_ring(status_streams)?;
		let mut data_in = streams_or_ring(in_streams)?;
		let mut data_out = streams_or_ring(out_streams)?;

		let dci_command = dci(command_addr);
		let dci_status = dci(status_addr);
		let dci_data_in = dci(in_addr);
		let dci_data_out = dci(out_addr);

		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		((dev.in_virt + 4) as *mut u32).write_volatile(1 | 1 << dci_command | 1 << dci_status | 1 << dci_data_in | 1 << dci_data_out);
		let entries: u32 = dci_command.max(dci_status).max(dci_data_in).max(dci_data_out);
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(entries << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);

		// The command pipe: an ordinary bulk OUT endpoint with one ring.
		write_endpoint(hc, dev, dci_command, command_packet as u32, 2, ring_command.phys | ring_command.cycle as u64, 0);
		// And the three answering pipes, each pointing at its stream context array.
		for (dci_value, packet, kind, stream) in [(dci_status, status_packet as u32, 6u32, &status), (dci_data_in, in_packet as u32, 6, &data_in), (dci_data_out, out_packet as u32, 2, &data_out)] {
			let (pointer, streams_count) = match stream {
				Endpoint::Streams(array) => (array.phys, status_streams),
				Endpoint::Plain(ring) => (ring.phys | ring.cycle as u64, 0),
			};
			write_endpoint(hc, dev, dci_value, packet, kind, pointer, streams_count);
		}
		if command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24).is_none() {
			// A CONTROLLER THAT WILL NOT CONFIGURE A STREAM ENDPOINT SAYS SO HERE, which is the one
			// failure that is about the CONTROLLER rather than the device: `HCCPARAMS1.MaxPSASize`
			// is what bounds the array this asked for.
			print(b"driver.xhci: the controller refused the UAS device's stream endpoints\n");
			return None;
		}
		control_nodata(hc, &mut hids, dev, 0x00, REQ_SET_CONFIGURATION, found.config_value as u16, 0)?;

		let (control_handle, control_virt, control_phys) = dma_page()?;
		let (data_handle, data_virt, data_phys) = dma_page()?;
		let mut device = Uas { dci_command, dci_status, dci_data_in, dci_data_out, ring_command, status: status.take_streams(), data_in: data_in.take_streams(), data_out: data_out.take_streams(), control_handle, control_virt, control_phys, data_handle, data_virt, data_phys, data_bytes: 4096, capacity: 0 };

		// A FRESHLY ATTACHED UNIT REFUSES ITS FIRST COMMAND with a power-on attention that clears by
		// being read, which is the same rule every other SCSI path here follows.
		//
		// ONE DEADLINE FOR THE WHOLE BRING-UP AND NOT ONE PER ATTEMPT. Sixteen attempts with a
		// budget each is sixteen budgets, and on a device that answers nothing that is a driver
		// that never reports at all - which is exactly what the first attempt at this did.
		let mut deadline = common::Deadline::ticks(BRINGUP_TICKS);
		let mut ready = false;
		let mut attempts: u32 = 0;
		while deadline.waiting() {
			attempts += 1;
			match command(hc, &mut hids, dev, &mut device, &scsi::test_unit_ready(), 0, false, &mut deadline) {
				Ok(_) => {
					ready = true;
					break;
				}
				Err(why) if why.retryable() => continue,
				Err(_) => break,
			}
		}
		if !ready {
			print(b"driver.xhci: the UAS target did not become ready inside the bring-up window\n");
			device.release();
			return None;
		}
		if command(hc, &mut hids, dev, &mut device, &scsi::read_capacity10(), 8, true, &mut deadline).is_err() {
			print(b"driver.xhci: the UAS target refused READ CAPACITY\n");
			device.release();
			return None;
		}
		let mut bytes = [0u8; 8];
		for (i, byte) in bytes.iter_mut().enumerate() {
			*byte = r8(device.data_virt + i as u64);
		}
		let Ok(capacity) = scsi::capacity(&bytes) else {
			device.release();
			return None;
		};
		if capacity.block_bytes != SECTOR {
			device.release();
			return None;
		}
		device.capacity = capacity.blocks * capacity.block_bytes as u64;
		// ONE LINE PER BOUND DEVICE, like every other class module here, and it carries the attempt
		// count because a unit that needed several is a unit that was clearing a power-on attention -
		// which is a different machine from one that answered immediately.
		let mut line: common::Bounded<96> = common::Bounded::new();
		line.push(b"driver.xhci: UAS target ready after ");
		line.decimal(attempts as u64);
		line.push(b" attempt(s), ");
		line.decimal(device.capacity);
		line.push(b" bytes over ");
		line.decimal(status_streams as u64);
		line.push(b" stream(s)\n");
		print(line.as_bytes());
		Some(device)
	}
}

// A pipe's transfer structure: a stream context array when the device demands streams, one ring when
// it does not.
enum Endpoint {
	Streams(Streams),
	Plain(Ring),
}

impl Endpoint {
	// Take the stream array out, building one over the plain ring when there is none. The rest of
	// this module drives one structure rather than two.
	fn take_streams(&mut self) -> Streams {
		match core::mem::replace(self, Endpoint::Plain(Ring { virt: 0, phys: 0, index: 0, cycle: 1, handle: 0 })) {
			Endpoint::Streams(array) => array,
			Endpoint::Plain(ring) => Streams { handle: 0, virt: 0, phys: 0, ring },
		}
	}
}

unsafe fn streams_or_ring(count: u32) -> Option<Endpoint> {
	unsafe { if count == 0 { Some(Endpoint::Plain(Ring::new()?)) } else { Some(Endpoint::Streams(Streams::new(count)?)) } }
}

// Write one endpoint context. `streams` of zero is an ordinary ring; anything else makes `pointer`
// the base of a linear stream context array.
unsafe fn write_endpoint(hc: &Xhci, dev: &UsbDevice, dci_value: u32, packet: u32, kind: u32, pointer: u64, streams: u32) {
	unsafe {
		let ep_ctx: u64 = dev.in_virt + (1 + dci_value as u64) * hc.ctx_size;
		let dword0: u32 = if streams == 0 {
			0
		} else {
			// LSA: the array is linear rather than a two-level tree, which is the only shape this
			// driver builds.
			1 << 15 | max_primary_streams(streams) << 10
		};
		(ep_ctx as *mut u32).write_volatile(dword0);
		((ep_ctx + 4) as *mut u32).write_volatile(packet << 16 | kind << 3 | 3 << 1);
		((ep_ctx + 8) as *mut u32).write_volatile(pointer as u32);
		((ep_ctx + 12) as *mut u32).write_volatile((pointer >> 32) as u32);
		((ep_ctx + 16) as *mut u32).write_volatile(packet);
	}
}

// Ring one endpoint's doorbell, naming the stream when it has one.
unsafe fn doorbell(hc: &Xhci, dev: &UsbDevice, dci_value: u32, stream: &Streams) {
	unsafe {
		let value = if stream.virt == 0 { dci_value } else { dci_value | STREAM_ID << 16 };
		w32(hc.db + dev.slot as u64 * 4, value);
	}
}

/// Run one SCSI command over the four pipes.
///
/// THE ANSWERS ARE POSTED BEFORE THE COMMAND IS SENT. A UAS device may answer as soon as it has the
/// command, and a transfer posted after it is a transfer the device was already waiting on - which
/// on a controller with one command outstanding is a stall rather than a race it survives.
#[allow(clippy::too_many_arguments)]
unsafe fn command(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas, cdb: &[u8], data_len: u32, data_in: bool, deadline: &mut common::Deadline) -> Result<u32, scsi::Sense> {
	unsafe {
		// The command information unit: the header, then the command block.
		// THE LUN FIELD IS A SCSI SINGLE-LEVEL LUN AND NOT VIRTIO'S ADDRESSING, which is eight bytes
		// of zero for logical unit zero. `scsi::virtio_lun` builds the other one - a REPORT LUNS
		// four-byte form with a leading 1 - and putting it here addresses a unit that is not there,
		// on a device that then answers nothing at all.
		let iu = uas::command_iu(TAG, &[0u8; 8], cdb.len() as u8);
		core::ptr::write_bytes(device.control_virt as *mut u8, 0, 512);
		for (i, byte) in iu.iter().enumerate() {
			((device.control_virt + COMMAND_OFF + i as u64) as *mut u8).write_volatile(*byte);
		}
		for (i, byte) in cdb.iter().enumerate() {
			((device.control_virt + COMMAND_OFF + uas::COMMAND_IU_HEADER as u64 + i as u64) as *mut u8).write_volatile(*byte);
		}
		// A SENTINEL THE DEVICE MUST OVERWRITE, for the reason every other transport here has one:
		// zero is a valid status, so a status buffer nobody wrote reads as success.
		((device.control_virt + STATUS_OFF) as *mut u8).write_volatile(0xFF);

		// Post the status answer first, then the data, then the command.
		device.status.ring.push(device.control_phys + STATUS_OFF, STATUS_LEN, TRB_NORMAL << 10 | TRB_IOC);
		doorbell(hc, dev, device.dci_status, &device.status);
		if data_len > 0 {
			if data_in {
				device.data_in.ring.push(device.data_phys, data_len, TRB_NORMAL << 10 | TRB_IOC);
				doorbell(hc, dev, device.dci_data_in, &device.data_in);
			} else {
				device.data_out.ring.push(device.data_phys, data_len, TRB_NORMAL << 10 | TRB_IOC);
				doorbell(hc, dev, device.dci_data_out, &device.data_out);
			}
		}
		device.ring_command.push(device.control_phys + COMMAND_OFF, (uas::COMMAND_IU_HEADER + cdb.len()) as u32, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, device.dci_command);

		// Wait for the status, servicing everything else that arrives.
		let data_dci = if data_len == 0 {
			None
		} else if data_in {
			Some(device.dci_data_in)
		} else {
			Some(device.dci_data_out)
		};
		let Some(outcome) = wait_for_status(hc, hids, dev.slot, device.dci_status, data_dci, deadline) else {
			print(b"driver.xhci: the UAS status pipe answered nothing inside the budget\n");
			recover(hc, hids, dev, device);
			return Err(scsi::Sense::Failed);
		};
		if outcome.status_code == CC_STALL {
			recover(hc, hids, dev, device);
			return Err(scsi::Sense::Failed);
		}
		if outcome.status_code != CC_SUCCESS && outcome.status_code != CC_SHORT_PACKET {
			let mut line: common::Bounded<96> = common::Bounded::new();
			line.push(b"driver.xhci: the UAS status pipe completed with code ");
			line.decimal(outcome.status_code as u64);
			line.push(b"\n");
			print(line.as_bytes());
			recover(hc, hids, dev, device);
			return Err(scsi::Sense::Failed);
		}

		// THE STATUS IS READ AS AN INFORMATION UNIT AND ITS TAG IS CHECKED, because an answer that
		// belongs to another command is not this command's status.
		let mut status = [0u8; 32];
		for (i, byte) in status.iter_mut().enumerate() {
			*byte = r8(device.control_virt + STATUS_OFF + i as u64);
		}
		match uas::answer(&status) {
			uas::Answer::Sense { tag, status, sense_len } => {
				if uas::matches(tag, TAG).is_err() {
					print(b"driver.xhci: a UAS answer carried a tag this driver did not issue\n");
					return Err(scsi::Sense::Failed);
				}
				if status == 0 {
					let moved = outcome.moved.unwrap_or(data_len);
					return Ok(moved);
				}
				// The sense data follows the unit's header.
				let mut sense = [0u8; scsi::SENSE_LEN];
				let take = (sense_len as usize).min(scsi::SENSE_LEN);
				for (i, byte) in sense.iter_mut().enumerate().take(take) {
					*byte = r8(device.control_virt + STATUS_OFF + uas::SENSE_IU_HEADER as u64 + i as u64);
				}
				Err(scsi::sense(&sense))
			}
			_ => {
				print(b"driver.xhci: a UAS status pipe answered with something that is not a sense unit\n");
				Err(scsi::Sense::Failed)
			}
		}
	}
}

// What waiting for a status found.
struct Outcome {
	status_code: u32,
	// How many data bytes the controller reported, when a data transfer was posted.
	moved: Option<u32>,
}

// Wait for the status pipe's completion, collecting the data pipe's on the way.
//
// THE TWO COMPLETIONS ARRIVE IN EITHER ORDER, which is the whole point of a tagged transport: the
// device answers when it is ready rather than in the order the pipes were posted. A wait that looked
// only for the status would discard the data completion, and with it the only account the controller
// keeps of how many bytes actually moved.
fn wait_for_status(hc: &mut Xhci, hids: &mut Hids, slot: u32, status_dci: u32, data_dci: Option<u32>, deadline: &mut common::Deadline) -> Option<Outcome> {
	unsafe {
		let mut moved: Option<u32> = None;
		let mut spins: u32 = 0;
		loop {
			if let Some((_p, status, control)) = take_event(hc) {
				let kind: u32 = control >> 10 & 0x3f;
				let this_slot: u32 = control >> 24;
				let this_dci: u32 = control >> 16 & 0x1f;
				if kind == TRB_EV_TRANSFER && this_slot == slot {
					if this_dci == status_dci {
						return Some(Outcome { status_code: status >> 24, moved });
					}
					if Some(this_dci) == data_dci {
						// The residual is what was NOT transferred, and the caller asked for a
						// length, so the difference is what moved.
						moved = Some(status & 0x00ff_ffff);
						continue;
					}
				}
				// A completion for the network adapter's receive endpoint is KEPT rather than
				// discarded - see `Xhci::net_pending`.
				if kind == TRB_EV_TRANSFER && hc.net_rx == Some((this_slot, this_dci)) && hc.net_pending.is_none() {
					hc.net_pending = Some((status, control));
					continue;
				}
				handle_hid_event(hc, hids, status, control);
				continue;
			}
			// BOUNDED IN TICKS AND NOT IN SPINS, which is the lesson this tree already paid for
			// once with the audio driver: a spin is a different amount of time on every machine and
			// on every architecture, and a bring-up that outlasts its bind window is a driver
			// DeviceManager kills for silence before it can say what it was waiting for. A device
			// that answers nothing costs this budget and no more.
			spins += 1;
			if !deadline.waiting() || spins > SPIN_BUDGET {
				return None;
			}
			if spins % 4096 == 0 {
				yield_now();
			}
		}
	}
}

// Unhalt every pipe after a failure. A UAS device that stalled one of them has the others still
// posted, so the recovery is per endpoint rather than a transport reset.
unsafe fn recover(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas) {
	{
		for (dci_value, incoming) in [(device.dci_status, true), (device.dci_data_in, true), (device.dci_data_out, false), (device.dci_command, false)] {
			let address: u16 = if incoming { 0x80 | (dci_value >> 1) as u16 } else { (dci_value >> 1) as u16 };
			let _ = control_nodata(hc, hids, dev, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, address);
		}
	}
}

/// Read or write blocks. Answers false when the transport or the target refused.
#[allow(clippy::too_many_arguments)]
pub unsafe fn transfer(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas, write: bool, lba: u32, count: u16) -> bool {
	unsafe {
		let bytes = count as u32 * SECTOR;
		if bytes as u64 > device.data_bytes {
			return false;
		}
		let cdb = scsi::read_write10(write, lba, count);
		let mut deadline = common::Deadline::ticks(REQUEST_TICKS);
		command(hc, hids, dev, device, &cdb, bytes, !write, &mut deadline).is_ok()
	}
}

/// Commit the medium's cache.
pub unsafe fn flush(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas) -> bool {
	unsafe {
		let mut deadline = common::Deadline::ticks(REQUEST_TICKS);
		command(hc, hids, dev, device, &scsi::synchronize_cache10(), 0, false, &mut deadline).is_ok()
	}
}

/// Serve one block request against the UAS target.
///
/// THE SAME WIRE AND THE SAME REFUSALS AS EVERY OTHER BLOCK SERVER HERE. `driver_protocol::block` is
/// the request and `drivers::blk` decides what is out of range, so this transport cannot answer a
/// bad request differently from the five that already exist - which is the thing extracting that
/// wire was for.
pub fn serve_block_request(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas, server: u64, req: &[u8; 16], handle: u64) {
	unsafe {
		let block::Request { op, lba, count } = block::Request::from_bytes(req);
		let sectors = device.capacity / SECTOR as u64;
		// THE MOST ONE REQUEST MOVES is what the data page holds, and it is refused rather than
		// clamped: a clamp turns a wrong request into a wrong WRITE.
		let most = device.data_bytes / SECTOR as u64;
		let admitted = if matches!(op, block::OP_READ | block::OP_WRITE) { blk::request_range(lba, count, sectors, most).and_then(|count| blk::command_lba32(lba, count).map(|_| count)) } else { Ok(count) };
		let Ok(count) = admitted else {
			if handle != 0 {
				close(handle);
			}
			reply(server, block::STATUS_INVALID, 0);
			return;
		};
		match op {
			block::OP_READ => {
				let bytes = count as u64 * SECTOR as u64;
				if !transfer(hc, hids, dev, device, false, lba as u32, count as u16) {
					reply(server, block::STATUS_ERR, 0);
					return;
				}
				grant(server, device.data_virt, bytes);
			}
			block::OP_WRITE => {
				let bytes = count as u64 * SECTOR as u64;
				if handle == 0 {
					reply(server, block::STATUS_INVALID, 0);
					return;
				}
				let Some(info) = object_info(handle) else {
					close(handle);
					reply(server, block::STATUS_INVALID, 0);
					return;
				};
				if blk::write_source(&info, bytes).is_err() {
					close(handle);
					reply(server, block::STATUS_INVALID, 0);
					return;
				}
				let Some(mapped) = map_object(handle) else {
					close(handle);
					reply(server, block::STATUS_ERR, 0);
					return;
				};
				core::ptr::copy_nonoverlapping(mapped as *const u8, device.data_virt as *mut u8, bytes as usize);
				unmap_object(handle);
				close(handle);
				let ok = transfer(hc, hids, dev, device, true, lba as u32, count as u16);
				reply(server, if ok { block::STATUS_OK } else { block::STATUS_ERR }, 0);
			}
			block::OP_CAPACITY => {
				send_blocking(server, &block::capacity_reply(device.capacity, most), 0);
			}
			block::OP_FLUSH => {
				let ok = flush(hc, hids, dev, device);
				reply(server, if ok { block::STATUS_OK } else { block::STATUS_ERR }, 0);
			}
			_ => {
				if handle != 0 {
					close(handle);
				}
				reply(server, block::STATUS_ERR, 0);
			}
		}
	}
}

fn reply(server: u64, status: u32, transferred: u64) {
	send_blocking(server, &block::reply(status), transferred);
}

// Hand the caller a copy of what was read. The DMA page stays this driver's.
unsafe fn grant(server: u64, from: u64, bytes: u64) {
	unsafe {
		let object = syscall(SYS_MEMORY_OBJECT_CREATE, bytes, 0, 0, 0);
		if sys_is_err(object) {
			reply(server, block::STATUS_ERR, 0);
			return;
		}
		let Some(mapped) = map_object(object) else {
			close(object);
			reply(server, block::STATUS_ERR, 0);
			return;
		};
		core::ptr::copy_nonoverlapping(from as *const u8, mapped as *mut u8, bytes as usize);
		unmap_object(object);
		let granted = duplicate(object, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(object);
		if granted < 0 {
			reply(server, block::STATUS_ERR, 0);
			return;
		}
		reply(server, block::STATUS_OK, granted as u64);
	}
}
