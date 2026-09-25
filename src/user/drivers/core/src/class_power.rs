// The USB HID Power Device side of driver.xhci: a UPS on the bus, published as the `power-source` provider
// PowerService drives over `liber:power`'s provider contract.
//
// GENERIC HID TRANSPORT, AND THE MODEL'S NORMALISATION. The report descriptor is read with the common HID field
// table and mapped by `drivers::hid_power`; the reports arrive the way every HID device's do - input reports on
// the interrupt pipe as the device changes, feature reports by GET_REPORT when this side asks - and what their
// bits mean is `power_model`'s, the same adapters the in-guest fixture proved the service against. Nothing here
// is a UPS-specific parser.
//
// ONE SOURCE. A UPS is one power source to this provider, local identity zero; the stream opens with a snapshot
// of it and carries an update each time an input report changes what the model makes of it.
//
// CONTROLS ARE ADVERTISED AND ONLY THOSE ARE SENT. A command the descriptor gives no writable field for is refused
// before anything leaves; one that is sent and not acknowledged is `indeterminate`, never retried here - the
// contract's answer to a command whose effect is uncertain is a fresh `query`.

use alloc::vec::Vec;
use proto::system::{ControlOutcome, Error, ProviderCommand, ProviderCommandKind, ProviderSource, ProviderUpdate, ProviderUpdateKind, SourceState, power_provider};
use rt::*;

use crate::classes::{self, Module, Pipe};
use crate::usb_hid::Hids;
use crate::{KIND_POWER, UsbDevice, Xhci, control_in_req, control_out_req, r8};
use drivers::common;
use drivers::hid::{self, FieldTable, ReportKind};
use drivers::hid_power::{self, Interface, Map, Report};
use drivers::usb_class::ClassKind;

// Deep enough for a snapshot and a burst of changes PowerService has not read yet.
const STREAM_DEPTH: u64 = 64;
const REQUEST_BYTES: usize = 64;
// The one source this provider publishes.
const LOCAL: u32 = 0;

pub struct Power {
	dev: UsbDevice,
	interface: u8,
	input: Pipe,
	table: FieldTable,
	map: Map,
	reports: Vec<Report>,
	consumer: u64,
	stream: u64,
	stream_seq: u32,
	revision: u64,
	// What the stream last carried, so an input report that changes nothing the model says is not an update.
	published: Option<SourceState>,
	buf: Vec<u8>,
}

/// Read a HID interface's report descriptor and decide whether it is a power device. The device comes back when
/// it is not, or when it could not be brought up.
///
/// # Safety
/// `dev` is an addressed device this controller owns.
pub unsafe fn probe(hc: &mut Xhci, mut dev: UsbDevice, interface: Interface) -> Result<Power, UsbDevice> {
	unsafe {
		let mut none = Hids::new();
		let length = interface.report_descriptor_length.min(4096);
		let Some(received) = control_in_req(hc, &mut none, &mut dev, 0x81, crate::REQ_GET_DESCRIPTOR, (hid_power::DT_REPORT as u16) << 8, interface.interface as u16, length) else { return Err(dev) };
		let descriptor: Vec<u8> = (0..received.min(length as u32) as u64).map(|at| r8(dev.data_virt + at)).collect();
		let Ok(table) = hid::fields(&descriptor) else { return Err(dev) };
		let Some(map) = hid_power::map(&table) else { return Err(dev) };
		if !crate::admits(hc, ClassKind::PowerDevice, dev.class) {
			return Err(dev);
		}
		let Some(mut input) = Pipe::new(dev.slot, dev.speed, &interface.interrupt_in) else {
			print(b"driver.xhci: a HID power device is on the bus and no pages were left for its pipe\n");
			return Err(dev);
		};
		if !classes::configure_pipes(hc, &mut dev, &[&input], 0) || !classes::select(hc, &mut dev, interface.config_value, interface.interface, interface.alternate) {
			input.release(hc);
			print(b"driver.xhci: a HID power device's interrupt endpoint or its setting was refused\n");
			return Err(dev);
		}
		let mut power = Power { dev, interface: interface.interface, input, table, map, reports: Vec::new(), consumer: 0, stream: 0, stream_seq: 0, revision: 0, published: None, buf: alloc::vec![0u8; REQUEST_BYTES] };
		power.read_all(hc, &mut none);
		let mut line: common::Bounded<128> = common::Bounded::new();
		line.push(b"driver.xhci: HID power device bound - ");
		line.decimal(power.map.reports().len() as u64);
		line.push(b" report(s) carry its values");
		if power.map.delay_before_shutdown.is_some() {
			line.push(b", a turn-off can be scheduled");
		}
		if !power.map.outlets.is_empty() {
			line.push(b", ");
			line.decimal(power.map.outlets.len() as u64);
			line.push(b" switchable outlet(s)");
		}
		line.push(b"\n");
		print(line.as_bytes());
		Ok(power)
	}
}

impl Power {
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

	// A report's body out of what arrived: past the ID byte when there is one, and only when it names this report.
	fn body_of(&self, id: u8, bytes: &[u8]) -> Option<Vec<u8>> {
		if self.table.uses_ids {
			if bytes.first() != Some(&id) {
				return None;
			}
			return Some(bytes[1..].to_vec());
		}
		Some(bytes.to_vec())
	}

	fn remember(&mut self, kind: ReportKind, id: u8, body: Vec<u8>) {
		match self.reports.iter_mut().find(|report| report.kind == kind && report.report_id == id) {
			Some(report) => report.body = body,
			None => self.reports.push(Report { kind, report_id: id, body }),
		}
	}

	// Every report the map reads, fresh from the device. A report the device will not give keeps what it last
	// gave, and one it never gave stays absent - which the model reads as "not known".
	fn read_all(&mut self, hc: &mut Xhci, hids: &mut Hids) {
		for (kind, id) in self.map.reports() {
			if let Some(body) = self.get_report(hc, hids, kind, id) {
				self.remember(kind, id, body);
			}
		}
	}

	fn state(&self) -> SourceState {
		power_model::hid::ups(&hid_power::decode(&self.map, &self.reports))
	}

	fn frame(&mut self, kind: ProviderUpdateKind, state: Option<SourceState>) -> bool {
		if self.stream == 0 {
			return false;
		}
		self.revision += 1;
		let update = ProviderUpdate { revision: self.revision, kind, source: state.map(|state| ProviderSource { local: LOCAL, state }), gone: None };
		let mut frame = [0u8; 2048];
		let mut handles = wire::Handles::new();
		let Some(len) = power_provider::updates_frame(self.stream_seq, &update, &mut frame, &mut handles) else { return false };
		if try_send(self.stream, &frame[..len], 0) {
			self.stream_seq += 1;
			return true;
		}
		// A STREAM THAT CANNOT TAKE A FRAME IS CLOSED: the contract's word for a loss is a closed subscription,
		// which PowerService answers by subscribing again for a fresh snapshot.
		close(self.stream);
		self.stream = 0;
		false
	}

	// Publish what changed, when anything did.
	fn update(&mut self) {
		let state = self.state();
		if self.published.as_ref() == Some(&state) {
			return;
		}
		if self.frame(ProviderUpdateKind::Updated, Some(state.clone())) {
			self.published = Some(state);
		}
	}

	// Write one control's value into its report and send it. The report is read first, so every other field in it
	// goes back as the device last said it was.
	fn write(&mut self, hc: &mut Xhci, hids: &mut Hids, field: hid::FieldInfo, logical: i64) -> Result<ControlOutcome, Error> {
		let mut body = match self.get_report(hc, hids, field.kind, field.report_id) {
			Some(body) => body,
			None => alloc::vec![0u8; self.table.body_bytes(field.kind, field.report_id) as usize],
		};
		if !hid_power::write_control(&mut body, &field, logical) {
			return Err(Error::Invalid);
		}
		let mut frame: Vec<u8> = Vec::new();
		if self.table.uses_ids {
			frame.push(field.report_id);
		}
		frame.extend_from_slice(&body);
		if frame.len() > 4096 {
			return Err(Error::Invalid);
		}
		unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), self.dev.data_virt as *mut u8, frame.len()) };
		match control_out_req(hc, hids, &mut self.dev, 0x21, hid_power::REQ_SET_REPORT, field.kind.request_type() << 8 | field.report_id as u16, self.interface as u16, frame.len() as u16) {
			Some(()) => {
				self.remember(field.kind, field.report_id, body);
				Ok(ControlOutcome::Done)
			}
			// SENT AND NOT ACKNOWLEDGED: whether the device acted is unknown, and it is said so.
			None => Ok(ControlOutcome::Indeterminate),
		}
	}
}

struct View<'a> {
	power: &'a mut Power,
	hc: &'a mut Xhci,
	hids: &'a mut Hids,
}

impl power_provider::Service for View<'_> {
	fn updates(&mut self) -> Vec<ProviderUpdate> {
		Vec::new()
	}

	fn command(&mut self, command: ProviderCommand) -> Result<ControlOutcome, Error> {
		if command.local != LOCAL {
			return Err(Error::NotFound);
		}
		let map = &self.power.map;
		let (field, logical) = match command.kind {
			ProviderCommandKind::ScheduleOutputOff => (map.delay_before_shutdown.ok_or(Error::Unsupported)?, i64::from(command.delay_seconds)),
			// MINUS ONE CANCELS, and a field whose range cannot hold it cannot cancel.
			ProviderCommandKind::CancelOutputOff => match map.delay_before_shutdown {
				Some(field) if field.logical_min <= -1 => (field, -1),
				_ => return Err(Error::Unsupported),
			},
			ProviderCommandKind::SetOutput => (*map.outlets.get(command.outlet as usize).ok_or(Error::Unsupported)?, i64::from(command.on)),
		};
		self.power.write(self.hc, self.hids, field, logical)
	}

	fn query(&mut self, local: u32) -> Result<SourceState, Error> {
		if local != LOCAL {
			return Err(Error::NotFound);
		}
		self.power.read_all(self.hc, self.hids);
		Ok(self.power.state())
	}
}

impl Module for Power {
	fn kind(&self) -> ClassKind {
		ClassKind::PowerDevice
	}

	fn inventory(&self) -> u8 {
		KIND_POWER
	}

	fn provider(&self) -> u16 {
		driver_protocol::provider::POWER_SOURCE
	}

	fn name(&self) -> &'static [u8] {
		driver_protocol::provider::USB_POWER_NAME
	}

	fn device(&self) -> &UsbDevice {
		&self.dev
	}

	fn device_mut(&mut self) -> &mut UsbDevice {
		&mut self.dev
	}

	// THE INTERRUPT PIPE STANDS FROM THE PUBLICATION ON: a UPS that loses mains says so once, in the input report
	// it sends then, and a provider that was not listening has no other way to learn it.
	fn start(&mut self, hc: &mut Xhci) {
		let length = self.table.reports(ReportKind::Input).map(|id| self.report_bytes(ReportKind::Input, id) as u32).max().unwrap_or(0).max(self.input.mps.min(64));
		self.input.post(hc, length);
	}

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
		// ONE CONSUMER, PowerService: a second connection is served, and holds nothing the first does.
		if self.consumer == 0 {
			self.consumer = chan;
		}
		if classes::correlation(&request).map(|(op, _)| op) == Some(power_provider::OP_UPDATES) {
			let mut view = View { power: self, hc, hids };
			let Some((corr, _)) = power_provider::updates_open(&mut view, &request, &mut handles) else { return true };
			let Some((producer, consumer)) = channel_with_depth(STREAM_DEPTH) else { return true };
			if self.stream != 0 {
				close(self.stream);
			}
			self.stream = producer;
			self.stream_seq = 0;
			send_caps_blocking(chan, &corr.to_le_bytes(), &[consumer]);
			// THE SNAPSHOT, then its end: the one source as it is now.
			let state = self.state();
			if self.frame(ProviderUpdateKind::Snapshot, Some(state.clone())) && self.frame(ProviderUpdateKind::SnapshotEnd, None) {
				self.published = Some(state);
			}
			return true;
		}
		let mut reply = [0u8; 2048];
		let mut reply_handles = wire::Handles::new();
		let mut view = View { power: self, hc, hids };
		if let Some(written) = power_provider::dispatch(&mut view, &request, &mut handles, &mut reply, &mut reply_handles) {
			send_caps_blocking(chan, &reply[..written], reply_handles.as_slice());
		}
		for &handle in handles.as_slice() {
			close(handle);
		}
		true
	}

	fn departed(&mut self, _hc: &mut Xhci, _hids: &mut Hids, chan: u64) {
		if self.consumer == chan {
			self.consumer = 0;
			if self.stream != 0 {
				close(self.stream);
				self.stream = 0;
			}
			self.published = None;
		}
	}

	fn absorb(&mut self, hc: &mut Xhci, hids: &mut Hids, _pointer: u64, status: u32, control: u32) -> bool {
		if !self.input.owns(control) {
			return false;
		}
		let (code, moved) = self.input.complete(status);
		if classes::succeeded(code) && moved > 0 {
			let bytes = self.input.read(moved as usize);
			let id = if self.table.uses_ids { bytes[0] } else { 0 };
			if let Some(body) = self.body_of(id, &bytes) {
				self.remember(ReportKind::Input, id, body);
				self.update();
			}
		} else if classes::stalled(code) {
			let _ = classes::clear_halt(hc, hids, &mut self.dev, &mut self.input);
		}
		self.start(hc);
		true
	}

	fn release(&mut self, hc: &mut Xhci) {
		if self.stream != 0 {
			close(self.stream);
			self.stream = 0;
		}
		self.input.release(hc);
	}
}
