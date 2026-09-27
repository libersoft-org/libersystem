use alloc::vec::Vec;

use super::*;

#[test]
fn an_interrupt_line_is_armed_with_its_buffer_before_its_trigger() {
	let mut lines = Lines::new(8, true);
	let mut steps = Vec::new();
	assert_eq!(lines.take(3, Scope::Interrupt(Trigger::Low), &mut steps), Ok(()));
	assert_eq!(
		steps,
		[
			Step::Send(Request { kind: MSG_SET_DIRECTION, line: 3, value: DIRECTION_IN }),
			Step::QueueEvent(3),
			Step::Send(Request { kind: MSG_SET_IRQ_TYPE, line: 3, value: 0x08 })
		]
	);
	// The request as the device reads it: type, line, value, little-endian.
	assert_eq!(Request { kind: MSG_SET_IRQ_TYPE, line: 3, value: 0x08 }.encode(), [6, 0, 3, 0, 8, 0, 0, 0]);
}

#[test]
fn an_event_is_delivered_once_and_the_line_stays_masked_until_acknowledged() {
	let mut lines = Lines::new(8, true);
	let mut steps = Vec::new();
	lines.take(5, Scope::Interrupt(Trigger::High), &mut steps).expect("the line is free");
	assert_eq!(lines.event(5, EVENT_VALID), Event::Deliver(5));
	// NOTHING MORE until the acknowledgement: the device holds no buffer for the line, and a second completion
	// could only be a stale one.
	assert_eq!(lines.event(5, EVENT_VALID), Event::Nothing, "delivered once");
	assert_eq!(lines.acknowledge(5), Some(Step::QueueEvent(5)), "the acknowledgement gives the buffer back - the device's own unmask");
	assert_eq!(lines.acknowledge(5), None, "and a second acknowledgement has nothing to give");
	// A LEVEL LINE STILL ASSERTED is reported again after the acknowledgement.
	assert_eq!(lines.event(5, EVENT_VALID), Event::Deliver(5));
}

#[test]
fn a_buffer_that_comes_back_invalid_or_for_nobody_delivers_nothing() {
	let mut lines = Lines::new(8, true);
	let mut steps = Vec::new();
	lines.take(1, Scope::Interrupt(Trigger::Both), &mut steps).expect("the line is free");
	assert_eq!(lines.event(1, 0), Event::Nothing, "a buffer completed invalid is a disarm, not an event");
	assert_eq!(lines.event(2, EVENT_VALID), Event::Nothing, "a line nobody holds");
}

#[test]
fn a_level_scope_reads_and_is_delivered_no_event() {
	let mut lines = Lines::new(8, true);
	let mut steps = Vec::new();
	lines.take(4, Scope::Level, &mut steps).expect("the line is free");
	assert_eq!(steps, [Step::Send(Request { kind: MSG_SET_DIRECTION, line: 4, value: DIRECTION_IN })], "input, and no buffer and no trigger");
	assert_eq!(lines.event(4, EVENT_VALID), Event::Nothing);
	assert_eq!(lines.acknowledge(4), None);
	assert!(!lines.is_interrupt(4));
}

#[test]
fn a_line_is_held_by_one_connection_and_given_back_disarmed() {
	let mut lines = Lines::new(8, true);
	let mut steps = Vec::new();
	lines.take(6, Scope::Interrupt(Trigger::Rising), &mut steps).expect("the line is free");
	assert_eq!(lines.take(6, Scope::Level, &mut steps), Err(Refusal::Held), "a second connection for a held line");
	assert_eq!(lines.take(8, Scope::Level, &mut steps), Err(Refusal::NoSuchLine));
	steps.clear();
	lines.give_back(6, &mut steps);
	assert_eq!(steps, [Step::Send(Request { kind: MSG_SET_IRQ_TYPE, line: 6, value: IRQ_NONE }), Step::Send(Request { kind: MSG_SET_DIRECTION, line: 6, value: DIRECTION_NONE })]);
	steps.clear();
	assert_eq!(lines.take(6, Scope::Level, &mut steps), Ok(()), "and free again once given back");
	// A DEVICE WITH NO EVENT QUEUE serves levels only.
	let mut plain = Lines::new(8, false);
	assert_eq!(plain.take(0, Scope::Interrupt(Trigger::High), &mut steps), Err(Refusal::NoEvents));
}

#[test]
fn the_names_are_one_nul_terminated_string_per_line() {
	let names = b"alpha\0\0gamma\0";
	assert_eq!(line_name(names, 0), Some(&b"alpha"[..]));
	assert_eq!(line_name(names, 1), Some(&b""[..]), "a line with no name");
	assert_eq!(line_name(names, 2), Some(&b"gamma"[..]));
	assert_eq!(Trigger::from_u8(0x08), Some(Trigger::Low));
	assert_eq!(Trigger::from_u8(0x05), None);
}
