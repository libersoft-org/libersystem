// THE MESSAGE IS HOSTILE INPUT, and this suite is mostly that.
//
// A driver is a separate process that may be wrong or malicious, so every length, count, name and
// handle count is bounded and validated before use. What each test asserts is one refusal, named -
// because "it is validated" is not a claim until the refusal has been watched to happen.

use super::*;

fn header(opcode: Opcode, generation: u64, payload_len: u32) -> Header {
	Header { version: VERSION, opcode, generation, payload_len }
}

#[test]
fn a_frame_round_trips_through_its_own_encoding() {
	let original = header(Opcode::Resource, 0x1122_3344_5566_7788, U16_PAYLOAD_LEN as u32);
	let bytes = original.encode();
	assert_eq!(bytes.len(), HEADER_LEN, "the header is the width it says it is");
	let mut frame = [0u8; HEADER_LEN + U16_PAYLOAD_LEN];
	frame[..HEADER_LEN].copy_from_slice(&bytes);
	encode_u16(ResourceKind::Irq as u16, &mut frame[HEADER_LEN..]);
	let decoded = Header::decode(&frame).expect("a frame this build wrote is a frame it can read");
	assert_eq!(decoded, original);
	assert_eq!(decode_resource(decoded.payload(&frame)), Ok(ResourceKind::Irq));
}

#[test]
fn the_generation_survives_the_wire_at_full_width() {
	// It is a `u64` and every bit of it matters: it is P02M0098's claim generation, and a frame
	// stamped with one that is no longer current is dropped. A narrowing anywhere on this path would
	// make two different bindings compare equal.
	let original = header(Opcode::Ready, u64::MAX, 0);
	let bytes = original.encode();
	assert_eq!(Header::decode(&bytes).map(|h| h.generation), Ok(u64::MAX));
}

#[test]
fn a_buffer_shorter_than_a_header_is_not_a_frame() {
	let bytes = header(Opcode::Ready, 1, 0).encode();
	for short in 0..HEADER_LEN {
		assert_eq!(Header::decode(&bytes[..short]), Err(FrameError::TooShort), "{short} bytes is not a header");
	}
}

#[test]
fn a_frame_without_the_magic_is_refused_before_anything_else_is_read() {
	let mut bytes = header(Opcode::Ready, 1, 0).encode();
	bytes[0] ^= 0xff;
	assert_eq!(Header::decode(&bytes), Err(FrameError::NotAFrame));
}

#[test]
fn a_version_this_build_does_not_implement_is_refused_and_named() {
	// NAMED, because the caller has to be able to say what it refused. A driver built against a
	// different version of the protocol is refused before it is given a device, and "refused" with
	// no number in it is a boot nobody can diagnose.
	let mut bytes = header(Opcode::Ready, 1, 0).encode();
	bytes[4..6].copy_from_slice(&(VERSION + 1).to_le_bytes());
	assert_eq!(Header::decode(&bytes), Err(FrameError::Version(VERSION + 1)));
}

#[test]
fn an_unknown_opcode_is_refused_rather_than_accepted_as_some_message_arriving() {
	// Which is the whole of what happens today: `launch_one` treats any message as success.
	let mut bytes = header(Opcode::Ready, 1, 0).encode();
	// 1 through 10 are allocated; 11 is the next one that is not.
	for raw in [0u16, 11, 12, 0xffff] {
		bytes[6..8].copy_from_slice(&raw.to_le_bytes());
		assert_eq!(Header::decode(&bytes), Err(FrameError::UnknownOpcode(raw)), "opcode {raw}");
	}
}

#[test]
fn a_declared_length_past_the_maximum_is_refused_before_the_payload_is_read() {
	// BEFORE, and that is the property: the refusal must not depend on the buffer actually holding
	// the bytes the header claims, because a sender that lies about a length is exactly the case
	// this is for.
	let mut bytes = header(Opcode::Bind, 1, 0).encode();
	bytes[16..20].copy_from_slice(&(MAX_PAYLOAD as u32 + 1).to_le_bytes());
	assert_eq!(Header::decode(&bytes), Err(FrameError::PayloadTooLong(MAX_PAYLOAD as u32 + 1)));
	bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
	assert_eq!(Header::decode(&bytes), Err(FrameError::PayloadTooLong(u32::MAX)));
}

#[test]
fn a_length_the_buffer_does_not_hold_is_refused() {
	// Inside the maximum and still a lie: the header says there are bytes that are not there.
	let mut bytes = [0u8; HEADER_LEN];
	bytes.copy_from_slice(&header(Opcode::Resource, 1, U16_PAYLOAD_LEN as u32).encode());
	assert_eq!(Header::decode(&bytes), Err(FrameError::TooShort), "the payload it declares is not in the buffer");
}

#[test]
fn each_opcode_declares_exactly_how_many_handles_it_carries() {
	// EXACTLY, NOT AT MOST. The channel does not enforce one handle per frame - this protocol
	// imposes it - and the ordinary receive takes the first and drops the rest. A reader that
	// accepted "at least one" would silently discard whatever a driver attached beyond it.
	assert_eq!(Opcode::Bind.handle_count(), 0);
	assert_eq!(Opcode::Ready.handle_count(), 0);
	assert_eq!(Opcode::Failed.handle_count(), 0);
	assert_eq!(Opcode::Resource.handle_count(), 1);
	assert_eq!(Opcode::Offer.handle_count(), 1);

	let resource = header(Opcode::Resource, 1, U16_PAYLOAD_LEN as u32);
	assert_eq!(resource.check_handles(1), Ok(()));
	assert_eq!(resource.check_handles(0), Err(FrameError::HandleCount { expected: 1, found: 0 }));
	assert_eq!(resource.check_handles(2), Err(FrameError::HandleCount { expected: 1, found: 2 }), "a second handle is a refusal, not a spare");
	let ready = header(Opcode::Ready, 1, 0);
	assert_eq!(ready.check_handles(0), Ok(()));
	assert_eq!(ready.check_handles(1), Err(FrameError::HandleCount { expected: 0, found: 1 }));
}

#[test]
fn a_payload_of_the_wrong_shape_is_refused_for_every_opcode_that_has_one() {
	assert_eq!(decode_ready(&[0u8]), Err(FrameError::PayloadShape), "a READY carrying something is not a READY");
	assert_eq!(decode_ready(&[]), Ok(()));
	assert_eq!(decode_resource(&[1u8]), Err(FrameError::PayloadShape));
	assert_eq!(decode_resource(&[1u8, 0, 0]), Err(FrameError::PayloadShape));
	assert_eq!(decode_failed(&[]), Err(FrameError::PayloadShape));
	assert_eq!(decode_bind(&[0u8; 4]).err(), Some(FrameError::PayloadShape));
	assert_eq!(decode_bind(&[0u8; BIND_LEN + 1]).err(), Some(FrameError::PayloadShape), "one byte too many is the wrong shape too");
}

#[test]
fn a_field_outside_its_closed_set_is_refused_and_the_number_is_reported() {
	for raw in [0u16, 6, 0xffff] {
		assert_eq!(decode_resource(&raw.to_le_bytes()), Err(FrameError::UnknownValue(raw)), "resource kind {raw}");
		assert_eq!(decode_failed(&raw.to_le_bytes()), Err(FrameError::UnknownValue(raw)), "failure code {raw}");
	}
}

#[test]
fn a_bind_carries_the_device_and_the_managers_own_count_of_what_follows() {
	// The count is a promise the manager makes about what it is ABOUT TO SEND, which is the only
	// kind it can keep: the registry entry has no resource list to read one from. Without it a
	// driver either waits forever for a resource it will never be sent, or starts before one it
	// needs has arrived.
	let mut info = abi::DeviceInfo::default();
	info.device_type = 2;
	info.bar_len = 0x4000;
	info.bus = 0;
	info.dev = 3;
	info.func = 0;
	let mut payload = [0u8; BIND_LEN];
	assert_eq!(encode_bind(&info, 3, &mut payload), BIND_LEN);
	let (decoded, count) = decode_bind(&payload).expect("a bind this build wrote");
	assert_eq!(decoded.device_type, 2);
	assert_eq!(decoded.bar_len, 0x4000);
	assert_eq!(decoded.dev, 3);
	assert_eq!(count, 3);
}

#[test]
fn the_declared_maximum_is_the_largest_payload_any_opcode_defines() {
	// If it were smaller than `BIND`'s, a legal frame would be refused as oversized; if it were
	// larger, the bound would not be the bound it claims to be.
	assert_eq!(MAX_PAYLOAD, BIND_LEN, "the maximum is BIND's, which is the largest");
	assert!(U16_PAYLOAD_LEN <= MAX_PAYLOAD);
	let bind = header(Opcode::Bind, 1, BIND_LEN as u32);
	let mut frame = [0u8; HEADER_LEN + BIND_LEN];
	frame[..HEADER_LEN].copy_from_slice(&bind.encode());
	assert!(Header::decode(&frame).is_ok(), "a full-size BIND is inside the bound");
}

#[test]
fn retryability_is_read_off_the_code_rather_than_decided_at_the_call_site() {
	// A set given as three examples in prose is a set every implementer closes differently, so the
	// whole of it is here and every member is asserted - including the ones that are NOT retryable,
	// which is the half a table written from memory tends to lose.
	assert!(DriverFailureCode::DeviceNotResponding.retryable(), "the part may yet come up");
	assert!(DriverFailureCode::OutOfMemory.retryable(), "a transient shortage");
	assert!(!DriverFailureCode::ResourceUnusable.retryable(), "a second attempt hands it the same thing");
	assert!(!DriverFailureCode::UnsupportedDevice.retryable(), "it read the device and will not drive it");
	assert!(!DriverFailureCode::InternalError.retryable(), "nothing says a second try differs");
}

#[test]
fn ready_and_failed_are_the_terminal_frames_and_nothing_else_is() {
	assert!(Opcode::Ready.is_terminal());
	assert!(Opcode::Failed.is_terminal());
	assert!(!Opcode::Bind.is_terminal());
	assert!(!Opcode::Resource.is_terminal());
	assert!(!Opcode::Offer.is_terminal());
}

#[test]
fn an_offer_names_a_kind_and_the_publisher_s_own_token() {
	// The kind says what the provider is; the token says WHICH of this driver's publications it is.
	// A driver publishing two providers of one kind has no other way to say later which of them is
	// going away, and it cannot name the identity the manager minted because it never sees one.
	let mut payload = [0u8; OFFER_PAYLOAD_LEN];
	assert_eq!(encode_offer(provider::BLOCK, 3, &mut payload), OFFER_PAYLOAD_LEN);
	assert_eq!(decode_offer(&payload), Ok((provider::BLOCK, 3)));

	// Two publications of ONE kind are told apart by the token and by nothing else, which is the
	// case the token exists for.
	let mut first = [0u8; OFFER_PAYLOAD_LEN];
	let mut second = [0u8; OFFER_PAYLOAD_LEN];
	encode_offer(provider::BLOCK, 0, &mut first);
	encode_offer(provider::BLOCK, 1, &mut second);
	assert_ne!(first, second);
	assert_eq!(decode_offer(&first).map(|(kind, _)| kind), decode_offer(&second).map(|(kind, _)| kind));
}

#[test]
fn a_withdrawal_names_a_token_carries_no_capability_and_does_not_end_the_handshake() {
	// A withdrawal retires ONE publication. The driver stays bound and its other providers stay
	// published, so this is not terminal - and it moves no capability, because the capability it is
	// about is one the manager already holds.
	assert_eq!(Opcode::from_u16(6), Some(Opcode::Withdraw));
	assert_eq!(Opcode::Withdraw.handle_count(), 0);
	assert!(!Opcode::Withdraw.is_terminal());
	let mut payload = [0u8; U16_PAYLOAD_LEN];
	encode_u16(7, &mut payload);
	assert_eq!(decode_withdraw(&payload), Ok(7));
}

#[test]
fn an_offer_payload_of_the_old_length_is_refused_rather_than_read_short() {
	// The offer carried two bytes before the token joined it. A decoder that accepted the shorter
	// form would read a token of whatever followed - so the length is exact, not a minimum.
	assert_eq!(decode_offer(&[1, 0]), Err(FrameError::PayloadShape));
	assert_eq!(decode_offer(&[1, 0, 0, 0, 0]), Err(FrameError::PayloadShape));
}

// ------------------------------------------------------- the heartbeat
//
// WHAT `rt::heartbeat` GETS WRONG, refused here one case at a time. That one returns true for ANY
// `Polled::Message`, so a driver emitting unrelated traffic reads as responsive and a busy driver
// and a wedged one look the same. A `PING`/`PONG` that was only ADDED, and never proved to refuse
// the three things the old one accepted, would leave a wedged driver looking healthy for exactly
// the same reason it does now.

#[test]
fn a_ping_carries_a_sequence_and_nothing_else() {
	// NOT THE GENERATION: it is in the frame HEADER, on every frame in both directions, so a second
	// copy in the body is a second thing that can disagree with the first.
	assert_eq!(Opcode::from_u16(7), Some(Opcode::Ping));
	assert_eq!(Opcode::from_u16(8), Some(Opcode::Pong));
	assert_eq!(Opcode::Ping.handle_count(), 0);
	assert_eq!(Opcode::Pong.handle_count(), 0);
	assert!(!Opcode::Ping.is_terminal(), "a ping does not end the handshake; it happens long after it");
	assert!(!Opcode::Pong.is_terminal());
	let mut payload = [0u8; SEQUENCE_PAYLOAD_LEN];
	assert_eq!(encode_sequence(0x0102_0304, &mut payload), SEQUENCE_PAYLOAD_LEN);
	assert_eq!(decode_sequence(&payload), Ok(0x0102_0304));
	// The length is exact, not a minimum: a short payload read as a sequence would answer with
	// whatever followed it.
	assert_eq!(decode_sequence(&[1, 0]), Err(FrameError::PayloadShape));
	assert_eq!(decode_sequence(&[1, 0, 0, 0, 0]), Err(FrameError::PayloadShape));
}

#[test]
fn a_frame_with_the_wrong_opcode_is_not_a_pong() {
	// The first of the three `rt::heartbeat` accepts. Every other opcode this protocol has is a
	// legitimate frame that is NOT an answer to "are you still answering", and the difference is a
	// comparison rather than "something arrived".
	for other in [Opcode::Bind, Opcode::Resource, Opcode::Offer, Opcode::Ready, Opcode::Failed, Opcode::Withdraw, Opcode::Ping] {
		assert_ne!(other, Opcode::Pong, "{other:?} is not a pong");
	}
}

#[test]
fn a_pong_stamped_with_another_generation_is_another_bindings_pong() {
	// The second. A driver being replaced cannot answer for its replacement: the generation is on
	// every frame's header, so the two are told apart by arithmetic rather than by timing.
	let mut payload = [0u8; SEQUENCE_PAYLOAD_LEN];
	encode_sequence(4, &mut payload);
	let old = Header { version: VERSION, opcode: Opcode::Pong, generation: 1, payload_len: payload.len() as u32 };
	let new = Header { version: VERSION, opcode: Opcode::Pong, generation: 2, payload_len: payload.len() as u32 };
	assert_ne!(old.generation, new.generation);
	// Both decode; what separates them is which binding is being asked about, and nothing in the
	// body can say it - which is the whole reason the generation is in the header.
	let mut bytes = old.encode().to_vec();
	bytes.extend_from_slice(&payload);
	assert_eq!(Header::decode(&bytes).map(|header| header.generation), Ok(1));
}

#[test]
fn an_out_of_order_sequence_is_a_different_number_and_that_is_the_whole_check() {
	// The third, and the one `rt::heartbeat` cannot express at all: it has no number to compare.
	// A duplicate, an answer from an earlier round, or one invented are all "not the sequence that
	// was asked", which is one comparison rather than three special cases.
	let mut asked = [0u8; SEQUENCE_PAYLOAD_LEN];
	let mut answered = [0u8; SEQUENCE_PAYLOAD_LEN];
	encode_sequence(7, &mut asked);
	for stale in [6u32, 8, 0, u32::MAX] {
		encode_sequence(stale, &mut answered);
		assert_ne!(decode_sequence(&asked), decode_sequence(&answered), "{stale} is not 7");
	}
	encode_sequence(7, &mut answered);
	assert_eq!(decode_sequence(&asked), decode_sequence(&answered));
}

#[test]
fn the_cadence_comes_out_of_the_deadline_and_is_never_zero() {
	// A period of zero is either a busy loop or - `wait_any` reading 0 as no timeout - no bound at
	// all. Both are the failures this arithmetic exists to prevent, reached BY the arithmetic.
	assert_eq!(heartbeat_period(1), 1, "half of 1 rounds up to 1, never to 0");
	assert_eq!(heartbeat_period(2), 1);
	assert_eq!(heartbeat_period(3), 2, "rounded UP, so a longer deadline is pinged less often");
	assert_eq!(heartbeat_period(100), 50);
	// Monotonic in the deadline: an entry asking for longer is never pinged MORE often.
	let mut previous = 0;
	for deadline in 1..=MAX_HEARTBEAT_DEADLINE {
		let period = heartbeat_period(deadline);
		assert!(period >= previous, "period must not fall as the deadline grows");
		assert!(period > 0);
		assert!(period <= deadline, "a driver always gets one whole period inside its deadline");
		previous = period;
	}
}
