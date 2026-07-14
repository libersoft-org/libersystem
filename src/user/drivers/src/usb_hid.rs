// The HID side of driver.xhci: the descriptor-driven input-device binding.
//
// A HID interface found during enumeration is configured here (its interrupt IN
// endpoint brought up, its report descriptor read and parsed into a `hid::Layout`),
// held in the `Hids` set the whole driver threads through its waits, and served:
// each completed input report diffs through the layout into key events for the
// shared keys module and folds into the normalized pointer state sent to
// InputService. The controller plumbing (rings, transfers, recovery) stays in
// xhci.rs; the report-descriptor parser is the `hid` module.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use rt::*;

use crate::hid;
use crate::keys::{self, Mods};
use crate::{CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, DESC_CONFIG, DT_ENDPOINT, DT_INTERFACE, FEATURE_ENDPOINT_HALT, REQ_CLEAR_FEATURE, REQ_GET_DESCRIPTOR, REQ_SET_CONFIGURATION, RT_ENDPOINT, SPEED_HIGH, SPEED_SUPER, TRB_CONFIGURE_ENDPOINT, TRB_EV_TRANSFER, TRB_IOC, TRB_NORMAL};
use crate::{Ring, UsbDevice, Xhci};
use crate::{command_and_wait, control_in, control_in_req, control_nodata, r8, reset_endpoint, w32};

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

// A configured HID input device: the interrupt IN endpoint's device context index
// and its transfer ring, on which input reports of the descriptor's size are
// posted and reaped; the parsed report layout the reports decode against; the
// previous report body per report id (the state the key diffs run against); the
// tracked modifiers; and the running pointer state (normalized position and
// buttons) a pointing device's reports fold into.
pub struct Hid {
	dci: u32,
	ring: Ring,
	pub layout: hid::Layout,
	posted: bool,
	prevs: Vec<(u8, [u8; 64])>,
	mods: Mods,
	x: i32,
	y: i32,
	buttons: u8,
}

// The bound HID devices the service loop reaps reports for. The synchronous
// control / transport waits service their events inline, so typing is never lost
// behind disk traffic; bring-up paths pass an empty set (no report TRB is in
// flight before the service loop posts the first one).
pub struct Hids {
	pub entries: Vec<(UsbDevice, Hid)>,
}

impl Hids {
	pub const fn new() -> Hids {
		Hids { entries: Vec::new() }
	}

	// Whether any bound device reports keyboard-page keys.
	pub fn any_keyboard(&self) -> bool {
		self.entries.iter().any(|(_, h)| h.layout.has_keyboard())
	}

	// Whether any bound device reports a pointer.
	pub fn any_pointer(&self) -> bool {
		self.entries.iter().any(|(_, h)| h.layout.has_pointer())
	}
}

// The pointer-event sink: the server end of the channel InputService reads,
// set once at startup. Pointer reports send through it from wherever they are
// reaped - the service loop or a wait deep inside a disk transfer.
pub static PTR_SINK: AtomicU64 = AtomicU64::new(0);
pub static KEY_SINK: AtomicU64 = AtomicU64::new(0);

// Configure the device's HID function, if it has one: read the configuration
// descriptor, find a HID interface (any subclass - keyboards, pointing devices
// and multimedia controls alike), its interrupt IN endpoint and its report
// descriptor's length, bring the endpoint up with a configure-endpoint command,
// select the configuration, then read and parse the report descriptor - the
// layout the device's input reports decode against. A boot-subclass keyboard
// whose report descriptor cannot be read falls back to the fixed boot protocol.
// None when the device carries no HID function whose reports the system consumes,
// or any step fails.
pub unsafe fn configure_hid(hc: &mut Xhci, dev: &mut UsbDevice) -> Option<Hid> {
	unsafe {
		// no HID device is serving yet, so the control waits see no HID events.
		let mut pending: Hids = Hids::new();
		// the configuration descriptor head names the total length; read it whole.
		control_in(hc, &mut pending, dev, DESC_CONFIG, 9)?;
		let total: u16 = (r8(dev.data_virt + 2) as u16 | (r8(dev.data_virt + 3) as u16) << 8).min(1024);
		let config_value: u16 = r8(dev.data_virt + 5) as u16;
		control_in(hc, &mut pending, dev, DESC_CONFIG, total)?;

		// walk the descriptors for a HID interface, its report descriptor's length
		// (the HID class descriptor rides between the interface and its endpoints)
		// and its interrupt IN endpoint.
		let mut offset: u64 = 0;
		let mut in_hid: bool = false;
		let mut boot_keyboard: bool = false;
		let mut iface: u16 = 0;
		let mut desc_len: u16 = 0;
		let mut found: Option<(u32, u32, u32)> = None; // (dci, mps, interval)
		while offset + 2 <= total as u64 {
			let length: u64 = r8(dev.data_virt + offset) as u64;
			let kind: u8 = r8(dev.data_virt + offset + 1);
			if length < 2 {
				break;
			}
			if kind == DT_INTERFACE && found.is_none() {
				in_hid = r8(dev.data_virt + offset + 5) == CLASS_HID;
				if in_hid {
					iface = r8(dev.data_virt + offset + 2) as u16;
					boot_keyboard = r8(dev.data_virt + offset + 6) == SUBCLASS_BOOT && r8(dev.data_virt + offset + 7) == PROTOCOL_KEYBOARD;
				}
			}
			if kind == DT_HID && in_hid && found.is_none() && length >= 9 {
				// wDescriptorLength of the (first) class descriptor, the report one.
				desc_len = r8(dev.data_virt + offset + 7) as u16 | (r8(dev.data_virt + offset + 8) as u16) << 8;
			}
			if kind == DT_ENDPOINT && in_hid && found.is_none() {
				let ep_addr: u8 = r8(dev.data_virt + offset + 2);
				let attrs: u8 = r8(dev.data_virt + offset + 3);
				if ep_addr & 0x80 != 0 && attrs & 0x3 == EP_ATTR_INTERRUPT {
					let mps: u32 = r8(dev.data_virt + offset + 4) as u32 | (r8(dev.data_virt + offset + 5) as u32) << 8;
					let interval: u32 = ep_interval(dev.speed, r8(dev.data_virt + offset + 6) as u32);
					found = Some(((ep_addr & 0xf) as u32 * 2 + 1, mps, interval));
				}
			}
			offset += length;
		}
		let (dci, mps, interval): (u32, u32, u32) = found?;

		// bring the endpoint up: the input context adds the slot context (its context
		// entries grown to cover the new DCI) and the interrupt IN endpoint context.
		let ring: Ring = Ring::new()?;
		core::ptr::write_bytes(dev.in_virt as *mut u8, 0, 4096);
		((dev.in_virt + 4) as *mut u32).write_volatile(1 | 1 << dci);
		let slot_ctx: u64 = dev.in_virt + hc.ctx_size;
		(slot_ctx as *mut u32).write_volatile(dci << 27 | dev.speed << 20 | dev.route);
		((slot_ctx + 4) as *mut u32).write_volatile(dev.port << 16);
		// endpoint context: interrupt IN (type 7), error count 3, the polling
		// interval, the ring's base, and the report size as the average/ESIT payload.
		let ep_ctx: u64 = dev.in_virt + (1 + dci as u64) * hc.ctx_size;
		(ep_ctx as *mut u32).write_volatile(interval << 16);
		((ep_ctx + 4) as *mut u32).write_volatile(mps << 16 | 7 << 3 | 3 << 1);
		((ep_ctx + 8) as *mut u32).write_volatile((ring.phys | ring.cycle as u64) as u32);
		((ep_ctx + 12) as *mut u32).write_volatile((ring.phys >> 32) as u32);
		((ep_ctx + 16) as *mut u32).write_volatile(8 | mps << 16);
		command_and_wait(hc, dev.in_phys, 0, TRB_CONFIGURE_ENDPOINT << 10 | dev.slot << 24)?;

		// select the configuration, then read and parse the report descriptor. The
		// device stays in its default report protocol - the layout tells the driver
		// what each report carries, no boot arrangement needed. A boot-subclass
		// keyboard whose descriptor cannot be read is put into the boot protocol
		// instead and decoded with the fixed boot layout.
		control_nodata(hc, &mut pending, dev, 0x00, REQ_SET_CONFIGURATION, config_value, 0)?;
		let layout: hid::Layout = match desc_len {
			0 => hid::Layout::empty(),
			_ => match control_in_req(hc, &mut pending, dev, RT_INTERFACE_IN, REQ_GET_DESCRIPTOR, DESC_REPORT << 8, iface, desc_len.min(1024)) {
				Some(()) => hid::parse(core::slice::from_raw_parts(dev.data_virt as *const u8, desc_len.min(1024) as usize)),
				None => hid::Layout::empty(),
			},
		};
		let layout: hid::Layout = if layout.is_useful() {
			layout
		} else if boot_keyboard {
			control_nodata(hc, &mut pending, dev, 0x21, HID_REQ_SET_PROTOCOL, 0, iface)?;
			hid::boot_keyboard()
		} else {
			return None;
		};
		Some(Hid { dci, ring, layout, posted: false, prevs: Vec::new(), mods: Mods::default(), x: 0, y: 0, buttons: 0 })
	}
}

// The xHCI endpoint-context interval field for an interrupt endpoint: the exponent
// of the service interval in 125 us microframes. High/SuperSpeed descriptors carry
// the exponent + 1 already; a full/low-speed bInterval counts 1 ms frames, so find
// the smallest exponent whose period covers it (bInterval * 8 microframes).
fn ep_interval(speed: u32, b_interval: u32) -> u32 {
	if speed == SPEED_HIGH || speed == SPEED_SUPER {
		return b_interval.clamp(1, 16) - 1;
	}
	let mut exp: u32 = 3;
	while exp < 15 && 1 << (exp - 3) < b_interval {
		exp += 1;
	}
	exp
}

// Post a HID device's next input-report TRB (sized by its layout) and ring its
// doorbell.
unsafe fn post_report(hc: &Xhci, dev: &UsbDevice, h: &mut Hid) {
	unsafe {
		h.ring.push(dev.data_phys, h.layout.report_bytes(), TRB_NORMAL << 10 | TRB_IOC);
		w32(hc.db + dev.slot as u64 * 4, h.dci);
		h.posted = true;
	}
}

// Post the first report TRB of every HID device that is not serving yet (at the
// service loop's start, and after a runtime attach).
pub unsafe fn post_reports(hc: &Xhci, hids: &mut Hids) {
	unsafe {
		for (dev, h) in hids.entries.iter_mut() {
			if !h.posted {
				post_report(hc, dev, h);
			}
		}
	}
}

// Handle one event ring entry against the bound HID devices: a successful
// transfer event for a device's interrupt endpoint is a fresh input report,
// which is decoded through its layout and the next report TRB posted. A stalled
// report is recovered (the endpoint unhalted, its ring repositioned, the
// device-side halt cleared) and reposted. Every other event is ignored.
pub unsafe fn handle_hid_event(hc: &mut Xhci, hids: &mut Hids, status: u32, control: u32) {
	unsafe {
		let kind: u32 = control >> 10 & 0x3f;
		let code: u32 = status >> 24;
		if kind != TRB_EV_TRANSFER {
			return;
		}
		let Some(i) = hids.entries.iter().position(|(dev, h)| control >> 24 == dev.slot && (control >> 16 & 0x1f) == h.dci) else {
			return;
		};
		let (dev, h) = &mut hids.entries[i];
		if code == CC_STALL {
			// the endpoint is halted (no reports can be in flight on it), so the
			// recovery's own waits run against the other devices only.
			let dequeue: u64 = h.ring.phys + h.ring.index * 16 | h.ring.cycle as u64;
			let (slot, dci): (u32, u32) = (dev.slot, h.dci);
			let addr: u16 = 0x80 | (dci >> 1) as u16;
			let mut rest: Hids = Hids { entries: core::mem::take(&mut hids.entries) };
			let mut moved: (UsbDevice, Hid) = rest.entries.swap_remove(i);
			reset_endpoint(hc, &mut rest, slot, dci, dequeue);
			let _ = control_nodata(hc, &mut rest, &mut moved.0, RT_ENDPOINT, REQ_CLEAR_FEATURE, FEATURE_ENDPOINT_HALT, addr);
			post_report(hc, &moved.0, &mut moved.1);
			rest.entries.push(moved);
			hids.entries = rest.entries;
			return;
		}
		if code != CC_SUCCESS && code != CC_SHORT_PACKET {
			return;
		}
		let len: usize = (h.layout.report_bytes() as usize).saturating_sub((status & 0xff_ffff) as usize).min(64);
		let mut report: [u8; 64] = [0u8; 64];
		for (i, slot) in report.iter_mut().enumerate().take(len) {
			*slot = r8(dev.data_virt + i as u64);
		}
		feed_hid_report(h, &report[..len]);
		post_report(hc, dev, h);
	}
}

// Decode one input report through the device's layout and feed the results on:
// keyboard- and Consumer-page changes (diffed against the previous report of the
// same report id) go to the shared keys module, and pointer fields fold into the
// running pointer state, sent to InputService when it changed - the same
// [x u16 LE][y u16 LE][buttons u8][wheel i8] frame the virtio pointer sends.
unsafe fn feed_hid_report(h: &mut Hid, report: &[u8]) {
	unsafe {
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
			if page == 0x07 && key_sink != 0 {
				let event: [u8; 3] = [raw as u8, (raw >> 8) as u8, down as u8];
				let _ = send_blocking(key_sink, &event, 0);
			}
			let code: u16 = usage_keycode(usage);
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
		let body_len: usize = body.len().min(64);
		prevs[prev_i].1[..body_len].copy_from_slice(&body[..body_len]);
	}
}

// Resolve a page-extended HID usage to its keycode: the keyboard page through
// the boot-usage table (its modifier range through the modifier map), the
// Consumer page through the multimedia map. 0 = unmapped.
fn usage_keycode(usage: u32) -> u16 {
	let (page, u): (u16, u32) = ((usage >> 16) as u16, usage & 0xffff);
	match page {
		0x07 if (0xe0..=0xe7).contains(&u) => keys::HID_MODIFIER_KEYCODES[(u - 0xe0) as usize],
		0x07 if u <= 0xff => keys::hid_keycode(u as u8),
		0x0c => keys::consumer_keycode(u as u16),
		_ => 0,
	}
}
