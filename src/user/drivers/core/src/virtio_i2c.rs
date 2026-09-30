// driver.virtio-i2c - an I2C/SMBus controller over virtio (device 34), serving the `liber:i2c-device` contract.
//
// NEVER THE WHOLE BUS. This driver publishes one `i2c-bus` provider, and the endpoint its offer carries serves
// nothing: it keeps no end of it, and DeviceManager closes it at publication. Its only connections are the
// ones DeviceManager mints with a SCOPED `CONNECT` naming one seven-bit address, and every transfer on such a
// connection goes to that address and no other - there is no address argument to get wrong. Each address is
// held by one connection at a time: a second connection for a held address, an unscoped connection and one
// scoped to a line is refused on the connection itself, by closing it.
//
// WHAT IT DECLARES: plain I2C, and the SMBus transactions that compose from I2C messages - quick (a zero-length
// request, which the device offers as a feature), send and receive byte, byte and word data, block write, the
// I2C block read after a one-byte command - with the packet error code computed here in software. NOT the
// block read with the device's count: a virtio-i2c request fixes each read's length when it is queued, so the
// count byte cannot choose it, and that transaction answers `unsupported`.
//
// A WRITE THEN A READ IS TWO REQUESTS, the first flagged FAIL_NEXT, which the device performs as ONE transfer
// with a repeated start. They go out together through indirect tables when the device offers them - its queue
// may be four entries long - and the device reports only success or failure, so every failure is
// `interrupted` to a consumer.
//
// THE SLEEP, between two transfers: nothing is in flight. A sleep that cuts the power stops the device first, and its
// resume negotiates it back and points it at the same ring, under the same binding - the children on this bus keep
// their connections.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use drivers::common;
use drivers::i2c::{self, Buffer, Plan};
use drivers::smbus::{self, Transaction};
use drivers::virtio::{self, Queue, Virtio};
use proto::system::{Error, I2cFunctionality, I2cReply, I2cStatus, i2c_device};
use rt::*;

const BUS_TOKEN: u16 = 0;

// The control page: two out headers, two statuses and two indirect tables.
const HEADER: u64 = 0;
const STATUS: u64 = 64;
const TABLES: u64 = 512;

// The controller, and which connection holds which address.
struct Controller {
	queue: Queue,
	indirect: bool,
	zero_length: bool,
	control: (u64, u64),
	write: (u64, u64),
	read: (u64, u64),
	held: Vec<(u64, u8)>,
}

impl Controller {
	// RUN ONE PLAN: its requests out together, and each one's status back. `data` is what the write request
	// carries. Answers the bus status and how many bytes the read request brought.
	fn run(&mut self, plan: &Plan, data: &[u8]) -> (I2cStatus, usize) {
		let control_phys = self.control.1;
		let mut chains: [[(u64, u32, bool); 3]; 2] = [[(0, 0, false); 3]; 2];
		let mut lens = [0usize; 2];
		let mut read_len = 0usize;
		for (k, request) in plan.requests.iter().flatten().enumerate() {
			// SAFETY: the control page is this driver's own DMA mapping, and the header slot is inside it.
			unsafe { core::ptr::copy_nonoverlapping(request.header.as_ptr(), (self.control.0 + HEADER + 16 * k as u64) as *mut u8, 8) };
			let header = (control_phys + HEADER + 16 * k as u64, 8u32, false);
			let status = (control_phys + STATUS + 16 * k as u64, 1u32, true);
			// SAFETY: as above.
			unsafe { ((self.control.0 + STATUS + 16 * k as u64) as *mut u8).write_volatile(0xFF) };
			lens[k] = match request.buffer {
				Buffer::None => {
					chains[k][0] = header;
					chains[k][1] = status;
					2
				}
				Buffer::Write(len) => {
					// SAFETY: `len` is at most `MAX_TRANSFER`, which the write page holds.
					unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), self.write.0 as *mut u8, len) };
					chains[k] = [header, (self.write.1, len as u32, false), status];
					3
				}
				Buffer::Read(len) => {
					read_len = len;
					chains[k] = [header, (self.read.1, len as u32, true), status];
					3
				}
			};
		}
		let count = plan.count();
		let parts: [&[(u64, u32, bool)]; 2] = [&chains[0][..lens[0]], &chains[1][..lens[1]]];
		let tables = if self.indirect { Some((self.control.0 + TABLES, control_phys + TABLES)) } else { None };
		let mut used = [0u32; 2];
		if self.queue.submit_chains(&parts[..count], tables, &mut used).is_err() {
			return (I2cStatus::Controller, 0);
		}
		for k in 0..count {
			// SAFETY: the device wrote the status byte and gave the chain back.
			let status = unsafe { ((self.control.0 + STATUS + 16 * k as u64) as *const u8).read_volatile() };
			if status != i2c::STATUS_OK {
				return (I2cStatus::Interrupted, 0);
			}
		}
		(I2cStatus::Ok, read_len)
	}

	fn read_back(&self, len: usize) -> Vec<u8> {
		let mut out = Vec::new();
		if out.try_reserve_exact(len).is_err() {
			return out;
		}
		// SAFETY: the device wrote `len` bytes into the read page, and `len` is bounded by `MAX_TRANSFER`.
		out.extend_from_slice(unsafe { core::slice::from_raw_parts(self.read.0 as *const u8, len) });
		out
	}

	fn plain(&mut self, address: u8, data: &[u8], read: usize) -> I2cReply {
		if data.len() > i2c::MAX_TRANSFER || read > i2c::MAX_TRANSFER {
			return reply(I2cStatus::TooLong, Vec::new());
		}
		let Some(plan) = i2c::plain(address, data.len(), read) else { return reply(I2cStatus::Unsupported, Vec::new()) };
		let (status, got) = self.run(&plan, data);
		reply(status, if status == I2cStatus::Ok { self.read_back(got) } else { Vec::new() })
	}

	fn smbus(&mut self, address: u8, transaction: Transaction<'_>, pec: bool) -> I2cReply {
		if matches!(transaction, Transaction::Quick { .. }) && !self.zero_length {
			return reply(I2cStatus::Unsupported, Vec::new());
		}
		let composed = match smbus::compose(address, transaction, pec) {
			Ok(composed) => composed,
			Err(smbus::Refusal::TooLong) => return reply(I2cStatus::TooLong, Vec::new()),
			Err(_) => return reply(I2cStatus::Unsupported, Vec::new()),
		};
		let plan = i2c::smbus(address, &composed);
		let (status, got) = self.run(&plan, composed.write());
		if status != I2cStatus::Ok {
			return reply(status, Vec::new());
		}
		let read = self.read_back(got);
		match composed.finish(&read) {
			Ok(bytes) => reply(I2cStatus::Ok, bytes.to_vec()),
			Err(smbus::Refusal::Pec) => reply(I2cStatus::Pec, Vec::new()),
			Err(_) => reply(I2cStatus::Interrupted, Vec::new()),
		}
	}
}

fn reply(status: I2cStatus, bytes: Vec<u8>) -> I2cReply {
	I2cReply { status, bytes: bytes.into() }
}

// ONE CONNECTION'S VIEW: the controller, and the one address it reaches.
struct Connection<'a> {
	controller: &'a mut Controller,
	address: u8,
}

impl i2c_device::Service for Connection<'_> {
	fn functionality(&mut self) -> Result<I2cFunctionality, Error> {
		Ok(I2cFunctionality { plain: true, max_transfer: i2c::MAX_TRANSFER as u16, quick: self.controller.zero_length, byte: true, byte_data: true, word_data: true, block_write: true, block_read: false, i2c_block_read: true, pec: true })
	}

	fn address(&mut self) -> Result<u8, Error> {
		Ok(self.address)
	}

	fn write(&mut self, data: Vec<u8>) -> Result<I2cReply, Error> {
		Ok(self.controller.plain(self.address, &data, 0))
	}

	fn read(&mut self, len: u16) -> Result<I2cReply, Error> {
		if len == 0 {
			return Ok(reply(I2cStatus::Unsupported, Vec::new()));
		}
		Ok(self.controller.plain(self.address, &[], len as usize))
	}

	fn write_read(&mut self, data: Vec<u8>, len: u16) -> Result<I2cReply, Error> {
		if data.is_empty() || len == 0 {
			return Ok(reply(I2cStatus::Unsupported, Vec::new()));
		}
		Ok(self.controller.plain(self.address, &data, len as usize))
	}

	fn quick(&mut self, read: bool) -> Result<I2cReply, Error> {
		Ok(self.controller.smbus(self.address, Transaction::Quick { read }, false))
	}

	fn send_byte(&mut self, value: u8, pec: bool) -> Result<I2cReply, Error> {
		Ok(self.controller.smbus(self.address, Transaction::SendByte(value), pec))
	}

	fn receive_byte(&mut self, pec: bool) -> Result<I2cReply, Error> {
		Ok(self.controller.smbus(self.address, Transaction::ReceiveByte, pec))
	}

	fn write_byte_data(&mut self, command: u8, value: u8, pec: bool) -> Result<I2cReply, Error> {
		Ok(self.controller.smbus(self.address, Transaction::WriteByteData { command, value }, pec))
	}

	fn read_byte_data(&mut self, command: u8, pec: bool) -> Result<I2cReply, Error> {
		Ok(self.controller.smbus(self.address, Transaction::ReadByteData { command }, pec))
	}

	fn write_word_data(&mut self, command: u8, value: u16, pec: bool) -> Result<I2cReply, Error> {
		Ok(self.controller.smbus(self.address, Transaction::WriteWordData { command, value }, pec))
	}

	fn read_word_data(&mut self, command: u8, pec: bool) -> Result<I2cReply, Error> {
		Ok(self.controller.smbus(self.address, Transaction::ReadWordData { command }, pec))
	}

	fn block_write(&mut self, command: u8, data: Vec<u8>, pec: bool) -> Result<I2cReply, Error> {
		Ok(self.controller.smbus(self.address, Transaction::BlockWrite { command, data: &data }, pec))
	}

	// THE ONE SMBUS TRANSACTION THIS CONTROLLER CANNOT CARRY: see the file's head.
	fn block_read(&mut self, _command: u8, _pec: bool) -> Result<I2cReply, Error> {
		Ok(reply(I2cStatus::Unsupported, Vec::new()))
	}

	fn i2c_block_read(&mut self, command: u8, len: u8, pec: bool) -> Result<I2cReply, Error> {
		Ok(self.controller.smbus(self.address, Transaction::I2cBlockRead { command, len: len as usize }, pec))
	}
}

// Serve one request on connection `index`; false when the consumer has gone.
fn serve(controller: &mut Controller, serving: &common::Serving, index: usize, buf: &mut [u8]) -> bool {
	let channel = serving.at(index);
	let (len, mut handles) = match try_recv_caps(channel, buf) {
		PolledCaps::Message { len, handles } => (len, handles),
		PolledCaps::Empty => return true,
		PolledCaps::Closed => return false,
	};
	let Some(&(_, address)) = controller.held.iter().find(|(held, _)| *held == channel) else { return false };
	let mut reply_buf = [0u8; 2200];
	let mut reply_handles = wire::Handles::new();
	let mut view = Connection { controller, address };
	if let Some(written) = i2c_device::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) {
		send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
	}
	true
}

// A REFUSED CONNECTION, SAID: the one line a person reading the log needs when a consumer finds its connection
// closed.
fn refused(address: Option<u8>, why: &[u8]) {
	let mut out = common::Bounded::<128>::new();
	out.push(b"driver.virtio-i2c: a connection ");
	if let Some(address) = address {
		out.push(b"for address 0x");
		out.push(&common::hex2(address));
		out.push(b" ");
	}
	out.push(b"was refused - ");
	out.push(why);
	out.push(b"\n");
	print(out.as_bytes());
}

struct Sleep<'a> {
	device: &'a Virtio,
	queue: &'a mut Queue,
	serving: &'a mut common::Serving,
	stopped: bool,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		match self.device.sleep(request) {
			Some(stopped) => {
				self.stopped = stopped;
				driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
			}
			None => driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding), awake_by_ms: 0 },
		}
	}

	fn resume(&mut self, lost_power: bool) -> bool {
		self.device.wake(&mut [&mut *self.queue], lost_power, core::mem::take(&mut self.stopped))
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

// A consumer has gone, or was refused: its address is free again, and the manager is told.
fn part(controller: &mut Controller, serving: &mut common::Serving, bootstrap: u64, bind: &common::Bind, index: usize) -> bool {
	let channel = serving.at(index);
	controller.held.retain(|(held, _)| *held != channel);
	let token = serving.close_at(index);
	common::disconnected(bootstrap, bind, token)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, resources) = common::handshake(bootstrap);
		let device: Virtio = common::bringup_bound(bootstrap, &bind, &resources, (i2c::FEATURE_ZERO_LENGTH_REQUEST as u32) | virtio::FEATURE_INDIRECT_DESC);
		let accepted = device.features_word0();
		let queue: Queue = match device.setup_queue(0) {
			Some(queue) => queue,
			None => exit(),
		};
		let page = || -> (u64, u64) {
			match dma_buffer_for(device.capability, 4096) {
				Some((_, virt, phys)) => (virt, phys),
				None => exit(),
			}
		};
		let (control, write, read) = (page(), page(), page());
		device.driver_ok();
		let mut controller = Controller { queue, indirect: accepted & virtio::FEATURE_INDIRECT_DESC != 0, zero_length: accepted & (i2c::FEATURE_ZERO_LENGTH_REQUEST as u32) != 0, control, write, read, held: Vec::new() };
		// THE OFFERED ENDPOINT SERVES NOTHING: its far end is the offer, and this end is closed at once.
		let Some((near, far)) = channel() else { exit() };
		let mut line = [0u8; 64];
		let n = common::describe(&mut line, b"virtio-i2c", &device, if controller.zero_length { b"i2c, smbus, quick" } else { b"i2c, smbus" });
		if !common::online(bootstrap, &bind, &line[..n], &[(driver_protocol::provider::I2C_BUS, far)]) {
			exit();
		}
		// THE CONTROLLER'S FIRMWARE NODE - its companion, whose `_AEI` lines and fields the ACPI service holds connections
		// through - asked for once online; the manager answers when the namespace has one.
		let _ = common::request_node(bootstrap, &bind);
		close(near);
		let mut serving = common::Serving::from_offers(&[(BUS_TOKEN, 0)]);
		let mut buf = alloc::vec![0u8; 2200];
		common::takes_sleep();
		loop {
			match common::wait_providers_until(bootstrap, &bind, &mut serving, &[], 0) {
				None => {
					if common::stop_requested() {
						common::finish_stop(bootstrap, &bind, device.capability, common::quiesce_virtio());
					}
					exit();
				}
				Some(None) => {
					if !common::take_sleep_step(bootstrap, &bind, &mut Sleep { device: &device, queue: &mut controller.queue, serving: &mut serving, stopped: false }) {
						if common::stop_requested() {
							common::finish_stop(bootstrap, &bind, device.capability, common::quiesce_virtio());
						}
						exit();
					}
				}
				Some(Some(common::ProviderReady::Connected(index))) => {
					// ONE ADDRESS, HELD BY ONE CONNECTION. Anything else is refused on the connection itself.
					let channel = serving.at(index);
					let admitted = match serving.scope_at(index) {
						driver_protocol::Scope::I2cAddress(address) if !controller.held.iter().any(|(_, held)| *held == address) => {
							controller.held.push((channel, address));
							true
						}
						driver_protocol::Scope::I2cAddress(address) => {
							refused(Some(address), b"another connection holds it");
							false
						}
						_ => {
							refused(None, b"its scope names no address");
							false
						}
					};
					if !admitted && !part(&mut controller, &mut serving, bootstrap, &bind, index) {
						exit();
					}
				}
				Some(Some(common::ProviderReady::Consumer(index))) => {
					if !serve(&mut controller, &serving, index, &mut buf) && !part(&mut controller, &mut serving, bootstrap, &bind, index) {
						exit();
					}
				}
				Some(Some(common::ProviderReady::Device(_))) => {}
			}
		}
	}
}
