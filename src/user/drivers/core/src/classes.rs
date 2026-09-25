// THE SERVICE-BACKED CLASS MODULES: a printer, a still-image camera, a smart-card reader, a MIDI device, a
// HID power device, a Bluetooth controller, a mobile-broadband modem, a video camera and a firmware-upgrade
// target - each one bound inside this process under `usb_class.rs`'s model, and each one PUBLISHED as the
// provider its destination service consumes.
//
// WHAT THEY SHARE IS HERE, AND WHAT THEY DO NOT IS IN THEIR OWN MODULES. Every one of them needs the same four
// things from the controller: its configuration descriptors read once, its pipes brought up in one
// CONFIGURE_ENDPOINT, the completions of transfers it left outstanding kept when a synchronous wait takes them
// off the ring, and a publication that appears when the device binds and is WITHDRAWN when it leaves. A
// module owns its device and its protocol; the hold below owns the publication and the tokens.
//
// ONE DEVICE PER CLASS, AND A SECOND IS REFUSED BY NAME: the budget in `usb_class.rs` says so, and `admits`
// prints which ceiling a refused device reached.
//
// A FRESH TOKEN FOR EVERY ATTACH. A token names a publication for the whole binding generation, withdrawn or
// not - DeviceManager refuses one it has seen - so a printer unplugged and plugged in again is published under
// a new one. The tokens start above the positional list the handshake offered, so no position can collide.

use alloc::boxed::Box;
use alloc::vec::Vec;
use rt::*;

use crate::usb_hid::Hids;
use crate::{CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, DESC_CONFIG, FEATURE_ENDPOINT_HALT, REQ_CLEAR_FEATURE, REQ_GET_DESCRIPTOR, REQ_SET_CONFIGURATION, RT_ENDPOINT, TRB_CONFIGURE_ENDPOINT, TRB_EV_CMD_COMPLETE, TRB_EV_TRANSFER, TRB_IOC, TRB_NORMAL, TRB_RESET_ENDPOINT, TRB_SET_TR_DEQUEUE};
use crate::{Ring, UsbDevice, Xhci};
use crate::{command, command_and_wait, control_in_req, control_nodata, dma_page, r8, take_event, w32};
use drivers::common;
use drivers::descriptor;
use drivers::usb_class::ClassKind;
use drivers::usb_function::{Endpoint, TRANSFER_BULK, TRANSFER_INTERRUPT};

/// The first token a class publication is offered under - above the handshake's positional list, with room
/// for that list to grow.
pub const FIRST_TOKEN: u16 = 32;

// The standard request that selects an alternate setting.
const REQ_SET_INTERFACE: u8 = 0x0b;
const RT_INTERFACE_OUT: u8 = 0x01;
// Stop Endpoint, and the completion codes a stopped transfer reports.
const TRB_STOP_ENDPOINT: u32 = 15;
const CC_STOPPED: u32 = 26;
const CC_STOPPED_LENGTH_INVALID: u32 = 27;
const CC_STOPPED_SHORT_PACKET: u32 = 28;
// The completions this controller keeps for class pipes during a synchronous wait. One transfer is ever
// outstanding on a class pipe, so this is the number of pipes nine modules can hold, with room over.
const MAX_KEPT: usize = 64;
// The configuration descriptors a probe reads: the first eight configurations, one page each.
const MAX_CONFIGURATIONS: u8 = 8;
const CONFIG_BYTES: u16 = 4096;

/// One class pipe: a bulk or interrupt endpoint, its transfer ring and the page its transfers move through.
///
/// ONE TRANSFER OUTSTANDING AT A TIME, which is what `busy` says and what every module here needs: a standing
/// receive is posted again when its completion has been read out of the page, and a write is answered when
/// the device took it. Two in flight through one page would be one overwriting the other.
pub struct Pipe {
	pub dci: u32,
	pub mps: u32,
	ep_type: u32,
	interval: u32,
	pub ring: Ring,
	handle: u64,
	pub virt: u64,
	pub phys: u64,
	pub busy: bool,
	/// How many bytes the outstanding transfer asked for.
	pub posted: u32,
	slot: u32,
}

impl Pipe {
	/// A pipe for `endpoint` on the device in `slot`, with its ring and its page.
	///
	/// # Safety
	/// Allocates DMA pages for this controller; `slot` is a slot it enabled.
	pub unsafe fn new(slot: u32, speed: u32, endpoint: &Endpoint) -> Option<Pipe> {
		unsafe {
			let ep_type: u32 = match (endpoint.transfer(), endpoint.is_in()) {
				(TRANSFER_BULK, false) => 2,
				(TRANSFER_BULK, true) => 6,
				(TRANSFER_INTERRUPT, false) => 3,
				(TRANSFER_INTERRUPT, true) => 7,
				// Isochronous pipes are the audio module's, and control pipes are endpoint zero's.
				_ => return None,
			};
			let mut ring = Ring::new()?;
			let Some((handle, virt, phys)) = dma_page() else {
				ring.release();
				return None;
			};
			let interval = if endpoint.transfer() == TRANSFER_INTERRUPT { interrupt_interval(speed, endpoint.interval as u32) } else { 0 };
			Some(Pipe { dci: endpoint.dci(), mps: endpoint.max_packet().max(8) as u32, ep_type, interval, ring, handle, virt, phys, busy: false, posted: 0, slot })
		}
	}

	pub fn is_in(&self) -> bool {
		self.dci & 1 == 1
	}

	/// Whether a transfer event is this pipe's.
	pub fn owns(&self, control: u32) -> bool {
		control >> 10 & 0x3f == TRB_EV_TRANSFER && control >> 24 == self.slot && (control >> 16 & 0x1f) == self.dci
	}

	/// Put `bytes` in the page, for an OUT transfer.
	pub fn fill(&mut self, bytes: &[u8]) -> bool {
		if bytes.len() > 4096 {
			return false;
		}
		unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.virt as *mut u8, bytes.len()) };
		true
	}

	/// The first `len` bytes of the page, copied out before the next transfer can overwrite them.
	pub fn read(&self, len: usize) -> Vec<u8> {
		let len = len.min(4096);
		let mut out = Vec::with_capacity(len);
		for at in 0..len {
			out.push(unsafe { r8(self.virt + at as u64) });
		}
		out
	}

	/// Post one transfer of `len` bytes through the page and ring the endpoint's doorbell. The completion
	/// is this pipe's to take - from the event loop, or from the stash a synchronous wait left it in.
	pub fn post(&mut self, hc: &mut Xhci, len: u32) -> bool {
		if self.busy || len == 0 || len > 4096 {
			return false;
		}
		unsafe {
			self.ring.push(self.phys, len, TRB_NORMAL << 10 | TRB_IOC);
			w32(hc.db + self.slot as u64 * 4, self.dci);
		}
		if !hc.class_rx.contains(&(self.slot, self.dci)) {
			hc.class_rx.push((self.slot, self.dci));
		}
		self.busy = true;
		self.posted = len;
		true
	}

	/// Ring the endpoint's doorbell again for the transfer already posted on it.
	///
	/// A DOORBELL MAY BE RUNG AT ANY TIME, and on a controller that polls a bulk IN endpoint by itself - which is
	/// what the specification describes - this is a no-op. It is here for the controllers that do not: QEMU's
	/// xHCI parks a transfer its device NAKed and tries it again only when the endpoint's doorbell rings or the
	/// device wakes it, and a device that has nothing to send until a command arrives on ANOTHER pipe - a PTP
	/// camera's response - never wakes it. So a module that has just given such a device a reason to answer
	/// rings the endpoint it expects the answer on.
	pub fn kick(&self, hc: &mut Xhci) {
		if self.busy {
			unsafe { w32(hc.db + self.slot as u64 * 4, self.dci) };
		}
	}

	/// What a completion of this pipe's transfer says: the code, and how many bytes moved.
	pub fn complete(&mut self, status: u32) -> (u32, u32) {
		self.busy = false;
		let residual = status & 0x00ff_ffff;
		(status >> 24, self.posted.saturating_sub(residual))
	}

	/// Where the producer is, with its cycle state: what a Set TR Dequeue Pointer names to skip everything
	/// posted before it.
	fn enqueue(&self) -> u64 {
		self.ring.phys + self.ring.index * 16 | self.ring.cycle as u64
	}

	/// Give the ring and the page back, and stop keeping completions for this endpoint.
	pub fn release(&mut self, hc: &mut Xhci) {
		self.ring.release();
		if self.handle != 0 {
			close(self.handle);
			self.handle = 0;
		}
		hc.class_rx.retain(|&(slot, dci)| (slot, dci) != (self.slot, self.dci));
		hc.class_pending.retain(|&(_, _, control)| !(control >> 24 == self.slot && (control >> 16 & 0x1f) == self.dci));
	}
}

// The xHCI endpoint-context interval for an interrupt endpoint: the exponent of its period in 125 us
// microframes. A high-speed descriptor already carries the exponent plus one; a full- or low-speed one counts
// 1 ms frames, so the period is the smallest power of two covering bInterval * 8 microframes.
fn interrupt_interval(speed: u32, b_interval: u32) -> u32 {
	if speed == crate::SPEED_HIGH || speed == crate::SPEED_SUPER {
		return b_interval.clamp(1, 16) - 1;
	}
	let mut exp: u32 = 3;
	while exp < 15 && 1 << (exp - 3) < b_interval {
		exp += 1;
	}
	exp
}

/// Bring a set of pipes up with ONE Configure Endpoint command: a device has one slot context, and its
/// Context Entries field has to cover the highest endpoint of all of them.
///
/// EACH PIPE IS DROPPED AND ADDED, which is a reconfiguration the specification allows and the only form
/// that is right whether or not an earlier module left the same endpoint enabled - a HID probe that
/// configured an interrupt pipe and then decided the device was not an input device did exactly that.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
/// `entries` is the highest endpoint the device will have enabled when this is done, for a module that
/// reconfigures one pipe of several: the slot context is rewritten, and a Context Entries field that stopped
/// at this pipe would leave the others past it.
pub unsafe fn configure_pipes(hc: &mut Xhci, dev: &mut UsbDevice, pipes: &[&Pipe], entries: u32) -> bool {
	unsafe {
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		let mut add: u32 = 1;
		let mut drop: u32 = 0;
		let mut entries: u32 = entries.max(1);
		for pipe in pipes {
			add |= 1 << pipe.dci;
			drop |= 1 << pipe.dci;
			entries = entries.max(pipe.dci);
		}
		(dev.in_virt as *mut u32).write_volatile(drop);
		((dev.in_virt + 4) as *mut u32).write_volatile(add);
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(entries << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		for pipe in pipes {
			let ep_ctx: u64 = dev.in_virt + (1 + pipe.dci as u64) * hc.ctx_size;
			(ep_ctx as *mut u32).write_volatile(pipe.interval << 16);
			((ep_ctx + 4) as *mut u32).write_volatile(pipe.mps << 16 | pipe.ep_type << 3 | 3 << 1);
			((ep_ctx + 8) as *mut u32).write_volatile((pipe.ring.phys | pipe.ring.cycle as u64) as u32);
			((ep_ctx + 12) as *mut u32).write_volatile((pipe.ring.phys >> 32) as u32);
			// The average transfer, and for an interrupt pipe the most one service interval carries.
			let esit: u32 = if pipe.ep_type == 7 || pipe.ep_type == 3 { pipe.mps } else { 0 };
			((ep_ctx + 16) as *mut u32).write_volatile(pipe.mps.min(1024) | esit << 16);
		}
		command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24).is_some()
	}
}

/// Select a configuration, and the alternate setting of one interface in it. Alternate zero is the setting
/// SET_CONFIGURATION leaves an interface in, and a device with nothing else may stall the request, so it is
/// not sent for zero.
pub fn select(hc: &mut Xhci, dev: &mut UsbDevice, config_value: u8, interface: u8, alternate: u8) -> bool {
	let mut none = Hids::new();
	if control_nodata(hc, &mut none, dev, 0x00, REQ_SET_CONFIGURATION, config_value as u16, 0).is_none() {
		return false;
	}
	alternate == 0 || control_nodata(hc, &mut none, dev, RT_INTERFACE_OUT, REQ_SET_INTERFACE, alternate as u16, interface as u16).is_some()
}

/// Every configuration descriptor the device has, read whole, one page at most each.
///
/// READ ONCE AND KEPT, because the data page they arrive in is the page every later control transfer
/// overwrites - a binder walking it after a SET_CONFIGURATION would be walking the answer to that.
///
/// # Safety
/// `dev` is an addressed device this controller owns, and its data page is this driver's mapping.
pub unsafe fn read_configurations(hc: &mut Xhci, dev: &mut UsbDevice) -> Vec<Vec<u8>> {
	unsafe {
		let mut none = Hids::new();
		let count = match control_in_req(hc, &mut none, dev, 0x80, REQ_GET_DESCRIPTOR, (descriptor::DT_DEVICE as u16) << 8, 0, 18) {
			Some(received) if received >= 18 => r8(dev.data_virt + 17).clamp(1, MAX_CONFIGURATIONS),
			_ => 1,
		};
		let mut out: Vec<Vec<u8>> = Vec::new();
		for index in 0..count {
			let Some(head) = control_in_req(hc, &mut none, dev, 0x80, REQ_GET_DESCRIPTOR, DESC_CONFIG << 8 | index as u16, 0, 9) else { continue };
			let head_bytes = core::slice::from_raw_parts(dev.data_virt as *const u8, head.min(9) as usize);
			let Some(record) = descriptor::Walk::new(head_bytes).next() else { continue };
			if descriptor::check_type(descriptor::DT_CONFIG, record.kind).is_err() {
				continue;
			}
			let Ok(total) = record.field16(2) else { continue };
			let total = total.min(CONFIG_BYTES);
			let Some(received) = control_in_req(hc, &mut none, dev, 0x80, REQ_GET_DESCRIPTOR, DESC_CONFIG << 8 | index as u16, 0, total) else { continue };
			let Ok(total) = descriptor::check_transfer(total, received) else { continue };
			out.push(core::slice::from_raw_parts(dev.data_virt as *const u8, total as usize).to_vec());
		}
		out
	}
}

/// Keep a completion a synchronous wait took off the ring, when it belongs to a class pipe. Answers whether
/// it was kept.
///
/// THE SAME HOLE THE NETWORK AND SERIAL ADAPTERS HAD, closed once for all nine: a transfer a class module
/// left outstanding completes whenever the device says so, and a wait for somebody else's transfer that
/// dropped it would leave the module waiting for an event that already came.
pub fn keep(hc: &mut Xhci, pointer: u64, status: u32, control: u32) -> bool {
	if control >> 10 & 0x3f != TRB_EV_TRANSFER || !hc.class_rx.contains(&(control >> 24, control >> 16 & 0x1f)) {
		return false;
	}
	if hc.class_pending.len() >= MAX_KEPT {
		print(b"driver.xhci: more class completions arrived during one wait than there are class pipes - one is dropped\n");
		return true;
	}
	hc.class_pending.push((pointer, status, control));
	true
}

/// Stop a pipe and abandon the transfer on it. Answers how many bytes the abandoned transfer had moved, as
/// the controller reported them - `None` when it reported a length it says is not valid.
///
/// THE STOPPED TRANSFER'S EVENT COMES BEFORE THE COMMAND'S, and a transfer that finished just before the stop
/// reports as finished; either way the event is read here and not left for the loop. The ring is then moved
/// past everything posted, so the next transfer starts clean.
pub fn abandon(hc: &mut Xhci, hids: &mut Hids, pipe: &mut Pipe) -> Option<u32> {
	let mut moved: Option<u32> = Some(0);
	// A completion already kept by an earlier wait is the answer, and the stop then stops nothing.
	if let Some(at) = hc.class_pending.iter().position(|&(_, _, control)| pipe.owns(control)) {
		let (_, status, _) = hc.class_pending.remove(at);
		let (_, bytes) = pipe.complete(status);
		return Some(bytes);
	}
	if !pipe.busy {
		return Some(0);
	}
	command(hc, 0, 0, TRB_STOP_ENDPOINT << 10 | pipe.dci << 16 | pipe.slot << 24);
	let mut spins: u32 = 0;
	loop {
		match unsafe { take_event(hc) } {
			Some((_, status, control)) if control >> 10 & 0x3f == TRB_EV_CMD_COMPLETE => {
				let _ = status;
				break;
			}
			Some((_, status, control)) if pipe.owns(control) => {
				let code = status >> 24;
				let (_, bytes) = pipe.complete(status);
				moved = match code {
					CC_STOPPED_LENGTH_INVALID => None,
					CC_STOPPED | CC_STOPPED_SHORT_PACKET | CC_SUCCESS | CC_SHORT_PACKET => Some(bytes),
					_ => Some(bytes),
				};
			}
			Some((pointer, status, control)) => {
				if !crate::keep_stray(hc, pointer, status, control) {
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
	pipe.busy = false;
	let dequeue = pipe.enqueue();
	command(hc, dequeue, 0, TRB_SET_TR_DEQUEUE << 10 | pipe.dci << 16 | pipe.slot << 24);
	let _ = crate::wait_command(hc, hids);
	moved
}

/// Post one transfer and wait for it, until `deadline` in clock ticks: the code and the bytes moved, or `None`
/// when the device did not finish in time - and the transfer is then abandoned, so nothing is left on the ring.
///
/// FOR A MODULE WHOSE CONTRACT IS A REQUEST AND ITS ANSWER, which a smart-card reader's is: it answers within its
/// own timeout or asks for more time, so the wait is bounded and short, and everything the loop owes anybody
/// else meanwhile - a keystroke, a standing receive - is handled or kept exactly as the other waits do.
pub fn transfer(hc: &mut Xhci, hids: &mut Hids, pipe: &mut Pipe, len: u32, deadline: u64) -> Option<(u32, u32)> {
	if !pipe.busy && !pipe.post(hc, len) {
		return None;
	}
	loop {
		if let Some(at) = hc.class_pending.iter().position(|&(_, _, control)| pipe.owns(control)) {
			let (_, status, _) = hc.class_pending.remove(at);
			return Some(pipe.complete(status));
		}
		match unsafe { take_event(hc) } {
			Some((_, status, control)) if pipe.owns(control) => return Some(pipe.complete(status)),
			Some((pointer, status, control)) => {
				if !crate::keep_stray(hc, pointer, status, control) {
					crate::usb_hid::handle_hid_event(hc, hids, status, control);
				}
			}
			None => {
				if clock() >= deadline {
					let _ = abandon(hc, hids, pipe);
					return None;
				}
				yield_now();
			}
		}
	}
}

/// Recover a pipe the device stalled: the controller's half (Reset Endpoint, then the ring moved past the
/// stalled transfer) and the device's half (CLEAR_FEATURE(ENDPOINT_HALT), which also resets its toggle).
pub fn clear_halt(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, pipe: &mut Pipe) -> bool {
	command(hc, 0, 0, TRB_RESET_ENDPOINT << 10 | pipe.dci << 16 | pipe.slot << 24);
	let reset = crate::wait_command(hc, hids);
	let dequeue = pipe.enqueue();
	command(hc, dequeue, 0, TRB_SET_TR_DEQUEUE << 10 | pipe.dci << 16 | pipe.slot << 24);
	let moved = crate::wait_command(hc, hids);
	pipe.busy = false;
	let address: u16 = (pipe.dci >> 1) as u16 | if pipe.is_in() { 0x80 } else { 0 };
	let cleared = control_nodata(hc, hids, dev, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, address).is_some();
	// RECOVERED ONLY IF ALL THREE HALVES WERE: a controller that refused either command still has the endpoint
	// halted, and the next transfer posted on it would never be run.
	reset == Some(CC_SUCCESS) && moved == Some(CC_SUCCESS) && cleared
}

/// Put a pipe back to where it was when it was brought up: whatever it had outstanding abandoned, its ring
/// emptied, the endpoint dropped and added again - which is what resets the controller's data toggle on an
/// endpoint that is not halted - and the device's end cleared with CLEAR_FEATURE(ENDPOINT_HALT).
///
/// FOR A DEVICE THAT RESET ITS OWN END, which a printer's SOFT_RESET does: a controller still holding the old
/// toggle sends the next packet as a retransmission, and the device drops it without a word.
pub fn reset_pipe(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, pipe: &mut Pipe, entries: u32) -> bool {
	let _ = abandon(hc, hids, pipe);
	unsafe {
		// EVERY TRB BUT THE LINK, and the producer back at the start with the cycle it began with: an
		// endpoint added again starts reading at the ring's base, where the previous pass's transfers are.
		core::ptr::write_bytes(pipe.ring.virt as *mut u8, 0, ((crate::RING_TRBS - 1) * 16) as usize);
		let link: u64 = pipe.ring.virt + (crate::RING_TRBS - 1) * 16;
		((link + 12) as *mut u32).write_volatile(crate::TRB_LINK << 10 | crate::TRB_TOGGLE_CYCLE);
		pipe.ring.index = 0;
		pipe.ring.cycle = 1;
		if !configure_pipes(hc, dev, &[&*pipe], entries) {
			return false;
		}
	}
	let address: u16 = (pipe.dci >> 1) as u16 | if pipe.is_in() { 0x80 } else { 0 };
	control_nodata(hc, hids, dev, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, address).is_some()
}

/// Whether a completion code is a transfer that happened: all of it, or a short packet's worth.
pub fn succeeded(code: u32) -> bool {
	code == CC_SUCCESS || code == CC_SHORT_PACKET
}

pub fn stalled(code: u32) -> bool {
	code == CC_STALL
}

/// Answer a request whose result is a `u32` or an error, AFTER the fact - for a module that answers when the
/// device finishes rather than when the request arrives. The encoding is the generated one: the correlation,
/// a result tag, then the value or the error's code.
pub fn answer_u32(chan: u64, corr: u32, result: Result<u32, proto::system::Error>) -> bool {
	let mut frame = [0u8; 9];
	frame[..4].copy_from_slice(&corr.to_le_bytes());
	let len = match result {
		Ok(value) => {
			frame[4] = 1;
			frame[5..9].copy_from_slice(&value.to_le_bytes());
			9
		}
		Err(error) => {
			frame[4] = 0;
			frame[5] = error as u8;
			6
		}
	};
	send_blocking(chan, &frame[..len], 0)
}

/// The same, for a request whose result is `unit` or an error.
pub fn answer_unit(chan: u64, corr: u32, result: Result<(), proto::system::Error>) -> bool {
	let mut frame = [0u8; 6];
	frame[..4].copy_from_slice(&corr.to_le_bytes());
	let len = match result {
		Ok(()) => {
			frame[4] = 1;
			5
		}
		Err(error) => {
			frame[4] = 0;
			frame[5] = error as u8;
			6
		}
	};
	send_blocking(chan, &frame[..len], 0)
}

/// The correlation of a request, which a module that answers later has to keep.
pub fn correlation(request: &[u8]) -> Option<(u16, u32)> {
	Some((u16::from_le_bytes([*request.first()?, *request.get(1)?]), u32::from_le_bytes([*request.get(2)?, *request.get(3)?, *request.get(4)?, *request.get(5)?])))
}

/// ONE BOUND CLASS MODULE, as the hold drives it.
pub trait Module {
	/// Its budget line.
	fn kind(&self) -> ClassKind;
	/// Its role in the `usb.list` inventory.
	fn inventory(&self) -> u8;
	/// The provider kind and name it is published under.
	fn provider(&self) -> u16;
	fn name(&self) -> &'static [u8];
	fn device(&self) -> &UsbDevice;
	/// Called once, when it is published: post whatever it keeps standing.
	fn start(&mut self, hc: &mut Xhci);
	/// Serve what one consumer sent. False when that consumer's endpoint has closed.
	fn serve(&mut self, hc: &mut Xhci, hids: &mut Hids, chan: u64, bootstrap: u64, bind: &common::Bind) -> bool;
	/// A consumer's connection is over, closed by it or by the withdrawal.
	fn departed(&mut self, hc: &mut Xhci, hids: &mut Hids, chan: u64);
	/// A transfer event, offered to every module; true when it was this one's.
	fn absorb(&mut self, hc: &mut Xhci, hids: &mut Hids, pointer: u64, status: u32, control: u32) -> bool;
	/// The device left, or the driver is going: give back its pipes and pages. The device itself is released
	/// by the hold after this.
	fn release(&mut self, hc: &mut Xhci);
	fn device_mut(&mut self) -> &mut UsbDevice;
	/// Channels of the module's own the loop should wake for - a consumer's transmit channel - and only while the
	/// module can take what arrives on them: one left in the set while its pipe is busy would wake the loop for
	/// ever with nothing it can do.
	fn waits(&self) -> Vec<u64> {
		Vec::new()
	}
	/// One of `waits` has something.
	fn ready(&mut self, _hc: &mut Xhci, _hids: &mut Hids, _handle: u64) {}
	/// The device is leaving ON PURPOSE - a DFU target detaching into its bootloader - and this is what has to
	/// outlive it. The controller holds it, and the module's publication with it, until a device that comes back
	/// in the same place adopts it or its deadline passes.
	fn handoff(&mut self) -> Option<Box<dyn Carry>> {
		None
	}
	/// A device that arrived where a handoff waits: take it over, if this is the device it waits for. Whether it did.
	fn adopt(&mut self, _hc: &mut Xhci, _hids: &mut Hids, _carried: &mut dyn Carry) -> bool {
		false
	}
}

/// WHAT A MODULE CARRIES ACROSS ITS DEVICE'S RE-ENUMERATION. Held by the controller between the device leaving and
/// a device arriving in its place; whatever it owes the consumer that is waiting - an answer - it pays when it
/// expires, so a device that never comes back never leaves a request unanswered.
pub trait Carry {
	/// Where the device that left was: its root port and the route below it.
	fn place(&self) -> (u32, u32);
	/// The clock tick after which it is not waited for.
	fn deadline(&self) -> u64;
	/// It was not adopted in time.
	fn expire(&mut self);
	fn as_any(&mut self) -> &mut dyn core::any::Any;
}

// A handoff waiting for its device, with the publication it keeps alive.
struct Pending {
	carried: Box<dyn Carry>,
	token: u16,
}

struct Bound {
	module: Box<dyn Module>,
	token: u16,
	published: bool,
}

/// The class modules this controller has bound, and what it owes the manager about them.
pub struct Classes {
	bound: Vec<Bound>,
	next_token: u16,
	// Publications whose device has left, to be withdrawn by the loop that holds the manager's channel.
	withdrawn: Vec<u16>,
	// Handoffs whose device left on purpose and is expected back.
	pending: Vec<Pending>,
}

impl Classes {
	pub const fn new() -> Classes {
		Classes { bound: Vec::new(), next_token: FIRST_TOKEN, withdrawn: Vec::new(), pending: Vec::new() }
	}

	pub fn add(&mut self, module: Box<dyn Module>) {
		self.bound.push(Bound { module, token: 0, published: false });
	}

	/// The roles bound, for the report line.
	pub fn roles(&self) -> impl Iterator<Item = u8> + '_ {
		self.bound.iter().map(|bound| bound.module.inventory())
	}

	pub fn owns(&self, token: u16) -> bool {
		token >= FIRST_TOKEN && (self.bound.iter().any(|bound| bound.published && bound.token == token) || self.pending.iter().any(|pending| pending.token == token))
	}

	/// Every module whose device is on this root port leaves: its pipes, its device and its budget line are
	/// given back here, and its publication is withdrawn by `settle`.
	pub fn detach_port(&mut self, hc: &mut Xhci, port: u32) {
		let mut index = 0;
		while index < self.bound.len() {
			if self.bound[index].module.device().port != port {
				index += 1;
				continue;
			}
			let mut gone = self.bound.remove(index);
			// A DEVICE THAT LEFT ON PURPOSE keeps its publication until the device it becomes arrives - or its
			// handoff expires, which answers whoever is waiting and withdraws it then.
			let handoff = if gone.published { gone.module.handoff() } else { None };
			gone.module.release(hc);
			gone.module.device_mut().release(hc);
			hc.budget.release(gone.module.kind());
			print(b"driver.xhci: ");
			print(crate::kind_name(gone.module.inventory()).as_bytes());
			match handoff {
				Some(carried) => {
					self.pending.push(Pending { carried, token: gone.token });
					print(b" detached on purpose - its publication is held for the device it comes back as\n");
				}
				None => {
					if gone.published {
						self.withdrawn.push(gone.token);
					}
					print(b" detached - its publication is withdrawn\n");
				}
			}
		}
	}

	/// Settle what is owed: withdraw what left, and publish what arrived, each under a fresh token.
	pub fn settle(&mut self, hc: &mut Xhci, hids: &mut Hids, bootstrap: u64, bind: &common::Bind, serving: &mut common::Serving) {
		self.expire(bootstrap, bind, serving);
		for token in self.withdrawn.drain(..) {
			// THE CONNECTIONS FIRST: a consumer holding one would otherwise keep a channel to a device that is
			// gone, and learn it only from its next request.
			serving.retire(token);
			common::withdraw(bootstrap, bind, token);
		}
		// A DEVICE THAT CAME BACK WHERE A HANDOFF WAITS is offered it first, and one that adopts it takes the
		// publication the handoff kept - the same token, the same consumers - rather than a new one.
		for bound in self.bound.iter_mut().filter(|bound| !bound.published) {
			let place = (bound.module.device().port, bound.module.device().route);
			let Some(at) = self.pending.iter().position(|pending| pending.carried.place() == place) else { continue };
			if bound.module.adopt(hc, hids, &mut *self.pending[at].carried) {
				let pending = self.pending.remove(at);
				bound.token = pending.token;
				bound.published = true;
				bound.module.start(hc);
			}
		}
		for bound in self.bound.iter_mut().filter(|bound| !bound.published) {
			let token = self.next_token;
			if token == u16::MAX {
				print(b"driver.xhci: no publication token is left for this binding - the class device stays unpublished\n");
				continue;
			}
			let Some((near, far)) = channel() else { continue };
			if !serving.publish(token, near) {
				close(near);
				close(far);
				print(b"driver.xhci: no room for another publication - the class device stays unpublished\n");
				continue;
			}
			if !common::offer_named(bootstrap, bind, bound.module.provider(), token, bound.module.name(), far) {
				serving.retire(token);
				continue;
			}
			self.next_token += 1;
			bound.token = token;
			bound.published = true;
			bound.module.start(hc);
		}
		let _ = hids;
	}

	/// Handoffs whose deadline passed: each answers what it owes, and its publication is withdrawn.
	pub fn expire(&mut self, bootstrap: u64, bind: &common::Bind, serving: &mut common::Serving) {
		let now = clock();
		let mut at = 0;
		while at < self.pending.len() {
			if now <= self.pending[at].carried.deadline() {
				at += 1;
				continue;
			}
			let mut pending = self.pending.remove(at);
			pending.carried.expire();
			serving.retire(pending.token);
			common::withdraw(bootstrap, bind, pending.token);
			print(b"driver.xhci: a device that left on purpose did not come back in time - its publication is withdrawn\n");
		}
	}

	/// Whether a handoff is waiting, which the loop keeps checking the deadline of.
	pub fn waiting(&self) -> bool {
		!self.pending.is_empty()
	}

	/// Serve one consumer of a class publication. Answers whether its endpoint closed; the module has then
	/// already been told.
	pub fn serve(&mut self, hc: &mut Xhci, hids: &mut Hids, token: u16, chan: u64, bootstrap: u64, bind: &common::Bind) -> bool {
		// A PUBLICATION HELD FOR A DEVICE THAT IS NOT BACK YET has nothing to answer with: what arrives is drained,
		// and the consumer's request waiting on the handoff is the one that is answered.
		let Some(bound) = self.bound.iter_mut().find(|bound| bound.published && bound.token == token) else {
			return drain(chan);
		};
		if bound.module.serve(hc, hids, chan, bootstrap, bind) {
			return false;
		}
		bound.module.departed(hc, hids, chan);
		true
	}

	/// Every published module's own channels, for the loop's wait set.
	pub fn waits(&self) -> Vec<u64> {
		self.bound.iter().filter(|bound| bound.published).flat_map(|bound| bound.module.waits()).collect()
	}

	/// One of them has something: the module that owns it takes it.
	pub fn ready(&mut self, hc: &mut Xhci, hids: &mut Hids, handle: u64) {
		if let Some(bound) = self.bound.iter_mut().find(|bound| bound.published && bound.module.waits().contains(&handle)) {
			bound.module.ready(hc, hids, handle);
		}
	}

	/// Offer a transfer event to every bound module.
	pub fn absorb(&mut self, hc: &mut Xhci, hids: &mut Hids, pointer: u64, status: u32, control: u32) -> bool {
		self.bound.iter_mut().any(|bound| bound.module.absorb(hc, hids, pointer, status, control))
	}

	/// The completions synchronous waits kept, oldest first.
	pub fn absorb_kept(&mut self, hc: &mut Xhci, hids: &mut Hids) {
		while !hc.class_pending.is_empty() {
			let (pointer, status, control) = hc.class_pending.remove(0);
			if !self.absorb(hc, hids, pointer, status, control) {
				crate::usb_hid::handle_hid_event(hc, hids, status, control);
			}
		}
	}
}

impl Default for Classes {
	fn default() -> Classes {
		Classes::new()
	}
}

// A request on a publication nothing serves any more: its capabilities are closed and it is not answered.
fn drain(chan: u64) -> bool {
	let mut buf = [0u8; 64];
	loop {
		match try_recv_caps(chan, &mut buf) {
			PolledCaps::Message { handles, .. } => {
				for &handle in handles.as_slice() {
					close(handle);
				}
			}
			PolledCaps::Empty => return false,
			PolledCaps::Closed => return true,
		}
	}
}

/// Offer a device nothing else bound to every class module, in the order below, over every configuration it
/// has. The device comes back when none takes it.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
pub unsafe fn probe(hc: &mut Xhci, mut dev: UsbDevice) -> Result<Box<dyn Module>, UsbDevice> {
	unsafe {
		let configurations = read_configurations(hc, &mut dev);
		for config in &configurations {
			if let Ok(binding) = drivers::ptp::bind(config) {
				if !crate::admits(hc, ClassKind::StillImage, dev.class) {
					return Err(dev);
				}
				return match crate::class_ptp::configure(hc, dev, binding) {
					Ok(module) => {
						let _ = hc.budget.admit(ClassKind::StillImage);
						Ok(Box::new(module))
					}
					Err(dev) => Err(dev),
				};
			}
			if let Ok(binding) = drivers::usb_midi::bind(config) {
				if !crate::admits(hc, ClassKind::Midi, dev.class) {
					return Err(dev);
				}
				return match crate::class_midi::configure(hc, dev, binding) {
					Ok(module) => {
						let _ = hc.budget.admit(ClassKind::Midi);
						Ok(Box::new(module))
					}
					Err(dev) => Err(dev),
				};
			}
			// A HID POWER DEVICE is a HID interface whose report descriptor says so, which only the descriptor can:
			// the input path above took every HID device it could use and left this one.
			for interface in drivers::hid_power::interfaces(config) {
				dev = match crate::class_power::probe(hc, dev, interface) {
					Ok(module) => {
						let _ = hc.budget.admit(ClassKind::PowerDevice);
						return Ok(Box::new(module));
					}
					Err(dev) => dev,
				};
			}
			if let Ok(binding) = drivers::ccid::bind(config) {
				if !crate::admits(hc, ClassKind::SmartCard, dev.class) {
					return Err(dev);
				}
				return match crate::class_ccid::configure(hc, dev, binding) {
					Ok(module) => {
						let _ = hc.budget.admit(ClassKind::SmartCard);
						Ok(Box::new(module))
					}
					Err(dev) => Err(dev),
				};
			}
			if let Ok(binding) = drivers::dfu::bind(config) {
				if !crate::admits(hc, ClassKind::Dfu, dev.class) {
					return Err(dev);
				}
				return match crate::class_dfu::configure(hc, dev, binding) {
					Ok(module) => {
						let _ = hc.budget.admit(ClassKind::Dfu);
						Ok(Box::new(module))
					}
					Err(dev) => Err(dev),
				};
			}
			if let Ok(binding) = drivers::bt_usb::bind(config) {
				if !crate::admits(hc, ClassKind::Bluetooth, dev.class) {
					return Err(dev);
				}
				return match crate::class_bt::configure(hc, dev, binding) {
					Ok(module) => {
						let _ = hc.budget.admit(ClassKind::Bluetooth);
						Ok(Box::new(module))
					}
					Err(dev) => Err(dev),
				};
			}
			if let Ok(binding) = drivers::mbim_cid::bind(config) {
				if !crate::admits(hc, ClassKind::Mbim, dev.class) {
					return Err(dev);
				}
				return match crate::class_mbim::configure(hc, dev, binding) {
					Ok(module) => {
						let _ = hc.budget.admit(ClassKind::Mbim);
						Ok(Box::new(module))
					}
					Err(dev) => Err(dev),
				};
			}
			match drivers::uvc::bind(config) {
				Ok(binding) => {
					if !crate::admits(hc, ClassKind::Video, dev.class) {
						return Err(dev);
					}
					return match crate::class_uvc::configure(hc, dev, binding) {
						Ok(module) => {
							let _ = hc.budget.admit(ClassKind::Video);
							Ok(Box::new(module))
						}
						Err(dev) => Err(dev),
					};
				}
				// SAID, BECAUSE A CAMERA LEFT SILENT IS A CAMERA NOBODY KNOWS IS UNSUPPORTED.
				Err(drivers::uvc::NotBindable::Isochronous) => print(b"driver.xhci: a video camera is on the bus and streams isochronously, which this transport does not drive\n"),
				Err(_) => {}
			}
			if let Ok(binding) = drivers::printer::bind(config) {
				if !crate::admits(hc, ClassKind::Printer, dev.class) {
					return Err(dev);
				}
				return match crate::class_printer::configure(hc, dev, binding) {
					Ok(module) => {
						let _ = hc.budget.admit(ClassKind::Printer);
						Ok(Box::new(module))
					}
					Err(dev) => Err(dev),
				};
			}
		}
		Err(dev)
	}
}
