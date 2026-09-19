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

// HOW MANY COMMANDS THIS DRIVER KEEPS IN FLIGHT, which is how many tags it uses.
//
// FOUR, BECAUSE FOUR IS WHAT A DISK HAS CONSUMERS. Every disk in this tree declares four, each
// issuing its own requests, so four tags is one per consumer and a fifth is a tag no caller exists
// for. The controller's own `CAP.NCS` is the other bound and the smaller of the two wins.
//
// AND A CONTROLLER THAT DOES NOT QUEUE USES TAG ZERO AND NOTHING ELSE. `CAP.SNCQ` is the question,
// and a driver that issued a queued command to a controller without it gets an aborted command
// rather than a refusal.
const QUEUED_TAGS: usize = 4;

// Where each tag's command table sits in the second page of the structures. A table is a 64-byte
// command FIS, 16 ATAPI bytes, 48 reserved and then the scatter-gather list - 256 bytes at
// `PRDT_ENTRIES` of eight - and a command table must be 128-byte aligned, which this stride is.
const TABLE_STRIDE: u64 = 256;

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
// HOW LONG A PORT IS GIVEN TO PRESENT ITS SIGNATURE after its link comes up, in the 100 Hz ticks
// READY is measured in. A fraction of the bind window, so a port that never presents one still
// leaves this driver time to say so.
const SIGNATURE_TICKS: u64 = 20;
// And how long the link is given to come up after a reset, which the specification measures in
// milliseconds and this system measures in hundredths of a second.
const RESET_TICKS: u64 = 100;

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
#[derive(Clone, Copy)]
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
	// The command list, the received-FIS area and one command table per tag.
	structures: Dma,
	disk: ahci::Disk,
	most_bytes: u64,
	// Whether this controller queues. Read once from `CAP.SNCQ`.
	queued: bool,
	// The growable data span each tag moves its request through. ONE PER TAG AND NOT ONE SHARED:
	// two commands in flight into one buffer is two transfers into the same memory, and the second
	// to finish wins.
	data: [Dma; QUEUED_TAGS],
}

impl Controller {
	fn most_sectors(&self) -> u64 {
		(self.most_bytes / self.disk.sector_bytes as u64).max(1)
	}

	fn table_virt(&self, tag: usize) -> u64 {
		self.structures.virt + PAGE + tag as u64 * TABLE_STRIDE
	}

	fn table_phys(&self, tag: usize) -> u64 {
		self.structures.phys + PAGE + tag as u64 * TABLE_STRIDE
	}

	fn grow(&mut self, tag: usize, bytes: u64) -> bool {
		if self.data[tag].bytes >= bytes && self.data[tag].virt != 0 {
			return true;
		}
		if self.data[tag].handle != 0 {
			dma_buffer_unmap(self.data[tag].handle);
			close(self.data[tag].handle);
			self.data[tag] = Dma { handle: 0, virt: 0, phys: 0, bytes: 0 };
		}
		match dma(self.device, bytes.next_multiple_of(PAGE)) {
			Some(span) => {
				self.data[tag] = span;
				true
			}
			None => false,
		}
	}

	// Build one command in `tag`'s slot and issue it, WITHOUT waiting for it.
	//
	// THE COMMAND HEADER, THE COMMAND FIS AND THE SCATTER-GATHER LIST ARE ALL WRITTEN HERE, and the
	// order matters only in that the header must name a table that is already built when the slot is
	// issued - which is why the doorbell-equivalent, `PxCI`, is the last store.
	//
	// A QUEUED COMMAND IS ISSUED IN THE SLOT WHOSE NUMBER IS ITS TAG, which AHCI requires: the two
	// are one number, and `PxSACT` is set BEFORE `PxCI` because the controller reads them together
	// and a slot issued before its active bit is a queued command with no tag.
	unsafe fn issue(&mut self, tag: usize, ata: u8, lba: u64, sectors: u32, bytes: u64, write: bool) -> Result<(), Fault> {
		unsafe {
			let queued = matches!(ata, ahci::ATA_READ_FPDMA_QUEUED | ahci::ATA_WRITE_FPDMA_QUEUED);
			let table = self.table_virt(tag);
			// The command FIS: a register host-to-device frame, 20 bytes of the table's first 64.
			zero(table, CT_PRDT_OFFSET);
			let fis = table;
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
			// THE QUEUED PAIR FILL FOUR OF THESE BYTES DIFFERENTLY, and `ahci::queued_fis` is where
			// that mapping is written down and held by fixtures: the COUNT moves into the features
			// bytes and the sector-count byte carries the TAG.
			if queued {
				let fields = ahci::queued_fis(sectors, tag as u32, false);
				((fis + 3) as *mut u8).write_volatile(fields.features);
				((fis + 11) as *mut u8).write_volatile(fields.features_exp);
				((fis + 12) as *mut u8).write_volatile(fields.count);
				((fis + 7) as *mut u8).write_volatile(fields.device);
			} else {
				((fis + 12) as *mut u8).write_volatile(sectors as u8);
				((fis + 13) as *mut u8).write_volatile((sectors >> 8) as u8);
			}

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
				let entry = table + CT_PRDT_OFFSET + (index as u64) * ahci::PRDT_ENTRY_LEN as u64;
				let span = ahci::prdt_span(bytes, index);
				let at = self.data[tag].phys + (index as u64) * ahci::PRDT_MAX_BYTES as u64;
				w64(entry, at);
				w32(entry + 8, 0);
				// The byte count is ZERO-BASED: writing the length straight in transfers one byte
				// too many on every entry.
				w32(entry + 12, ahci::prdt_count(span));
			}

			// The command header in this tag's slot: the FIS length in dwords, the write bit, and how
			// many scatter-gather entries the table holds.
			let header = self.structures.virt + (tag as u64) * 32;
			let dw0 = 5u32 | if write { 1 << 6 } else { 0 } | ((entries as u32) << 16);
			w32(header, dw0);
			w32(header + 4, 0);
			w64(header + 8, self.table_phys(tag));

			// Clear any error latched from before, or the task file check below reports somebody
			// else's failure as this command's. ONLY WHEN NOTHING ELSE IS OUTSTANDING: a queue with
			// commands in it has a task file that belongs to them, and clearing it under them throws
			// away the error that stopped the queue.
			if r32(self.port + ahci::PORT_SACT) == 0 && r32(self.port + ahci::PORT_CI) == 0 {
				w32(self.port + ahci::PORT_SERR, r32(self.port + ahci::PORT_SERR));
				w32(self.port + ahci::PORT_IS, r32(self.port + ahci::PORT_IS));
			}

			// THE TABLE MUST BE VISIBLE BEFORE THE SLOT IS ISSUED. `write_volatile` stops the
			// compiler reordering these and says nothing about the machine; the same fence the NVMe
			// driver needs before its doorbell is needed here before `PxCI`.
			core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
			if queued {
				w32(self.port + ahci::PORT_SACT, 1 << tag);
			}
			w32(self.port + ahci::PORT_CI, 1 << tag);
			Ok(())
		}
	}

	// Where one tag's command has got to. `queued` picks which register answers, and the two are not
	// interchangeable - see `ahci::queued_outcome`.
	unsafe fn poll(&self, tag: usize, queued: bool) -> Outcome {
		unsafe {
			let tfd = r32(self.port + ahci::PORT_TFD);
			if queued { ahci::queued_outcome(r32(self.port + ahci::PORT_SACT), tag as u32, tfd) } else { ahci::outcome(r32(self.port + ahci::PORT_CI), tag as u32, tfd) }
		}
	}

	// Issue one command in tag zero and wait for it, which is what the paths with no caller to
	// return to need: IDENTIFY at bring-up and the FLUSH a clean stop certifies.
	unsafe fn command(&mut self, ata: u8, lba: u64, sectors: u32, bytes: u64, write: bool) -> Result<(), Fault> {
		unsafe {
			self.issue(0, ata, lba, sectors, bytes, write)?;
			let mut spins: u64 = 0;
			loop {
				match self.poll(0, false) {
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

// Bring one port's link up from nothing: a COMRESET asked for in `PxSCTL.DET`, held, released, and
// waited on. The command engine must already be stopped, which the caller does.
//
// THE HOLD IS A TIME AND NOT A SPIN COUNT. The specification asks for at least a millisecond with
// `DET` at one, and a millisecond is a tenth of a tick here - so this holds for one whole tick,
// which is the smallest unit this system can measure and is safely more than the minimum.
unsafe fn reset_port(port: u64) {
	unsafe {
		// Keep the speed and power-management fields the platform set; only the detection field is
		// this driver's to write.
		let sctl = r32(port + ahci::PORT_SCTL) & !0x0F;
		w32(port + ahci::PORT_SCTL, sctl | 1);
		let mut hold = common::Deadline::ticks(1);
		while hold.waiting() {}
		w32(port + ahci::PORT_SCTL, sctl);
		let mut link = common::Deadline::ticks(RESET_TICKS);
		while link.waiting() {
			if ahci::link_up(r32(port + ahci::PORT_SSTS)) {
				break;
			}
		}
		// THE RESET RECORDS ITS OWN ERRORS, and a port whose `PxSERR` still carries them refuses the
		// first command with a fault that describes the reset rather than the command.
		w32(port + ahci::PORT_SERR, 0xFFFF_FFFF);
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
		report.push(b" bytes");
		// WHETHER THIS CONTROLLER QUEUES IS PART OF WHAT IT IS. Two machines with the same disk and
		// different controllers serve a busy consumer set differently, and an operator reading a
		// slow machine's log should not have to guess which one this is.
		if controller.queued {
			report.push(b", ncq ");
			report.decimal(QUEUED_TAGS as u64);
			report.push(b" tags");
		} else {
			report.push(b", one command at a time");
		}
		report.push(b")");
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
			// THIS DRIVER RESETS THE PORT ITSELF RATHER THAN INHERITING SOMEBODY ELSE'S, and that
			// is the defect only a run on a second machine could find.
			//
			// On x86_64 the firmware enumerates the AHCI controller before this driver ever sees it -
			// the boot log lists a SATA device among its boot entries - so the link was already up
			// and `PxSIG` already latched when the driver read them. On the aarch64 machine the
			// firmware does not touch the controller at all, and the driver read a port whose link
			// had never been brought up: "port 0 carries nothing", with a disk plainly attached and
			// QEMU reporting it. An AHCI driver that only works after somebody else initialised the
			// port is a driver that works on one machine.
			//
			// THE SEQUENCE IS THE SPECIFICATION'S, AND ITS FIRST STEP IS THE ONE THAT IS EASY TO
			// MISS: `PxSIG` IS NOT A REGISTER THE HBA FILLS BY ITSELF. It is copied out of the
			// device's first register FIS, and the HBA can only receive that FIS once the port has a
			// FIS receive AREA and `PxCMD.FRE` is set. A port probed with the receive engine off
			// reports a link that is up and a signature that is zero, for ever - which is exactly
			// what "port 0 carries nothing" was. So the buffers are programmed BEFORE the reset,
			// rather than after a port has been chosen.
			//
			// Then: ask for a COMRESET in `PxSCTL.DET`, hold it, release it, wait for the link,
			// clear the errors the reset itself recorded, and only then believe the signature. Every
			// wait is bounded in the 100 Hz ticks the bind window is measured in.
			w64(port + ahci::PORT_CLB, structures.phys);
			w64(port + ahci::PORT_FB, structures.phys + RECEIVED_FIS_OFFSET);
			w32(port + ahci::PORT_CMD, r32(port + ahci::PORT_CMD) | ahci::CMD_FIS_RECEIVE_ENABLE);
			if !ahci::link_up(r32(port + ahci::PORT_SSTS)) || matches!(ahci::attached(r32(port + ahci::PORT_SIG)), Attached::None) {
				reset_port(port);
			}
			if !ahci::link_up(r32(port + ahci::PORT_SSTS)) {
				continue;
			}
			// AND THE SIGNATURE APPEARS AFTER THE LINK RATHER THAN WITH IT. `PxSIG` is written when
			// the device sends its first register FIS, which is a moment after `PxSSTS.DET` reaches
			// three; a port still busy has not finished presenting itself.
			let mut settle = common::Deadline::ticks(SIGNATURE_TICKS);
			while settle.waiting() {
				let busy = r32(port + ahci::PORT_TFD) & (ahci::TFD_BSY | ahci::TFD_DRQ) != 0;
				if !busy && !matches!(ahci::attached(r32(port + ahci::PORT_SIG)), Attached::None) {
					break;
				}
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
			// the binding unit is a PCI function and a port is not expressible in one. The buffers
			// are already programmed above - the receive engine had to be running before the
			// signature could appear - so what is left is to start the command engine.
			start_port(port);
			chosen = Some(port);
			break;
		}
		let port = chosen.ok_or(Bringup::NoDisk)?;

		// THE TAGS THIS CONTROLLER ACTUALLY HAS. `CAP.NCS` is how many command slots a port carries,
		// and a driver issuing a tag past it writes a header outside the command list.
		let tags = (caps.slots as usize).min(QUEUED_TAGS);
		let mut controller = Controller { device, port, structures, disk: ahci::Disk { sectors: 0, sector_bytes: 512 }, most_bytes: TRANSFER_BOUND, queued: caps.queued && tags > 1, data: [Dma { handle: 0, virt: 0, phys: 0, bytes: 0 }; QUEUED_TAGS] };

		// IDENTIFY DEVICE answers into tag zero's data span, 512 bytes of it.
		if !controller.grow(0, 512) {
			return Err(Bringup::NoDma);
		}
		zero(controller.data[0].virt, 512);
		controller.named(ahci::ATA_IDENTIFY, 0, 0, 512, false).map_err(|_| Bringup::Identify)?;
		let mut words = [0u16; 256];
		for (index, word) in words.iter_mut().enumerate() {
			*word = (controller.data[0].virt as *const u16).add(index).read_volatile();
		}
		controller.disk = ahci::identify(&words).map_err(|_| Bringup::Disk)?;
		Ok(controller)
	}
}

// ONE REQUEST WAITING ON ONE TAG.
#[derive(Clone, Copy)]
struct InFlight {
	// The consumer to answer, or zero for a free tag.
	owner: u64,
	// A read hands back a buffer holding what the disk wrote; a write has already handed its bytes
	// over and only needs a status.
	read: bool,
	bytes: u64,
}

const FREE: InFlight = InFlight { owner: 0, read: false, bytes: 0 };

// What taking one consumer's request did.
enum Took {
	// It is on a tag and waiting for the disk.
	Placed,
	// It was answered without one - a capacity, a flush, a refusal - or there was nothing to take.
	Answered,
	// The consumer has gone and the serving set has closed over the hole, so THIS INDEX NOW HOLDS
	// SOMEBODY ELSE and the caller must not step past it.
	Departed,
}

unsafe fn serve(bootstrap: u64, bind: &common::Bind, controller: &mut Controller, blk_server: u64) -> ! {
	unsafe {
		let mut request = [0u8; block::REQUEST_LEN];
		let mut serving = common::Serving::new(blk_server, 0);
		let mut flight = [FREE; QUEUED_TAGS];
		loop {
			let Some(at) = common::serve_any_or_answer(bootstrap, bind, &mut serving) else {
				let flushed = controller.flush().is_ok();
				if !flushed {
					print(b"driver.ahci: the disk did not complete the flush this stop requires - no clean stop is claimed for it\n");
				}
				common::finish_stop(bootstrap, bind, controller.device, flushed);
				exit();
			};
			take(bootstrap, bind, &mut serving, at, controller, &mut request, &mut flight);

			// AND EVERY OTHER CONSUMER THAT ALREADY HAS A REQUEST WAITING, while tags remain.
			//
			// THIS IS WHAT THE QUEUE IS FOR, and it is the only place concurrency can come from: the
			// block contract is one request and one reply per message, so a single consumer never
			// has two in flight. A disk here has FOUR consumers, and when several of them ask at
			// once a driver with one command slot serves them one after another while the disk sits
			// idle between. `poll_ready` is a wait against the current instant, so a consumer with
			// nothing waiting costs one syscall and is stepped over.
			//
			// A CONTROLLER THAT DOES NOT QUEUE COLLECTS NOTHING, because its second command would
			// have to wait for the first anyway and collecting it early only delays its answer.
			if controller.queued {
				let mut scan = 0usize;
				while scan < serving.as_slice().len() && flight.iter().any(|entry| entry.owner == 0) {
					if !poll_ready(serving.at(scan)) {
						scan += 1;
						continue;
					}
					match take(bootstrap, bind, &mut serving, scan, controller, &mut request, &mut flight) {
						// The set closed over the hole with its last entry, so this index is a
						// different consumer now and stepping past it would skip them.
						Took::Departed => continue,
						_ => scan += 1,
					}
				}
			}
			complete(controller, &mut flight);
		}
	}
}

// Take one consumer's request: answer it here if it needs no disk, or put it on a free tag.
unsafe fn take(bootstrap: u64, bind: &common::Bind, serving: &mut common::Serving, at: usize, controller: &mut Controller, request: &mut [u8; block::REQUEST_LEN], flight: &mut [InFlight; QUEUED_TAGS]) -> Took {
	unsafe {
		let endpoint: u64 = serving.at(at);
		// A CONSUMER THAT CLOSED IS ONE CLIENT LEAVING AND NOT THIS DRIVER'S END. The rule for
		// dropping it and telling the manager is in `recv_from_consumer`, which says why.
		let Some((len, handle)) = common::recv_from_consumer(bootstrap, bind, serving, at, request) else {
			return Took::Departed;
		};
		let Some(block::Request { op, lba, count }) = block::Request::decode(&request[..len]) else {
			if handle != 0 {
				close(handle);
			}
			reply(endpoint, block::STATUS_INVALID, 0);
			return Took::Answered;
		};
		// REFUSED AND NOT CLAMPED, and the disk is not asked.
		if matches!(op, block::OP_READ | block::OP_WRITE) && blk::request_range(lba, count, controller.disk.sectors, controller.most_sectors()).is_err() {
			if handle != 0 {
				close(handle);
			}
			reply(endpoint, block::STATUS_INVALID, 0);
			return Took::Answered;
		}
		match op {
			block::OP_READ => place_read(controller, endpoint, lba, count, flight),
			block::OP_WRITE => place_write(controller, endpoint, lba, count, handle, flight),
			block::OP_CAPACITY => {
				let bytes = controller.disk.sectors * controller.disk.sector_bytes as u64;
				send_blocking(endpoint, &block::capacity_reply(bytes, controller.most_sectors()), 0);
				Took::Answered
			}
			// A FLUSH IS NOT A QUEUED COMMAND AND MUST NOT MEET ONE. The specification forbids
			// mixing queued and non-queued commands on a port, so this waits for whatever is on the
			// tags before it issues - which at this point is nothing, because a batch is completed
			// before the next one is collected.
			block::OP_FLUSH => {
				let ok = controller.flush().is_ok();
				reply(endpoint, if ok { block::STATUS_OK } else { block::STATUS_ERR }, 0);
				Took::Answered
			}
			_ => {
				if handle != 0 {
					close(handle);
				}
				reply(endpoint, block::STATUS_ERR, 0);
				Took::Answered
			}
		}
	}
}

fn free_tag(flight: &[InFlight; QUEUED_TAGS]) -> Option<usize> {
	flight.iter().position(|entry| entry.owner == 0)
}

unsafe fn place_read(controller: &mut Controller, endpoint: u64, lba: u64, count: u32, flight: &mut [InFlight; QUEUED_TAGS]) -> Took {
	unsafe {
		let bytes = count as u64 * controller.disk.sector_bytes as u64;
		let Some(tag) = free_tag(flight) else {
			reply(endpoint, block::STATUS_ERR, 0);
			return Took::Answered;
		};
		let ata = if controller.queued { ahci::ATA_READ_FPDMA_QUEUED } else { ahci::ATA_READ_DMA_EXT };
		if !controller.grow(tag, bytes) || controller.issue(tag, ata, lba, count, bytes, false).is_err() {
			reply(endpoint, block::STATUS_ERR, 0);
			return Took::Answered;
		}
		flight[tag] = InFlight { owner: endpoint, read: true, bytes };
		Took::Placed
	}
}

// THE THREE-PART CONTRACT ON THE TRANSFERRED OBJECT IS CHECKED BEFORE IT IS MAPPED: a memory object,
// readable through this handle, and at least as long as the request says.
unsafe fn place_write(controller: &mut Controller, endpoint: u64, lba: u64, count: u32, handle: u64, flight: &mut [InFlight; QUEUED_TAGS]) -> Took {
	unsafe {
		if handle == 0 {
			reply(endpoint, block::STATUS_INVALID, 0);
			return Took::Answered;
		}
		let bytes = count as u64 * controller.disk.sector_bytes as u64;
		let Some(info) = object_info(handle) else {
			close(handle);
			reply(endpoint, block::STATUS_INVALID, 0);
			return Took::Answered;
		};
		if blk::write_source(&info, bytes).is_err() {
			close(handle);
			reply(endpoint, block::STATUS_INVALID, 0);
			return Took::Answered;
		}
		let Some(tag) = free_tag(flight) else {
			close(handle);
			reply(endpoint, block::STATUS_ERR, 0);
			return Took::Answered;
		};
		if !controller.grow(tag, bytes) {
			close(handle);
			reply(endpoint, block::STATUS_ERR, 0);
			return Took::Answered;
		}
		let Some(mapped) = map_object(handle) else {
			close(handle);
			reply(endpoint, block::STATUS_ERR, 0);
			return Took::Answered;
		};
		// THE BYTES ARE COPIED BEFORE THE COMMAND IS ISSUED, which is what lets the caller's object
		// be unmapped and closed here rather than held until the disk is done with it.
		core::ptr::copy_nonoverlapping(mapped as *const u8, controller.data[tag].virt as *mut u8, bytes as usize);
		unmap_object(handle);
		close(handle);
		let ata = if controller.queued { ahci::ATA_WRITE_FPDMA_QUEUED } else { ahci::ATA_WRITE_DMA_EXT };
		if controller.issue(tag, ata, lba, count, bytes, true).is_err() {
			reply(endpoint, block::STATUS_ERR, 0);
			return Took::Answered;
		}
		flight[tag] = InFlight { owner: endpoint, read: false, bytes };
		Took::Placed
	}
}

// Wait for every tag this batch put on the disk and answer each consumer as its command finishes.
//
// EACH IS ANSWERED WHEN IT FINISHES AND NOT WHEN THE BATCH DOES, which is the difference the queue
// buys: a short read behind a long one does not wait for it.
//
// AND A FAILURE STOPS THE WHOLE QUEUE, which is the half a non-queued driver never has to think
// about. A queued command that fails sets `TFD.ERR` and the port stops accepting, so every other
// outstanding tag is abandoned rather than failed on its own - `queued_outcome` answers `Failed` for
// all of them, which is the truth, and the port is restarted so the next batch has somewhere to go.
unsafe fn complete(controller: &mut Controller, flight: &mut [InFlight; QUEUED_TAGS]) {
	unsafe {
		if flight.iter().all(|entry| entry.owner == 0) {
			return;
		}
		let queued = controller.queued;
		let mut spins: u64 = 0;
		let mut failed = false;
		loop {
			let mut waiting = false;
			for tag in 0..QUEUED_TAGS {
				if flight[tag].owner == 0 {
					continue;
				}
				match controller.poll(tag, queued) {
					Outcome::Pending => waiting = true,
					Outcome::Done => {
						// The data the controller wrote must be visible before it is read out.
						core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
						finish(controller, tag, flight);
					}
					Outcome::Failed { status, error } => {
						let mut line: common::Bounded<128> = common::Bounded::new();
						line.push(b"driver.ahci: queued command on tag ");
						line.decimal(tag as u64);
						line.push(b" failed, status ");
						line.push(&common::hex2(status));
						line.push(b" error ");
						line.push(&common::hex2(error));
						line.push(b"\n");
						print(line.as_bytes());
						reply(flight[tag].owner, block::STATUS_ERR, 0);
						flight[tag] = FREE;
						failed = true;
					}
				}
			}
			if !waiting {
				break;
			}
			spins += 1;
			if spins > COMMAND_SPINS {
				for tag in 0..QUEUED_TAGS {
					if flight[tag].owner != 0 {
						print(b"driver.ahci: a queued command never completed\n");
						reply(flight[tag].owner, block::STATUS_ERR, 0);
						flight[tag] = FREE;
					}
				}
				failed = true;
				break;
			}
		}
		// A PORT THAT STOPPED HAS TO BE STARTED AGAIN, or every later batch times out against a
		// controller that is no longer accepting anything.
		if failed {
			stop_port(controller.port);
			w32(controller.port + ahci::PORT_SERR, r32(controller.port + ahci::PORT_SERR));
			w32(controller.port + ahci::PORT_IS, r32(controller.port + ahci::PORT_IS));
			start_port(controller.port);
		}
	}
}

// Answer one finished tag: a read hands back what the disk wrote, a write only its status.
unsafe fn finish(controller: &mut Controller, tag: usize, flight: &mut [InFlight; QUEUED_TAGS]) {
	unsafe {
		let entry = flight[tag];
		flight[tag] = FREE;
		if !entry.read {
			reply(entry.owner, block::STATUS_OK, 0);
			return;
		}
		let object: u64 = syscall(SYS_MEMORY_OBJECT_CREATE, entry.bytes, 0, 0, 0);
		if sys_is_err(object) {
			reply(entry.owner, block::STATUS_ERR, 0);
			return;
		}
		let Some(mapped) = map_object(object) else {
			close(object);
			reply(entry.owner, block::STATUS_ERR, 0);
			return;
		};
		core::ptr::copy_nonoverlapping(controller.data[tag].virt as *const u8, mapped as *mut u8, entry.bytes as usize);
		unmap_object(object);
		let granted: i64 = duplicate(object, RIGHT_READ | RIGHT_MAP | RIGHT_TRANSFER);
		close(object);
		if granted < 0 {
			reply(entry.owner, block::STATUS_ERR, 0);
			return;
		}
		reply(entry.owner, block::STATUS_OK, granted as u64);
	}
}

fn reply(endpoint: u64, status: u32, transferred: u64) {
	send_blocking(endpoint, &block::reply(status), transferred);
}
