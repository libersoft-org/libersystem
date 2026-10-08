// The HID side of driver.xhci: the descriptor-driven input-device binding.
//
// EVERY HID INTERFACE of a device found during enumeration is configured here, up to four - each with its
// own interrupt IN endpoint and ring, its OWN REPORT PAGE (the device's data page carries its control
// transfers and cannot be shared by several standing reports), its report descriptor parsed into a
// `hid::Layout`, its previous reports and its gamepads - and the device is held once in the `Hids` set the
// whole driver threads through its waits, with its interfaces. Each completed input report is routed by slot
// and endpoint to its interface and decoded through that layout: keyboard-page changes become cooked events
// for the shared keys module plus canonical transitions for InputService, pointer fields fold into the
// normalized state sent there, contacts go to the touch sink - and every gamepad's state goes out on the
// `gamepad` provider, through the publisher table `driver_protocol::gamepad` states the rules of. The
// controller plumbing (rings, transfers, recovery) stays in xhci.rs; the report-descriptor parser is the
// `hid` module.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use driver_protocol::gamepad;
use rt::*;

use crate::{CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, DESC_CONFIG, DT_ENDPOINT, DT_INTERFACE, FEATURE_DEVICE_REMOTE_WAKEUP, FEATURE_ENDPOINT_HALT, REQ_CLEAR_FEATURE, REQ_GET_DESCRIPTOR, REQ_SET_CONFIGURATION, REQ_SET_FEATURE, RT_ENDPOINT, TRB_CONFIGURE_ENDPOINT, TRB_EV_TRANSFER, TRB_IOC, TRB_NORMAL};
use crate::{Ring, UsbDevice, Xhci};
use crate::{command_and_wait, control_in, control_in_req, control_nodata, dma_page, r8, reset_endpoint, w32};
use drivers::descriptor;
use drivers::hid;
use drivers::keys::{self, Mods};
use drivers::usb_class::HID_INTERFACES;

// The HID class SET_PROTOCOL request (to the interface): wValue 0 selects the
// fixed boot-report layout - the fallback for a boot-subclass keyboard whose
// report descriptor cannot be read.
const HID_REQ_SET_PROTOCOL: u8 = 0x0b;

// The HID identity within a configuration: the HID class descriptor (embedded
// after the interface descriptor) names the report descriptor's length, and the
// report descriptor itself is read with an interface-targeted GET_DESCRIPTOR.
const DT_HID: u8 = 0x21;
const DESC_REPORT: u16 = 0x22;
const RT_INTERFACE_IN: u8 = 0x81;
const CLASS_HID: u8 = 3;
const SUBCLASS_BOOT: u8 = 1;
const PROTOCOL_KEYBOARD: u8 = 1;
const EP_ATTR_INTERRUPT: u8 = 3;

/// The most gamepads this controller publishes at once. A gamepad past it is bound and not published, and
/// the driver says so.
pub const MAX_PADS: usize = 16;

// A report page: the DMA page one interface's standing input report lands in.
struct Page {
	handle: u64,
	virt: u64,
	phys: u64,
}

// ONE GAMEPAD OF AN INTERFACE: its mapping, its handle on the `gamepad` wire (zero when the publisher's table
// was full and it was not published), the state its reports have mapped to, and whether an out-of-range axis
// has been reported - once per gamepad.
struct Pad {
	map: hid::GamepadMap,
	handle: u32,
	state: gamepad::State,
	said: bool,
}

// ONE BOUND HID INTERFACE: its number, its interrupt IN endpoint's device context index and its transfer ring,
// its report page, the parsed report layout its reports decode against, the previous report body per report id
// (the state the key diffs run against), the tracked modifiers, the running pointer state a pointing device's
// reports fold into, and its gamepads.
pub struct Hid {
	iface: u8,
	dci: u32,
	ring: Ring,
	page: Page,
	pub layout: hid::Layout,
	posted: bool,
	prevs: Vec<(u8, [u8; 64])>,
	mods: Mods,
	x: i32,
	y: i32,
	buttons: u8,
	pads: Vec<Pad>,
}

impl Hid {
	// Give this interface's ring and report page back. Called when the device goes away, and for an interface
	// that turned out to carry nothing the system consumes: a DMA page that nobody closes is a page leaked per
	// attach.
	pub fn release(&mut self) {
		self.ring.release();
		if self.page.handle != 0 {
			close(self.page.handle);
			self.page.handle = 0;
		}
	}

	pub fn has_gamepad(&self) -> bool {
		!self.pads.is_empty()
	}
}

// THE GAMEPAD PUBLICATION: the publisher table, and the consumer connection its frames go to - the
// publication's first connection, the offered one until InputService has opened it. Held in `Hids`, because
// a gamepad's report is reaped wherever a HID event is, the service loop or a wait deep inside a transfer.
pub struct Pads {
	publisher: gamepad::Publisher<MAX_PADS>,
	sink: u64,
}

// The connection, as the publisher's sender: a frame goes, or it is owed. NEVER A CLOSE - see
// `gamepad::Sender`.
struct Sink(u64);

impl gamepad::Sender for Sink {
	fn send(&mut self, frame: &[u8]) -> bool {
		matches!(try_send_outcome(self.0, frame, 0), SendOutcome::Delivered)
	}
}

impl Pads {
	pub const fn new() -> Pads {
		Pads { publisher: gamepad::Publisher::new(), sink: 0 }
	}

	// Follow the connection the publication is served on. A new one - the first or any later one - is owed an
	// ARRIVAL and a STATE for every gamepad held; none is owed nothing.
	pub fn follow(&mut self, sink: u64) {
		if sink == self.sink {
			return;
		}
		self.sink = sink;
		if sink != 0 {
			self.publisher.connected();
		} else {
			self.publisher.disconnected();
		}
		self.flush();
	}

	// Send what is owed, oldest first, as the connection takes it.
	pub fn flush(&mut self) {
		if self.sink != 0 {
			self.publisher.flush(&mut Sink(self.sink));
		}
	}

	// Whether anything is owed - what the service loop's one-tick retry is armed on.
	pub fn owes(&self) -> bool {
		self.publisher.owes()
	}
}

// The bound HID devices the service loop reaps reports for, each with its interfaces, and the gamepad
// publication. The synchronous control / transport waits service their events inline, so typing is never
// lost behind disk traffic; bring-up paths pass an empty set (no report TRB is in flight before the service
// loop posts the first one).
pub struct Hids {
	pub entries: Vec<(UsbDevice, Vec<Hid>)>,
	pub pads: Pads,
}

impl Hids {
	pub const fn new() -> Hids {
		Hids { entries: Vec::new(), pads: Pads::new() }
	}

	fn interfaces(&self) -> impl Iterator<Item = &Hid> {
		self.entries.iter().flat_map(|(_, interfaces)| interfaces.iter())
	}

	// Whether a bound keyboard's remote wakeup is enabled - what a suspend to idle can be woken through.
	pub fn remote_wakeup(&self) -> bool {
		self.entries.iter().any(|(dev, _)| dev.remote_wakeup)
	}

	// Whether any bound interface reports keyboard-page keys.
	pub fn any_keyboard(&self) -> bool {
		self.interfaces().any(|h| h.layout.has_keyboard())
	}

	// Whether any bound interface reports a pointer.
	pub fn any_pointer(&self) -> bool {
		self.interfaces().any(|h| h.layout.has_pointer())
	}

	// Whether any bound interface reports contacts.
	pub fn any_digitizer(&self) -> bool {
		self.interfaces().any(|h| h.layout.has_digitizer())
	}

	// Whether any bound interface carries a gamepad.
	pub fn any_gamepad(&self) -> bool {
		self.interfaces().any(Hid::has_gamepad)
	}
}

// The pointer-event sink: the server end of the channel InputService reads,
// set once at startup. Pointer reports send through it from wherever they are
// reaped - the service loop or a wait deep inside a disk transfer.
pub static PTR_SINK: AtomicU64 = AtomicU64::new(0);
pub static KEY_SINK: AtomicU64 = AtomicU64::new(0);
/// The trusted key sink, for a keyboard: every transition goes there too, without waiting.
pub static TRUSTED_KEY_SINK: AtomicU64 = AtomicU64::new(0);
/// WHERE CONTACTS GO, AND IT IS NOT THE POINTER SINK.
///
/// A consumer of `POINTER` is handed ONE cursor, and a surface published as one would be flattened
/// into whichever contact was decoded last - which is a two-finger gesture arriving as a cursor that
/// teleports. `ProviderKind::Touch` sits beside it for that reason, and this is the end of it.
pub static TOUCH_SINK: AtomicU64 = AtomicU64::new(0);

// One HID interface as the configuration descriptor declares it: its number, whether it is a boot keyboard,
// its report descriptor's length, and its interrupt IN endpoint (dci, max packet, interval).
struct Declared {
	iface: u8,
	boot_keyboard: bool,
	desc_len: u16,
	endpoint: Option<(u32, u32, u32)>,
}

// Configure the device's HID function, if it has one: read the configuration descriptor, find EVERY HID
// interface (any subclass - keyboards, pointing devices, multimedia controls and gamepads alike) with its
// interrupt IN endpoint and its report descriptor's length, up to four, bring ALL their endpoints up with ONE
// configure-endpoint command, select the configuration, then read and parse each report descriptor - the
// layout that interface's reports decode against. A boot-subclass keyboard whose report descriptor cannot be
// read falls back to the fixed boot protocol, per interface. The device is bound when at least one interface
// carries something the system consumes; the others give their ring and page back. Each gamepad of a bound
// interface is attached to the publisher under a label naming where it is. None when the device carries no
// HID interface whose reports the system consumes, or any step fails.
pub unsafe fn configure_hid(hc: &mut Xhci, dev: &mut UsbDevice, pads: &mut Pads) -> Option<Vec<Hid>> {
	unsafe {
		// no HID device is serving yet, so the control waits see no HID events.
		let mut pending: Hids = Hids::new();
		// the configuration descriptor head names the total length; read it whole.
		let head = control_in(hc, &mut pending, dev, DESC_CONFIG, 9)?;
		// THE HEAD IS A RECORD LIKE ANY OTHER: nine bytes, of which the total length is two and the
		// configuration value is one - and a device that returned four of them has not sent them.
		let head_bytes = core::slice::from_raw_parts(dev.data_virt as *const u8, head.min(9) as usize);
		let head_record = descriptor::Walk::new(head_bytes).next()?;
		descriptor::check_type(DESC_CONFIG as u8, head_record.kind).ok()?;
		let total: u16 = head_record.field16(2).ok()?.min(1024);
		let config_value: u16 = head_record.field(5).ok()? as u16;
		// `bmAttributes` bit 5: the device can signal its own resume from suspend.
		let declares_remote_wakeup = head_record.field(7).is_ok_and(|attributes| attributes & 0x20 != 0);
		let received = control_in(hc, &mut pending, dev, DESC_CONFIG, total)?;
		// AND THE WHOLE DESCRIPTOR HAS TO HAVE ARRIVED before it is walked.
		let total = descriptor::check_transfer(total, received).ok()?;
		let config_bytes = core::slice::from_raw_parts(dev.data_virt as *const u8, total as usize);

		// walk the descriptors for every HID interface, its report descriptor's length (the HID class
		// descriptor rides between the interface and its endpoints) and its interrupt IN endpoint.
		// EVERY FIELD IS READ FROM INSIDE ITS OWN RECORD. A record whose `bLength` is two is legal, and the
		// class byte at offset five of it is the next record's header.
		let mut declared: Vec<Declared> = Vec::new();
		let mut current: Option<Declared> = None;
		for record in descriptor::Walk::new(config_bytes) {
			let kind = record.kind;
			if kind == DT_INTERFACE {
				if let Some(done) = current.take() {
					declared.push(done);
				}
				let iface = record.field(2).unwrap_or(0);
				// ONE ENTRY PER INTERFACE NUMBER: an alternate setting of an interface already found is the
				// same interface, and binding it twice would be two rings on one endpoint.
				if record.field(5) == Ok(CLASS_HID) && !declared.iter().any(|seen| seen.iface == iface) {
					current = Some(Declared { iface, boot_keyboard: record.field(6) == Ok(SUBCLASS_BOOT) && record.field(7) == Ok(PROTOCOL_KEYBOARD), desc_len: 0, endpoint: None });
				}
				continue;
			}
			let Some(open) = current.as_mut() else { continue };
			if kind == DT_HID && open.desc_len == 0 {
				// wDescriptorLength of the (first) class descriptor, the report one.
				open.desc_len = record.field16(7).unwrap_or(0);
			}
			if kind == DT_ENDPOINT && open.endpoint.is_none() {
				let (Ok(ep_addr), Ok(attrs)) = (record.field(2), record.field(3)) else { continue };
				if ep_addr & 0x80 != 0 && attrs & 0x3 == EP_ATTR_INTERRUPT {
					let (Ok(mps), Ok(interval)) = (record.field16(4), record.field(6)) else { continue };
					open.endpoint = Some(((ep_addr & 0xf) as u32 * 2 + 1, mps as u32, crate::classes::interrupt_interval(dev.speed, interval as u32)));
				}
			}
		}
		if let Some(done) = current.take() {
			declared.push(done);
		}
		// UP TO FOUR, the interfaces the class budget charged this device for.
		declared.retain(|found| found.endpoint.is_some());
		declared.truncate(HID_INTERFACES as usize);
		if declared.is_empty() {
			return None;
		}

		// A RING AND A REPORT PAGE PER INTERFACE, before the controller is told about any of them: an allocation
		// that fails leaves nothing configured.
		let mut interfaces: Vec<Hid> = Vec::with_capacity(declared.len());
		for found in declared.iter() {
			let (dci, _, _) = found.endpoint?;
			let ring = Ring::new();
			let page = dma_page();
			match (ring, page) {
				(Some(ring), Some((handle, virt, phys))) => interfaces.push(Hid { iface: found.iface, dci, ring, page: Page { handle, virt, phys }, layout: hid::Layout::empty(), posted: false, prevs: Vec::new(), mods: Mods::default(), x: 0, y: 0, buttons: 0, pads: Vec::new() }),
				(ring, page) => {
					if let Some(mut ring) = ring {
						ring.release();
					}
					if let Some((handle, _, _)) = page {
						close(handle);
					}
					for mut made in interfaces {
						made.release();
					}
					return None;
				}
			}
		}

		// bring EVERY endpoint up in ONE configure-endpoint command: the input context adds the slot context
		// (its context entries grown to cover the highest new DCI) and each interrupt IN endpoint context.
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		let entries: u32 = interfaces.iter().map(|h| h.dci).max().unwrap_or(1);
		let add: u32 = interfaces.iter().fold(1, |flags, h| flags | 1 << h.dci);
		((dev.in_virt + 4) as *mut u32).write_volatile(add);
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(entries << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		for (h, found) in interfaces.iter().zip(declared.iter()) {
			let (dci, mps, interval) = found.endpoint?;
			// endpoint context: interrupt IN (type 7), error count 3, the polling interval, the ring's base,
			// and the report size as the average/ESIT payload.
			let ep_ctx: u64 = dev.in_virt + (1 + dci as u64) * hc.ctx_size;
			(ep_ctx as *mut u32).write_volatile(interval << 16);
			((ep_ctx + 4) as *mut u32).write_volatile(mps << 16 | 7 << 3 | 3 << 1);
			((ep_ctx + 8) as *mut u32).write_volatile((h.ring.phys | h.ring.cycle as u64) as u32);
			((ep_ctx + 12) as *mut u32).write_volatile((h.ring.phys >> 32) as u32);
			((ep_ctx + 16) as *mut u32).write_volatile(8 | mps << 16);
		}
		let configured = command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24).is_some();

		// select the configuration, then read and parse each report descriptor. The device stays in its
		// default report protocol - the layout tells the driver what each report carries, no boot arrangement
		// needed. A boot-subclass keyboard whose descriptor cannot be read is put into the boot protocol
		// instead and decoded with the fixed boot layout.
		if !configured || control_nodata(hc, &mut pending, dev, 0x00, REQ_SET_CONFIGURATION, config_value, 0).is_none() {
			for mut made in interfaces {
				made.release();
			}
			return None;
		}
		let mut bound: Vec<Hid> = Vec::with_capacity(interfaces.len());
		for (mut h, found) in interfaces.into_iter().zip(declared.iter()) {
			let layout: hid::Layout = match found.desc_len {
				0 => hid::Layout::empty(),
				// A REPORT DESCRIPTOR IS PARSED OVER WHAT ARRIVED and not over what was asked for: the data
				// page is reused, so the bytes past a short answer are the configuration descriptor this
				// driver read a moment ago.
				desc_len => match control_in_req(hc, &mut pending, dev, RT_INTERFACE_IN, REQ_GET_DESCRIPTOR, DESC_REPORT << 8, found.iface as u16, desc_len.min(1024)) {
					Some(received) => hid::parse(core::slice::from_raw_parts(dev.data_virt as *const u8, received.min(desc_len.min(1024) as u32) as usize)),
					None => hid::Layout::empty(),
				},
			};
			h.layout = if layout.is_useful() {
				layout
			} else if found.boot_keyboard && control_nodata(hc, &mut pending, dev, 0x21, HID_REQ_SET_PROTOCOL, 0, found.iface as u16).is_some() {
				hid::boot_keyboard()
			} else {
				// NOT AN INPUT INTERFACE, and its ring and page go back: a HID power device takes this path every
				// time it is plugged in, and the pages used to stay pinned for the life of the driver.
				h.release();
				continue;
			};
			bound.push(h);
		}
		if bound.is_empty() {
			return None;
		}
		// A KEYBOARD THAT CAN WAKE THE MACHINE is told it may, here, while no report is in flight: its remote wakeup only
		// acts while its port is suspended, and enabling it at a sleep would be a control transfer racing the keys it
		// reports. A pointer is not enabled - moving it is not a reason to wake. A device that refuses keeps working.
		if declares_remote_wakeup && bound.iter().any(|h| h.layout.has_keyboard()) {
			dev.remote_wakeup = control_nodata(hc, &mut pending, dev, 0x00, REQ_SET_FEATURE, FEATURE_DEVICE_REMOTE_WAKEUP, 0).is_some();
		}
		// EACH GAMEPAD IS ATTACHED TO THE PUBLICATION, labelled by where it is: the device, the port, the
		// interface - and which collection, for a second one in one interface. Its state is the one before the
		// first report until a report changes it, which is what its ARRIVAL implies.
		for h in bound.iter_mut() {
			for (index, map) in h.layout.gamepads().into_iter().enumerate() {
				let label = pad_label(dev, h.iface, index);
				let Some(shape) = map.shape(&label) else { continue };
				let handle = match pads.publisher.attach(shape) {
					Some(handle) => handle,
					None => {
						print(b"driver.xhci: a gamepad was bound and not published: gamepad slots or lifetime handles are exhausted\n");
						0
					}
				};
				h.pads.push(Pad { map, handle, state: shape.initial(), said: false });
			}
		}
		Some(bound)
	}
}

// `usb VVVV:PPPP port P if I`, and ` pad N` for the Nth collection of one interface past the first.
fn pad_label(dev: &UsbDevice, iface: u8, index: usize) -> Vec<u8> {
	let mut label: Vec<u8> = Vec::with_capacity(gamepad::MAX_LABEL);
	label.extend_from_slice(b"usb ");
	push_hex4(&mut label, dev.vendor);
	label.push(b':');
	push_hex4(&mut label, dev.product);
	label.extend_from_slice(b" port ");
	push_decimal(&mut label, dev.port);
	label.extend_from_slice(b" if ");
	push_decimal(&mut label, iface as u32);
	if index > 0 {
		label.extend_from_slice(b" pad ");
		push_decimal(&mut label, index as u32 + 1);
	}
	label.truncate(gamepad::MAX_LABEL);
	label
}

fn push_hex4(out: &mut Vec<u8>, value: u16) {
	for shift in [12u16, 8, 4, 0] {
		out.push(b"0123456789abcdef"[(value >> shift & 0xf) as usize]);
	}
}

fn push_decimal(out: &mut Vec<u8>, value: u32) {
	let mut digits = [0u8; 10];
	let mut count = 0;
	let mut rest = value;
	loop {
		digits[count] = b'0' + (rest % 10) as u8;
		count += 1;
		rest /= 10;
		if rest == 0 {
			break;
		}
	}
	out.extend(digits[..count].iter().rev());
}

// Post an interface's next input-report TRB (sized by its layout, into its own report page) and ring its
// doorbell.
fn post_report(hc: &Xhci, dev: &UsbDevice, h: &mut Hid) {
	unsafe {
		h.ring.push(h.page.phys, h.layout.report_bytes(), TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, h.dci);
		h.posted = true;
	}
}

// Post the first report TRB of every HID interface that is not serving yet (at the service loop's start, and
// after a runtime attach).
pub fn post_reports(hc: &Xhci, hids: &mut Hids) {
	for (dev, interfaces) in hids.entries.iter_mut() {
		for h in interfaces.iter_mut().filter(|h| !h.posted) {
			post_report(hc, dev, h);
		}
	}
}

// Handle one event ring entry against the bound HID interfaces: a successful transfer event for an
// interface's interrupt endpoint - ROUTED BY SLOT AND ENDPOINT - is a fresh input report, which is decoded
// through its layout and the next report TRB posted. A stalled report is recovered (the endpoint unhalted,
// its ring repositioned, the device-side halt cleared) and reposted. Every other event is ignored.
pub fn handle_hid_event(hc: &mut Xhci, hids: &mut Hids, status: u32, control: u32) {
	unsafe {
		let kind: u32 = control >> 10 & 0x3f;
		let code: u32 = status >> 24;
		if kind != TRB_EV_TRANSFER {
			return;
		}
		let (slot, dci) = (control >> 24, control >> 16 & 0x1f);
		let Some(i) = hids.entries.iter().position(|(dev, _)| dev.slot == slot) else { return };
		let Some(j) = hids.entries[i].1.iter().position(|h| h.dci == dci) else { return };
		if code == CC_STALL {
			// the endpoint is halted (no reports can be in flight on it), so the recovery's own waits run
			// against the other devices only - and the gamepad publication, which a report reaped meanwhile
			// is published through.
			let dequeue: u64 = {
				let h = &hids.entries[i].1[j];
				h.ring.phys + h.ring.index * 16 | h.ring.cycle as u64
			};
			let addr: u16 = 0x80 | (dci >> 1) as u16;
			let mut rest: Hids = Hids { entries: core::mem::take(&mut hids.entries), pads: core::mem::replace(&mut hids.pads, Pads::new()) };
			let mut moved: (UsbDevice, Vec<Hid>) = rest.entries.swap_remove(i);
			reset_endpoint(hc, &mut rest, slot, dci, dequeue);
			let _ = control_nodata(hc, &mut rest, &mut moved.0, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, addr);
			post_report(hc, &moved.0, &mut moved.1[j]);
			rest.entries.push(moved);
			hids.entries = rest.entries;
			hids.pads = rest.pads;
			return;
		}
		if code != CC_SUCCESS && code != CC_SHORT_PACKET {
			return;
		}
		let (entries, pads) = (&mut hids.entries, &mut hids.pads);
		let (dev, interfaces) = &mut entries[i];
		let h = &mut interfaces[j];
		let len: usize = (h.layout.report_bytes() as usize).saturating_sub((status & 0xff_ffff) as usize).min(64);
		let mut report: [u8; 64] = [0u8; 64];
		for (i, slot) in report.iter_mut().enumerate().take(len) {
			*slot = r8(h.page.virt + i as u64);
		}
		feed_hid_report(h, pads, &report[..len]);
		post_report(hc, dev, h);
	}
}

// Decode one input report through the interface's layout and feed the results on: keyboard- and
// Consumer-page changes (diffed against the previous report of the same report id) go to the shared keys
// module, pointer fields fold into the running pointer state, sent to InputService when it changed - the same
// [x u16 LE][y u16 LE][buttons u8][wheel i8] frame the virtio pointer sends - contacts go to the touch sink,
// and each gamepad the report carries is mapped and, when its state changed, published.
unsafe fn feed_hid_report(h: &mut Hid, pads: &mut Pads, report: &[u8]) {
	if report.is_empty() {
		return;
	}
	let (id, body): (u8, &[u8]) = if h.layout.uses_ids() { (report[0], &report[1..]) } else { (0, report) };
	let prev_i: usize = match h.prevs.iter().position(|&(pid, _)| pid == id) {
		Some(i) => i,
		None => {
			h.prevs.push((id, [0u8; 64]));
			h.prevs.len() - 1
		}
	};
	let (layout, prevs, mods): (&hid::Layout, &mut Vec<(u8, [u8; 64])>, &mut Mods) = (&h.layout, &mut h.prevs, &mut h.mods);
	layout.keys_diff(id, &prevs[prev_i].1, body, &mut |usage, down| {
		let page: u16 = (usage >> 16) as u16;
		let raw: u16 = usage as u16;
		let key_sink: u64 = KEY_SINK.load(Ordering::Relaxed);
		if page == 0x07 {
			let event: [u8; 3] = [raw as u8, (raw >> 8) as u8, down as u8];
			let trusted: u64 = TRUSTED_KEY_SINK.load(Ordering::Relaxed);
			if trusted != 0 {
				let _ = try_send(trusted, &event, 0);
			}
			if key_sink != 0 {
				let _ = send_blocking(key_sink, &event, 0);
			}
		}
		// A SYSTEM KEY - the consumer page's brightness pair - goes on the raw sink as its own five-byte frame, and never on
		// the trusted keyboard's sink, which carries the keyboard page alone.
		if page == 0x0c
			&& key_sink != 0
			&& let Some(frame) = keys::system_key_frame(raw, down)
		{
			let _ = send_blocking(key_sink, &frame, 0);
		}
		let code: u16 = keys::usage_keycode(usage);
		if code != 0 {
			keys::feed_key(code, down as u32, mods);
		}
	});
	let (mut x, mut y, mut buttons, mut wheel): (i32, i32, u8, i32) = (h.x, h.y, h.buttons, 0);
	if layout.pointer_fold(id, body, &mut x, &mut y, &mut buttons, &mut wheel) && (x != h.x || y != h.y || buttons != h.buttons || wheel != 0) {
		let mut msg: [u8; 6] = [0u8; 6];
		msg[0..2].copy_from_slice(&(x as u16).to_le_bytes());
		msg[2..4].copy_from_slice(&(y as u16).to_le_bytes());
		msg[4] = buttons;
		msg[5] = wheel.clamp(-127, 127) as i8 as u8;
		let sink: u64 = PTR_SINK.load(Ordering::Relaxed);
		if sink != 0 {
			// non-blocking: with no consumer routed, pointer events just drop.
			let _ = try_send(sink, &msg, 0);
		}
		h.x = x;
		h.y = y;
		h.buttons = buttons;
	}
	// AND THE CONTACTS, WHICH ARE NOT A POINTER AND ARE NOT DIFFED.
	//
	// A key is a state that CHANGED and a pointer is a position that MOVED, so both are compared
	// against the previous report. A contact is neither: what a touch report carries is who is down
	// and where, and a finger held still is still down. Diffing them would make a stationary finger
	// disappear and its lift arrive as nothing.
	//
	// ONE MESSAGE PER CONTACT, which is the shape InputService's own note asks for: a frame carrying
	// several would need a count beside the bytes, and the stream it publishes is per contact anyway.
	//
	// ASKED ONLY OF A DEVICE THAT HAS THE PAGE, so a keyboard and a mouse cost no walk at all.
	if layout.has_digitizer() {
		let sink: u64 = TOUCH_SINK.load(Ordering::Relaxed);
		if sink != 0 {
			let mut contacts: [hid::Contact; hid::MAX_CONTACTS] = [hid::Contact { id: 0, tip: false, x: 0, y: 0 }; hid::MAX_CONTACTS];
			let found: usize = layout.contacts(id, body, &mut contacts);
			for contact in contacts.iter().take(found) {
				// `[id u8][tip u8][x u16 LE][y u16 LE]` - see `input_service`, which states this
				// shape where it reads it.
				let mut msg: [u8; 6] = [0u8; 6];
				msg[0] = contact.id;
				msg[1] = contact.tip as u8;
				msg[2..4].copy_from_slice(&(contact.x.clamp(0, u16::MAX as i32) as u16).to_le_bytes());
				msg[4..6].copy_from_slice(&(contact.y.clamp(0, u16::MAX as i32) as u16).to_le_bytes());
				// NON-BLOCKING, like the pointer beside it: a consumer that is not reading must not
				// hold the controller's event loop, and a dropped contact is a frame of a gesture
				// rather than a state nothing can recover.
				let _ = try_send(sink, &msg, 0);
			}
		}
	}
	// AND THE GAMEPADS, WHICH ARE NONE OF THE ABOVE. A state goes out only when the mapped state CHANGED - a
	// gamepad at rest sends no next report, and an unchanged one is not a change - and one the connection
	// cannot take is owed rather than dropped, so a release is never lost behind a full channel.
	for pad in h.pads.iter_mut().filter(|pad| pad.map.reads(id)) {
		let mut state = pad.state;
		let applied = pad.map.apply(id, body, &mut state);
		if applied.refused && !pad.said {
			print(b"driver.xhci: a gamepad reported an axis outside the range its descriptor declared; that axis keeps its last value\n");
			pad.said = true;
		}
		if state != pad.state {
			pad.state = state;
			if pad.handle != 0 {
				pads.publisher.report(pad.handle, state);
			}
		}
	}
	if !h.pads.is_empty() {
		pads.flush();
	}
	// One place for the tail rule, with one test: see `hid::remember`.
	hid::remember(&mut prevs[prev_i].1, body);
}

// Every gamepad of a device that is going away departs - before its rings and pages are released.
pub fn depart(pads: &mut Pads, interfaces: &[Hid]) {
	for pad in interfaces.iter().flat_map(|h| h.pads.iter()) {
		if pad.handle != 0 {
			pads.publisher.detach(pad.handle);
		}
	}
	pads.flush();
}
