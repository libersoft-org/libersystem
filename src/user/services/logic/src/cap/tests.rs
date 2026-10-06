use super::*;
use crate::bap::Found;
use crate::le_audio::Capabilities;
use alloc::vec;

fn found(location: u32, max_octets: u16, source: bool) -> Found {
	let capabilities = Capabilities { frequencies: 0b1011_0100, durations: 0b10, channel_counts: 1, min_octets: 26, max_octets, frames_per_sdu: 1 };
	Found { sink: Some(capabilities), source: source.then_some(capabilities), sink_locations: location, set: None, volume: None }
}

fn codec_configured() -> State {
	State::CodecConfigured { preferences: crate::ascs::Preferences { retransmissions: 2, max_latency_ms: 20, delay_min_us: 10_000, delay_max_us: 40_000 }, config: None }
}

#[test]
// A PAIR PLAYS MUSIC: one configuration both take - the best the weaker supports - each its own channel; then the
// CIG, QoS, Enable, the CISes together, the data paths, and the streams.
fn a_pair_streams_music_through_every_step() {
	let members = [(0x40, found(location::FRONT_LEFT, 155, true)), (0x41, found(location::FRONT_RIGHT, 100, true))];
	let (mut group, actions) = Group::start(&members, Purpose::Media).unwrap();
	let left = Config { allocation: location::FRONT_LEFT, ..Config::new(32_000, 10_000, 80) };
	let right = Config { allocation: location::FRONT_RIGHT, ..left };
	assert_eq!(actions, vec![Action::Member(0x40, Step::Configure(Some(left), None)), Action::Member(0x41, Step::Configure(Some(right), None))]);
	assert!(group.on_ase(0x40, Direction::Sink, &codec_configured()).is_empty(), "the other member is not configured yet");
	let cig = group.on_ase(0x41, Direction::Sink, &codec_configured());
	assert_eq!(
		cig,
		vec![Action::Hci(
			le_iso::opcode::LE_SET_CIG_PARAMETERS,
			le_iso::set_cig_parameters(
				CIG,
				10_000,
				20,
				&[
					CisParameters { id: 0, max_sdu_to_peripheral: 80, max_sdu_to_central: 0, retransmissions: 2 },
					CisParameters { id: 1, max_sdu_to_peripheral: 80, max_sdu_to_central: 0, retransmissions: 2 }
				]
			)
		)]
	);
	assert_eq!(group.on_cig(&[0x60, 0x61]), vec![Action::Member(0x40, Step::Qos { cis: 0, sink: Some(left), source: None }), Action::Member(0x41, Step::Qos { cis: 1, sink: Some(right), source: None })]);
	group.on_ase(0x40, Direction::Sink, &State::QosConfigured { cig: 1, cis: 0 });
	assert_eq!(group.on_ase(0x41, Direction::Sink, &State::QosConfigured { cig: 1, cis: 1 }), vec![Action::Member(0x40, Step::Enable(le_audio::context::MEDIA)), Action::Member(0x41, Step::Enable(le_audio::context::MEDIA))]);
	group.on_ase(0x40, Direction::Sink, &State::Enabling { cig: 1, cis: 0 });
	assert_eq!(group.on_ase(0x41, Direction::Sink, &State::Enabling { cig: 1, cis: 1 }), vec![Action::Hci(le_iso::opcode::LE_CREATE_CIS, le_iso::create_cis(&[(0x60, 0x40), (0x61, 0x41)]))]);
	assert_eq!(group.on_cis(0x60, 0), vec![Action::Hci(le_iso::opcode::LE_SETUP_ISO_DATA_PATH, le_iso::setup_iso_data_path(0x60, le_iso::Direction::Input).to_vec())]);
	group.on_cis(0x61, 0);
	group.on_ase(0x40, Direction::Sink, &State::Streaming { cig: 1, cis: 0 });
	let streaming = group.on_ase(0x41, Direction::Sink, &State::Streaming { cig: 1, cis: 1 });
	assert_eq!(streaming, vec![Action::Streaming(vec![Stream { link: 0x40, cis: 0x60, sink: Some(left), source: None }, Stream { link: 0x41, cis: 0x61, sink: Some(right), source: None }])]);
	// STOP: released, the CISes disconnected, and only once they are down the CIG removed.
	assert_eq!(group.stop(), vec![Action::Member(0x40, Step::Release), Action::Member(0x41, Step::Release)]);
	assert!(group.stop().is_empty(), "a second stop asks for nothing again");
	assert!(group.on_ase(0x40, Direction::Sink, &State::Releasing).is_empty(), "the other member still streams");
	assert_eq!(group.on_ase(0x41, Direction::Sink, &State::Releasing), vec![Action::Hci(DISCONNECT, vec![0x60, 0, REMOTE_USER_TERMINATED]), Action::Hci(DISCONNECT, vec![0x61, 0, REMOTE_USER_TERMINATED])]);
	assert!(group.on_cis_down(0x60).is_empty(), "a CIS is still up: no CIG removal a controller would refuse");
	group.on_ase(0x40, Direction::Sink, &State::Idle);
	assert_eq!(group.on_cis_down(0x61), vec![Action::Hci(le_iso::opcode::LE_REMOVE_CIG, vec![CIG]), Action::Stopped("stopped")]);
	assert!(group.on_ase(0x41, Direction::Sink, &State::Idle).is_empty(), "a stopped group says nothing more");
}

#[test]
// A CIS LOST WHILE STREAMING winds the group down the same way: the ASEs released, the other CIS disconnected.
fn a_lost_cis_winds_the_group_down() {
	let members = [(0x40, found(location::FRONT_LEFT, 155, false)), (0x41, found(location::FRONT_RIGHT, 155, false))];
	let (mut group, _) = Group::start(&members, Purpose::Media).unwrap();
	group.on_ase(0x40, Direction::Sink, &codec_configured());
	group.on_ase(0x41, Direction::Sink, &codec_configured());
	group.on_cig(&[0x60, 0x61]);
	group.on_cis(0x60, 0);
	group.on_cis(0x61, 0);
	group.on_ase(0x40, Direction::Sink, &State::Streaming { cig: 1, cis: 0 });
	group.on_ase(0x41, Direction::Sink, &State::Streaming { cig: 1, cis: 1 });
	assert_eq!(group.on_cis_down(0x61), vec![Action::Member(0x40, Step::Release), Action::Member(0x41, Step::Release)]);
	group.on_ase(0x41, Direction::Sink, &State::Idle);
	assert_eq!(group.on_ase(0x40, Direction::Sink, &State::Releasing), vec![Action::Hci(DISCONNECT, vec![0x60, 0, REMOTE_USER_TERMINATED])]);
	assert_eq!(group.on_cis_down(0x60), vec![Action::Hci(le_iso::opcode::LE_REMOVE_CIG, vec![CIG]), Action::Stopped("a CIS was lost")]);
}

#[test]
// A CALL: mono voice to both, recorded from the first member with a source, whose CIS carries both ways and is made
// ready to receive.
fn a_call_records_from_one_member() {
	let members = [(0x40, found(location::FRONT_LEFT, 155, true)), (0x41, found(location::FRONT_RIGHT, 155, false))];
	let (mut group, actions) = Group::start(&members, Purpose::Voice).unwrap();
	let voice = Config::new(16_000, 10_000, 40);
	assert_eq!(actions[0], Action::Member(0x40, Step::Configure(Some(Config { allocation: location::FRONT_LEFT, ..voice }), Some(voice))));
	assert_eq!(actions[1], Action::Member(0x41, Step::Configure(Some(Config { allocation: location::FRONT_RIGHT, ..voice }), None)));
	group.on_ase(0x40, Direction::Sink, &codec_configured());
	group.on_ase(0x41, Direction::Sink, &codec_configured());
	let cig = group.on_ase(0x40, Direction::Source, &codec_configured());
	let [Action::Hci(_, parameters)] = &cig[..] else { panic!("the CIG") };
	assert_eq!(&parameters[15..20], &[0, 40, 0, 40, 0], "the recording member's CIS carries 40 bytes both ways");
	group.on_cig(&[0x60, 0x61]);
	let actions = group.on_cis(0x60, 0);
	assert!(actions.contains(&Action::Hci(le_iso::opcode::LE_SETUP_ISO_DATA_PATH, le_iso::setup_iso_data_path(0x60, le_iso::Direction::Output).to_vec())));
	assert!(actions.contains(&Action::Member(0x40, Step::ReceiverReady)));
}

#[test]
// NO COMMON CONFIGURATION, NO SINK, A FAILED CIS, A LOST MEMBER: the group says why.
fn the_group_says_why_it_cannot_stream() {
	let mut odd = found(location::FRONT_LEFT, 155, false);
	odd.sink = Some(Capabilities { frequencies: 0b1, durations: 0b01, channel_counts: 1, min_octets: 20, max_octets: 30, frames_per_sdu: 1 });
	assert_eq!(Group::start(&[(0x40, odd)], Purpose::Media).err(), Some("no LC3 configuration suits every member"));
	let silent = Found { sink: None, ..found(0, 155, true) };
	assert_eq!(Group::start(&[(0x40, silent)], Purpose::Media).err(), Some("no member of the device has a sink"));
	let (mut group, _) = Group::start(&[(0x40, found(location::FRONT_LEFT | location::FRONT_RIGHT, 155, false))], Purpose::Media).unwrap();
	group.on_ase(0x40, Direction::Sink, &codec_configured());
	group.on_cig(&[0x60]);
	assert_eq!(group.on_cis(0x60, 0x3e), vec![Action::Member(0x40, Step::Release)]);
	assert_eq!(group.on_ase(0x40, Direction::Sink, &State::Idle), vec![Action::Hci(le_iso::opcode::LE_REMOVE_CIG, vec![CIG]), Action::Stopped("a CIS was not established")]);
	let (mut pair, _) = Group::start(&[(0x40, found(location::FRONT_LEFT, 155, false)), (0x41, found(location::FRONT_RIGHT, 155, false))], Purpose::Media).unwrap();
	let lost = pair.on_link_lost(0x41);
	assert_eq!(lost, vec![Action::Stopped("a member's link was lost")], "nothing was configured: nothing to release, no CIG to remove");
}
