//! WHICH METHOD ANSWERS AN EVENT. A general-purpose event is answered by `\_GPE._Lxx` (level) or `\_GPE._Exx` (edge),
//! `xx` its number in two hexadecimal digits; a GPIO-signalled event by the same names in the controller's scope, and
//! by `_EVT(pin)` for a pin above 255 or one no name matches. An edge event's status is acknowledged before its method
//! runs and a level event's after.

use alloc::vec::Vec;

/// How an event fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
	Level,
	Edge,
}

/// A `_Lxx` or `_Exx` name, as (number, trigger); `None` for anything else.
pub fn parse(name: &[u8; 4]) -> Option<(u16, Trigger)> {
	if name[0] != b'_' {
		return None;
	}
	let trigger = match name[1] {
		b'L' => Trigger::Level,
		b'E' => Trigger::Edge,
		_ => return None,
	};
	let digit = |byte: u8| -> Option<u16> {
		match byte {
			b'0'..=b'9' => Some((byte - b'0') as u16),
			b'A'..=b'F' => Some((byte - b'A' + 10) as u16),
			_ => None,
		}
	};
	Some((digit(name[2])? << 4 | digit(name[3])?, trigger))
}

/// The name for an event.
pub fn name(number: u16, trigger: Trigger) -> Option<[u8; 4]> {
	if number > 0xFF {
		return None;
	}
	let hex = b"0123456789ABCDEF";
	Some([b'_', if trigger == Trigger::Level { b'L' } else { b'E' }, hex[(number >> 4) as usize], hex[(number & 0xF) as usize]])
}

/// Every event a scope's method names answer, sorted by number - the second of two names for one number is not an
/// event of its own.
pub fn handled(names: &[[u8; 4]]) -> Vec<(u16, Trigger)> {
	let mut out: Vec<(u16, Trigger)> = names.iter().filter_map(parse).collect();
	out.sort_by_key(|(number, _)| *number);
	out.dedup_by_key(|(number, _)| *number);
	out
}

/// What a GPIO-signalled event on `pin` runs, given the controller scope's names: `_Exx` or `_Lxx` for a pin up to 255
/// that one names, else `_EVT` if the scope has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
	Named([u8; 4], Trigger),
	Evt,
}

pub fn gpio_answer(pin: u32, names: &[[u8; 4]], has_evt: bool) -> Option<Answer> {
	if pin <= 0xFF {
		for trigger in [Trigger::Edge, Trigger::Level] {
			if let Some(name) = name(pin as u16, trigger)
				&& names.contains(&name)
			{
				return Some(Answer::Named(name, trigger));
			}
		}
	}
	has_evt.then_some(Answer::Evt)
}
