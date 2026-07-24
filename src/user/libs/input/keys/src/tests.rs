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
