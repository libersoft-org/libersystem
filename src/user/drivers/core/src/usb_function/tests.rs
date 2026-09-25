use super::*;
use alloc::vec::Vec;

fn config(body: &[u8]) -> Vec<u8> {
	let mut out: Vec<u8> = alloc::vec![9, descriptor::DT_CONFIG, 0, 0, 1, 3, 0, 0x80, 50];
	out.extend_from_slice(body);
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out
}

fn interface(number: u8, alternate: u8, endpoints: u8, class: u8, subclass: u8, protocol: u8) -> [u8; 9] {
	[9, descriptor::DT_INTERFACE, number, alternate, endpoints, class, subclass, protocol, 0]
}

fn endpoint(address: u8, attributes: u8, packet: u16, interval: u8) -> [u8; 7] {
	let packet = packet.to_le_bytes();
	[7, descriptor::DT_ENDPOINT, address, attributes, packet[0], packet[1], interval]
}

#[test]
fn settings_carry_their_endpoints_and_the_records_between_them() {
	let mut body: Vec<u8> = Vec::new();
	body.extend_from_slice(&[8, DT_INTERFACE_ASSOCIATION, 0, 2, 0x0e, 3, 0, 0]);
	body.extend_from_slice(&interface(0, 0, 1, 0x0e, 1, 0));
	body.extend_from_slice(&[5, DT_CS_INTERFACE, 1, 0x10, 0x01]);
	body.extend_from_slice(&endpoint(0x83, 0x03, 16, 8));
	body.extend_from_slice(&[5, DT_CS_ENDPOINT, 3, 16, 0]);
	body.extend_from_slice(&interface(1, 0, 2, 7, 1, 2));
	body.extend_from_slice(&endpoint(0x01, 0x02, 512, 0));
	body.extend_from_slice(&endpoint(0x82, 0x02, 512, 0));
	let bytes = config(&body);
	let parsed = Configuration::parse(&bytes).expect("a well-formed configuration parses");
	assert_eq!(parsed.value, 3);
	assert_eq!(parsed.settings.len(), 2);
	let first = &parsed.settings[0];
	assert_eq!((first.interface, first.class, first.subclass), (0, 0x0e, 1));
	let functional: Vec<u8> = parsed.functional(first).map(|record| record.kind).collect();
	assert_eq!(functional, [DT_CS_INTERFACE], "the class record between the interface and its endpoint is the setting's");
	let extra: Vec<u8> = parsed.endpoint_extra(&first.endpoints[0]).map(|record| record.kind).collect();
	assert_eq!(extra, [DT_CS_ENDPOINT], "and the one after the endpoint is the endpoint's");
	assert!(first.endpoints[0].is_interrupt_in());
	assert_eq!(first.endpoints[0].dci(), 7);
	let printer = parsed.find(7, 1, |setting| setting.first(Endpoint::is_bulk_out).is_some()).expect("the printer setting is found by class");
	assert_eq!(printer.first(Endpoint::is_bulk_out).map(|e| e.dci()), Some(2));
	assert_eq!(printer.first(Endpoint::is_bulk_in).map(|e| e.dci()), Some(5));
}

#[test]
fn a_transfer_that_is_not_a_configuration_is_refused() {
	let mut bytes = config(&interface(0, 0, 0, 7, 1, 1));
	bytes[1] = descriptor::DT_STRING;
	assert_eq!(Configuration::parse(&bytes).err(), Some(Refused::NotConfiguration));
	assert_eq!(Configuration::parse(&[]).err(), Some(Refused::NotConfiguration));
}

#[test]
fn short_records_and_misplaced_endpoints_are_refused() {
	// An interface record whose own length stops before the class byte.
	let mut body: Vec<u8> = alloc::vec![5, descriptor::DT_INTERFACE, 0, 0, 1];
	body.extend_from_slice(&endpoint(0x81, 0x02, 64, 0));
	assert_eq!(Configuration::parse(&config(&body)).err(), Some(Refused::Malformed));
	// An endpoint before any interface.
	assert_eq!(Configuration::parse(&config(&endpoint(0x81, 0x02, 64, 0))).err(), Some(Refused::Malformed));
	// An endpoint record cut before its interval.
	let mut body: Vec<u8> = interface(0, 0, 1, 7, 1, 1).to_vec();
	body.extend_from_slice(&[6, descriptor::DT_ENDPOINT, 0x01, 0x02, 64, 0]);
	assert_eq!(Configuration::parse(&config(&body)).err(), Some(Refused::Malformed));
}

#[test]
fn endpoint_zero_and_a_repeated_address_are_refused() {
	let mut body: Vec<u8> = interface(0, 0, 1, 7, 1, 1).to_vec();
	body.extend_from_slice(&endpoint(0x80, 0x02, 64, 0));
	assert_eq!(Configuration::parse(&config(&body)).err(), Some(Refused::BadEndpoint));
	let mut body: Vec<u8> = interface(0, 0, 2, 7, 1, 1).to_vec();
	body.extend_from_slice(&endpoint(0x01, 0x02, 64, 0));
	body.extend_from_slice(&endpoint(0x01, 0x02, 64, 0));
	assert_eq!(Configuration::parse(&config(&body)).err(), Some(Refused::BadEndpoint));
}

#[test]
fn a_record_that_runs_past_the_transfer_is_a_walk_fault() {
	let mut body: Vec<u8> = interface(0, 0, 1, 7, 1, 1).to_vec();
	body.extend_from_slice(&[40, DT_CS_INTERFACE, 1]);
	assert_eq!(Configuration::parse(&config(&body)).err(), Some(Refused::Walk(DescriptorFault::Overrun)));
	let mut body: Vec<u8> = interface(0, 0, 1, 7, 1, 1).to_vec();
	body.extend_from_slice(&[1, DT_CS_INTERFACE]);
	assert_eq!(Configuration::parse(&config(&body)).err(), Some(Refused::Walk(DescriptorFault::Malformed)));
}

#[test]
fn a_miscounted_setting_keeps_the_endpoints_that_are_there() {
	let mut body: Vec<u8> = interface(0, 0, 5, 7, 1, 1).to_vec();
	body.extend_from_slice(&endpoint(0x01, 0x02, 64, 0));
	let bytes = config(&body);
	let parsed = Configuration::parse(&bytes).expect("a miscounted setting is read as it is");
	assert_eq!(parsed.settings[0].endpoints.len(), 1);
}

#[test]
fn more_settings_than_a_device_can_have_are_refused() {
	let mut body: Vec<u8> = Vec::new();
	for number in 0..=MAX_SETTINGS as u8 {
		body.extend_from_slice(&interface(number, 0, 0, 0xff, 0, 0));
	}
	assert_eq!(Configuration::parse(&config(&body)).err(), Some(Refused::TooMany));
}

#[test]
fn packet_sizes_drop_the_high_bandwidth_bits() {
	let mut body: Vec<u8> = interface(0, 0, 1, 7, 1, 1).to_vec();
	body.extend_from_slice(&endpoint(0x81, 0x03, 0x1400, 1));
	let bytes = config(&body);
	let parsed = Configuration::parse(&bytes).unwrap();
	assert_eq!(parsed.settings[0].endpoints[0].max_packet(), 0x400);
}
