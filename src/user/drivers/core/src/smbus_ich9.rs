// driver.smbus-ich9 - the ICH9 SMBus host controller (8086:2930, q35's 00:1f.3), serving the `liber:i2c-device`
// contract on address-scoped connections.
//
// NEVER THE WHOLE BUS, as every `i2c-bus` provider: the endpoint its offer carries serves nothing, and its only
// connections are the ones DeviceManager mints with a scoped `CONNECT` naming one seven-bit address - for a child
// binding, an IPMI SSIF interface, with its claim. Each address is held by one connection at a time.
//
// WHAT IT DECLARES is what SSIF uses and nothing more: the SMBus BLOCK WRITE, the BLOCK READ whose count THE DEVICE
// sends first, and PEC - the controller's own, appended and checked by the hardware. It declares NO plain I2C transfer:
// the controller performs SMBus protocols only, so a HID-over-I2C device is refused on it at bind. Its other protocols
// (quick, byte, word, the I2C block read) are declared when a consumer needs one.
//
// ITS REGISTERS are its SMBus base, BAR 4, an I/O BAR the claim mints as a port range; the kernel sets HOSTC.HST_EN
// for the claim, without which the base decodes nothing, and restores HOSTC at the release. EVERY TRANSACTION POLLS the
// host status register - no interrupt, no SMBus alert - within `TRANSACTION_TICKS`, and one still busy then is killed.
// Idle, it touches no register at all: it binds on every q35 boot, since QEMU always has the function.
//
// THE SLEEP: a transfer is never in flight where a `SUSPEND` is read, and no register is touched until `RESUME`. The
// resume finds the base decoding again - the kernel writes HOSTC again for the claim after an S3 - and clears whatever
// status the reset left.

#![no_std]
#![no_main]

extern crate alloc;

#[cfg(target_arch = "x86_64")]
mod smbus {
	use alloc::vec::Vec;
	use drivers::common;
	use proto::system::{Error, I2cFunctionality, I2cReply, I2cStatus, i2c_device};
	use rt::*;

	const BUS_TOKEN: u16 = 0;

	// The host registers, from the SMBus base.
	const HST_STS: u16 = 0x00;
	const HST_CNT: u16 = 0x02;
	const HST_CMD: u16 = 0x03;
	const XMIT_SLVA: u16 = 0x04;
	const HST_D0: u16 = 0x05;
	const HOST_BLOCK_DB: u16 = 0x07;
	const AUX_STS: u16 = 0x0C;
	const AUX_CTL: u16 = 0x0D;

	const STS_HOST_BUSY: u8 = 1 << 0;
	const STS_INTR: u8 = 1 << 1;
	const STS_DEV_ERR: u8 = 1 << 2;
	const STS_BUS_ERR: u8 = 1 << 3;
	const STS_FAILED: u8 = 1 << 4;
	const STS_BYTE_DONE: u8 = 1 << 7;
	// Every status bit a transaction leaves, written back to clear them.
	const STS_CLEAR: u8 = STS_INTR | STS_DEV_ERR | STS_BUS_ERR | STS_FAILED | STS_BYTE_DONE;

	const CNT_KILL: u8 = 1 << 1;
	const CNT_START: u8 = 1 << 6;
	const CNT_PEC_EN: u8 = 1 << 7;
	// The block data protocol, in HST_CNT's bits 4:2.
	const PROTOCOL_BLOCK: u8 = 5 << 2;

	// AUX_CTL: the hardware appends and checks the CRC; the 32-byte block buffer.
	const AUX_CRCE: u8 = 1 << 0;
	const AUX_E32B: u8 = 1 << 1;
	// AUX_STS: the CRC check failed.
	const AUX_STS_CRCE: u8 = 1 << 0;

	// The most one block carries.
	const BLOCK: usize = 32;
	// How long one transaction may take, a tenth of a second - several times what SMBus allows a device.
	const TRANSACTION_TICKS: u64 = TICKS_PER_SECOND / 10;
	// Status reads spun before the wait starts sleeping between them.
	const SPINS: u32 = 256;

	struct Controller {
		base: u16,
		// Each admitted connection and the address it reaches.
		held: Vec<(u64, u8)>,
	}

	// THE SLEEP - see the head of this file.
	struct Sleep<'a> {
		controller: &'a Controller,
		serving: &'a mut common::Serving,
	}

	impl common::SleepStep for Sleep<'_> {
		fn suspend(&mut self, _request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
			print(b"driver.smbus-ich9: suspended - no register is touched until the resume\n");
			driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
		}

		fn resume(&mut self, _lost_power: bool) -> bool {
			if self.controller.inb(HST_STS) == 0xFF && self.controller.inb(AUX_CTL) == 0xFF {
				print(b"driver.smbus-ich9: its SMBus base decodes nothing after the sleep\n");
				return false;
			}
			self.controller.outb(HST_STS, STS_CLEAR);
			self.controller.outb(AUX_STS, AUX_STS_CRCE);
			print(b"driver.smbus-ich9: resumed - its base decodes, and the status is cleared\n");
			true
		}

		fn serving(&mut self) -> Option<&mut common::Serving> {
			Some(self.serving)
		}
	}

	fn reply(status: I2cStatus, bytes: Vec<u8>) -> I2cReply {
		I2cReply { status, bytes: bytes.into() }
	}

	impl Controller {
		fn inb(&self, register: u16) -> u8 {
			rt::port::inb(self.base + register)
		}

		fn outb(&self, register: u16, value: u8) {
			rt::port::outb(self.base + register, value);
		}

		// Wait for the host to finish: the status once it is neither busy nor silent, `None` at the deadline.
		fn settle(&self) -> Option<u8> {
			let deadline = clock() + TRANSACTION_TICKS;
			let mut spins = 0u32;
			loop {
				let status = self.inb(HST_STS);
				if status & STS_HOST_BUSY == 0 && status & (STS_INTR | STS_DEV_ERR | STS_BUS_ERR | STS_FAILED) != 0 {
					return Some(status);
				}
				if clock() >= deadline {
					return None;
				}
				if spins < SPINS {
					spins += 1;
				} else {
					sleep_until(clock() + 1);
				}
			}
		}

		// THE HOST MADE READY: not busy with a transaction of anyone's, its status cleared, the block buffer on and the
		// CRC as asked. False when it stays busy.
		fn begin(&self, pec: bool) -> bool {
			if self.inb(HST_STS) & STS_HOST_BUSY != 0 && self.settle().is_none() {
				self.kill();
				return false;
			}
			self.outb(HST_STS, STS_CLEAR);
			self.outb(AUX_CTL, AUX_E32B | if pec { AUX_CRCE } else { 0 });
			true
		}

		// A transaction still busy at its deadline is killed, and the host left idle.
		fn kill(&self) {
			self.outb(HST_CNT, CNT_KILL);
			let _ = self.settle();
			self.outb(HST_CNT, 0);
			self.outb(HST_STS, STS_CLEAR);
		}

		// Start the block protocol and wait: the reply's status for anything but success.
		fn run(&self, pec: bool) -> Result<(), I2cStatus> {
			self.outb(HST_CNT, CNT_START | PROTOCOL_BLOCK | if pec { CNT_PEC_EN } else { 0 });
			let Some(status) = self.settle() else {
				self.kill();
				return Err(I2cStatus::Controller);
			};
			let failed = if status & STS_DEV_ERR != 0 {
				// A DEVICE ERROR IS A NACK - an SSIF BMC's answer that it is not ready - unless the CRC was wrong.
				Err(if pec && self.inb(AUX_STS) & AUX_STS_CRCE != 0 { I2cStatus::Pec } else { I2cStatus::NoDevice })
			} else if status & (STS_BUS_ERR | STS_FAILED) != 0 {
				Err(I2cStatus::Interrupted)
			} else {
				Ok(())
			};
			if pec {
				self.outb(AUX_STS, AUX_STS_CRCE);
			}
			failed
		}

		fn finish(&self) {
			self.outb(HST_STS, STS_CLEAR);
			self.outb(AUX_CTL, 0);
		}

		// SMBUS BLOCK WRITE: the command, the count and the bytes, through the block buffer.
		fn block_write(&self, address: u8, command: u8, data: &[u8], pec: bool) -> I2cReply {
			if data.is_empty() || data.len() > BLOCK {
				return reply(I2cStatus::TooLong, Vec::new());
			}
			if !self.begin(pec) {
				return reply(I2cStatus::Controller, Vec::new());
			}
			self.outb(XMIT_SLVA, address << 1);
			self.outb(HST_CMD, command);
			self.outb(HST_D0, data.len() as u8);
			// READING THE CONTROL REGISTER RESETS THE BLOCK BUFFER'S INDEX.
			let _ = self.inb(HST_CNT);
			for &byte in data {
				self.outb(HOST_BLOCK_DB, byte);
			}
			let outcome = self.run(pec);
			self.finish();
			match outcome {
				Ok(()) => reply(I2cStatus::Ok, Vec::new()),
				Err(status) => reply(status, Vec::new()),
			}
		}

		// SMBUS BLOCK READ: the command, then THE COUNT THE DEVICE SENDS and that many bytes, at most 32.
		fn block_read(&self, address: u8, command: u8, pec: bool) -> I2cReply {
			if !self.begin(pec) {
				return reply(I2cStatus::Controller, Vec::new());
			}
			self.outb(XMIT_SLVA, (address << 1) | 1);
			self.outb(HST_CMD, command);
			let outcome = self.run(pec);
			let bytes = match outcome {
				Ok(()) => {
					let count = self.inb(HST_D0) as usize;
					if count == 0 || count > BLOCK {
						self.finish();
						return reply(I2cStatus::TooLong, Vec::new());
					}
					let _ = self.inb(HST_CNT);
					(0..count).map(|_| self.inb(HOST_BLOCK_DB)).collect()
				}
				Err(status) => {
					self.finish();
					return reply(status, Vec::new());
				}
			};
			self.finish();
			reply(I2cStatus::Ok, bytes)
		}
	}

	// ONE CONNECTION'S VIEW: the controller, and the one address it reaches.
	struct Connection<'a> {
		controller: &'a mut Controller,
		address: u8,
	}

	impl i2c_device::Service for Connection<'_> {
		fn functionality(&mut self) -> Result<I2cFunctionality, Error> {
			Ok(I2cFunctionality { plain: false, max_transfer: BLOCK as u16, quick: false, byte: false, byte_data: false, word_data: false, block_write: true, block_read: true, i2c_block_read: false, pec: true })
		}

		fn address(&mut self) -> Result<u8, Error> {
			Ok(self.address)
		}

		fn write(&mut self, _data: Vec<u8>) -> Result<I2cReply, Error> {
			Ok(reply(I2cStatus::Unsupported, Vec::new()))
		}

		fn read(&mut self, _len: u16) -> Result<I2cReply, Error> {
			Ok(reply(I2cStatus::Unsupported, Vec::new()))
		}

		fn write_read(&mut self, _data: Vec<u8>, _len: u16) -> Result<I2cReply, Error> {
			Ok(reply(I2cStatus::Unsupported, Vec::new()))
		}

		fn quick(&mut self, _read: bool) -> Result<I2cReply, Error> {
			Ok(reply(I2cStatus::Unsupported, Vec::new()))
		}

		fn send_byte(&mut self, _value: u8, _pec: bool) -> Result<I2cReply, Error> {
			Ok(reply(I2cStatus::Unsupported, Vec::new()))
		}

		fn receive_byte(&mut self, _pec: bool) -> Result<I2cReply, Error> {
			Ok(reply(I2cStatus::Unsupported, Vec::new()))
		}

		fn write_byte_data(&mut self, _command: u8, _value: u8, _pec: bool) -> Result<I2cReply, Error> {
			Ok(reply(I2cStatus::Unsupported, Vec::new()))
		}

		fn read_byte_data(&mut self, _command: u8, _pec: bool) -> Result<I2cReply, Error> {
			Ok(reply(I2cStatus::Unsupported, Vec::new()))
		}

		fn write_word_data(&mut self, _command: u8, _value: u16, _pec: bool) -> Result<I2cReply, Error> {
			Ok(reply(I2cStatus::Unsupported, Vec::new()))
		}

		fn read_word_data(&mut self, _command: u8, _pec: bool) -> Result<I2cReply, Error> {
			Ok(reply(I2cStatus::Unsupported, Vec::new()))
		}

		fn block_write(&mut self, command: u8, data: Vec<u8>, pec: bool) -> Result<I2cReply, Error> {
			Ok(self.controller.block_write(self.address, command, &data, pec))
		}

		fn block_read(&mut self, command: u8, pec: bool) -> Result<I2cReply, Error> {
			Ok(self.controller.block_read(self.address, command, pec))
		}

		fn i2c_block_read(&mut self, _command: u8, _len: u8, _pec: bool) -> Result<I2cReply, Error> {
			Ok(reply(I2cStatus::Unsupported, Vec::new()))
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
		let mut reply_buf = [0u8; 512];
		let mut reply_handles = wire::Handles::new();
		let mut view = Connection { controller, address };
		if let Some(written) = i2c_device::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) {
			send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
		}
		true
	}

	fn refused(address: Option<u8>, why: &[u8]) {
		let mut out = common::Bounded::<128>::new();
		out.push(b"driver.smbus-ich9: a connection ");
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

	fn part(controller: &mut Controller, serving: &mut common::Serving, bootstrap: u64, bind: &common::Bind, index: usize) -> bool {
		let channel = serving.at(index);
		controller.held.retain(|(held, _)| *held != channel);
		let token = serving.close_at(index);
		common::disconnected(bootstrap, bind, token)
	}

	#[unsafe(no_mangle)]
	pub extern "C" fn __user_main(bootstrap: u64) -> ! {
		let (bind, resources) = common::handshake(bootstrap);
		let unusable = driver_protocol::DriverFailureCode::ResourceUnusable;
		// THE SMBUS BASE: BAR 4, recorded by the kernel's scan as an I/O BAR and minted with the claim.
		let found = (0..bind.info.port_count as usize).find(|&at| bind.info.ports[at].source == rt::PORT_SOURCE_IO_BAR && bind.info.ports[at].index == 4 && bind.info.ports[at].len >= 16);
		let Some(at) = found.filter(|&at| at < resources.port_range_count) else {
			print(b"driver.smbus-ich9: it was not handed its SMBus base (BAR 4)\n");
			common::failed(bootstrap, &bind, unusable)
		};
		if port_range_map(resources.port_ranges[at]) < 0 {
			print(b"driver.smbus-ich9: its SMBus base could not be mapped\n");
			common::failed(bootstrap, &bind, unusable);
		}
		let base = bind.info.ports[at].base as u16;
		let mut controller = Controller { base, held: Vec::new() };
		// A BASE THAT DECODES NOTHING reads all ones: HOSTC.HST_EN did not take.
		if controller.inb(HST_STS) == 0xFF && controller.inb(AUX_CTL) == 0xFF {
			print(b"driver.smbus-ich9: its SMBus base decodes nothing - the host is not enabled\n");
			common::failed(bootstrap, &bind, unusable);
		}
		let Some((near, far)) = channel() else { exit() };
		let mut line = common::Bounded::<96>::new();
		line.push(b"driver.smbus-ich9: online (SMBus base 0x");
		line.push(&common::hex2((base >> 8) as u8));
		line.push(&common::hex2(base as u8));
		line.push(b", block write, block read, pec)");
		if !common::online(bootstrap, &bind, line.as_bytes(), &[(driver_protocol::provider::I2C_BUS, far)]) {
			exit();
		}
		// THE FUNCTION'S FIRMWARE NODE, the resource source a child's `I2cSerialBusV2` names - asked for once online.
		let _ = common::request_node(bootstrap, &bind);
		close(near);
		let mut serving = common::Serving::from_offers(&[(BUS_TOKEN, 0)]);
		let mut buf = alloc::vec![0u8; 512];
		common::takes_sleep();
		loop {
			match common::wait_providers_until(bootstrap, &bind, &mut serving, &[], 0) {
				None => {
					if common::stop_requested() {
						common::finish_stop(bootstrap, &bind, 0, true);
					}
					exit();
				}
				Some(None) => {
					if !common::take_sleep_step(bootstrap, &bind, &mut Sleep { controller: &controller, serving: &mut serving }) {
						if common::stop_requested() {
							common::finish_stop(bootstrap, &bind, 0, true);
						}
						exit();
					}
				}
				Some(Some(common::ProviderReady::Connected(index))) => {
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

// THE ICH9 IS AN x86 CHIPSET'S: the other ports have no port space and no ICH9, and no row of theirs matches this
// driver.
#[cfg(not(target_arch = "x86_64"))]
#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = drivers::common::handshake(bootstrap);
	rt::print(b"driver.smbus-ich9: this machine has no port space - the ICH9 SMBus host is x86's\n");
	drivers::common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice)
}
