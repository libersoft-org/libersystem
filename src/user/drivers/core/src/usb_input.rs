// THE USB HID BINDING ABOVE ANY HOST CONTROLLER: which interfaces of a configuration are HID ones, what each
// interface's reports decode into, and where the decoded input goes.
//
// IT WAS THE xHCI DRIVER'S, and a second controller would have had to copy it. Nothing here moves a byte on a
// bus: a controller reads the configuration and each report descriptor, posts the interrupt transfers, and hands
// every completed report to `feed`. What a report MEANS - keyboard-page changes diffed against the previous
// report of the same id into cooked events for the keys module and canonical transitions for InputService,
// pointer fields folded into the normalized state, contacts sent to the touch sink, every gamepad's state
// published through the publisher table `driver_protocol::gamepad` states the rules of - is decided here once,
// for xHCI, EHCI, OHCI and UHCI alike. The report-descriptor parser is the `hid` module.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use driver_protocol::gamepad;
use rt::*;

use crate::descriptor;
use crate::hid;
use crate::keys::{self, Mods};
use crate::usb_class::HID_INTERFACES;

// The HID class SET_PROTOCOL request (to the interface): wValue 0 selects the fixed boot-report layout - the
// fallback for a boot-subclass keyboard whose report descriptor cannot be read.
pub const HID_REQ_SET_PROTOCOL: u8 = 0x0b;

// The HID identity within a configuration: the HID class descriptor (embedded after the interface descriptor)
// names the report descriptor's length, and the report descriptor itself is read with an interface-targeted
// GET_DESCRIPTOR.
const DT_HID: u8 = 0x21;
pub const DESC_REPORT: u16 = 0x22;
pub const RT_INTERFACE_IN: u8 = 0x81;
const CLASS_HID: u8 = 3;
const SUBCLASS_BOOT: u8 = 1;
const PROTOCOL_KEYBOARD: u8 = 1;
const EP_ATTR_INTERRUPT: u8 = 3;

/// The most gamepads one controller publishes at once. A gamepad past it is bound and not published, and the
/// driver says so.
pub const MAX_PADS: usize = 16;

// The longest input report an interface is decoded from: a report page's first 64 bytes.
pub const MAX_REPORT: usize = 64;

/// The pointer-event sink: the server end of the channel InputService reads, set once at startup. Pointer reports
/// send through it from wherever they are reaped - the service loop or a wait deep inside a disk transfer.
pub static PTR_SINK: AtomicU64 = AtomicU64::new(0);
/// The raw key sink.
pub static KEY_SINK: AtomicU64 = AtomicU64::new(0);
/// The trusted key sink, for a keyboard: every transition goes there too, without waiting.
pub static TRUSTED_KEY_SINK: AtomicU64 = AtomicU64::new(0);
/// WHERE CONTACTS GO, AND IT IS NOT THE POINTER SINK.
///
/// A consumer of `POINTER` is handed ONE cursor, and a surface published as one would be flattened into whichever
/// contact was decoded last - which is a two-finger gesture arriving as a cursor that teleports.
/// `ProviderKind::Touch` sits beside it for that reason, and this is the end of it.
pub static TOUCH_SINK: AtomicU64 = AtomicU64::new(0);

/// One HID interface as the configuration descriptor declares it: its number, whether it is a boot keyboard, its
/// report descriptor's length, and its interrupt IN endpoint - its address, its maximum packet and its `bInterval`
/// as the descriptor has it, which each controller turns into its own schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Declared {
	pub iface: u8,
	pub boot_keyboard: bool,
	pub desc_len: u16,
	pub endpoint: Option<Endpoint>,
}

/// An interrupt IN endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
	pub address: u8,
	pub max_packet: u16,
	pub interval: u8,
}

/// EVERY HID INTERFACE OF A CONFIGURATION, any subclass - keyboards, pointing devices, multimedia controls and
/// gamepads alike - with its interrupt IN endpoint and its report descriptor's length, up to the four the class
/// budget charges a device for. An interface with no interrupt IN endpoint is not one.
///
/// EVERY FIELD IS READ FROM INSIDE ITS OWN RECORD. A record whose `bLength` is two is legal, and the class byte at
/// offset five of it is the next record's header. ONE ENTRY PER INTERFACE NUMBER: an alternate setting of an
/// interface already found is the same interface, and binding it twice would be two transfers on one endpoint.
pub fn declare(config: &[u8]) -> Vec<Declared> {
	let mut declared: Vec<Declared> = Vec::new();
	let mut current: Option<Declared> = None;
	for record in descriptor::Walk::new(config) {
		let kind = record.kind;
		if kind == descriptor::DT_INTERFACE {
			if let Some(done) = current.take() {
				declared.push(done);
			}
			let iface = record.field(2).unwrap_or(0);
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
		if kind == descriptor::DT_ENDPOINT && open.endpoint.is_none() {
			let (Ok(address), Ok(attrs)) = (record.field(2), record.field(3)) else { continue };
			if address & 0x80 != 0 && attrs & 0x3 == EP_ATTR_INTERRUPT {
				let (Ok(max_packet), Ok(interval)) = (record.field16(4), record.field(6)) else { continue };
				open.endpoint = Some(Endpoint { address, max_packet, interval });
			}
		}
	}
	if let Some(done) = current.take() {
		declared.push(done);
	}
	declared.retain(|found| found.endpoint.is_some());
	declared.truncate(HID_INTERFACES as usize);
	declared
}

/// WHAT AN INTERFACE'S REPORTS DECODE AGAINST, from what its report descriptor read gave (`None` for one that
/// could not be read). The device stays in its default report protocol - the layout tells the driver what each
/// report carries, no boot arrangement needed. A boot-subclass keyboard whose descriptor cannot be used is put
/// into the boot protocol instead (`set_boot_protocol`, the controller's SET_PROTOCOL) and decoded with the fixed
/// boot layout. None for an interface that carries nothing the system consumes - a HID power device takes this
/// path every time it is plugged in, and its transfer and page go back.
pub fn layout_for(report_descriptor: Option<&[u8]>, boot_keyboard: bool, set_boot_protocol: impl FnOnce() -> bool) -> Option<hid::Layout> {
	let layout = report_descriptor.map(hid::parse).unwrap_or_else(hid::Layout::empty);
	if layout.is_useful() {
		Some(layout)
	} else if boot_keyboard && set_boot_protocol() {
		Some(hid::boot_keyboard())
	} else {
		None
	}
}

// ONE GAMEPAD OF AN INTERFACE: its mapping, its handle on the `gamepad` wire (zero when the publisher's table was
// full and it was not published), the state its reports have mapped to, and whether an out-of-range axis has been
// reported - once per gamepad.
struct Pad {
	map: hid::GamepadMap,
	handle: u32,
	state: gamepad::State,
	said: bool,
}

/// ONE BOUND HID INTERFACE'S INPUT STATE: its number, the parsed report layout its reports decode against, the
/// previous report body per report id (the state the key diffs run against), the tracked modifiers, the running
/// pointer state a pointing device's reports fold into, its gamepads - and the driver its lines are said by.
pub struct Input {
	pub iface: u8,
	pub layout: hid::Layout,
	prevs: Vec<(u8, [u8; MAX_REPORT])>,
	mods: Mods,
	x: i32,
	y: i32,
	buttons: u8,
	pads: Vec<Pad>,
	who: &'static [u8],
}

impl Input {
	/// An interface's input state over `layout`, its lines said as `who` (`driver.xhci`).
	pub fn new(iface: u8, layout: hid::Layout, who: &'static [u8]) -> Input {
		Input { iface, layout, prevs: Vec::new(), mods: Mods::default(), x: 0, y: 0, buttons: 0, pads: Vec::new(), who }
	}

	pub fn has_gamepad(&self) -> bool {
		!self.pads.is_empty()
	}

	/// EACH GAMEPAD OF THIS INTERFACE ATTACHED TO THE PUBLICATION, labelled by where it is: the device, the port,
	/// the interface - and which collection, for a second one in one interface. Its state is the one before the
	/// first report until a report changes it, which is what its ARRIVAL implies.
	pub fn attach_pads(&mut self, vendor: u16, product: u16, port: u32, pads: &mut Pads) {
		for (index, map) in self.layout.gamepads().into_iter().enumerate() {
			let label = pad_label(vendor, product, port, self.iface, index);
			let Some(shape) = map.shape(&label) else { continue };
			let handle = match pads.publisher.attach(shape) {
				Some(handle) => handle,
				None => {
					say(self.who, b"a gamepad was bound and not published: gamepad slots or lifetime handles are exhausted");
					0
				}
			};
			self.pads.push(Pad { map, handle, state: shape.initial(), said: false });
		}
	}
}

/// THE GAMEPAD PUBLICATION: the publisher table, and the consumer connection its frames go to - the publication's
/// first connection, the offered one until InputService has opened it. Held beside a controller's bound devices,
/// because a gamepad's report is reaped wherever a HID report is.
pub struct Pads {
	publisher: gamepad::Publisher<MAX_PADS>,
	sink: u64,
}

// The connection, as the publisher's sender: a frame goes, or it is owed. NEVER A CLOSE - see `gamepad::Sender`.
struct Sink(u64);

impl gamepad::Sender for Sink {
	fn send(&mut self, frame: &[u8]) -> bool {
		matches!(try_send_outcome(self.0, frame, 0), SendOutcome::Delivered)
	}
}

impl Default for Pads {
	fn default() -> Self {
		Self::new()
	}
}

impl Pads {
	pub const fn new() -> Pads {
		Pads { publisher: gamepad::Publisher::new(), sink: 0 }
	}

	/// Follow the connection the publication is served on. A new one - the first or any later one - is owed an
	/// ARRIVAL and a STATE for every gamepad held; none is owed nothing.
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

	/// Send what is owed, oldest first, as the connection takes it.
	pub fn flush(&mut self) {
		if self.sink != 0 {
			self.publisher.flush(&mut Sink(self.sink));
		}
	}

	/// Whether anything is owed - what a service loop's one-tick retry is armed on.
	pub fn owes(&self) -> bool {
		self.publisher.owes()
	}
}

/// Every gamepad of a device that is going away departs - before its transfers and pages are released.
pub fn depart<'a>(pads: &mut Pads, inputs: impl Iterator<Item = &'a Input>) {
	for pad in inputs.flat_map(|input| input.pads.iter()) {
		if pad.handle != 0 {
			pads.publisher.detach(pad.handle);
		}
	}
	pads.flush();
}

/// DECODE ONE INPUT REPORT through the interface's layout and feed the results on: keyboard- and Consumer-page
/// changes (diffed against the previous report of the same report id) go to the shared keys module, pointer fields
/// fold into the running pointer state, sent to InputService when it changed - the same [x u16 LE][y u16 LE]
/// [buttons u8][wheel i8] frame the virtio pointer sends - contacts go to the touch sink, and each gamepad the
/// report carries is mapped and, when its state changed, published.
pub fn feed(input: &mut Input, pads: &mut Pads, report: &[u8]) {
	if report.is_empty() {
		return;
	}
	let report = &report[..report.len().min(MAX_REPORT)];
	let (id, body): (u8, &[u8]) = if input.layout.uses_ids() { (report[0], &report[1..]) } else { (0, report) };
	let prev_i: usize = match input.prevs.iter().position(|&(pid, _)| pid == id) {
		Some(i) => i,
		None => {
			input.prevs.push((id, [0u8; MAX_REPORT]));
			input.prevs.len() - 1
		}
	};
	let (layout, prevs, mods): (&hid::Layout, &mut Vec<(u8, [u8; MAX_REPORT])>, &mut Mods) = (&input.layout, &mut input.prevs, &mut input.mods);
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
		// A SYSTEM KEY - the consumer page's brightness pair - goes on the raw sink as its own five-byte frame, and
		// never on the trusted keyboard's sink, which carries the keyboard page alone.
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
	let (mut x, mut y, mut buttons, mut wheel): (i32, i32, u8, i32) = (input.x, input.y, input.buttons, 0);
	if layout.pointer_fold(id, body, &mut x, &mut y, &mut buttons, &mut wheel) && (x != input.x || y != input.y || buttons != input.buttons || wheel != 0) {
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
		input.x = x;
		input.y = y;
		input.buttons = buttons;
	}
	// AND THE CONTACTS, WHICH ARE NOT A POINTER AND ARE NOT DIFFED.
	//
	// A key is a state that CHANGED and a pointer is a position that MOVED, so both are compared against the
	// previous report. A contact is neither: what a touch report carries is who is down and where, and a finger
	// held still is still down. Diffing them would make a stationary finger disappear and its lift arrive as
	// nothing.
	//
	// ONE MESSAGE PER CONTACT, which is the shape InputService's own note asks for: a frame carrying several would
	// need a count beside the bytes, and the stream it publishes is per contact anyway.
	//
	// ASKED ONLY OF A DEVICE THAT HAS THE PAGE, so a keyboard and a mouse cost no walk at all.
	if layout.has_digitizer() {
		let sink: u64 = TOUCH_SINK.load(Ordering::Relaxed);
		if sink != 0 {
			let mut contacts: [hid::Contact; hid::MAX_CONTACTS] = [hid::Contact { id: 0, tip: false, x: 0, y: 0 }; hid::MAX_CONTACTS];
			let found: usize = layout.contacts(id, body, &mut contacts);
			for contact in contacts.iter().take(found) {
				// `[id u8][tip u8][x u16 LE][y u16 LE]` - see `input_service`, which states this shape where it
				// reads it.
				let mut msg: [u8; 6] = [0u8; 6];
				msg[0] = contact.id;
				msg[1] = contact.tip as u8;
				msg[2..4].copy_from_slice(&(contact.x.clamp(0, u16::MAX as i32) as u16).to_le_bytes());
				msg[4..6].copy_from_slice(&(contact.y.clamp(0, u16::MAX as i32) as u16).to_le_bytes());
				// NON-BLOCKING, like the pointer beside it: a consumer that is not reading must not hold the
				// controller's event loop, and a dropped contact is a frame of a gesture rather than a state nothing
				// can recover.
				let _ = try_send(sink, &msg, 0);
			}
		}
	}
	// AND THE GAMEPADS, WHICH ARE NONE OF THE ABOVE. A state goes out only when the mapped state CHANGED - a
	// gamepad at rest sends no next report, and an unchanged one is not a change - and one the connection cannot
	// take is owed rather than dropped, so a release is never lost behind a full channel.
	for pad in input.pads.iter_mut().filter(|pad| pad.map.reads(id)) {
		let mut state = pad.state;
		let applied = pad.map.apply(id, body, &mut state);
		if applied.refused && !pad.said {
			say(input.who, b"a gamepad reported an axis outside the range its descriptor declared; that axis keeps its last value");
			pad.said = true;
		}
		if state != pad.state {
			pad.state = state;
			if pad.handle != 0 {
				pads.publisher.report(pad.handle, state);
			}
		}
	}
	if !input.pads.is_empty() {
		pads.flush();
	}
	// One place for the tail rule, with one test: see `hid::remember`.
	hid::remember(&mut prevs[prev_i].1, body);
}

fn say(who: &[u8], line: &[u8]) {
	print(who);
	print(b": ");
	print(line);
	print(b"\n");
}

/// `usb VVVV:PPPP port P if I`, and ` pad N` for the Nth collection of one interface past the first.
pub fn pad_label(vendor: u16, product: u16, port: u32, iface: u8, index: usize) -> Vec<u8> {
	let mut label: Vec<u8> = Vec::with_capacity(gamepad::MAX_LABEL);
	label.extend_from_slice(b"usb ");
	push_hex4(&mut label, vendor);
	label.push(b':');
	push_hex4(&mut label, product);
	label.extend_from_slice(b" port ");
	push_decimal(&mut label, port);
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

#[cfg(test)]
mod tests;
