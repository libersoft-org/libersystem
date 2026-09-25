// The USB Video class side of driver.xhci: a camera on the bus, published as the `camera` provider CameraService
// drives over `liber:camera-device`.
//
// A PROVIDER NORMALIZES AND WRITES; THE SERVICE DECIDES. The streaming interface's descriptors go through the same
// staged normalizer the in-guest fixture's do, a stream is negotiated with the device's own probe and commit, and
// assembled frames are written into the buffers the service queued - and only while queued, each handed back under
// the lease it was given. No frame is decoded here: YUY2 is carried as it arrives, and a consumer that wants pixels
// uses the codec leaves.
//
// BULK STREAMING, ONE PAYLOAD A TRANSFER. A bulk camera sends each payload as one transfer, its header first: which
// frame the data belongs to (FID), whether the frame ends there (EOF), whether the device knows it is bad (ERR).
// `drivers::uvc::Assembler` reads those bits and decides where each payload's data goes; this module is its sink,
// the leased buffers. A frame the device marked bad, one that overran its buffer, a YUY2 frame of the wrong length
// and a compressed frame no EOF ended are DROPPED with the device as the reason - and their buffer stays queued for
// the next. A frame that found no queued buffer is dropped with that reason. Isochronous streaming is not this
// transport's.

use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{CameraDeviceEvent, CameraDrop, CameraDropReason, CameraFrame, CameraOpen, ColorMatrix, ColorRange, DiscreteIntervals, Error, FormatInfo, FrameSize, FrameType, Interval, IntervalRange, Intervals, Negotiated, StreamRequest, camera_device};
use rt::*;

use crate::classes::{self, Module, Pipe};
use crate::usb_hid::Hids;
use crate::{FEATURE_ENDPOINT_HALT, KIND_CAMERA, REQ_CLEAR_FEATURE, RT_ENDPOINT, UsbDevice, Xhci, control_in_req, control_nodata, control_out_req, r8};
use drivers::common;
use drivers::usb_class::ClassKind;
use drivers::uvc::{self, Binding};

const VERSION: u32 = 1;
const REQUEST_BYTES: usize = 256;
const STREAM_DEPTH: u64 = 64;
// A payload is one transfer, and a transfer is a page at most.
const MAX_PAYLOAD: u32 = 4096;

struct Slot {
	id: u8,
	handle: u64,
	base: u64,
	bytes: u64,
	queued: Option<u64>,
}

struct Stream {
	generation: u64,
	selection: uvc::Selection,
	slots: Vec<Slot>,
	running: bool,
	sequence: u64,
	max_payload: u32,
	assembler: uvc::Assembler,
	/// The slot the open frame is going into, and the lease it was queued under.
	current: Option<(usize, u64)>,
}

// THE ASSEMBLER'S SINK: the consumer's buffers. A frame takes the first queued one, is written only inside it, and
// ends with the slot it had - handed to `finish`, which decides what the consumer is told.
struct Sink<'a> {
	slots: &'a mut [Slot],
	current: &'a mut Option<(usize, u64)>,
	ended: Vec<(Option<(usize, u64)>, uvc::Assembled)>,
}

impl uvc::FrameSink for Sink<'_> {
	fn open(&mut self) -> Option<u64> {
		let at = self.slots.iter().position(|slot| slot.queued.is_some())?;
		let lease = self.slots[at].queued.take().unwrap_or(0);
		*self.current = Some((at, lease));
		Some(self.slots[at].bytes)
	}

	fn write(&mut self, offset: u32, data: &[u8]) {
		let Some((at, _)) = *self.current else { return };
		let slot = &self.slots[at];
		// SAFETY: `base` maps the whole buffer, `slot.bytes` long, writable because the handle carries `write`; the
		// assembler writes only inside the capacity `open` returned, which is `slot.bytes`; nothing else writes the
		// buffer while a frame is going into it.
		unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), (slot.base + u64::from(offset)) as *mut u8, data.len()) };
	}

	fn finish(&mut self, frame: uvc::Assembled) {
		self.ended.push((self.current.take(), frame));
	}
}

pub struct Uvc {
	dev: UsbDevice,
	binding: Binding,
	normalized: uvc::Normalized,
	input: Pipe,
	connection: u64,
	consumer: u64,
	stream: Option<Stream>,
	events: u64,
	event_seq: u32,
	buf: Vec<u8>,
}

fn interval(units: u32) -> Interval {
	Interval { numerator: units, denominator: uvc::INTERVAL_UNITS_PER_SECOND }
}

fn wire_format(format: &uvc::Format) -> FormatInfo {
	FormatInfo {
		index: format.index,
		frame_type: if format.kind == uvc::Kind::Yuy2 { FrameType::Yuy2 } else { FrameType::Mjpeg },
		compressed: format.kind == uvc::Kind::Mjpeg,
		sizes: format.sizes.len() as u8,
		range: match format.range {
			uvc::Range::Unknown => ColorRange::Unknown,
			uvc::Range::Limited => ColorRange::Limited,
			uvc::Range::Full => ColorRange::Full,
		},
		matrix: match format.matrix {
			uvc::Matrix::Unknown => ColorMatrix::Unknown,
			uvc::Matrix::Bt601 => ColorMatrix::Bt601,
			uvc::Matrix::Bt709 => ColorMatrix::Bt709,
		},
	}
}

fn wire_size(size: &uvc::Size) -> FrameSize {
	let intervals = match &size.intervals {
		uvc::Intervals::Discrete(values) => Intervals::Discrete(DiscreteIntervals { values: values.iter().map(|&units| interval(units)).collect() }),
		uvc::Intervals::Stepwise { minimum, maximum, step } => Intervals::Stepwise(IntervalRange { minimum: interval(*minimum), maximum: interval(*maximum), step: interval(*step) }),
	};
	FrameSize { index: size.index, width: size.width, height: size.height, max_bytes: size.max_bytes, intervals }
}

/// Normalize a bound camera's descriptors, bring its streaming pipe up and select its configuration.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
pub unsafe fn configure(hc: &mut Xhci, mut dev: UsbDevice, binding: Binding) -> Result<Uvc, UsbDevice> {
	unsafe {
		let normalized = match uvc::normalize(&binding.graph) {
			Ok(normalized) if !normalized.formats.is_empty() => normalized,
			_ => {
				print(b"driver.xhci: a camera's streaming descriptors were refused by the normalizer, or offer nothing it carries\n");
				return Err(dev);
			}
		};
		let Some(mut input) = Pipe::new(dev.slot, dev.speed, &binding.bulk_in) else {
			print(b"driver.xhci: a camera is on the bus and no pages were left for its pipe\n");
			return Err(dev);
		};
		if !classes::configure_pipes(hc, &mut dev, &[&input], 0) || !classes::select(hc, &mut dev, binding.config_value, binding.streaming_interface, 0) {
			input.release(hc);
			print(b"driver.xhci: a camera's streaming endpoint or its configuration was refused\n");
			return Err(dev);
		}
		let mut line: common::Bounded<96> = common::Bounded::new();
		line.push(b"driver.xhci: video camera bound - bulk streaming, ");
		line.decimal(normalized.formats.len() as u64);
		line.push(b" format(s)\n");
		print(line.as_bytes());
		Ok(Uvc { dev, binding, normalized, input, connection: 0, consumer: 0, stream: None, events: 0, event_seq: 0, buf: alloc::vec![0u8; REQUEST_BYTES] })
	}
}

impl Uvc {
	fn event(&mut self, event: &CameraDeviceEvent) {
		if self.events == 0 {
			return;
		}
		let mut frame = [0u8; 256];
		let mut handles = wire::Handles::new();
		if let Some(len) = camera_device::events_frame(self.event_seq, event, &mut frame, &mut handles)
			&& try_send(self.events, &frame[..len], 0)
		{
			self.event_seq += 1;
		}
	}

	// Every mapping of every client buffer released: the stream is gone from this side.
	fn release_all(&mut self) {
		if let Some(stream) = self.stream.take() {
			for slot in stream.slots {
				unmap_object(slot.handle);
				close(slot.handle);
			}
		}
	}

	// A frame over: delivered into its buffer, or dropped with its reason and its buffer queued again.
	fn finish(&mut self, slot: Option<(usize, u64)>, frame: uvc::Assembled) {
		let Some(stream) = self.stream.as_mut() else { return };
		let sequence = stream.sequence;
		stream.sequence += 1;
		let generation = stream.generation;
		let expected = match stream.selection.kind {
			uvc::Kind::Yuy2 => Some(stream.selection.stride * u32::from(stream.selection.height)),
			uvc::Kind::Mjpeg => None,
		};
		let event = match slot {
			None => CameraDeviceEvent::Dropped(CameraDrop { stream_generation: generation, first_sequence: sequence, count: 1, reason: CameraDropReason::NoBuffer }),
			Some((at, lease)) => {
				if frame.whole(expected) {
					CameraDeviceEvent::Frame(CameraFrame { stream_generation: generation, sequence, buffer: stream.slots[at].id, lease, valid_bytes: frame.written, arrival_ns: clock_ns(), device: None })
				} else {
					// THE BUFFER GOES BACK TO THE QUEUE under the lease it had: it was never handed over.
					stream.slots[at].queued = Some(lease);
					CameraDeviceEvent::Dropped(CameraDrop { stream_generation: generation, first_sequence: sequence, count: 1, reason: CameraDropReason::Device })
				}
			}
		};
		self.event(&event);
	}

	// One payload, into the frame it belongs to - and every frame it ended, reported.
	fn payload(&mut self, transfer: &[u8]) {
		let Some(stream) = self.stream.as_mut() else { return };
		let limit = stream.selection.max_bytes;
		let mut sink = Sink { slots: &mut stream.slots, current: &mut stream.current, ended: Vec::new() };
		stream.assembler.feed(transfer, limit, &mut sink);
		for (slot, frame) in sink.ended {
			self.finish(slot, frame);
		}
	}

	fn stream(&mut self, generation: u64) -> Result<&mut Stream, Error> {
		self.stream.as_mut().filter(|stream| stream.generation == generation).ok_or(Error::Stale)
	}

	fn post(&mut self, hc: &mut Xhci) {
		if let Some(stream) = self.stream.as_ref()
			&& stream.running
			&& !self.input.busy
		{
			let length = stream.max_payload;
			self.input.post(hc, length);
		}
	}

	// STOP ON THE DEVICE'S SIDE: a bulk camera stops streaming when its endpoint's halt is cleared.
	fn halt(&mut self, hc: &mut Xhci, hids: &mut Hids) {
		let _ = classes::abandon(hc, hids, &mut self.input);
		let address = self.binding.bulk_in.address as u16;
		let _ = control_nodata(hc, hids, &mut self.dev, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, address);
	}
}

struct View<'a> {
	uvc: &'a mut Uvc,
	hc: &'a mut Xhci,
	hids: &'a mut Hids,
}

impl camera_device::Service for View<'_> {
	fn open(&mut self, version: u32) -> Result<CameraOpen, Error> {
		if version != VERSION {
			return Err(Error::Unsupported);
		}
		self.uvc.connection += 1;
		Ok(CameraOpen { connection_generation: self.uvc.connection, name: String::from("USB camera"), generation: 1 })
	}

	fn formats(&mut self) -> Result<Vec<FormatInfo>, Error> {
		Ok(self.uvc.normalized.formats.iter().map(wire_format).collect())
	}

	fn sizes(&mut self, format: u8) -> Result<Vec<FrameSize>, Error> {
		let found = self.uvc.normalized.formats.iter().find(|candidate| candidate.index == format).ok_or(Error::NotFound)?;
		Ok(found.sizes.iter().map(wire_size).collect())
	}

	// THE NORMALIZER'S SELECTION, PROBED AND COMMITTED: what the device answers must be what was asked for, or the
	// negotiation is refused rather than quietly different.
	fn negotiate(&mut self, generation: u64, request: StreamRequest) -> Result<Negotiated, Error> {
		if self.uvc.stream.as_ref().is_some_and(|stream| stream.running) {
			return Err(Error::Again);
		}
		if request.interval.denominator != uvc::INTERVAL_UNITS_PER_SECOND {
			return Err(Error::Unsupported);
		}
		let selection = uvc::select(&self.uvc.normalized, request.format, request.size, request.interval.numerator).ok_or(Error::Unsupported)?;
		let format = self.uvc.normalized.formats.iter().find(|candidate| candidate.index == request.format).ok_or(Error::NotFound)?;
		let info = wire_format(format);
		let length = self.uvc.binding.probe_length();
		let interface = self.uvc.binding.streaming_interface as u16;
		let wanted = uvc::probe(selection.format, selection.size, selection.interval, length);
		unsafe { core::ptr::copy_nonoverlapping(wanted.as_ptr(), self.uvc.dev.data_virt as *mut u8, wanted.len()) };
		control_out_req(self.hc, self.hids, &mut self.uvc.dev, uvc::RT_CLASS_INTERFACE_OUT, uvc::SET_CUR, (uvc::VS_PROBE_CONTROL as u16) << 8, interface, length).ok_or(Error::Io)?;
		let received = control_in_req(self.hc, self.hids, &mut self.uvc.dev, uvc::RT_CLASS_INTERFACE_IN, uvc::GET_CUR, (uvc::VS_PROBE_CONTROL as u16) << 8, interface, length).ok_or(Error::Io)?;
		let answered: Vec<u8> = (0..received.min(length as u32) as u64).map(|at| unsafe { r8(self.uvc.dev.data_virt + at) }).collect();
		let probed = uvc::probed(&answered).ok_or(Error::Io)?;
		if (probed.format, probed.frame, probed.interval) != (selection.format, selection.size, selection.interval) {
			return Err(Error::Unsupported);
		}
		// ONE PAYLOAD A TRANSFER, AND A TRANSFER A PAGE: a device that sends larger payloads is not one this
		// transport can cut into frames.
		if probed.max_payload == 0 || probed.max_payload > MAX_PAYLOAD {
			return Err(Error::Unsupported);
		}
		unsafe { core::ptr::copy_nonoverlapping(answered.as_ptr(), self.uvc.dev.data_virt as *mut u8, answered.len()) };
		control_out_req(self.hc, self.hids, &mut self.uvc.dev, uvc::RT_CLASS_INTERFACE_OUT, uvc::SET_CUR, (uvc::VS_COMMIT_CONTROL as u16) << 8, interface, answered.len() as u16).ok_or(Error::Io)?;
		self.uvc.release_all();
		self.uvc.stream = Some(Stream { generation, selection, slots: Vec::new(), running: false, sequence: 0, max_payload: probed.max_payload, assembler: uvc::Assembler::new(), current: None });
		Ok(Negotiated { stream_generation: generation, format: selection.format, frame_type: info.frame_type, width: selection.width, height: selection.height, interval: request.interval, max_bytes: selection.max_bytes, stride: selection.stride, plane_offset: 0, range: info.range, matrix: info.matrix })
	}

	fn register(&mut self, generation: u64, buffer: u8, memory: u64) -> Result<(), Error> {
		let Some(object) = object_info(memory) else {
			close(memory);
			return Err(Error::Invalid);
		};
		let Ok(stream) = self.uvc.stream(generation) else {
			close(memory);
			return Err(Error::Stale);
		};
		if stream.running || stream.slots.iter().any(|slot| slot.id == buffer) {
			close(memory);
			return Err(Error::Invalid);
		}
		// SAFETY: mapping a memory object this process was handed; the base is used only while it is mapped.
		let Some(base) = (unsafe { map_object(memory) }) else {
			close(memory);
			return Err(Error::Denied);
		};
		stream.slots.push(Slot { id: buffer, handle: memory, base, bytes: object.size, queued: None });
		Ok(())
	}

	fn queue(&mut self, generation: u64, buffer: u8, lease: u64) -> Result<(), Error> {
		let stream = self.uvc.stream(generation)?;
		let slot = stream.slots.iter_mut().find(|slot| slot.id == buffer).ok_or(Error::NotFound)?;
		slot.queued = Some(lease);
		Ok(())
	}

	fn start(&mut self, generation: u64) -> Result<(), Error> {
		let stream = self.uvc.stream(generation)?;
		stream.running = true;
		self.uvc.post(self.hc);
		Ok(())
	}

	// STOP: no more writes, the device told, every buffer back and every mapping released - and then the answer.
	fn stop(&mut self, generation: u64) -> Result<(), Error> {
		self.uvc.stream(generation)?;
		if let Some(stream) = self.uvc.stream.as_mut() {
			stream.running = false;
		}
		self.uvc.halt(self.hc, self.hids);
		self.uvc.release_all();
		Ok(())
	}

	fn events(&mut self) -> Vec<CameraDeviceEvent> {
		Vec::new()
	}
}

impl Module for Uvc {
	fn kind(&self) -> ClassKind {
		ClassKind::Video
	}

	fn inventory(&self) -> u8 {
		KIND_CAMERA
	}

	fn provider(&self) -> u16 {
		driver_protocol::provider::CAMERA
	}

	fn name(&self) -> &'static [u8] {
		driver_protocol::provider::USB_VIDEO_NAME
	}

	fn device(&self) -> &UsbDevice {
		&self.dev
	}

	fn device_mut(&mut self) -> &mut UsbDevice {
		&mut self.dev
	}

	fn start(&mut self, _hc: &mut Xhci) {}

	fn serve(&mut self, hc: &mut Xhci, hids: &mut Hids, chan: u64, _bootstrap: u64, _bind: &common::Bind) -> bool {
		let mut buf = core::mem::take(&mut self.buf);
		let polled = try_recv_caps(chan, &mut buf);
		self.buf = buf;
		let (len, mut handles) = match polled {
			PolledCaps::Message { len, handles } => (len, handles),
			PolledCaps::Empty => return true,
			PolledCaps::Closed => return false,
		};
		let request = self.buf[..len].to_vec();
		if self.consumer == 0 {
			self.consumer = chan;
		}
		if classes::correlation(&request).map(|(op, _)| op) == Some(camera_device::OP_EVENTS) {
			let mut view = View { uvc: self, hc, hids };
			let Some((corr, _)) = camera_device::events_open(&mut view, &request, &mut handles) else { return true };
			let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
			if self.events != 0 {
				close(self.events);
			}
			self.events = producer;
			self.event_seq = 0;
			send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
			return true;
		}
		let mut reply = [0u8; 2048];
		let mut reply_handles = wire::Handles::new();
		let mut view = View { uvc: self, hc, hids };
		if let Some(written) = camera_device::dispatch(&mut view, &request, &mut handles, &mut reply, &mut reply_handles) {
			send_caps_blocking(chan, &reply[..written], reply_handles.as_slice());
		}
		for &handle in handles.as_slice() {
			close(handle);
		}
		true
	}

	fn departed(&mut self, hc: &mut Xhci, hids: &mut Hids, chan: u64) {
		if self.consumer != chan {
			return;
		}
		self.consumer = 0;
		if self.stream.as_ref().is_some_and(|stream| stream.running) {
			self.halt(hc, hids);
		}
		self.release_all();
		if self.events != 0 {
			close(self.events);
			self.events = 0;
		}
	}

	fn absorb(&mut self, hc: &mut Xhci, hids: &mut Hids, _pointer: u64, status: u32, control: u32) -> bool {
		if !self.input.owns(control) {
			return false;
		}
		let (code, moved) = self.input.complete(status);
		if self.stream.as_ref().is_some_and(|stream| stream.running) {
			if classes::succeeded(code) && moved > 0 {
				let transfer = self.input.read(moved as usize);
				self.payload(&transfer);
			} else if classes::stalled(code) {
				let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.input);
			}
		}
		self.post(hc);
		true
	}

	fn release(&mut self, hc: &mut Xhci) {
		self.release_all();
		if self.events != 0 {
			close(self.events);
			self.events = 0;
		}
		self.input.release(hc);
	}
}
