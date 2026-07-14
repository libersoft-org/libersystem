#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use proto::system::KeyEvent;

pub mod usage {
	pub const A: u16 = 0x04;
	pub const Q: u16 = 0x14;
	pub const ESCAPE: u16 = 0x29;
	pub const RIGHT: u16 = 0x4f;
	pub const LEFT: u16 = 0x50;
	pub const DOWN: u16 = 0x51;
	pub const UP: u16 = 0x52;
	pub const LEFT_CTRL: u16 = 0xe0;
	pub const LEFT_SHIFT: u16 = 0xe1;
	pub const LEFT_ALT: u16 = 0xe2;
	pub const LEFT_GUI: u16 = 0xe3;
	pub const RIGHT_CTRL: u16 = 0xe4;
	pub const RIGHT_SHIFT: u16 = 0xe5;
	pub const RIGHT_ALT: u16 = 0xe6;
	pub const RIGHT_GUI: u16 = 0xe7;

	pub const fn is_modifier(code: u16) -> bool {
		code >= LEFT_CTRL && code <= RIGHT_GUI
	}

	pub const fn is_ctrl(code: u16) -> bool {
		code == LEFT_CTRL || code == RIGHT_CTRL
	}

	pub const fn is_alt(code: u16) -> bool {
		code == LEFT_ALT || code == RIGHT_ALT
	}
}

#[derive(Default)]
pub struct KeyState {
	held: Vec<u16>,
}

impl KeyState {
	pub const fn new() -> KeyState {
		KeyState { held: Vec::new() }
	}

	pub fn record_raw(&mut self, raw: &[u8]) -> Option<KeyEvent> {
		if raw.len() != 3 || raw[2] > 1 {
			return None;
		}
		self.record(u16::from_le_bytes([raw[0], raw[1]]), raw[2] != 0)
	}

	pub fn record(&mut self, code: u16, pressed: bool) -> Option<KeyEvent> {
		let held = self.held.iter().position(|current| *current == code);
		if pressed {
			if held.is_some() {
				return None;
			}
			self.held.push(code);
		} else if let Some(index) = held {
			self.held.swap_remove(index);
		} else {
			return None;
		}
		Some(KeyEvent { code, pressed })
	}

	pub fn is_held(&self, code: u16) -> bool {
		self.held.contains(&code)
	}

	pub fn emergency_chord(&self, event: &KeyEvent) -> bool {
		event.pressed && event.code == usage::ESCAPE && self.held.iter().any(|code| usage::is_ctrl(*code)) && self.held.iter().any(|code| usage::is_alt(*code))
	}

	pub fn synthetic_releases(&self) -> Vec<KeyEvent> {
		self.held.iter().map(|code| KeyEvent { code: *code, pressed: false }).collect()
	}
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn raw_edges_suppress_duplicates_and_impossible_releases() {
		let mut state = KeyState::new();
		assert_eq!(state.record_raw(&[usage::A as u8, 0, 1]), Some(KeyEvent { code: usage::A, pressed: true }));
		assert_eq!(state.record_raw(&[usage::A as u8, 0, 1]), None);
		assert_eq!(state.record_raw(&[usage::A as u8, 0, 0]), Some(KeyEvent { code: usage::A, pressed: false }));
		assert_eq!(state.record_raw(&[usage::A as u8, 0, 0]), None);
		assert_eq!(state.record_raw(&[0, 0]), None);
	}

	#[test]
	fn emergency_chord_requires_ctrl_alt_and_escape_down() {
		let mut state = KeyState::new();
		state.record(usage::LEFT_CTRL, true);
		state.record(usage::RIGHT_ALT, true);
		let escape = state.record(usage::ESCAPE, true).unwrap();
		assert!(state.emergency_chord(&escape));
		assert!(!state.emergency_chord(&KeyEvent { code: usage::ESCAPE, pressed: false }));
	}

	#[test]
	fn synthetic_releases_do_not_change_physical_state() {
		let mut state = KeyState::new();
		state.record(usage::LEFT_SHIFT, true);
		state.record(usage::A, true);
		let releases = state.synthetic_releases();
		assert_eq!(releases.len(), 2);
		assert!(releases.iter().all(|event| !event.pressed));
		assert!(state.is_held(usage::A));
	}
}
