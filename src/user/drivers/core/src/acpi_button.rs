// THE ACPI BUTTONS AND THE LID - the parts of the `acpi_button` driver a host can test: which class a row is and where
// its presses come from, what its `Notify` values mean, what `_LID` answers, and who acts on a press.
//
// A control-method power button (`PNP0C0C`), sleep button (`PNP0C0E`) or lid (`PNP0C0D`). A button raises `Notify(0x80)`
// when it is pressed and `Notify(0x02)` when it woke the machine; a lid raises `0x80` when it moved, and `_LID` answers
// nonzero for open. A FIXED-HARDWARE power or sleep button is the kernel's row (`LNXPWRBN`, `LNXSLPBN`) with no node: its
// presses arrive from DeviceManager.

use aml::wire::Value;

/// What a node is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
	PowerButton,
	SleepButton,
	Lid,
}

/// Where a row's presses come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
	/// Its namespace node's `Notify`.
	Node,
	/// DeviceManager's `PRESSED`: the PM1 event the kernel decoded, for the kernel's row of a fixed-hardware button.
	Fixed,
}

/// The class a row's match ids name, `_HID` first and each `_CID` after, and where its presses come from.
pub fn class_of<'a>(ids: impl IntoIterator<Item = &'a [u8]>) -> Option<(Class, Source)> {
	for id in ids {
		match id {
			b"PNP0C0C" => return Some((Class::PowerButton, Source::Node)),
			b"PNP0C0E" => return Some((Class::SleepButton, Source::Node)),
			b"PNP0C0D" => return Some((Class::Lid, Source::Node)),
			b"LNXPWRBN" => return Some((Class::PowerButton, Source::Fixed)),
			b"LNXSLPBN" => return Some((Class::SleepButton, Source::Fixed)),
			_ => {}
		}
	}
	None
}

/// Who acts on a button's press.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Actor {
	/// The consumer of the button's `platform-switch` - the power-state policy - which decides what a press does.
	Consumer,
	/// The driver itself, as it did before any policy existed: the power button powers off, the sleep button suspends. So
	/// a press never does nothing for want of a consumer.
	Driver,
}

/// THE PRESS'S ACTOR: the consumer when one watches the button and the press reached its stream; the driver otherwise -
/// no consumer, or one whose stream is full or gone.
pub fn actor(watched: bool, delivered: bool) -> Actor {
	if watched && delivered { Actor::Consumer } else { Actor::Driver }
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
