// driver.virtio-gpio - a GPIO controller over virtio (device 41), serving the `liber:gpio-device` contract.
//
// NEVER THE WHOLE CONTROLLER. This driver publishes one `gpio-lines` provider, and the endpoint its offer
// carries serves nothing: DeviceManager closes it at publication. Its only connections are the ones
// DeviceManager mints with a SCOPED `CONNECT` naming one line and how it fires, and every operation on such a
// connection is about that line. Each line is held by one connection at a time: a second connection for a
// held line, an unscoped one and one scoped to an address are refused on the connection itself, by closing it.
//
// INPUT LINES ONLY. A connection scoped for level reads sets its line to input and reads it; one scoped with a
// trigger also arms the line and gives the device ONE event buffer for it (see `gpio.rs`), so an event is
// delivered once and the line stays masked until the consumer acknowledges it. A line whose connection goes is
// disarmed and deactivated.
//
// THE EVENT QUEUE IS INTERRUPT-DRIVEN, on this device's own MSI-X vector: a line may not fire for the life of
// the machine, and a driver that polled for it would spin under a cooperative scheduler for nothing. The
// request queue is polled, one request at a time, as every virtio control queue here is.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use driver_protocol::GpioTrigger as ScopedTrigger;
use drivers::common;
use drivers::gpio::{self, Event, Lines, Request, Scope, Step, Trigger};
use drivers::virtio::{Queue, Virtio};
use proto::system::{Error, GpioEvent, GpioLine, GpioTrigger, gpio_device};
use rt::*;

const LINES_TOKEN: u16 = 0;

// ONE EVENT IS OUTSTANDING AT A TIME, so a stream never holds more than one unread frame.
const STREAM_DEPTH: u64 = 4;

// The request page: the request, and its response from here.
const RESPONSE: u64 = 64;

// The event page: one slot per line - the line number the buffer names, and the status the device writes at
// `EVENT_STATUS` into it. So a page serves this many lines' events.
const EVENT_SLOT: u64 = 16;
const EVENT_STATUS: u64 = 8;
const EVENT_LINES: u16 = (4096 / EVENT_SLOT) as u16;

// The names page: the status, then the names.
const NAMES_MAX: usize = 4096 - 1;

// A name as the line contract carries it.
const NAME_MAX: usize = 64;

// Where one connection's event is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Delivery {
	// Nothing to deliver: the line is armed, or the connection reads levels alone.
	Idle,
	// The line fired and the consumer has not been told: it has asked for no stream yet.
	Pending,
	// Delivered, and not yet acknowledged.
	Outstanding,
}

// ONE CONNECTION AND THE LINE IT HOLDS.
struct Held {
	channel: u64,
	line: u16,
	trigger: GpioTrigger,
	// The stream's producer end, once the consumer asked for events.
	events: u64,
	frames: u32,
	// Events delivered on this connection.
	sequence: u32,
	delivery: Delivery,
	// The level the pending or outstanding event was taken at.
	level: bool,
}

struct Controller {
	requests: Queue,
	// None on a device that offers no event queue: it serves levels alone.
	events: Option<Queue>,
	request_page: (u64, u64),
	event_page: (u64, u64),
	names: Vec<u8>,
	lines: Lines,
	// How many lines, from zero, can be armed: the event page's slots, and the event queue's room for one
	// two-descriptor buffer per line.
	event_lines: u16,
	held: Vec<Held>,
}

impl Controller {
	// ONE REQUEST, answered: the response's value byte, or `None` when the device refused it or did not answer.
	fn ask(&mut self, request: Request) -> Option<u8> {
		let (virt, phys) = self.request_page;
		// SAFETY: the request page is this driver's own DMA mapping, and both offsets are inside it.
		unsafe {
			core::ptr::copy_nonoverlapping(request.encode().as_ptr(), virt as *mut u8, 8);
			((virt + RESPONSE) as *mut u8).write_volatile(0xFF);
		}
		self.requests.submit_checked(&[(phys, 8, false), (phys + RESPONSE, 2, true)]).ok()?;
		// SAFETY: the device gave the chain back, so the response is written.
		let (status, value) = unsafe { (((virt + RESPONSE) as *const u8).read_volatile(), ((virt + RESPONSE + 1) as *const u8).read_volatile()) };
		(status == gpio::STATUS_OK).then_some(value)
	}

	// Queue `line`'s event buffer: the device completes it when the line fires, or at once when it is disarmed.
	fn post_event(&mut self, line: u16) {
		let (virt, phys) = self.event_page;
		let slot = EVENT_SLOT * line as u64;
		// SAFETY: `line` is below `event_lines`, so its slot is inside the event page.
		unsafe {
			((virt + slot) as *mut u16).write_volatile(line.to_le());
			((virt + slot + EVENT_STATUS) as *mut u8).write_volatile(0xFF);
		}
		let Some(events) = self.events.as_mut() else { return };
		if events.post_chain(2 * line, &[(phys + slot, 2, false), (phys + slot + EVENT_STATUS, 1, true)]) {
			events.notify();
		}
	}

	// Each step in order; false at the first that failed.
	fn run(&mut self, steps: &[Step]) -> bool {
		for &step in steps {
			match step {
				Step::Send(request) => {
					if self.ask(request).is_none() {
						return false;
					}
				}
				Step::QueueEvent(line) => self.post_event(line),
			}
		}
		true
	}

	// EVERY EVENT BUFFER THE DEVICE GAVE BACK: delivered to its line's connection, once, or dropped - the line
	// was disarmed, or nobody holds it.
	fn drain_events(&mut self) {
		while let Some((id, _)) = self.events.as_mut().and_then(Queue::take_used) {
			let line = id / 2;
			if line >= self.event_lines {
				continue;
			}
			// SAFETY: the slot is inside the event page, and the device gave its buffer back.
			let status = unsafe { ((self.event_page.0 + EVENT_SLOT * line as u64 + EVENT_STATUS) as *const u8).read_volatile() };
			if let Event::Deliver(line) = self.lines.event(line, status) {
				// THE LEVEL WHEN IT WAS TAKEN: read as the event comes off the queue.
				let level = self.ask(Request { kind: gpio::MSG_GET_VALUE, line, value: 0 }).is_some_and(|value| value != 0);
				if let Some(at) = self.held.iter().position(|held| held.line == line) {
					self.held[at].delivery = Delivery::Pending;
					self.held[at].level = level;
					self.flush(at);
				}
			}
		}
	}

	// Tell connection `at` about its pending event, if it has a stream to hear it on.
	fn flush(&mut self, at: usize) {
		let held = &mut self.held[at];
		if held.delivery != Delivery::Pending || held.events == 0 {
			return;
		}
		let event = GpioEvent { level: held.level, sequence: held.sequence + 1 };
		let mut frame = [0u8; 64];
		let mut handles = wire::Handles::new();
		let Some(len) = gpio_device::events_frame(held.frames, &event, &mut frame, &mut handles) else { return };
		if try_send(held.events, &frame[..len], 0) {
			held.frames += 1;
			held.sequence += 1;
			held.delivery = Delivery::Outstanding;
		}
	}

	// TAKE `line` FOR a new connection, scoped with `trigger`. False when it cannot be had.
	fn take(&mut self, channel: u64, line: u32, trigger: ScopedTrigger) -> bool {
		let Ok(line) = u16::try_from(line) else { return false };
		let (scope, wire) = match trigger {
			ScopedTrigger::Level => (Scope::Level, GpioTrigger::None),
			ScopedTrigger::Rising => (Scope::Interrupt(Trigger::Rising), GpioTrigger::Rising),
			ScopedTrigger::Falling => (Scope::Interrupt(Trigger::Falling), GpioTrigger::Falling),
			ScopedTrigger::Both => (Scope::Interrupt(Trigger::Both), GpioTrigger::Both),
			ScopedTrigger::High => (Scope::Interrupt(Trigger::High), GpioTrigger::High),
			ScopedTrigger::Low => (Scope::Interrupt(Trigger::Low), GpioTrigger::Low),
		};
		if matches!(scope, Scope::Interrupt(_)) && line >= self.event_lines {
			return false;
		}
		let mut steps = Vec::new();
		if self.lines.take(line, scope, &mut steps).is_err() {
			return false;
		}
		if !self.run(&steps) {
			self.release(line);
			return false;
		}
		self.held.push(Held { channel, line, trigger: wire, events: 0, frames: 0, sequence: 0, delivery: Delivery::Idle, level: false });
		true
	}

	// The connection on `channel` is over: its line disarmed and deactivated.
	fn give_back(&mut self, channel: u64) {
		let Some(at) = self.held.iter().position(|held| held.channel == channel) else { return };
		let held = self.held.swap_remove(at);
		if held.events != 0 {
			close(held.events);
		}
		self.release(held.line);
	}

	fn release(&mut self, line: u16) {
		let mut steps = Vec::new();
		self.lines.give_back(line, &mut steps);
		// A STEP THAT FAILS HERE LEAVES NOTHING TO UNDO: the line is no connection's any more either way.
		for step in steps {
			if let Step::Send(request) = step {
				let _ = self.ask(request);
			}
		}
		// Disarming completed the line's buffer if the device had it; taken off the queue now, so a new
		// connection for the line starts with the buffer here.
		self.drain_events();
	}

	fn name(&self, line: u16) -> String {
		let name = gpio::line_name(&self.names, line).unwrap_or(&[]);
		let mut text = String::from_utf8_lossy(name).into_owned();
		if text.len() > NAME_MAX {
			let mut end = NAME_MAX;
			while !text.is_char_boundary(end) {
				end -= 1;
			}
			text.truncate(end);
		}
		text
	}
}

// ONE CONNECTION'S VIEW: the controller, and the entry of the line it holds.
struct Connection<'a> {
	controller: &'a mut Controller,
	at: usize,
}

impl gpio_device::Service for Connection<'_> {
	fn line(&mut self) -> Result<GpioLine, Error> {
		let held = &self.controller.held[self.at];
		let (line, trigger) = (held.line, held.trigger);
		Ok(GpioLine { line: line as u32, name: self.controller.name(line), trigger })
	}

	fn level(&mut self) -> Result<bool, Error> {
		let line = self.controller.held[self.at].line;
		self.controller.ask(Request { kind: gpio::MSG_GET_VALUE, line, value: 0 }).map(|value| value != 0).ok_or(Error::Io)
	}

	fn events(&mut self) -> Vec<GpioEvent> {
		Vec::new()
	}

	// THE LINE MAY FIRE AGAIN: its buffer goes back to the device, which reports a level line still asserted
	// at once. Refused when no event is outstanding - one not yet delivered included.
	fn acknowledge(&mut self) -> Result<(), Error> {
		let held = &self.controller.held[self.at];
		if held.delivery != Delivery::Outstanding {
			return Err(Error::Invalid);
		}
		let line = held.line;
		let Some(Step::QueueEvent(line)) = self.controller.lines.acknowledge(line) else { return Err(Error::Invalid) };
		self.controller.held[self.at].delivery = Delivery::Idle;
		self.controller.post_event(line);
		Ok(())
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
	let Some(at) = controller.held.iter().position(|held| held.channel == channel) else { return false };
	if len >= 2 && u16::from_le_bytes([buf[0], buf[1]]) == gpio_device::OP_EVENTS {
		let mut view = Connection { controller: &mut *controller, at };
		let Some((corr, _)) = gpio_device::events_open(&mut view, &buf[..len], &mut handles) else { return true };
		// NO STREAM for a connection that reads levels alone: the answer carries none, which is the refusal.
		if controller.held[at].trigger == GpioTrigger::None {
			send_blocking(channel, &corr.to_le_bytes(), 0);
			return true;
		}
		let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else {
			send_blocking(channel, &corr.to_le_bytes(), 0);
			return true;
		};
		// A SECOND STREAM REPLACES THE FIRST. An event already delivered on the first is not delivered
		// again - it was delivered once, and is still the consumer's to acknowledge.
		let held = &mut controller.held[at];
		if held.events != 0 {
			close(held.events);
		}
		held.events = producer;
		held.frames = 0;
		send_caps_blocking(channel, &corr.to_le_bytes(), &[consumer]);
		controller.flush(at);
		return true;
	}
	let mut reply_buf = [0u8; 256];
	let mut reply_handles = wire::Handles::new();
	let mut view = Connection { controller, at };
	if let Some(written) = gpio_device::dispatch(&mut view, &buf[..len], &mut handles, &mut reply_buf, &mut reply_handles) {
		send_caps_blocking(channel, &reply_buf[..written], reply_handles.as_slice());
	}
	true
}

// A consumer has gone, or was refused: its line is given back, and the manager is told.
fn part(controller: &mut Controller, serving: &mut common::Serving, bootstrap: u64, bind: &common::Bind, index: usize) -> bool {
	controller.give_back(serving.at(index));
	let token = serving.close_at(index);
	common::disconnected(bootstrap, bind, token)
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	unsafe {
		let (bind, resources) = common::handshake(bootstrap);
		let mut device: Virtio = common::bringup_bound(bootstrap, &bind, &resources, gpio::FEATURE_IRQ as u32);
		let has_events = device.features_word0() & gpio::FEATURE_IRQ as u32 != 0;
		let irq: u64 = resources.irq;
		if irq != 0 {
			device.set_msix_vector(0);
		}
		let count = u16::from_le_bytes([device.config_read(0), device.config_read(1)]);
		let names_size = u32::from_le_bytes([device.config_read(4), device.config_read(5), device.config_read(6), device.config_read(7)]) as usize;
		let Some(requests) = device.setup_queue(0) else { exit() };
		// NO EVENT QUEUE, NO INTERRUPT LINES: a device without the feature serves levels alone.
		let events = if has_events && irq != 0 { device.setup_queue(1) } else { None };
		let page = || -> (u64, u64) {
			match dma_buffer_for(device.capability, 4096) {
				Some((_, virt, phys)) => (virt, phys),
				None => exit(),
			}
		};
		let (request_page, event_page, names_page) = (page(), page(), page());
		let event_lines = match &events {
			Some(queue) => {
				queue.enable_interrupts();
				count.min(EVENT_LINES).min(queue.size() / 2)
			}
			None => 0,
		};
		device.driver_ok();
		let mut controller = Controller { requests, events, request_page, event_page, names: Vec::new(), lines: Lines::new(count, event_lines != 0), event_lines, held: Vec::new() };
		// THE LINE NAMES, once: a device whose list does not fit the page has lines without names.
		if names_size != 0 && names_size <= NAMES_MAX {
			let request = Request { kind: gpio::MSG_GET_NAMES, line: 0, value: 0 };
			core::ptr::copy_nonoverlapping(request.encode().as_ptr(), request_page.0 as *mut u8, 8);
			if controller.requests.submit_checked(&[(request_page.1, 8, false), (names_page.1, 1 + names_size as u32, true)]).is_ok() && (names_page.0 as *const u8).read_volatile() == gpio::STATUS_OK {
				controller.names.extend_from_slice(core::slice::from_raw_parts((names_page.0 + 1) as *const u8, names_size));
			}
		}
		// THE OFFERED ENDPOINT SERVES NOTHING: its far end is the offer, and this end is closed at once.
		let Some((near, far)) = channel() else { exit() };
		let mut line = [0u8; 64];
		let n = common::describe(&mut line, b"virtio-gpio", &device, if event_lines != 0 { b"levels, events" } else { b"levels" });
		if !common::online(bootstrap, &bind, &line[..n], &[(driver_protocol::provider::GPIO_LINES, far)]) {
			exit();
		}
		close(near);
		let mut serving = common::Serving::from_offers(&[(LINES_TOKEN, 0)]);
		let mut buf = alloc::vec![0u8; 512];
		let irq_set = [irq];
		let devices: &[u64] = if irq != 0 { &irq_set } else { &[] };
		loop {
			match common::wait_providers_or_answer(bootstrap, &bind, &mut serving, devices) {
				None => {
					if common::stop_requested() {
						common::finish_stop(bootstrap, &bind, device.capability, common::quiesce_virtio());
					}
					exit();
				}
				Some(common::ProviderReady::Connected(index)) => {
					// ONE LINE, HELD BY ONE CONNECTION. Anything else is refused on the connection itself.
					let admitted = match serving.scope_at(index) {
						driver_protocol::Scope::GpioLine { line, trigger } => controller.take(serving.at(index), line, trigger),
						_ => false,
					};
					if !admitted && !part(&mut controller, &mut serving, bootstrap, &bind, index) {
						exit();
					}
				}
				Some(common::ProviderReady::Consumer(index)) => {
					if !serve(&mut controller, &serving, index, &mut buf) && !part(&mut controller, &mut serving, bootstrap, &bind, index) {
						exit();
					}
				}
				Some(common::ProviderReady::Device(_)) => {
					// Read the ISR before acknowledging, which deasserts a level-triggered line and reads zero on
					// MSI-X.
					let _ = device.read_isr();
					interrupt_ack(irq);
					controller.drain_events();
				}
			}
		}
	}
}
