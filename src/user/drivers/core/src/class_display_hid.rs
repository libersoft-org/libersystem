// The USB monitor side of driver.xhci: a monitor's HID interface - the Monitor Control class's brightness, published as
// a `backlight` provider DisplayService drives, and the ambient-light sensor a monitor may carry on the same interface,
// published as an `ambient-light` provider the brightness policy reads.
//
// GENERIC HID TRANSPORT, AND THE CLASS'S USAGES. The report descriptor is read with the common HID field table and
// mapped by `drivers::hid_display`: the brightness is a feature report, read by GET_REPORT and written by SET_REPORT in
// its own logical range; the EDID, where the monitor gives one, is a feature report read once, which names the monitor
// the backlight belongs to; the illuminance is an input report on the interrupt pipe, sent as the light changes.
//
// TWO PUBLICATIONS, ONE DEVICE. A monitor with a sensor is two providers with two consumers, so the module carries the
// second publication the class machinery offers; a monitor without one is a backlight alone, and a sensor that is not
// on a monitor an ambient-light provider alone.
//
// THE STABLE KEY a stored level is filed under is `usb:` and the vendor, product and serial number, or the port path
// where the device has no serial - which is stable for as long as it stays in the same socket.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use proto::system::{AmbientLightDescription, BacklightDescription, BacklightEvent, BacklightRange, BacklightScale, BacklightSource, BacklightTarget, Error, Illuminance, MonitorIdentity, ambient_light, backlight};
use rt::*;

use crate::classes::{self, Module, Pipe};
use crate::usb_hid::Hids;
use crate::{KIND_MONITOR, UsbDevice, Xhci, control_in_req, control_out_req, r8};
use drivers::common;
use drivers::hid::{self, FieldTable, ReportKind};
use drivers::hid_display::{self, Map};
use drivers::hid_power::{self, Interface};
use drivers::usb_class::ClassKind;

// Deep enough for a reading and the changes the policy has not read yet.
const STREAM_DEPTH: u64 = 16;
const REQUEST_BYTES: usize = 256;

pub struct Display {
	dev: UsbDevice,
	interface: u8,
	input: Option<Pipe>,
	table: FieldTable,
	map: Map,
	key: String,
	monitor: Option<hid_display::Identity>,
	// The last illuminance the sensor sent, so a stream opened between reports starts with it.
	last: Option<u64>,
	levels: u64,
	level_seq: u32,
	light: u64,
	light_seq: u32,
	buf: Vec<u8>,
}

/// Read a HID interface's report descriptor and decide whether it is a monitor or a light sensor. The device comes back
/// when it is neither, or when it could not be brought up.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
pub unsafe fn probe(hc: &mut Xhci, mut dev: UsbDevice, interface: Interface) -> Result<Display, UsbDevice> {
	unsafe {
		let mut none = Hids::new();
		let length = interface.report_descriptor_length.min(4096);
		// THE LAST HID CLASS ASKED, so what it cannot read it says: a HID interface no input, power or monitor class
		// took is otherwise a device that left no trace.
		let Some(received) = control_in_req(hc, &mut none, &mut dev, 0x81, crate::REQ_GET_DESCRIPTOR, (hid_power::DT_REPORT as u16) << 8, interface.interface as u16, length) else {
			print(b"driver.xhci: a HID interface's report descriptor could not be read - no HID class binds it\n");
			return Err(dev);
		};
		let descriptor: Vec<u8> = (0..received.min(length as u32) as u64).map(|at| r8(dev.data_virt + at)).collect();
		let table = match hid::fields(&descriptor) {
			Ok(table) => table,
			Err(refused) => {
				print(match refused {
					hid::FieldsRefused::Truncated => b"driver.xhci: a HID interface's report descriptor runs past its end - no HID class binds it\n".as_slice(),
					hid::FieldsRefused::TooDeep => b"driver.xhci: a HID interface's report descriptor nests too deep - no HID class binds it\n".as_slice(),
					hid::FieldsRefused::TooLarge => b"driver.xhci: a HID interface's report descriptor declares a report or a field count past what is read - no HID class binds it\n".as_slice(),
				});
				return Err(dev);
			}
		};
		let Some(map) = hid_display::map(&table) else {
			let mut line: common::Bounded<160> = common::Bounded::new();
			line.push(b"driver.xhci: a HID interface of ");
			line.decimal(table.fields.len() as u64);
			line.push(b" field(s) in a ");
			line.decimal(u64::from(received));
			line.push(b"-byte descriptor carries nothing an input, power or monitor class reads\n");
			print(line.as_bytes());
			return Err(dev);
		};
		if !crate::admits(hc, ClassKind::Display, dev.class) {
			return Err(dev);
		}
		// The interrupt pipe carries the sensor and changes made with the monitor's own buttons.
		let mut input = None;
		if map.brightness_input.is_some() || map.illuminance.is_some_and(|field| field.kind == ReportKind::Input) {
			let Some(pipe) = Pipe::new(dev.slot, dev.speed, &interface.interrupt_in) else {
				print(b"driver.xhci: a monitor is on the bus and no pages were left for its sensor's pipe\n");
				return Err(dev);
			};
			input = Some(pipe);
		}
		let pipes: Vec<&Pipe> = input.iter().collect();
		if !(pipes.is_empty() || classes::configure_pipes(hc, &mut dev, &pipes, 0)) || !classes::select(hc, &mut dev, interface.config_value, interface.interface, interface.alternate) {
			if let Some(mut pipe) = input {
				pipe.release(hc);
			}
			print(b"driver.xhci: a monitor's interrupt endpoint or its setting was refused\n");
			return Err(dev);
		}
		let key = match crate::class_dfu::serial_of(hc, &mut dev) {
			Some(serial) if !serial.is_empty() => format!("usb:{:04x}:{:04x}:{serial}", dev.vendor, dev.product),
			_ => format!("usb:{:04x}:{:04x}@port{}.{:x}", dev.vendor, dev.product, dev.port, dev.route),
		};
		let mut display = Display { dev, interface: interface.interface, input, table, map, key, monitor: None, last: None, levels: 0, level_seq: 0, light: 0, light_seq: 0, buf: alloc::vec![0u8; REQUEST_BYTES] };
		// A SENSOR IS TOLD TO REPORT: all events, at full power - where it declares the two properties. A device reset
		// clears them, and the enumeration that follows one comes back through this probe.
		display.wake_sensor(hc, &mut none);
		if let Some(first) = display.map.edid.first().copied() {
			let edid = display.get_report(hc, &mut none, ReportKind::Feature, first.report_id).map(|body| hid_display::edid_bytes(&display.map, &body)).unwrap_or_default();
			display.monitor = hid_display::edid_identity(&edid);
		}
		let mut line: common::Bounded<192> = common::Bounded::new();
		line.push(b"driver.xhci: monitor bound - ");
		line.push(display.key.as_bytes());
		if let Some(brightness) = display.map.brightness {
			let (minimum, maximum) = hid_display::range(&brightness);
			line.push(b", brightness ");
			line.decimal(u64::from(minimum));
			line.push(b"..");
			line.decimal(u64::from(maximum));
		}
		line.push(if display.monitor.is_some() { b", its EDID read".as_slice() } else { b", no EDID".as_slice() });
		if display.map.is_light_sensor() {
			line.push(b", an ambient-light sensor");
		}
		line.push(b"\n");
		print(line.as_bytes());
		Ok(display)
	}
}

impl Display {
	fn report_bytes(&self, kind: ReportKind, id: u8) -> u16 {
		(self.table.body_bytes(kind, id) + u32::from(self.table.uses_ids)) as u16
	}

	// One report's body by GET_REPORT, the ID byte checked and taken off when the device numbers its reports.
	fn get_report(&mut self, hc: &mut Xhci, hids: &mut Hids, kind: ReportKind, id: u8) -> Option<Vec<u8>> {
		let length = self.report_bytes(kind, id);
		if length == 0 {
			return None;
		}
		let received = control_in_req(hc, hids, &mut self.dev, 0xa1, hid_power::REQ_GET_REPORT, kind.request_type() << 8 | id as u16, self.interface as u16, length)?;
		let bytes: Vec<u8> = (0..received.min(length as u32) as u64).map(|at| unsafe { r8(self.dev.data_virt + at) }).collect();
		self.body_of(id, &bytes)
	}

	fn body_of(&self, id: u8, bytes: &[u8]) -> Option<Vec<u8>> {
		if self.table.uses_ids {
			if bytes.first() != Some(&id) {
				return None;
			}
			return Some(bytes[1..].to_vec());
		}
		Some(bytes.to_vec())
	}

	fn level(&mut self, hc: &mut Xhci, hids: &mut Hids) -> Result<u32, Error> {
		let field = self.map.brightness.ok_or(Error::Unsupported)?;
		if field.kind != ReportKind::Feature {
			return Err(Error::Unsupported);
		}
		let body = self.get_report(hc, hids, field.kind, field.report_id).ok_or(Error::Io)?;
		hid_display::level(&field, &body).ok_or(Error::Unsupported)
	}

	// Write a level into the brightness report and send it. The report is read first where it can be, so every other
	// field in it goes back as the monitor last said it was.
	fn set_level(&mut self, hc: &mut Xhci, hids: &mut Hids, level: u32) -> Result<u32, Error> {
		let field = self.map.brightness.ok_or(Error::Unsupported)?;
		let mut body = match self.get_report(hc, hids, field.kind, field.report_id) {
			Some(body) if field.kind == ReportKind::Feature => body,
			_ => alloc::vec![0u8; self.table.body_bytes(field.kind, field.report_id) as usize],
		};
		if !hid_display::write_level(&mut body, &field, level) {
			return Err(Error::Invalid);
		}
		let mut frame: Vec<u8> = Vec::new();
		if self.table.uses_ids {
			frame.push(field.report_id);
		}
		frame.extend_from_slice(&body);
		unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), self.dev.data_virt as *mut u8, frame.len()) };
		control_out_req(hc, hids, &mut self.dev, 0x21, hid_power::REQ_SET_REPORT, field.kind.request_type() << 8 | field.report_id as u16, self.interface as u16, frame.len() as u16).ok_or(Error::Io)?;
		Ok(level)
	}

	// The sensor's reporting and power states set, in the report or reports that carry them.
	fn wake_sensor(&mut self, hc: &mut Xhci, hids: &mut Hids) {
		let wanted = [(self.map.reporting_state, hid_display::ALL_EVENTS), (self.map.power_state, hid_display::FULL_POWER)];
		for (field, selector) in wanted {
			let Some(field) = field else { continue };
			let Some(value) = hid_display::selector_value(&field, selector) else { continue };
			let mut body = match self.get_report(hc, hids, field.kind, field.report_id) {
				Some(body) => body,
				None => alloc::vec![0u8; self.table.body_bytes(field.kind, field.report_id) as usize],
			};
			if !hid::write_field(&mut body, &field, value) {
				continue;
			}
			let mut frame: Vec<u8> = Vec::new();
			if self.table.uses_ids {
				frame.push(field.report_id);
			}
			frame.extend_from_slice(&body);
			unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), self.dev.data_virt as *mut u8, frame.len()) };
			if control_out_req(hc, hids, &mut self.dev, 0x21, hid_power::REQ_SET_REPORT, field.kind.request_type() << 8 | field.report_id as u16, self.interface as u16, frame.len() as u16).is_none() {
				print(b"driver.xhci: a light sensor refused its reporting or power state - it may report nothing\n");
			}
		}
	}

	fn describe(&self) -> Result<BacklightDescription, Error> {
		let field = self.map.brightness.ok_or(Error::Unsupported)?;
		let (minimum, maximum) = hid_display::range(&field);
		Ok(BacklightDescription {
			source: BacklightSource::UsbMonitor,
			key: self.key.clone(),
			scale: BacklightScale::Range(BacklightRange { minimum, maximum }),
			ac_default: None,
			battery_default: None,
			target: match self.monitor {
				Some(monitor) => BacklightTarget::Monitor(MonitorIdentity { manufacturer: monitor.manufacturer, product: monitor.product, serial: monitor.serial }),
				None => BacklightTarget::None,
			},
		})
	}

	// THE READING NOW, for a stream that opens: asked of the device, or the last it sent.
	fn reading(&mut self, hc: &mut Xhci, hids: &mut Hids) -> Option<u64> {
		let field = self.map.illuminance?;
		if let Some(body) = self.get_report(hc, hids, field.kind, field.report_id)
			&& let Some(milli_lux) = hid_display::milli_lux(&field, &body)
		{
			self.last = Some(milli_lux);
		}
		self.last
	}

	fn send_level(&mut self, level: u32) {
		if self.levels == 0 {
			return;
		}
		let mut frame = [0u8; 64];
		let mut handles = wire::Handles::new();
		let Some(len) = backlight::events_frame(self.level_seq, &BacklightEvent::Level(level), &mut frame, &mut handles) else { return };
		if try_send(self.levels, &frame[..len], 0) {
			self.level_seq = self.level_seq.wrapping_add(1);
		} else {
			close(self.levels);
			self.levels = 0;
		}
	}

	fn send_reading(&mut self, milli_lux: u64) {
		if self.light == 0 {
			return;
		}
		let mut frame = [0u8; 64];
		let mut handles = wire::Handles::new();
		let Some(len) = ambient_light::events_frame(self.light_seq, &Illuminance { milli_lux }, &mut frame, &mut handles) else { return };
		if try_send(self.light, &frame[..len], 0) {
			self.light_seq = self.light_seq.wrapping_add(1);
			return;
		}
		// A STREAM THAT CANNOT TAKE A READING IS CLOSED: the policy opens it again and starts from the reading then.
		close(self.light);
		self.light = 0;
	}

	fn request(&mut self, chan: u64) -> Option<(Vec<u8>, wire::Handles)> {
		let mut buf = core::mem::take(&mut self.buf);
		let polled = try_recv_caps(chan, &mut buf);
		self.buf = buf;
		match polled {
			PolledCaps::Message { len, handles } => Some((self.buf[..len].to_vec(), handles)),
			PolledCaps::Empty => Some((Vec::new(), wire::Handles::new())),
			PolledCaps::Closed => None,
		}
	}
}

struct BacklightView<'a> {
	display: &'a mut Display,
	hc: &'a mut Xhci,
	hids: &'a mut Hids,
}

impl backlight::Service for BacklightView<'_> {
	fn firmware_display_id(&mut self) -> Result<Option<u32>, Error> {
		Ok(None)
	}

	fn describe(&mut self) -> Result<BacklightDescription, Error> {
		self.display.describe()
	}

	fn get(&mut self) -> Result<u32, Error> {
		self.display.level(self.hc, self.hids)
	}

	fn set(&mut self, level: u32) -> Result<u32, Error> {
		self.display.set_level(self.hc, self.hids, level)
	}

	fn events(&mut self) -> Vec<BacklightEvent> {
		Vec::new()
	}
}

struct LightView;

impl ambient_light::Service for LightView {
	// A HID SENSOR CARRIES NO RESPONSE CURVE: the policy follows its own.
	fn describe(&mut self) -> Result<AmbientLightDescription, Error> {
		Ok(AmbientLightDescription { curve: Vec::new() })
	}

	fn events(&mut self) -> Vec<Illuminance> {
		Vec::new()
	}
}

impl Module for Display {
	fn kind(&self) -> ClassKind {
		ClassKind::Display
	}

	fn inventory(&self) -> u8 {
		KIND_MONITOR
	}

	// THE FIRST PUBLICATION IS THE BACKLIGHT where the monitor has one, the sensor's otherwise.
	fn provider(&self) -> u16 {
		if self.map.is_monitor() { driver_protocol::provider::BACKLIGHT } else { driver_protocol::provider::AMBIENT_LIGHT }
	}

	fn name(&self) -> &'static [u8] {
		if self.map.is_monitor() { driver_protocol::provider::USB_BACKLIGHT_NAME } else { driver_protocol::provider::USB_AMBIENT_LIGHT_NAME }
	}

	fn second(&self) -> Option<(u16, &'static [u8])> {
		(self.map.is_monitor() && self.map.is_light_sensor()).then_some((driver_protocol::provider::AMBIENT_LIGHT, driver_protocol::provider::USB_AMBIENT_LIGHT_NAME))
	}

	fn device(&self) -> &UsbDevice {
		&self.dev
	}

	fn device_mut(&mut self) -> &mut UsbDevice {
		&mut self.dev
	}

	// THE SENSOR'S PIPE STANDS FROM THE PUBLICATION ON: a reading the policy was not there for is still the last one.
	fn start(&mut self, hc: &mut Xhci) {
		let length = [self.map.illuminance, self.map.brightness_input].into_iter().flatten().filter(|field| field.kind == ReportKind::Input).map(|field| u32::from(self.report_bytes(field.kind, field.report_id))).max().unwrap_or(0);
		if let Some(input) = self.input.as_mut() {
			let length = length.max(input.mps.min(64));
			input.post(hc, length);
		}
	}

	fn serve(&mut self, hc: &mut Xhci, hids: &mut Hids, chan: u64, _bootstrap: u64, _bind: &common::Bind) -> bool {
		if !self.map.is_monitor() {
			return self.serve_second(hc, hids, chan);
		}
		let Some((request, mut handles)) = self.request(chan) else { return false };
		if request.is_empty() {
			return true;
		}
		if classes::correlation(&request).map(|(op, _)| op) == Some(backlight::OP_EVENTS) {
			let mut view = BacklightView { display: self, hc, hids };
			let Some((corr, _)) = backlight::events_open(&mut view, &request, &mut handles) else { return true };
			let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
			if self.levels != 0 {
				close(self.levels);
			}
			self.levels = producer;
			self.level_seq = 0;
			send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
			return true;
		}
		let mut reply = [0u8; 512];
		let mut reply_handles = wire::Handles::new();
		let mut view = BacklightView { display: self, hc, hids };
		if let Some(written) = backlight::dispatch(&mut view, &request, &mut handles, &mut reply, &mut reply_handles) {
			send_caps_blocking(chan, &reply[..written], reply_handles.as_slice());
		}
		for &handle in handles.as_slice() {
			close(handle);
		}
		true
	}

	fn serve_second(&mut self, hc: &mut Xhci, hids: &mut Hids, chan: u64) -> bool {
		let Some((request, mut handles)) = self.request(chan) else { return false };
		if request.is_empty() {
			return true;
		}
		if classes::correlation(&request).map(|(op, _)| op) == Some(ambient_light::OP_EVENTS) {
			let Some((corr, _)) = ambient_light::events_open(&mut LightView, &request, &mut handles) else { return true };
			let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
			if self.light != 0 {
				close(self.light);
			}
			self.light = producer;
			self.light_seq = 0;
			send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
			// THE STREAM'S FIRST ITEM IS THE READING NOW, so the policy never waits for a change to learn the light.
			if let Some(milli_lux) = self.reading(hc, hids) {
				self.send_reading(milli_lux);
			}
			return true;
		}
		let mut reply = [0u8; 512];
		let mut reply_handles = wire::Handles::new();
		if let Some(written) = ambient_light::dispatch(&mut LightView, &request, &mut handles, &mut reply, &mut reply_handles) {
			send_caps_blocking(chan, &reply[..written], reply_handles.as_slice());
		}
		for &handle in handles.as_slice() {
			close(handle);
		}
		true
	}

	fn departed(&mut self, _hc: &mut Xhci, _hids: &mut Hids, _chan: u64) {}

	fn absorb(&mut self, hc: &mut Xhci, _hids: &mut Hids, _pointer: u64, status: u32, control: u32) -> bool {
		let Some(input) = self.input.as_mut() else { return false };
		if !input.owns(control) {
			return false;
		}
		let (code, moved) = input.complete(status);
		if classes::succeeded(code) && moved > 0 {
			let bytes = input.read(moved as usize);
			let id = if self.table.uses_ids { bytes[0] } else { 0 };
			if let (Some(field), Some(body)) = (self.map.illuminance, self.body_of(id, &bytes))
				&& field.kind == ReportKind::Input
				&& field.report_id == id
				&& let Some(milli_lux) = hid_display::milli_lux(&field, &body)
			{
				self.last = Some(milli_lux);
				self.send_reading(milli_lux);
			}
			if let (Some(field), Some(body)) = (self.map.brightness_input, self.body_of(id, &bytes))
				&& field.report_id == id
				&& let Some(level) = hid_display::level(&field, &body)
			{
				self.send_level(level);
			}
		} else if classes::stalled(code) {
			let mut none = Hids::new();
			if let Some(input) = self.input.as_mut() {
				let _ = classes::clear_halt(hc, &mut none, &mut self.dev, input);
			}
		}
		self.start(hc);
		true
	}

	fn release(&mut self, hc: &mut Xhci) {
		for stream in [&mut self.levels, &mut self.light] {
			if *stream != 0 {
				close(*stream);
				*stream = 0;
			}
		}
		if let Some(input) = self.input.as_mut() {
			input.release(hc);
		}
	}
}
