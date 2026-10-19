// driver.nvme - the userspace NVM Express controller driver.
//
// DeviceManager launches this program with a `BIND` naming the controller and a transferred
// DeviceMemory capability to its BAR 0, which for NVMe is the whole register file. The driver maps
// it, takes the controller through its reset and enable handshake, builds an admin queue pair and
// one I/O queue pair, asks the controller and its first namespace to identify themselves, and then
// serves the block wire - the same `driver_protocol::block` that `driver.virtio-blk` and the USB
// mass-storage path serve - for the life of the system.
//
// EVERY DECISION IN IT LIVES IN `drivers::nvme` AND IS HOST-TESTED. What is left here is the part a
// real controller has to be present for: the MMIO, the DMA, the doorbells and the waiting. That
// split is what the roadmap's Definition of Done asks for, because the register layouts, the
// completion rules and the PRP arithmetic are where an NVMe driver is actually wrong and none of
// them need a device to be watched failing.
//
// POLLED, LIKE THE VIRTIO-BLK AND xHCI DRIVERS THIS TREE ALREADY RUNS. One command is outstanding at
// a time and its completion is reaped by spinning on the phase bit. MSI-X is resolved by the kernel
// and handed over, so making this interrupt-driven later is a change to this file alone.

#![no_std]
#![no_main]

use driver_protocol::block;
use drivers::common::Deadline;
use drivers::nvme::{self, Fault, Prp, Reaped};
use drivers::{blk, common};
use rt::*;

// The host page this driver describes DMA in. Every PRP entry below is in these units.
const PAGE: u64 = 4096;

// How many entries each queue gets. ONE COMMAND IS OUTSTANDING AT A TIME, so a deeper queue buys
// nothing and costs pinned DMA frames; the number is here because the controller's own maximum is
// read and clamped against it rather than assumed.
const WANT_ENTRIES: u32 = 8;

// The I/O queue pair's id. Queue 0 is the admin pair, always.
const IO_QUEUE: u16 = 1;

// The largest transfer this driver will move in one request, before the controller's own `MDTS` is
// applied on top. It bounds the growable data span and therefore the DMA this process pins.
const TRANSFER_BOUND: u64 = 128 * 1024;

// How long one I/O command may take before the driver gives up on it, in the 100 Hz ticks of
// `common::Deadline`: thirty seconds, the bound Linux gives an I/O command by default. A REAL BOUND
// AND A TIME, NOT A SPIN COUNT: a controller that never answers must not hold this process and its
// clients for ever, and a count of spins is a different time in every build and on every machine -
// so a slow flush on a busy host was a "timeout" in one build and not in another.
const IO_TICKS: u64 = 3000;

// How long the admin commands of one bring-up or one reset may take, together. A fraction of the
// bind window READY is due in (200 ticks), shared by every command in it, so a bring-up where
// everything times out still finishes in time to say so.
const ADMIN_TICKS: u64 = 150;

// How long a controller is given to finish the shutdown it is told about before a sleep cuts its
// power: inside the five seconds this driver's `suspend-deadline` gives the whole sleep step.
const SHUTDOWN_TICKS: u64 = 200;

// The enable/disable handshake's budget per unit of the controller's own `CAP.TO`, which the
// specification states in 500 ms units - fifty ticks - so this is scaled by what the controller
// reports rather than being a constant this driver invented.
const READY_TICKS_PER_UNIT: u64 = 50;

// Admin command opcodes.
const ADMIN_CREATE_SQ: u8 = 0x01;
const ADMIN_CREATE_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const ADMIN_SET_FEATURES: u8 = 0x09;

// I/O command opcodes.
const IO_FLUSH: u8 = 0x00;
const IO_WRITE: u8 = 0x01;
const IO_READ: u8 = 0x02;

// `IDENTIFY`'s controller-or-namespace selector.
const CNS_NAMESPACE: u32 = 0x00;
const CNS_CONTROLLER: u32 = 0x01;

// `SET FEATURES` feature id 7: how many I/O queues the host wants.
const FEATURE_NUM_QUEUES: u32 = 0x07;

// `CREATE I/O QUEUE` dword 11: the queue is physically contiguous.
const QUEUE_CONTIGUOUS: u32 = 1 << 0;

// The first namespace. THE BINDING UNIT IS A PCI FUNCTION and a namespace is not expressible in it,
// which the roadmap says in as many words, so one controller serves one block provider and the
// namespace is chosen here rather than being something a client names.
const NAMESPACE: u32 = 1;

// `IDENTIFY CONTROLLER` field offsets.
const ID_CTRL_MDTS: u64 = 77;
// `IDENTIFY NAMESPACE` field offsets: size at 0, formatted LBA size at 26, protection settings at
// 29, and the sixteen four-byte LBA format entries from 128.
const ID_NS_FLBAS: u64 = 26;
const ID_NS_DPS: u64 = 29;
const ID_NS_LBAF: u64 = 128;

unsafe fn r8(addr: u64) -> u8 {
	unsafe { (addr as *const u8).read_volatile() }
}
unsafe fn r32(addr: u64) -> u32 {
	unsafe { (addr as *const u32).read_volatile() }
}
unsafe fn w32(addr: u64, v: u32) {
	unsafe { (addr as *mut u32).write_volatile(v) }
}
// A 64-bit controller register, as two 32-bit halves, low first. The specification permits 32-bit
// access to every register and requires the low half first for the queue base addresses, so this is
// the portable form rather than a concession.
unsafe fn r64(addr: u64) -> u64 {
	unsafe { (r32(addr) as u64) | ((r32(addr + 4) as u64) << 32) }
}
unsafe fn w64(addr: u64, v: u64) {
	unsafe {
		w32(addr, v as u32);
		w32(addr + 4, (v >> 32) as u32);
	}
}

// Where bring-up gave up.
//
// EVERY ARM IS A DIFFERENT DEFECT, and the driver prints which one. The first version of this
// function answered `None` from six places and the manager reported "the device is not responding"
// for all of them, which is true of exactly one. A driver that cannot say where it stopped makes its
// own log useless at the moment the log is the only thing there is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bringup {
	// `CAP` says this controller cannot be driven at all.
	Unusable,
	// It never left, or never reached, the ready state.
	NotReady,
	// The DMA for the queues could not be taken.
	NoDma,
	// An admin command failed, timed out, or answered for something else.
	Admin,
	// `IDENTIFY NAMESPACE` described a namespace this driver will not serve.
	Namespace,
	// The I/O queue pair was not created.
	NoIoQueue,
}

impl Bringup {
	fn name(self) -> &'static [u8] {
		match self {
			Bringup::Unusable => b"the controller's CAP does not describe a controller this driver can use",
			Bringup::NotReady => b"the controller did not reach the ready state its own CAP.TO allows for",
			Bringup::NoDma => b"the queue memory could not be taken",
			Bringup::Admin => b"an admin command did not complete",
			Bringup::Namespace => b"namespace 1 is not one this driver serves",
			Bringup::NoIoQueue => b"the I/O queue pair was not created",
		}
	}
}

// One submission/completion pair, and everything needed to ring for it.
struct Pair {
	sq_virt: u64,
	sq_tail: u32,
	cq_virt: u64,
	cq_head: u32,
	// THE PHASE THIS PASS EXPECTS. The ring memory is never cleared, so the entry sitting in a slot
	// is the previous pass's until the controller flips this bit. Nothing else tells them apart.
	phase: bool,
	entries: u32,
	sq_doorbell: u64,
	cq_doorbell: u64,
}

impl Pair {
	// Write one 64-byte submission entry and ring the doorbell.
	unsafe fn submit(&mut self, entry: &[u32; 16]) {
		let slot = self.sq_virt + (self.sq_tail as u64) * nvme::SQ_ENTRY_LEN as u64;
		for (i, dword) in entry.iter().enumerate() {
			unsafe { w32(slot + (i as u64) * 4, *dword) };
		}
		self.sq_tail = (self.sq_tail + 1) % self.entries;
		// THE ENTRY MUST BE VISIBLE BEFORE THE DOORBELL SAYS IT IS THERE. The sixty-four bytes go to
		// write-back DMA memory and the doorbell is an uncached register, and nothing in the
		// language orders one against the other: `write_volatile` stops the COMPILER reordering
		// them and says nothing about the machine. x86's store ordering usually hides that, which is
		// exactly why it is worth a fence - this tree also builds for aarch64 and riscv64, where a
		// controller reading a half-written submission entry is not a hypothetical.
		core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
		unsafe { w32(self.sq_doorbell, self.sq_tail) };
	}

	// Wait for the completion of `command_id`, saying what happened when it is not success.
	//
	// THE OPCODE AND THE CONTROLLER'S OWN STATUS ARE PRINTED, because "an admin command did not
	// complete" names five commands and a status type and code name exactly one thing. A driver that
	// swallows the controller's own answer leaves whoever reads the log guessing at the one fact the
	// controller took the trouble to report.
	unsafe fn wait_named(&mut self, command_id: u16, opcode: u8, deadline: &mut Deadline) -> Result<(), Fault> {
		let outcome = unsafe { self.wait(command_id, deadline) };
		if let Err(fault) = outcome {
			let mut line: common::Bounded<128> = common::Bounded::new();
			line.push(b"driver.nvme: command ");
			line.push(&common::hex2(opcode));
			match fault {
				Fault::Failed { status_type, status_code } => {
					line.push(b" was refused by the controller, status type ");
					line.push(&common::hex2(status_type));
					line.push(b" code ");
					line.push(&common::hex2(status_code));
				}
				Fault::Timeout => line.push(b" never completed"),
				Fault::Down => line.push(b" was not issued - the controller could not be reset"),
				Fault::Unexpected { saw, expected, dw2, dw3 } => {
					// DWORD 2 IS THE FIELD THAT SETTLES IT: bits 31:16 are the SUBMISSION QUEUE this
					// completion claims to be for, and bits 15:0 are how far the controller has
					// consumed that queue. An SQ head of zero means it consumed nothing and this is
					// not an answer to anything; a head of one against queue zero means it consumed
					// the admin entry this driver wrote and reported someone else's id for it.
					line.push(b" got id ");
					line.decimal(saw as u64);
					line.push(b" waiting for ");
					line.decimal(expected as u64);
					line.push(b", sq ");
					line.decimal((dw2 >> 16) as u64);
					line.push(b" head ");
					line.decimal((dw2 & 0xFFFF) as u64);
					line.push(b", dw3 ");
					line.push(&common::hex2((dw3 >> 24) as u8));
					line.push(&common::hex2((dw3 >> 16) as u8));
					line.push(&common::hex2((dw3 >> 8) as u8));
					line.push(&common::hex2(dw3 as u8));
				}
			}
			line.push(b"\n");
			print(line.as_bytes());
		}
		outcome
	}

	// Wait for the completion of `command_id`, until `deadline`.
	unsafe fn wait(&mut self, command_id_wanted: u16, deadline: &mut Deadline) -> Result<(), Fault> {
		loop {
			// THE PHASE IS READ FIRST, THEN A BARRIER, THEN THE REST OF THE ENTRY - and that order
			// is the repair for the defect that made every bring-up a coin toss.
			//
			// WHAT WAS SEEN, after three rounds of teaching this driver to say things: a completion
			// carrying command id 0 with a success status, while the submission entry read back out
			// of queue memory carried the id that was actually sent. The field that settled it was
			// dword 2, whose top half names the submission queue and whose bottom half says how far
			// the controller has consumed it: `sq 0 head 0`, with five commands submitted. A
			// controller that had consumed nothing cannot have been answering anything - so the
			// entry was not a completion at all. It was ZEROES WITH THE PHASE BIT ALREADY SET.
			//
			// A SIXTEEN-BYTE WRITE FROM A DEVICE IS NOT ATOMIC TO THIS READER. The phase bit becomes
			// visible when its part of the entry lands, and the id, the status and the queue head can
			// still be the zeroes the driver put there. Reading the whole entry and then deciding is
			// what lets a half-arrived completion be believed; a barrier BEFORE the read orders
			// nothing about a write that is still in flight while it runs.
			//
			// So: read the phase, and only once it says this entry is this pass's, fence and read the
			// entry again. Every real completion is read twice and every empty slot once, which is
			// the cost of not believing one that has not finished arriving.
			let slot = self.cq_virt + (self.cq_head as u64) * nvme::CQ_ENTRY_LEN as u64;
			let phase_seen = unsafe { r32(slot + 12) } & (1 << 16) != 0;
			if phase_seen != self.phase {
				if !deadline.waiting() {
					return Err(Fault::Timeout);
				}
				continue;
			}
			core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
			let mut entry = [0u8; nvme::CQ_ENTRY_LEN];
			for (i, byte) in entry.iter_mut().enumerate() {
				*byte = unsafe { r8(slot + i as u64) };
			}
			match nvme::reap(&entry, self.phase, command_id_wanted) {
				// AN ENTRY WHOSE PHASE ARRIVED BEFORE THE REST OF IT. `drivers::nvme` owns that rule
				// now, where a host test can watch it: it was written here, in the one place no test
				// could reach, and it is the rule that ended an intermittent bring-up.
				Reaped::Arriving => {
					if !deadline.waiting() {
						return Err(Fault::Timeout);
					}
				}
				Reaped::Empty => {
					if !deadline.waiting() {
						return Err(Fault::Timeout);
					}
				}
				Reaped::Unexpected { command_id } => {
					let mut dw2 = [0u8; 4];
					dw2.copy_from_slice(&entry[8..12]);
					let mut dw3 = [0u8; 4];
					dw3.copy_from_slice(&entry[12..16]);
					return Err(Fault::Unexpected { saw: command_id, expected: command_id_wanted, dw2: u32::from_le_bytes(dw2), dw3: u32::from_le_bytes(dw3) });
				}
				Reaped::Mine { succeeded, .. } => {
					let decoded = nvme::Completion::decode(&entry);
					// THE HEAD MOVES AND THE PHASE FLIPS ON THE WRAP, in that order, and the
					// doorbell is rung whether the command succeeded or failed: the entry has been
					// consumed either way, and a controller whose completion queue is never
					// acknowledged stops after `entries` commands.
					self.cq_head += 1;
					if self.cq_head == self.entries {
						self.cq_head = 0;
						self.phase = !self.phase;
					}
					// The head is published only once this entry has been read out of the ring: the
					// doorbell tells the controller the slot is free to overwrite.
					core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
					unsafe { w32(self.cq_doorbell, self.cq_head) };
					return if succeeded { Ok(()) } else { Err(Fault::Failed { status_type: decoded.status_type, status_code: decoded.status_code }) };
				}
			}
		}
	}
}

// A contiguous DMA region this driver owns. `handle` is zero for a carved-out slice of a larger
// region, which does not own the handle and must not release it.
struct Dma {
	handle: u64,
	virt: u64,
	phys: u64,
	bytes: u64,
}

// Take a DMA region of `bytes`, naming the device it is for.
//
// NAMING THE DEVICE IS NOT OPTIONAL. If this process dies still holding the buffer, the kernel keeps
// those frames out of circulation until somebody resets the controller, rather than handing them to
// whoever allocates next while a PRP entry still points at them. There is no IOMMU on every profile,
// so nothing else stops that write.
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

// Zero a mapped region before the controller is told about it.
//
// A QUEUE WHOSE MEMORY STILL HOLDS WHATEVER WAS THERE has completion entries in it whose phase bit
// may already match, and the first `wait` would read one as the answer to a command that has not
// been sent.
unsafe fn zero(virt: u64, bytes: u64) {
	unsafe { core::ptr::write_bytes(virt as *mut u8, 0, bytes as usize) };
}

// The controller, once it is up.
struct Controller {
	base: u64,
	device: u64,
	caps: nvme::Capabilities,
	// How many entries each of the four queues has.
	entries: u32,
	admin: Pair,
	io: Pair,
	// THE FOUR QUEUE PAGES - admin submission and completion, then I/O submission and completion -
	// contiguous and in that order, so a reset empties them and describes them to the controller
	// again from the same memory.
	queues: Dma,
	// The next command id, monotonic so that a late completion from a timed-out command cannot be
	// mistaken for the next one's answer.
	next_id: u16,
	// The largest single transfer: `MDTS` against this driver's own bound.
	most_bytes: u64,
	namespace: nvme::Namespace,
	// The scratch page identify answers into, and the one page of PRP list a transfer of more than
	// two pages needs.
	scratch: Dma,
	prp_list: Dma,
	// The growable data span one request moves through.
	data: Dma,
	// A reset that did not bring the controller back: nothing is issued until one does.
	down: bool,
}

// One command on the I/O queue, as `Controller::io` issues it - and, after a reset, issues it again.
#[derive(Clone, Copy)]
enum Io {
	Transfer { opcode: u8, lba: u64, blocks: u32, bytes: u64 },
	Flush,
}

// THE DRIVER'S CONTROLLER, AS ITS RECOVERY SEES IT. Its registers and queues are its own for the life of the binding,
// which is what makes issuing and resetting through them sound from here.
impl nvme::Recover for Controller {
	type Io = Io;

	fn issue(&mut self, io: Io) -> Result<(), Fault> {
		// SAFETY: the controller's registers and queue memory are this binding's, mapped for its life.
		unsafe { Controller::issue(self, io) }
	}

	fn reset(&mut self) -> bool {
		// SAFETY: as above.
		unsafe { Controller::reset(self, false) }
	}

	fn down(&self) -> bool {
		self.down
	}

	fn say(&mut self, line: &[u8]) {
		print(line);
	}
}

impl Controller {
	// ZERO IS NEVER ISSUED, and that is a rule this driver keeps rather than a fact that happens to
	// hold. It makes "command id 0" mean exactly one thing on the completion side: an entry that has
	// not finished arriving. Without it, a half-written entry whose phase bit landed first is
	// indistinguishable from a completion for a command somebody else sent, and those want opposite
	// responses - wait, or give up.
	fn command_id(&mut self) -> u16 {
		self.next_id = self.next_id.wrapping_add(1);
		if self.next_id == nvme::NEVER_ISSUED {
			self.next_id = nvme::NEVER_ISSUED.wrapping_add(1);
		}
		self.next_id
	}

	// How many blocks one request to this controller may carry.
	fn most_blocks(&self) -> u64 {
		(self.most_bytes / self.namespace.block_bytes as u64).max(1)
	}

	// Make the data span at least `bytes` long, reallocating when it is not.
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

	// From disabled to enabled with an empty admin pair, over the queue memory this driver already
	// holds - the part of a bring-up a reset has to do again before the I/O pair.
	unsafe fn enable(&mut self) -> Result<(), Bringup> {
		unsafe {
			// A QUEUE WHOSE MEMORY STILL HOLDS THE LAST PASS has completion entries whose phase bit may
			// already match, so it is emptied and both pairs start over - every time, not only the first.
			zero(self.queues.virt, self.queues.bytes);
			self.admin = ring(self.base, &self.caps, &self.queues, self.entries, 0);
			self.io = ring(self.base, &self.caps, &self.queues, self.entries, IO_QUEUE);
			// AQA carries both sizes ZERO-BASED, which is the same trap `CAP.MQES` sets and is why the
			// decoded `entries` is the real count everywhere else in this file.
			let entries = self.entries;
			w32(self.base + nvme::REG_AQA, ((entries - 1) << 16) | (entries - 1));
			w64(self.base + nvme::REG_ASQ, self.queues.phys);
			w64(self.base + nvme::REG_ACQ, self.queues.phys + PAGE);
			// CC: the two entry sizes as powers of two, the NVM command set (zero), the host page size
			// (zero, meaning 4 kB, which `Capabilities::decode` already checked this host can describe),
			// no shutdown notification, and enable.
			w32(self.base + nvme::REG_CC, (nvme::CQ_ENTRY_SHIFT << 20) | (nvme::SQ_ENTRY_SHIFT << 16) | nvme::CC_ENABLE);
			if !wait_ready(self.base, true, self.caps.timeout_500ms) {
				return Err(Bringup::NotReady);
			}

			// THE DOORBELLS ARE SET TO ZERO BEFORE THE FIRST COMMAND, and this is the repair for the
			// defect that made every bring-up a coin toss.
			//
			// WHAT WAS SEEN: the first admin command was submitted with command id 1 and opcode 6, read
			// back out of the queue memory to prove the write had landed, and the completion that came
			// back carried command id 0 with a SUCCESS status. A zeroed submission entry is exactly that
			// command - opcode 0x00 is FLUSH and its command id is 0 - so the controller had fetched a
			// slot this driver never wrote. It does that when its tail doorbell still holds a value from
			// before: on enable it believes there are entries queued up to that tail and consumes them,
			// and the entries are the zeroed page.
			//
			// A DRIVER MAY NOT ASSUME THE RESET CLEARED THEM. Writing its own view - tail zero, head
			// zero - onto a queue it has just created costs two stores and cannot be wrong: the queue IS
			// empty at that moment, so zero is what both ends should believe.
			self.zero_doorbells(0);
			Ok(())
		}
	}

	unsafe fn zero_doorbells(&self, queue: u16) {
		unsafe {
			w32(self.base + self.caps.doorbell(queue, false), 0);
			w32(self.base + self.caps.doorbell(queue, true), 0);
		}
	}

	// One admin command and its completion, inside the bring-up's or the reset's shared deadline.
	unsafe fn admin(&mut self, opcode: u8, namespace: u32, prp1: u64, cdw: [u32; 6], deadline: &mut Deadline) -> Result<(), Fault> {
		let id = self.command_id();
		unsafe {
			self.admin.submit(&command(opcode, id, namespace, prp1, 0, cdw));
			self.admin.wait_named(id, opcode, deadline)
		}
	}

	// IDENTIFY CONTROLLER, for `MDTS`, and IDENTIFY NAMESPACE, for the size, the formatted LBA and
	// the protection settings: what this controller can move in one command, and what it serves.
	unsafe fn identify(&mut self, deadline: &mut Deadline) -> Result<(u64, nvme::Namespace), Bringup> {
		unsafe {
			let (phys, virt) = (self.scratch.phys, self.scratch.virt);
			zero(virt, PAGE);
			self.admin(ADMIN_IDENTIFY, 0, phys, [CNS_CONTROLLER, 0, 0, 0, 0, 0], deadline).map_err(|_| Bringup::Admin)?;
			let most_bytes = nvme::max_transfer(r8(virt + ID_CTRL_MDTS), self.caps.min_page, TRANSFER_BOUND);
			zero(virt, PAGE);
			self.admin(ADMIN_IDENTIFY, NAMESPACE, phys, [CNS_NAMESPACE, 0, 0, 0, 0, 0], deadline).map_err(|_| Bringup::Admin)?;
			let blocks = r64(virt);
			let flbas = r8(virt + ID_NS_FLBAS);
			let dps = r8(virt + ID_NS_DPS);
			let index = nvme::lba_format_index(flbas).ok_or(Bringup::Namespace)?;
			let lbaf = r32(virt + ID_NS_LBAF + (index as u64) * 4);
			let namespace = nvme::namespace(blocks, flbas, lbaf, dps).map_err(|_| Bringup::Namespace)?;
			Ok((most_bytes, namespace))
		}
	}

	// THE I/O PAIR, made on an enabled controller.
	unsafe fn open_io(&mut self, deadline: &mut Deadline) -> Result<(), Bringup> {
		unsafe {
			// ASK FOR ONE I/O QUEUE PAIR. Both counts in dword 11 are zero-based, so zero asks for one
			// of each. The controller's answer may be fewer than asked for; one is the fewest this
			// driver can work with, and the create commands below fail against a controller granting
			// none rather than this driver assuming it got what it asked for.
			self.admin(ADMIN_SET_FEATURES, 0, 0, [FEATURE_NUM_QUEUES, 0, 0, 0, 0, 0], deadline).map_err(|_| Bringup::NoIoQueue)?;
			// THE COMPLETION QUEUE FIRST, because the submission queue names it. The other order is
			// refused by the controller, which is the good case; the bad case is a driver that ignores
			// the status and then waits forever on a queue that was never made.
			let size = (IO_QUEUE as u32) | ((self.entries - 1) << 16);
			let (io_sq, io_cq) = (self.queues.phys + 2 * PAGE, self.queues.phys + 3 * PAGE);
			self.admin(ADMIN_CREATE_CQ, 0, io_cq, [size, QUEUE_CONTIGUOUS, 0, 0, 0, 0], deadline).map_err(|_| Bringup::NoIoQueue)?;
			let reports_to = (IO_QUEUE as u32) << 16;
			self.admin(ADMIN_CREATE_SQ, 0, io_sq, [size, QUEUE_CONTIGUOUS | reports_to, 0, 0, 0, 0], deadline).map_err(|_| Bringup::NoIoQueue)?;
			// The same for the pair that was just created, for the same reason.
			self.zero_doorbells(IO_QUEUE);
			Ok(())
		}
	}

	// RESET THE CONTROLLER AND MAKE ITS QUEUES AGAIN, over the same memory, keeping the binding, its
	// provider and every consumer's connection - the system volume a StorageService mounted over that
	// provider among them. A timed-out command and a resume after lost power both need exactly this:
	// the first because a reset is the only thing that ends a command the controller may still be
	// running, the second because a controller that lost its power has no queues at all.
	//
	// `same_disk` identifies the controller and namespace 1 again and refuses what is not the disk
	// this driver has been serving: a resume must find THE disk its consumers were using, with the
	// size and transfer limit they were told, not merely a disk.
	unsafe fn reset(&mut self, same_disk: bool) -> bool {
		unsafe {
			let mut deadline = Deadline::ticks(ADMIN_TICKS);
			let back = disable(self.base, &self.caps) && self.enable().is_ok() && (!same_disk || self.identify(&mut deadline).is_ok_and(|(most_bytes, namespace)| most_bytes == self.most_bytes && namespace == self.namespace)) && self.open_io(&mut deadline).is_ok();
			self.down = !back;
			back
		}
	}

	// Describe the data span to the controller and issue one namespace command over it.
	//
	// THE PRP PLAN COMES FROM THE TESTED FUNCTION and the only thing done here is writing the list
	// page it may ask for. A driver that computed the page count inline would be one page short for
	// every buffer that does not start on a page boundary.
	unsafe fn transfer(&mut self, opcode: u8, lba: u64, blocks: u32, bytes: u64) -> Result<(), Fault> {
		let plan = match nvme::prp(self.data.phys, bytes, PAGE, self.most_bytes) {
			Ok(plan) => plan,
			Err(_) => return Err(Fault::Failed { status_type: 0xFF, status_code: 0xFF }),
		};
		let (prp1, prp2) = match plan {
			Prp::Single { prp1 } => (prp1, 0),
			Prp::Pair { prp1, prp2 } => (prp1, prp2),
			Prp::List { prp1, entries } => {
				for index in 0..entries {
					let address = nvme::list_entry(self.data.phys, PAGE, index);
					unsafe { w64(self.prp_list.virt + (index as u64) * 8, address) };
				}
				(prp1, self.prp_list.phys)
			}
		};
		// dword 10 and 11 are the starting LBA; dword 12's low sixteen bits are the block count,
		// ZERO-BASED like every other count in this specification.
		let cdw = [lba as u32, (lba >> 32) as u32, blocks - 1, 0, 0, 0];
		let id = self.command_id();
		unsafe {
			self.io.submit(&command(opcode, id, NAMESPACE, prp1, prp2, cdw));
			self.io.wait_named(id, opcode, &mut Deadline::ticks(IO_TICKS))
		}
	}

	// Issue one I/O command, once.
	unsafe fn issue(&mut self, io: Io) -> Result<(), Fault> {
		unsafe {
			match io {
				Io::Transfer { opcode, lba, blocks, bytes } => self.transfer(opcode, lba, blocks, bytes),
				Io::Flush => {
					let id = self.command_id();
					self.io.submit(&command(IO_FLUSH, id, NAMESPACE, 0, 0, [0; 6]));
					self.io.wait_named(id, IO_FLUSH, &mut Deadline::ticks(IO_TICKS))
				}
			}
		}
	}

	// ONE I/O COMMAND, RECOVERED - by `nvme::recovered`, which the host tests drive against a model of a controller that
	// stops answering, gives up its reset, or comes back.
	unsafe fn io(&mut self, io: Io) -> Result<(), Fault> {
		nvme::recovered(self, io)
	}

	// Every write this controller has taken must reach the medium.
	unsafe fn flush(&mut self) -> Result<(), Fault> {
		unsafe { self.io(Io::Flush) }
	}

	// TELL THE CONTROLLER ITS POWER IS GOING - a normal shutdown notification - and wait for it to
	// finish, which settles its own state and stops its DMA. One that does not confirm the shutdown is
	// reset instead, which stops its DMA too; a controller that does neither is not left for a sleep.
	unsafe fn shut_down(&self) -> bool {
		unsafe {
			let cc = r32(self.base + nvme::REG_CC);
			w32(self.base + nvme::REG_CC, (cc & !nvme::CC_SHUTDOWN_MASK) | nvme::CC_SHUTDOWN_NORMAL);
			let mut deadline = Deadline::ticks(SHUTDOWN_TICKS);
			while deadline.waiting() {
				if nvme::shutdown_complete(r32(self.base + nvme::REG_CSTS)) {
					return true;
				}
			}
			print(b"driver.nvme: the controller did not confirm its shutdown - it is reset instead\n");
			disable(self.base, &self.caps)
		}
	}
}

// The pair for `queue` - 0, the admin pair, or `IO_QUEUE` - over its two pages of the queue memory,
// as a fresh ring.
fn ring(base: u64, caps: &nvme::Capabilities, queues: &Dma, entries: u32, queue: u16) -> Pair {
	let first = if queue == 0 { 0 } else { 2 };
	Pair {
		sq_virt: queues.virt + first * PAGE,
		sq_tail: 0,
		cq_virt: queues.virt + (first + 1) * PAGE,
		cq_head: 0,
		// A FRESH RING EXPECTS PHASE ONE: the memory was zeroed and the controller sets the bit on the
		// first entry it writes.
		phase: true,
		entries,
		sq_doorbell: base + caps.doorbell(queue, false),
		cq_doorbell: base + caps.doorbell(queue, true),
	}
}

// TAKE THE CONTROLLER DOWN: `CC.EN` cleared and `CSTS.RDY` waited out. That is the controller
// reset, and it ends every command the controller was running - so once this answers true, nothing
// it was given can still move a byte.
unsafe fn disable(base: u64, caps: &nvme::Capabilities) -> bool {
	unsafe {
		let cc = r32(base + nvme::REG_CC);
		if cc & nvme::CC_ENABLE != 0 {
			w32(base + nvme::REG_CC, cc & !nvme::CC_ENABLE);
		}
		wait_ready(base, false, caps.timeout_500ms)
	}
}

// One command, as the sixteen dwords the specification lays out: opcode and command id in dword 0,
// namespace in dword 1, the PRP pair in dwords 6 to 9, and the command-specific dwords from 10.
fn command(opcode: u8, command_id: u16, namespace: u32, prp1: u64, prp2: u64, cdw: [u32; 6]) -> [u32; 16] {
	let mut out = [0u32; 16];
	out[0] = (opcode as u32) | ((command_id as u32) << 16);
	out[1] = namespace;
	out[6] = prp1 as u32;
	out[7] = (prp1 >> 32) as u32;
	out[8] = prp2 as u32;
	out[9] = (prp2 >> 32) as u32;
	out[10..16].copy_from_slice(&cdw);
	out
}

// Wait for `CSTS.RDY` to reach `want`, or give up.
//
// `CSTS.CFS` IS WATCHED ALONGSIDE IT. A bring-up that only waits for READY waits out the whole
// timeout against a controller that has already reported a fatal fault and will never be ready.
// A RESET IS NOT REFUSED FOR IT: clearing `CC.EN` is what a controller in that state is owed, and
// the wait for `RDY` to fall is what says whether the reset took.
unsafe fn wait_ready(base: u64, want: bool, timeout_500ms: u32) -> bool {
	let mut deadline = Deadline::ticks(READY_TICKS_PER_UNIT * (timeout_500ms.max(1) as u64));
	loop {
		let csts = unsafe { r32(base + nvme::REG_CSTS) };
		if want && csts & nvme::CSTS_FATAL != 0 {
			return false;
		}
		if (csts & nvme::CSTS_READY != 0) == want {
			return true;
		}
		if !deadline.waiting() {
			return false;
		}
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
				// SAID BEFORE THE FAILURE IS REPORTED, because the manager's code is a category and
				// this is the sentence.
				let mut line: common::Bounded<160> = common::Bounded::new();
				line.push(b"driver.nvme: bring-up gave up at ");
				line.push(&common::hex2(bind.info.bus));
				line.push(b":");
				line.push(&common::hex2(bind.info.dev));
				line.push(b".");
				line.push(&[b'0' + (bind.info.func % 10)]);
				line.push(b" - ");
				line.push(why.name());
				line.push(b"\n");
				print(line.as_bytes());
				common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
			}
		};
		let (blk_server, blk_client): (u64, u64) = match channel() {
			Some(pair) => pair,
			None => common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable),
		};
		// ADDRESSED, like every other driver's report: a machine may carry more than one controller
		// and two identical online lines name neither of them.
		let mut report: common::Bounded<96> = common::Bounded::new();
		report.push(b"driver.nvme: online (");
		report.push(&common::hex2(bind.info.bus));
		report.push(b":");
		report.push(&common::hex2(bind.info.dev));
		report.push(b".");
		report.push(&[b'0' + (bind.info.func % 10)]);
		report.push(b", ");
		report.decimal(controller.namespace.blocks);
		report.push(b" x ");
		report.decimal(controller.namespace.block_bytes as u64);
		report.push(b" bytes)");
		common::online(bootstrap, &bind, report.as_bytes(), &[(driver_protocol::provider::BLOCK, blk_client)]);
		serve(bootstrap, &bind, &mut controller, blk_server)
	}
}

// Take the controller from whatever state it was left in to one with an admin pair, an I/O pair and
// an identified namespace.
unsafe fn bring_up(base: u64, device: u64) -> Result<Controller, Bringup> {
	unsafe {
		let caps = nvme::Capabilities::decode(r64(base + nvme::REG_CAP), PAGE).map_err(|_| Bringup::Unusable)?;
		// DISABLE FIRST, WHATEVER THE CONTROLLER WAS DOING. It may have been left enabled by the
		// firmware or by a driver that died, in which case its old queues are still live and its old
		// DMA frames are still being written into.
		if !disable(base, &caps) {
			return Err(Bringup::NotReady);
		}
		// "I HAVE RESET THIS DEVICE", which releases the DMA frames the kernel is holding for the
		// driver that died before this one. Straight after the reset, and not later.
		device_quiesced(device);

		let entries = caps.queue_entries(WANT_ENTRIES);
		// Six pages: an admin pair, an I/O pair, a scratch page for identify, and one PRP list page.
		// Contiguous, so every virtual offset is also a physical one.
		let region = dma(device, 6 * PAGE).ok_or(Bringup::NoDma)?;
		zero(region.virt, region.bytes);
		let at = |n: u64| Dma { handle: 0, virt: region.virt + n * PAGE, phys: region.phys + n * PAGE, bytes: PAGE };
		let queues = Dma { handle: 0, virt: region.virt, phys: region.phys, bytes: 4 * PAGE };
		let admin = ring(base, &caps, &queues, entries, 0);
		let io = ring(base, &caps, &queues, entries, IO_QUEUE);
		let mut controller = Controller { base, device, caps, entries, admin, io, queues, next_id: 0, most_bytes: TRANSFER_BOUND, namespace: nvme::Namespace { blocks: 0, block_bytes: 0 }, scratch: at(4), prp_list: at(5), data: Dma { handle: 0, virt: 0, phys: 0, bytes: 0 }, down: false };
		controller.enable()?;
		let mut deadline = Deadline::ticks(ADMIN_TICKS);
		(controller.most_bytes, controller.namespace) = controller.identify(&mut deadline)?;
		controller.open_io(&mut deadline)?;
		Ok(controller)
	}
}

// THE SLEEP, between two requests: the controller's volatile write cache is flushed, so a battery that dies in the
// sleep loses nothing this driver accepted, and for a state that cuts the power the controller is told so and finishes
// its shutdown - which stops its DMA into memory about to stop being refreshed. ITS RESUME, after lost power or after
// that shutdown, resets the controller and makes its queues again over the same memory, and checks it is still the same
// disk: under the same binding, with every consumer's connection kept, so a system volume a StorageService mounted over
// this provider is still mounted after the sleep. Only a controller that does not come back is bound again.
struct Sleep<'a> {
	controller: &'a mut Controller,
	serving: &'a mut common::Serving,
	// Whether this step shut the controller down, which a resume that lost no power must undo all the same.
	shut_down: bool,
}

impl common::SleepStep for Sleep<'_> {
	fn suspend(&mut self, request: &driver_protocol::SuspendRequest) -> driver_protocol::Suspended {
		let refused = driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Refused(driver_protocol::DriverFailureCode::DeviceNotResponding), awake_by_ms: 0 };
		if unsafe { self.controller.flush() }.is_err() {
			print(b"driver.nvme: the sleep is refused - the controller did not complete the flush it requires\n");
			return refused;
		}
		if request.state.loses_power() {
			if !unsafe { self.controller.shut_down() } {
				print(b"driver.nvme: the sleep is refused - the controller neither finished its shutdown nor stopped\n");
				return refused;
			}
			self.shut_down = true;
		}
		driver_protocol::Suspended { outcome: driver_protocol::SuspendOutcome::Done, awake_by_ms: 0 }
	}

	fn resume(&mut self, lost_power: bool) -> bool {
		let reset = lost_power || core::mem::take(&mut self.shut_down);
		if !reset {
			return true;
		}
		let back = unsafe { self.controller.reset(true) };
		print(if back { b"driver.nvme: the controller is back from the sleep - reset, its queues made again, the same disk\n" } else { b"driver.nvme: the controller did not come back from the sleep\n" });
		back
	}

	fn serving(&mut self) -> Option<&mut common::Serving> {
		Some(self.serving)
	}
}

// Serve the block wire until the manager stops this driver.
//
// THE SAME SHAPE AS `driver.virtio-blk`, deliberately: every consumer of this disk gets its own
// endpoint and they accumulate in `Serving`, the manager's ping is answered by this loop rather than
// by a second one, and a stop flushes before it certifies anything.
unsafe fn serve(bootstrap: u64, bind: &common::Bind, controller: &mut Controller, blk_server: u64) -> ! {
	unsafe {
		let mut request = [0u8; block::REQUEST_LEN];
		// TOKEN ZERO, because `online` names its offers by their position in its own list and this
		// driver publishes exactly one. The token is what a `DISCONNECT` names.
		let mut serving = common::Serving::new(blk_server, 0);
		common::takes_sleep();
		loop {
			let ready = common::serve_any_or_sleep(bootstrap, bind, &mut serving);
			if let Some(None) = ready {
				if common::take_sleep_step(bootstrap, bind, &mut Sleep { controller: &mut *controller, serving: &mut serving, shut_down: false }) {
					continue;
				}
			}
			let Some(Some(at)) = ready.filter(|_| !common::stop_requested()) else {
				// THE FLUSH IS WHAT `STOPPED` CERTIFIES. A controller with writes still in its cache
				// that reported a clean stop would be making a certificate about something that has
				// not happened, so the flush's own answer is part of it.
				let flushed = controller.flush().is_ok();
				if !flushed {
					print(b"driver.nvme: the controller did not complete the flush this stop requires - no clean stop is claimed for it\n");
				}
				// AND THE CONTROLLER IS RESET, which is what makes the stop QUIET. `finish_stop` claims this
				// device masters nothing any more, and a controller left enabled still owns its queues and
				// may still write into them.
				let stopped = disable(controller.base, &controller.caps);
				if !stopped {
					print(b"driver.nvme: the controller did not confirm its reset - no clean stop is claimed for it\n");
				}
				common::finish_stop(bootstrap, bind, controller.device, flushed && stopped);
				exit();
			};
			let endpoint: u64 = serving.at(at);
			// A CONSUMER THAT CLOSED IS ONE CLIENT LEAVING AND NOT THIS DRIVER'S END. The rule for
			// dropping it and telling the manager is in `recv_from_consumer`, which says why.
			let Some((len, handle)) = common::recv_from_consumer(bootstrap, bind, &mut serving, at, &mut request) else {
				continue;
			};
			let Some(block::Request { op, lba, count }) = block::Request::decode(&request[..len]) else {
				if handle != 0 {
					close(handle);
				}
				reply(endpoint, block::STATUS_INVALID, 0);
				continue;
			};
			// REFUSED AND NOT CLAMPED, and the controller is not asked. A clamp turns a wrong
			// request into a wrong WRITE: the caller believes its bytes landed where it said.
			if matches!(op, block::OP_READ | block::OP_WRITE) && blk::request_range(lba, count, controller.namespace.blocks, controller.most_blocks()).is_err() {
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
					let bytes = controller.namespace.blocks * controller.namespace.block_bytes as u64;
					send_blocking(endpoint, &block::capacity_reply(bytes, controller.most_blocks(), block::DeviceClass::Nvme), 0);
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

// A read: move the blocks into the data span, then hand the caller a memory object holding them.
unsafe fn serve_read(controller: &mut Controller, endpoint: u64, lba: u64, count: u32) {
	unsafe {
		let bytes = count as u64 * controller.namespace.block_bytes as u64;
		if !controller.grow(bytes) || controller.io(Io::Transfer { opcode: IO_READ, lba, blocks: count, bytes }).is_err() {
			reply(endpoint, block::STATUS_ERR, 0);
			return;
		}
		// The caller's copy is its own object: the DMA span stays this driver's, because a span the
		// controller writes into is not something to hand across a channel.
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

// A write: the caller's object is read into the data span and then written to the medium.
//
// THE THREE-PART CONTRACT ON THAT OBJECT IS CHECKED BEFORE IT IS MAPPED. It must be a memory object,
// readable through this handle, and at least as long as the request says - a driver that computed
// `count * block` and copied that many bytes out of whatever it was handed would read far past a
// short object, which is the defect `drivers::blk::write_source` exists to refuse.
unsafe fn serve_write(controller: &mut Controller, endpoint: u64, lba: u64, count: u32, handle: u64) {
	unsafe {
		if handle == 0 {
			reply(endpoint, block::STATUS_INVALID, 0);
			return;
		}
		let bytes = count as u64 * controller.namespace.block_bytes as u64;
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
		let ok = controller.io(Io::Transfer { opcode: IO_WRITE, lba, blocks: count, bytes }).is_ok();
		reply(endpoint, if ok { block::STATUS_OK } else { block::STATUS_ERR }, 0);
	}
}
