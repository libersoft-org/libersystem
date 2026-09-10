// The record, the latch and the relay check, each driven through every ending it has.

use super::*;

const ENFORCING: [u8; 8] = *b"LSDM\x01\x01\x02\x00";
const DEGRADED: [u8; 8] = *b"LSDM\x01\x02\x02\x00";

#[test]
fn the_record_is_exactly_the_frozen_eight_bytes() {
	// Written out as literals rather than through the encoder, so a change to the encoder that
	// moved a byte would fail here instead of round-tripping through itself.
	assert_eq!(encode_harness(Mode::EnforcingRequired), ENFORCING);
	assert_eq!(encode_harness(Mode::NoIommu), DEGRADED);
	assert_eq!(decode_harness(&ENFORCING), Ok(Mode::EnforcingRequired));
	assert_eq!(decode_harness(&DEGRADED), Ok(Mode::NoIommu));
	assert_eq!(RECORD_LEN, 8);
}

#[test]
fn every_malformed_shape_is_refused_and_named() {
	assert_eq!(decode_harness(&ENFORCING[..7]), Err(Malformed::Length(7)), "seven bytes");
	assert_eq!(decode_harness(b"LSDM\x01\x01\x02\x00\x00"), Err(Malformed::Length(9)), "nine bytes");
	assert_eq!(decode_harness(b""), Err(Malformed::Length(0)));
	assert_eq!(decode_harness(b"LSDN\x01\x01\x02\x00"), Err(Malformed::Magic), "another magic is not a version to tolerate");
	assert_eq!(decode_harness(b"LSDM\x02\x01\x02\x00"), Err(Malformed::Version(2)), "an unknown version is malformed, not ignored");
	assert_eq!(decode_harness(b"LSDM\x01\x00\x02\x00"), Err(Malformed::Mode(0)), "there is no encoding for absence");
	assert_eq!(decode_harness(b"LSDM\x01\x03\x02\x00"), Err(Malformed::Mode(3)));
	// THE ONE THIS CARRIER EXISTS TO REFUSE: a replaceable medium claiming authentication.
	assert_eq!(decode_harness(b"LSDM\x01\x01\x01\x00"), Err(Malformed::Provenance(1)), "`signed` on a harness carrier is malformed");
	assert_eq!(decode_harness(b"LSDM\x01\x01\x00\x00"), Err(Malformed::Provenance(0)));
	assert_eq!(decode_harness(b"LSDM\x01\x01\x02\x01"), Err(Malformed::Reserved(1)), "the reserved byte must be zero to validate");
	// And nothing about a malformed record is interpreted: the mode byte of a record whose
	// provenance is wrong is not read as a mode.
	assert_eq!(Carrier::from_bytes(Some(b"LSDM\x01\x02\x01\x00")), Carrier::Malformed(Malformed::Provenance(1)));
	assert_eq!(Carrier::from_bytes(None), Carrier::Absent);
}

#[test]
fn the_boot_info_words_round_trip_and_zero_is_absent() {
	for handoff in [Handoff::Signed(Mode::EnforcingRequired), Handoff::Signed(Mode::NoIommu), Handoff::Harness(Mode::EnforcingRequired), Handoff::Harness(Mode::NoIommu)] {
		let (mode, provenance) = handoff.words();
		assert_eq!(Handoff::from_words(mode, provenance), Some(handoff));
	}
	assert_eq!(Handoff::from_words(0, 0), None, "an uninitialised field is ABSENT");
	assert_eq!(Handoff::from_words(1, 0), None, "a mode with no provenance is not a hand-off");
	assert_eq!(Handoff::from_words(0, 2), None);
	assert_eq!(Handoff::from_words(3, 2), None);
	assert_eq!(Handoff::from_words(1, 3), None);
	assert_eq!(MODE_ABSENT, 0);
	assert_eq!(PROVENANCE_ABSENT, 0);
}

fn latched(fields: &[Option<u32>]) -> Latch {
	let mut latch = Latch::new();
	for field in fields {
		latch.record(*field);
	}
	latch
}

#[test]
fn an_all_tag_zero_set_takes_exactly_the_paths_valid_carrier() {
	// The test and development row: every selected manifest declares no mode, and the harness
	// carrier supplies it with `harness` provenance.
	let latch = latched(&[None, None, None]);
	assert_eq!(latch.manifests(), 3);
	assert_eq!(latch.resolve(Carrier::Valid(Mode::NoIommu)), Ok(Handoff::Harness(Mode::NoIommu)));
	assert_eq!(latch.resolve(Carrier::Valid(Mode::EnforcingRequired)), Ok(Handoff::Harness(Mode::EnforcingRequired)));
	// The same set without a carrier, and with a carrier that is not this record.
	assert_eq!(latch.resolve(Carrier::Absent), Err(LatchRefusal::CarrierAbsent));
	assert_eq!(latch.resolve(Carrier::Malformed(Malformed::Provenance(1))), Err(LatchRefusal::CarrierMalformed(Malformed::Provenance(1))));
}

#[test]
fn an_all_tag_one_set_is_the_signed_mode_and_tolerates_no_second_producer() {
	let latch = latched(&[Some(MODE_ENFORCING_REQUIRED), Some(MODE_ENFORCING_REQUIRED)]);
	assert_eq!(latch.resolve(Carrier::Absent), Ok(Handoff::Signed(Mode::EnforcingRequired)));
	let degraded = latched(&[Some(MODE_NO_IOMMU)]);
	assert_eq!(degraded.resolve(Carrier::Absent), Ok(Handoff::Signed(Mode::NoIommu)));
	// A harness carrier beside a signed set refuses EVEN WHEN ITS MODE AGREES, and even when it is
	// malformed: what is refused is the second producer, not its value.
	assert_eq!(latch.resolve(Carrier::Valid(Mode::EnforcingRequired)), Err(LatchRefusal::SecondProducer));
	assert_eq!(latch.resolve(Carrier::Valid(Mode::NoIommu)), Err(LatchRefusal::SecondProducer));
	assert_eq!(latch.resolve(Carrier::Malformed(Malformed::Magic)), Err(LatchRefusal::SecondProducer));
}

#[test]
fn a_mixed_or_disagreeing_set_refuses_whatever_the_carrier_says() {
	let mixed = latched(&[None, Some(MODE_ENFORCING_REQUIRED)]);
	assert_eq!(mixed.resolve(Carrier::Absent), Err(LatchRefusal::MixedPresence));
	assert_eq!(mixed.resolve(Carrier::Valid(Mode::EnforcingRequired)), Err(LatchRefusal::MixedPresence));
	let mixed_the_other_way = latched(&[Some(MODE_NO_IOMMU), None]);
	assert_eq!(mixed_the_other_way.resolve(Carrier::Absent), Err(LatchRefusal::MixedPresence));
	let differing = latched(&[Some(MODE_ENFORCING_REQUIRED), Some(MODE_NO_IOMMU)]);
	assert_eq!(differing.resolve(Carrier::Absent), Err(LatchRefusal::DifferingValues));
	// NO FIRST-MANIFEST PRECEDENCE: a contradiction recorded third is still the answer.
	let late = latched(&[Some(MODE_NO_IOMMU), Some(MODE_NO_IOMMU), Some(MODE_ENFORCING_REQUIRED)]);
	assert_eq!(late.resolve(Carrier::Absent), Err(LatchRefusal::DifferingValues));
	let late_presence = latched(&[None, None, Some(MODE_NO_IOMMU)]);
	assert_eq!(late_presence.resolve(Carrier::Valid(Mode::NoIommu)), Err(LatchRefusal::MixedPresence));
	// And a boot that verified nothing has no set to latch.
	assert_eq!(Latch::new().resolve(Carrier::Valid(Mode::NoIommu)), Err(LatchRefusal::NothingSelected));
}

#[test]
fn the_x86_64_relay_check_admits_exactly_the_relayed_input() {
	// The normal relay: harness provenance, and the fw_cfg record still there and agreeing.
	assert_eq!(relay_check(MODE_ENFORCING_REQUIRED, PROVENANCE_HARNESS, Carrier::Valid(Mode::EnforcingRequired)), Ok(Handoff::Harness(Mode::EnforcingRequired)));
	assert_eq!(relay_check(MODE_NO_IOMMU, PROVENANCE_HARNESS, Carrier::Valid(Mode::NoIommu)), Ok(Handoff::Harness(Mode::NoIommu)));
	// The input naming the other mode, malformed, or gone.
	assert_eq!(relay_check(MODE_ENFORCING_REQUIRED, PROVENANCE_HARNESS, Carrier::Valid(Mode::NoIommu)), Err(RelayRefusal::InputDisagrees));
	assert_eq!(relay_check(MODE_ENFORCING_REQUIRED, PROVENANCE_HARNESS, Carrier::Malformed(Malformed::Mode(3))), Err(RelayRefusal::InputMalformed(Malformed::Mode(3))));
	assert_eq!(relay_check(MODE_ENFORCING_REQUIRED, PROVENANCE_HARNESS, Carrier::Absent), Err(RelayRefusal::InputAbsent));
	// A signed hand-off with the fw_cfg record present is an independent producer, agreeing or not.
	assert_eq!(relay_check(MODE_ENFORCING_REQUIRED, PROVENANCE_SIGNED, Carrier::Absent), Ok(Handoff::Signed(Mode::EnforcingRequired)));
	assert_eq!(relay_check(MODE_ENFORCING_REQUIRED, PROVENANCE_SIGNED, Carrier::Valid(Mode::EnforcingRequired)), Err(RelayRefusal::IndependentInput));
	assert_eq!(relay_check(MODE_NO_IOMMU, PROVENANCE_SIGNED, Carrier::Malformed(Malformed::Magic)), Err(RelayRefusal::IndependentInput));
	// And no hand-off refuses before the input is looked at, whatever it says.
	assert_eq!(relay_check(MODE_ABSENT, PROVENANCE_ABSENT, Carrier::Valid(Mode::NoIommu)), Err(RelayRefusal::NoHandoff));
	assert_eq!(relay_check(MODE_NO_IOMMU, PROVENANCE_ABSENT, Carrier::Valid(Mode::NoIommu)), Err(RelayRefusal::NoHandoff));
}
