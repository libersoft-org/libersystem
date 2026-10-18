// driver.ohci - an OHCI USB 1.1 host controller, the controller engine under the shared USB core.
//
// WHAT IS OHCI'S AND NOTHING ELSE: three lists of endpoint descriptors the controller walks - interrupt from the
// table in its communications area, control and bulk from its head registers - one descriptor per pipe with its
// transfer descriptors queued from its head to a dummy at its tail, the data toggle carried in the endpoint
// descriptor, and a root hub that raises its status-change interrupt when a device comes or goes. Everything USB is
// the shared core's - `drivers::usb_host` - and the class modules under it are the ones xHCI's driver runs.
//
// ONE TRANSFER DESCRIPTOR CARRIES A STAGE, and a stage is at most two pages - the descriptor's own limit - so a bulk
// transfer runs stage by stage, each to its end, and a short packet ends the transfer where it happened rather than
// letting the next stage read the device's status into the data. A control transfer is one queue - setup, data,
// status - since a short data stage there is followed by the status stage, which is what buffer rounding does.
//
// ITS REGISTERS ARE BAR 0, its descriptors and data below 4 GiB, and its interrupt the function's INTx on a line other
// functions share: every wake reads and clears the status before the line is acknowledged.
//
// DEVELOPMENT-ONLY, AND THAT IS A DECISION: the final compatibility wave, not enabled by default until a target shows
// it needs it - see the manifest.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};
use drivers::usb_host::{self, Controller, Dma, Done, Endpoint, Kind, Pipe, Pool, Port, Speed};
use drivers::{common, keys, usb_input};
use rt::*;

// The operational registers.
const HC_CONTROL: u64 = 0x04;
const HC_COMMAND_STATUS: u64 = 0x08;
const HC_INTERRUPT_STATUS: u64 = 0x0c;
const HC_INTERRUPT_ENABLE: u64 = 0x10;
const HC_INTERRUPT_DISABLE: u64 = 0x14;
const HC_HCCA: u64 = 0x18;
const HC_CONTROL_HEAD: u64 = 0x20;
const HC_CONTROL_CURRENT: u64 = 0x24;
const HC_BULK_HEAD: u64 = 0x28;
const HC_BULK_CURRENT: u64 = 0x2c;
const HC_FM_INTERVAL: u64 = 0x34;
const HC_FM_NUMBER: u64 = 0x3c;
const HC_PERIODIC_START: u64 = 0x40;
const HC_LS_THRESHOLD: u64 = 0x44;
const HC_RH_DESCRIPTOR_A: u64 = 0x48;
const HC_RH_STATUS: u64 = 0x50;
const HC_RH_PORT: u64 = 0x54;

const CONTROL_CBSR: u32 = 3;
const CONTROL_PLE: u32 = 1 << 2;
const CONTROL_CLE: u32 = 1 << 4;
const CONTROL_BLE: u32 = 1 << 5;
const CONTROL_OPERATIONAL: u32 = 2 << 6;
const CONTROL_HCFS: u32 = 3 << 6;
const CONTROL_IR: u32 = 1 << 8;
const COMMAND_HCR: u32 = 1 << 0;
const COMMAND_CLF: u32 = 1 << 1;
const COMMAND_BLF: u32 = 1 << 2;
const COMMAND_OCR: u32 = 1 << 3;
const INT_SO: u32 = 1 << 0;
const INT_WDH: u32 = 1 << 1;
const INT_UE: u32 = 1 << 4;
const INT_RHSC: u32 = 1 << 6;
const INT_MIE: u32 = 1 << 31;
const INT_ALL: u32 = 0x7f | 1 << 30;
const RHA_PSM: u32 = 1 << 8;
const RHA_NPS: u32 = 1 << 9;
const RH_LPSC: u32 = 1 << 16;

const PORT_CCS: u32 = 1 << 0;
const PORT_PES: u32 = 1 << 1;
const PORT_PRS: u32 = 1 << 4;
const PORT_PPS: u32 = 1 << 8;
const PORT_LSDA: u32 = 1 << 9;
const PORT_CSC: u32 = 1 << 16;
const PORT_PRSC: u32 = 1 << 20;
const PORT_CHANGES: u32 = 0x1f << 16;

// An endpoint descriptor's first word, and the head pointer's flags.
const ED_LOW_SPEED: u32 = 1 << 13;
const ED_SKIP: u32 = 1 << 14;
const HEAD_HALTED: u32 = 1 << 0;
const HEAD_CARRY: u32 = 1 << 1;

// A general transfer descriptor's first word.
const TD_ROUNDING: u32 = 1 << 18;
const DP_SETUP: u32 = 0;
const DP_OUT: u32 = 1;
const DP_IN: u32 = 2;
const TD_DATA0: u32 = 2 << 24;
const TD_DATA1: u32 = 3 << 24;
const TD_CARRY: u32 = 0;
const TD_NO_INTERRUPT: u32 = 7 << 21;
const CC_NO_ERROR: u32 = 0;
const CC_STALL: u32 = 4;
const CC_UNDERRUN: u32 = 9;
const CC_NOT_ACCESSED: u32 = 0xf;

const SLOTS: u32 = 512;
const SLOT: u32 = 32;
const SETUP_SLOT: u32 = 0;
const MAX_PIPES: usize = 64;
// The most one transfer descriptor carries: two pages, the second boundary being the descriptor's limit.
const TD_MAX: u32 = 8192;
const CONTROL_TICKS: u64 = 100;
const BULK_TICKS: u64 = 500;

// Which list a pipe's descriptor is on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum List {
	Interrupt,
	Control,
	Bulk,
}

// ONE PIPE: its endpoint, its list, its endpoint descriptor's slot and the dummy TD at its tail, and a standing
// interrupt transfer's TD with its buffer and length.
struct PipeState {
	endpoint: Endpoint,
	list: List,
	ed: u32,
	dummy: u32,
	standing: Option<(u32, u32, u32)>,
}

struct Ohci {
	base: u64,
	irq: u64,
	device: u64,
	ports: u32,
	pool: Pool,
	hcca: Dma,
	anchors: [u32; 3],
	pipes: Vec<Option<PipeState>>,
	port_changed: bool,
	said_error: bool,
}

// A TD's three facts as this driver queued it: its slot, where its buffer starts and how long it is.
#[derive(Clone, Copy)]
struct Queued {
	td: u32,
	buffer: u32,
	len: u32,
}

impl Ohci {
	fn reg(&self, offset: u64) -> u32 {
		// SAFETY: the controller's register window, mapped for this process.
		unsafe { ((self.base + offset) as *const u32).read_volatile() }
	}

	fn set(&self, offset: u64, value: u32) {
		// SAFETY: as `reg`.
		unsafe { ((self.base + offset) as *mut u32).write_volatile(value) }
	}

	fn port_reg(port: u32) -> u64 {
		HC_RH_PORT + (port as u64 - 1) * 4
	}

	// TAKE THE CONTROLLER FROM WHOEVER HELD IT, RESET IT AND BUILD ITS LISTS: system management firmware asked to give
	// it up when it routes the interrupt to itself, the frame interval kept across the software reset, the
	// communications area and the three anchors in place, the controller operational with every list enabled, and the
	// root ports powered.
	fn bring_up(base: u64, irq: u64, device: u64) -> Option<Ohci> {
		let reg = |offset: u64| unsafe { ((base + offset) as *const u32).read_volatile() };
		let set = |offset: u64, value: u32| unsafe { ((base + offset) as *mut u32).write_volatile(value) };
		if reg(HC_CONTROL) & CONTROL_IR != 0 {
			set(HC_COMMAND_STATUS, COMMAND_OCR);
			let deadline = clock() + 50;
			while reg(HC_CONTROL) & CONTROL_IR != 0 {
				if clock() >= deadline {
					print(b"driver.ohci: the firmware did not give the controller up\n");
					return None;
				}
				yield_now();
			}
		}
		let interval = match reg(HC_FM_INTERVAL) & 0x3fff {
			0 => 11999,
			interval => interval,
		};
		set(HC_INTERRUPT_DISABLE, INT_MIE | INT_ALL);
		set(HC_COMMAND_STATUS, COMMAND_HCR);
		let deadline = clock() + 10;
		while reg(HC_COMMAND_STATUS) & COMMAND_HCR != 0 {
			if clock() >= deadline {
				return None;
			}
			yield_now();
		}
		// RESET, AND THE CONTROLLER IS IN ITS SUSPEND STATE: nothing a previous instance pointed it at is in flight.
		device_quiesced(device);
		let pool = Pool::new(device, SLOT, SLOTS)?;
		let hcca = usb_host::dma_below_4g(device, 4096)?;
		// SAFETY: the communications area, which the controller does not read until it is operational below.
		unsafe { hcca.bytes_mut(256) }.fill(0);
		let mut ohci = Ohci { base, irq, device, ports: reg(HC_RH_DESCRIPTOR_A) & 0xff, pool, hcca, anchors: [0; 3], pipes: (0..MAX_PIPES).map(|_| None).collect(), port_changed: false, said_error: false };
		let setup = ohci.pool.take()?;
		debug_assert_eq!(setup, SETUP_SLOT);
		for at in 0..3 {
			let anchor = ohci.pool.take()?;
			ohci.pool.write(anchor, 0, ED_SKIP);
			ohci.anchors[at] = anchor;
		}
		let interrupt = ohci.pool.phys(ohci.anchors[0]);
		for entry in 0..32u64 {
			// SAFETY: as above.
			unsafe { ((ohci.hcca.virt + entry * 4) as *mut u32).write_volatile(interrupt) };
		}
		fence(Ordering::SeqCst);
		set(HC_HCCA, ohci.hcca.phys);
		set(HC_CONTROL_HEAD, ohci.pool.phys(ohci.anchors[1]));
		set(HC_CONTROL_CURRENT, 0);
		set(HC_BULK_HEAD, ohci.pool.phys(ohci.anchors[2]));
		set(HC_BULK_CURRENT, 0);
		set(HC_INTERRUPT_STATUS, INT_ALL);
		let toggle = reg(HC_FM_INTERVAL) & (1 << 31);
		set(HC_FM_INTERVAL, (toggle ^ (1 << 31)) | ((interval - 210) * 6 / 7) << 16 | interval);
		set(HC_PERIODIC_START, interval * 9 / 10);
		set(HC_LS_THRESHOLD, 0x628);
		set(HC_CONTROL, (reg(HC_CONTROL) & !CONTROL_HCFS & !0x3f) | CONTROL_CBSR | CONTROL_PLE | CONTROL_CLE | CONTROL_BLE | CONTROL_OPERATIONAL);
		set(HC_INTERRUPT_ENABLE, INT_MIE | INT_WDH | INT_RHSC | INT_UE | INT_SO);
		// THE PORTS' POWER: always on for a hub that switches none, one switch for all, or one per port.
		let descriptor = reg(HC_RH_DESCRIPTOR_A);
		if descriptor & RHA_NPS == 0 {
			if descriptor & RHA_PSM == 0 {
				set(HC_RH_STATUS, RH_LPSC);
			} else {
				for port in 1..=ohci.ports {
					set(Self::port_reg(port), PORT_PPS);
				}
			}
		}
		// POTPGT, in two-millisecond units, rounded up to ticks.
		sleep_until(clock() + 1 + (descriptor >> 24) as u64 * 2 / 10);
		Some(ohci)
	}

	fn frame(&self) -> u32 {
		self.reg(HC_FM_NUMBER) & 0xffff
	}

	// WAIT FOR A FRAME BOUNDARY TO PASS TWICE, so a descriptor taken out of a list is in no frame the controller is
	// still walking.
	fn frames_pass(&self) {
		for _ in 0..2 {
			let now = self.frame();
			let deadline = clock() + 2;
			while self.frame() == now && clock() < deadline {
				yield_now();
			}
		}
	}

	fn td_write(&self, td: u32, control: u32, buffer: u32, len: u32, next: u32) {
		self.pool.write(td, 0, CC_NOT_ACCESSED << 28 | control);
		self.pool.write(td, 4, if len == 0 { 0 } else { buffer });
		self.pool.write(td, 8, next);
		self.pool.write(td, 12, if len == 0 { 0 } else { buffer + len - 1 });
	}

	// QUEUE TDs ON A PIPE: the dummy at its tail becomes the first of them, a fresh dummy goes behind the last, and
	// the tail pointer moves to it - after every word was written - and the list is told it has work. None, with
	// nothing queued, when the pool cannot hold them.
	fn queue(&mut self, at: usize, stages: &[(u32, u32, u32)]) -> Option<Vec<Queued>> {
		if self.pool.free() < stages.len() {
			return None;
		}
		let (ed, dummy, list) = {
			let state = self.pipes[at].as_ref()?;
			(state.ed, state.dummy, state.list)
		};
		let mut slots: Vec<u32> = alloc::vec![dummy];
		for _ in 0..stages.len() {
			slots.push(self.pool.take()?);
		}
		let mut queued = Vec::new();
		for (index, &(control, buffer, len)) in stages.iter().enumerate() {
			self.td_write(slots[index], control, buffer, len, self.pool.phys(slots[index + 1]));
			queued.push(Queued { td: slots[index], buffer, len });
		}
		let tail = *slots.last()?;
		self.td_write(tail, 0, 0, 0, 0);
		fence(Ordering::SeqCst);
		self.pool.write(ed, 4, self.pool.phys(tail));
		if let Some(state) = self.pipes[at].as_mut() {
			state.dummy = tail;
		}
		match list {
			List::Control => self.set(HC_COMMAND_STATUS, COMMAND_CLF),
			List::Bulk => self.set(HC_COMMAND_STATUS, COMMAND_BLF),
			List::Interrupt => {}
		}
		Some(queued)
	}

	// WHERE A QUEUE STANDS: None while it runs; once every TD retired, or the descriptor halted on one, how many bytes
	// moved and whether it stalled or failed. A short packet is an ending - buffer rounding retires its TD without an
	// error - and the bytes it moved are the buffer up to where the controller stopped.
	fn outcome(&self, at: usize, queued: &[Queued]) -> Option<(u32, Option<bool>)> {
		let ed = self.pipes[at].as_ref()?.ed;
		let halted = self.pool.read(ed, 8) & HEAD_HALTED != 0;
		let mut moved = 0;
		for entry in queued {
			let control = self.pool.read(entry.td, 0);
			let code = control >> 28;
			if code == CC_NOT_ACCESSED || code == CC_NOT_ACCESSED - 1 {
				return if halted { Some((moved, Some(false))) } else { None };
			}
			let current = self.pool.read(entry.td, 4);
			let part = if current == 0 { entry.len } else { current.saturating_sub(entry.buffer) };
			moved += part;
			match code {
				CC_NO_ERROR => {}
				CC_UNDERRUN => return Some((moved, None)),
				CC_STALL => return Some((moved, Some(true))),
				_ => return Some((moved, Some(false))),
			}
		}
		Some((moved, None))
	}

	// AFTER A QUEUE ENDED: the TDs it had given back and the descriptor's queue emptied to its dummy, its halt cleared
	// and its toggle carry kept - a stall's toggle is reset by the caller, which clears the device's halt too.
	fn settle_queue(&mut self, at: usize, queued: Vec<Queued>) {
		let Some((ed, dummy)) = self.pipes[at].as_ref().map(|state| (state.ed, state.dummy)) else { return };
		let head = self.pool.read(ed, 8);
		if head & !0xf != self.pool.phys(dummy) {
			// Halted part way, or abandoned: skipped while its queue is taken back.
			let control = self.pool.read(ed, 0);
			self.pool.write(ed, 0, control | ED_SKIP);
			self.frames_pass();
			self.pool.write(ed, 8, self.pool.phys(dummy) | (head & HEAD_CARRY));
			self.pool.write(ed, 0, control & !ED_SKIP);
		} else if head & HEAD_HALTED != 0 {
			self.pool.write(ed, 8, head & !HEAD_HALTED);
		}
		for entry in queued {
			self.pool.give(entry.td);
		}
	}

	// RUN A QUEUE ON A PIPE TO ITS END, bounded.
	fn run_queue(&mut self, at: usize, stages: &[(u32, u32, u32)], ticks: u64) -> Option<(u32, Option<bool>)> {
		let queued = self.queue(at, stages)?;
		let irq = self.irq;
		let ended = usb_host::settle(self, irq, ticks, |ohci| ohci.outcome(at, &queued).is_some());
		let result = if ended { self.outcome(at, &queued) } else { None };
		self.settle_queue(at, queued);
		result
	}

	fn index(pipe: Pipe) -> usize {
		pipe as usize - 1
	}

	// The anchor of a list, which every descriptor on it is linked behind.
	fn anchor(&self, list: List) -> u32 {
		self.anchors[match list {
			List::Interrupt => 0,
			List::Control => 1,
			List::Bulk => 2,
		}]
	}
}

impl Controller for Ohci {
	fn who(&self) -> &'static [u8] {
		b"driver.ohci"
	}

	fn ports(&self) -> u32 {
		self.ports
	}

	fn port(&mut self, port: u32) -> Port {
		let value = self.reg(Self::port_reg(port));
		Port { connected: value & PORT_CCS != 0, enabled: value & PORT_PES != 0, changed: value & PORT_CSC != 0 }
	}

	// THE ROOT PORT RESET: asked for, waited for on its change bit, the change acknowledged, and the speed read off
	// the low-speed bit.
	fn reset_port(&mut self, port: u32) -> Option<Speed> {
		let register = Self::port_reg(port);
		if self.reg(register) & PORT_CCS == 0 {
			return None;
		}
		self.set(register, PORT_PRS);
		let deadline = clock() + 10;
		while self.reg(register) & PORT_PRSC == 0 {
			if clock() >= deadline {
				return None;
			}
			sleep_until(clock() + 1);
		}
		self.set(register, PORT_PRSC);
		sleep_until(clock() + 1);
		let value = self.reg(register);
		if value & PORT_PES == 0 {
			return None;
		}
		Some(if value & PORT_LSDA != 0 { Speed::Low } else { Speed::Full })
	}

	fn acknowledge_port(&mut self, port: u32) {
		let register = Self::port_reg(port);
		self.set(register, self.reg(register) & PORT_CHANGES);
	}

	// A PIPE IS AN ENDPOINT DESCRIPTOR on its kind's list, behind that list's anchor, with an empty queue: its head and
	// tail both the dummy TD.
	fn open(&mut self, endpoint: &Endpoint) -> Option<Pipe> {
		let at = self.pipes.iter().position(Option::is_none)?;
		let ed = self.pool.take()?;
		let Some(dummy) = self.pool.take() else {
			self.pool.give(ed);
			return None;
		};
		let list = match endpoint.kind {
			Kind::Control => List::Control,
			Kind::Bulk => List::Bulk,
			Kind::Interrupt => List::Interrupt,
		};
		let first = (endpoint.address as u32 & 0x7f) | (endpoint.number as u32 & 0xf) << 7 | if endpoint.speed == Speed::Low { ED_LOW_SPEED } else { 0 } | (endpoint.max_packet as u32 & 0x7ff) << 16;
		self.pool.write(ed, 0, first);
		self.pool.write(ed, 4, self.pool.phys(dummy));
		self.pool.write(ed, 8, self.pool.phys(dummy));
		let anchor = self.anchor(list);
		self.pool.write(ed, 12, self.pool.read(anchor, 12));
		fence(Ordering::SeqCst);
		self.pool.write(anchor, 12, self.pool.phys(ed));
		self.pipes[at] = Some(PipeState { endpoint: *endpoint, list, ed, dummy, standing: None });
		Some(at as Pipe + 1)
	}

	// SKIPPED, UNLINKED FROM BEHIND WHATEVER PRECEDES IT, two frame boundaries waited, and its descriptors given back.
	fn close(&mut self, pipe: Pipe) {
		let Some(state) = self.pipes.get_mut(Self::index(pipe)).and_then(Option::take) else { return };
		let first = self.pool.read(state.ed, 0);
		self.pool.write(state.ed, 0, first | ED_SKIP);
		let target = self.pool.phys(state.ed);
		let mut at = self.anchor(state.list);
		loop {
			let next = self.pool.read(at, 12);
			if next == target {
				self.pool.write(at, 12, self.pool.read(state.ed, 12));
				break;
			}
			let Some(slot) = self.pool.slot_of(next) else { break };
			at = slot;
		}
		self.frames_pass();
		if let Some((td, _, _)) = state.standing {
			self.pool.give(td);
		}
		self.pool.give(state.dummy);
		self.pool.give(state.ed);
	}

	fn retarget(&mut self, pipe: Pipe, address: u8, max_packet: u16) {
		let Some(state) = self.pipes.get_mut(Self::index(pipe)).and_then(Option::as_mut) else { return };
		state.endpoint.address = address;
		state.endpoint.max_packet = max_packet;
		let first = self.pool.read(state.ed, 0) & !(0x7f | 0x7ff << 16);
		self.pool.write(state.ed, 0, first | address as u32 & 0x7f | (max_packet as u32 & 0x7ff) << 16);
	}

	// ONE QUEUE: the setup TD on DATA0, the data stage on DATA1 with buffer rounding - a short answer retires it and
	// the status stage runs - and the status stage on DATA1 the other way, interrupting when it retires.
	fn control(&mut self, pipe: Pipe, setup: &[u8; 8], data: Option<(u32, u16)>) -> Done {
		let at = Self::index(pipe);
		if self.pipes.get(at).and_then(Option::as_ref).is_none() {
			return Done::Failed;
		}
		for (word, bytes) in setup.chunks(4).enumerate() {
			self.pool.write(SETUP_SLOT, word as u32 * 4, u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
		}
		let data_in = setup[0] & 0x80 != 0;
		let mut stages = alloc::vec![(DP_SETUP << 19 | TD_DATA0 | TD_NO_INTERRUPT, self.pool.phys(SETUP_SLOT), 8)];
		let with_data = data.filter(|&(_, len)| len > 0);
		if let Some((phys, len)) = with_data {
			stages.push(((if data_in { DP_IN } else { DP_OUT }) << 19 | TD_DATA1 | TD_ROUNDING | TD_NO_INTERRUPT, phys, len as u32));
		}
		let status_in = !(data_in && with_data.is_some());
		stages.push(((if status_in { DP_IN } else { DP_OUT }) << 19 | TD_DATA1, 0, 0));
		match self.run_queue(at, &stages, CONTROL_TICKS) {
			Some((moved, None)) => Done::Ok(moved.saturating_sub(8)),
			Some((moved, Some(true))) => Done::Stall(moved.saturating_sub(8)),
			_ => Done::Failed,
		}
	}

	// A BULK TRANSFER STAGE BY STAGE, each one TD of at most two pages run to its end, the toggle carried in the
	// endpoint descriptor - until all of it moved, a stage came back short, or one failed.
	fn bulk(&mut self, pipe: Pipe, phys: u32, len: u32) -> Done {
		let at = Self::index(pipe);
		let Some(endpoint) = self.pipes.get(at).and_then(Option::as_ref).map(|state| state.endpoint) else { return Done::Failed };
		let direction = if endpoint.dir_in { DP_IN } else { DP_OUT };
		let mut moved = 0;
		loop {
			let at_phys = phys + moved;
			let stage = (len - moved).min(TD_MAX - (at_phys & 0xfff));
			let Some((part, failed)) = self.run_queue(at, &[(direction << 19 | TD_CARRY | TD_ROUNDING, at_phys, stage)], BULK_TICKS) else { return Done::Failed };
			moved += part;
			match failed {
				Some(true) => return Done::Stall(moved),
				Some(false) => return Done::Failed,
				None if part < stage || moved >= len => return Done::Ok(moved),
				None => continue,
			}
		}
	}

	fn submit(&mut self, pipe: Pipe, phys: u32, len: u32) -> bool {
		let at = Self::index(pipe);
		if self.pipes.get(at).and_then(Option::as_ref).is_none_or(|state| state.standing.is_some()) {
			return false;
		}
		let Some(queued) = self.queue(at, &[(DP_IN << 19 | TD_CARRY | TD_ROUNDING, phys, len)]) else { return false };
		if let Some(state) = self.pipes[at].as_mut() {
			state.standing = Some((queued[0].td, phys, len));
		}
		true
	}

	fn reap(&mut self, pipe: Pipe) -> Option<Done> {
		let at = Self::index(pipe);
		let (td, buffer, len) = self.pipes.get(at)?.as_ref()?.standing?;
		let queued = [Queued { td, buffer, len }];
		let (moved, failed) = self.outcome(at, &queued)?;
		if let Some(state) = self.pipes[at].as_mut() {
			state.standing = None;
		}
		self.settle_queue(at, queued.to_vec());
		Some(match failed {
			Some(true) => Done::Stall(moved),
			Some(false) => Done::Failed,
			None => Done::Ok(moved),
		})
	}

	// THE TOGGLE IS CARRIED IN THE ENDPOINT DESCRIPTOR'S HEAD POINTER, and back to DATA0 is that bit cleared - on a
	// descriptor whose queue is empty, which is the only time a caller asks.
	fn reset_toggle(&mut self, pipe: Pipe) {
		let Some(ed) = self.pipes.get(Self::index(pipe)).and_then(Option::as_ref).map(|state| state.ed) else { return };
		let head = self.pool.read(ed, 8);
		self.pool.write(ed, 8, head & !HEAD_CARRY & !HEAD_HALTED);
	}

	// THE STATUS, READ AND CLEARED. A root hub change is remembered, and its interrupt held off until the change is
	// taken - its change bit stays set until then, and an enabled interrupt would raise the line on every look.
	fn service(&mut self) {
		let status = self.reg(HC_INTERRUPT_STATUS) & INT_ALL;
		self.set(HC_INTERRUPT_STATUS, status);
		if status & INT_RHSC != 0 {
			self.port_changed = true;
			self.set(HC_INTERRUPT_DISABLE, INT_RHSC);
		}
		if status & INT_UE != 0 && !self.said_error {
			self.said_error = true;
			print(b"driver.ohci: the controller reported an unrecoverable error and stopped\n");
		}
	}

	fn take_port_change(&mut self) -> bool {
		if core::mem::take(&mut self.port_changed) {
			self.set(HC_INTERRUPT_ENABLE, INT_RHSC);
			return true;
		}
		false
	}

	fn port_poll(&self) -> Option<u64> {
		None
	}

	fn dma(&mut self, bytes: u32) -> Option<Dma> {
		usb_host::dma_below_4g(self.device, bytes)
	}

	fn max_transfer(&self) -> u32 {
		64 * 1024
	}

	fn splits(&self) -> bool {
		false
	}

	// SUSPEND: the lists off and the controller in its USB-suspend state, after which it masters nothing.
	fn halt(&mut self) -> bool {
		let control = self.reg(HC_CONTROL);
		self.set(HC_CONTROL, control & !(CONTROL_PLE | CONTROL_CLE | CONTROL_BLE));
		self.frames_pass();
		self.set(HC_CONTROL, (control & !CONTROL_HCFS & !(CONTROL_PLE | CONTROL_CLE | CONTROL_BLE)) | 3 << 6);
		true
	}

	fn run(&mut self) -> bool {
		let control = self.reg(HC_CONTROL);
		self.set(HC_CONTROL, (control & !CONTROL_HCFS) | 1 << 6);
		sleep_until(clock() + 3);
		self.set(HC_CONTROL, (control & !CONTROL_HCFS) | CONTROL_OPERATIONAL | CONTROL_PLE | CONTROL_CLE | CONTROL_BLE);
		true
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let device = resources.device;
	// THE INTERRUPT IS THE FUNCTION'S INTx, its row's line - there is no other.
	if device == 0 || resources.line_count == 0 {
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let irq = resources.lines[0];
	keys::set_power(resources.syspower);
	keys::set_console_input(resources.console);
	usb_input::KEY_SINK.store(resources.keys, Ordering::Relaxed);
	usb_input::TRUSTED_KEY_SINK.store(resources.trusted_keys, Ordering::Relaxed);
	// SAFETY: the device's register window, which the claim minted for this process.
	let base: u64 = unsafe { syscall(SYS_DEVICE_MEMORY_MAP, device, 0, 0, 0) };
	if sys_is_err(base) {
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::ResourceUnusable);
	}
	let Some(ohci) = Ohci::bring_up(base, irq, device) else {
		print(b"driver.ohci: the controller did not reset or would not run\n");
		common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::DeviceNotResponding);
	};
	let Some(host) = usb_host::Host::new(ohci) else { common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::OutOfMemory) };
	usb_host::run(bootstrap, &bind, device, irq, host)
}
