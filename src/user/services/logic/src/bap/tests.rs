use super::*;
use crate::gatt_server::Server;
use crate::le_audio::{context, location};
use alloc::vec;

// AN EARBUD'S TABLE, served by this crate's own GATT server: PACS, ASCS with a sink and a source ASE, CSIS with an
// encrypted key, and VCS.
fn earbud(ltk_wire: &[u8; 16], sirk: &[u8; 16]) -> Server {
	let mut server = Server::new(b"earbud");
	let capabilities = Capabilities { frequencies: 0b1011_0100, durations: 0b10, channel_counts: 1, min_octets: 26, max_octets: 155, frames_per_sdu: 1 };
	let pac = le_audio::encode_pac(&le_audio::LC3_ID, &capabilities.encode(), &[]);
	server.add_service(
		uuid::PACS,
		&[
			(uuid::SINK_PAC, 0x02, pac.clone(), false),
			(uuid::SINK_AUDIO_LOCATIONS, 0x02, location::FRONT_LEFT.to_le_bytes().to_vec(), false),
			(uuid::SOURCE_PAC, 0x02, pac, false),
		],
	);
	server.add_service(uuid::ASCS, &[(uuid::SINK_ASE, 0x12, vec![1, 0], false), (uuid::SOURCE_ASE, 0x12, vec![2, 0], false), (uuid::ASE_CONTROL_POINT, 0x1c, vec![], true)]);
	let mut ltk = *ltk_wire;
	ltk.reverse();
	let encrypted = le_audio::sef(&ltk, sirk);
	let mut key = vec![0x00];
	key.extend(encrypted.iter().rev());
	server.add_service(uuid::CSIS, &[(uuid::SET_IDENTITY_RESOLVING_KEY, 0x02, key, false), (uuid::COORDINATED_SET_SIZE, 0x02, vec![2], false), (uuid::SET_MEMBER_RANK, 0x02, vec![1], false)]);
	server.add_service(uuid::VCS, &[(uuid::VOLUME_STATE, 0x12, vec![100, 0, 3], false), (uuid::VOLUME_CONTROL_POINT, 0x08, vec![], true)]);
	server
}

// Every request answered by the server until the member asks nothing more: the events it raised.
fn run(member: &mut Member, server: &mut Server, mut outs: Vec<Out>) -> Vec<Event> {
	let mut events = Vec::new();
	for _ in 0..200 {
		let mut next = Vec::new();
		for out in outs {
			match out {
				Out::Request(pdu) => next.extend(member.on_response(&server.answer(&pdu).expect("every request is answered"))),
				Out::Event(event) => events.push(event),
			}
		}
		if next.is_empty() {
			break;
		}
		outs = next;
	}
	events
}

#[test]
// THE WALK: the MTU, every service, the four this client uses walked to their descriptors, what it reads read - the
// set's key decrypted with the link's - and notifications turned on.
fn the_walk_finds_what_the_earbud_has() {
	let ltk_wire = [0x42; 16];
	let sirk = [0x17; 16];
	let mut server = earbud(&ltk_wire, &sirk);
	let (mut member, first) = Member::new(ltk_wire);
	let events = run(&mut member, &mut server, first);
	let Some(Event::Ready(found)) = events.last() else { panic!("the walk did not end ready: {events:?}") };
	assert_eq!(found.sink.map(|capabilities| capabilities.max_octets), Some(155));
	assert!(found.source.is_some());
	assert_eq!(found.sink_locations, location::FRONT_LEFT);
	assert_eq!(found.set, Some((sirk, 2, 1)));
	assert_eq!(found.volume, Some(VolumeState { setting: 100, muted: false, counter: 3 }));
	assert!(member.ready());
	// THE NOTIFICATIONS: the ASEs', the control point's and the volume's configuration written.
	let sink = member.characteristic(uuid::SINK_ASE).unwrap();
	assert_eq!(server.value(sink.configuration.unwrap()), Some(&[1u8, 0][..]));
	let volume = member.characteristic(uuid::VOLUME_STATE).unwrap();
	assert_eq!(server.value(volume.configuration.unwrap()), Some(&[1u8, 0][..]));
	assert_eq!(member.ases(Direction::Sink).map(|ase| ase.id).collect::<Vec<_>>(), [1]);
}

#[test]
// THE STREAM'S STEPS: Config Codec written to the control point, each ASE's notification an event, a refusal said,
// QoS and Enable for what is configured, and the volume written with its counter.
fn the_ase_operations_are_written_and_their_states_reported() {
	let ltk_wire = [0x42; 16];
	let mut server = earbud(&ltk_wire, &[0x17; 16]);
	let (mut member, first) = Member::new(ltk_wire);
	run(&mut member, &mut server, first);
	let config = le_audio::Config::new(48_000, 10_000, 120);
	let outs = member.configure(Some(config), Some(le_audio::Config::new(16_000, 10_000, 40)));
	let [Out::Request(pdu)] = &outs[..] else { panic!("one write") };
	let point = member.characteristic(uuid::ASE_CONTROL_POINT).unwrap().value;
	assert_eq!(&pdu[..3], &[op::WRITE_REQUEST, point as u8, 0]);
	assert_eq!(&pdu[3..6], &[ascs::opcode::CONFIG_CODEC, 2, 1]);
	assert!(member.on_response(&server.answer(pdu).unwrap()).is_empty());
	// THE EARBUD CONFIGURES the sink ASE and says so.
	let sink = member.characteristic(uuid::SINK_ASE).unwrap().value;
	let field = config.encode();
	let mut configured = vec![1, ascs::state::CODEC_CONFIGURED, 0, 2, 2, 20, 0, 0x10, 0x27, 0, 0x40, 0x9c, 0, 0x10, 0x27, 0, 0x40, 0x9c, 0];
	configured.extend_from_slice(&le_audio::LC3_ID);
	configured.push(field.len() as u8);
	configured.extend_from_slice(&field);
	let events = member.on_notification(sink, &configured);
	assert!(matches!(&events[..], [Out::Event(Event::Ase { direction: Direction::Sink, id: 1, state: State::CodecConfigured { .. } })]));
	// A REFUSAL on the control point, for the source ASE.
	let refused = member.on_notification(point, &[ascs::opcode::CONFIG_CODEC, 2, 1, 0, 0, 2, 0x0a, 0x02]);
	assert_eq!(refused, vec![Out::Event(Event::Refused { opcode: 1, id: 2, code: 0x0a, reason: 0x02 })]);
	// QOS for the configured sink ASE, the earbud's own preferences in it.
	let outs = member.qos(1, 0, Some(config), None);
	let [Out::Request(pdu)] = &outs[..] else { panic!("one write") };
	assert_eq!(&pdu[3..], &ascs::config_qos(&[Qos { ase: 1, cig: 1, cis: 0, sdu_interval_us: 10_000, max_sdu: 120, retransmissions: 2, max_latency_ms: 20, presentation_delay_us: 10_000 }])[..]);
	member.on_response(&server.answer(pdu).unwrap());
	member.on_notification(sink, &[1, ascs::state::QOS_CONFIGURED, 1, 0, 0x10, 0x27, 0, 0, 2, 120, 0, 2, 20, 0, 0x10, 0x27, 0]);
	let outs = member.enable(context::MEDIA);
	let [Out::Request(pdu)] = &outs[..] else { panic!("one write") };
	assert_eq!(&pdu[3..], &ascs::enable(&[(1, context::MEDIA)])[..]);
	member.on_response(&server.answer(pdu).unwrap());
	// THE VOLUME.
	let outs = member.set_volume(50);
	let [Out::Request(pdu)] = &outs[..] else { panic!("one write") };
	assert_eq!(&pdu[3..], &[0x04, 3, 128]);
}

#[test]
// A DEVICE WITHOUT ASCS is not one this client drives, and says so.
fn a_device_without_stream_control_fails_the_walk() {
	let mut server = Server::new(b"speaker");
	server.add_service(uuid::VCS, &[(uuid::VOLUME_STATE, 0x12, vec![1, 0, 0], false)]);
	let (mut member, first) = Member::new([0; 16]);
	let events = run(&mut member, &mut server, first);
	assert!(matches!(events.last(), Some(Event::Failed(_))), "{events:?}");
}
