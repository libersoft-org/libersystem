use super::*;

fn nic(slot: u32, generation: u32) -> Publication {
	Publication { slot, generation, binding_generation: 1 }
}

fn switched(decision: Decision) -> Selected {
	match decision {
		Decision::Switch { to, .. } => to,
		Decision::Keep => panic!("a switch, not keep"),
	}
}

#[test]
fn a_service_without_a_link_takes_the_first_nic_and_keeps_it() {
	let mut uplinks = Uplinks::new();
	assert_eq!(switched(uplinks.published(nic(4, 1)).unwrap()), Selected::Nic(nic(4, 1)));
	// A LATE, LOWER PUBLICATION DOES NOT DISPLACE A USABLE ONE.
	assert_eq!(uplinks.published(nic(2, 1)).unwrap(), Decision::Keep);
	assert_eq!(uplinks.selected(), Selected::Nic(nic(4, 1)));
	assert_eq!(uplinks.published(nic(4, 1)).unwrap(), Decision::Keep, "a repeat changes nothing");
}

#[test]
fn losing_the_selected_nic_falls_back_to_the_lowest_live_one() {
	let mut uplinks = Uplinks::new();
	uplinks.published(nic(4, 1)).unwrap();
	uplinks.published(nic(7, 1)).unwrap();
	uplinks.published(nic(2, 3)).unwrap();
	assert_eq!(switched(uplinks.withdrawn(nic(4, 1))), Selected::Nic(nic(2, 3)));
	// A failed channel is torn down and not chosen again until republished.
	assert_eq!(switched(uplinks.failed(nic(2, 3))), Selected::Nic(nic(7, 1)));
	assert_eq!(switched(uplinks.withdrawn(nic(7, 1))), Selected::None, "nothing usable left: unlinked");
	assert_eq!(switched(uplinks.published(nic(2, 4)).unwrap()), Selected::Nic(nic(2, 4)), "a republication is taken");
	// Withdrawing what is not selected changes nothing.
	uplinks.published(nic(9, 1)).unwrap();
	assert_eq!(uplinks.withdrawn(nic(9, 1)), Decision::Keep);
}

#[test]
fn every_switch_is_a_new_interface_generation() {
	let mut uplinks = Uplinks::new();
	let first = match uplinks.published(nic(1, 1)).unwrap() {
		Decision::Switch { generation, .. } => generation,
		Decision::Keep => panic!(),
	};
	let second = match uplinks.withdrawn(nic(1, 1)) {
		Decision::Switch { generation, .. } => generation,
		Decision::Keep => panic!(),
	};
	assert!(second > first);
}

#[test]
fn sixteen_publications_are_tracked_and_the_seventeenth_refused() {
	let mut uplinks = Uplinks::new();
	for slot in 0..MAX_NICS as u32 {
		uplinks.published(nic(slot, 1)).unwrap();
	}
	assert_eq!(uplinks.published(nic(99, 1)), Err(Refusal::Exhausted));
	assert_eq!(uplinks.nics().len(), MAX_NICS);
}

#[test]
fn a_modem_reservation_is_busy_over_a_nic_unless_replacing_is_allowed() {
	let mut uplinks = Uplinks::new();
	uplinks.published(nic(3, 1)).unwrap();
	assert_eq!(uplinks.reserve(false), Err(Refusal::Busy), "admission refused, and the NIC intact");
	assert_eq!(uplinks.selected(), Selected::Nic(nic(3, 1)));
	let reservation = uplinks.reserve(true).unwrap();
	assert_eq!(uplinks.reserve(true), Err(Refusal::Busy), "one modem link at a time");
	assert_eq!(switched(uplinks.install(reservation).unwrap()), Selected::Modem(reservation));
	assert_eq!(uplinks.reserve(true), Err(Refusal::Busy), "and none while one is installed");
	// Removing it brings the replaced NIC back.
	assert_eq!(switched(uplinks.release(reservation)), Selected::Nic(nic(3, 1)));
}

#[test]
fn with_no_nic_a_modem_needs_no_replace_authority() {
	let mut uplinks = Uplinks::new();
	let reservation = uplinks.reserve(false).unwrap();
	assert_eq!(switched(uplinks.install(reservation).unwrap()), Selected::Modem(reservation));
	// A NIC appearing while the modem is installed is recorded and not selected.
	assert_eq!(uplinks.published(nic(5, 1)).unwrap(), Decision::Keep);
	assert_eq!(uplinks.selected(), Selected::Modem(reservation));
	// When the modem goes, the deterministic fallback is that NIC.
	assert_eq!(switched(uplinks.release(reservation)), Selected::Nic(nic(5, 1)));
}

#[test]
fn a_nic_that_appeared_after_a_reservation_is_not_replaced_without_authority() {
	let mut uplinks = Uplinks::new();
	let reservation = uplinks.reserve(false).unwrap();
	// A publication arriving while a reservation is open is taken: a link now beats a modem later...
	assert_eq!(switched(uplinks.published(nic(1, 1)).unwrap()), Selected::Nic(nic(1, 1)));
	// ...and cannot be replaced by the reservation that had no authority to replace one.
	assert_eq!(uplinks.install(reservation), Err(Refusal::Busy));
	assert_eq!(uplinks.reserve(false), Err(Refusal::Busy), "admission refused - but the reservation was given back");
}

#[test]
fn the_replaced_nic_gone_means_the_fallback_or_nothing() {
	let mut uplinks = Uplinks::new();
	uplinks.published(nic(3, 1)).unwrap();
	let reservation = uplinks.reserve(true).unwrap();
	uplinks.install(reservation).unwrap();
	assert_eq!(uplinks.withdrawn(nic(3, 1)), Decision::Keep, "the modem stays");
	assert_eq!(switched(uplinks.release(reservation)), Selected::None, "no NIC left: unlinked");
	// A stale release of the old reservation restores nothing.
	uplinks.published(nic(8, 1)).unwrap();
	assert_eq!(uplinks.release(reservation), Decision::Keep);
	assert_eq!(uplinks.selected(), Selected::Nic(nic(8, 1)));
}

#[test]
fn a_reservation_given_back_unused_changes_nothing() {
	let mut uplinks = Uplinks::new();
	uplinks.published(nic(3, 1)).unwrap();
	let reservation = uplinks.reserve(true).unwrap();
	assert_eq!(uplinks.release(reservation), Decision::Keep);
	assert_eq!(uplinks.selected(), Selected::Nic(nic(3, 1)));
	assert_eq!(uplinks.install(reservation), Err(Refusal::NotFound));
}

fn attachment() -> Attachment {
	Attachment { family: 4, address: [10, 64, 0, 2], prefix: 30, gateway: Some([10, 64, 0, 1]), dns: [Some([10, 64, 0, 1]), None], mtu: 1400 }
}

#[test]
fn a_raw_ip_configuration_is_validated_before_anything_is_installed() {
	assert_eq!(validate(&attachment()), Ok(()));
	assert_eq!(validate(&Attachment { family: 6, ..attachment() }), Err(Refusal::Unsupported), "IPv6 is typed unsupported");
	assert_eq!(validate(&Attachment { family: 5, ..attachment() }), Err(Refusal::Invalid));
	// The network and broadcast addresses of a /30 are not host addresses.
	assert_eq!(validate(&Attachment { address: [10, 64, 0, 0], gateway: None, ..attachment() }), Err(Refusal::Invalid));
	assert_eq!(validate(&Attachment { address: [10, 64, 0, 3], gateway: None, ..attachment() }), Err(Refusal::Invalid));
	// A /32 point-to-point address has neither and is fine.
	assert_eq!(validate(&Attachment { prefix: 32, gateway: None, ..attachment() }), Ok(()));
	// A gateway off the subnet, or the address itself.
	assert_eq!(validate(&Attachment { gateway: Some([10, 64, 1, 1]), ..attachment() }), Err(Refusal::Invalid));
	assert_eq!(validate(&Attachment { gateway: Some([10, 64, 0, 2]), ..attachment() }), Err(Refusal::Invalid));
	// Multicast, loopback and unspecified addresses are not unicast.
	assert_eq!(validate(&Attachment { address: [224, 0, 0, 1], ..attachment() }), Err(Refusal::Invalid));
	assert_eq!(validate(&Attachment { dns: [Some([127, 0, 0, 1]), None], ..attachment() }), Err(Refusal::Invalid));
	// The MTU bounds, exactly.
	assert_eq!(validate(&Attachment { mtu: MIN_MTU, ..attachment() }), Ok(()));
	assert_eq!(validate(&Attachment { mtu: MAX_MTU, ..attachment() }), Ok(()));
	assert_eq!(validate(&Attachment { mtu: MIN_MTU - 1, ..attachment() }), Err(Refusal::Invalid));
	assert_eq!(validate(&Attachment { mtu: MAX_MTU + 1, ..attachment() }), Err(Refusal::Invalid));
	assert_eq!(validate(&Attachment { prefix: 0, ..attachment() }), Err(Refusal::Invalid));
	assert_eq!(validate(&Attachment { prefix: 33, ..attachment() }), Err(Refusal::Invalid));
}
