use super::*;
use alloc::vec;

#[test]
// THE COMMANDS' LAYOUTS, field by field as the Core specification orders them.
fn the_commands_carry_their_fields_in_order() {
	let cig = set_cig_parameters(
		1,
		10_000,
		20,
		&[
			CisParameters { id: 0, max_sdu_to_peripheral: 120, max_sdu_to_central: 0, retransmissions: 2 },
			CisParameters { id: 1, max_sdu_to_peripheral: 120, max_sdu_to_central: 40, retransmissions: 2 },
		],
	);
	assert_eq!(&cig[..16], &[1, 0x10, 0x27, 0x00, 0x10, 0x27, 0x00, 0, 0, 0, 20, 0, 20, 0, 2, 0]);
	assert_eq!(&cig[16..24], &[120, 0, 0, 0, 2, 2, 2, 2]);
	assert_eq!(cig.len(), 15 + 2 * 9);
	assert_eq!(cig_handles(&[0, 1, 2, 0x60, 0x00, 0x61, 0x00]), Some((1, vec![0x60, 0x61])));
	assert_eq!(cig_handles(&[0x0c, 1, 0]), None, "a refusal has no handles");
	assert_eq!(create_cis(&[(0x60, 0x40)]), [1, 0x60, 0, 0x40, 0]);
	assert_eq!(setup_iso_data_path(0x61, Direction::Output), [0x61, 0, 1, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0]);
	assert_eq!(extended_scan_enable(true), [1, 0, 0, 0, 0, 0]);
	let sync = periodic_create_sync(3, 1, &[1, 2, 3, 4, 5, 6]);
	assert_eq!(&sync[..9], &[0, 3, 1, 1, 2, 3, 4, 5, 6]);
	assert_eq!(&sync[11..13], &[0xe8, 0x03]);
	let big = big_create_sync(0, 0x0001, Some(&[7; 16]), &[1, 2]);
	assert_eq!(&big[..4], &[0, 1, 0, 1]);
	assert_eq!(&big[4..20], &[7; 16]);
	assert_eq!(&big[20..], &[0, 0xe8, 0x03, 2, 1, 2]);
	assert_eq!(set_host_feature(ISOCHRONOUS_CHANNELS_HOST_SUPPORT, true), [32, 1]);
}

#[test]
// THE EVENTS: each decoded from a body laid out as the specification lays it out, and one that runs short refused.
fn the_events_are_read_and_short_ones_refused() {
	let mut report = vec![1u8];
	report.extend_from_slice(&[0x00, 0x00, 1, 6, 5, 4, 3, 2, 1, 1, 2, 7, 0x7f, 0xc4, 0x50, 0x00, 0, 0, 0, 0, 0, 0, 0, 3, 0xaa, 0xbb, 0xcc]);
	let Some(Event::ExtendedReports(reports)) = event(subevent::EXTENDED_ADVERTISING_REPORT, &report) else { panic!("a report") };
	assert_eq!(reports[0].wire_address, [6, 5, 4, 3, 2, 1]);
	assert_eq!((reports[0].sid, reports[0].rssi, reports[0].periodic_interval), (7, -60, 0x50));
	assert_eq!(reports[0].data, [0xaa, 0xbb, 0xcc]);
	assert_eq!(event(subevent::EXTENDED_ADVERTISING_REPORT, &report[..report.len() - 1]), None);
	let established = [0u8, 0x01, 0x00, 7, 1, 6, 5, 4, 3, 2, 1, 2, 0x50, 0x00, 0];
	assert_eq!(event(subevent::PERIODIC_ADVERTISING_SYNC_ESTABLISHED, &established), Some(Event::SyncEstablished(SyncEstablished { status: 0, sync: 1, sid: 7, address_type: 1, wire_address: [6, 5, 4, 3, 2, 1] })));
	assert_eq!(event(subevent::PERIODIC_ADVERTISING_REPORT, &[1, 0, 0x7f, 0xc4, 0xff, 0, 2, 9, 9]), Some(Event::PeriodicReport { sync: 1, data: vec![9, 9], complete: true }));
	let mut cis = [0u8; 28];
	cis[1] = 0x60;
	cis[22] = 120;
	cis[24] = 40;
	cis[26] = 8;
	assert_eq!(event(subevent::CIS_ESTABLISHED, &cis), Some(Event::CisEstablished(CisEstablished { status: 0, handle: 0x60, max_pdu_to_peripheral: 120, max_pdu_to_central: 40, iso_interval: 8 })));
	let mut info = [0u8; 19];
	info[0] = 1;
	info[2] = 2;
	info[11..14].copy_from_slice(&[0x10, 0x27, 0x00]);
	info[14] = 100;
	info[18] = 1;
	assert_eq!(event(subevent::BIGINFO_ADVERTISING_REPORT, &info), Some(Event::BigInfo(BigInfo { sync: 1, bises: 2, sdu_interval_us: 10_000, max_sdu: 100, encrypted: true })));
	let mut big = vec![0u8, 0, 0, 0, 0, 1, 1, 0, 0, 100, 0, 8, 0, 2];
	big.extend_from_slice(&[0x70, 0x00, 0x71, 0x00]);
	assert_eq!(event(subevent::BIG_SYNC_ESTABLISHED, &big), Some(Event::BigEstablished(BigEstablished { status: 0, big: 0, handles: vec![0x70, 0x71] })));
	assert_eq!(event(subevent::BIG_SYNC_ESTABLISHED, &big[..15]), None);
	assert_eq!(event(subevent::BIG_SYNC_LOST, &[0, 0x13]), Some(Event::BigLost { big: 0, reason: 0x13 }));
	assert_eq!(buffer_sizes_v2(&[0, 251, 0, 8, 120, 0, 4]), Some(((251, 8), (120, 4))));
}

#[test]
// ISO DATA: an SDU sent whole reads back; fragments put together; a time stamp skipped; out of order and over-long
// dropped.
fn iso_packets_round_trip_and_reassemble() {
	let packet = iso_packet(0x60, 7, &[1, 2, 3]);
	assert_eq!(packet, [0x60, 0x20, 7, 0, 7, 0, 3, 0, 1, 2, 3]);
	let mut reassembly = Reassembly::default();
	assert_eq!(reassembly.push(&packet), Some(Sdu { handle: 0x60, sequence: 7, valid: true, data: vec![1, 2, 3] }));
	// FIRST (with a time stamp) and LAST.
	let first = [0x61, 0x40, 10, 0, 1, 1, 1, 1, 9, 0, 5, 0, 1, 2];
	let last = [0x61, 0x30, 3, 0, 3, 4, 5];
	assert_eq!(reassembly.push(&first), None);
	assert_eq!(reassembly.push(&last), Some(Sdu { handle: 0x61, sequence: 9, valid: true, data: vec![1, 2, 3, 4, 5] }));
	assert_eq!(reassembly.push(&last), None, "a last fragment with no first is dropped");
	// A packet the controller marked lost.
	let lost = [0x60, 0x20, 4, 0, 8, 0, 0, 0x80];
	assert_eq!(reassembly.push(&lost).map(|sdu| sdu.valid), Some(false));
	let mut huge = vec![0x60, 0x20, 0, 0, 0, 0, 0xff, 0x0f];
	let len = (huge.len() - 4) as u16;
	huge[2..4].copy_from_slice(&len.to_le_bytes());
	assert_eq!(reassembly.push(&huge), None, "an SDU past the bound");
}

#[test]
// THE CLASSIFIER: a stream's handle is ISO, a link's ACL, anybody else's unknown - and each kind's ceiling after.
fn data_packets_are_classified_by_their_handle() {
	let iso = |handle: u16| handle == 0x60;
	let link = |handle: u16| handle == 0x40;
	assert_eq!(classify(&[0x60, 0x20, 0, 0], iso, link, 27, 300), Class::Iso);
	assert_eq!(classify(&[0x40, 0x20, 0, 0], iso, link, 27, 300), Class::Acl);
	assert_eq!(classify(&[0x41, 0x20, 0, 0], iso, link, 27, 300), Class::Unknown);
	assert_eq!(classify(&[0x40, 0x20, 0, 0, 0, 0, 0, 0], iso, link, 7, 300), Class::TooLong, "an ACL packet above the ACL ceiling");
	assert_eq!(classify(&[0x60, 0x20, 0, 0, 0, 0, 0, 0], iso, link, 7, 300), Class::Iso, "the ISO ceiling, not the ACL one");
}

#[test]
// THE LEGACY PARAMETERS, MOVED: every field of LE Create Connection where LE Extended Create Connection has it, and the
// scan's likewise.
fn legacy_parameters_move_to_their_extended_places() {
	let legacy = [0x60, 0x00, 0x30, 0x00, 1, 0, 1, 2, 3, 4, 5, 6, 1, 0x18, 0, 0x28, 0, 0, 0, 0x48, 0, 0, 0, 0, 0];
	let extended = extended_create_connection(&legacy);
	assert_eq!(&extended[..10], &[1, 1, 0, 1, 2, 3, 4, 5, 6, 0x01]);
	assert_eq!(&extended[10..14], &[0x60, 0x00, 0x30, 0x00]);
	assert_eq!(&extended[14..], &legacy[13..]);
	assert_eq!(extended_scan_parameters_from(&[1, 0x10, 0, 0x10, 0, 1, 0]), [1, 0, 1, 1, 0x10, 0, 0x10, 0]);
	assert_eq!(LE_EVENT_MASK & 0x1ff, 0x1ff);
	assert_ne!(LE_EVENT_MASK & (1 << 24), 0, "CIS established");
}
