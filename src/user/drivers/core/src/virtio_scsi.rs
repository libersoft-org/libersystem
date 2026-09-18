// driver.virtio-scsi - the userspace virtio-scsi driver.
//
// virtio-scsi carries the SCSI command set over a virtqueue: a request is a header naming the target
// and carrying a command descriptor block, the data, and a response header the device writes back
// with a status and its sense data. This driver brings the device up over the shared virtio
// transport, finds the first target that answers, reads its capacity, and serves
// `driver_protocol::block` - the same wire `virtio-blk`, the USB mass-storage path, NVMe, AHCI and
// SDHCI serve.
//
// THE SCSI ITSELF IS IN `drivers::scsi` AND IS HOST-TESTED, and this item is what extracted it: the
// command set lived inside `usb_storage.rs`, where a second consumer would have copied it. The USB
// path moved onto the same core in the same change, which is the rule this tree states for a shared
// contract and the one the block wire's three divergent copies are the argument for.
//
// IT COMPLEMENTS `virtio-blk` RATHER THAN REPLACING IT. A virtio block device speaks its own tiny
// request format; this speaks SCSI to a target behind a controller, which is what a machine with a
// real HBA looks like, and the same block contract comes out of both.

#![no_std]
#![no_main]

use driver_protocol::block;
use drivers::{blk, common, scsi, virtio};
use rt::*;

// The virtqueues virtio-scsi defines: control, event, and the first request queue.
const QUEUE_EVENT: u16 = 1;
const QUEUE_REQUEST: u16 = 2;

// The most logical units this driver serves. Each one is a published block provider, a name to tell
// it from the others and a slot in the serving set, and the handshake carries eight offers - so this
// is what a BOUNDED driver takes of a bus that may report far more.
const MAX_UNITS: usize = 4;

// The most targets walked, whatever the device says it has. `max_target` is the DEVICE'S number and
// is 255 on the model this tree runs against: a driver that walked all of them would spend its whole
// bind window asking empty targets whether they are ready.
const MAX_TARGETS: u16 = 8;

// How many event buffers the device may fill before this driver drains them. Four, because the only
// events it acts on are a unit arriving and a unit leaving - and the device says so when it had to
// drop some, which is answered by re-enumerating rather than by a deeper queue.
const EVENT_BUFFERS: u16 = 4;

// THE WHOLE ENUMERATION'S BUDGET, SHARED, and this is the defect it exists for.
//
// A queue command polls for a completion with a spin budget of its own, so a device that answers
// nothing costs a full budget PER COMMAND. Walking nine targets with sixteen ready-retries each is
// 144 of them, and the driver spends every one before it answers its bind - which reads, from the
// manager's side, as a driver that never started. The same shape cost the UAS bring-up an afternoon.
//
// The budget below is a fraction of the two-second bind window, and it bounds the WALK: one dead
// command still costs what one costs, and the walk stops rather than paying for it again.
const ENUMERATE_TICKS: u64 = 60;

// Device configuration offsets. `sense_size` and `cdb_size` are WRITABLE and this driver leaves them
// at the device's defaults rather than negotiating: the layout below is built from what it reads.
const CFG_SENSE_SIZE: u64 = 20;
const CFG_CDB_SIZE: u64 = 24;
const CFG_MAX_TARGET: u64 = 30;

// The request header: an eight-byte addressing field, an id, three one-byte fields, then the command
// descriptor block padded to `cdb_size`.
const REQ_LUN: u64 = 0;
const REQ_ID: u64 = 8;
const REQ_TASK_ATTR: u64 = 16;
const REQ_CDB: u64 = 19;

// The response header: the sense length, the residual, a status qualifier, the status, the response
// code, and then the sense data padded to `sense_size`.
const RESP_SENSE_LEN: u64 = 0;
const RESP_STATUS: u64 = 10;
const RESP_RESPONSE: u64 = 11;
const RESP_SENSE: u64 = 12;

// Where in the control page each part sits. The request header, the response header and a small
// answer buffer share one page, which keeps a command to one allocation.
const REQ_OFF: u64 = 0;
const RESP_OFF: u64 = 256;
const ANSWER_OFF: u64 = 1024;
const ANSWER_LEN: u32 = 256;
// And the event buffers, in the same page and past everything the request path uses.
const EVENT_OFF: u64 = 2048;

// The most blocks one request moves. The ten-byte command's count field is sixteen bits, and the
// data span is grown to whatever this admits.
const MOST_BLOCKS: u64 = 64;

// How many times a unit that says it is becoming ready is asked again. A power-on attention clears
// by being read, so the first command after a reset is expected to be refused once.
const READY_ATTEMPTS: u32 = 16;

unsafe fn w8(addr: u64, v: u8) {
	unsafe { (addr as *mut u8).write_volatile(v) }
}
unsafe fn r8(addr: u64) -> u8 {
	unsafe { (addr as *const u8).read_volatile() }
}

// Read a little-endian u32 out of the device-specific configuration.
fn config_u32(device: &virtio::Virtio, offset: u64) -> u32 {
	let mut value = 0u32;
	for i in 0..4u64 {
		value |= (device.config_read(offset + i) as u32) << (i * 8);
	}
	value
}

// One logical unit this driver serves: where it is, how big it is, and the name its provider is
// published under.
#[derive(Clone, Copy)]
struct Unit {
	// The addressing field this unit answers on.
	lun: [u8; 8],
	capacity: scsi::Capacity,
	target: u8,
	number: u16,
	// `tNlM`, built once so the name a provider is published under and the name a report prints are
	// the same bytes.
	name: [u8; UNIT_NAME_LEN],
	name_len: usize,
	// Cleared when the device reports the unit gone. The publication stays - a consumer holding it
	// learns from the refusals, and a driver cannot un-say what the catalogue has already handed out.
	present: bool,
}

// `t` + up to three digits + `l` + up to five digits.
const UNIT_NAME_LEN: usize = 10;

impl Unit {
	// A slot nothing has been found for yet. `present` is what tells it from a unit.
	const EMPTY: Unit = Unit { lun: [0; 8], capacity: scsi::Capacity { blocks: 0, block_bytes: 0 }, target: 0, number: 0, name: [0; UNIT_NAME_LEN], name_len: 0, present: false };

	fn named(target: u8, number: u16, lun: [u8; 8], capacity: scsi::Capacity) -> Unit {
		let mut name = [0u8; UNIT_NAME_LEN];
		let mut at = 0usize;
		name[at] = b't';
		at += 1;
		at += common::push_decimal(&mut name[at..], target as u64);
		name[at] = b'l';
		at += 1;
		at += common::push_decimal(&mut name[at..], number as u64);
		Unit { lun, capacity, target, number, name, name_len: at, present: true }
	}

	fn name(&self) -> &[u8] {
		&self.name[..self.name_len]
	}
}

// What one command did. THREE ANSWERS AND NOT TWO, because "it failed" covers two different
// situations that need different decisions: a TARGET that refused the command is one unit's answer,
// and a DEVICE that completed nothing is the whole bus going quiet. An enumeration that cannot tell
// them apart asks the next target, and the next, and pays a full poll budget for each.
enum Outcome {
	Done,
	Refused(scsi::Sense),
	Silent,
}

impl Outcome {
	fn ok(&self) -> bool {
		matches!(self, Outcome::Done)
	}

	fn retryable(&self) -> bool {
		matches!(self, Outcome::Refused(sense) if sense.retryable())
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, resources) = common::handshake(bootstrap);
		let mut device: virtio::Virtio = common::bringup_bound(bootstrap, &bind, &resources, 0);
		// THE INTERRUPT IS WHAT MAKES HOTPLUG POSSIBLE, and a binding without one is not a failure.
		//
		// The event queue is where a unit arriving or leaving is announced, and a driver with no
		// vector would have to POLL it - which under a cooperative scheduler is a thread spinning for
		// the guest's whole life to notice a disk that may never be plugged. Without a vector this
		// driver serves what it enumerated and says nothing about what changes, which is the honest
		// degradation.
		let irq: u64 = resources.irq;
		if irq != 0 {
			device.set_msix_vector(0);
		}
		let sense_size = config_u32(&device, CFG_SENSE_SIZE).max(scsi::SENSE_LEN as u32);
		let cdb_size = config_u32(&device, CFG_CDB_SIZE).max(scsi::CDB10_LEN as u32);
		let max_target = (device.config_read(CFG_MAX_TARGET) as u16) | ((device.config_read(CFG_MAX_TARGET + 1) as u16) << 8);

		let queue = device.setup_queue(QUEUE_REQUEST);
		// THE EVENT QUEUE IS SET UP BEFORE `driver_ok`, like every other queue: a device may post to
		// it the moment it is running, and a queue built afterwards would miss whatever it said
		// first. A device that will not give one is not a reason to refuse the bus - the units found
		// below are still served, without hotplug.
		let mut events = device.setup_queue(QUEUE_EVENT);
		device.driver_ok();
		let Some(queue) = queue else {
			let mut line = [0u8; 64];
			let n = common::describe_state(&mut line, b"virtio-scsi", &device, b"DEGRADED", b"no request queue");
			common::online_and_stand(bootstrap, &bind, &line[..n], 0, 0, device.capability);
		};

		// One page for the request header, the response header and a small answer buffer.
		let control = dma_buffer_create_for(queue.capability, 4096);
		if control < 0 {
			let mut line = [0u8; 64];
			let n = common::describe_state(&mut line, b"virtio-scsi", &device, b"DEGRADED", b"no control page");
			common::online_and_stand(bootstrap, &bind, &line[..n], 0, 0, device.capability);
		}
		let virt = dma_buffer_map(control as u64);
		if sys_is_err(virt as u64) {
			exit();
		}
		let virt = virt as u64;
		let phys = dma_buffer_phys(control as u64);

		let mut units = [Unit::EMPTY; MAX_UNITS];
		let found = find_units(&queue, virt, phys, sense_size, cdb_size, max_target, &mut units);
		if found == 0 {
			let mut line = [0u8; 64];
			let n = common::describe_state(&mut line, b"virtio-scsi", &device, b"DEGRADED", b"no unit answered");
			common::online_and_stand(bootstrap, &bind, &line[..n], 0, 0, device.capability);
		}

		// A CHANNEL PER SLOT AND NOT PER UNIT FOUND, because a unit that arrives later is published
		// under a token this driver already holds a server for - the manager's offer names a token,
		// and a token invented after the handshake belongs to no publication.
		let mut servers = [0u64; MAX_UNITS];
		let mut clients = [0u64; MAX_UNITS];
		for slot in 0..MAX_UNITS {
			let Some((server, client)) = channel() else {
				let mut line = [0u8; 64];
				let n = common::describe_state(&mut line, b"virtio-scsi", &device, b"DEGRADED", b"no channel");
				common::online_and_stand(bootstrap, &bind, &line[..n], 0, 0, device.capability);
			};
			servers[slot] = server;
			clients[slot] = client;
		}

		let mut report: common::Bounded<160> = common::Bounded::new();
		report.push(b"driver.virtio-scsi: online (");
		report.push(&common::hex2(bind.info.bus));
		report.push(b":");
		report.push(&common::hex2(bind.info.dev));
		report.push(b".");
		report.push(&[b'0' + (bind.info.func % 10)]);
		report.push(b", ");
		report.decimal(found as u64);
		report.push(if found == 1 { b" unit: " } else { b" units: " });
		for (slot, unit) in units[..found].iter().enumerate() {
			if slot != 0 {
				report.push(b", ");
			}
			report.push(unit.name());
			report.push(b" ");
			report.decimal(unit.capacity.blocks);
			report.push(b" x ");
			report.decimal(unit.capacity.block_bytes as u64);
			report.push(b" bytes");
		}
		report.push(b")");
		// ONE OFFER PER SLOT, EACH UNDER ITS UNIT'S OWN NAME. A kind says what a provider IS and a
		// name says WHICH one: four block providers whose every other catalogue field is identical
		// would otherwise hand a consumer asking for `block` whichever came first - which for a disk
		// is a consumer writing to the wrong one with nothing anywhere to notice.
		// A ZERO HANDLE KEEPS ITS TOKEN AND IS NOT SENT: the slot is reserved for a unit that arrives.
		let mut offers = [(driver_protocol::provider::BLOCK, 0u64, &[][..]); MAX_UNITS];
		for (slot, offer) in offers.iter_mut().enumerate() {
			*offer = (driver_protocol::provider::BLOCK, if slot < found { clients[slot] } else { 0 }, units[slot].name());
		}
		common::online_named(bootstrap, &bind, report.as_bytes(), &offers);
		if let Some(queue) = events.as_mut() {
			post_event_buffers(queue, phys + EVENT_OFF);
		}
		serve(bootstrap, &bind, &queue, events, virt, phys, sense_size, cdb_size, &mut units, found, &servers, &clients, irq, device.capability)
	}
}

// Seed the event queue with the buffers the device fills, and let it interrupt when it does.
unsafe fn post_event_buffers(queue: &mut virtio::Queue, phys: u64) {
	for id in 0..EVENT_BUFFERS {
		queue.post_recv(id, phys + id as u64 * scsi::EVENT_LEN as u64, scsi::EVENT_LEN as u32);
	}
	queue.enable_interrupts();
	queue.notify();
}

// Run one SCSI command against `lun`, answering the target's status.
//
// THE RESPONSE CODE AND THE STATUS ARE BOTH READ, because they answer different questions: whether
// the DEVICE carried the request, and what the TARGET thought of it. A driver reading only the second
// calls a request that was never delivered a success, since the target set nothing.
#[allow(clippy::too_many_arguments)]
unsafe fn command(queue: &virtio::Queue, virt: u64, phys: u64, sense_size: u32, cdb_size: u32, lun: &[u8; 8], cdb: &[u8], data: Option<(u64, u32, bool)>) -> Outcome {
	unsafe {
		let request = virt + REQ_OFF;
		core::ptr::write_bytes(request as *mut u8, 0, (REQ_CDB + cdb_size as u64) as usize);
		for (i, byte) in lun.iter().enumerate() {
			w8(request + REQ_LUN + i as u64, *byte);
		}
		w8(request + REQ_ID, 1);
		w8(request + REQ_TASK_ATTR, 0); // simple
		for (i, byte) in cdb.iter().enumerate() {
			w8(request + REQ_CDB + i as u64, *byte);
		}
		let response = virt + RESP_OFF;
		core::ptr::write_bytes(response as *mut u8, 0, (RESP_SENSE + sense_size as u64) as usize);
		// A SENTINEL THE DEVICE MUST OVERWRITE. Zero is a valid response code, so a device that wrote
		// nothing at all would read as a successfully carried request.
		w8(response + RESP_RESPONSE, 0xFF);

		let request_len = (REQ_CDB + cdb_size as u64) as u32;
		let response_len = (RESP_SENSE + sense_size as u64) as u32;
		// THIS DEVICE COUNTS THE DATA AND THE RESPONSE, IN BOTH DIRECTIONS, which is the bound the
		// chain is checked against. Measured: a read of eight bytes reports 8 + 108 and a write of a
		// sector reports 512 + 108, so the ordinary bound - what the chain offered for WRITING -
		// accepts the read and refuses the write, which is exactly what it did.
		let most_used = response_len.saturating_add(data.map_or(0, |(_, len, _)| len));
		// READABLE PARTS FIRST AND WRITABLE AFTER, which is the order virtio requires of a chain and
		// which puts the data on the correct side depending on the direction.
		let moved = match data {
			Some((data_phys, data_len, to_device)) if to_device => {
				let bufs = [(phys + REQ_OFF, request_len, false), (data_phys, data_len, false), (phys + RESP_OFF, response_len, true)];
				queue.submit_bounded(&bufs, most_used)
			}
			Some((data_phys, data_len, _)) => {
				let bufs = [(phys + REQ_OFF, request_len, false), (phys + RESP_OFF, response_len, true), (data_phys, data_len, true)];
				queue.submit_bounded(&bufs, most_used)
			}
			None => {
				let bufs = [(phys + REQ_OFF, request_len, false), (phys + RESP_OFF, response_len, true)];
				queue.submit_bounded(&bufs, most_used)
			}
		};
		let moved = match moved {
			Ok(moved) => Some(moved),
			Err(virtio::UsedFault::NoCompletion) => {
				// THE DEVICE COMPLETED NOTHING, which is not this target's answer and not this
				// command's. Said once and handed up as `Silent`, so the caller stops asking rather
				// than paying the same poll budget for every target left on the bus.
				print(b"driver.virtio-scsi: the request queue completed nothing - the device is not answering\n");
				return Outcome::Silent;
			}
			Err(fault) => {
				let mut line: common::Bounded<192> = common::Bounded::new();
				line.push(b"driver.virtio-scsi: queue refused command ");
				line.push(&common::hex2(cdb[0]));
				line.push(b" - ");
				line.push(match fault {
					virtio::UsedFault::Id => b"the used element named a descriptor this chain did not post",
					virtio::UsedFault::Length => b"the device reported more written than the chain offered",
					_ => b"no completion",
				});
				line.push(b", req ");
				line.decimal(request_len as u64);
				line.push(b" resp ");
				line.decimal(response_len as u64);
				line.push(b"\n");
				print(line.as_bytes());
				None
			}
		};
		let Some(moved) = moved else {
			// THE ONE BRANCH THAT SAID NOTHING, which is why a failing write looked like a target
			// refusing it: the chain never reached the device at all, and every field the response
			// would have carried was still the driver's own sentinel.
			let mut line: common::Bounded<96> = common::Bounded::new();
			line.push(b"driver.virtio-scsi: command ");
			line.push(&common::hex2(cdb[0]));
			line.push(b" was not carried by the queue");
			line.push(b"\n");
			print(line.as_bytes());
			return Outcome::Refused(scsi::Sense::Failed);
		};
		let _ = moved;
		let code = r8(response + RESP_RESPONSE);
		let status = r8(response + RESP_STATUS);
		if code != 0 || status != 0 {
			// BOTH NUMBERS, because they answer different questions and "the command failed" names
			// neither. A sentinel still standing at 0xFF is a third answer again: the device wrote
			// nothing into the response at all, which is the chain being wrong rather than the
			// target refusing anything.
			let mut line: common::Bounded<96> = common::Bounded::new();
			line.push(b"driver.virtio-scsi: command ");
			line.push(&common::hex2(cdb[0]));
			line.push(b" response ");
			line.push(&common::hex2(code));
			line.push(b" status ");
			line.push(&common::hex2(status));
			line.push(b"\n");
			print(line.as_bytes());
		}
		match scsi::virtio_outcome(code, status) {
			Ok(()) => Outcome::Done,
			Err(scsi::Sense::Other { key: 0xFE }) => {
				// CHECK CONDITION: the sense the device copied back says what happened, and it is
				// read from the response rather than asked for with a second command - which is what
				// makes this transport's error path shorter than the USB one's.
				let mut sense = [0u8; scsi::SENSE_LEN];
				for (i, byte) in sense.iter_mut().enumerate() {
					*byte = r8(response + RESP_SENSE + i as u64);
				}
				Outcome::Refused(scsi::sense(&sense))
			}
			Err(other) => Outcome::Refused(other),
		}
	}
}

// Walk the bus and take the logical units that answer, up to this driver's bound.
//
// TARGETS AND THEN UNITS BEHIND EACH, which is the shape of a SCSI bus and the reason this transport
// exists beside `virtio-blk`: one controller in front of several targets, each with its own units.
// A driver that took the first unit that answered served one disk on a machine that has four.
//
// THE UNITS COME FROM THE TARGET RATHER THAN FROM A GUESS. `REPORT LUNS` is what a target answers
// with the list of what is behind it, and probing numbers instead means a command per number that
// is not there - which is the cost this walk's budget exists to bound.
//
// EVERYTHING IS UNDER ONE SHARED DEADLINE. See `ENUMERATE_TICKS`.
#[allow(clippy::too_many_arguments)]
unsafe fn find_units(queue: &virtio::Queue, virt: u64, phys: u64, sense_size: u32, cdb_size: u32, max_target: u16, units: &mut [Unit; MAX_UNITS]) -> usize {
	unsafe {
		let mut found = 0usize;
		let mut deadline = common::Deadline::ticks(ENUMERATE_TICKS);
		let last = max_target.min(MAX_TARGETS - 1);
		for id in 0..=last {
			if found == MAX_UNITS || !deadline.waiting() {
				break;
			}
			// A UNIT'S FIRST COMMAND AFTER POWER-ON IS REFUSED ONCE and the refusal clears by being
			// read, which is why this asks more than once rather than deciding on the first answer.
			// The retries are the TARGET'S, under the walk's own budget: a target that is simply not
			// there answers at once and costs one command, not sixteen.
			let probe = scsi::virtio_lun(id as u8, 0);
			let mut ready = false;
			for _ in 0..READY_ATTEMPTS {
				match command(queue, virt, phys, sense_size, cdb_size, &probe, &scsi::test_unit_ready(), None) {
					Outcome::Done => {
						ready = true;
						break;
					}
					// THE WHOLE WALK STOPS, not this target's retries. A device that completed
					// nothing will complete nothing for the next target either, and asking costs a
					// poll budget each time.
					Outcome::Silent => return found,
					other if other.retryable() && deadline.waiting() => continue,
					_ => break,
				}
			}
			if !ready {
				continue;
			}
			// WHAT IS BEHIND THIS TARGET, asked of the target. A refusal is not a bus with nothing on
			// it: `REPORT LUNS` is mandatory for a modern target and optional for an old one, so a
			// target that will not answer it is served at unit zero, which is where a single-unit
			// target puts its medium.
			let mut numbers = [0u16; MAX_UNITS];
			let mut count = 0usize;
			let answer = (phys + ANSWER_OFF, ANSWER_LEN, true);
			core::ptr::write_bytes((virt + ANSWER_OFF) as *mut u8, 0, ANSWER_LEN as usize);
			let listed = command(queue, virt, phys, sense_size, cdb_size, &probe, &scsi::report_luns(ANSWER_LEN), Some(answer));
			if let Outcome::Silent = listed {
				return found;
			}
			if listed.ok() {
				let mut bytes = [0u8; ANSWER_LEN as usize];
				for (i, byte) in bytes.iter_mut().enumerate() {
					*byte = r8(virt + ANSWER_OFF + i as u64);
				}
				count = scsi::luns(&bytes, &mut numbers);
			}
			if count == 0 {
				numbers[0] = 0;
				count = 1;
			}
			for &number in numbers[..count].iter() {
				if found == MAX_UNITS || !deadline.waiting() {
					break;
				}
				let lun = scsi::virtio_lun(id as u8, number);
				core::ptr::write_bytes((virt + ANSWER_OFF) as *mut u8, 0, 8);
				let read = command(queue, virt, phys, sense_size, cdb_size, &lun, &scsi::read_capacity10(), Some((phys + ANSWER_OFF, 8, true)));
				if let Outcome::Silent = read {
					return found;
				}
				if !read.ok() {
					continue;
				}
				let mut bytes = [0u8; 8];
				for (i, byte) in bytes.iter_mut().enumerate() {
					*byte = r8(virt + ANSWER_OFF + i as u64);
				}
				if let Ok(capacity) = scsi::capacity(&bytes) {
					units[found] = Unit::named(id as u8, number, lun, capacity);
					found += 1;
				}
			}
		}
		found
	}
}

// A growable contiguous DMA span the block data rides.
struct Span {
	handle: u64,
	virt: u64,
	phys: u64,
	bytes: u64,
	capability: u64,
}

impl Span {
	fn grow(&mut self, bytes: u64) -> bool {
		if self.bytes >= bytes && self.virt != 0 {
			return true;
		}
		if self.handle != 0 {
			dma_buffer_unmap(self.handle);
			close(self.handle);
			self.handle = 0;
			self.virt = 0;
			self.bytes = 0;
		}
		let handle = dma_buffer_create_for(self.capability, bytes.next_multiple_of(4096));
		if handle < 0 {
			return false;
		}
		let virt = unsafe { dma_buffer_map(handle as u64) };
		if sys_is_err(virt as u64) {
			return false;
		}
		self.handle = handle as u64;
		self.virt = virt as u64;
		self.phys = unsafe { dma_buffer_phys(handle as u64) };
		self.bytes = bytes.next_multiple_of(4096);
		true
	}
}

// SERVE EVERY PUBLISHED UNIT FROM ONE LOOP, AND WATCH THE BUS WHILE DOING IT.
//
// One loop and not a thread per unit: they share one device, one request queue and one interrupt, so
// threads would contend over exactly what they are meant to separate. The token a request arrives
// under is the slot it is about - `online_named` assigned it as the position in the offer list - and
// that is what lets four disks be served over one wire without a request ever reaching the wrong one.
#[allow(clippy::too_many_arguments)]
unsafe fn serve(bootstrap: u64, bind: &common::Bind, queue: &virtio::Queue, mut events: Option<virtio::Queue>, virt: u64, phys: u64, sense_size: u32, cdb_size: u32, units: &mut [Unit; MAX_UNITS], found: usize, servers: &[u64; MAX_UNITS], clients: &[u64; MAX_UNITS], irq: u64, device: u64) -> ! {
	unsafe {
		let mut span = Span { handle: 0, virt: 0, phys: 0, bytes: 0, capability: queue.capability };
		let mut request = [0u8; block::REQUEST_LEN];
		let mut published = found;
		let mut offers = [(0u16, 0u64); MAX_UNITS];
		for (token, slot) in offers.iter_mut().enumerate() {
			*slot = (token as u16, if token < found { servers[token] } else { 0 });
		}
		let mut serving = common::Serving::from_offers(&offers);
		let devices: [u64; 1] = [irq];
		let waits: &[u64] = if irq != 0 { &devices } else { &[] };
		loop {
			let ready = match common::wait_providers_or_answer(bootstrap, bind, &mut serving, waits) {
				Some(ready) => ready,
				None => {
					// THE FLUSH IS PER UNIT, because each has its own cache and a stop that
					// synchronised one of four claims a clean stop for three it never asked.
					let mut flushed = true;
					for unit in units[..published].iter().filter(|unit| unit.present) {
						if !command(queue, virt, phys, sense_size, cdb_size, &unit.lun, &scsi::synchronize_cache10(), None).ok() {
							flushed = false;
						}
					}
					if !flushed {
						print(b"driver.virtio-scsi: a unit did not complete the cache synchronise this stop requires - no clean stop is claimed for it\n");
					}
					let quiet = common::quiesce_virtio();
					common::finish_stop(bootstrap, bind, device, quiet && flushed);
					exit();
				}
			};
			let at = match ready {
				// A replacement consumer on one unit. The others are untouched.
				common::ProviderReady::Connected(_) => continue,
				// The device has something to say about the bus.
				common::ProviderReady::Device(_) => {
					if let Some(stream) = events.as_mut() {
						drain_events(bootstrap, bind, stream, queue, virt, phys, sense_size, cdb_size, units, &mut published, clients);
					}
					continue;
				}
				common::ProviderReady::Consumer(at) => at,
			};
			let token = serving.token_at(at) as usize;
			let endpoint = serving.at(at);
			let Received::Message { len, handle } = recv_blocking(endpoint, &mut request) else {
				continue;
			};
			let Some(block::Request { op, lba, count }) = block::Request::decode(&request[..len]) else {
				if handle != 0 {
					close(handle);
				}
				reply(endpoint, block::STATUS_INVALID, 0);
				continue;
			};
			// A TOKEN NO UNIT OF THIS DRIVER'S OWNS, or one whose unit the device has taken away.
			// Refused rather than served: what the caller holds still addresses a publication, and
			// the medium behind it is gone.
			let Some(unit) = units.get(token).copied().filter(|unit| unit.present) else {
				if handle != 0 {
					close(handle);
				}
				reply(endpoint, block::STATUS_ERR, 0);
				continue;
			};
			let block_bytes = unit.capacity.block_bytes as u64;
			// REFUSED AND NOT CLAMPED, and the ten-byte command's thirty-two-bit address is checked
			// as well as the range: a medium past two terabytes needs the sixteen-byte form, and
			// truncating instead would write two terabytes away from where the caller said.
			if matches!(op, block::OP_READ | block::OP_WRITE) && (blk::request_range(lba, count, unit.capacity.blocks, MOST_BLOCKS).is_err() || blk::command_lba32(lba, count).is_err()) {
				if handle != 0 {
					close(handle);
				}
				reply(endpoint, block::STATUS_INVALID, 0);
				continue;
			}
			match op {
				block::OP_READ => {
					let bytes = count as u64 * block_bytes;
					if !span.grow(bytes) {
						reply(endpoint, block::STATUS_ERR, 0);
						continue;
					}
					let cdb = scsi::read_write10(false, lba as u32, count as u16);
					if !command(queue, virt, phys, sense_size, cdb_size, &unit.lun, &cdb, Some((span.phys, bytes as u32, false))).ok() {
						reply(endpoint, block::STATUS_ERR, 0);
						continue;
					}
					grant(endpoint, span.virt, bytes);
				}
				block::OP_WRITE => {
					let bytes = count as u64 * block_bytes;
					if handle == 0 {
						reply(endpoint, block::STATUS_INVALID, 0);
						continue;
					}
					let Some(info) = object_info(handle) else {
						close(handle);
						reply(endpoint, block::STATUS_INVALID, 0);
						continue;
					};
					if blk::write_source(&info, bytes).is_err() {
						close(handle);
						reply(endpoint, block::STATUS_INVALID, 0);
						continue;
					}
					if !span.grow(bytes) {
						close(handle);
						reply(endpoint, block::STATUS_ERR, 0);
						continue;
					}
					let Some(mapped) = map_object(handle) else {
						close(handle);
						reply(endpoint, block::STATUS_ERR, 0);
						continue;
					};
					core::ptr::copy_nonoverlapping(mapped as *const u8, span.virt as *mut u8, bytes as usize);
					unmap_object(handle);
					close(handle);
					let cdb = scsi::read_write10(true, lba as u32, count as u16);
					let ok = command(queue, virt, phys, sense_size, cdb_size, &unit.lun, &cdb, Some((span.phys, bytes as u32, true))).ok();
					reply(endpoint, if ok { block::STATUS_OK } else { block::STATUS_ERR }, 0);
				}
				block::OP_CAPACITY => {
					let bytes = unit.capacity.blocks * block_bytes;
					send_blocking(endpoint, &block::capacity_reply(bytes, MOST_BLOCKS), 0);
				}
				block::OP_FLUSH => {
					let ok = command(queue, virt, phys, sense_size, cdb_size, &unit.lun, &scsi::synchronize_cache10(), None).ok();
					reply(endpoint, if ok { block::STATUS_OK } else { block::STATUS_ERR }, 0);
				}
				// EVERY OTHER OPCODE IS REFUSED HERE, AND THAT IS THIS DRIVER'S PASSTHROUGH POLICY.
				//
				// The item this driver was written for says unsupported passthrough commands are not
				// exposed to ordinary storage clients, and the way that is kept is that there is no
				// opcode on this wire that carries one: the block contract names read, write, flush
				// and capacity, and a request outside that set is answered rather than translated
				// into a command descriptor block of the caller's choosing. A driver that forwarded
				// an unknown opcode would let any consumer of a disk send FORMAT UNIT.
				_ => {
					if handle != 0 {
						close(handle);
					}
					reply(endpoint, block::STATUS_ERR, 0);
				}
			}
		}
	}
}

// WHAT THE DEVICE SAID ABOUT THE BUS, AND WHAT THIS DRIVER DOES ABOUT IT.
//
// The event queue is the only place a unit arriving or leaving is announced. Every buffer the device
// filled is drained and re-posted in one pass: a buffer left unposted is one the device cannot use
// again, and four of those is an event queue that has stopped.
#[allow(clippy::too_many_arguments)]
unsafe fn drain_events(bootstrap: u64, bind: &common::Bind, stream: &mut virtio::Queue, queue: &virtio::Queue, virt: u64, phys: u64, sense_size: u32, cdb_size: u32, units: &mut [Unit; MAX_UNITS], published: &mut usize, clients: &[u64; MAX_UNITS]) {
	unsafe {
		let mut rescan = false;
		while let Some((id, _len)) = stream.take_used() {
			let mut buffer = [0u8; scsi::EVENT_LEN];
			let at = virt + EVENT_OFF + id as u64 * scsi::EVENT_LEN as u64;
			for (i, byte) in buffer.iter_mut().enumerate() {
				*byte = r8(at + i as u64);
			}
			// RE-POSTED WHATEVER IT SAID. A buffer whose event this driver does not model is still a
			// buffer the device needs back.
			core::ptr::write_bytes(at as *mut u8, 0, scsi::EVENT_LEN);
			stream.post_recv(id, phys + EVENT_OFF + id as u64 * scsi::EVENT_LEN as u64, scsi::EVENT_LEN as u32);
			let Some((event, lun, missed)) = scsi::virtio_event(&buffer) else { continue };
			// EVENTS THE DEVICE HAD TO DROP ARE ANSWERED BY LOOKING AGAIN, because the events that
			// said what changed are exactly the ones that were thrown away.
			if missed {
				rescan = true;
			}
			match event {
				scsi::Event::Removed => {
					for unit in units.iter_mut() {
						if unit.present && unit.lun == lun {
							unit.present = false;
							let mut line: common::Bounded<96> = common::Bounded::new();
							line.push(b"driver.virtio-scsi: unit ");
							line.push(unit.name());
							line.push(b" was removed - its provider stays published and refuses\n");
							print(line.as_bytes());
						}
					}
				}
				scsi::Event::Rescan | scsi::Event::HardReset | scsi::Event::ParamChange => rescan = true,
				scsi::Event::None | scsi::Event::Other => {}
			}
		}
		stream.notify();
		if rescan {
			reconcile(bootstrap, bind, queue, virt, phys, sense_size, cdb_size, units, published, clients);
		}
	}
}

// Walk the bus again and make what is published agree with what is there.
//
// A SLOT KEEPS ITS UNIT AND ITS NAME FOR THE LIFE OF THE DRIVER. A consumer holding a provider for
// `t0l1` must not find `t2l0` behind it after a rescan, so a slot is matched by the unit it was
// published for and never re-used for a different one - a free slot is one no unit has ever taken.
#[allow(clippy::too_many_arguments)]
unsafe fn reconcile(bootstrap: u64, bind: &common::Bind, queue: &virtio::Queue, virt: u64, phys: u64, sense_size: u32, cdb_size: u32, units: &mut [Unit; MAX_UNITS], published: &mut usize, clients: &[u64; MAX_UNITS]) {
	unsafe {
		let max_target = MAX_TARGETS;
		let mut seen = [Unit::EMPTY; MAX_UNITS];
		let found = find_units(queue, virt, phys, sense_size, cdb_size, max_target, &mut seen);
		let mut still = [false; MAX_UNITS];
		for unit in seen[..found].iter() {
			if let Some(slot) = units.iter().position(|held| held.name_len != 0 && held.target == unit.target && held.number == unit.number) {
				// A UNIT THAT CAME BACK, possibly with a different medium in it.
				let returning = !units[slot].present;
				units[slot].capacity = unit.capacity;
				units[slot].present = true;
				still[slot] = true;
				if returning {
					let mut line: common::Bounded<96> = common::Bounded::new();
					line.push(b"driver.virtio-scsi: unit ");
					line.push(units[slot].name());
					line.push(b" answered again\n");
					print(line.as_bytes());
				}
				continue;
			}
			// A UNIT THIS DRIVER HAS NEVER SEEN, into a slot no unit has ever taken.
			let Some(slot) = units.iter().position(|held| held.name_len == 0) else {
				print(b"driver.virtio-scsi: a unit appeared and this driver has no slot left to publish it\n");
				break;
			};
			units[slot] = *unit;
			still[slot] = true;
			if *published <= slot {
				*published = slot + 1;
			}
			let mut line: common::Bounded<96> = common::Bounded::new();
			line.push(b"driver.virtio-scsi: unit ");
			line.push(units[slot].name());
			line.push(b" appeared, ");
			line.decimal(units[slot].capacity.blocks);
			line.push(b" x ");
			line.decimal(units[slot].capacity.block_bytes as u64);
			line.push(b" bytes\n");
			print(line.as_bytes());
			// PUBLISHED UNDER THE TOKEN THE SLOT ALREADY OWNS, which the handshake reserved with a
			// zero handle. A token invented now would belong to no publication the manager knows.
			if !common::offer_named(bootstrap, bind, driver_protocol::provider::BLOCK, slot as u16, units[slot].name(), clients[slot]) {
				print(b"driver.virtio-scsi: the manager did not take the new unit's provider\n");
			}
		}
		// And what did not answer this time is gone, whatever the events said.
		for (slot, unit) in units.iter_mut().enumerate() {
			if unit.name_len != 0 && unit.present && !still[slot] {
				unit.present = false;
			}
		}
	}
}

fn reply(endpoint: u64, status: u32, transferred: u64) {
	send_blocking(endpoint, &block::reply(status), transferred);
}

// Hand the caller a copy of what was read. The DMA span stays this driver's: a buffer the device
// writes into is not something to pass across a channel.
unsafe fn grant(endpoint: u64, from: u64, bytes: u64) {
	unsafe {
		let object = syscall(SYS_MEMORY_OBJECT_CREATE, bytes, 0, 0, 0);
		if sys_is_err(object) {
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		}
		let Some(mapped) = map_object(object) else {
			close(object);
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		};
		core::ptr::copy_nonoverlapping(from as *const u8, mapped as *mut u8, bytes as usize);
		unmap_object(object);
		let granted = duplicate(object, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(object);
		if granted < 0 {
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		}
		reply(endpoint, block::STATUS_OK, granted as u64);
	}
}
