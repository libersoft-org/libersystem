use super::*;

#[test]
fn a_length_is_the_tag_and_every_other_length_is_refused() {
	assert_eq!(message(&[0u8; PERIOD_BYTES as usize]), Message::Play);
	assert_eq!(message(&[]), Message::EndPlayback);
	assert_eq!(message(&[CMD_CAPTURE]), Message::Capture);
	assert_eq!(message(&[CMD_CAPTURE_STOP]), Message::EndCapture);

	// A SHORT PERIOD IS NOT A SMALL ONE. A server that played it would play part of a period as
	// though it were whole, which is a click rather than an error - and AudioService pads rather
	// than sending one, so this length was never a shape of this wire.
	assert_eq!(message(&[0u8; PERIOD_BYTES as usize - 1]), Message::Unknown);
	assert_eq!(message(&[0u8; PERIOD_BYTES as usize + 1]), Message::Unknown);
	// AND AN UNDEFINED COMMAND BYTE IS NOT CAPTURE. A server that treated anything one byte long as
	// a capture request would answer a question nobody asked with a buffer of samples.
	assert_eq!(message(&[0]), Message::Unknown);
	assert_eq!(message(&[3]), Message::Unknown);
	assert_eq!(message(&[255]), Message::Unknown);
}

#[test]
fn a_refusal_cannot_be_mistaken_for_samples() {
	// The whole reason a refusal is empty: a period is PERIOD_BYTES, so a consumer tells "here are
	// your samples" from "no" by the length alone.
	assert!(REFUSED.is_empty());
	assert_ne!(REFUSED.len(), PERIOD_BYTES as usize);
	assert_ne!(OK.len(), PERIOD_BYTES as usize);
	assert!(!OK.is_empty(), "and an acknowledgement is not a refusal either");
}

#[test]
fn the_format_is_the_one_both_servers_negotiate() {
	// 512 stereo 16-bit frames.
	assert_eq!(PERIOD_BYTES, 512 * CHANNELS as u32 * (BITS as u32 / 8));
	assert_eq!(RATE_HZ, 48_000);
}
