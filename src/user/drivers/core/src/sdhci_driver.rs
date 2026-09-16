// driver.sdhci - the userspace SD/eMMC host controller driver.
//
// DeviceManager launches this program with a `BIND` naming the controller and a DeviceMemory
// capability to BAR 0. The driver maps it, resets it, powers and clocks the slot, walks the card
// identification sequence, reads the card's capacity out of its CSD, and serves
// `driver_protocol::block` - the same wire the three storage drivers before it serve.
//
// THE SD PROTOCOL ITSELF IS IN `drivers::sdhci` AND KNOWS NOTHING ABOUT PCI, which is what the item
// that owns this driver asks for: board-specific clock, reset, regulator and pinmux glue belongs
// outside the universal driver, and the way to keep it outside is for the protocol core to have no
// idea how the controller was attached. This file is the attachment and the register access; that
// file is the protocol.
//
// PIO, NOT ADMA. The item says the simple path is proven first, so a transfer moves one block
// through the controller's data port rather than by scatter-gather DMA. It is slower and it is the
// path whose failure modes are visible.

#![no_std]
#![no_main]

use driver_protocol::block;
use drivers::sdhci::{self, Completion, Response};
use drivers::{blk, common};
use rt::*;

// The identification clock every card must answer at, and the speed one runs at afterwards.
const IDENTIFY_HZ: u32 = 400_000;
const TRANSFER_HZ: u32 = 25_000_000;

// One block per transfer: what makes the simple path simple.
const BLOCK_BYTES: u32 = sdhci::BLOCK_BYTES;

// Bounded waits. A controller or a card that never answers must not hold this process.
const COMMAND_SPINS: u64 = 50_000_000;
const RESET_SPINS: u64 = 10_000_000;
const POWERUP_SPINS: u64 = 200_000;

unsafe fn r8(addr: u64) -> u8 {
	unsafe { (addr as *const u8).read_volatile() }
}
unsafe fn w8(addr: u64, v: u8) {
	unsafe { (addr as *mut u8).write_volatile(v) }
}
unsafe fn r16(addr: u64) -> u16 {
	unsafe { (addr as *const u16).read_volatile() }
}
unsafe fn w16(addr: u64, v: u16) {
	unsafe { (addr as *mut u16).write_volatile(v) }
}
unsafe fn r32(addr: u64) -> u32 {
	unsafe { (addr as *const u32).read_volatile() }
}
unsafe fn w32(addr: u64, v: u32) {
	unsafe { (addr as *mut u32).write_volatile(v) }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bringup {
	NoCard,
	ResetStuck,
	ClockStuck,
	NoAnswer,
	Card,
}

impl Bringup {
	fn name(self) -> &'static [u8] {
		match self {
			Bringup::NoCard => b"the slot is empty",
			Bringup::ResetStuck => b"the controller did not finish its software reset",
			Bringup::ClockStuck => b"the internal clock never reported itself stable",
			Bringup::NoAnswer => b"the card did not answer the identification sequence",
			Bringup::Card => b"the card is not one this driver serves",
		}
	}

	// Whether a second attempt could differ, which is the question DeviceManager acts on. An empty
	// slot and a card this driver will not serve are final answers; the rest are states a retry can
	// find differently.
	fn retryable(self) -> bool {
		!matches!(self, Bringup::NoCard | Bringup::Card)
	}
}

struct Controller {
	base: u64,
	device: u64,
	card: sdhci::Card,
	// Whether the card is addressed in blocks rather than bytes. Getting this wrong reads the first
	// sector of the medium for every request, successfully, for ever.
	high_capacity: bool,
	read_only: bool,
}

impl Controller {
	// Send one command and wait for it. `argument` is the command's own; `wanted` is the set of
	// interrupt-status bits that mean it finished.
	unsafe fn command(&self, index: u8, response: Response, argument: u32, has_data: bool, wanted: u32) -> Result<[u32; 4], ()> {
		unsafe {
			// THE INHIBITS ARE ASKED RATHER THAN WAITED OUT. A command sent while the line is busy
			// is dropped by the controller with nothing reported.
			let mut spins: u64 = 0;
			while sdhci::may_send(r32(self.base + sdhci::REG_PRESENT_STATE), has_data).is_err() {
				spins += 1;
				if spins > COMMAND_SPINS {
					return Err(());
				}
			}
			// The status bits are write-one-to-clear; anything left from the previous command would
			// otherwise be read as this one's answer.
			w32(self.base + sdhci::REG_INT_STATUS, 0xFFFF_FFFF);
			w32(self.base + sdhci::REG_ARGUMENT, argument);
			core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
			w16(self.base + sdhci::REG_COMMAND, sdhci::command_word(index, response, has_data));

			let mut spins: u64 = 0;
			loop {
				let status = r32(self.base + sdhci::REG_INT_STATUS);
				match sdhci::completion(status, wanted) {
					Completion::Waiting => {
						spins += 1;
						if spins > COMMAND_SPINS {
							return Err(());
						}
					}
					Completion::Failed { .. } => return Err(()),
					Completion::Done => {
						core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
						let mut answer = [0u32; 4];
						for (index, word) in answer.iter_mut().enumerate() {
							*word = r32(self.base + sdhci::REG_RESPONSE + (index as u64) * 4);
						}
						return Ok(answer);
					}
				}
			}
		}
	}

	// Move one block through the controller's data port.
	//
	// THE BUFFER IS READ IN WHOLE WORDS AND THE COUNT IS FIXED, so a short read cannot leave the
	// port half-drained: a controller with data still in it refuses the next command, and the driver
	// that left it there would see a timeout on an unrelated request.
	unsafe fn transfer(&self, index: u8, block: u64, buffer: *mut u8, write: bool) -> Result<(), ()> {
		unsafe {
			w16(self.base + sdhci::REG_BLOCK_SIZE, BLOCK_BYTES as u16);
			w16(self.base + sdhci::REG_BLOCK_COUNT, 1);
			// Transfer mode: block count enabled, and the direction. No DMA, no multi-block.
			let mode: u16 = (1 << 1) | if write { 0 } else { 1 << 4 };
			w16(self.base + sdhci::REG_TRANSFER_MODE, mode);
			let address = sdhci::address_for(block, self.high_capacity) as u32;
			let ready = if write { sdhci::INT_BUFFER_WRITE_READY } else { sdhci::INT_BUFFER_READ_READY };
			self.command(index, Response::Short, address, true, sdhci::INT_COMMAND_COMPLETE | ready)?;

			let port = self.base + sdhci::REG_BUFFER;
			let words = (BLOCK_BYTES / 4) as usize;
			for word in 0..words {
				let at = buffer.add(word * 4) as *mut u32;
				if write {
					w32(port, at.read_unaligned());
				} else {
					at.write_unaligned(r32(port));
				}
			}

			// THE TRANSFER IS NOT DONE WHEN THE BUFFER IS EMPTY. The card is still writing, and a
			// driver that reported success here would let the next command start while it is.
			let mut spins: u64 = 0;
			loop {
				let status = r32(self.base + sdhci::REG_INT_STATUS);
				match sdhci::completion(status, sdhci::INT_TRANSFER_COMPLETE) {
					Completion::Waiting => {
						spins += 1;
						if spins > COMMAND_SPINS {
							return Err(());
						}
					}
					Completion::Failed { .. } => return Err(()),
					Completion::Done => return Ok(()),
				}
			}
		}
	}
}

// Set the slot's clock, stopping it first: the divider may only be changed while the SD clock is off.
unsafe fn set_clock(base: u64, base_hz: u32, target_hz: u32) -> bool {
	unsafe {
		w16(base + sdhci::REG_CLOCK_CONTROL, 0);
		let divider = sdhci::clock_divider(base_hz, target_hz);
		w16(base + sdhci::REG_CLOCK_CONTROL, sdhci::clock_control(divider));
		let mut spins: u64 = 0;
		while r16(base + sdhci::REG_CLOCK_CONTROL) & sdhci::CLOCK_INTERNAL_STABLE == 0 {
			spins += 1;
			if spins > RESET_SPINS {
				return false;
			}
		}
		let now = r16(base + sdhci::REG_CLOCK_CONTROL);
		w16(base + sdhci::REG_CLOCK_CONTROL, now | sdhci::CLOCK_SD_ENABLE);
		true
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, resources) = common::handshake(bootstrap);
		let device: u64 = resources.device;
		if device == 0 {
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
		}
		let base: u64 = syscall(SYS_DEVICE_MEMORY_MAP, device, 0, 0, 0);
		if sys_is_err(base) {
			common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
		}
		let controller = match bring_up(base, device) {
			Ok(controller) => controller,
			Err(why) => {
				let mut line: common::Bounded<160> = common::Bounded::new();
				line.push(b"driver.sdhci: bring-up gave up at ");
				line.push(&common::hex2(bind.info.bus));
				line.push(b":");
				line.push(&common::hex2(bind.info.dev));
				line.push(b".");
				line.push(&[b'0' + (bind.info.func % 10)]);
				line.push(b" - ");
				line.push(why.name());
				line.push(b"\n");
				print(line.as_bytes());
				let code = if why.retryable() { driver_protocol::DriverFailureCode::DeviceNotResponding } else { driver_protocol::DriverFailureCode::UnsupportedDevice };
				common::failed(bootstrap, &bind, code);
			}
		};
		let (blk_server, blk_client): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable),
		};
		let mut report: common::Bounded<96> = common::Bounded::new();
		report.push(b"driver.sdhci: online (");
		report.push(&common::hex2(bind.info.bus));
		report.push(b":");
		report.push(&common::hex2(bind.info.dev));
		report.push(b".");
		report.push(&[b'0' + (bind.info.func % 10)]);
		report.push(b", ");
		report.decimal(controller.card.blocks);
		report.push(b" x ");
		report.decimal(controller.card.block_bytes as u64);
		if controller.read_only {
			report.push(b" bytes, read-only)");
		} else {
			report.push(b" bytes)");
		}
		common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::BLOCK, blk_client)]);
		serve(bootstrap, &bind, controller, blk_server)
	}
}

unsafe fn bring_up(base: u64, device: u64) -> Result<Controller, Bringup> {
	unsafe {
		// A full software reset first, whatever state the firmware left it in.
		w8(base + sdhci::REG_SOFTWARE_RESET, sdhci::RESET_ALL);
		let mut spins: u64 = 0;
		while r8(base + sdhci::REG_SOFTWARE_RESET) & sdhci::RESET_ALL != 0 {
			spins += 1;
			if spins > RESET_SPINS {
				return Err(Bringup::ResetStuck);
			}
		}
		if !sdhci::card_present(r32(base + sdhci::REG_PRESENT_STATE)) {
			return Err(Bringup::NoCard);
		}

		// Power the slot at the highest voltage the controller says it supports. Capabilities bits
		// 24, 25 and 26 are 3.3 V, 3.0 V and 1.8 V.
		let caps = r32(base + sdhci::REG_CAPABILITIES);
		let voltage: u8 = if caps & (1 << 24) != 0 {
			0x0E
		} else if caps & (1 << 25) != 0 {
			0x0C
		} else {
			0x0A
		};
		w8(base + sdhci::REG_POWER_CONTROL, voltage | 1);

		// The base clock is capabilities bits 8..16, in megahertz. A controller reporting zero there
		// is one whose clock this driver cannot compute, so the identification clock stands in.
		let base_hz = (((caps >> 8) & 0xFF) as u32).max(1) * 1_000_000;
		if !set_clock(base, base_hz, IDENTIFY_HZ) {
			return Err(Bringup::ClockStuck);
		}
		w8(base + sdhci::REG_TIMEOUT_CONTROL, 0x0E);
		// EVERY STATUS BIT IS ENABLED AND NO SIGNAL IS. `INT_ENABLE` decides what reaches the status
		// register this driver polls; `SIGNAL_ENABLE` decides what raises an interrupt. Enabling the
		// second on a polling driver hands the machine an interrupt nobody acknowledges.
		w32(base + sdhci::REG_INT_ENABLE, 0xFFFF_FFFF);
		w32(base + sdhci::REG_SIGNAL_ENABLE, 0);

		let controller = Controller { base, device, card: sdhci::Card { blocks: 0, block_bytes: BLOCK_BYTES }, high_capacity: false, read_only: false };

		// The identification sequence.
		controller.command(sdhci::CMD_GO_IDLE, Response::None, 0, false, sdhci::INT_COMMAND_COMPLETE).map_err(|_| Bringup::NoAnswer)?;
		// CMD8 with the 2.7-3.6 V pattern: a card that echoes it back is version 2 or later, and one
		// that does not answer at all is an older card. Both continue; only the host-capacity bit
		// below differs, and the OCR answers that.
		let _ = controller.command(sdhci::CMD_SEND_IF_COND, Response::Short, 0x1AA, false, sdhci::INT_COMMAND_COMPLETE);

		// ACMD41 until the card finishes powering up. THE HOST CAPACITY BIT IS SET so a high-capacity
		// card may say so; without it, every card answers as standard capacity and is then addressed
		// in bytes.
		let mut ocr = sdhci::Ocr { ready: false, high_capacity: false };
		let mut spins: u64 = 0;
		while !ocr.ready {
			controller.command(sdhci::CMD_APP_CMD, Response::Short, 0, false, sdhci::INT_COMMAND_COMPLETE).map_err(|_| Bringup::NoAnswer)?;
			let answer = controller.command(sdhci::ACMD_SD_SEND_OP_COND, Response::ShortNoCrc, 0x4010_0000, false, sdhci::INT_COMMAND_COMPLETE).map_err(|_| Bringup::NoAnswer)?;
			ocr = sdhci::ocr(answer[0]);
			spins += 1;
			if spins > POWERUP_SPINS {
				return Err(Bringup::NoAnswer);
			}
		}

		controller.command(sdhci::CMD_ALL_SEND_CID, Response::Long, 0, false, sdhci::INT_COMMAND_COMPLETE).map_err(|_| Bringup::NoAnswer)?;
		let published = controller.command(sdhci::CMD_SEND_RELATIVE_ADDR, Response::Short, 0, false, sdhci::INT_COMMAND_COMPLETE).map_err(|_| Bringup::NoAnswer)?;
		let rca = published[0] & 0xFFFF_0000;
		// THE CSD COMES BACK AS A 136-BIT ANSWER WITH ITS LOW EIGHT BITS DROPPED, which every SD host
		// controller does: the four response words hold bits 127..8 of the CSD shifted down by eight.
		// Shifting them back is what makes the capacity fields land where the specification puts them.
		let raw = controller.command(sdhci::CMD_SEND_CSD, Response::Long, rca, false, sdhci::INT_COMMAND_COMPLETE).map_err(|_| Bringup::NoAnswer)?;
		let csd = [(raw[3] << 8) | (raw[2] >> 24), (raw[2] << 8) | (raw[1] >> 24), (raw[1] << 8) | (raw[0] >> 24), raw[0] << 8];
		let card = sdhci::csd_capacity(csd).map_err(|_| Bringup::Card)?;

		controller.command(sdhci::CMD_SELECT_CARD, Response::ShortBusy, rca, false, sdhci::INT_COMMAND_COMPLETE).map_err(|_| Bringup::NoAnswer)?;
		controller.command(sdhci::CMD_SET_BLOCKLEN, Response::Short, BLOCK_BYTES, false, sdhci::INT_COMMAND_COMPLETE).map_err(|_| Bringup::NoAnswer)?;
		if !set_clock(base, base_hz, TRANSFER_HZ) {
			return Err(Bringup::ClockStuck);
		}

		let present = r32(base + sdhci::REG_PRESENT_STATE);
		Ok(Controller { base, device, card, high_capacity: ocr.high_capacity, read_only: sdhci::write_protected(present) })
	}
}

unsafe fn serve(bootstrap: u64, bind: &common::Bind, controller: Controller, blk_server: u64) -> ! {
	unsafe {
		let mut request = [0u8; block::REQUEST_LEN];
		let mut serving = common::Serving::new(blk_server, 0);
		loop {
			let Some(at) = common::serve_any_or_answer(bootstrap, bind, &mut serving) else {
				// AN SD CARD HAS NO WRITE CACHE THIS DRIVER CAN FLUSH. Every write is complete when
				// its transfer is, which the transfer wait already proves, so a clean stop is claimed
				// because nothing is outstanding rather than because a flush was answered.
				common::finish_stop(bootstrap, bind, controller.device, true);
				exit();
			};
			let endpoint: u64 = serving.at(at);
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
			// ONE BLOCK PER REQUEST ON THE PIO PATH, and it is a refusal rather than a split: a
			// caller that asked for eight and got one would believe it had eight.
			if matches!(op, block::OP_READ | block::OP_WRITE) && blk::request_range(lba, count, controller.card.blocks, 1).is_err() {
				if handle != 0 {
					close(handle);
				}
				reply(endpoint, block::STATUS_INVALID, 0);
				continue;
			}
			match op {
				block::OP_READ => serve_read(&controller, endpoint, lba),
				block::OP_WRITE => serve_write(&controller, endpoint, lba, handle),
				block::OP_CAPACITY => {
					let bytes = controller.card.blocks * controller.card.block_bytes as u64;
					send_blocking(endpoint, &block::capacity_reply(bytes, 1), 0);
				}
				block::OP_FLUSH => reply(endpoint, block::STATUS_OK, 0),
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

unsafe fn serve_read(controller: &Controller, endpoint: u64, lba: u64) {
	unsafe {
		let object: u64 = syscall(SYS_MEMORY_OBJECT_CREATE, BLOCK_BYTES as u64, 0, 0, 0);
		if sys_is_err(object) {
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		}
		let Some(mapped) = map_object(object) else {
			close(object);
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		};
		let moved = controller.transfer(sdhci::CMD_READ_SINGLE_BLOCK, lba, mapped as *mut u8, false).is_ok();
		unmap_object(object);
		if !moved {
			close(object);
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		}
		let granted: i64 = duplicate(object, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(object);
		if granted < 0 {
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		}
		reply(endpoint, block::STATUS_OK, granted as u64);
	}
}

unsafe fn serve_write(controller: &Controller, endpoint: u64, lba: u64, handle: u64) {
	unsafe {
		if handle == 0 {
			reply(endpoint, block::STATUS_INVALID, 0);
			return;
		}
		// A READ-ONLY CARD REFUSES THE WRITE AND KEEPS SERVING READS, which is what "cover read-only
		// cards" means: the medium is not broken, it is protected, and the caller is told which.
		if controller.read_only {
			close(handle);
			reply(endpoint, block::STATUS_INVALID, 0);
			return;
		}
		let Some(info) = object_info(handle) else {
			close(handle);
			reply(endpoint, block::STATUS_INVALID, 0);
			return;
		};
		if blk::write_source(&info, BLOCK_BYTES as u64).is_err() {
			close(handle);
			reply(endpoint, block::STATUS_INVALID, 0);
			return;
		}
		let Some(mapped) = map_object(handle) else {
			close(handle);
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		};
		let moved = controller.transfer(sdhci::CMD_WRITE_BLOCK, lba, mapped as *mut u8, true).is_ok();
		unmap_object(handle);
		close(handle);
		reply(endpoint, if moved { block::STATUS_OK } else { block::STATUS_ERR }, 0);
	}
}
