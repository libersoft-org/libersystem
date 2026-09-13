// The input driver's decoding, as pure decisions: what a device says about its own axes, and what one
// event does to the pointer - tested on the host through the crate's seam.
//
// WHAT THE DEVICE CHOOSES HERE. The axis range comes from the device's `ABS_INFO` block, and the
// pointer's whole coordinate system is derived from it: a range of zero makes every position
// normalise to zero, and a negative one - the block is unsigned on the wire and signed here - makes
// the clamp an empty interval, so the pointer sits in the corner whatever the device reports.
//
// AND THE EVENT FOLD IS WHERE A POINTER'S BEHAVIOUR LIVES: an absolute axis sets, a relative one
// nudges and clamps, a wheel accumulates a MOMENTARY delta that is not part of the held state, and a
// button is a bit. Those are four rules and they were four arms of a match inside an `unsafe`
// function over a raw address, which is why they had no test.

// The virtio_input_event types this driver reads.
pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_REL: u16 = 2;
pub const EV_ABS: u16 = 3;

// The axis and button codes.
pub const AXIS_X: u16 = 0;
pub const AXIS_Y: u16 = 1;
pub const REL_WHEEL: u16 = 8;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

// The range a relative device is given, since it reports none.
pub const REL_RANGE: i32 = 0x7fff;

// What a normalised coordinate is scaled to.
pub const NORM_MAX: u16 = u16::MAX;

// The maximum an absolute axis reports, from its `ABS_INFO` block.
//
// A BLOCK SHORTER THAN THE FIELD IS NOT A BLOCK: the maximum is the second of five unsigned words, so
// anything under eight bytes does not contain it - and reading it anyway returns whatever the config
// window holds. A maximum that is zero or negative once it is signed is not a range either.
pub fn axis_max(size: u8, max_word: u32) -> Option<i32> {
	if size < 8 {
		return None;
	}
	let max = max_word as i32;
	(max > 0).then_some(max)
}

// The bound to clamp an axis to: the device's own when it has one, the relative range otherwise.
pub fn axis_bound(reported: Option<i32>) -> i32 {
	reported.unwrap_or(REL_RANGE)
}

// The pointer state accumulated across one event group.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pointer {
	pub x: i32,
	pub y: i32,
	pub buttons: u8,
}

// Fold one event into the pointer state. Returns true on `EV_SYN` - the end of a group, and the point
// at which the accumulated state is a complete frame.
pub fn fold(state: &mut Pointer, wheel: &mut i32, kind: u16, code: u16, value: i32, max_x: i32, max_y: i32) -> bool {
	match kind {
		EV_SYN => return true,
		EV_ABS => match code {
			AXIS_X => state.x = value.clamp(0, max_x),
			AXIS_Y => state.y = value.clamp(0, max_y),
			_ => {}
		},
		EV_REL => match code {
			AXIS_X => state.x = state.x.saturating_add(value).clamp(0, max_x),
			AXIS_Y => state.y = state.y.saturating_add(value).clamp(0, max_y),
			// THE WHEEL ACCUMULATES AND SATURATES. It is a momentary delta reported per frame, and a
			// device that reports two billion ticks in one group must not wrap it into a scroll the
			// other way.
			REL_WHEEL => *wheel = wheel.saturating_add(value),
			_ => {}
		},
		EV_KEY => {
			let bit: u8 = match code {
				BTN_LEFT => 1,
				BTN_RIGHT => 2,
				BTN_MIDDLE => 4,
				_ => 0,
			};
			if bit != 0 {
				if value != 0 {
					state.buttons |= bit;
				} else {
					state.buttons &= !bit;
				}
			}
		}
		_ => {}
	}
	false
}

// One axis position scaled onto the normalised range a consumer receives.
pub fn normalize(value: i32, max: i32) -> u16 {
	if max <= 0 {
		return 0;
	}
	let value = value.clamp(0, max);
	((value as u64 * NORM_MAX as u64) / max as u64) as u16
}

#[cfg(test)]
mod tests;
