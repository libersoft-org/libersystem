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
use crate::{DRIVEN_STREAMS, Ring, Streams, UsbDevice, Xhci};
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

/// THE TAG IS THE STREAM, so these two constants are one fact written twice and must stay together.
/// Never zero - that value is reserved by both the stream mechanism and the UAS tag rules.
///
/// Commands run under the first tag. Task management runs under the SECOND, because its header
/// carries a tag of its own while the managed command's tag sits in a different field - so a device
/// answering it answers on the second tag's stream, and a driver with one stream is not listening
/// there. That was measured against the live device before this second stream existed: it said
/// nothing at all.
/// How many commands may be outstanding at once. The item says to CAP outstanding commands, and
/// this is the cap: each one needs a stream, a data page and a pair of buffers, so the number is a
/// resource decision and not a guess.
const COMMAND_TAGS: u32 = 2;
/// Command tags are streams 1..=COMMAND_TAGS; task management takes the one after them.
const MANAGEMENT_STREAM: u32 = STREAM_ID + COMMAND_TAGS;
const MANAGEMENT_TAG: u16 = MANAGEMENT_STREAM as u16;

/// The stream a command tag index names, and the tag that goes in the information unit. They are
/// the same number because in this transport the tag IS the stream.
const fn command_stream(index: usize) -> u32 {
	STREAM_ID + index as u32
}
const fn command_tag(index: usize) -> u16 {
	command_stream(index) as u16
}

/// THE CONTROL PAGE IS DIVIDED PER TAG, for the same reason the data pages are: every outstanding
/// tag has a command unit the device is reading and a status unit the device will write, and two
/// tags sharing either one is two commands writing over each other.
const COMMAND_OFF: u64 = 0;
const COMMAND_STRIDE: u64 = 128;
const STATUS_OFF: u64 = 512;
const STATUS_STRIDE: u64 = 128;
/// The largest status information unit this driver reads back: the header plus its sense data.
const STATUS_LEN: u32 = 96;
/// AND THE TASK-MANAGEMENT PAIR HAS ITS OWN TWO REGIONS, because the whole point of its tag is that
/// it is outstanding AT THE SAME TIME as a command: sharing a command's buffers would have the
/// request overwrite the very bytes the command it is aborting is waiting on.
const MANAGEMENT_OFF: u64 = 1024;
const RESPONSE_OFF: u64 = 1152;
/// A response information unit is eight bytes; this reads a little more so a device that answers a
/// longer one is not read past the end of what it wrote.
const RESPONSE_LEN: u32 = 32;

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
	/// ONE DATA BUFFER PER COMMAND TAG, because that is what "outstanding" means. Two commands in
	/// flight sharing one page would have the second overwrite what the first is still moving, and
	/// the transport would be tagged while the memory under it was not.
	tags: [TagBuffer; COMMAND_TAGS as usize],
	/// What each tag is currently carrying, so a completion can be finished by the loop that drains
	/// events rather than by a caller standing over it.
	pending: [Pending; COMMAND_TAGS as usize],
	data_bytes: u64,
	pub capacity: u64,
}

/// One command tag's data page.
struct TagBuffer {
	handle: u64,
	virt: u64,
	phys: u64,
}

/// A request issued under one tag and not yet answered.
///
/// THE CONSUMER'S CHANNEL IS PART OF IT, and that is the whole reason concurrency is safe here.
/// `driver_protocol::block` carries NO correlation id, so two replies on ONE channel must arrive in
/// the order the requests were made - and a tagged transport answers in whatever order the device
/// finishes. So tags are handed to requests from DIFFERENT consumers, each of which has at most one
/// outstanding, and each reply goes back on the channel its request came from. Out-of-order
/// completion is then invisible to every consumer, because no consumer sees more than one of them.
struct Pending {
	server: u64,
	op: u32,
	bytes: u64,
	/// Set once the data pipe's completion has been seen, so the status can be answered with what
	/// actually moved rather than with what was asked for.
	moved: Option<u32>,
	data_dci: Option<u32>,
	/// When this request stops being worth waiting for, in the clock `common::Deadline` reads.
	deadline_at: u64,
}

impl Pending {
	const IDLE: Pending = Pending { server: 0, op: 0, bytes: 0, moved: None, data_dci: None, deadline_at: 0 };

	fn active(&self) -> bool {
		self.server != 0
	}
}

impl Uas {
	// The first command tag with nothing outstanding on it, or none when every tag is in flight.
	//
	// THE CAP IS THIS ARRAY'S LENGTH, which is what the item means by capping outstanding commands:
	// a request arriving with every tag busy is not queued and not refused, it is run the old way -
	// synchronously on the tag that is about to free itself - so a consumer never waits on a queue
	// this driver would then have to bound, age and drain.
	fn free_tag(&self) -> Option<usize> {
		self.pending.iter().position(|slot| !slot.active())
	}

	pub fn release(&mut self) {
		self.ring_command.release();
		self.status.release();
		self.data_in.release();
		self.data_out.release();
		if self.control_handle != 0 {
			close(self.control_handle);
			self.control_handle = 0;
		}
		for tag in self.tags.iter_mut() {
			if tag.handle != 0 {
				close(tag.handle);
				tag.handle = 0;
				tag.virt = 0;
				tag.phys = 0;
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
		// EVERY TAG THIS DRIVER ISSUES HAS TO HAVE AN ENTRY. Stream ids start at one - zero means "no
		// stream" - so driving N tags needs N+1 entries, and this driver issues two: the command tag
		// and the task-management tag. A device advertising fewer is describing a transport with fewer
		// tags than this one drives, and binding it would write stream contexts past the array.
		//
		// ZERO IS A DIFFERENT ANSWER AND NOT A SMALL ONE: it is the high-speed shape, which has no
		// streams at all and is driven over a plain ring.
		if status_streams != 0 && status_streams <= DRIVEN_STREAMS {
			print(b"driver.xhci: the UAS device advertises fewer streams than this driver's tags need - it is left unbound\n");
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
		// ONE DATA PAGE PER COMMAND TAG, taken up front: a transport that allocated them on demand
		// would fail to start a command for want of memory in the middle of serving, which is the one
		// moment there is nothing useful to do about it.
		let mut tags: [TagBuffer; COMMAND_TAGS as usize] = [const { TagBuffer { handle: 0, virt: 0, phys: 0 } }; COMMAND_TAGS as usize];
		for tag in tags.iter_mut() {
			let Some((handle, virt, phys)) = dma_page() else {
				for built in tags.iter_mut() {
					if built.handle != 0 {
						close(built.handle);
					}
				}
				close(control_handle);
				return None;
			};
			*tag = TagBuffer { handle, virt, phys };
		}
		let mut device = Uas { dci_command, dci_status, dci_data_in, dci_data_out, ring_command, status: status.take_streams(), data_in: data_in.take_streams(), data_out: data_out.take_streams(), control_handle, control_virt, control_phys, tags, pending: [const { Pending::IDLE }; COMMAND_TAGS as usize], data_bytes: 4096, capacity: 0 };

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
			match command(hc, &mut hids, dev, &mut device, 0, &scsi::test_unit_ready(), 0, false, &mut deadline) {
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
		if command(hc, &mut hids, dev, &mut device, 0, &scsi::read_capacity10(), 8, true, &mut deadline).is_err() {
			print(b"driver.xhci: the UAS target refused READ CAPACITY\n");
			device.release();
			return None;
		}
		let mut bytes = [0u8; 8];
		for (i, byte) in bytes.iter_mut().enumerate() {
			*byte = r8(device.tags[0].virt + i as u64);
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
		// THE MANAGEMENT PATH IS EXERCISED ONCE AT BRING-UP, ON EVERY BOOT, and the answer is in the
		// online line. An ABORT TASK naming a tag with nothing outstanding is the one management
		// request that is safe to send to a healthy device - SAM has it answer "function complete"
		// rather than act - so it proves the second stream carries answers without provoking the fault
		// it exists to clean up after. The alternative is a path that is only ever taken when something
		// has already gone wrong, which is the path nobody finds out is broken until then.
		let mut probe = common::Deadline::ticks(BRINGUP_TICKS);
		let management = abort_task(hc, &mut hids, dev, &mut device, command_tag(0), &mut probe);

		// ONE LINE PER BOUND DEVICE, like every other class module here, and it carries the attempt
		// count because a unit that needed several is a unit that was clearing a power-on attention -
		// which is a different machine from one that answered immediately.
		// THE BOUND IS ABOVE THIS LINE AND NOT BELOW IT. The truncation is deliberate and stays - a
		// driver that panicked writing a report is a device that never came up - but a report cut
		// mid-word tells an operator less than no report at all, which is what 96 did to it once the
		// management verdict joined the line.
		let mut line: common::Bounded<160> = common::Bounded::new();
		line.push(b"driver.xhci: UAS target ready after ");
		line.decimal(attempts as u64);
		line.push(b" attempt(s), ");
		line.decimal(device.capacity);
		line.push(b" bytes over ");
		line.decimal(status_streams as u64);
		line.push(b" stream(s), task management ");
		line.push(if management { b"answered" } else { b"UNANSWERED" });
		line.push(b"\n");
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
			Endpoint::Plain(ring) => {
				let mut rings: [Ring; DRIVEN_STREAMS as usize] = [const { Ring { virt: 0, phys: 0, index: 0, cycle: 1, handle: 0 } }; DRIVEN_STREAMS as usize];
				rings[0] = ring;
				Streams { handle: 0, virt: 0, phys: 0, rings }
			}
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
unsafe fn doorbell(hc: &Xhci, dev: &UsbDevice, dci_value: u32, stream: &Streams, id: u32) {
	unsafe {
		// A PIPE WITHOUT STREAMS TAKES NO ID, and writing one into the doorbell of an ordinary
		// endpoint is a reserved field the controller is entitled to refuse.
		let value = if stream.virt == 0 { dci_value } else { dci_value | id << 16 };
		w32(hc.db + dev.slot as u64 * 4, value);
	}
}

/// Run one SCSI command over the four pipes.
///
/// THE ANSWERS ARE POSTED BEFORE THE COMMAND IS SENT. A UAS device may answer as soon as it has the
/// command, and a transfer posted after it is a transfer the device was already waiting on - which
/// on a controller with one command outstanding is a stall rather than a race it survives.
/// POST ONE COMMAND'S THREE TRANSFERS AND RING THE DOORBELLS, without waiting for any of them.
///
/// Both the synchronous path and the concurrent one go through this, because "a command is
/// outstanding" has to mean the same arrangement of rings either way - a second posting routine
/// would be a second definition of what a tag is.
unsafe fn post(hc: &mut Xhci, dev: &UsbDevice, device: &mut Uas, slot: usize, cdb: &[u8], data_len: u32, data_in: bool) -> bool {
	unsafe {
		// The command information unit: the header, then the command block.
		// THE LUN FIELD IS A SCSI SINGLE-LEVEL LUN AND NOT VIRTIO'S ADDRESSING, which is eight bytes
		// of zero for logical unit zero. `scsi::virtio_lun` builds the other one - a REPORT LUNS
		// four-byte form with a leading 1 - and putting it here addresses a unit that is not there,
		// on a device that then answers nothing at all.
		let tag = command_tag(slot);
		let stream = command_stream(slot);
		let command_at = COMMAND_OFF + slot as u64 * COMMAND_STRIDE;
		let status_at = STATUS_OFF + slot as u64 * STATUS_STRIDE;
		let iu = uas::command_iu(tag, &[0u8; 8], cdb.len() as u8);
		// ONLY THIS TAG'S REGION IS CLEARED. Zeroing the whole page would wipe the command another
		// tag is still executing out from under the device.
		core::ptr::write_bytes((device.control_virt + command_at) as *mut u8, 0, COMMAND_STRIDE as usize);
		for (i, byte) in iu.iter().enumerate() {
			((device.control_virt + command_at + i as u64) as *mut u8).write_volatile(*byte);
		}
		for (i, byte) in cdb.iter().enumerate() {
			((device.control_virt + command_at + uas::COMMAND_IU_HEADER as u64 + i as u64) as *mut u8).write_volatile(*byte);
		}
		// A SENTINEL THE DEVICE MUST OVERWRITE, for the reason every other transport here has one:
		// zero is a valid status, so a status buffer nobody wrote reads as success.
		((device.control_virt + status_at) as *mut u8).write_volatile(0xFF);

		// Post the status answer first, then the data, then the command - each on THIS TAG'S STREAM,
		// because the device answers a tag on the stream the tag names.
		let Some(ring) = device.status.ring(stream) else { return false };
		ring.push(device.control_phys + status_at, STATUS_LEN, TRB_NORMAL << 10 | TRB_IOC);
		doorbell(hc, dev, device.dci_status, &device.status, stream);
		if data_len > 0 {
			if data_in {
				let Some(ring) = device.data_in.ring(stream) else { return false };
				ring.push(device.tags[slot].phys, data_len, TRB_NORMAL << 10 | TRB_IOC);
				doorbell(hc, dev, device.dci_data_in, &device.data_in, stream);
			} else {
				let Some(ring) = device.data_out.ring(stream) else { return false };
				ring.push(device.tags[slot].phys, data_len, TRB_NORMAL << 10 | TRB_IOC);
				doorbell(hc, dev, device.dci_data_out, &device.data_out, stream);
			}
		}
		device.ring_command.push(device.control_phys + command_at, (uas::COMMAND_IU_HEADER + cdb.len()) as u32, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, device.dci_command);
		true
	}
}

/// Run one SCSI command over the four pipes and wait for it.
#[allow(clippy::too_many_arguments)]
unsafe fn command(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas, slot: usize, cdb: &[u8], data_len: u32, data_in: bool, deadline: &mut common::Deadline) -> Result<u32, scsi::Sense> {
	unsafe {
		let tag = command_tag(slot);
		let stream = command_stream(slot);
		let status_at = STATUS_OFF + slot as u64 * STATUS_STRIDE;
		if !post(hc, dev, device, slot, cdb, data_len, data_in) {
			return Err(scsi::Sense::Failed);
		}

		// Wait for the status, servicing everything else that arrives.
		let data_dci = if data_len == 0 {
			None
		} else if data_in {
			Some(device.dci_data_in)
		} else {
			Some(device.dci_data_out)
		};
		let Some(outcome) = wait_for_status(hc, hids, dev.slot, device, stream, data_dci, deadline) else {
			print(b"driver.xhci: the UAS status pipe answered nothing inside the budget\n");
			// THE DEVICE IS TOLD, AND NOT ONLY THE CONTROLLER. Clearing an endpoint halt makes the
			// CONTROLLER forget a transfer while the DEVICE goes on executing the command - and a
			// device still executing a read will eventually write into a buffer this driver has
			// handed to somebody else. ABORT TASK is what says so to the device, and it is possible
			// here because the request carries a tag of its own on a stream of its own.
			//
			// A FRESH BUDGET, because the one that just expired cannot wait for anything. It is the
			// bring-up's rather than a request's: a device that has already missed one deadline is
			// not owed a second long one, and the caller is a block request somebody is waiting on.
			let mut cleanup = common::Deadline::ticks(BRINGUP_TICKS);
			if !abort_task(hc, hids, dev, device, tag, &mut cleanup) {
				// AND THE ESCALATION IS THE UNIT RESET, which is why `uas::task_done` is two codes and
				// not one: a driver reading a successful abort as a failure would reset a unit whose
				// other commands were fine.
				let mut escalation = common::Deadline::ticks(BRINGUP_TICKS);
				if !reset_unit(hc, hids, dev, device, &mut escalation) {
					print(b"driver.xhci: the UAS target refused both an abort and a unit reset\n");
				}
			}
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
			*byte = r8(device.control_virt + status_at + i as u64);
		}
		match uas::answer(&status) {
			uas::Answer::Sense { tag: answered, status, sense_len } => {
				if uas::matches(answered, tag).is_err() {
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
					*byte = r8(device.control_virt + status_at + uas::SENSE_IU_HEADER as u64 + i as u64);
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
fn wait_for_status(hc: &mut Xhci, hids: &mut Hids, slot: u32, device: &mut Uas, stream: u32, data_dci: Option<u32>, deadline: &mut common::Deadline) -> Option<Outcome> {
	unsafe {
		let status_dci = device.dci_status;
		let mut moved: Option<u32> = None;
		let mut spins: u32 = 0;
		loop {
			if let Some((pointer, status, control)) = take_event(hc) {
				let kind: u32 = control >> 10 & 0x3f;
				let this_slot: u32 = control >> 24;
				let this_dci: u32 = control >> 16 & 0x1f;
				if kind == TRB_EV_TRANSFER && this_slot == slot {
					// THE STREAM IS CHECKED AND NOT JUST THE ENDPOINT, which is the whole reason a tagged
					// transport exists. A task-management answer arrives on the SAME endpoint under a
					// different tag, and a wait that stopped at the endpoint would take it for this
					// command's status - reading a response unit as a sense unit and answering a block
					// request from it.
					if this_dci == status_dci && device.status.stream_of(pointer) == Some(stream) {
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

/// Ask the device to abort the command outstanding under the command tag.
///
/// WHY THIS CAN EXIST NOW AND COULD NOT BEFORE. In this transport the tag IS the stream, and a
/// task-management request carries a tag of ITS OWN - the managed command's tag travels in a
/// separate field. So the device answers the request on the request tag's stream. With one stream
/// built, that answer landed where nothing was posted and the live device read as silent; this was
/// measured, not predicted, and the second stream is what removes it.
///
/// AND THE REQUEST IS POSTED ON THE COMMAND PIPE, which carries no streams: commands and management
/// requests are serialised by the tag inside the information unit, and only the ANSWERING pipes are
/// tagged. Ringing the command doorbell with a stream id would be writing a reserved field.
pub unsafe fn abort_task(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas, managed: u16, deadline: &mut common::Deadline) -> bool {
	unsafe { task_management(hc, hids, dev, device, uas::TMF_ABORT_TASK, managed, deadline) }
}

/// Reset the logical unit, which is the escalation when an abort is refused.
pub unsafe fn reset_unit(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas, deadline: &mut common::Deadline) -> bool {
	// A UNIT RESET NAMES NO COMMAND: the managed-tag field is defined only for the functions that
	// address one, and a driver writing a tag there is asking for something the specification does
	// not describe.
	unsafe { task_management(hc, hids, dev, device, uas::TMF_LOGICAL_UNIT_RESET, 0, deadline) }
}

unsafe fn task_management(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas, function: u8, managed: u16, deadline: &mut common::Deadline) -> bool {
	unsafe {
		let iu = uas::task_management_iu(MANAGEMENT_TAG, function, managed, &[0u8; 8]);
		for (i, byte) in iu.iter().enumerate() {
			((device.control_virt + MANAGEMENT_OFF + i as u64) as *mut u8).write_volatile(*byte);
		}
		// THE SAME SENTINEL RULE AS THE STATUS BUFFER, and here it matters more: `RESPONSE_COMPLETE`
		// is zero, so a response buffer nobody wrote reads as a successful abort.
		for i in 0..RESPONSE_LEN as u64 {
			((device.control_virt + RESPONSE_OFF + i) as *mut u8).write_volatile(0xFF);
		}

		// Post the answer on the management tag's stream, then send the request.
		let Some(ring) = device.status.ring(MANAGEMENT_STREAM) else { return false };
		ring.push(device.control_phys + RESPONSE_OFF, RESPONSE_LEN, TRB_NORMAL << 10 | TRB_IOC);
		doorbell(hc, dev, device.dci_status, &device.status, MANAGEMENT_STREAM);
		device.ring_command.push(device.control_phys + MANAGEMENT_OFF, uas::TASK_MANAGEMENT_IU_LEN as u32, TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, device.dci_command);

		let Some(code) = wait_for_stream(hc, hids, dev.slot, device, MANAGEMENT_STREAM, deadline) else {
			print(b"driver.xhci: the UAS device did not answer a task-management request\n");
			return false;
		};
		if code != CC_SUCCESS && code != CC_SHORT_PACKET {
			return false;
		}
		let mut response = [0u8; 16];
		for (i, byte) in response.iter_mut().enumerate() {
			*byte = r8(device.control_virt + RESPONSE_OFF + i as u64);
		}
		match uas::answer(&response) {
			uas::Answer::Response { tag, code } => {
				// THE ANSWER'S TAG IS THE REQUEST'S, not the managed command's, and checking the wrong
				// one here would accept an answer belonging to the command being aborted.
				if uas::matches(tag, MANAGEMENT_TAG).is_err() {
					print(b"driver.xhci: a UAS task-management answer carried a tag this driver did not issue\n");
					return false;
				}
				uas::task_done(code)
			}
			_ => {
				print(b"driver.xhci: a UAS task-management request was answered with something that is not a response unit\n");
				false
			}
		}
	}
}

// Wait for a completion on ONE stream of the status pipe, servicing everything else that arrives.
//
// ROUTED BY THE TRB POINTER, because an xHCI Transfer Event carries no stream id - the endpoint and
// the slot are in it and the stream is not. Each stream's ring is its own page, so the pointer the
// event carries says which ring the completed TRB came from, and that is the routing.
fn wait_for_stream(hc: &mut Xhci, hids: &mut Hids, slot: u32, device: &mut Uas, stream: u32, deadline: &mut common::Deadline) -> Option<u32> {
	unsafe {
		let mut spins: u32 = 0;
		loop {
			if let Some((pointer, status, control)) = take_event(hc) {
				let kind: u32 = control >> 10 & 0x3f;
				let this_slot: u32 = control >> 24;
				let this_dci: u32 = control >> 16 & 0x1f;
				if kind == TRB_EV_TRANSFER && this_slot == slot && this_dci == device.dci_status && device.status.stream_of(pointer) == Some(stream) {
					return Some(status >> 24);
				}
				if kind == TRB_EV_TRANSFER && hc.net_rx == Some((this_slot, this_dci)) && hc.net_pending.is_none() {
					hc.net_pending = Some((status, control));
					continue;
				}
				handle_hid_event(hc, hids, status, control);
				continue;
			}
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

/// Offer one controller event to the outstanding requests, answering the consumer whose tag it
/// completes. Answers true when the event belonged here and must not be handled again.
///
/// THIS IS WHERE TAGGED CONCURRENCY ACTUALLY HAPPENS. A synchronous transport does not need it:
/// there is one command, so the next completion is its. With several tags outstanding the device
/// answers in whatever order it finishes, and the only thing that says which request an answer
/// belongs to is the STREAM the completion came back on - read out of the TRB pointer, because an
/// xHCI Transfer Event has no stream field.
pub fn absorb_event(device: &mut Uas, slot_id: u32, pointer: u64, status: u32, control: u32) -> bool {
	unsafe {
		if control >> 10 & 0x3f != TRB_EV_TRANSFER || control >> 24 != slot_id {
			return false;
		}
		let this_dci: u32 = control >> 16 & 0x1f;

		// A DATA PIPE'S COMPLETION IS KEPT AND NOT ANSWERED ON. It carries the residue, which is the
		// only account the controller keeps of how many bytes moved, and it can arrive either side of
		// the status - which is the whole point of a tagged transport.
		if let Some(stream) = device.data_in.stream_of(pointer).or_else(|| device.data_out.stream_of(pointer))
			&& let Some(index) = stream.checked_sub(STREAM_ID).map(|i| i as usize)
			&& index < COMMAND_TAGS as usize
			&& device.pending[index].active()
			&& device.pending[index].data_dci == Some(this_dci)
		{
			device.pending[index].moved = Some(status & 0x00ff_ffff);
			return true;
		}

		if this_dci != device.dci_status {
			return false;
		}
		let Some(stream) = device.status.stream_of(pointer) else { return false };
		let Some(index) = stream.checked_sub(STREAM_ID).map(|i| i as usize) else { return false };
		if index >= COMMAND_TAGS as usize || !device.pending[index].active() {
			return false;
		}

		let code = status >> 24;
		let request = core::mem::replace(&mut device.pending[index], Pending::IDLE);
		finish(device, index, request, code);
		true
	}
}

// Answer one completed request on the channel it came from.
unsafe fn finish(device: &mut Uas, index: usize, request: Pending, code: u32) {
	unsafe {
		if code != CC_SUCCESS && code != CC_SHORT_PACKET {
			reply(request.server, block::STATUS_ERR, 0);
			return;
		}
		let status_at = STATUS_OFF + index as u64 * STATUS_STRIDE;
		let mut unit = [0u8; 32];
		for (i, byte) in unit.iter_mut().enumerate() {
			*byte = r8(device.control_virt + status_at + i as u64);
		}
		// THE TAG IN THE ANSWER IS CHECKED AGAINST THIS TAG, even though the stream already routed it.
		// The two are the same fact from different sides - the controller's and the device's - and a
		// device that disagreed with its own stream is one this driver must not answer a consumer for.
		let good = match uas::answer(&unit) {
			uas::Answer::Sense { tag, status, .. } => uas::matches(tag, command_tag(index)).is_ok() && status == 0,
			_ => false,
		};
		if !good {
			reply(request.server, block::STATUS_ERR, 0);
			return;
		}
		if request.op == block::OP_READ {
			grant(request.server, device.tags[index].virt, request.bytes);
		} else {
			reply(request.server, block::STATUS_OK, 0);
		}
	}
}

/// Fail any outstanding request whose budget has run out, so a device that stops answering does not
/// leave a consumer waiting for ever and its tag held for ever.
pub fn expire(device: &mut Uas) {
	{
		let now = clock();
		for index in 0..COMMAND_TAGS as usize {
			if !device.pending[index].active() || now < device.pending[index].deadline_at {
				continue;
			}
			let request = core::mem::replace(&mut device.pending[index], Pending::IDLE);
			print(b"driver.xhci: a UAS request outlived its budget and its tag is given back\n");
			reply(request.server, block::STATUS_ERR, 0);
		}
	}
}

/// Poll the controller until every outstanding request has been answered, or the budget runs out.
///
/// POLLED AND NOT WAITED FOR, because nothing in this driver is woken by a transfer completion: the
/// service loop sleeps on its consumers and on the controller's interrupt, and this controller's
/// transfer events have always been read by spinning over the event ring. An asynchronous request
/// that relied on an interrupt would simply never be answered - which is exactly what the first
/// attempt at this did, and the silence looked like a device fault rather than a driver that had
/// gone to sleep holding somebody's request.
///
/// EVERYTHING ELSE ON THE RING IS STILL SERVICED, the same way the synchronous wait does it: an
/// adapter's frame completion is stashed and a HID event is handled, because a ring drained by one
/// consumer and discarded is a keyboard that stops working while a disk is busy.
pub fn reap(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas) {
	unsafe {
		let mut deadline = common::Deadline::ticks(REQUEST_TICKS);
		let mut spins: u32 = 0;
		while outstanding(device) > 0 {
			if let Some((pointer, status, control)) = take_event(hc) {
				if absorb_event(device, dev.slot, pointer, status, control) {
					continue;
				}
				let kind: u32 = control >> 10 & 0x3f;
				let this_slot: u32 = control >> 24;
				let this_dci: u32 = control >> 16 & 0x1f;
				if kind == TRB_EV_TRANSFER && hc.net_rx == Some((this_slot, this_dci)) && hc.net_pending.is_none() {
					hc.net_pending = Some((status, control));
					continue;
				}
				handle_hid_event(hc, hids, status, control);
				continue;
			}
			spins += 1;
			if !deadline.waiting() || spins > SPIN_BUDGET {
				// The budget is the same one a synchronous request gets, and what it leaves behind is
				// handled by `expire`: the consumer is answered with an error and the tag comes back.
				return;
			}
			if spins % 4096 == 0 {
				yield_now();
			}
		}
	}
}

/// Whether another request could be taken without waiting for one of these to finish.
///
/// THE LOOP ASKS BEFORE IT REAPS. A driver that drained completions before reading the next
/// request would finish each command before the next was posted, and two tags would never be
/// outstanding however many the transport had - which is a queue depth of one wearing the clothes
/// of a tagged transport. Filling the free tags first is bounded by the cap, so nothing else the
/// loop serves can be starved for more than that many passes.
pub fn has_free_tag(device: &Uas) -> bool {
	device.free_tag().is_some()
}

/// How many tags are carrying a request, which is what "this transport is busy" means here.
fn outstanding(device: &Uas) -> usize {
	device.pending.iter().filter(|slot| slot.active()).count()
}

/// SAID ONCE, THE FIRST TIME IT IS TRUE. Tagged concurrency is not something a driver can assert
/// about itself: either two commands were outstanding on this machine or they were not, and the
/// only honest evidence is a line printed at the moment it happened. Once, because a report per
/// request would be a log nobody reads and a number nobody checks.
static REPORTED_CONCURRENCY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn report_concurrency(device: &Uas) {
	let busy = outstanding(device);
	if busy < 2 || REPORTED_CONCURRENCY.swap(true, core::sync::atomic::Ordering::Relaxed) {
		return;
	}
	let mut line: common::Bounded<96> = common::Bounded::new();
	line.push(b"driver.xhci: ");
	line.decimal(busy as u64);
	line.push(b" UAS commands outstanding at once, each on its own tag\n");
	print(line.as_bytes());
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
pub unsafe fn transfer(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas, slot: usize, write: bool, lba: u32, count: u16) -> bool {
	unsafe {
		let bytes = count as u32 * SECTOR;
		if bytes as u64 > device.data_bytes {
			return false;
		}
		let cdb = scsi::read_write10(write, lba, count);
		let mut deadline = common::Deadline::ticks(REQUEST_TICKS);
		command(hc, hids, dev, device, slot, &cdb, bytes, !write, &mut deadline).is_ok()
	}
}

/// Commit the medium's cache.
pub unsafe fn flush(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, device: &mut Uas) -> bool {
	unsafe {
		let mut deadline = common::Deadline::ticks(REQUEST_TICKS);
		command(hc, hids, dev, device, 0, &scsi::synchronize_cache10(), 0, false, &mut deadline).is_ok()
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
		// WHICH TAG THIS REQUEST RUNS UNDER. A free tag is taken and the request is left OUTSTANDING -
		// the loop that drains events finishes it - so a second consumer's request can be posted while
		// this one is still moving. With every tag busy there is none to take, and the request runs the
		// synchronous way on tag zero, which also drains the completion that frees it.
		let free = device.free_tag();
		let slot = free.unwrap_or(0);
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
				if free.is_some() && post(hc, dev, device, slot, &scsi::read_write10(false, lba as u32, count as u16), bytes as u32, true) {
					device.pending[slot] = Pending { server, op, bytes, moved: None, data_dci: Some(device.dci_data_in), deadline_at: clock() + REQUEST_TICKS };
					report_concurrency(device);
					return;
				}
				if !transfer(hc, hids, dev, device, slot, false, lba as u32, count as u16) {
					reply(server, block::STATUS_ERR, 0);
					return;
				}
				grant(server, device.tags[slot].virt, bytes);
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
				core::ptr::copy_nonoverlapping(mapped as *const u8, device.tags[slot].virt as *mut u8, bytes as usize);
				unmap_object(handle);
				close(handle);
				// THE SOURCE IS ALREADY COPIED INTO THIS TAG'S PAGE, so the write can be left
				// outstanding exactly like a read: nothing the consumer owns is still needed.
				if free.is_some() && post(hc, dev, device, slot, &scsi::read_write10(true, lba as u32, count as u16), bytes as u32, false) {
					device.pending[slot] = Pending { server, op, bytes, moved: None, data_dci: Some(device.dci_data_out), deadline_at: clock() + REQUEST_TICKS };
					report_concurrency(device);
					return;
				}
				let ok = transfer(hc, hids, dev, device, slot, true, lba as u32, count as u16);
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
