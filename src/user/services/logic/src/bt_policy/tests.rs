use super::*;

fn trusted(profiles: &[Profile]) -> Trust {
	profiles.iter().fold(Trust::default(), |trust, profile| trust.with(*profile, true))
}

#[test]
fn a_profile_channel_needs_its_trust_and_encryption_and_sdp_needs_nothing() {
	let input = Peer { trust: trusted(&[Profile::Input]), receive_waits_for_it: false, encrypted: true };
	assert_eq!(decide(inbound_by_psm(psm::HID_INTERRUPT), &input), Decision::Accept);
	assert_eq!(decide(inbound_by_psm(psm::AVDTP), &input), Decision::Refused, "trusted for input, not audio");
	assert_eq!(decide(inbound_by_psm(psm::SDP), &Peer::default()), Decision::Accept, "SDP answers any connected peer");
	let unencrypted = Peer { encrypted: false, ..input };
	assert_eq!(decide(inbound_by_psm(psm::HID_CONTROL), &unencrypted), Decision::Refused, "user data needs encryption");
	assert_eq!(decide(inbound_by_psm(0x1003), &input), Decision::NotServed);
}

#[test]
fn opp_is_admitted_only_from_the_peer_a_receive_waits_for() {
	let everything = Peer { trust: Trust(0xFF), receive_waits_for_it: false, encrypted: true };
	assert_eq!(decide(inbound_by_psm(OPP_PSM), &everything), Decision::Refused, "no standing trust admits OPP");
	assert_eq!(decide(inbound_by_channel(OPP_CHANNEL), &everything), Decision::Refused);
	let waited_for = Peer { trust: Trust::default(), receive_waits_for_it: true, encrypted: true };
	assert_eq!(decide(inbound_by_psm(OPP_PSM), &waited_for), Decision::Accept);
	assert_eq!(decide(inbound_by_psm(psm::RFCOMM), &waited_for), Decision::Accept, "its RFCOMM session too");
	assert_eq!(decide(inbound_by_channel(HFP_CHANNEL), &waited_for), Decision::Refused, "but not the voice channel");
}

#[test]
fn rfcomm_admits_the_session_for_any_channel_and_each_channel_for_its_profile() {
	let voice = Peer { trust: trusted(&[Profile::Voice]), receive_waits_for_it: false, encrypted: true };
	assert_eq!(decide(inbound_by_psm(psm::RFCOMM), &voice), Decision::Accept);
	assert_eq!(decide(inbound_by_channel(HSP_CHANNEL), &voice), Decision::Accept);
	assert_eq!(decide(inbound_by_channel(OPP_CHANNEL), &voice), Decision::Refused);
	assert_eq!(decide(inbound_by_channel(20), &voice), Decision::NotServed);
	let input_only = Peer { trust: trusted(&[Profile::Input]), ..voice };
	assert_eq!(decide(inbound_by_psm(psm::RFCOMM), &input_only), Decision::Refused, "no channel served here is open to it");
}

#[test]
fn the_scan_mode_follows_the_radio_trust_receive_pairable_and_discoverable() {
	assert_eq!(scan(false, true, true, true, Some(10), 0), Scan::None);
	assert_eq!(scan(true, false, false, false, None, 0), Scan::None, "nobody to wait for: not connectable");
	assert_eq!(scan(true, true, false, false, None, 0), Scan::Page);
	assert_eq!(scan(true, false, true, false, None, 0), Scan::Page);
	assert_eq!(scan(true, false, false, true, None, 0), Scan::Page);
	let until = discoverable_until(1_000, 600);
	assert_eq!(until, 181_000, "180 seconds at most");
	assert_eq!(scan(true, false, false, false, Some(until), 50_000), Scan::Both);
	assert_eq!(scan(true, false, false, false, Some(until), 181_000), Scan::None, "and it ends");
}

#[test]
fn trust_bits_and_names_round_trip() {
	for profile in Profile::ALL {
		assert_eq!(Profile::from_name(profile.name()), Some(profile));
		assert!(Trust::default().with(profile, true).has(profile));
		assert!(!Trust(0xFF).with(profile, false).has(profile));
	}
	assert!(!trusted(&[Profile::Spp, Profile::Gatt]).any_inbound(), "client roles admit nothing inbound");
	assert!(trusted(&[Profile::Pan]).any_inbound());
}
