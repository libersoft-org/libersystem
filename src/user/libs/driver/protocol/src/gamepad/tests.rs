use super::*;

/// The harness's gamepad: X, Y, Z and Rz over 0..255, sixteen buttons, one hat.
fn harness_shape(label: &[u8]) -> Shape {
	let axis = |usage: u32| Axis { usage: 0x0001_0000 | usage, minimum: 0, maximum: 255 };
	Shape::new(label, 16, 1, &[axis(0x30), axis(0x31), axis(0x32), axis(0x35)]).expect("a gamepad")
}

/// A consumer connection that takes frames only while it is `open`, and keeps what it took - in fixed
/// arrays, because this crate is `no_std` and so are its tests.
struct Wire {
	open: bool,
	taken: [[u8; MAX_FRAME]; 32],
	lengths: [usize; 32],
	count: usize,
}

impl Wire {
	fn new(open: bool) -> Wire {
		Wire { open, taken: [[0; MAX_FRAME]; 32], lengths: [0; 32], count: 0 }
	}

	fn frame(&self, index: usize) -> Frame {
		decode(&self.taken[index][..self.lengths[index]]).expect("every frame a publisher sends decodes")
	}
}

impl Sender for Wire {
	fn send(&mut self, frame: &[u8]) -> bool {
		if !self.open || self.count == self.taken.len() {
			return false;
		}
		self.taken[self.count][..frame.len()].copy_from_slice(frame);
		self.lengths[self.count] = frame.len();
		self.count += 1;
		true
	}
}

fn state(buttons: u32, hat: u8, axes: [i32; 4]) -> State {
	let mut values = [0i32; MAX_AXES];
	values[..4].copy_from_slice(&axes);
	State { buttons, hats: [hat, CENTRED], axes: values }
}

#[test]
fn every_frame_round_trips() {
	let label = b"usb 1234:5678 port 3 if 0";
	let shape = harness_shape(label);
	let mut out = [0u8; MAX_FRAME];
	let length = encode_arrival(7, &shape, &mut out);
	assert_eq!(length, 9 + label.len() + 4 * 12);
	assert_eq!(decode(&out[..length]), Some(Frame::Arrival { handle: 7, shape }));

	let pressed = state(0x8001, 2, [0, 128, 128, 255]);
	let length = encode_state(7, &shape, &pressed, &mut out);
	assert_eq!(length, 11 + 4 * 4);
	let Some(Frame::State(frame)) = decode(&out[..length]) else { panic!("a STATE decodes as one") };
	assert_eq!(frame.handle, 7);
	assert_eq!(shape.state(&frame), Some(pressed), "and matches its gamepad as the state it was");

	let length = encode_departure(7, &mut out);
	assert_eq!(decode(&out[..length]), Some(Frame::Departure { handle: 7 }));

	// THE WIDEST SHAPE the wire allows fits the frame bound exactly.
	let axes = [Axis { usage: 0x0002_00BA, minimum: i32::MIN, maximum: i32::MAX }; MAX_AXES];
	let widest = Shape::new(&[b'x'; MAX_LABEL], MAX_BUTTONS, MAX_HATS, &axes).expect("the widest gamepad");
	let length = encode_arrival(u32::MAX, &widest, &mut out);
	assert_eq!(length, MAX_FRAME);
	assert_eq!(decode(&out[..length]), Some(Frame::Arrival { handle: u32::MAX, shape: widest }));
}

#[test]
fn every_malformed_frame_is_refused() {
	let shape = harness_shape(b"pad");
	let mut out = [0u8; MAX_FRAME];
	let arrival = encode_arrival(1, &shape, &mut out);
	let good = out;

	assert_eq!(decode(&[]), None, "nothing is not a frame");
	assert_eq!(decode(&[9, 1, 0, 0, 0]), None, "an unknown tag");
	assert_eq!(decode(&good[..arrival - 1]), None, "an ARRIVAL one byte short of its axes");
	let mut long = [0u8; MAX_FRAME + 1];
	long[..arrival].copy_from_slice(&good[..arrival]);
	assert_eq!(decode(&long[..arrival + 1]), None, "and one byte long");
	let mut bad = good;
	bad[7] = 9;
	assert_eq!(decode(&bad[..arrival]), None, "nine axes");
	let mut bad = good;
	bad[8] = 33;
	assert_eq!(decode(&bad[..arrival]), None, "a label past thirty-two bytes");
	let mut bad = good;
	bad[9] = 0x07;
	assert_eq!(decode(&bad[..arrival]), None, "a label that is not printable ASCII");
	let mut bad = good;
	bad[5] = 33;
	assert_eq!(decode(&bad[..arrival]), None, "thirty-three buttons");
	let mut bad = good;
	bad[6] = 3;
	assert_eq!(decode(&bad[..arrival]), None, "three hats");

	let length = encode_state(1, &shape, &shape.initial(), &mut out);
	assert_eq!(decode(&out[..length - 1]), None, "a STATE whose axes are not whole values");
	assert_eq!(decode(&out[..10]), None, "a STATE short of its hats");
	let mut bad = out;
	bad[9] = 9;
	assert_eq!(decode(&bad[..length]), None, "a hat above centred");
	let mut nine = [0u8; 11 + 9 * 4];
	nine[0] = STATE;
	assert_eq!(decode(&nine), None, "a STATE with nine axes");

	assert_eq!(decode(&[DEPARTURE, 1, 0, 0]), None, "a DEPARTURE short of its handle");
	assert_eq!(decode(&[DEPARTURE, 1, 0, 0, 0, 0]), None, "and one byte long");
}

#[test]
fn a_state_is_refused_by_a_gamepad_it_does_not_fit() {
	let shape = harness_shape(b"pad");
	let frame = |buttons: u32, hats: [u8; 2], axes: u8| StateFrame { handle: 1, buttons, hats, axes, values: [0; MAX_AXES] };
	assert!(shape.state(&frame(0x8000, [0, CENTRED], 4)).is_some(), "button sixteen, the hat north and four axes fit");
	assert_eq!(shape.state(&frame(0x1_0000, [CENTRED, CENTRED], 4)), None, "button seventeen is not this gamepad's");
	assert_eq!(shape.state(&frame(0, [CENTRED, 0], 4)), None, "nor is a second hat pointing anywhere");
	assert_eq!(shape.state(&frame(0, [CENTRED, CENTRED], 3)), None, "nor three axes");
	assert_eq!(shape.state(&frame(0, [CENTRED, CENTRED], 5)), None, "nor five");
}

#[test]
fn the_state_before_the_first_report_is_at_rest_and_centred() {
	let shape = harness_shape(b"pad");
	let initial = shape.initial();
	assert_eq!(initial.buttons, 0);
	assert_eq!(initial.hats, [CENTRED, CENTRED], "a hat at rest is centred and never north");
	assert_eq!(&initial.axes[..4], &[127, 127, 127, 127], "the midpoint of 0..255, rounded toward the minimum");
	assert_eq!(Axis { usage: 0, minimum: -1, maximum: 0 }.midpoint(), -1);
	assert_eq!(Axis { usage: 0, minimum: -32768, maximum: 32767 }.midpoint(), -1);
	assert_eq!(Axis { usage: 0, minimum: i32::MIN, maximum: i32::MAX }.midpoint(), -1, "computed in 64 bits, so the widest range does not overflow");
	assert_eq!(Axis { usage: 0, minimum: 10, maximum: 10 }.midpoint(), 10);
}

#[test]
fn a_refused_state_goes_later_as_the_current_state() {
	let mut publisher = Publisher::<4>::new();
	let mut wire = Wire::new(true);
	publisher.connected();
	let pad = publisher.attach(harness_shape(b"pad")).expect("a gamepad");
	assert!(!publisher.flush(&mut wire), "its ARRIVAL goes at once");
	assert_eq!(wire.count, 1);

	// THE CONNECTION IS FULL: the change is owed, not dropped, and a later change folds into it.
	wire.open = false;
	assert!(publisher.report(pad, state(1, 2, [0, 128, 128, 255])));
	assert!(publisher.flush(&mut wire), "the refused STATE is owed");
	assert!(publisher.report(pad, state(0, CENTRED, [255, 128, 128, 255])));
	assert!(publisher.owes());

	wire.open = true;
	assert!(!publisher.flush(&mut wire), "and goes once the connection takes frames");
	assert_eq!(wire.count, 2, "ONE state, not one per change");
	let Frame::State(frame) = wire.frame(1) else { panic!("a STATE") };
	assert_eq!(harness_shape(b"pad").state(&frame), Some(state(0, CENTRED, [255, 128, 128, 255])), "carrying the gamepad's CURRENT state");

	// AN UNCHANGED STATE OWES NOTHING.
	assert!(publisher.report(pad, state(0, CENTRED, [255, 128, 128, 255])));
	assert!(!publisher.owes());
}

#[test]
fn a_refused_arrival_goes_before_the_first_state_and_a_departure_after_the_last() {
	let mut publisher = Publisher::<4>::new();
	let mut wire = Wire::new(false);
	publisher.connected();
	let first = publisher.attach(harness_shape(b"first")).expect("a gamepad");
	assert!(publisher.report(first, state(4, 0, [0, 0, 0, 0])));
	assert!(publisher.flush(&mut wire), "the ARRIVAL and the STATE are owed");

	wire.open = true;
	assert!(!publisher.flush(&mut wire));
	assert!(matches!(wire.frame(0), Frame::Arrival { handle, .. } if handle == first), "the ARRIVAL first");
	assert!(matches!(wire.frame(1), Frame::State(frame) if frame.handle == first), "then its STATE");

	// A DEPARTURE refused, then a state of ANOTHER gamepad: the two leave in the order they became owed.
	let second = publisher.attach(harness_shape(b"second")).expect("a second gamepad");
	assert!(!publisher.flush(&mut wire));
	wire.open = false;
	assert!(publisher.detach(first));
	assert!(publisher.report(second, state(1, CENTRED, [1, 2, 3, 4])));
	assert!(publisher.flush(&mut wire));
	wire.open = true;
	assert!(!publisher.flush(&mut wire));
	assert_eq!(wire.count, 5);
	assert_eq!(wire.frame(3), Frame::Departure { handle: first }, "the DEPARTURE after the first gamepad's last STATE");
	assert!(matches!(wire.frame(4), Frame::State(frame) if frame.handle == second));
	assert_eq!(publisher.len(), 1, "and a departed gamepad leaves the table once its DEPARTURE is sent");
}

#[test]
fn a_departure_clears_the_unsent_mark_and_an_unannounced_gamepad_departs_silently() {
	let mut publisher = Publisher::<4>::new();
	let mut wire = Wire::new(true);
	publisher.connected();
	let pad = publisher.attach(harness_shape(b"pad")).expect("a gamepad");
	assert!(!publisher.flush(&mut wire));
	wire.open = false;
	assert!(publisher.report(pad, state(1, 0, [0, 0, 0, 0])));
	assert!(publisher.detach(pad));
	wire.open = true;
	assert!(!publisher.flush(&mut wire));
	assert_eq!(wire.count, 2, "the DEPARTURE alone - the consumer releases a departed gamepad whole");
	assert_eq!(wire.frame(1), Frame::Departure { handle: pad });

	// ITS ARRIVAL NEVER WENT OUT, SO NEITHER DOES ITS DEPARTURE: the consumer never heard of it.
	wire.open = false;
	let unheard = publisher.attach(harness_shape(b"unheard")).expect("a gamepad");
	assert!(publisher.report(unheard, state(2, 1, [9, 9, 9, 9])));
	assert!(publisher.detach(unheard));
	assert!(!publisher.owes(), "nothing is owed for a gamepad the consumer never heard of");
	wire.open = true;
	assert!(!publisher.flush(&mut wire));
	assert_eq!(wire.count, 2);
	assert!(publisher.is_empty());
}

#[test]
fn every_connection_is_owed_every_gamepad_and_nothing_goes_to_none() {
	let mut publisher = Publisher::<4>::new();
	let mut wire = Wire::new(true);
	// NO CONSUMER: nothing is owed, whatever happens.
	let bound_at_boot = publisher.attach(harness_shape(b"boot")).expect("a gamepad");
	let gone = publisher.attach(harness_shape(b"gone")).expect("a gamepad");
	assert!(publisher.detach(gone));
	assert!(!publisher.owes());
	assert!(!publisher.flush(&mut wire));
	assert_eq!(wire.count, 0, "nothing is sent to nobody");

	// THE FIRST CONNECTION: an ARRIVAL and the state before the first report.
	publisher.connected();
	assert!(!publisher.flush(&mut wire));
	assert_eq!(wire.count, 2);
	assert!(matches!(wire.frame(0), Frame::Arrival { handle, .. } if handle == bound_at_boot));
	let Frame::State(frame) = wire.frame(1) else { panic!("a STATE") };
	assert_eq!(harness_shape(b"boot").state(&frame), Some(harness_shape(b"boot").initial()), "the initial state, for a gamepad with no report");

	// A LATER CONNECTION after the first went away, with a departure the old one never took: the new
	// consumer hears of the live gamepad, at its current state, and never of the departed one.
	let leaving = publisher.attach(harness_shape(b"leaving")).expect("a gamepad");
	assert!(!publisher.flush(&mut wire));
	assert!(publisher.report(bound_at_boot, state(3, 4, [5, 6, 7, 8])));
	wire.open = false;
	assert!(publisher.detach(leaving));
	publisher.disconnected();
	assert!(!publisher.owes());
	publisher.connected();
	let before = wire.count;
	wire.open = true;
	assert!(!publisher.flush(&mut wire));
	assert_eq!(wire.count - before, 2);
	assert!(matches!(wire.frame(before), Frame::Arrival { handle, .. } if handle == bound_at_boot));
	let Frame::State(frame) = wire.frame(before + 1) else { panic!("a STATE") };
	assert_eq!(harness_shape(b"boot").state(&frame), Some(state(3, 4, [5, 6, 7, 8])));
	assert_eq!(publisher.len(), 1);
}

#[test]
fn no_refusal_closes_anything_and_handles_are_never_reused() {
	let mut publisher = Publisher::<2>::new();
	let mut wire = Wire::new(false);
	publisher.connected();
	let pad = publisher.attach(harness_shape(b"pad")).expect("a gamepad");
	for step in 0..100u32 {
		assert!(publisher.report(pad, state(step & 0xFFFF, (step % 9) as u8, [step as i32 % 256, 0, 0, 0])));
		assert!(publisher.flush(&mut wire), "refused and still owed, never given up on");
	}
	wire.open = true;
	assert!(!publisher.flush(&mut wire));
	assert_eq!(wire.count, 2, "the ARRIVAL and one STATE, the last");

	// A REPORT THE SHAPE DOES NOT ADMIT, OR FOR A HANDLE NOT HELD, CHANGES NOTHING.
	assert!(!publisher.report(pad, state(1 << 16, CENTRED, [0, 0, 0, 0])), "button seventeen");
	assert!(!publisher.report(pad, state(0, 9, [0, 0, 0, 0])), "a hat past centred");
	assert!(!publisher.report(pad + 100, state(0, CENTRED, [0, 0, 0, 0])), "a handle nobody holds");
	assert!(!publisher.owes());

	assert!(publisher.detach(pad));
	assert!(!publisher.flush(&mut wire));
	let again = publisher.attach(harness_shape(b"pad")).expect("the same gamepad again");
	assert_ne!(again, pad, "plugged back, it is a new gamepad");
	let other = publisher.attach(harness_shape(b"other")).expect("a second");
	assert_eq!(publisher.attach(harness_shape(b"third")), None, "and the table holds what it was sized for");
	assert_ne!(other, again);
}
