// THE HID HOST'S DECODING, BOTH RADIOS: a peer's report through the shared report-descriptor parser - the one a USB
// device's goes through, `drivers::hid` - into what InputService takes: pointer motion, key transitions with their
// usage page, and gamepad frames on the production gamepad wire. NEVER A HID REPORT, so InputService does not
// become a second HID implementation.
//
// A classic HID device and a HOGP one in report mode both bring a report descriptor; a device that brings none is
// driven in boot protocol, through the boot keyboard's or the boot mouse's descriptor, parsed the same way.

use alloc::vec::Vec;
use driver_protocol::gamepad as pad_wire;
use drivers::hid::{self, GamepadMap, Layout};
use proto::system::{GamepadFrame, InputReport, KeyTransition, MouseReport};

// The report ids one device's previous reports are kept for, for the key diff.
const MAX_IDS: usize = 16;
// The midpoint the pointer fold runs from, so a relative report comes back out as its own delta.
const MID: i32 = hid::NORM_MAX / 2;
// The boot mouse's report: three buttons and two relative 8-bit axes, the descriptor HID's appendix gives.
#[rustfmt::skip]
const BOOT_MOUSE: [u8; 50] = [
	0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00,
	0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02,
	0x95, 0x01, 0x75, 0x05, 0x81, 0x03,
	0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06,
	0xc0, 0xc0,
];

struct Pad {
	map: GamepadMap,
	shape: pad_wire::Shape,
	state: pad_wire::State,
	handle: u32,
}

pub(crate) struct Decoder {
	layout: Layout,
	previous: Vec<(u8, [u8; 64])>,
	buttons: u8,
	pads: Vec<Pad>,
}

fn frame(bytes: &[u8]) -> InputReport {
	InputReport::Gamepad(GamepadFrame { bytes: bytes.to_vec() })
}

impl Decoder {
	// A device's own report descriptor: `None` for one that decodes into nothing this system consumes.
	pub fn new(descriptor: &[u8]) -> Option<Decoder> {
		let layout = hid::parse(descriptor);
		layout.is_useful().then(|| Decoder::of(layout))
	}

	pub fn boot_keyboard() -> Decoder {
		Decoder::of(hid::boot_keyboard())
	}

	pub fn boot_mouse() -> Decoder {
		Decoder::of(hid::parse(&BOOT_MOUSE))
	}

	fn of(layout: Layout) -> Decoder {
		let mut pads = Vec::new();
		for (at, map) in layout.gamepads().into_iter().enumerate() {
			if let Some(shape) = map.shape(b"bluetooth gamepad") {
				pads.push(Pad { state: shape.initial(), shape, map, handle: at as u32 + 1 });
			}
		}
		Decoder { layout, previous: Vec::new(), buttons: 0, pads }
	}

	// What a consumer is told as its stream opens: each gamepad's arrival, with its shape.
	pub fn arrivals(&self) -> Vec<InputReport> {
		let mut out = Vec::new();
		for pad in &self.pads {
			let mut bytes = [0u8; pad_wire::MAX_FRAME];
			let len = pad_wire::encode_arrival(pad.handle, &pad.shape, &mut bytes);
			out.push(frame(&bytes[..len]));
		}
		out
	}

	// One report as it came: its id first where the descriptor uses ids.
	pub fn report(&mut self, bytes: &[u8]) -> Vec<InputReport> {
		match (self.layout.uses_ids(), bytes.split_first()) {
			(true, Some((&id, body))) => self.report_with_id(id, body),
			(true, None) => Vec::new(),
			(false, _) => self.report_with_id(0, bytes),
		}
	}

	// One report body under its id - which HOGP gives beside a notification rather than inside it.
	pub fn report_with_id(&mut self, id: u8, body: &[u8]) -> Vec<InputReport> {
		let mut out = Vec::new();
		let at = match self.previous.iter().position(|(held, _)| *held == id) {
			Some(at) => at,
			None if self.previous.len() < MAX_IDS => {
				self.previous.push((id, [0u8; 64]));
				self.previous.len() - 1
			}
			None => return out,
		};
		self.layout.keys_diff(id, &self.previous[at].1, body, &mut |usage, down| {
			out.push(InputReport::Key(KeyTransition { page: (usage >> 16) as u16, usage: usage as u16, down }));
		});
		hid::remember(&mut self.previous[at].1, body);
		let (mut x, mut y, mut buttons, mut wheel) = (MID, MID, self.buttons, 0);
		if self.layout.pointer_fold(id, body, &mut x, &mut y, &mut buttons, &mut wheel) && (x != MID || y != MID || buttons != self.buttons || wheel != 0) {
			self.buttons = buttons;
			out.push(InputReport::Pointer(MouseReport { dx: x - MID, dy: y - MID, wheel, buttons: buttons & 0x07 }));
		}
		for pad in self.pads.iter_mut().filter(|pad| pad.map.reads(id)) {
			let before = pad.state;
			pad.map.apply(id, body, &mut pad.state);
			if pad.state != before {
				let mut bytes = [0u8; pad_wire::MAX_FRAME];
				let len = pad_wire::encode_state(pad.handle, &pad.shape, &pad.state, &mut bytes);
				out.push(frame(&bytes[..len]));
			}
		}
		out
	}

	// EVERYTHING HELD, LET GO: every key a previous report holds goes up, the buttons up, and each gamepad departs -
	// what a consumer is told before a stream ends, so nothing is left down for ever.
	pub fn releases(&mut self) -> Vec<InputReport> {
		let mut out = Vec::new();
		let empty = [0u8; 64];
		for (id, previous) in self.previous.iter_mut() {
			self.layout.keys_diff(*id, previous, &empty, &mut |usage, down| {
				out.push(InputReport::Key(KeyTransition { page: (usage >> 16) as u16, usage: usage as u16, down }));
			});
			*previous = [0u8; 64];
		}
		if self.buttons != 0 {
			self.buttons = 0;
			out.push(InputReport::Pointer(MouseReport { dx: 0, dy: 0, wheel: 0, buttons: 0 }));
		}
		for pad in &self.pads {
			let mut bytes = [0u8; pad_wire::MAX_FRAME];
			let len = pad_wire::encode_departure(pad.handle, &mut bytes);
			out.push(frame(&bytes[..len]));
		}
		out
	}
}
