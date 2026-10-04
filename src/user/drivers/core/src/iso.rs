// AN ISOCHRONOUS PIPE OF A CLASS MODULE: one endpoint's ring and one page of packet buffers, each one service
// interval's payload. IN, as many transfers stand as the page holds (up to `CAPTURE_POSTED`), each asking for its
// completion and re-posted as it completes - what a payload is, is how much of the interval the device filled. OUT,
// the pieces a sender hands over are posted into free buffers, one completion asked for on the last of each send,
// and every piece up to the TRB an event names is done with it. A camera's stream and a Bluetooth controller's voice
// are both this; an audio stream keeps its own, because its sink paces itself by a feedback endpoint.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use rt::*;

use crate::usb_audio::{CC_RING_OVERRUN, CC_RING_UNDERRUN, EP_TYPE_ISOCH_IN, TRB_ISOCH, TRB_START_ISOCH_ASAP, interval_exponent};
use crate::usb_hid::Hids;
use crate::{CC_SHORT_PACKET, CC_SUCCESS, Ring, SPEED_HIGH, TRB_CONFIGURE_ENDPOINT, TRB_EV_CMD_COMPLETE, TRB_EV_TRANSFER, TRB_IOC, TRB_SET_TR_DEQUEUE, UsbDevice, Xhci};
use crate::{command, command_and_wait, dma_page, keep_stray, r8, take_event, w32, wait_command};
use drivers::usb_class::CAPTURE_POSTED;
use drivers::usb_function::Endpoint;

const TRB_STOP_ENDPOINT: u32 = 15;
// The endpoint type an isochronous OUT pipe has; IN's is `EP_TYPE_ISOCH_IN`.
const EP_TYPE_ISOCH_OUT: u32 = 1;
/// A payload is one transfer, and a transfer is a page at most.
pub(crate) const MAX_PAYLOAD: u32 = 4096;

pub(crate) struct IsoPipe {
	/// The bytes one service interval carries, and so each buffer's size.
	pub capacity: u32,
	slot: u32,
	dci: u32,
	input: bool,
	ring: Ring,
	page: u64,
	virt: u64,
	phys: u64,
	/// How many buffers the page holds, and so how many transfers may stand.
	pub buffers: u32,
	posted: u32,
	/// The buffer the next transfer posted lands in, and the one the oldest standing transfer does.
	next: u32,
	done: u32,
	/// An OUT pipe's standing pieces, by TRB address, oldest first.
	inflight: VecDeque<u64>,
}

/// What one completion on an isochronous IN pipe was.
pub(crate) enum Completed {
	/// A payload - possibly empty.
	Payload(Vec<u8>),
	/// A packet the transport missed or failed.
	Lost,
	/// An event with no transfer behind it: the ring was found empty, or too full.
	Nothing,
}

/// BRING ISOCHRONOUS ENDPOINTS UP IN ONE CONFIGURE ENDPOINT, each at its payload `capacity`: dropped and added, as
/// `configure_pipes` brings a bulk pipe up, so an endpoint an earlier setting left at another size is replaced - and
/// the slot's context entries covering them and `floor`, the highest endpoint the device already has, because a
/// slot context that covered only these would leave the others outside it. `None`, with nothing held, when a ring, a
/// page or the controller refused.
///
/// # Safety
/// `dev` is an addressed device this controller owns, its endpoints in the setting about to be selected.
pub(crate) unsafe fn open(hc: &mut Xhci, dev: &mut UsbDevice, endpoints: &[(Endpoint, u32)], floor: u32) -> Option<Vec<IsoPipe>> {
	unsafe {
		let high_speed = dev.speed == SPEED_HIGH;
		let mut pipes: Vec<IsoPipe> = Vec::new();
		let give_back = |hc: &mut Xhci, pipes: &mut Vec<IsoPipe>| {
			for mut pipe in pipes.drain(..) {
				pipe.release(hc);
			}
		};
		if endpoints.iter().any(|&(_, capacity)| capacity == 0 || capacity > MAX_PAYLOAD) {
			return None;
		}
		for &(endpoint, capacity) in endpoints {
			let Some(mut ring) = Ring::new() else {
				give_back(hc, &mut pipes);
				return None;
			};
			let Some((page, virt, phys)) = dma_page() else {
				ring.release();
				give_back(hc, &mut pipes);
				return None;
			};
			let buffers = (MAX_PAYLOAD / capacity).min(CAPTURE_POSTED).max(1);
			pipes.push(IsoPipe { capacity, slot: dev.slot, dci: endpoint.dci(), input: endpoint.is_in(), ring, page, virt, phys, buffers, posted: 0, next: 0, done: 0, inflight: VecDeque::new() });
		}
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		let flags = pipes.iter().fold(0u32, |flags, pipe| flags | 1 << pipe.dci);
		let entries = pipes.iter().map(|pipe| pipe.dci).max().unwrap_or(0).max(floor);
		(dev.in_virt as *mut u32).write_volatile(flags);
		((dev.in_virt + 4) as *mut u32).write_volatile(1 | flags);
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(entries << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		for (pipe, &(endpoint, _)) in pipes.iter().zip(endpoints) {
			let ep_ctx: u64 = dev.in_virt + (1 + pipe.dci as u64) * hc.ctx_size;
			(ep_ctx as *mut u32).write_volatile(interval_exponent(dev.speed, endpoint.interval) << 16);
			// ERROR COUNT ZERO, as isochronous is; at high speed the packets a microframe adds are the burst field.
			let burst = if high_speed { u32::from(endpoint.packet >> 11 & 0x3).min(2) } else { 0 };
			let kind = if pipe.input { EP_TYPE_ISOCH_IN } else { EP_TYPE_ISOCH_OUT };
			((ep_ctx + 4) as *mut u32).write_volatile(u32::from(endpoint.max_packet()) << 16 | burst << 8 | kind << 3);
			((ep_ctx + 8) as *mut u32).write_volatile((pipe.ring.phys | pipe.ring.cycle as u64) as u32);
			((ep_ctx + 12) as *mut u32).write_volatile((pipe.ring.phys >> 32) as u32);
			((ep_ctx + 16) as *mut u32).write_volatile(pipe.capacity | pipe.capacity << 16);
		}
		if command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24).is_none() {
			give_back(hc, &mut pipes);
			return None;
		}
		// THEIR COMPLETIONS ARE KEPT by a synchronous wait that meets them, which would otherwise take a standing
		// transfer's event off the ring and drop it.
		for pipe in &pipes {
			if !hc.class_rx.contains(&(dev.slot, pipe.dci)) {
				hc.class_rx.push((dev.slot, pipe.dci));
			}
		}
		Some(pipes)
	}
}

impl IsoPipe {
	pub(crate) fn owns(&self, control: u32) -> bool {
		control >> 10 & 0x3f == TRB_EV_TRANSFER && control >> 24 == self.slot && (control >> 16 & 0x1f) == self.dci
	}

	/// AN IN PIPE'S TRANSFERS, posted until as many stand as there are buffers; the doorbell rung once.
	pub(crate) fn post_in(&mut self, hc: &mut Xhci) {
		let mut posted_any = false;
		while self.input && self.posted < self.buffers {
			let at = self.phys + u64::from(self.next) * u64::from(self.capacity);
			// EVERY ONE ASKS FOR ITS COMPLETION: what a payload is, is how much of the interval the device filled.
			unsafe { self.ring.push(at, self.capacity, TRB_ISOCH << 10 | TRB_START_ISOCH_ASAP | TRB_IOC) };
			self.next = (self.next + 1) % self.buffers;
			self.posted += 1;
			posted_any = true;
		}
		if posted_any {
			unsafe { w32(hc.db + self.slot as u64 * 4, self.dci) };
		}
	}

	/// ONE COMPLETION ON AN IN PIPE, in the order the transfers stand.
	pub(crate) fn complete_in(&mut self, status: u32) -> Completed {
		let code = status >> 24;
		if code == CC_RING_UNDERRUN || code == CC_RING_OVERRUN || self.posted == 0 {
			return Completed::Nothing;
		}
		let buffer = self.done;
		self.done = (self.done + 1) % self.buffers;
		self.posted -= 1;
		if code != CC_SUCCESS && code != CC_SHORT_PACKET {
			return Completed::Lost;
		}
		let moved = self.capacity.saturating_sub(status & 0x00ff_ffff);
		let base = self.virt + u64::from(buffer) * u64::from(self.capacity);
		Completed::Payload((0..u64::from(moved)).map(|at| unsafe { r8(base + at) }).collect())
	}

	/// How many more pieces an OUT pipe can take now.
	pub(crate) fn room(&self) -> u32 {
		self.buffers - self.posted
	}

	/// AN OUT PIPE'S PIECES, one service interval each, posted into free buffers with ONE completion asked for, on the
	/// last; the address of that TRB, which the completion will name - or `None`, with nothing posted, when there is
	/// not room for every one of them or one is larger than an interval carries.
	pub(crate) fn send_out(&mut self, hc: &mut Xhci, pieces: &[&[u8]]) -> Option<u64> {
		if self.input || pieces.is_empty() || pieces.len() as u32 > self.room() || pieces.iter().any(|piece| piece.len() as u32 > self.capacity) {
			return None;
		}
		let mut last = 0u64;
		for (index, piece) in pieces.iter().enumerate() {
			let at = self.virt + u64::from(self.next) * u64::from(self.capacity);
			for (offset, &byte) in piece.iter().enumerate() {
				unsafe { ((at + offset as u64) as *mut u8).write_volatile(byte) };
			}
			let phys = self.phys + u64::from(self.next) * u64::from(self.capacity);
			last = self.ring.phys + self.ring.index * 16;
			let ioc = if index + 1 == pieces.len() { TRB_IOC } else { 0 };
			core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
			unsafe { self.ring.push(phys, piece.len() as u32, TRB_ISOCH << 10 | TRB_START_ISOCH_ASAP | ioc) };
			self.inflight.push_back(last);
			self.next = (self.next + 1) % self.buffers;
			self.posted += 1;
		}
		unsafe { w32(hc.db + self.slot as u64 * 4, self.dci) };
		Some(last)
	}

	/// ONE COMPLETION ON AN OUT PIPE: every piece up to the TRB it names is done, and their buffers are free. How many,
	/// or zero for an event with no transfer behind it.
	pub(crate) fn complete_out(&mut self, pointer: u64, status: u32) -> u32 {
		let code = status >> 24;
		if code == CC_RING_UNDERRUN || code == CC_RING_OVERRUN {
			return 0;
		}
		let Some(at) = self.inflight.iter().position(|&trb| trb == pointer) else { return 0 };
		self.inflight.drain(..=at);
		let done = at as u32 + 1;
		self.posted = self.posted.saturating_sub(done);
		self.done = (self.done + done) % self.buffers;
		done
	}

	/// EVERY STANDING TRANSFER ABANDONED: the endpoint stopped - the stopped transfers' events read and dropped, anybody
	/// else's kept - and the ring moved past everything posted.
	pub(crate) fn stop(&mut self, hc: &mut Xhci, hids: &mut Hids) {
		if self.posted > 0 {
			command(hc, 0, 0, TRB_STOP_ENDPOINT << 10 | self.dci << 16 | self.slot << 24);
			let mut spins: u32 = 0;
			loop {
				match unsafe { take_event(hc) } {
					Some((_, _, control)) if control >> 10 & 0x3f == TRB_EV_CMD_COMPLETE => break,
					Some((_, _, control)) if self.owns(control) => {}
					Some((pointer, status, control)) => {
						if !keep_stray(hc, pointer, status, control) {
							crate::usb_hid::handle_hid_event(hc, hids, status, control);
						}
					}
					None => {
						spins += 1;
						if spins > 1_000_000 {
							break;
						}
						if spins % 4096 == 0 {
							yield_now();
						}
					}
				}
			}
			let dequeue = self.ring.phys + self.ring.index * 16 | self.ring.cycle as u64;
			command(hc, dequeue, 0, TRB_SET_TR_DEQUEUE << 10 | self.dci << 16 | self.slot << 24);
			let _ = wait_command(hc, hids);
		}
		hc.class_pending.retain(|&(_, _, control)| !self.owns(control));
		self.posted = 0;
		self.done = self.next;
		self.inflight.clear();
	}

	pub(crate) fn release(&mut self, hc: &mut Xhci) {
		self.ring.release();
		if self.page != 0 {
			close(self.page);
			self.page = 0;
		}
		hc.class_rx.retain(|&(slot, dci)| (slot, dci) != (self.slot, self.dci));
		hc.class_pending.retain(|&(_, _, control)| !(control >> 24 == self.slot && (control >> 16 & 0x1f) == self.dci));
	}
}
