use super::{Credits, Kind, Limits, Refusal, SPEC_MAX_ACL, SPEC_MAX_COMMAND, SPEC_MAX_EVENT, SPEC_MAX_ISO, Session, check_inbound, check_outbound};

// A provider that carries everything the specification allows, ISO included.
fn full() -> Limits {
	Limits::of(true, SPEC_MAX_COMMAND, SPEC_MAX_EVENT, SPEC_MAX_ACL, SPEC_MAX_ISO)
}

// What nearly every controller here actually is: no ISO, and its own smaller ACL buffer.
fn ordinary() -> Limits {
	Limits::of(false, SPEC_MAX_COMMAND, SPEC_MAX_EVENT, 251 + 4, 0)
}

#[test]
// A HOST SENDING AN EVENT HAS CONFUSED ITS DIRECTIONS, which is a different mistake from being
// early and is refused differently - a caller that saw one refusal for both would retry the one
// that can never succeed.
fn a_kind_that_travels_the_other_way_is_refused_as_a_direction_and_not_as_a_bound() {
	assert_eq!(check_outbound(&full(), Kind::Event as u16, 8), Err(Refusal::WrongDirection(Kind::Event)));
	assert_eq!(check_inbound(&full(), Kind::Command as u16, 8), Err(Refusal::WrongDirection(Kind::Command)));
	// Data travels both ways and is admitted in both.
	assert_eq!(check_outbound(&full(), Kind::Acl as u16, 8), Ok(Kind::Acl));
	assert_eq!(check_inbound(&full(), Kind::Acl as u16, 8), Ok(Kind::Acl));
	assert_eq!(check_outbound(&full(), Kind::Command as u16, 8), Ok(Kind::Command));
	assert_eq!(check_inbound(&full(), Kind::Event as u16, 8), Ok(Kind::Event));
	// A number this vocabulary does not have is neither a direction nor a length.
	assert_eq!(check_outbound(&full(), 0, 8), Err(Refusal::UnknownKind(0)));
	assert_eq!(check_outbound(&full(), 5, 8), Err(Refusal::UnknownKind(5)));
	assert_eq!(check_inbound(&full(), 9999, 8), Err(Refusal::UnknownKind(9999)));
}

#[test]
// THE BOUND THAT APPLIES IS THE SMALLER OF THE TWO. A controller reports its own buffer size and a
// host bound by the specification's ceiling alone hands it a packet it answers with a hardware error
// rather than with data.
fn the_ceiling_is_the_smaller_of_what_the_provider_advertised_and_what_the_format_allows() {
	let small = ordinary();
	assert_eq!(small.ceiling(Kind::Acl), 255, "the controller's own buffer, not the format's 1028");
	assert_eq!(check_outbound(&small, Kind::Acl as u16, 255), Ok(Kind::Acl), "the ceiling itself is inside it");
	assert_eq!(check_outbound(&small, Kind::Acl as u16, 256), Err(Refusal::TooLong { len: 256, bound: 255 }));
	// AND A PROVIDER ADVERTISING MORE THAN THE FORMAT IS NOT BELIEVED: a number larger than the
	// format can express came from somewhere other than the controller.
	let liar = Limits::of(true, u32::MAX, u32::MAX, u32::MAX, u32::MAX);
	assert_eq!(liar.ceiling(Kind::Command), SPEC_MAX_COMMAND);
	assert_eq!(liar.ceiling(Kind::Event), SPEC_MAX_EVENT);
	assert_eq!(liar.ceiling(Kind::Acl), SPEC_MAX_ACL);
	assert_eq!(liar.ceiling(Kind::Iso), SPEC_MAX_ISO);
	assert_eq!(check_outbound(&liar, Kind::Acl as u16, SPEC_MAX_ACL + 1), Err(Refusal::TooLong { len: SPEC_MAX_ACL + 1, bound: SPEC_MAX_ACL }));
	// An empty packet is not a packet: every kind has a header.
	assert_eq!(check_outbound(&full(), Kind::Command as u16, 0), Err(Refusal::Empty));
	assert_eq!(check_inbound(&full(), Kind::Event as u16, 0), Err(Refusal::Empty));
}

#[test]
// ISO IS RESERVED AND NEGOTIATED, and a provider that does not carry it refuses the kind rather than
// the length. The two are different answers to a consumer: one means never, the other means smaller.
fn a_kind_the_provider_does_not_carry_is_unsupported_rather_than_too_long() {
	let no_iso = ordinary();
	assert_eq!(check_outbound(&no_iso, Kind::Iso as u16, 8), Err(Refusal::Unsupported(Kind::Iso)));
	assert_eq!(check_inbound(&no_iso, Kind::Iso as u16, 8), Err(Refusal::Unsupported(Kind::Iso)));
	assert!(full().carries(Kind::Iso));
	assert_eq!(check_outbound(&full(), Kind::Iso as u16, 4100), Ok(Kind::Iso));
	// A ceiling of zero is a kind with no room at all, which is the same answer as not carrying it.
	let zero = Limits::of(true, SPEC_MAX_COMMAND, SPEC_MAX_EVENT, SPEC_MAX_ACL, 0);
	assert_eq!(check_outbound(&zero, Kind::Iso as u16, 1), Err(Refusal::Unsupported(Kind::Iso)));
}

#[test]
// A TRANSPORT SEND IS NOT A COMMAND COMPLETION. This is the whole reason the credit model exists: a
// host that treated the enqueue as the answer would issue its next command against credits the
// controller has not returned, and the controller drops it - which from the host looks like a
// command that was answered and then forgotten.
fn draining_the_queue_is_not_the_controller_answering() {
	let mut credits = Credits::new(1, 4, 8);
	assert_eq!(credits.take(Kind::Command), Ok(()));
	assert_eq!(credits.commands_in_flight(), 1);
	assert_eq!(credits.take(Kind::Command), Err(Refusal::NoCommandCredit), "one outstanding is what a controller reports");
	// The wire drained. The controller has still answered nothing.
	credits.drained();
	assert_eq!(credits.queued(), 0);
	assert_eq!(credits.commands_in_flight(), 1, "the credit the controller holds is still held");
	assert_eq!(credits.take(Kind::Command), Err(Refusal::NoCommandCredit));
	// Now it answers.
	credits.commands_completed(1);
	assert_eq!(credits.commands_in_flight(), 0);
	assert_eq!(credits.take(Kind::Command), Ok(()));
}

#[test]
// THREE COUNTERS AND THEY ARE NOT THE SAME BOUND, so a host stalled on one learns which. Folding
// them together is how a transport comes to refuse packets it has room for.
fn the_queue_the_command_window_and_the_data_buffers_are_three_separate_refusals() {
	let mut credits = Credits::new(1, 2, 3);
	assert_eq!(credits.take(Kind::Acl), Ok(()));
	assert_eq!(credits.take(Kind::Acl), Ok(()));
	assert_eq!(credits.take(Kind::Acl), Err(Refusal::NoAclCredit), "the controller's buffers, not the queue");
	assert_eq!(credits.queued(), 2, "and nothing was charged by the refusal");
	assert_eq!(credits.acl_in_flight(), 2);
	credits.acl_completed(2);
	assert_eq!(credits.acl_in_flight(), 0);
	// Now the transport's own queue is what stops it: three deep, and nothing has drained.
	let mut queue = Credits::new(8, 8, 2);
	assert_eq!(queue.take(Kind::Acl), Ok(()));
	assert_eq!(queue.take(Kind::Command), Ok(()));
	assert_eq!(queue.take(Kind::Acl), Err(Refusal::QueueFull));
	assert_eq!(queue.acl_in_flight(), 1, "a refused send charged neither counter");
	assert_eq!(queue.commands_in_flight(), 1);
	queue.drained();
	assert_eq!(queue.take(Kind::Acl), Ok(()));
}

#[test]
// A CONTROLLER THAT RETURNS MORE THAN IT OWES IS NOT BELIEVED INTO A LARGER WINDOW. The completion
// count is the controller's own number, and a host that added it to a free counter would let a
// confused controller widen the host's sense of how many commands it may have in flight.
fn a_completion_for_more_than_is_outstanding_does_not_widen_the_window() {
	let mut credits = Credits::new(1, 2, 8);
	assert_eq!(credits.take(Kind::Command), Ok(()));
	credits.commands_completed(u32::MAX);
	assert_eq!(credits.commands_in_flight(), 0);
	assert_eq!(credits.take(Kind::Command), Ok(()));
	assert_eq!(credits.take(Kind::Command), Err(Refusal::NoCommandCredit), "still one, which is what was advertised");
	let mut data = Credits::new(1, 2, 8);
	assert_eq!(data.take(Kind::Acl), Ok(()));
	data.acl_completed(1000);
	assert_eq!(data.acl_in_flight(), 0);
	assert_eq!(data.take(Kind::Acl), Ok(()));
	assert_eq!(data.take(Kind::Acl), Ok(()));
	assert_eq!(data.take(Kind::Acl), Err(Refusal::NoAclCredit), "still two");
}

#[test]
// A RESET ENDS THE SESSION AND THE CONTROLLER OWES NOTHING FROM IT. Credits held across a reset are
// credits nothing will ever return, which is a transport that works until the first reset and then
// stops sending for ever.
fn a_reset_gives_every_credit_back_and_a_late_packet_belongs_to_the_session_that_is_gone() {
	let mut credits = Credits::new(1, 2, 8);
	assert_eq!(credits.take(Kind::Command), Ok(()));
	assert_eq!(credits.take(Kind::Acl), Ok(()));
	credits.reset();
	assert_eq!((credits.commands_in_flight(), credits.acl_in_flight(), credits.queued()), (0, 0, 0));
	assert_eq!(credits.take(Kind::Command), Ok(()));

	let mut session = Session::new(0);
	assert!(session.admits(0));
	let second = session.advance();
	assert_eq!(second, 1);
	assert!(session.admits(1));
	assert!(!session.admits(0), "a packet the controller had already queued belongs to the session that ended");
	// WRAPPING, because the epoch is an identity and not a count. At the top it must still produce a
	// value distinguishable from its neighbours rather than sticking to one that admits everything
	// the previous session sent.
	let mut old = Session::new(u32::MAX);
	assert_eq!(old.advance(), 0);
	assert!(old.admits(0) && !old.admits(u32::MAX));
}
