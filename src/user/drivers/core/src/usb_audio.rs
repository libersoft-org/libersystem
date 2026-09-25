// The USB Audio Class side of driver.xhci: isochronous sinks and sources, the walk that chose them, and the
// standing capture that keeps a source's transfers posted.
//
// ISOCHRONOUS IS A TRANSFER TYPE NOTHING ELSE HERE USES, and that is most of this module. The bus
// RESERVES BANDWIDTH for one and delivers late rather than not at all: there is no retry, no stall
// to clear and no completion code that means "try again". A driver that treated a short or missing
// completion the way it treats a bulk one would be waiting for an answer the transport is defined
// not to give.
//
// WHAT IT PLAYS AND RECORDS IS DECIDED IN `drivers::uac` AND HELD BY FIXTURES: the format descriptor,
// the alternate setting that carries an endpoint at all, and the arithmetic that turns one period of
// this system's wire into the packets an endpoint's size allows.
//
// ONE PROVIDER, TWO DIRECTIONS, UP TO TWO DEVICES. The PCM wire is one channel that carries playback and
// capture, and AudioService opens one provider - so a period goes to whichever device has a sink and a
// capture to whichever has a source. A headset is both; QEMU's speaker and a microphone are two.
//
// A CAPTURE STANDS WHILE IT RUNS. From the first `CMD_CAPTURE` until `CMD_CAPTURE_STOP` the source's
// capture setting is selected and `CAPTURE_POSTED` isochronous IN transfers stay posted, each re-posted as
// it completes and each completion appended to a FIFO of eight periods; a capture request is answered from
// the FIFO with exactly one period, at once or when one is complete. A FIFO that is full when a completion
// arrives is an OVERRUN, and it ends the capture: the wire has no shape for "samples were lost here", so the
// next request is refused - which AudioService turns into a recording that is no longer available - rather
// than answered with a stream that has a hole in it.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use rt::*;

use crate::usb_hid::Hids;
use crate::{CC_SHORT_PACKET, CC_SUCCESS, DESC_CONFIG, REQ_GET_DESCRIPTOR, REQ_SET_CONFIGURATION, TRB_CONFIGURE_ENDPOINT, TRB_EV_CMD_COMPLETE, TRB_IOC, TRB_SET_TR_DEQUEUE};
use crate::{Ring, UsbDevice, Xhci};
use crate::{command, command_and_wait, control_in_req, control_nodata, control_out_req, dma_page, keep_stray, r8, take_event, w32, wait_command, wait_transfer};
use driver_protocol::audio;
use drivers::common;
use drivers::descriptor;
use drivers::uac::{self, Direction};
use drivers::usb_class::CAPTURE_POSTED;

const REQ_SET_INTERFACE: u8 = 0x0b;
const RT_INTERFACE_OUT: u8 = 0x01;
// UAC1 SET_CUR on an endpoint, and the sampling frequency control it addresses.
const RT_CLASS_ENDPOINT_OUT: u8 = 0x22;
const UAC_SET_CUR: u8 = 0x01;
const UAC_SAMPLING_FREQ_CONTROL: u16 = 0x01;

/// The isochronous TRB type, and the endpoint types for an isochronous OUT and IN pipe.
///
/// ONE AND FIVE ARE OUT AND IN, and six and two are the bulk pair every other module here uses. A
/// driver that configured an isochronous endpoint as bulk asks the controller for a pipe the device
/// does not have; one that posted a Normal TRB on an isochronous ring posts a transfer the
/// controller has no schedule for.
const TRB_ISOCH: u32 = 5;
const EP_TYPE_ISOCH_OUT: u32 = 1;
const EP_TYPE_ISOCH_IN: u32 = 5;
const TRB_STOP_ENDPOINT: u32 = 15;
// A transfer ring an isochronous endpoint found empty when its interval came round: an event with no transfer
// behind it, which completes nothing that was posted.
const CC_RING_UNDERRUN: u32 = 14;
const CC_RING_OVERRUN: u32 = 15;
// The xHCI speed of a high-speed device, whose isochronous interval is microframes rather than frames.
const SPEED_HIGH: u32 = 3;

/// Start this transfer as soon as the schedule allows, rather than naming a frame.
///
/// A FRAME ID IS A PROMISE ABOUT WHEN, and a driver that names one has to know what frame the
/// controller is on and how far ahead it may schedule. `SIA` is the transport saying "you choose",
/// which for a sink whose consumer paces it is the right answer: the period arrives when it arrives.
const TRB_START_ISOCH_ASAP: u32 = 1 << 31;

/// The captured bytes a source may hold for its consumer: eight periods, about 85 ms.
pub const CAPTURE_FIFO: usize = 8 * audio::PERIOD_BYTES as usize;

/// A bound audio sink.
pub struct Sink {
	dci: u32,
	ring: Ring,
	/// The page one period is staged in before the controller reads it, and its handle.
	page: u64,
	virt: u64,
	phys: u64,
	binding: uac::Binding,
}

/// A bound audio source and its standing capture.
pub struct Source {
	pub dci: u32,
	ring: Ring,
	page: u64,
	virt: u64,
	phys: u64,
	binding: uac::Binding,
	/// How many packet buffers the page holds, and so how many transfers may stand.
	slots: u32,
	running: bool,
	posted: u32,
	/// The buffer the next transfer posted lands in, and the one the oldest standing transfer does.
	next: u32,
	done: u32,
	fifo: VecDeque<u8>,
	/// The capture was ended by an overrun, and is refused until it is stopped.
	overrun: bool,
	dropped: u64,
	/// The consumer channel a capture request is waiting on, or zero.
	pub waiting: u64,
}

/// One audio device's streams.
pub struct Audio {
	pub sink: Option<Sink>,
	pub source: Option<Source>,
}

impl Sink {
	/// What this sink is, for the line the driver reports itself with.
	pub fn describes(&self) -> (u8, u8, u16) {
		(self.binding.format.channels, self.binding.format.bits, self.binding.max_packet)
	}
}

impl Source {
	pub fn describes(&self) -> (u8, u8, u16) {
		(self.binding.format.channels, self.binding.format.bits, self.binding.max_packet)
	}
}

impl Audio {
	pub fn release(&mut self) {
		if let Some(sink) = self.sink.as_mut() {
			sink.ring.release();
			close(sink.page);
		}
		if let Some(source) = self.source.as_mut() {
			// A CAPTURE WAITING WHEN ITS SOURCE LEAVES is refused rather than left waiting for ever.
			if source.waiting != 0 {
				send_blocking(source.waiting, audio::REFUSED, 0);
				source.waiting = 0;
			}
			source.ring.release();
			close(source.page);
		}
	}
}

/// The audio devices bound here: at most two, and at most one sink and one source among them.
pub struct Devices {
	pub list: Vec<(UsbDevice, Audio)>,
}

impl Devices {
	pub const fn new() -> Devices {
		Devices { list: Vec::new() }
	}

	pub fn is_empty(&self) -> bool {
		self.list.is_empty()
	}

	pub fn has_sink(&self) -> bool {
		self.list.iter().any(|(_, audio)| audio.sink.is_some())
	}

	pub fn has_source(&self) -> bool {
		self.list.iter().any(|(_, audio)| audio.source.is_some())
	}

	pub fn sink(&mut self) -> Option<(&mut UsbDevice, &mut Sink)> {
		self.list.iter_mut().find_map(|(dev, audio)| audio.sink.as_mut().map(|sink| (dev, sink)))
	}

	pub fn source(&mut self) -> Option<(&mut UsbDevice, &mut Source)> {
		self.list.iter_mut().find_map(|(dev, audio)| audio.source.as_mut().map(|source| (dev, source)))
	}

	/// Take every audio device on `port` off the bus: its streams released and its device handed back to the
	/// caller, which releases its slot and its budget line.
	pub fn take_port(&mut self, port: u32) -> Vec<UsbDevice> {
		let mut gone = Vec::new();
		let mut at = 0;
		while at < self.list.len() {
			if self.list[at].0.port == port {
				let (dev, mut audio) = self.list.remove(at);
				audio.release();
				gone.push(dev);
			} else {
				at += 1;
			}
		}
		gone
	}

	/// The consumer channel closed: a capture waiting on it has nobody to answer.
	pub fn consumer_closed(&mut self, channel: u64) {
		if let Some((_, source)) = self.source()
			&& source.waiting == channel
		{
			source.waiting = 0;
		}
	}
}

impl Default for Devices {
	fn default() -> Devices {
		Devices::new()
	}
}

// The context interval exponent for an isochronous endpoint: log2 of microframes. The descriptor's `bInterval`
// is 2^(n-1) FRAMES at full speed and 2^(n-1) microframes at high speed - so one frame is an exponent of three,
// and a driver that wrote the descriptor's number gets a schedule eight times too fast, which the controller
// answers with underruns.
fn interval_exponent(speed: u32, interval: u8) -> u32 {
	let n = interval.clamp(1, 16) as u32;
	if speed == SPEED_HIGH { n - 1 } else { n - 1 + 3 }
}

// One endpoint context in the input context, for an isochronous pipe in `direction`.
unsafe fn write_endpoint(hc: &Xhci, dev: &UsbDevice, dci: u32, kind: u32, binding: &uac::Binding, ring: &Ring) {
	unsafe {
		let ep_ctx: u64 = dev.in_virt + (1 + dci as u64) * hc.ctx_size;
		(ep_ctx as *mut u32).write_volatile(interval_exponent(dev.speed, binding.interval) << 16);
		// ERROR COUNT ZERO, WHICH IS WHAT ISOCHRONOUS MEANS. The bulk endpoints here write three -
		// retry this many times - and an isochronous endpoint that retried would be delivering last
		// interval's audio in this one.
		((ep_ctx + 4) as *mut u32).write_volatile((binding.max_packet as u32) << 16 | kind << 3);
		((ep_ctx + 8) as *mut u32).write_volatile((ring.phys | ring.cycle as u64) as u32);
		((ep_ctx + 12) as *mut u32).write_volatile((ring.phys >> 32) as u32);
		// The average transfer and the most one service interval carries, which for a periodic endpoint is
		// the packet it moves every time.
		((ep_ctx + 16) as *mut u32).write_volatile(binding.max_packet as u32 | (binding.max_packet as u32) << 16);
	}
}

/// Walk a device's configurations for the streams this system's wire can drive - a sink if `sink` is
/// wanted, a source if `source` is - and configure what was found.
pub unsafe fn configure_audio(hc: &mut Xhci, dev: &mut UsbDevice, sink: bool, source: bool) -> Option<Audio> {
	unsafe {
		let mut hids: Hids = Hids::new();
		let configurations = match control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, (descriptor::DT_DEVICE as u16) << 8, 0, 18) {
			Some(received) if received >= 18 => r8(dev.data_virt + 17).max(1),
			_ => 1,
		};
		let mut chosen: Option<(Option<uac::Binding>, Option<uac::Binding>)> = None;
		let mut refusal: Option<uac::NotBindable> = None;
		for index in 0..configurations.min(8) {
			let Some(head) = control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, DESC_CONFIG << 8 | index as u16, 0, 9) else {
				continue;
			};
			let head_bytes = core::slice::from_raw_parts(dev.data_virt as *const u8, head.min(9) as usize);
			let Some(head_record) = descriptor::Walk::new(head_bytes).next() else { continue };
			if descriptor::check_type(descriptor::DT_CONFIG, head_record.kind).is_err() {
				continue;
			}
			let Ok(total) = head_record.field16(2) else { continue };
			let total = total.min(1024);
			let Some(received) = control_in_req(hc, &mut hids, dev, 0x80, REQ_GET_DESCRIPTOR, DESC_CONFIG << 8 | index as u16, 0, total) else {
				continue;
			};
			let Ok(total) = descriptor::check_transfer(total, received) else { continue };
			let config_bytes = core::slice::from_raw_parts(dev.data_virt as *const u8, total as usize);
			let found_sink = if sink { uac::bind_for(config_bytes, Direction::Sink) } else { Err(uac::NotBindable::NoUsableFormat) };
			let found_source = if source { uac::bind_for(config_bytes, Direction::Source) } else { Err(uac::NotBindable::NoUsableFormat) };
			if found_sink.is_ok() || found_source.is_ok() {
				chosen = Some((found_sink.ok(), found_source.ok()));
				break;
			}
			refusal = found_sink.err().or(found_source.err());
		}
		let Some((sink_binding, source_binding)) = chosen else {
			// SAID, AND WITH THE REASON, for the reason the CDC module says its own: a class module
			// that answers `None` for every device it is offered is indistinguishable from a broken
			// one.
			print(b"driver.xhci: this device is not an audio sink or source this driver binds - ");
			print(match refusal {
				Some(uac::NotBindable::NoAudioControl) | None => b"no configuration of it carries an audio-control interface".as_slice(),
				Some(uac::NotBindable::NoUsableFormat) => b"no alternate setting offers 48 kHz stereo 16-bit, and this driver refuses rather than resamples",
				Some(uac::NotBindable::Malformed) => b"its configuration descriptor is malformed",
				Some(uac::NotBindable::TooMany) => b"its configuration describes more than this bounded walk follows",
			});
			print(b"\n");
			return None;
		};
		let config_value = sink_binding.or(source_binding)?.config_value;

		if control_nodata(hc, &mut hids, dev, 0x00, REQ_SET_CONFIGURATION, config_value as u16, 0).is_none() {
			print(b"driver.xhci: the audio device refused its configuration\n");
			return None;
		}
		// THE SINK'S ALTERNATE SETTING IS THE WHOLE POINT. Setting zero is the ZERO-BANDWIDTH one by
		// specification - it carries no endpoint at all - so a driver that configured the device and
		// stopped has a streaming interface with nothing on it and a stream that never starts. The
		// CDC data interface has the same trap and it is written down there too. A SOURCE'S IS SELECTED
		// WHEN A CAPTURE STARTS, and put back when it stops: a microphone left streaming spends the bus's
		// bandwidth on samples nobody reads.
		if let Some(bound) = sink_binding
			&& control_nodata(hc, &mut hids, dev, RT_INTERFACE_OUT, REQ_SET_INTERFACE, bound.alternate as u16, bound.streaming_interface as u16).is_none()
		{
			print(b"driver.xhci: the audio device refused the alternate setting that carries its endpoint\n");
			return None;
		}

		let dci_of = |binding: &uac::Binding| (binding.endpoint & 0x0f) as u32 * 2 + u32::from(binding.endpoint & 0x80 != 0);
		let sink_ring = match sink_binding {
			Some(_) => Some(Ring::new()?),
			None => None,
		};
		let source_ring = match source_binding {
			Some(_) => Some(Ring::new()?),
			None => None,
		};
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		let mut add: u32 = 1;
		let mut entries: u32 = 0;
		if let (Some(bound), Some(ring)) = (sink_binding.as_ref(), sink_ring.as_ref()) {
			let dci = dci_of(bound);
			add |= 1 << dci;
			entries = entries.max(dci);
			write_endpoint(hc, dev, dci, EP_TYPE_ISOCH_OUT, bound, ring);
		}
		if let (Some(bound), Some(ring)) = (source_binding.as_ref(), source_ring.as_ref()) {
			let dci = dci_of(bound);
			add |= 1 << dci;
			entries = entries.max(dci);
			write_endpoint(hc, dev, dci, EP_TYPE_ISOCH_IN, bound, ring);
		}
		((dev.in_virt + 4) as *mut u32).write_volatile(add);
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(entries << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		if command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24).is_none() {
			print(b"driver.xhci: the audio device's isochronous endpoints were refused by the controller\n");
			return None;
		}
		let sink = match (sink_binding, sink_ring) {
			(Some(binding), Some(ring)) => {
				let (page, virt, phys) = dma_page()?;
				Some(Sink { dci: dci_of(&binding), ring, page, virt, phys, binding })
			}
			_ => None,
		};
		let source = match (source_binding, source_ring) {
			(Some(binding), Some(ring)) => {
				let (page, virt, phys) = dma_page()?;
				let slots = (4096 / binding.max_packet.max(1) as u32).min(CAPTURE_POSTED).max(1);
				Some(Source { dci: dci_of(&binding), ring, page, virt, phys, binding, slots, running: false, posted: 0, next: 0, done: 0, fifo: VecDeque::new(), overrun: false, dropped: 0, waiting: 0 })
			}
			_ => None,
		};
		Some(Audio { sink, source })
	}
}

/// Play one period, answering whether the controller took it.
///
/// ONE TRANSFER DESCRIPTOR PER SERVICE INTERVAL. An isochronous endpoint carries `max_packet` bytes
/// each time the bus schedules it, so a period is several transfers - and the last one is short,
/// which is not an error: the wire's period and the device's packet size divide evenly only by
/// accident.
pub unsafe fn play(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, sink: &mut Sink, period: &[u8]) -> bool {
	unsafe {
		let bytes = period.len().min(4096);
		for (index, byte) in period[..bytes].iter().enumerate() {
			((sink.virt + index as u64) as *mut u8).write_volatile(*byte);
		}
		let packets = uac::packets_for(bytes as u32, sink.binding.max_packet);
		if packets == 0 {
			return false;
		}
		core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
		for index in 0..packets {
			let span = uac::packet_span(bytes as u32, sink.binding.max_packet, index);
			let at = sink.phys + index as u64 * sink.binding.max_packet as u64;
			// ONLY THE LAST ONE ASKS FOR A COMPLETION. An interrupt per packet is eleven events a
			// period at this size, and what the caller is waiting for is the period, not the packet.
			let last = index + 1 == packets;
			let control = TRB_ISOCH << 10 | TRB_START_ISOCH_ASAP | if last { TRB_IOC } else { 0 };
			sink.ring.push(at, span, control);
		}
		w32(hc.db + dev.slot as u64 * 4, sink.dci);
		// A SHORT PACKET IS A NORMAL COMPLETION HERE. The last transfer of a period is short
		// whenever the period is not a multiple of the packet size, which is every period at this
		// wire's size against this device's.
		match wait_transfer(hc, hids, dev.slot, sink.dci) {
			Some(code) => code == CC_SUCCESS || code == CC_SHORT_PACKET,
			None => false,
		}
	}
}

// Post capture transfers until as many stand as the page has buffers for, and ring the doorbell once.
unsafe fn post_captures(hc: &mut Xhci, dev: &UsbDevice, source: &mut Source) {
	unsafe {
		let mut posted_any = false;
		while source.running && !source.overrun && source.posted < source.slots {
			let at = source.phys + source.next as u64 * source.binding.max_packet as u64;
			// EVERY ONE ASKS FOR ITS COMPLETION, unlike a sink's: what a capture needs from each is how much the
			// device sent, which only its own event says.
			source.ring.push(at, source.binding.max_packet as u32, TRB_ISOCH << 10 | TRB_START_ISOCH_ASAP | TRB_IOC);
			source.next = (source.next + 1) % source.slots;
			source.posted += 1;
			posted_any = true;
		}
		if posted_any {
			w32(hc.db + dev.slot as u64 * 4, source.dci);
		}
	}
}

// Start a capture: the source's streaming setting selected, its rate set where the device has a choice, the
// FIFO empty and the transfers standing.
unsafe fn start_capture(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, source: &mut Source) -> bool {
	unsafe {
		let bound = source.binding;
		if control_nodata(hc, hids, dev, RT_INTERFACE_OUT, REQ_SET_INTERFACE, bound.alternate as u16, bound.streaming_interface as u16).is_none() {
			print(b"driver.xhci: the audio source refused the alternate setting that carries its endpoint\n");
			return false;
		}
		// A RATE IS SET ONLY WHERE THE DEVICE OFFERS MORE THAN ONE. A device with one rate may not implement the
		// control at all, and a stall there is not a reason to refuse a stream that is already at 48 kHz.
		if bound.format.continuous || bound.format.rate_count > 1 {
			let rate = uac::WANTED_RATE_HZ.to_le_bytes();
			core::ptr::copy_nonoverlapping(rate.as_ptr(), dev.data_virt as *mut u8, 3);
			let _ = control_out_req(hc, hids, dev, RT_CLASS_ENDPOINT_OUT, UAC_SET_CUR, UAC_SAMPLING_FREQ_CONTROL << 8, bound.endpoint as u16, 3);
		}
		source.fifo.clear();
		source.overrun = false;
		source.running = true;
		post_captures(hc, dev, source);
		true
	}
}

// Stop a capture: every standing transfer abandoned - the ring moved past them - the zero-bandwidth setting
// selected and the FIFO emptied.
unsafe fn stop_capture(hc: &mut Xhci, hids: &mut Hids, dev: &mut UsbDevice, source: &mut Source) {
	unsafe {
		source.running = false;
		if source.posted > 0 {
			command(hc, 0, 0, TRB_STOP_ENDPOINT << 10 | source.dci << 16 | dev.slot << 24);
			// THE STOPPED TRANSFERS' EVENTS COME BEFORE THE COMMAND'S, and they complete nothing anybody waits for.
			let mut spins: u32 = 0;
			loop {
				match take_event(hc) {
					Some((_, _, control)) if control >> 10 & 0x3f == TRB_EV_CMD_COMPLETE => break,
					Some((_, _, control)) if control >> 24 == dev.slot && control >> 16 & 0x1f == source.dci => {}
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
			let dequeue = source.ring.phys + source.ring.index * 16 | source.ring.cycle as u64;
			command(hc, dequeue, 0, TRB_SET_TR_DEQUEUE << 10 | source.dci << 16 | dev.slot << 24);
			let _ = wait_command(hc, hids);
		}
		source.posted = 0;
		source.done = source.next;
		hc.audio_pending.clear();
		let bound = source.binding;
		let _ = control_nodata(hc, hids, dev, RT_INTERFACE_OUT, REQ_SET_INTERFACE, 0, bound.streaming_interface as u16);
		source.fifo.clear();
		source.overrun = false;
		if source.waiting != 0 {
			send_blocking(source.waiting, audio::REFUSED, 0);
			source.waiting = 0;
		}
	}
}

// Answer a waiting capture from the FIFO, if there is something to answer with.
fn answer_waiting(source: &mut Source) {
	if source.waiting == 0 {
		return;
	}
	if source.overrun {
		send_blocking(source.waiting, audio::REFUSED, 0);
		source.waiting = 0;
	} else if source.fifo.len() >= audio::PERIOD_BYTES as usize {
		let period: Vec<u8> = source.fifo.drain(..audio::PERIOD_BYTES as usize).collect();
		send_blocking(source.waiting, &period, 0);
		source.waiting = 0;
	}
}

/// A capture request on the provider's channel `server`: answered now from the FIFO, or when a period is there.
pub unsafe fn capture(hc: &mut Xhci, hids: &mut Hids, devices: &mut Devices, server: u64) {
	unsafe {
		let Some((dev, source)) = devices.source() else {
			// CAPTURE IS REFUSED AND THE REFUSAL IS THE WIRE'S OWN: an empty reply, which cannot be mistaken
			// for samples because a period is never empty. No device here records.
			send_blocking(server, audio::REFUSED, 0);
			return;
		};
		// ONE REQUEST AT A TIME, as the wire is used; an overrun ends the capture until it is stopped.
		if source.overrun || source.waiting != 0 {
			send_blocking(server, audio::REFUSED, 0);
			return;
		}
		if !source.running && !start_capture(hc, hids, dev, source) {
			send_blocking(server, audio::REFUSED, 0);
			return;
		}
		source.waiting = server;
		answer_waiting(source);
	}
}

/// The end of the capture stream.
pub unsafe fn end_capture(hc: &mut Xhci, hids: &mut Hids, devices: &mut Devices) {
	unsafe {
		if let Some((dev, source)) = devices.source() {
			if source.running || source.overrun {
				let mut line: common::Bounded<128> = common::Bounded::new();
				line.push(b"driver.xhci: audio capture stopped");
				if source.dropped > 0 {
					line.push(b" - ");
					line.decimal(source.dropped);
					line.push(b" bytes were lost to an overrun");
				}
				line.push(b"\n");
				print(line.as_bytes());
			}
			stop_capture(hc, hids, dev, source);
			source.dropped = 0;
		}
	}
}

/// A completion for the source's endpoint: its bytes into the FIFO, its transfer posted again, and a waiting
/// capture answered. Whether the event was the source's.
pub unsafe fn absorb(hc: &mut Xhci, devices: &mut Devices, status: u32, control: u32) -> bool {
	unsafe {
		let Some((dev, source)) = devices.source() else { return false };
		if control >> 24 != dev.slot || control >> 16 & 0x1f != source.dci {
			return false;
		}
		let code = status >> 24;
		// AN EMPTY RING'S EVENT HAS NO TRANSFER BEHIND IT, and counting it as one would lose track of the
		// buffers that do.
		if code == CC_RING_UNDERRUN || code == CC_RING_OVERRUN {
			return true;
		}
		if source.posted == 0 {
			return true;
		}
		let slot = source.done;
		source.done = (source.done + 1) % source.slots;
		source.posted -= 1;
		let packet = source.binding.max_packet as u32;
		let moved = if code == CC_SUCCESS || code == CC_SHORT_PACKET { packet.saturating_sub(status & 0x00ff_ffff) } else { 0 };
		if source.running && !source.overrun && moved > 0 {
			if source.fifo.len() + moved as usize > CAPTURE_FIFO {
				source.overrun = true;
				source.dropped += moved as u64;
				print(b"driver.xhci: audio capture overran its FIFO - the capture is ended, and refused until it is stopped\n");
			} else {
				let base = source.virt + slot as u64 * packet as u64;
				for at in 0..moved as u64 {
					source.fifo.push_back(r8(base + at));
				}
			}
		}
		post_captures(hc, dev, source);
		answer_waiting(source);
		true
	}
}
