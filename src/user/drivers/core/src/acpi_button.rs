// THE ACPI BUTTONS AND THE LID - the parts of the `acpi_button` driver a host can test: which class a node is, what its
// `Notify` values mean, and what `_LID` answers.
//
// A control-method power button (`PNP0C0C`), sleep button (`PNP0C0E`) or lid (`PNP0C0D`). A button raises `Notify(0x80)`
// when it is pressed and `Notify(0x02)` when it woke the machine; a lid raises `0x80` when it moved, and `_LID` answers
// nonzero for open.

use aml::wire::Value;

/// What a node is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
	PowerButton,
	SleepButton,
	Lid,
}

/// The class a row's match ids name, `_HID` first and each `_CID` after.
pub fn class_of<'a>(ids: impl IntoIterator<Item = &'a [u8]>) -> Option<Class> {
	for id in ids {
		match id {
			b"PNP0C0C" => return Some(Class::PowerButton),
			b"PNP0C0E" => return Some(Class::SleepButton),
			b"PNP0C0D" => return Some(Class::Lid),
			_ => {}
		}
	}
	None
}

/// What one `Notify` value means to a node of this class.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
	/// A button pressed, or a lid moved - the lid is read again.
	Pressed,
	/// The device woke the machine: said, and never a second request.
	Woke,
	/// A value this class does not raise.
	Other(u32),
}

pub const NOTIFY_PRESSED: u32 = 0x80;
pub const NOTIFY_WAKE: u32 = 0x02;

pub fn event_of(value: u32) -> Event {
	match value {
		NOTIFY_PRESSED => Event::Pressed,
		NOTIFY_WAKE => Event::Woke,
		other => Event::Other(other),
	}
}

/// `_LID`'s answer: `Ok(true)` for closed. Anything but an integer is refused by name.
pub fn lid_closed(value: &Value) -> Result<bool, &'static str> {
	match value {
		Value::Integer(open) => Ok(*open == 0),
		_ => Err("_LID answered something that is not an integer"),
	}
}

#[cfg(test)]
mod tests;
