use super::*;

#[test]
// A PASS-THROUGH: the AVCTP header with AV Remote Control's profile, a CONTROL to the panel, the operation and its
// release bit - and the answer a target gives what it does not do.
fn a_pass_through_round_trips_and_is_answered_not_implemented() {
	let play = Frame::pass_through(3, Operation::Play, true);
	assert_eq!(play.encode(), [0x30, 0x11, 0x0e, 0x00, 0x48, 0x7c, 0x44, 0x00]);
	let read = Frame::decode(&play.encode()).unwrap();
	assert_eq!(read.operation(), Some((Operation::Play, true)));
	let released = Frame::pass_through(3, Operation::Play, false);
	assert_eq!(Frame::decode(&released.encode()).unwrap().operation(), Some((Operation::Play, false)));
	let answer = read.answer(ctype::NOT_IMPLEMENTED, &read.operands);
	let bytes = answer.encode();
	assert_eq!(bytes[0], 0x32, "the same label, as a response");
	assert_eq!(bytes[3], ctype::NOT_IMPLEMENTED);
	assert_eq!(Frame::decode(&[0x30, 0x11, 0x0f, 0, 0x48, 0x7c]), None, "another profile's frame is not this one's");
}

#[test]
// THE ABSOLUTE VOLUME: SetAbsoluteVolume and the volume-changed registration in vendor-dependent frames, and a level
// that survives the trip between 0..100 and 0..127.
fn the_absolute_volume_round_trips() {
	let set = Frame::vendor(1, ctype::CONTROL, pdu::SET_ABSOLUTE_VOLUME, &[to_absolute(50)]);
	let read = Frame::decode(&set.encode()).unwrap();
	assert_eq!(read.pdu(), Some((pdu::SET_ABSOLUTE_VOLUME, &[64u8][..])));
	let register = Frame::vendor(2, ctype::NOTIFY, pdu::REGISTER_NOTIFICATION, &[EVENT_VOLUME_CHANGED, 0, 0, 0, 0]);
	assert_eq!(Frame::decode(&register.encode()).unwrap().pdu().map(|(pdu, params)| (pdu, params[0])), Some((pdu::REGISTER_NOTIFICATION, EVENT_VOLUME_CHANGED)));
	for level in 0..=100u8 {
		assert_eq!(from_absolute(to_absolute(level)), level, "level {level}");
	}
	assert_eq!(to_absolute(100), MAX_VOLUME);
	assert_eq!(from_absolute(0xff), 100, "the reserved top bit is ignored");
}
