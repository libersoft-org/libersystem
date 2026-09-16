// driver.ahci - the userspace AHCI SATA host bus adapter driver.
//
// DeviceManager launches this program with a `BIND` naming the controller and a transferred
// DeviceMemory capability to its ABAR, which for AHCI is BAR 5 rather than BAR 0. The driver maps
// it, enables AHCI mode, walks the ports the controller says it IMPLEMENTS, finds the first one with
// a SATA disk on an active link, builds that port's command list, received-FIS area and one command
// table, identifies the disk, and then serves `driver_protocol::block` - the same wire
// `driver.virtio-blk`, `driver.nvme` and the USB mass-storage path serve.
//
// EVERY DECISION IN IT LIVES IN `drivers::ahci` AND IS HOST-TESTED, the same split as the NVMe
// driver and for the same reason. What is left here is the part a real controller has to be present
// for: the MMIO, the DMA, the command issue and the waiting.
//
// POLLED AND ONE COMMAND AT A TIME. NCQ is deliberately out of the first slice - the item says the
// single-command path comes first - so exactly one slot is ever in use and `PxCI` is spun on.

#![no_std]
#![no_main]

use driver_protocol::block;
use drivers::ahci::{self, Attached, Outcome};
use drivers::{blk, common};
use rt::*;

const PAGE: u64 = 4096;

// The slot this driver issues every command in. One at a time, so one slot.
const SLOT: u32 = 0;

// The largest transfer one block request may move, before the disk's own limits.
const TRANSFER_BOUND: u64 = 128 * 1024;

// How many scatter-gather entries the one command table carries. The data span is physically
// contiguous, so a transfer needs one entry per four mebibytes and this is far more than
// `TRANSFER_BOUND` can ask for; it is a bound rather than a guess.
const PRDT_ENTRIES: usize = 8;

// Command-table layout: a 64-byte command FIS, a 16-byte ATAPI command this driver never sends, 48
// reserved bytes, and then the scatter-gather list.
const CT_PRDT_OFFSET: u64 = 0x80;

// The command list is one kibibyte of 32-byte headers; the received-FIS area is 256 bytes and must
// be 256-aligned, which putting it straight after the list satisfies.
const COMMAND_LIST_LEN: u64 = 1024;
const RECEIVED_FIS_OFFSET: u64 = COMMAND_LIST_LEN;

// Bounded waits. A controller that never answers must not hold this process or its client.
const COMMAND_SPINS: u64 = 200_000_000;
const PORT_SPINS: u64 = 50_000_000;

unsafe fn r32(addr: u64) -> u32 {
	unsafe { (addr as *const u32).read_volatile() }
}
unsafe fn w32(addr: u64, v: u32) {
	unsafe { (addr as *mut u32).write_volatile(v) }
}
// A 64-bit AHCI register is a pair of 32-bit halves, low first.
unsafe fn w64(addr: u64, v: u64) {
	unsafe {
		w32(addr, v as u32);
		w32(addr + 4, (v >> 32) as u32);
	}
}

// A contiguous DMA region this driver owns.
struct Dma {
	handle: u64,
	virt: u64,
	phys: u64,
	bytes: u64,
}

// Take a DMA region, naming the device it is for, so the kernel holds its frames if this process
// dies while the controller still has their addresses.
fn dma(device: u64, bytes: u64) -> Option<Dma> {
	let handle: i64 = dma_buffer_create_for(device, bytes);
	if handle < 0 {
		return None;
	}
	let virt: i64 = unsafe { dma_buffer_map(handle as u64) };
	if sys_is_err(virt as u64) {
		return None;
	}
	Some(Dma { handle: handle as u64, virt: virt as u64, phys: unsafe { dma_buffer_phys(handle as u64) }, bytes })
}

unsafe fn zero(virt: u64, bytes: u64) {
	unsafe { core::ptr::write_bytes(virt as *mut u8, 0, bytes as usize) };
}

// Where bring-up gave up. Every arm is a different defect and the driver prints which, for the
// reason the NVMe driver's own log made plain: a manager's failure code is a category, not a
// diagnosis.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bringup {
	Unusable,
	NoDma,
	PortStuck,
	NoDisk,
	Identify,
	Disk,
}

impl Bringup {
	// WHETHER A SECOND ATTEMPT COULD DIFFER, which is a different question from what went wrong and
	// is the one DeviceManager acts on.
	//
	// THIS MACHINE ANSWERS IT OUT LOUD. The q35 chipset carries its own SATA controller at 00:1f.2
	// with a CD-ROM on port 2, so a boot binds this driver twice: once to the controller with a disk
	// behind it, and once to one whose only device is ATAPI. The second is not a part that may yet
	// come up - it read the device and will not drive it, and nothing changes that - so reporting it
	// as retryable had DeviceManager restart a driver that had already given its final answer, and
	// the boot log carried the same refusal twice for no reason.
	fn retryable(self) -> bool {
		match self {
			// The controller said what it is and this driver will not drive it.
			Bringup::Unusable | Bringup::NoDisk | Bringup::Disk => false,
			// A resource shortage, a port that did not settle, a command that did not come back:
			// each of these is a state a second attempt can find differently.
			Bringup::NoDma | Bringup::PortStuck | Bringup::Identify => true,
		}
	}

	fn name(self) -> &'static [u8] {
		match self {
			Bringup::Unusable => b"the controller's CAP does not describe one this driver can use",
			Bringup::NoDma => b"the command list and FIS memory could not be taken",
			Bringup::PortStuck => b"a port did not stop when it was asked to",
			Bringup::NoDisk => b"no implemented port has a SATA disk on an active link",
			Bringup::Identify => b"IDENTIFY DEVICE did not complete",
			Bringup::Disk => b"the disk is not one this driver serves",
		}
	}
}

// Why a command did not produce an answer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
	Failed { status: u8, error: u8 },
	Timeout,
}

// The controller and the one port this driver serves.
struct Controller {
	device: u64,
	port: u64,
	// The command list, the received-FIS area and the one command table.
	structures: Dma,
	table_phys: u64,
	table_virt: u64,
	disk: ahci::Disk,
	most_bytes: u64,
	// The growable data span one request moves through.
	data: Dma,
}

impl Controller {
	fn most_sectors(&self) -> u64 {
		(self.most_bytes / self.disk.sector_bytes as u64).max(1)
	}

	fn grow(&mut self, bytes: u64) -> bool {
		if self.data.bytes >= bytes && self.data.virt != 0 {
			return true;
		}
		if self.data.handle != 0 {
			dma_buffer_unmap(self.data.handle);
			close(self.data.handle);
			self.data = Dma { handle: 0, virt: 0, phys: 0, bytes: 0 };
		}
		match dma(self.device, bytes.next_multiple_of(PAGE)) {
			Some(span) => {
				self.data = span;
				true
			}
			None => false,
		}
	}

	// Build one command in slot zero and issue it, then wait for it.
	//
	// THE COMMAND HEADER, THE COMMAND FIS AND THE SCATTER-GATHER LIST ARE ALL WRITTEN HERE, and the
	// order matters only in that the header must name a table that is already built when the slot is
	// issued - which is why the doorbell-equivalent, `PxCI`, is the last store.
	unsafe fn command(&mut self, ata: u8, lba: u64, sectors: u32, bytes: u64, write: bool) -> Result<(), Fault> {
		unsafe {
			// The command FIS: a register host-to-device frame, 20 bytes of the table's first 64.
			zero(self.table_virt, CT_PRDT_OFFSET);
			let fis = self.table_virt;
			(fis as *mut u8).write_volatile(0x27); // register host to device
			((fis + 1) as *mut u8).write_volatile(0x80); // this frame carries a command
			((fis + 2) as *mut u8).write_volatile(ata);
			((fis + 4) as *mut u8).write_volatile(lba as u8);
			((fis + 5) as *mut u8).write_volatile((lba >> 8) as u8);
			((fis + 6) as *mut u8).write_volatile((lba >> 16) as u8);
			// LBA MODE, ALWAYS. Bit 6 of the device register is what says the three address bytes
			// are a block number rather than a cylinder/head/sector triple, and a controller handed
			// a zero here addresses geometry that has not existed for decades.
			((fis + 7) as *mut u8).write_volatile(1 << 6);
			((fis + 8) as *mut u8).write_volatile((lba >> 24) as u8);
			((fis + 9) as *mut u8).write_volatile((lba >> 32) as u8);
			((fis + 10) as *mut u8).write_volatile((lba >> 40) as u8);
			((fis + 12) as *mut u8).write_volatile(sectors as u8);
			((fis + 13) as *mut u8).write_volatile((sectors >> 8) as u8);

			// The scatter-gather list, one entry per four mebibytes of a contiguous span.
			let entries = if bytes == 0 {
				0
			} else {
				match ahci::prdt_entries(bytes, PRDT_ENTRIES) {
					Ok(entries) => entries,
					Err(_) => return Err(Fault::Failed { status: 0, error: 0 }),
				}
			};
			for index in 0..entries {
				let entry = self.table_virt + CT_PRDT_OFFSET + (index as u64) * ahci::PRDT_ENTRY_LEN as u64;
				let span = ahci::prdt_span(bytes, index);
				let at = self.data.phys + (index as u64) * ahci::PRDT_MAX_BYTES as u64;
				w64(entry, at);
				w32(entry + 8, 0);
				// The byte count is ZERO-BASED: writing the length straight in transfers one byte
				// too many on every entry.
				w32(entry + 12, ahci::prdt_count(span));
			}

			// The command header in slot zero: the FIS length in dwords, the write bit, and how many
			// scatter-gather entries the table holds.
			let header = self.structures.virt + (SLOT as u64) * 32;
			let dw0 = 5u32 | if write { 1 << 6 } else { 0 } | ((entries as u32) << 16);
			w32(header, dw0);
			w32(header + 4, 0);
			w64(header + 8, self.table_phys);

			// Clear any error latched from before, or the task file check below reports somebody
			// else's failure as this command's.
			w32(self.port + ahci::PORT_SERR, r32(self.port + ahci::PORT_SERR));
			w32(self.port + ahci::PORT_IS, r32(self.port + ahci::PORT_IS));

			// THE TABLE MUST BE VISIBLE BEFORE THE SLOT IS ISSUED. `write_volatile` stops the
			// compiler reordering these and says nothing about the machine; the same fence the NVMe
			// driver needs before its doorbell is needed here before `PxCI`.
			core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
			w32(self.port + ahci::PORT_CI, 1 << SLOT);

			let mut spins: u64 = 0;
			loop {
				let ci = r32(self.port + ahci::PORT_CI);
				let tfd = r32(self.port + ahci::PORT_TFD);
				match ahci::outcome(ci, SLOT, tfd) {
					Outcome::Pending => {
						spins += 1;
						if spins > COMMAND_SPINS {
							return Err(Fault::Timeout);
						}
					}
					Outcome::Done => {
						// The data the controller wrote must be visible before the caller reads it.
						core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
						return Ok(());
					}
					Outcome::Failed { status, error } => return Err(Fault::Failed { status, error }),
				}
			}
		}
	}

	// The same, saying what happened when it is not success.
	unsafe fn named(&mut self, ata: u8, lba: u64, sectors: u32, bytes: u64, write: bool) -> Result<(), Fault> {
		let outcome = unsafe { self.command(ata, lba, sectors, bytes, write) };
		if let Err(fault) = outcome {
			let mut line: common::Bounded<128> = common::Bounded::new();
			line.push(b"driver.ahci: command ");
			line.push(&common::hex2(ata));
			match fault {
				Fault::Failed { status, error } => {
					line.push(b" failed, status ");
					line.push(&common::hex2(status));
					line.push(b" error ");
					line.push(&common::hex2(error));
				}
				Fault::Timeout => line.push(b" never completed"),
			}
			line.push(b"\n");
			print(line.as_bytes());
		}
		outcome
	}

	unsafe fn flush(&mut self) -> Result<(), Fault> {
		unsafe { self.named(ahci::ATA_FLUSH_CACHE_EXT, 0, 0, 0, false) }
	}
}

// Stop a port's command engine and wait for it to be stopped.
//
// BOTH RUNNING BITS, because they stop separately. `PxCMD.ST` starts the command list and `FRE` the
// FIS receive; the controller acknowledges each by clearing `CR` and `FR`, and a driver that
// rewrote `PxCLB` while either was still running is changing an address the controller is using.
unsafe fn stop_port(port: u64) -> bool {
	unsafe {
		let cmd = r32(port + ahci::PORT_CMD);
		w32(port + ahci::PORT_CMD, cmd & !(ahci::CMD_START | ahci::CMD_FIS_RECEIVE_ENABLE));
		let mut spins: u64 = 0;
		loop {
			let now = r32(port + ahci::PORT_CMD);
			if now & (ahci::CMD_LIST_RUNNING | ahci::CMD_FIS_RECEIVE_RUNNING) == 0 {
				return true;
			}
			spins += 1;
			if spins > PORT_SPINS {
				return false;
			}
		}
	}
}

// Start it again, FIS receive first: the controller may post a FIS the moment the command list runs,
// and it must have somewhere to put it.
unsafe fn start_port(port: u64) {
	unsafe {
		let cmd = r32(port + ahci::PORT_CMD);
		w32(port + ahci::PORT_CMD, cmd | ahci::CMD_FIS_RECEIVE_ENABLE);
		let cmd = r32(port + ahci::PORT_CMD);
		w32(port + ahci::PORT_CMD, cmd | ahci::CMD_START);
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
		let mut controller = match bring_up(base, device) {
			Ok(controller) => controller,
			Err(why) => {
				let mut line: common::Bounded<160> = common::Bounded::new();
				line.push(b"driver.ahci: bring-up gave up at ");
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
		report.push(b"driver.ahci: online (");
		report.push(&common::hex2(bind.info.bus));
		report.push(b":");
		report.push(&common::hex2(bind.info.dev));
		report.push(b".");
		report.push(&[b'0' + (bind.info.func % 10)]);
		report.push(b", ");
		report.decimal(controller.disk.sectors);
		report.push(b" x ");
		report.decimal(controller.disk.sector_bytes as u64);
		report.push(b" bytes)");
		common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::BLOCK, blk_client)]);
		serve(bootstrap, &bind, &mut controller, blk_server)
	}
}

unsafe fn bring_up(base: u64, device: u64) -> Result<Controller, Bringup> {
	unsafe {
		// AHCI MODE IS ENABLED BEFORE ANYTHING ELSE IS READ. A controller left in a legacy
		// compatibility mode answers the AHCI registers with whatever that mode puts there.
		let ghc = r32(base + ahci::REG_GHC);
		w32(base + ahci::REG_GHC, ghc | ahci::GHC_AHCI_ENABLE);
		let caps = ahci::Capabilities::decode(r32(base + ahci::REG_CAP));
		let pi = r32(base + ahci::REG_PI);
		// The structures go wherever the DMA allocator puts them, so whether this controller can
		// address them is a question with an answer rather than an assumption.
		let structures = dma(device, 2 * PAGE).ok_or(Bringup::NoDma)?;
		let above_4g = structures.phys + structures.bytes > u32::MAX as u64;
		caps.usable(pi, above_4g).map_err(|_| Bringup::Unusable)?;
		zero(structures.virt, structures.bytes);

		// THE PORTS THE CONTROLLER IMPLEMENTS, not zero to the port count: those are different
		// numbers on real hardware and walking the count reads registers of ports that are not there.
		let mut chosen: Option<u64> = None;
		for index in ahci::implemented_ports(pi, caps.ports) {
			let port = base + ahci::port_offset(index);
			if !stop_port(port) {
				return Err(Bringup::PortStuck);
			}
			if !ahci::link_up(r32(port + ahci::PORT_SSTS)) {
				continue;
			}
			match ahci::attached(r32(port + ahci::PORT_SIG)) {
				Attached::Disk => {}
				// REFUSED BY NAME rather than driven. An ATAPI device answers a packet command set
				// this driver does not speak, and issuing ATA reads to one produces errors that look
				// like a failing disk.
				other => {
					let mut line: common::Bounded<96> = common::Bounded::new();
					line.push(b"driver.ahci: port ");
					line.decimal(index as u64);
					line.push(match other {
						Attached::Atapi => b" carries an ATAPI device, which this driver does not serve",
						Attached::Unsupported => b" carries a device whose signature this driver does not serve",
						_ => b" carries nothing",
					});
					line.push(b"\n");
					print(line.as_bytes());
					continue;
				}
			}
			// ONE PORT, ONE BLOCK PROVIDER, for the same reason the NVMe driver serves one namespace:
			// the binding unit is a PCI function and a port is not expressible in one.
			w64(port + ahci::PORT_CLB, structures.phys);
			w64(port + ahci::PORT_FB, structures.phys + RECEIVED_FIS_OFFSET);
			start_port(port);
			chosen = Some(port);
			break;
		}
		let port = chosen.ok_or(Bringup::NoDisk)?;

		let table_virt = structures.virt + PAGE;
		let table_phys = structures.phys + PAGE;
		let mut controller = Controller { device, port, structures, table_phys, table_virt, disk: ahci::Disk { sectors: 0, sector_bytes: 512 }, most_bytes: TRANSFER_BOUND, data: Dma { handle: 0, virt: 0, phys: 0, bytes: 0 } };

		// IDENTIFY DEVICE answers into the data span, 512 bytes of it.
		if !controller.grow(512) {
			return Err(Bringup::NoDma);
		}
		zero(controller.data.virt, 512);
		controller.named(ahci::ATA_IDENTIFY, 0, 0, 512, false).map_err(|_| Bringup::Identify)?;
		let mut words = [0u16; 256];
		for (index, word) in words.iter_mut().enumerate() {
			*word = (controller.data.virt as *const u16).add(index).read_volatile();
		}
		controller.disk = ahci::identify(&words).map_err(|_| Bringup::Disk)?;
		Ok(controller)
	}
}

unsafe fn serve(bootstrap: u64, bind: &common::Bind, controller: &mut Controller, blk_server: u64) -> ! {
	unsafe {
		let mut request = [0u8; block::REQUEST_LEN];
		let mut serving = common::Serving::new(blk_server, 0);
		loop {
			let Some(at) = common::serve_any_or_answer(bootstrap, bind, &mut serving) else {
				let flushed = controller.flush().is_ok();
				if !flushed {
					print(b"driver.ahci: the disk did not complete the flush this stop requires - no clean stop is claimed for it\n");
				}
				common::finish_stop(bootstrap, bind, controller.device, flushed);
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
			// REFUSED AND NOT CLAMPED, and the disk is not asked.
			if matches!(op, block::OP_READ | block::OP_WRITE) && blk::request_range(lba, count, controller.disk.sectors, controller.most_sectors()).is_err() {
				if handle != 0 {
					close(handle);
				}
				reply(endpoint, block::STATUS_INVALID, 0);
				continue;
			}
			match op {
				block::OP_READ => serve_read(controller, endpoint, lba, count),
				block::OP_WRITE => serve_write(controller, endpoint, lba, count, handle),
				block::OP_CAPACITY => {
					let bytes = controller.disk.sectors * controller.disk.sector_bytes as u64;
					send_blocking(endpoint, &block::capacity_reply(bytes, controller.most_sectors()), 0);
				}
				block::OP_FLUSH => {
					let ok = controller.flush().is_ok();
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

unsafe fn serve_read(controller: &mut Controller, endpoint: u64, lba: u64, count: u32) {
	unsafe {
		let bytes = count as u64 * controller.disk.sector_bytes as u64;
		if !controller.grow(bytes) || controller.named(ahci::ATA_READ_DMA_EXT, lba, count, bytes, false).is_err() {
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		}
		let object: u64 = syscall(SYS_MEMORY_OBJECT_CREATE, bytes, 0, 0, 0);
		if sys_is_err(object) {
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		}
		let Some(mapped) = map_object(object) else {
			close(object);
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		};
		core::ptr::copy_nonoverlapping(controller.data.virt as *const u8, mapped as *mut u8, bytes as usize);
		unmap_object(object);
		let granted: i64 = duplicate(object, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(object);
		if granted < 0 {
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		}
		reply(endpoint, block::STATUS_OK, granted as u64);
	}
}

// THE THREE-PART CONTRACT ON THE TRANSFERRED OBJECT IS CHECKED BEFORE IT IS MAPPED: a memory object,
// readable through this handle, and at least as long as the request says.
unsafe fn serve_write(controller: &mut Controller, endpoint: u64, lba: u64, count: u32, handle: u64) {
	unsafe {
		if handle == 0 {
			reply(endpoint, block::STATUS_INVALID, 0);
			return;
		}
		let bytes = count as u64 * controller.disk.sector_bytes as u64;
		let Some(info) = object_info(handle) else {
			close(handle);
			reply(endpoint, block::STATUS_INVALID, 0);
			return;
		};
		if blk::write_source(&info, bytes).is_err() {
			close(handle);
			reply(endpoint, block::STATUS_INVALID, 0);
			return;
		}
		if !controller.grow(bytes) {
			close(handle);
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		}
		let Some(mapped) = map_object(handle) else {
			close(handle);
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		};
		core::ptr::copy_nonoverlapping(mapped as *const u8, controller.data.virt as *mut u8, bytes as usize);
		unmap_object(handle);
		close(handle);
		let ok = controller.named(ahci::ATA_WRITE_DMA_EXT, lba, count, bytes, true).is_ok();
		reply(endpoint, if ok { block::STATUS_OK } else { block::STATUS_ERR }, 0);
	}
}
