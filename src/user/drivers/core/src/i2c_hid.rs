// HID OVER I2C - the parts of the `i2c_hid` driver a host can test.
//
// WHAT A BINDING PUBLISHES, AND THE MODE A TOUCHPAD IS LEFT IN. The driver sends no Input Mode or Device Mode feature
// report, so a precision touchpad stays in the mouse mode it powers on in - and returns to after every RESET - and
// reports through its mouse collection alone: InputService derives no pointer from contacts, so a touchpad switched to
// its touch pad collection would deliver contacts and move no cursor. The report descriptor's APPLICATION COLLECTIONS
// then decide: a Mouse or Pointer collection publishes `pointer`; a Touch Screen collection, through which a
// touchscreen reports its contacts in the mode it starts in, publishes `touch`; a Touch Pad collection, silent without
// the switch, and any other collection publish nothing. EACH INPUT REPORT IS DECODED BY THE COLLECTION ITS REPORT ID
// BELONGS TO, so a touchscreen's contact axes - Generic Desktop X and Y like a mouse's - never fold into a pointer.
//
// THE RESET HANDSHAKE: power on, RESET, and the reset indication - a zero-length input report - awaited within
// `RESET_BOUND_TICKS`; a device that does not answer is sent RESET once more and then refused.
//
// THE STORM RULE: a line event with no report behind it resets the device once, and the next fails the binding. A
// level-triggered line the device holds asserted with nothing to read would otherwise be an event, a read and an
// acknowledgement for ever.

use crate::hid::{self, Layout};

/// The application collections this driver reads, page-extended.
pub const USAGE_POINTER: u32 = 0x0001_0001;
pub const USAGE_MOUSE: u32 = 0x0001_0002;
pub const USAGE_TOUCH_SCREEN: u32 = 0x000D_0004;
pub const USAGE_TOUCH_PAD: u32 = 0x000D_0005;

/// THE `_DSM` HID over I2C defines: its UUID, and the function that answers the HID descriptor register.
pub const DESCRIPTOR_DSM_UUID: &str = "3cdff6f7-4267-4555-ad05-b30a3d8938de";
pub const DESCRIPTOR_DSM_REVISION: u64 = 1;
pub const DESCRIPTOR_DSM_FUNCTION: u64 = 1;

/// How long the reset indication may take. The specification's bound is five seconds.
pub const RESET_BOUND_TICKS: u64 = 5 * rt::TICKS_PER_SECOND;

/// THE TREE'S `hid-descr-addr`, from the row's property block: the device's own value record of that name, one
/// big-endian cell. Records are `[kind][depth][name_len u16][value_len u32][name][value padded to four]`.
pub fn tree_descriptor_register(block: &[u8]) -> Option<u16> {
	let mut at = 0usize;
	while at + 8 <= block.len() {
		let (kind, depth) = (block[at], block[at + 1]);
		let name_len = u16::from_le_bytes([block[at + 2], block[at + 3]]) as usize;
		let value_len = u32::from_le_bytes([block[at + 4], block[at + 5], block[at + 6], block[at + 7]]) as usize;
		let name_at = at + 8;
		let value_at = name_at.checked_add(name_len)?;
		let end = value_at.checked_add((value_len + 3) & !3)?;
		if end > block.len() || value_at + value_len > block.len() {
			return None;
		}
		if kind == rt::DEVICE_PROPERTY_VALUE && depth == 0 && &block[name_at..value_at] == b"hid-descr-addr" && value_len == 4 {
			return u16::try_from(u32::from_be_bytes([block[value_at], block[value_at + 1], block[value_at + 2], block[value_at + 3]])).ok();
		}
		at = end;
	}
	None
}

/// THE `_DSM`'s ANSWER, in the node channel's value encoding - an integer, `0x01` and eight little-endian bytes - as the
/// register it names; anything else, or a number past sixteen bits, is not a register.
pub fn dsm_register(answer: &[u8]) -> Option<u16> {
	if answer.len() != 9 || answer[0] != 0x01 {
		return None;
	}
	u16::try_from(u64::from_le_bytes(answer[1..9].try_into().ok()?)).ok()
}

/// Where one input report goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
	Pointer,
	Touch,
	Nothing,
}

/// The collection report `id` belongs to, as a route.
pub fn route(layout: &Layout, id: u8) -> Route {
	match layout.application_of(id) {
		Some(USAGE_MOUSE | USAGE_POINTER) => Route::Pointer,
		Some(USAGE_TOUCH_SCREEN) => Route::Touch,
		_ => Route::Nothing,
	}
}

/// What a binding publishes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Publications {
	pub pointer: bool,
	pub touch: bool,
}

pub fn publications(layout: &Layout) -> Publications {
	let applications = layout.applications();
	Publications { pointer: applications.iter().any(|usage| matches!(*usage, USAGE_MOUSE | USAGE_POINTER)), touch: applications.contains(&USAGE_TOUCH_SCREEN) }
}

/// A report as the layout reads it: its id and its body.
pub fn split<'a>(layout: &Layout, report: &'a [u8]) -> Option<(u8, &'a [u8])> {
	if layout.uses_ids() { report.split_first().map(|(id, body)| (*id, body)) } else { Some((0, report)) }
}

/// THE POINTER'S RUNNING STATE, and the frame a report that moved it sends: `[x u16 LE][y u16 LE][buttons u8][wheel
/// i8]`, the frame every pointer provider sends.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Pointer {
	x: i32,
	y: i32,
	buttons: u8,
}

impl Pointer {
	/// Fold report `id` of the pointer route; the frame when it moved, clicked or scrolled.
	pub fn feed(&mut self, layout: &Layout, id: u8, body: &[u8]) -> Option<[u8; 6]> {
		let (mut x, mut y, mut buttons, mut wheel) = (self.x, self.y, self.buttons, 0i32);
		if !layout.pointer_fold(id, body, &mut x, &mut y, &mut buttons, &mut wheel) || (x == self.x && y == self.y && buttons == self.buttons && wheel == 0) {
			return None;
		}
		(self.x, self.y, self.buttons) = (x, y, buttons);
		let mut frame = [0u8; 6];
		frame[0..2].copy_from_slice(&(x.clamp(0, u16::MAX as i32) as u16).to_le_bytes());
		frame[2..4].copy_from_slice(&(y.clamp(0, u16::MAX as i32) as u16).to_le_bytes());
		frame[4] = buttons;
		frame[5] = wheel.clamp(-127, 127) as i8 as u8;
		Some(frame)
	}
}

/// REPORT `id`'s CONTACTS of the touch route, one frame each: `[id u8][tip u8][x u16 LE][y u16 LE]`.
pub fn contact_frames(layout: &Layout, id: u8, body: &[u8], out: &mut [[u8; 6]; hid::MAX_CONTACTS]) -> usize {
	let mut contacts = [hid::Contact::default(); hid::MAX_CONTACTS];
	let found = layout.contacts(id, body, &mut contacts);
	for (frame, contact) in out.iter_mut().zip(contacts.iter().take(found)) {
		frame[0] = contact.id;
		frame[1] = contact.tip as u8;
		frame[2..4].copy_from_slice(&(contact.x.clamp(0, u16::MAX as i32) as u16).to_le_bytes());
		frame[4..6].copy_from_slice(&(contact.y.clamp(0, u16::MAX as i32) as u16).to_le_bytes());
	}
	found
}

/// What a reset indication that did not come asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResetNext {
	/// Send RESET once more and wait again.
	Again,
	/// The device did not answer twice: refused.
	Refuse,
}

/// ONE RESET HANDSHAKE: the RESET sent, the indication awaited, retried once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ResetHandshake {
	sent: u8,
}

impl ResetHandshake {
	pub const fn new() -> Self {
		Self { sent: 1 }
	}

	pub fn timed_out(&mut self) -> ResetNext {
		if self.sent >= 2 {
			return ResetNext::Refuse;
		}
		self.sent += 1;
		ResetNext::Again
	}
}

impl Default for ResetHandshake {
	fn default() -> Self {
		Self::new()
	}
}

/// What a line event with no report behind it asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StormAnswer {
	/// Reset the device - the one reset this binding spends on a storm.
	Reset,
	/// The binding has spent it: fail.
	Fail,
}

/// THE STORM RULE, for one binding.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Storm {
	reset_spent: bool,
}

impl Storm {
	pub fn empty_event(&mut self) -> StormAnswer {
		if self.reset_spent {
			return StormAnswer::Fail;
		}
		self.reset_spent = true;
		StormAnswer::Reset
	}
}

#[cfg(test)]
mod tests;
