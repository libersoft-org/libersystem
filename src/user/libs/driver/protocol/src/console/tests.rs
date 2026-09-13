// EVERY ONE OF THESE WATCHES A REFUSAL, because "the version is checked" is not a claim until the
// check has been seen to refuse something. The four decisions here replaced four conventions that
// no test could reach: an empty message meaning backpressure, a magic tag meaning reconnect, a
// driver's name meaning "this is the consumer", and nothing at all meaning "this is the version".

use super::*;
extern crate alloc;
use alloc::vec::Vec;

#[test]
fn a_version_that_was_not_asked_for_is_refused_rather_than_downgraded() {
	assert_eq!(attach(WIRE_VERSION, WIRE_VERSION, 4096), Attach::Speak { version: WIRE_VERSION, max_frame: 4096 });
	// The refusal is the point: a provider that answered version 1 to a consumer asking for 2 would
	// be agreeing to frame bytes one way and read them another, and nothing downstream could tell.
	assert_eq!(attach(2, WIRE_VERSION, 4096), Attach::Unsupported);
	assert_eq!(attach(0, WIRE_VERSION, 4096), Attach::Unsupported);
}

#[test]
fn a_bound_that_cannot_carry_a_frame_is_not_an_attachment() {
	// Zero would be a provider that accepts an attachment and refuses every write made under it.
	assert_eq!(attach(WIRE_VERSION, WIRE_VERSION, 0), Attach::Unusable);
	// And above what the wire can express is a promise the encoder could not keep: the length
	// prefix is a `u16`.
	assert_eq!(attach(WIRE_VERSION, WIRE_VERSION, MAX_WRITE + 1), Attach::Unusable);
	assert_eq!(attach(WIRE_VERSION, WIRE_VERSION, MAX_WRITE), Attach::Speak { version: WIRE_VERSION, max_frame: MAX_WRITE });
}

fn identity(slot: u32, provider: u32, binding: u64) -> Identity {
	Identity { slot, provider_generation: provider, binding_generation: binding }
}

#[test]
fn a_withdrawal_that_names_another_provider_does_not_detach_this_one() {
	let held = identity(3, 7, 11);
	// The same slot, a later provider: this is the publication that REPLACED the held one, and
	// treating its withdrawal as the held one's is exactly the mistake the generations exist to
	// stop.
	assert_eq!(follow(Some(held), identity(3, 8, 11), false), Follow::Ignore);
	// The same slot and provider generation under a later binding is a different device claim.
	assert_eq!(follow(Some(held), identity(3, 7, 12), false), Follow::Ignore);
	// A different slot entirely is somebody else's.
	assert_eq!(follow(Some(held), identity(4, 7, 11), false), Follow::Ignore);
	// And the one actually held does detach.
	assert_eq!(follow(Some(held), held, false), Follow::Detach);
}

#[test]
fn a_live_publication_attaches_only_when_nothing_is_held() {
	assert_eq!(follow(None, identity(1, 1, 1), true), Follow::Attach);
	// Already attached: a second live provider is not taken instead. It stays in the catalogue for
	// the consumer that wants it, which is what one publication per consumer means.
	assert_eq!(follow(Some(identity(1, 1, 1)), identity(2, 1, 1), true), Follow::Ignore);
	// A withdrawal seen while nothing is held is not a detach of something that is not there.
	assert_eq!(follow(None, identity(1, 1, 1), false), Follow::Ignore);
}

#[test]
fn an_unattached_consumer_cannot_move_a_byte_or_take_a_stream() {
	assert_eq!(admit_write(false, 16, 4096), Admit::NotAttached);
	assert_eq!(grant_stream(false, false), Grant::NotAttached);
	// And once attached, both are available exactly once each in the way they say.
	assert_eq!(admit_write(true, 16, 4096), Admit::Write);
	assert_eq!(grant_stream(true, false), Grant::Mint);
	assert_eq!(grant_stream(true, true), Grant::AlreadyGranted);
}

#[test]
fn a_write_is_refused_by_length_rather_than_cut_to_fit() {
	assert_eq!(admit_write(true, 0, 4096), Admit::BadLength);
	assert_eq!(admit_write(true, 4097, 4096), Admit::BadLength);
	assert_eq!(admit_write(true, 4096, 4096), Admit::Write);
}

#[test]
fn a_frame_longer_than_the_bound_is_cut_into_spans_that_cover_it_exactly() {
	let len: usize = 10_000;
	let max: u32 = 4096;
	let mut spans: Vec<(usize, usize)> = Vec::new();
	let mut sent: usize = 0;
	while let Some((from, to)) = write_span(sent, len, max) {
		spans.push((from, to));
		sent = to;
	}
	assert_eq!(spans, [(0, 4096), (4096, 8192), (8192, 10_000)]);
	// Contiguous, in order, and every byte exactly once - which is the whole property a byte stream
	// needs from its cutting.
	assert_eq!(spans.first().map(|span| span.0), Some(0));
	assert_eq!(spans.last().map(|span| span.1), Some(len));
	for pair in spans.windows(2) {
		assert_eq!(pair[0].1, pair[1].0);
	}
	// A frame that fits goes in one call, and an exhausted one asks for nothing more.
	assert_eq!(write_span(0, 100, max), Some((0, 100)));
	assert_eq!(write_span(len, len, max), None);
	// A bound of zero cannot cut anything, and answering with an empty span would be an infinite
	// loop in every caller.
	assert_eq!(write_span(0, 100, 0), None);
}

#[test]
fn a_partial_write_loses_the_provider_and_a_backpressure_refusal_does_not() {
	assert_eq!(classify_write(WriteAnswer::Took(512), 512), Wrote::All);
	// The remainder of a half-taken span would be read as the beginning of the next frame, so there
	// is nothing to recover: the attachment ends.
	assert_eq!(classify_write(WriteAnswer::Took(511), 512), Wrote::ProviderLost);
	assert_eq!(classify_write(WriteAnswer::Took(0), 512), Wrote::ProviderLost);
	// `again` is the HOST having stopped reading. The device is fine, and tearing down a working
	// port because somebody closed a terminal is the failure this distinction exists to prevent.
	assert_eq!(classify_write(WriteAnswer::Again, 512), Wrote::SessionOver);
	assert_eq!(classify_write(WriteAnswer::Refused, 512), Wrote::ProviderLost);
	assert_eq!(classify_write(WriteAnswer::NoAnswer, 512), Wrote::ProviderLost);
}
