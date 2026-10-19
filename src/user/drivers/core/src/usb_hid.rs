// The HID side of driver.xhci: the xHCI half of the descriptor-driven input-device binding.
//
// EVERY HID INTERFACE of a device found during enumeration is configured here, up to four - each with its own
// interrupt IN endpoint and ring and its OWN REPORT PAGE (the device's data page carries its control transfers
// and cannot be shared by several standing reports) - and the device is held once in the `Hids` set the whole
// driver threads through its waits, with its interfaces. Each completed input report is routed by slot and
// endpoint to its interface and handed to the shared binding, `drivers::usb_input`, which decides which interfaces
// are HID ones and what their reports mean - the same code every other USB host controller's driver runs. The
// controller plumbing (rings, transfers, recovery) stays here and in xhci.rs; the report-descriptor parser is the
// `hid` module.

use alloc::vec::Vec;

use crate::{CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, DESC_CONFIG, FEATURE_DEVICE_REMOTE_WAKEUP, FEATURE_ENDPOINT_HALT, REQ_CLEAR_FEATURE, REQ_GET_DESCRIPTOR, REQ_SET_CONFIGURATION, REQ_SET_FEATURE, RT_ENDPOINT, TRB_CONFIGURE_ENDPOINT, TRB_EV_TRANSFER, TRB_IOC, TRB_NORMAL};
use crate::{Ring, UsbDevice, Xhci};
use crate::{command_and_wait, control_in, control_in_req, control_nodata, dma_page, r8, reset_endpoint, w32};
use drivers::descriptor;
use drivers::hid;
use drivers::usb_input::{self, DESC_REPORT, HID_REQ_SET_PROTOCOL, Input, RT_INTERFACE_IN};

// THE SINKS AND THE GAMEPAD PUBLICATION ARE THE SHARED BINDING'S - see `drivers::usb_input` - and are named here
// under the names the rest of this driver has always used for them.
pub use drivers::usb_input::{KEY_SINK, PTR_SINK, Pads, TOUCH_SINK, TRUSTED_KEY_SINK};

// A report page: the DMA page one interface's standing input report lands in.
struct Page {
	handle: u64,
	virt: u64,
	phys: u64,
}

// ONE BOUND HID INTERFACE: its interrupt IN endpoint's device context index and its transfer ring, its report page,
// and the input state its reports decode through - the shared binding's.
pub struct Hid {
	dci: u32,
	ring: Ring,
	page: Page,
	posted: bool,
	pub input: Input,
}

impl Hid {
	// Give this interface's ring and report page back. Called when the device goes away, and for an interface
	// that turned out to carry nothing the system consumes: a DMA page that nobody closes is a page leaked per
	// attach.
	pub fn release(&mut self) {
		self.ring.release();
		drivers::common::release_dma(self.page.handle);
		self.page.handle = 0;
	}

	pub fn has_gamepad(&self) -> bool {
		self.input.has_gamepad()
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
		self.interfaces().any(|h| h.input.layout.has_keyboard())
	}

	// Whether any bound interface reports a pointer.
	pub fn any_pointer(&self) -> bool {
		self.interfaces().any(|h| h.input.layout.has_pointer())
	}

	// Whether any bound interface reports contacts.
	pub fn any_digitizer(&self) -> bool {
		self.interfaces().any(|h| h.input.layout.has_digitizer())
	}

	// Whether any bound interface carries a gamepad.
	pub fn any_gamepad(&self) -> bool {
		self.interfaces().any(Hid::has_gamepad)
	}
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

		// EVERY HID INTERFACE, up to four, with its interrupt IN endpoint and its report descriptor's length - the
		// shared binding's walk, read from inside each record.
		let declared = usb_input::declare(config_bytes);
		if declared.is_empty() {
			return None;
		}

		// A RING AND A REPORT PAGE PER INTERFACE, before the controller is told about any of them: an allocation
		// that fails leaves nothing configured.
		let mut interfaces: Vec<Hid> = Vec::with_capacity(declared.len());
		for found in declared.iter() {
			let dci = endpoint_dci(found.endpoint?);
			let ring = Ring::new();
			let page = dma_page();
			match (ring, page) {
				(Some(ring), Some((handle, virt, phys))) => interfaces.push(Hid { dci, ring, page: Page { handle, virt, phys }, posted: false, input: Input::new(found.iface, hid::Layout::empty(), b"driver.xhci") }),
				(ring, page) => {
					if let Some(mut ring) = ring {
						ring.release();
					}
					if let Some((handle, _, _)) = page {
						drivers::common::release_dma(handle);
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
			let endpoint = found.endpoint?;
			let (dci, mps, interval) = (endpoint_dci(endpoint), endpoint.max_packet as u32, crate::classes::interrupt_interval(dev.speed, endpoint.interval as u32));
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
			// A REPORT DESCRIPTOR IS PARSED OVER WHAT ARRIVED and not over what was asked for: the data page is
			// reused, so the bytes past a short answer are the configuration descriptor this driver read a moment
			// ago.
			let report = match found.desc_len {
				0 => None,
				desc_len => control_in_req(hc, &mut pending, dev, RT_INTERFACE_IN, REQ_GET_DESCRIPTOR, DESC_REPORT << 8, found.iface as u16, desc_len.min(1024)).map(|received| core::slice::from_raw_parts(dev.data_virt as *const u8, received.min(desc_len.min(1024) as u32) as usize)),
			};
			let chosen = usb_input::layout_for(report, found.boot_keyboard, || control_nodata(hc, &mut pending, dev, 0x21, HID_REQ_SET_PROTOCOL, 0, found.iface as u16).is_some());
			let Some(layout) = chosen else {
				// NOT AN INPUT INTERFACE, and its ring and page go back: a HID power device takes this path every
				// time it is plugged in, and the pages used to stay pinned for the life of the driver.
				h.release();
				continue;
			};
			h.input.layout = layout;
			bound.push(h);
		}
		if bound.is_empty() {
			return None;
		}
		// A KEYBOARD THAT CAN WAKE THE MACHINE is told it may, here, while no report is in flight: its remote wakeup only
		// acts while its port is suspended, and enabling it at a sleep would be a control transfer racing the keys it
		// reports. A pointer is not enabled - moving it is not a reason to wake. A device that refuses keeps working.
		if declares_remote_wakeup && bound.iter().any(|h| h.input.layout.has_keyboard()) {
			dev.remote_wakeup = control_nodata(hc, &mut pending, dev, 0x00, REQ_SET_FEATURE, FEATURE_DEVICE_REMOTE_WAKEUP, 0).is_some();
		}
		// EACH GAMEPAD IS ATTACHED TO THE PUBLICATION, labelled by where it is - see `usb_input::Input::attach_pads`.
		for h in bound.iter_mut() {
			h.input.attach_pads(dev.vendor, dev.product, dev.port, pads);
		}
		Some(bound)
	}
}

// The device context index of an endpoint address: twice its number, plus one for IN.
fn endpoint_dci(endpoint: usb_input::Endpoint) -> u32 {
	(endpoint.address & 0xf) as u32 * 2 + (endpoint.address >> 7) as u32
}

// Post an interface's next input-report TRB (sized by its layout, into its own report page) and ring its
// doorbell.
fn post_report(hc: &mut Xhci, dev: &UsbDevice, h: &mut Hid) {
	if !hc.hid_rx.contains(&(dev.slot, h.dci)) {
		hc.hid_rx.push((dev.slot, h.dci));
	}
	unsafe {
		h.ring.push(h.page.phys, h.input.layout.report_bytes(), TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, h.dci);
		h.posted = true;
	}
}

// Post the first report TRB of every HID interface that is not serving yet (at the service loop's start, and
// after a runtime attach).
pub fn post_reports(hc: &mut Xhci, hids: &mut Hids) {
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
		let Some(i) = hids.entries.iter().position(|(dev, _)| dev.slot == slot) else {
			crate::keep_hid(hc, status, control);
			return;
		};
		let Some(j) = hids.entries[i].1.iter().position(|h| h.dci == dci) else {
			crate::keep_hid(hc, status, control);
			return;
		};
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
		let len: usize = (h.input.layout.report_bytes() as usize).saturating_sub((status & 0xff_ffff) as usize).min(usb_input::MAX_REPORT);
		let mut report: [u8; usb_input::MAX_REPORT] = [0u8; usb_input::MAX_REPORT];
		for (i, slot) in report.iter_mut().enumerate().take(len) {
			*slot = r8(h.page.virt + i as u64);
		}
		usb_input::feed(&mut h.input, pads, &report[..len]);
		post_report(hc, dev, h);
	}
}

// Every gamepad of a device that is going away departs - before its rings and pages are released.
pub fn depart(pads: &mut Pads, interfaces: &[Hid]) {
	usb_input::depart(pads, interfaces.iter().map(|h| &h.input));
}
