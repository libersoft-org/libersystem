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
const QUEUE_REQUEST: u16 = 2;

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

struct Target {
	// The addressing field this target answers on.
	lun: [u8; 8],
	capacity: scsi::Capacity,
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, device) = common::bringup(bootstrap);
		let sense_size = config_u32(&device, CFG_SENSE_SIZE).max(scsi::SENSE_LEN as u32);
		let cdb_size = config_u32(&device, CFG_CDB_SIZE).max(scsi::CDB10_LEN as u32);
		let max_target = (device.config_read(CFG_MAX_TARGET) as u16) | ((device.config_read(CFG_MAX_TARGET + 1) as u16) << 8);

		let queue = device.setup_queue(QUEUE_REQUEST);
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

		let Some(target) = find_target(&queue, virt, phys, sense_size, cdb_size, max_target) else {
			let mut line = [0u8; 64];
			let n = common::describe_state(&mut line, b"virtio-scsi", &device, b"DEGRADED", b"no target answered");
			common::online_and_stand(bootstrap, &bind, &line[..n], 0, 0, device.capability);
		};

		let (blk_server, blk_client) = match channel() {
			Some(pair) => pair,
			None => {
				let mut line = [0u8; 64];
				let n = common::describe_state(&mut line, b"virtio-scsi", &device, b"DEGRADED", b"no channel");
				common::online_and_stand(bootstrap, &bind, &line[..n], 0, 0, device.capability);
			}
		};
		let mut report: common::Bounded<96> = common::Bounded::new();
		report.push(b"driver.virtio-scsi: online (");
		report.push(&common::hex2(bind.info.bus));
		report.push(b":");
		report.push(&common::hex2(bind.info.dev));
		report.push(b".");
		report.push(&[b'0' + (bind.info.func % 10)]);
		report.push(b", target ");
		report.decimal(target.lun[1] as u64);
		report.push(b", ");
		report.decimal(target.capacity.blocks);
		report.push(b" x ");
		report.decimal(target.capacity.block_bytes as u64);
		report.push(b" bytes)");
		common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::BLOCK, blk_client)]);
		serve(bootstrap, &bind, &queue, virt, phys, sense_size, cdb_size, &target, blk_server, device.capability)
	}
}

// Run one SCSI command against `lun`, answering the target's status.
//
// THE RESPONSE CODE AND THE STATUS ARE BOTH READ, because they answer different questions: whether
// the DEVICE carried the request, and what the TARGET thought of it. A driver reading only the second
// calls a request that was never delivered a success, since the target set nothing.
#[allow(clippy::too_many_arguments)]
unsafe fn command(queue: &virtio::Queue, virt: u64, phys: u64, sense_size: u32, cdb_size: u32, lun: &[u8; 8], cdb: &[u8], data: Option<(u64, u32, bool)>) -> Result<u32, scsi::Sense> {
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
			return Err(scsi::Sense::Failed);
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
			Ok(()) => Ok(r8(response + RESP_SENSE_LEN) as u32),
			Err(scsi::Sense::Other { key: 0xFE }) => {
				// CHECK CONDITION: the sense the device copied back says what happened, and it is
				// read from the response rather than asked for with a second command - which is what
				// makes this transport's error path shorter than the USB one's.
				let mut sense = [0u8; scsi::SENSE_LEN];
				for (i, byte) in sense.iter_mut().enumerate() {
					*byte = r8(response + RESP_SENSE + i as u64);
				}
				Err(scsi::sense(&sense))
			}
			Err(other) => Err(other),
		}
	}
}

// Walk the targets the device reports until one answers with a capacity.
unsafe fn find_target(queue: &virtio::Queue, virt: u64, phys: u64, sense_size: u32, cdb_size: u32, max_target: u16) -> Option<Target> {
	unsafe {
		for id in 0..=max_target.min(8) {
			let lun = scsi::virtio_lun(id as u8, 0);
			// A UNIT'S FIRST COMMAND AFTER POWER-ON IS REFUSED ONCE and the refusal clears by being
			// read, which is why this asks more than once rather than deciding on the first answer.
			let mut ready = false;
			for _ in 0..READY_ATTEMPTS {
				match command(queue, virt, phys, sense_size, cdb_size, &lun, &scsi::test_unit_ready(), None) {
					Ok(_) => {
						ready = true;
						break;
					}
					Err(why) if why.retryable() => continue,
					Err(_) => break,
				}
			}
			if !ready {
				continue;
			}
			let answer = (phys + ANSWER_OFF, ANSWER_LEN.min(8), false);
			if command(queue, virt, phys, sense_size, cdb_size, &lun, &scsi::read_capacity10(), Some((answer.0, 8, false))).is_err() {
				continue;
			}
			let mut bytes = [0u8; 8];
			for (i, byte) in bytes.iter_mut().enumerate() {
				*byte = r8(virt + ANSWER_OFF + i as u64);
			}
			if let Ok(capacity) = scsi::capacity(&bytes) {
				return Some(Target { lun, capacity });
			}
		}
		None
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

#[allow(clippy::too_many_arguments)]
unsafe fn serve(bootstrap: u64, bind: &common::Bind, queue: &virtio::Queue, virt: u64, phys: u64, sense_size: u32, cdb_size: u32, target: &Target, blk_server: u64, device: u64) -> ! {
	unsafe {
		let mut span = Span { handle: 0, virt: 0, phys: 0, bytes: 0, capability: queue.capability };
		let mut request = [0u8; block::REQUEST_LEN];
		let mut serving = common::Serving::new(blk_server, 0);
		let block_bytes = target.capacity.block_bytes as u64;
		loop {
			let Some(at) = common::serve_any_or_answer(bootstrap, bind, &mut serving) else {
				let flushed = command(queue, virt, phys, sense_size, cdb_size, &target.lun, &scsi::synchronize_cache10(), None).is_ok();
				if !flushed {
					print(b"driver.virtio-scsi: the target did not complete the cache synchronise this stop requires - no clean stop is claimed for it\n");
				}
				let quiet = common::quiesce_virtio();
				common::finish_stop(bootstrap, bind, device, quiet && flushed);
				exit();
			};
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
			// REFUSED AND NOT CLAMPED, and the ten-byte command's thirty-two-bit address is checked
			// as well as the range: a medium past two terabytes needs the sixteen-byte form, and
			// truncating instead would write two terabytes away from where the caller said.
			if matches!(op, block::OP_READ | block::OP_WRITE) && (blk::request_range(lba, count, target.capacity.blocks, MOST_BLOCKS).is_err() || blk::command_lba32(lba, count).is_err()) {
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
					if command(queue, virt, phys, sense_size, cdb_size, &target.lun, &cdb, Some((span.phys, bytes as u32, false))).is_err() {
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
					let ok = command(queue, virt, phys, sense_size, cdb_size, &target.lun, &cdb, Some((span.phys, bytes as u32, true))).is_ok();
					reply(endpoint, if ok { block::STATUS_OK } else { block::STATUS_ERR }, 0);
				}
				block::OP_CAPACITY => {
					let bytes = target.capacity.blocks * block_bytes;
					send_blocking(endpoint, &block::capacity_reply(bytes, MOST_BLOCKS), 0);
				}
				block::OP_FLUSH => {
					let ok = command(queue, virt, phys, sense_size, cdb_size, &target.lun, &scsi::synchronize_cache10(), None).is_ok();
					reply(endpoint, if ok { block::STATUS_OK } else { block::STATUS_ERR }, 0);
				}
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
