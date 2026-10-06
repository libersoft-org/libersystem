// The LE Audio world is a SEPARATE implementation from the host's, so its set functions are held to CSIS's own sample
// data here, its encodings to the layouts the specifications give, and its state machines and its controller to the
// transitions and refusals they name - before the two are ever made to agree in the guest.
use super::earbud::probe;
use super::*;
use crate::bt_le_world::{self, LeOut, LeWorld};
use crate::bt_peer::{f4, f5, f6};
use alloc::vec;

fn hex<const N: usize>(text: &str) -> [u8; N] {
	let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
	let mut out = [0u8; N];
	for (at, slot) in out.iter_mut().enumerate() {
		*slot = u8::from_str_radix(&clean[at * 2..at * 2 + 2], 16).expect("hex");
	}
	out
}

// ------------------------------------------------------------------ CSIS's sample data

#[test]
// CSIS APPENDIX A.1: sih over the sample SIRK and prand.
fn sih_meets_the_csis_sample_data() {
	let sirk: [u8; 16] = hex("457d7d09 21a1fd22 cecd8c86 dd72cccd");
	assert_eq!(sih(&sirk, [0x69, 0xf5, 0x63]), [0x19, 0x48, 0xda]);
}

#[test]
// CSIS APPENDIX A.2: the salt, k1 and sef over the sample SIRK and LTK - and sef undoing itself, which is sdf.
fn sef_meets_the_csis_sample_data() {
	let sirk: [u8; 16] = hex("457d7d09 21a1fd22 cecd8c86 dd72cccd");
	let ltk: [u8; 16] = hex("676e1b9b d448696f 061ec622 3ce5ced9");
	assert_eq!(salt(b"SIRKenc"), hex("6901983f 18149e82 3c7d133a 7d774572"));
	assert_eq!(k1(&ltk, &salt(b"SIRKenc"), b"csis"), hex("5277453c c094d982 b0e8ee53 2f2d1f8b"));
	let encrypted = sef(&ltk, &sirk);
	assert_eq!(encrypted, hex("170a3835 e13524a0 7e2562d5 f25fd346"));
	assert_eq!(sef(&ltk, &encrypted), sirk);
}

#[test]
// AN RSI IS hash || prand, least significant first, and it resolves under its set's key and no other.
fn a_resolvable_set_identifier_resolves_under_its_key() {
	let sirk: [u8; 16] = hex("457d7d09 21a1fd22 cecd8c86 dd72cccd");
	let rsi_bytes = rsi(&sirk, [0x69, 0xf5, 0x63]);
	assert_eq!(rsi_bytes, [0xda, 0x48, 0x19, 0x63, 0xf5, 0x69]);
	assert!(resolves(&sirk, &rsi_bytes));
	assert!(!resolves(&SIRK, &rsi_bytes));
	// The earbuds' own identifiers: `01` on top, the set's key resolving both, regenerated at a reset.
	let mut audio = Audio::new(7);
	let before = [audio.earbuds[0].rsi, audio.earbuds[1].rsi];
	for earbud in &audio.earbuds {
		assert_eq!(earbud.rsi[5] >> 6, 0b01);
		assert!(resolves(&SIRK, &earbud.rsi));
	}
	audio.reset();
	assert_ne!([audio.earbuds[0].rsi, audio.earbuds[1].rsi], before);
	assert!(resolves(&SIRK, &audio.earbuds[1].rsi));
}

// ------------------------------------------------------------------ the encodings

#[test]
// LE EXTENDED ADVERTISING REPORT (7.7.65.13), for a legacy ADV_IND: event type 0x0013, LE 1M, no secondary PHY, no SID,
// no TX power - and the legacy report of the same advertiser is the one the fixture always sent.
fn the_extended_report_has_the_core_layout() {
	let data = [0x02, 0x01, 0x06];
	let advert = Advert::legacy(0x01, [0xc0, 0xff, 0xee, 0x00, 0x00, 0x01], -60, &data);
	assert_eq!(advert.extended_report(), vec![1, 0x13, 0x00, 0x01, 0x01, 0x00, 0x00, 0xee, 0xff, 0xc0, 0x01, 0x00, 0xff, 0x7f, 0xc4, 0x00, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 3, 0x02, 0x01, 0x06]);
	assert_eq!(advert.legacy_report(), vec![1, 0x00, 0x01, 0x01, 0x00, 0x00, 0xee, 0xff, 0xc0, 3, 0x02, 0x01, 0x06, 0xc4]);
	assert!(advert.is_legacy());
}

#[test]
// BAP'S ANNOUNCEMENTS: the Broadcast_ID little-endian with the Broadcast Name, and the BASE as Table 3.15 lays it out.
fn the_broadcast_announcements_have_baps_layout() {
	let mut announcement = vec![0x06, 0x16, 0x52, 0x18, 0x56, 0x34, 0x12, 18, 0x30];
	announcement.extend_from_slice(b"fixture broadcast");
	assert_eq!(broadcast::announcement(), announcement);
	#[rustfmt::skip]
	let base = vec![
		0x40, 0x9c, 0x00, 1,
		2, 0x06, 0, 0, 0, 0, 10, 0x02, 0x01, 0x08, 0x02, 0x02, 0x01, 0x03, 0x04, 100, 0x00, 4, 0x03, 0x02, 0x04, 0x00,
		1, 6, 0x05, 0x03, 0x01, 0, 0, 0,
		2, 6, 0x05, 0x03, 0x02, 0, 0, 0,
	];
	assert_eq!(broadcast::base(), base);
	let data = broadcast::periodic_data();
	assert_eq!(&data[..4], &[base.len() as u8 + 3, 0x16, 0x51, 0x18]);
	assert_eq!(&data[4..], &base[..]);
}

#[test]
// ISO DATA PACKETS (5.4.5): a complete SDU out, PB 0b10 with the status flag valid; one in with and without a time stamp;
// fragments and lengths that disagree.
fn iso_packets_have_the_core_layout() {
	assert_eq!(iso_packet(0x0100, 7, &[1, 2, 3]), vec![0x00, 0x21, 7, 0, 7, 0, 3, 0, 1, 2, 3]);
	let packet = iso_in(&[0x00, 0x21, 7, 0, 9, 0, 3, 0, 1, 2, 3]).expect("a complete SDU");
	assert_eq!((packet.handle, packet.boundary, packet.sequence, packet.sdu), (0x0100, 0b10, 9, &[1u8, 2, 3][..]));
	let stamped = iso_in(&[0x01, 0x61, 11, 0, 0xaa, 0xbb, 0xcc, 0xdd, 4, 0, 3, 0, 1, 2, 3]).expect("a stamped SDU");
	assert_eq!((stamped.handle, stamped.sequence, stamped.sdu), (0x0101, 4, &[1u8, 2, 3][..]));
	assert!(iso_in(&[0x00, 0x21, 7, 0, 9, 0, 4, 0, 1, 2, 3]).is_none());
	assert!(iso_in(&[0x00, 0x21, 8, 0, 9, 0, 3, 0, 1, 2, 3]).is_none());
	let first = iso_in(&[0x00, 0x01, 6, 0, 1, 0, 9, 0, 1, 2]).expect("a first fragment");
	assert_eq!(first.boundary, 0b00);
}

#[test]
// A CODEC CONFIGURATION'S LTVs: read whole, refused malformed or incomplete.
fn a_codec_configuration_reads_its_ltvs() {
	let codec = codec_configuration(&[0x02, 0x01, 0x08, 0x02, 0x02, 0x01, 0x05, 0x03, 0x03, 0, 0, 0, 0x03, 0x04, 120, 0]).expect("a configuration");
	assert_eq!((codec.rate, codec.duration_us, codec.octets, codec.allocation, codec.channels(), codec.sdu()), (48_000, 10_000, 120, Some(3), 2, 240));
	assert!(codec_configuration(&[0x02, 0x01, 0x08, 0x03, 0x04, 120, 0]).is_err());
	assert!(codec_configuration(&[0x02, 0x01, 0x0e, 0x02, 0x02, 0x01, 0x03, 0x04, 120, 0]).is_err());
	assert!(codec_configuration(&[0x02, 0x01, 0x08, 0x02, 0x02, 0x01, 0x04, 0x04, 120, 0]).is_err());
	assert_eq!(ltvs(&[0x02, 0x01, 0x08, 0x05, 0x03, 0x01]), Err(0x03));
}

// ------------------------------------------------------------------ an earbud's server, driven by ATT

const LINK: EarLink = EarLink { handle: 0x0064, encrypted: true, ltk: Some([0x11; 16]) };

fn le_audio() -> Audio {
	let mut audio = Audio::new(7);
	audio.choose(true);
	audio.hci_reset();
	audio.take_log();
	audio
}

fn ctx() -> Ctx {
	Ctx { links: [Some(LINK), None], acls: vec![0x0064, 0x0060] }
}

fn att(audio: &mut Audio, pdu: &[u8]) -> Vec<Vec<u8>> {
	audio.att(0, pdu, &LINK).into_iter().map(|out| if let AudioOut::Att(0, pdu) = out { pdu } else { panic!("only ATT from the earbud") }).collect()
}

fn write(audio: &mut Audio, handle: u16, value: &[u8]) -> Vec<Vec<u8>> {
	let mut pdu = vec![0x12];
	pdu.extend_from_slice(&handle.to_le_bytes());
	pdu.extend_from_slice(value);
	att(audio, &pdu)
}

fn handle(audio: &Audio, uuid: u16) -> u16 {
	probe::value_handle(&audio.earbuds[0], uuid)
}

// The notifications of one value among an earbud's PDUs.
fn notified(pdus: &[Vec<u8>], handle: u16) -> Vec<Vec<u8>> {
	pdus.iter().filter(|pdu| pdu[0] == 0x1b && u16::from_le_bytes([pdu[1], pdu[2]]) == handle).map(|pdu| pdu[3..].to_vec()).collect()
}

fn events(outs: &[AudioOut]) -> Vec<Vec<u8>> {
	outs.iter().filter_map(|out| if let AudioOut::Event(bytes) = out { Some(bytes.clone()) } else { None }).collect()
}

fn subevents(outs: &[AudioOut], subevent: u8) -> Vec<Vec<u8>> {
	events(outs).into_iter().filter(|bytes| bytes[0] == 0x3e && bytes[2] == subevent).map(|bytes| bytes[3..].to_vec()).collect()
}

fn completion(outs: &[AudioOut]) -> Vec<u8> {
	events(outs).into_iter().find(|bytes| bytes[0] == 0x0e).map(|bytes| bytes[5..].to_vec()).expect("a command complete")
}

// The MTU BAP's client takes, and the ASE control point's and both endpoints' notifications on.
fn configured(audio: &mut Audio) -> (u16, u16, u16) {
	assert_eq!(att(audio, &[0x02, 0x40, 0x00]), vec![vec![0x03, 247, 0]]);
	let (sink, source, control) = (handle(audio, 0x2bc4), handle(audio, 0x2bc5), handle(audio, 0x2bc6));
	for value in [sink, source, control] {
		assert_eq!(write(audio, value + 1, &[0x01, 0x00]), vec![vec![0x13]]);
	}
	(sink, source, control)
}

// 48 kHz, 10 ms, front left, 120 octets.
const SINK_CONFIG: [u8; 16] = [0x02, 0x01, 0x08, 0x02, 0x02, 0x01, 0x05, 0x03, 0x01, 0, 0, 0, 0x03, 0x04, 120, 0];

fn config_codec(id: u8, config: &[u8]) -> Vec<u8> {
	let mut value = vec![0x01, 1, id, 0x02, 0x02, 0x06, 0, 0, 0, 0, config.len() as u8];
	value.extend_from_slice(config);
	value
}

fn config_qos(id: u8, cig: u8, cis: u8, interval: u32, max_sdu: u16, latency: u16, delay: u32) -> Vec<u8> {
	let mut value = vec![0x02, 1, id, cig, cis];
	value.extend_from_slice(&le24(interval));
	value.extend_from_slice(&[0x00, 0x02]);
	value.extend_from_slice(&max_sdu.to_le_bytes());
	value.push(2);
	value.extend_from_slice(&latency.to_le_bytes());
	value.extend_from_slice(&le24(delay));
	value
}

fn set_cig(cises: &[(u8, u16, u16)]) -> Vec<u8> {
	let mut params = vec![1];
	params.extend_from_slice(&le24(10_000));
	params.extend_from_slice(&le24(10_000));
	params.extend_from_slice(&[0, 0, 0, 20, 0, 20, 0, cises.len() as u8]);
	for &(id, c2p, p2c) in cises {
		params.push(id);
		params.extend_from_slice(&c2p.to_le_bytes());
		params.extend_from_slice(&p2c.to_le_bytes());
		params.extend_from_slice(&[0x02, 0x02, 2, 2]);
	}
	params
}

const TRANSPARENT_PATH: [u8; 9] = [0x00, 0x03, 0, 0, 0, 0, 0, 0, 0];

fn path(handle: u16, direction: u8) -> Vec<u8> {
	let mut params = handle.to_le_bytes().to_vec();
	params.push(direction);
	params.extend_from_slice(&TRANSPARENT_PATH);
	params.extend_from_slice(&[0]);
	params
}

#[test]
// THE SINK, END TO END: codec, QoS, enable, its CIS, the SDUs it hears, the CIS gone, released - each transition
// notified in the layout ASCS gives, and the lines a gate reads.
fn a_sink_streams_and_is_released() {
	let mut audio = le_audio();
	let (sink, _, control) = configured(&mut audio);
	let out = write(&mut audio, control, &config_codec(1, &SINK_CONFIG));
	assert_eq!(out[0], vec![0x13]);
	assert_eq!(notified(&out, control), vec![vec![0x01, 1, 1, 0x00, 0x00]]);
	let mut codec_configured = vec![1, 1, 0x00, 0x02, 2, 20, 0, 0x10, 0x27, 0x00, 0x40, 0x9c, 0x00, 0x20, 0x4e, 0x00, 0x40, 0x9c, 0x00, 0x06, 0, 0, 0, 0, 16];
	codec_configured.extend_from_slice(&SINK_CONFIG);
	assert_eq!(notified(&out, sink), vec![codec_configured]);
	let out = write(&mut audio, control, &config_qos(1, 1, 0, 10_000, 120, 20, 40_000));
	assert_eq!(notified(&out, sink), vec![vec![1, 2, 1, 0, 0x10, 0x27, 0x00, 0x00, 0x02, 120, 0, 2, 20, 0, 0x40, 0x9c, 0x00]]);
	let out = write(&mut audio, control, &[0x03, 1, 1, 4, 0x03, 0x02, 0x04, 0x00]);
	assert_eq!(notified(&out, sink), vec![vec![1, 3, 1, 0, 4, 0x03, 0x02, 0x04, 0x00]]);
	let lines = audio.take_log();
	assert!(lines.contains(&String::from("earbud L sink ASE is codec configured: 48000 Hz, 10000 us, 120 octets, location front left")));
	assert!(lines.contains(&String::from("earbud L sink ASE is QoS configured: CIG 1 CIS 0, SDU interval 10000 us, max SDU 120")));
	assert!(lines.contains(&String::from("earbud L sink ASE is enabling, contexts 0x0004")));

	// THE GROUP AND ITS STREAM: a handle for the CIS, the stream established, the sink streaming on its own.
	let out = audio.command(0x2062, &set_cig(&[(0, 120, 0)]), &ctx()).expect("set CIG");
	assert_eq!(completion(&out), vec![0, 1, 1, 0x00, 0x01]);
	let out = audio.command(0x2064, &[1, 0x00, 0x01, 0x64, 0x00], &ctx()).expect("create CIS");
	assert_eq!(events(&out)[0], vec![0x0f, 4, 0, 1, 0x64, 0x20]);
	let established = subevents(&out, 0x19);
	assert_eq!(established.len(), 1);
	assert_eq!(established[0].len(), 28);
	assert_eq!(&established[0][..3], &[0, 0x00, 0x01]);
	assert_eq!(u16::from_le_bytes([established[0][26], established[0][27]]), 8);
	let pdus: Vec<Vec<u8>> = out.iter().filter_map(|out| if let AudioOut::Att(0, pdu) = out { Some(pdu.clone()) } else { None }).collect();
	assert_eq!(notified(&pdus, sink), vec![vec![1, 4, 1, 0, 4, 0x03, 0x02, 0x04, 0x00]]);
	assert_eq!(audio.take_log(), vec![String::from("earbud L CIS is established"), String::from("earbud L sink ASE is streaming")]);

	// A GROUP WITH A STREAM UP IS NEITHER REMOVED NOR RECONFIGURED.
	assert_eq!(completion(&audio.command(0x2065, &[1], &ctx()).expect("remove CIG")), vec![0x0c, 1]);
	assert_eq!(completion(&audio.command(0x2062, &set_cig(&[(0, 120, 0)]), &ctx()).expect("set CIG")), vec![0x0c, 1, 0]);

	// THE SDUs: refused until the input path is set up, then heard - the fiftieth not silent says what it heard.
	let out = audio.command(0x206e, &path(0x0100, 0), &ctx()).expect("setup path");
	assert_eq!(completion(&out), vec![0, 0x00, 0x01]);
	let mut frame = [0u8; 120];
	crate::bt_lc3::write_tone(20, 48_000, 10_000, &mut frame).expect("a tone frame");
	let loudest = crate::bt_lc3::read(&frame, 48_000, 10_000).expect("the frame read").loudest.expect("a tone");
	for sequence in 0..50u16 {
		let out = audio.iso(&iso_packet(0x0100, sequence, &frame), &ctx());
		assert_eq!(events(&out), vec![vec![0x13, 5, 1, 0x00, 0x01, 1, 0]]);
	}
	assert_eq!(audio.take_log(), vec![heard_line('L', 48_000, 0, loudest)]);
	assert_eq!(heard_line('L', 48_000, 0, 20), "earbud L heard 50 frames of LC3 at 48000 Hz, every frame whole, the loudest line at 1000 Hz");

	// THE CIS DISCONNECTED: its completion, the sink back to QoS Configured.
	let out = audio.command(0x0406, &[0x00, 0x01, 0x13], &ctx()).expect("disconnect");
	assert_eq!(events(&out)[..2], [vec![0x0f, 4, 0, 1, 0x06, 0x04], vec![0x05, 4, 0, 0x00, 0x01, 0x16]]);
	assert_eq!(probe::state(&audio.earbuds[0], 1), 2);
	assert!(audio.take_log().contains(&String::from("earbud L CIS is disconnected")));
	assert_eq!(completion(&audio.command(0x2065, &[1], &ctx()).expect("remove CIG")), vec![0, 1]);

	// RELEASED with no CIS: Releasing, then Idle at once.
	let out = write(&mut audio, control, &[0x08, 1, 1]);
	assert_eq!(notified(&out, sink), vec![vec![1, 6], vec![1, 0]]);
	assert!(audio.take_log().contains(&String::from("earbud L sink ASE is released")));
}

#[test]
// THE REFUSALS ASCS NAMES: an unknown opcode, a length that does not add up, an unknown endpoint, a transition the state
// machine lacks, the wrong direction, a codec or configuration no record covers, and every QoS parameter out of line.
fn the_control_point_refuses_what_ascs_refuses() {
	let mut audio = le_audio();
	let (_, _, control) = configured(&mut audio);
	let answer = |audio: &mut Audio, value: &[u8]| notified(&write(audio, control, value), control).remove(0);
	assert_eq!(answer(&mut audio, &[0x09, 1, 1]), vec![0x09, 0xff, 0, 0x01, 0]);
	assert_eq!(answer(&mut audio, &[0x01, 1]), vec![0x01, 0xff, 0, 0x02, 0]);
	assert_eq!(answer(&mut audio, &[0x05, 0]), vec![0x05, 0xff, 0, 0x02, 0]);
	assert_eq!(answer(&mut audio, &[0x08, 1, 9]), vec![0x08, 1, 9, 0x03, 0]);
	assert_eq!(answer(&mut audio, &[0x03, 1, 1, 0]), vec![0x03, 1, 1, 0x04, 0]);
	assert_eq!(answer(&mut audio, &[0x04, 1, 1]), vec![0x04, 1, 1, 0x05, 0]);
	// 44.1 kHz is in no record; a transparent codec is not LC3; a configuration without its frame duration is malformed.
	assert_eq!(answer(&mut audio, &config_codec(1, &[0x02, 0x01, 0x07, 0x02, 0x02, 0x01, 0x03, 0x04, 120, 0])), vec![0x01, 1, 1, 0x06, 0]);
	let mut transparent = config_codec(1, &SINK_CONFIG);
	transparent[5] = 0x03;
	assert_eq!(answer(&mut audio, &transparent), vec![0x01, 1, 1, 0x07, 0x01]);
	assert_eq!(answer(&mut audio, &config_codec(1, &[0x02, 0x01, 0x08, 0x03, 0x04, 120, 0])), vec![0x01, 1, 1, 0x09, 0x02]);
	// Front right is not where the left earbud is; 200 octets is more than it takes.
	assert_eq!(answer(&mut audio, &config_codec(1, &[0x02, 0x01, 0x08, 0x02, 0x02, 0x01, 0x05, 0x03, 0x02, 0, 0, 0, 0x03, 0x04, 120, 0])), vec![0x01, 1, 1, 0x06, 0]);
	assert_eq!(answer(&mut audio, &config_codec(1, &[0x02, 0x01, 0x08, 0x02, 0x02, 0x01, 0x03, 0x04, 200, 0])), vec![0x01, 1, 1, 0x06, 0]);
	assert_eq!(probe::state(&audio.earbuds[0], 1), 0);
	// The microphone's record: 24 kHz and 40 octets yes, 48 kHz no.
	assert_eq!(answer(&mut audio, &config_codec(2, &[0x02, 0x01, 0x08, 0x02, 0x02, 0x01, 0x03, 0x04, 40, 0])), vec![0x01, 1, 2, 0x06, 0]);
	assert_eq!(answer(&mut audio, &config_codec(2, &[0x02, 0x01, 0x05, 0x02, 0x02, 0x01, 0x03, 0x04, 40, 0])), vec![0x01, 1, 2, 0, 0]);
	assert_eq!(answer(&mut audio, &config_codec(1, &SINK_CONFIG)), vec![0x01, 1, 1, 0, 0]);
	// QoS: a reserved CIG, an SDU interval the frames do not fill, an SDU they do not fit, a latency and a delay out of
	// range - and a sink and a microphone may share one CIS.
	assert_eq!(answer(&mut audio, &config_qos(1, 0xf0, 0, 10_000, 120, 20, 40_000)), vec![0x02, 1, 1, 0x09, 0x0a]);
	assert_eq!(answer(&mut audio, &config_qos(1, 1, 0, 7_500, 120, 20, 40_000)), vec![0x02, 1, 1, 0x07, 0x03]);
	assert_eq!(answer(&mut audio, &config_qos(1, 1, 0, 10_000, 100, 20, 40_000)), vec![0x02, 1, 1, 0x07, 0x06]);
	assert_eq!(answer(&mut audio, &config_qos(1, 1, 0, 10_000, 120, 3, 40_000)), vec![0x02, 1, 1, 0x09, 0x08]);
	assert_eq!(answer(&mut audio, &config_qos(1, 1, 0, 10_000, 120, 20, 50_000)), vec![0x02, 1, 1, 0x07, 0x09]);
	assert_eq!(answer(&mut audio, &config_qos(1, 1, 0, 10_000, 120, 20, 40_000)), vec![0x02, 1, 1, 0, 0]);
	assert_eq!(answer(&mut audio, &config_qos(2, 1, 0, 10_000, 40, 20, 40_000)), vec![0x02, 1, 2, 0, 0]);
	// Enable: a ringtone the sink is not available for, metadata that overruns, then media.
	assert_eq!(answer(&mut audio, &[0x03, 1, 1, 4, 0x03, 0x02, 0x00, 0x02]), vec![0x03, 1, 1, 0x0b, 0x02]);
	assert_eq!(answer(&mut audio, &[0x03, 1, 1, 3, 0x05, 0x02, 0x04]), vec![0x03, 1, 1, 0x0c, 0x02]);
	assert_eq!(answer(&mut audio, &[0x03, 1, 1, 4, 0x03, 0x02, 0x04, 0x00]), vec![0x03, 1, 1, 0, 0]);
	// THE MICROPHONE ENABLED, but its Receiver Start Ready refused while its CIS is not up; a configuration is refused
	// once enabled.
	assert_eq!(answer(&mut audio, &[0x03, 1, 2, 0]), vec![0x03, 1, 2, 0, 0]);
	assert_eq!(answer(&mut audio, &[0x04, 1, 2]), vec![0x04, 1, 2, 0x04, 0]);
	assert_eq!(answer(&mut audio, &config_codec(1, &SINK_CONFIG)), vec![0x01, 1, 1, 0x04, 0]);
	assert!(audio.take_log().contains(&String::from("earbud L source ASE is enabling, contexts 0x0001")));
	// TWO ENDPOINTS IN ONE WRITE, answered one by one.
	assert_eq!(answer(&mut audio, &[0x05, 2, 1, 2]), vec![0x05, 2, 1, 0, 0, 2, 0, 0]);
	assert_eq!((probe::state(&audio.earbuds[0], 1), probe::state(&audio.earbuds[0], 2)), (2, 5));
	assert_eq!(answer(&mut audio, &[0x06, 1, 2]), vec![0x06, 1, 2, 0, 0]);
	assert_eq!(answer(&mut audio, &[0x06, 1, 1]), vec![0x06, 1, 1, 0x05, 0]);
}

#[test]
// THE MICROPHONE: its CIS up and its output path set, Receiver Start Ready starts it, and it sends one SDU a tick - a few
// late ones caught up, never a flood.
fn the_microphone_streams_on_the_clock() {
	let mut audio = le_audio();
	let (_, source, control) = configured(&mut audio);
	write(&mut audio, control, &config_codec(2, &[0x02, 0x01, 0x05, 0x02, 0x02, 0x01, 0x05, 0x03, 0x01, 0, 0, 0, 0x03, 0x04, 40, 0]));
	write(&mut audio, control, &config_qos(2, 1, 0, 10_000, 40, 20, 40_000));
	write(&mut audio, control, &[0x03, 1, 2, 4, 0x03, 0x02, 0x02, 0x00]);
	audio.command(0x2062, &set_cig(&[(0, 0, 40)]), &ctx()).expect("set CIG");
	let out = audio.command(0x2064, &[1, 0x00, 0x01, 0x64, 0x00], &ctx()).expect("create CIS");
	assert_eq!(subevents(&out, 0x19)[0][0], 0);
	assert_eq!(completion(&audio.command(0x206e, &path(0x0100, 1), &ctx()).expect("setup path")), vec![0, 0x00, 0x01]);
	assert!(audio.tick(100, &ctx()).is_empty());
	let out = write(&mut audio, control, &[0x04, 1, 2]);
	assert_eq!(notified(&out, source)[0][..2], [2, 4]);
	assert!(audio.take_log().contains(&String::from("earbud L sends its microphone")));
	assert!(audio.active());
	let sdus = |out: &[AudioOut]| out.iter().filter_map(|out| if let AudioOut::Iso(packet) = out { Some(packet.clone()) } else { None }).collect::<Vec<_>>();
	let first = sdus(&audio.tick(101, &ctx()));
	assert_eq!(first.len(), 1);
	assert_eq!(&first[0][..8], &[0x00, 0x21, 44, 0, 0, 0, 40, 0]);
	let mut frame = [0u8; 40];
	crate::bt_lc3::write_tone(20, 24_000, 10_000, &mut frame).expect("a tone frame");
	assert_eq!(&first[0][8..], &frame);
	assert_eq!(sdus(&audio.tick(102, &ctx())).len(), 1);
	// LATE: three caught up, and the rest skipped.
	let late = sdus(&audio.tick(110, &ctx()));
	assert_eq!(late.len(), 3);
	assert_eq!(&late[2][4..6], &[4, 0]);
	assert_eq!(sdus(&audio.tick(111, &ctx())).len(), 1);
	// THE LINK DROPS: the CIS's completion first, and the endpoints go idle with it.
	let out = audio.acl_lost(0, 0x0064, 0x13);
	assert_eq!(events(&out), vec![vec![0x05, 4, 0, 0x00, 0x01, 0x13]]);
	assert_eq!(probe::state(&audio.earbuds[0], 2), 0);
	assert!(!audio.active());
}

#[test]
// CREATE CIS: a handle that names nothing and a stream already up are refused outright; a link to a device that is not
// an earbud, and an earbud with no endpoint waiting, are refused in the established event.
fn create_cis_refuses_what_it_cannot_establish() {
	let mut audio = le_audio();
	audio.command(0x2062, &set_cig(&[(0, 120, 0), (1, 120, 0)]), &ctx()).expect("set CIG");
	assert_eq!(events(&audio.command(0x2064, &[1, 0x09, 0x01, 0x64, 0x00], &ctx()).expect("create CIS"))[0][2], 0x02);
	assert_eq!(events(&audio.command(0x2064, &[1, 0x00, 0x01, 0x99, 0x00], &ctx()).expect("create CIS"))[0][2], 0x02);
	let out = audio.command(0x2064, &[2, 0x00, 0x01, 0x60, 0x00, 0x01, 0x01, 0x64, 0x00], &ctx()).expect("create CIS");
	let established = subevents(&out, 0x19);
	assert_eq!((established[0][0], established[1][0]), (0x1a, 0x0d));
	assert!(audio.take_log().is_empty());
	// SET CIG answers a handle for each stream, and a malformed one is refused.
	assert_eq!(completion(&audio.command(0x2062, &set_cig(&[(0, 120, 0), (1, 120, 0)]), &ctx()).expect("set CIG again")), vec![0, 1, 2, 0x00, 0x01, 0x01, 0x01]);
	assert_eq!(completion(&audio.command(0x2062, &set_cig(&[(0, 120, 0), (0, 120, 0)]), &ctx()).expect("set CIG twice the same")), vec![0x12, 1, 0]);
	assert_eq!(completion(&audio.command(0x2065, &[7], &ctx()).expect("remove CIG")), vec![0x02, 7]);
	// A DATA PATH needs an established CIS, and the host's codec.
	assert_eq!(completion(&audio.command(0x206e, &path(0x0100, 0), &ctx()).expect("setup path")), vec![0x0c, 0x00, 0x01]);
	let mut lc3 = path(0x0100, 0);
	lc3[4] = 0x06;
	assert_eq!(completion(&audio.command(0x206e, &lc3, &ctx()).expect("setup path")), vec![0x11, 0x00, 0x01]);
	assert_eq!(completion(&audio.command(0x206f, &[0x00, 0x01, 0x01], &ctx()).expect("remove path")), vec![0x0c, 0x00, 0x01]);
}

#[test]
// THE CONTROLLER'S KIND, and THE EXTENDED-MODE RULE: a plain LE controller knows no LE Audio command; an LE Audio one
// refuses the legacy scan and initiation once an extended command was sent, until a reset.
fn the_controller_kind_and_the_extended_mode_rule() {
	let mut audio = Audio::new(7);
	assert!(audio.command(0x2003, &[], &ctx()).is_none());
	assert!(audio.command(0x2041, &[0, 0, 1, 1, 0x10, 0, 0x10, 0], &ctx()).is_none());
	assert!(!audio.extended());
	audio.choose(true);
	assert!(audio.command(0x2003, &[], &ctx()).is_none());
	audio.hci_reset();
	assert_eq!(audio.take_log(), vec![String::from("controller will be an LE Audio one from its next reset"), String::from("controller reset as an LE Audio one")]);
	let features = completion(&audio.command(0x2003, &[], &ctx()).expect("features"));
	assert_eq!(features, vec![0, 0x01, 0x30, 0x00, 0x90, 0, 0, 0, 0]);
	assert_eq!(completion(&audio.command(0x2060, &[], &ctx()).expect("buffers")), vec![0, 251, 0, 4, 251, 0, 8]);
	assert!(audio.command(0x200c, &[1, 0], &ctx()).is_none());
	assert!(audio.command(0x2041, &[0, 0, 1, 1, 0x10, 0, 0x10, 0], &ctx()).is_none());
	assert_eq!(audio.extended_scan_parameters(&[0, 0, 1, 1, 0x10, 0, 0x10, 0]), 0);
	assert_eq!(audio.extended_scan_parameters(&[0, 0, 2, 1, 0x10, 0, 0x10, 0]), 0x12);
	assert_eq!(completion(&audio.command(0x200c, &[1, 0], &ctx()).expect("refused")), vec![0x0c]);
	assert_eq!(completion(&audio.command(0x200b, &[0; 7], &ctx()).expect("refused")), vec![0x0c]);
	assert_eq!(events(&audio.command(0x200d, &[0; 25], &ctx()).expect("refused")), vec![vec![0x0f, 4, 0x0c, 1, 0x0d, 0x20]]);
	// Not while scanning; a duration ends in a timeout.
	assert_eq!(audio.extended_scan_enable(&[1, 0, 5, 0, 0, 0], 40), (0, true));
	assert_eq!(audio.extended_scan_parameters(&[0, 0, 1, 1, 0x10, 0, 0x10, 0]), 0x0c);
	assert!(audio.tick(44, &ctx()).is_empty());
	assert_eq!(subevents(&audio.tick(45, &ctx()), 0x11), vec![Vec::<u8>::new()]);
	assert!(!audio.scanning_extended());
	audio.hci_reset();
	assert!(audio.command(0x200c, &[1, 0], &ctx()).is_none());
	audio.choose(false);
	audio.hci_reset();
	assert!(audio.command(0x2003, &[], &ctx()).is_none());
}

#[test]
// THE SET'S CHARACTERISTICS: the key encrypted under the bond's LTK - which sdf opens - the size and the rank, none of
// them on a link that is not encrypted.
fn the_set_key_is_read_encrypted_under_the_bond() {
	let mut audio = le_audio();
	let sirk = handle(&audio, 0x2b84);
	let value = att(&mut audio, &[0x0a, sirk as u8, (sirk >> 8) as u8]).remove(0);
	assert_eq!((value[0], value[1]), (0x0b, 0x00));
	let mut encrypted: [u8; 16] = value[2..].try_into().expect("sixteen octets");
	encrypted.reverse();
	assert_eq!(sef(&[0x11; 16], &encrypted), SIRK);
	let size = handle(&audio, 0x2b85);
	assert_eq!(att(&mut audio, &[0x0a, size as u8, 0]), vec![vec![0x0b, 2]]);
	let rank = probe::value_handle(&audio.earbuds[1], 0x2b87);
	let link = EarLink { handle: 0x0065, encrypted: true, ltk: Some([0x22; 16]) };
	let answer = audio.att(1, &[0x0a, rank as u8, 0], &link);
	assert_eq!(answer, vec![AudioOut::Att(1, vec![0x0b, 2])]);
	let plain = EarLink { encrypted: false, ..LINK };
	assert_eq!(audio.att(0, &[0x0a, sirk as u8, 0], &plain), vec![AudioOut::Att(0, vec![0x01, 0x0a, sirk as u8, 0, 0x0f])]);
	let stranger = EarLink { encrypted: false, ltk: None, ..LINK };
	assert_eq!(audio.att(0, &[0x0a, sirk as u8, 0], &stranger), vec![AudioOut::Att(0, vec![0x01, 0x0a, sirk as u8, 0, 0x05])]);
}

#[test]
// DISCOVERY: the services by group, PACS's records and the earbud's MTU.
fn the_earbud_is_discovered_as_a_gatt_server_is() {
	let mut audio = le_audio();
	assert_eq!(att(&mut audio, &[0x02, 0x00, 0x01]), vec![vec![0x03, 247, 0]]);
	let services = att(&mut audio, &[0x10, 0x01, 0x00, 0xff, 0xff, 0x00, 0x28]).remove(0);
	let uuids: Vec<u16> = services[2..].chunks(6).map(|entry| u16::from_le_bytes([entry[4], entry[5]])).collect();
	assert_eq!(uuids, vec![0x1800, 0x1850, 0x184e, 0x1846, 0x1844]);
	let ascs = att(&mut audio, &[0x06, 0x01, 0x00, 0xff, 0xff, 0x00, 0x28, 0x4e, 0x18]).remove(0);
	assert_eq!(ascs[0], 0x07);
	let pac = handle(&audio, 0x2bc9);
	#[rustfmt::skip]
	let record = vec![0x0b, 1, 0x06, 0, 0, 0, 0, 19,
		0x03, 0x01, 0xb4, 0x00, 0x02, 0x02, 0x02, 0x02, 0x03, 0x01, 0x05, 0x04, 26, 0, 155, 0, 0x02, 0x05, 1,
		4, 0x03, 0x01, 0x06, 0x00];
	assert_eq!(att(&mut audio, &[0x0a, pac as u8, 0]), vec![record]);
	let contexts = handle(&audio, 0x2bcd);
	assert_eq!(att(&mut audio, &[0x0a, contexts as u8, 0]), vec![vec![0x0b, 0x07, 0x00, 0x03, 0x00]]);
	// A NOTIFICATION LONGER THAN THE MTU ALLOWS is cut to it, as ATT cuts one: at the default 23, twenty octets of value.
	let mut fresh = le_audio();
	let (sink, control) = (handle(&fresh, 0x2bc4), handle(&fresh, 0x2bc6));
	write(&mut fresh, sink + 1, &[0x01, 0x00]);
	assert_eq!(notified(&write(&mut fresh, control, &config_codec(1, &SINK_CONFIG)), sink)[0].len(), 20);
	// The rest read from where the notification stopped: the codec's last four octets, the length, the configuration.
	let long = att(&mut fresh, &[0x0c, sink as u8, 0, 20, 0]).remove(0);
	assert_eq!(long, [&[0x0d, 0, 0, 0, 0, 16][..], &SINK_CONFIG[..]].concat());
	// THE RIGHT EARBUD has no microphone: no source record, no source endpoint, no source contexts.
	assert_eq!(probe::value_handle(&audio.earbuds[1], 0x2bcb), 0);
	assert_eq!(probe::value_handle(&audio.earbuds[1], 0x2bc5), 0);
}

#[test]
// THE VOLUME SERVER: an absolute set notified, a stale change counter and an opcode it lacks refused with VCS's errors,
// mute and unmute, and the earbud's own change.
fn the_volume_server_keeps_its_change_counter() {
	let mut audio = le_audio();
	let (state, control, flags) = (handle(&audio, 0x2b7d), handle(&audio, 0x2b7e), handle(&audio, 0x2b7f));
	write(&mut audio, state + 1, &[0x01, 0x00]);
	write(&mut audio, flags + 1, &[0x01, 0x00]);
	let out = write(&mut audio, control, &[0x04, 0, 204]);
	assert_eq!(out[0], vec![0x13]);
	assert_eq!(notified(&out, state), vec![vec![204, 0, 1]]);
	assert_eq!(notified(&out, flags), vec![vec![1]]);
	assert_eq!(write(&mut audio, control, &[0x04, 0, 10]), vec![vec![0x01, 0x12, control as u8, 0, 0x80]]);
	assert_eq!(write(&mut audio, control, &[0x07, 1]), vec![vec![0x01, 0x12, control as u8, 0, 0x81]]);
	assert_eq!(notified(&write(&mut audio, control, &[0x06, 1]), state), vec![vec![204, 1, 2]]);
	assert_eq!(notified(&write(&mut audio, control, &[0x03, 2]), state), vec![vec![220, 0, 3]]);
	let own = audio.own_volume(0, 100, Some(&LINK));
	assert_eq!(own, vec![AudioOut::Att(0, vec![0x1b, state as u8, 0, 100, 0, 4])]);
	let lines = audio.take_log();
	for line in ["earbud L volume was set to 204", "earbud L was muted", "earbud L volume was stepped to 220", "earbud L was unmuted", "earbud L changed its own volume to 100"] {
		assert!(lines.contains(&String::from(line)), "{line}");
	}
}

#[test]
// THE CALL BEARER CLIENT against a host's attribute server: services by group until the generic bearer, its
// characteristics, its descriptors, both notifications on, the call state read - then a call answered.
fn the_earbud_finds_the_hosts_call_bearer() {
	let mut audio = le_audio();
	let request = |out: Vec<AudioOut>| -> Vec<u8> {
		match out.as_slice() {
			[AudioOut::Att(0, pdu)] => pdu.clone(),
			other => panic!("one request, not {other:?}"),
		}
	};
	assert_eq!(request(audio.secured(0)), vec![0x10, 0x01, 0x00, 0xff, 0xff, 0x00, 0x28]);
	assert!(audio.secured(0).is_empty());
	let out = att(&mut audio, &[0x11, 6, 0x01, 0x00, 0x05, 0x00, 0x00, 0x18]);
	assert_eq!(out, vec![vec![0x10, 0x06, 0x00, 0xff, 0xff, 0x00, 0x28]]);
	let out = att(&mut audio, &[0x11, 6, 0x06, 0x00, 0x10, 0x00, 0x4c, 0x18]);
	assert_eq!(out, vec![vec![0x08, 0x06, 0x00, 0x10, 0x00, 0x03, 0x28]]);
	let out = att(&mut audio, &[0x09, 7, 0x07, 0x00, 0x12, 0x08, 0x00, 0xbd, 0x2b, 0x0a, 0x00, 0x1c, 0x0b, 0x00, 0xbe, 0x2b]);
	assert_eq!(out, vec![vec![0x08, 0x0b, 0x00, 0x10, 0x00, 0x03, 0x28]]);
	let out = att(&mut audio, &[0x01, 0x08, 0x0b, 0x00, 0x0a]);
	assert_eq!(out, vec![vec![0x04, 0x09, 0x00, 0x10, 0x00]]);
	let out = att(&mut audio, &[0x05, 1, 0x09, 0x00, 0x02, 0x29, 0x0a, 0x00, 0x03, 0x28, 0x0b, 0x00, 0xbe, 0x2b, 0x0c, 0x00, 0x02, 0x29]);
	assert_eq!(out, vec![vec![0x04, 0x0d, 0x00, 0x10, 0x00]]);
	let out = att(&mut audio, &[0x01, 0x04, 0x0d, 0x00, 0x0a]);
	assert_eq!(out, vec![vec![0x12, 0x09, 0x00, 0x01, 0x00]]);
	assert_eq!(att(&mut audio, &[0x13]), vec![vec![0x12, 0x0c, 0x00, 0x01, 0x00]]);
	assert_eq!(att(&mut audio, &[0x13]), vec![vec![0x0a, 0x08, 0x00]]);
	assert!(att(&mut audio, &[0x0b, 1, 0x00, 0x00]).is_empty());
	assert_eq!(audio.take_log(), vec![String::from("earbud L found the host's call bearer"), String::from("earbud L saw call state incoming")]);
	// THE CALL ANSWERED: the write, the host's answer, its state.
	assert_eq!(request(audio.call(0, 0).expect("a bearer")), vec![0x12, 0x0b, 0x00, 0x00, 1]);
	assert!(att(&mut audio, &[0x13]).is_empty());
	assert!(att(&mut audio, &[0x1b, 0x0b, 0x00, 0x00, 1, 0x00]).is_empty());
	assert!(att(&mut audio, &[0x1b, 0x08, 0x00, 1, 0x03, 0x00]).is_empty());
	assert!(att(&mut audio, &[0x1b, 0x08, 0x00, 1, 0x03, 0x00]).is_empty());
	assert_eq!(att(&mut audio, &[0x1d, 0x08, 0x00]), vec![vec![0x1e]]);
	let lines = audio.take_log();
	assert_eq!(&lines[1..], &[String::from("earbud L's call control was answered success"), String::from("earbud L saw call state active"), String::from("earbud L saw call state none")]);
	// THE RIGHT EARBUD, with no bearer found, cannot press.
	assert!(audio.call(1, 0).is_none());
}

#[test]
// THE BROADCAST: a sync waiting for a silent source, found when it starts encrypted; its reports; a BIG refused the wrong
// code and synchronized with the right one; its streams; and everything lost when it stops.
fn the_broadcast_is_found_synchronised_and_lost() {
	let mut audio = le_audio();
	let mut create = vec![0x00, broadcast::SID, 0x01];
	create.extend_from_slice(&[0x0e, 0x00, 0x20, 0xdc, 0x1b, 0xd0]);
	create.extend_from_slice(&[0, 0, 0xe8, 0x03, 0]);
	let out = audio.command(0x2044, &create, &ctx()).expect("create sync");
	assert_eq!(events(&out), vec![vec![0x0f, 4, 0, 1, 0x44, 0x20]]);
	assert_eq!(events(&audio.command(0x2044, &create, &ctx()).expect("create sync"))[0][2], 0x0c);
	let out = audio.broadcast(2).expect("a mode");
	let synced = subevents(&out, 0x0e);
	assert_eq!(synced, vec![vec![0, 0x01, 0x00, 1, 0x01, 0x0e, 0x00, 0x20, 0xdc, 0x1b, 0xd0, 0x01, 80, 0, 0x05]]);
	let report = subevents(&out, 0x0f).remove(0);
	assert_eq!(&report[..6], &[0x01, 0x00, 0x7f, 0xc9, 0xff, 0x00]);
	assert_eq!(&report[7..], &broadcast::periodic_data()[..]);
	let info = subevents(&out, 0x22).remove(0);
	assert_eq!(info, vec![0x01, 0x00, 2, 2, 8, 0, 1, 0, 2, 100, 0, 0x10, 0x27, 0x00, 100, 0, 0x02, 0x00, 1]);
	assert_eq!(audio.take_log(), vec![String::from("broadcast source is broadcasting encrypted"), String::from("broadcast source's periodic train was synchronised")]);
	// THE REPORTS AGAIN a hundred milliseconds on.
	assert!(audio.tick(5, &ctx()).is_empty());
	assert_eq!(subevents(&audio.tick(10, &ctx()), 0x22).len(), 1);
	// THE BIG: no code, then a wrong one - MIC failure and no handles; then the right one, two streams.
	let big = |code: &[u8; 16], encryption: u8| {
		let mut params = vec![0x05, 0x01, 0x00, encryption];
		params.extend_from_slice(code);
		params.extend_from_slice(&[0, 0xe8, 0x03, 2, 1, 2]);
		params
	};
	let out = audio.command(0x206b, &big(&[0; 16], 0), &ctx()).expect("BIG create sync");
	assert_eq!(subevents(&out, 0x1d), vec![vec![0x3d, 0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]]);
	let out = audio.command(0x206b, &big(b"fixture-code-017", 1), &ctx()).expect("BIG create sync");
	assert_eq!(subevents(&out, 0x1d)[0][0], 0x3d);
	let out = audio.command(0x206b, &big(&BROADCAST_CODE, 1), &ctx()).expect("BIG create sync");
	let established = subevents(&out, 0x1d).remove(0);
	assert_eq!((established[0], established[1], established[13]), (0, 0x05, 2));
	assert_eq!(&established[14..], &[0x80, 0x01, 0x81, 0x01]);
	let lines = audio.take_log();
	assert_eq!(lines.iter().filter(|line| *line == "broadcast source's BIG refused a wrong Broadcast Code").count(), 2);
	assert!(lines.contains(&String::from("broadcast source's BIG was synchronised: 2 streams")));
	// THE STREAMS: the right BIS's frames once its output path is set.
	assert_eq!(completion(&audio.command(0x206e, &path(0x0181, 0), &ctx()).expect("setup path")), vec![0x0c, 0x81, 0x01]);
	assert_eq!(completion(&audio.command(0x206e, &path(0x0181, 1), &ctx()).expect("setup path")), vec![0, 0x81, 0x01]);
	let out = audio.tick(11, &ctx());
	let packets: Vec<Vec<u8>> = out.iter().filter_map(|out| if let AudioOut::Iso(packet) = out { Some(packet.clone()) } else { None }).collect();
	assert_eq!(packets.len(), 1);
	let mut frame = [0u8; 100];
	crate::bt_lc3::write_tone(60, 48_000, 10_000, &mut frame).expect("a tone frame");
	assert_eq!(packets[0], iso_packet(0x0181, 0, &frame));
	// STOPPED: the BIG lost, then the train.
	let out = audio.broadcast(0).expect("a mode");
	assert_eq!(events(&out), vec![vec![0x3e, 3, 0x1e, 0x05, 0x13], vec![0x3e, 3, 0x10, 0x01, 0x00]]);
	assert_eq!(audio.take_log(), vec![String::from("broadcast source stopped")]);
	assert!(!audio.active());
	assert!(audio.broadcast(3).is_none());
}

#[test]
// A CREATE SYNC CANCELLED: its completion, then the sync's end with Operation Cancelled by Host; a cancel with nothing
// pending is refused.
fn a_create_sync_is_cancelled() {
	let mut audio = le_audio();
	let mut create = vec![0x00, broadcast::SID, 0x01];
	create.extend_from_slice(&[0x0e, 0x00, 0x20, 0xdc, 0x1b, 0xd0, 0, 0, 0xe8, 0x03, 0]);
	audio.command(0x2044, &create, &ctx()).expect("create sync");
	let out = audio.command(0x2045, &[], &ctx()).expect("cancel");
	assert_eq!(completion(&out), vec![0]);
	assert_eq!(subevents(&out, 0x0e)[0][0], 0x44);
	assert_eq!(completion(&audio.command(0x2045, &[], &ctx()).expect("cancel")), vec![0x0c]);
	assert_eq!(completion(&audio.command(0x2046, &[0x01, 0x00], &ctx()).expect("terminate")), vec![0x42]);
}

// ------------------------------------------------------------------ the controller, through the LE world

fn le_frames(outs: &[LeOut]) -> Vec<Vec<u8>> {
	outs.iter().filter_map(|out| if let LeOut::Acl(bytes) = out { Some(bytes[8..].to_vec()) } else { None }).collect()
}

fn le_events(outs: &[LeOut]) -> Vec<Vec<u8>> {
	outs.iter().filter_map(|out| if let LeOut::Event(bytes) = out { Some(bytes.clone()) } else { None }).collect()
}

fn l2cap(cid: u16, payload: &[u8]) -> Vec<u8> {
	let mut pdu = (payload.len() as u16).to_le_bytes().to_vec();
	pdu.extend_from_slice(&cid.to_le_bytes());
	pdu.extend_from_slice(payload);
	pdu
}

fn reversed16(bytes: &[u8]) -> [u8; 16] {
	let mut out: [u8; 16] = bytes.try_into().expect("sixteen octets");
	out.reverse();
	out
}

#[test]
// THE EARBUDS AS A SCAN HEARS THEM: on a plain controller in legacy PDUs that fit their thirty-one octets; on an LE Audio
// one in connectable extended advertising with the set identifier - and the earlier devices' legacy reports unchanged.
fn the_earbuds_advertise_as_the_controller_allows() {
	let mut le = LeWorld::new(7);
	let legacy = le.advertise();
	assert_eq!(legacy.len(), 5);
	let LeOut::Event(tag) = &legacy[0] else { panic!("an event") };
	let mut expected = vec![0x3e, 0, 0x02, 1, 0x00, 0x01];
	expected.extend_from_slice(&le.adverts()[0].address.iter().rev().copied().collect::<Vec<u8>>());
	expected.extend_from_slice(&[13, 12, 0x09]);
	expected.extend_from_slice(b"fixture tag");
	expected.push(0xc0);
	expected[1] = expected.len() as u8 - 2;
	assert_eq!(tag, &expected);
	let LeOut::Event(earbud) = &legacy[3] else { panic!("an event") };
	assert_eq!(earbud[12], 31);
	assert_eq!(&earbud[5..12], &[0x01, 0x0c, 0x00, 0x20, 0xdc, 0x1b, 0xd0]);
	le.choose_kind(true);
	le.hci_reset();
	assert!(le.take_log().contains(&String::from("controller reset as an LE Audio one")));
	assert_eq!(le.advertise().len(), 3);
	let reports = le_events(&le.extended_reports());
	assert_eq!(reports.len(), 5);
	let left = &reports[3];
	assert_eq!((left[2], u16::from_le_bytes([left[4], left[5]]), left[14], left[15]), (0x0d, 0x0001, 0x01, 0));
	let data = &left[28..];
	let rsi_ad = ltvs(data).expect("AD structures").into_iter().find(|ad| ad.0 == 0x2e).expect("an RSI").1;
	assert!(resolves(&SIRK, rsi_ad.try_into().expect("six octets")));
	assert!(ltvs(data).expect("AD structures").iter().any(|ad| ad.0 == 0x16 && ad.1[..3] == [0x4e, 0x18, 0x00]));
	// THE BROADCAST SOURCE is heard while it broadcasts, non-connectable with SID 1 and its train.
	le.broadcast(1).expect("a mode");
	let reports = le_events(&le.extended_reports());
	let source = reports.last().expect("the source");
	assert_eq!((u16::from_le_bytes([source[4], source[5]]), source[14], source[15], u16::from_le_bytes([source[18], source[19]])), (0x0000, 1, 1, 80));
}

#[test]
// THE LEFT EARBUD END TO END ON THE CONTROLLER: connected, paired with Just Works by Secure Connections, encrypted, its
// identity given, bonded - and its call bearer client starting; then a CIS up, and the link's drop taking the CIS's
// completion before its own.
fn the_left_earbud_bonds_streams_and_drops() {
	let mut le = LeWorld::new(7);
	le.choose_kind(true);
	le.hci_reset();
	let at = 4;
	let handle = 0x0064;
	let host = [0x00, 0x00, 0x1b, 0xdc, 0x00, 0x00, 0x01];
	let connected = le_events(&le.connect(at, host, false));
	assert_eq!(&connected[0][3..6], &[0, 0x64, 0x00]);
	let smp = |le: &mut LeWorld, pdu: &[u8]| le_frames(&le.acl(handle, &l2cap(0x0006, pdu)).expect("the earbud's link"));
	let request = [0x01, 0x03, 0x00, 0x09, 16, 0x00, 0x02];
	let response = smp(&mut le, &request);
	assert_eq!(response, vec![vec![0x02, 0x03, 0x00, 0x09, 16, 0x00, 0x02]]);
	let pka = [0x33u8; 64];
	let mut key = vec![0x0c];
	key.extend_from_slice(&pka);
	let answers = smp(&mut le, &key);
	assert_eq!(answers[0][..], [&[0x0c][..], &bt_le_world::public_key(at)[..]].concat()[..]);
	let na = [0x5au8; 16];
	let mut random = vec![0x04];
	random.extend_from_slice(&reversed16(&na));
	let nb = reversed16(&smp(&mut le, &random)[0][1..]);
	let own = [0x01, 0xd0, 0x1b, 0xdc, 0x20, 0x00, 0x0c];
	let (mac_key, ltk) = f5(&bt_le_world::dhkey(at), &na, &nb, &host, &own);
	let pkax: [u8; 32] = pka[..32].iter().rev().copied().collect::<Vec<u8>>().try_into().expect("32 octets");
	let pkbx: [u8; 32] = bt_le_world::public_key(at)[..32].iter().rev().copied().collect::<Vec<u8>>().try_into().expect("32 octets");
	let _ = f4(&pkbx, &pkax, &nb, 0);
	let ea = f6(&mac_key, &na, &nb, &[0; 16], &[request[3], request[2], request[1]], &host, &own);
	let mut check = vec![0x0d];
	check.extend_from_slice(&reversed16(&ea));
	assert_eq!(smp(&mut le, &check)[0][0], 0x0d);
	let out = le.encrypt(handle, [0; 8], 0, ltk).expect("the earbud's link");
	assert_eq!(le_events(&out)[0], vec![0x08, 4, 0, 0x64, 0x00, 1]);
	let frames = le_frames(&out);
	assert_eq!(frames[0][0], 0x08);
	assert_eq!(&frames[1][..2], &[0x09, 0x01]);
	assert_eq!(frames[2], vec![0x10, 0x01, 0x00, 0xff, 0xff, 0x00, 0x28]);
	let lines = le.take_log();
	assert!(lines.contains(&String::from("earbud L bonded")), "{lines:?}");

	// THE SINK ENABLED THROUGH THE LINK, its CIS created, then the ACL dropped by the device.
	let att = |le: &mut LeWorld, pdu: &[u8]| le_frames(&le.acl(handle, &l2cap(0x0004, pdu)).expect("the earbud's link"));
	let control = probe::value_handle(&le.audio.earbuds[0], 0x2bc6);
	let write = |control: u16, value: &[u8]| {
		let mut pdu = vec![0x12];
		pdu.extend_from_slice(&control.to_le_bytes());
		pdu.extend_from_slice(value);
		pdu
	};
	assert_eq!(att(&mut le, &write(control, &config_codec(1, &SINK_CONFIG))), vec![vec![0x13]]);
	att(&mut le, &write(control, &config_qos(1, 1, 0, 10_000, 120, 20, 40_000)));
	att(&mut le, &write(control, &[0x03, 1, 1, 0]));
	le.command(0x2062, &set_cig(&[(0, 120, 0)])).expect("set CIG");
	let out = le.command(0x2064, &[1, 0x00, 0x01, 0x64, 0x00]).expect("create CIS");
	assert_eq!(le_events(&out)[1][3], 0);
	let out = le.act_disconnect(EARBUD_L).expect("a link");
	assert_eq!(le_events(&out)[..2], [vec![0x05, 4, 0, 0x00, 0x01, 0x13], vec![0x05, 4, 0, 0x64, 0x00, 0x13]]);
	let lines = le.take_log();
	assert!(lines.contains(&String::from("earbud L CIS is disconnected")));
	assert_eq!(probe::state(&le.audio.earbuds[0], 1), 0);
}

// ------------------------------------------------------------------ the guest's session, and what ends it

// AN EARBUD L BONDED AND ENCRYPTED on a fresh LE Audio world: the exchange a host runs, Just Works by Secure Connections,
// and the host's identity given back.
fn bond_left(le: &mut LeWorld) {
	let (at, handle) = (4, 0x0064);
	let host = [0x00, 0x00, 0x1b, 0xdc, 0x00, 0x00, 0x01];
	le.connect(at, host, false);
	let smp = |le: &mut LeWorld, pdu: &[u8]| le_frames(&le.acl(handle, &l2cap(0x0006, pdu)).expect("the earbud's link"));
	let request = [0x01, 0x03, 0x00, 0x09, 16, 0x00, 0x02];
	smp(le, &request);
	let mut key = vec![0x0c];
	key.extend_from_slice(&[0x33u8; 64]);
	smp(le, &key);
	let na = [0x5au8; 16];
	let mut random = vec![0x04];
	random.extend_from_slice(&reversed16(&na));
	let nb = reversed16(&smp(le, &random)[0][1..]);
	let own = [0x01, 0xd0, 0x1b, 0xdc, 0x20, 0x00, 0x0c];
	let (mac_key, ltk) = f5(&bt_le_world::dhkey(at), &na, &nb, &host, &own);
	let ea = f6(&mac_key, &na, &nb, &[0; 16], &[request[3], request[2], request[1]], &host, &own);
	let mut check = vec![0x0d];
	check.extend_from_slice(&reversed16(&ea));
	smp(le, &check);
	le.encrypt(handle, [0; 8], 0, ltk).expect("the earbud's link");
	le.acl(handle, &l2cap(0x0006, &[0x08; 17])).expect("the earbud's link");
	le.acl(handle, &l2cap(0x0006, &[0x09, 0x00, 1, 0, 0, 0xdc, 0x1b, 0x00])).expect("the earbud's link");
}

fn write_request(handle: u16, value: &[u8]) -> Vec<u8> {
	let mut pdu = vec![0x12];
	pdu.extend_from_slice(&handle.to_le_bytes());
	pdu.extend_from_slice(value);
	pdu
}

// THE GUEST'S SESSION as the LE Audio gate drives it: the controller made an LE Audio one, an extended scan, earbud L
// bonded and its sink streaming on CIS 0 of CIG 1, a broadcast in the clear joined - its train and both BISes - and the
// host's SDUs every tick. What one tick of it owes is returned for the caller to keep driving.
fn session() -> (LeWorld, u16) {
	let mut le = LeWorld::new(7);
	le.choose_kind(true);
	le.hci_reset();
	let scan = [0, 0, 1, 1, 0x10, 0, 0x10, 0];
	assert!(le.command(0x2041, &scan).is_none());
	assert_eq!(le.extended_scan_parameters(&scan), 0);
	assert_eq!(le.extended_scan_enable(&[1, 0, 0xf4, 0x01, 0, 0], 10), (0, true));
	bond_left(&mut le);
	let handle = 0x0064;
	let att = |le: &mut LeWorld, pdu: &[u8]| le_frames(&le.acl(handle, &l2cap(0x0004, pdu)).expect("the earbud's link"));
	att(&mut le, &[0x02, 0xf7, 0x00]);
	let (sink, control) = (probe::value_handle(&le.audio.earbuds[0], 0x2bc4), probe::value_handle(&le.audio.earbuds[0], 0x2bc6));
	for value in [sink, control] {
		att(&mut le, &write_request(value + 1, &[1, 0]));
	}
	att(&mut le, &write_request(control, &config_codec(1, &SINK_CONFIG)));
	att(&mut le, &write_request(control, &config_qos(1, 1, 0, 10_000, 120, 20, 40_000)));
	att(&mut le, &write_request(control, &[0x03, 1, 1, 4, 0x03, 0x02, 0x04, 0x00]));
	le.command(0x2062, &set_cig(&[(0, 120, 0), (1, 120, 0)])).expect("set CIG");
	le.command(0x2064, &[1, 0x00, 0x01, 0x64, 0x00]).expect("create CIS");
	le.command(0x206e, &path(0x0100, 0)).expect("a path");
	le.broadcast(1).expect("a mode");
	let mut create = vec![0x00, broadcast::SID, 0x01];
	create.extend_from_slice(&[0x0e, 0x00, 0x20, 0xdc, 0x1b, 0xd0, 0, 0, 0xe8, 0x03, 0]);
	le.command(0x2044, &create).expect("create sync");
	let mut big = vec![0x05, 0x01, 0x00, 0x00];
	big.extend_from_slice(&[0; 16]);
	big.extend_from_slice(&[0, 0xe8, 0x03, 2, 1, 2]);
	le.command(0x206b, &big).expect("BIG create sync");
	le.command(0x206e, &path(0x0180, 1)).expect("a path");
	le.command(0x206e, &path(0x0181, 1)).expect("a path");
	le.take_log();
	(le, control)
}

// AN INDEPENDENT ENCODER'S TONE: five consecutive frames of 1500 Hz from liblc3 (Google's LC3, written apart from both
// the host's codec and the fixture's), 48 kHz, 10 ms, 120 octets - what a host's encoder sends an earbud.
const LIBLC3_1500_HZ: [&str; 5] = [
	"afdecf91f515c6f9fd944f0bb11bdc4c188e6a252e65a11ab788fb818132d951b80ec5f551e50e6b30bd7a14012cb900000c3ebe04b4cf000b00000000cc7f8cfc6e2d8c1c248a38fef90e3d737398aba635f62a75637a49f4c2c4c641fb83b18c97c963bd0127ea5a69cc560228f6826693916d9a59d614",
	"afdecf91f515c6f9fd944f0bb11bdc4c188e6a252e65a11ab788fb818132d951b80ec5f551e50e6b30bd7a14012cb900000c3ebe04b4cf000b00000000cc7f8cfc6e2d8c1c248a38fef90e3d737398aba635f62a75637a49f4c2c4c641fb83b18c97c963bd0127ea5a69cc560228f6826693916d9a59d614",
	"afdecf91f515c6f9fd944f0bb11bdc4c188e6a252e65a11ab788fb818132d951b80ec5f551e50e6b30bd7a14012cb900000c3ebe04b4cf000b00000000cc7f8cfc6e2d8c1c248a38fef90e3d737398aba635f62a75637a49f4c2c4c641fb83b18c97c963bd0127ea5a69cc560228f6826693916d9a59d614",
	"afdecf91f515c6f9fd944f0bb11bdc4c188e6a252e65a11ab788fb818132d951b80ec5f551e50e6b30bd7a14012cb900000c3ebe04b4cf000b00000000cc7f8cfc6e2d8c1c248a38fef90e3d737398aba635f62a75637a49f4c2c4c641fb83b18c97c963bd0127ea5a69cc560228f6826693916d9a59d614",
	"afdecf91f515c6f9fd944f0bb11bdc4c188e6a252e65a11ab788fb818132d951b80ec5f551e50e6b30bd7a14012cb900000c3ebe04b4cf000b00000000cc7f8cfc6e2d8c1c248a38fef90e3d737398aba635f62a75637a49f4c2c4c641fb83b18c97c963bd0127ea5a69cc560228f6826693916d9a59d614",
];

fn liblc3_frames() -> Vec<Vec<u8>> {
	LIBLC3_1500_HZ.iter().map(|text| (0..text.len() / 2).map(|at| u8::from_str_radix(&text[at * 2..at * 2 + 2], 16).expect("hex")).collect()).collect()
}

#[test]
// THE GUEST'S SESSION RUN LONG: a minute of the host's SDUs - an independent encoder's frames and the fixture's own -
// with the clock stalling now and then, the scan ending on its duration and enabled again, the broadcast's BISes and its
// train's reports all the while. The earbud says what it heard once, and the streams keep their pace.
fn the_session_runs_a_minute_with_its_clock_stalling() {
	let (mut le, _) = session();
	let mut frames = liblc3_frames();
	for line in [20u16, 29, 30, 60] {
		let mut frame = vec![0u8; 120];
		crate::bt_lc3::write_tone(line, 48_000, 10_000, &mut frame).expect("a frame");
		frames.push(frame);
	}
	let (mut now, mut iso, mut timeouts, mut heard) = (100u64, 0usize, 0usize, Vec::new());
	for step in 0..6_000u32 {
		now += if step % 997 == 0 { 40 } else { 1 };
		if step % 1500 == 1499 {
			le.extended_scan_enable(&[1, 0, 0xf4, 0x01, 0, 0], now);
			le.extended_reports();
		}
		le.iso(&iso_packet(0x0100, step as u16, &frames[(step as usize / 50) % frames.len()]));
		let out = le.tick(now);
		iso += out.iter().filter(|out| matches!(out, LeOut::Iso(_))).count();
		timeouts += le_events(&out).iter().filter(|event| event[0] == 0x3e && event[2] == 0x11).count();
		heard.extend(le.take_log().into_iter().filter(|line| line.contains(" heard ")));
	}
	assert!(iso >= 2 * 5_900, "{iso}");
	assert!(timeouts >= 3, "{timeouts}");
	assert_eq!(heard.len(), 1, "{heard:?}");
	assert!(heard[0].starts_with("earbud L heard 50 frames of LC3 at 48000 Hz, every frame whole, the loudest line at 1"), "{}", heard[0]);
}

#[test]
// THE BROADCAST ROUTE ENDS MID-STREAM, as a host ends it when the session that asked for it goes: the BISes' paths
// removed, the BIG and the train let go, then the earbud's stream disabled, its CIS taken down, its group removed and
// its endpoint released - while SDUs still arrive and the clock runs. And the source stopping after all of it.
fn the_broadcast_route_ends_mid_stream() {
	let (mut le, control) = session();
	let handle = 0x0064;
	let mut now = 1_000u64;
	let mut frame = [0u8; 120];
	crate::bt_lc3::write_tone(29, 48_000, 10_000, &mut frame).expect("a frame");
	let run = |le: &mut LeWorld, ticks: u32, now: &mut u64| {
		for _ in 0..ticks {
			*now += 1;
			le.iso(&iso_packet(0x0100, *now as u16, &frame));
			le.tick(*now);
		}
	};
	run(&mut le, 200, &mut now);
	assert!(le.audio_active());
	for (opcode, params) in [(0x206fu16, vec![0x80, 0x01, 0x02]), (0x206f, vec![0x81, 0x01, 0x02]), (0x206c, vec![0x05]), (0x2046, vec![0x01, 0x00]), (0x2042, vec![0, 0, 0, 0, 0, 0])] {
		if opcode == 0x2042 {
			assert_eq!(le.extended_scan_enable(&params, now), (0, false));
			continue;
		}
		let out = le.command(opcode, &params).expect("an LE Audio command");
		assert_eq!(le_events(&out)[0][5], 0, "{opcode:#06x}");
		run(&mut le, 3, &mut now);
	}
	assert_eq!(le_frames(&le.acl(handle, &l2cap(0x0004, &write_request(control, &[0x05, 1, 1]))).expect("a link"))[0], vec![0x13]);
	run(&mut le, 3, &mut now);
	let out = le.command(0x0406, &[0x00, 0x01, 0x13]).expect("a CIS");
	assert_eq!(le_events(&out)[1], vec![0x05, 4, 0, 0x00, 0x01, 0x16]);
	run(&mut le, 3, &mut now);
	assert_eq!(le_events(&le.command(0x206f, &[0x00, 0x01, 0x01]).expect("a CIS"))[0][5], 0x0c);
	assert_eq!(le_events(&le.command(0x2065, &[1]).expect("a CIG"))[0][5], 0);
	le.acl(handle, &l2cap(0x0004, &write_request(control, &[0x08, 1, 1])));
	assert_eq!(probe::state(&le.audio.earbuds[0], 1), 0);
	run(&mut le, 50, &mut now);
	assert!(!le.audio_active());
	let out = le.broadcast(0).expect("a mode");
	assert!(le_events(&out).is_empty());
	assert!(le.take_log().contains(&String::from("broadcast source stopped")));
}

#[test]
// THE EARBUD'S VERDICT IS GIVEN ONCE, and after it the earbud reads no more of the stream: an SDU it could not have read
// whole changes nothing it said.
fn the_judge_reads_nothing_after_its_verdict() {
	let mut audio = le_audio();
	let (_, _, control) = configured(&mut audio);
	write(&mut audio, control, &config_codec(1, &SINK_CONFIG));
	write(&mut audio, control, &config_qos(1, 1, 0, 10_000, 120, 20, 40_000));
	write(&mut audio, control, &[0x03, 1, 1, 0]);
	audio.command(0x2062, &set_cig(&[(0, 120, 0)]), &ctx()).expect("set CIG");
	audio.command(0x2064, &[1, 0x00, 0x01, 0x64, 0x00], &ctx()).expect("create CIS");
	audio.command(0x206e, &path(0x0100, 0), &ctx()).expect("a path");
	audio.take_log();
	let frames = liblc3_frames();
	for sequence in 0..50u16 {
		audio.iso(&iso_packet(0x0100, sequence, &frames[usize::from(sequence) % frames.len()]), &ctx());
	}
	// LIBLC3'S 1500 Hz, judged by the fixture's own reader: every frame whole, its energy in line 29 or 30.
	let lines = audio.take_log();
	assert_eq!(lines.len(), 1, "{lines:?}");
	assert!(lines[0] == heard_line('L', 48_000, 0, 29) || lines[0] == heard_line('L', 48_000, 0, 30), "{}", lines[0]);
	for sequence in 50..60u16 {
		audio.iso(&iso_packet(0x0100, sequence, &[0xff; 7]), &ctx());
	}
	assert!(audio.take_log().is_empty());
}

#[test]
// A HOST WHOSE GENERIC BEARER IS ITS LAST SERVICE, ending at 0xffff as many servers say it: the earbud finds its call
// state and control point and turns both on, with no handle counted past the end.
fn a_bearer_ending_at_the_last_handle_is_found() {
	let mut audio = le_audio();
	audio.secured(0);
	assert_eq!(att(&mut audio, &[0x11, 6, 0x10, 0x00, 0xff, 0xff, 0x4c, 0x18]), vec![vec![0x08, 0x10, 0x00, 0xff, 0xff, 0x03, 0x28]]);
	let out = att(&mut audio, &[0x09, 7, 0x11, 0x00, 0x12, 0x12, 0x00, 0xbd, 0x2b, 0x14, 0x00, 0x1c, 0x15, 0x00, 0xbe, 0x2b]);
	assert_eq!(out, vec![vec![0x08, 0x15, 0x00, 0xff, 0xff, 0x03, 0x28]]);
	assert_eq!(att(&mut audio, &[0x01, 0x08, 0x15, 0x00, 0x0a]), vec![vec![0x04, 0x13, 0x00, 0xff, 0xff]]);
	let out = att(&mut audio, &[0x05, 1, 0x13, 0x00, 0x02, 0x29, 0x14, 0x00, 0x03, 0x28, 0x15, 0x00, 0xbe, 0x2b, 0x16, 0x00, 0x02, 0x29]);
	assert_eq!(out, vec![vec![0x04, 0x17, 0x00, 0xff, 0xff]]);
	assert_eq!(att(&mut audio, &[0x01, 0x04, 0x17, 0x00, 0x0a]), vec![vec![0x12, 0x13, 0x00, 0x01, 0x00]]);
	assert_eq!(att(&mut audio, &[0x13]), vec![vec![0x12, 0x16, 0x00, 0x01, 0x00]]);
	assert_eq!(att(&mut audio, &[0x13]), vec![vec![0x0a, 0x12, 0x00]]);
	assert!(audio.take_log().contains(&String::from("earbud L found the host's call bearer")));
}

#[test]
// EVERYTHING A HOST COULD SEND, at random, for a while: every LE command 0x2000-0x207f and a Disconnect with random
// parameters, ATT requests of every kind at every handle of both earbuds, ISO on every handle, the actions, scans,
// links dropped and taken again, controller resets of either kind - the clock jumping. Nothing here may panic.
fn random_hosts_do_not_bring_the_controller_down() {
	for seed in 1..=12u64 {
		let mut random = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0xdead_beef;
		let mut next = move || {
			random ^= random >> 12;
			random ^= random << 25;
			random ^= random >> 27;
			random.wrapping_mul(0x2545_F491_4F6C_DD1D)
		};
		let mut le = LeWorld::new(seed);
		le.choose_kind(true);
		le.hci_reset();
		bond_left(&mut le);
		let host = [0x00, 0x00, 0x1b, 0xdc, 0x00, 0x00, 0x01];
		le.connect(5, host, false);
		le.encrypt(0x0065, [0; 8], 0, [7; 16]);
		let mut now = 100u64;
		for _ in 0..8_000u32 {
			now += 1 + if next() % 100 == 0 { next() % 1000 } else { 0 };
			let handle = [0x0064u16, 0x0065][(next() % 2) as usize];
			match next() % 12 {
				0 | 1 => {
					let opcode = if next() % 8 == 0 { 0x0406 } else { 0x2000 + (next() % 0x80) as u16 };
					let mut params: Vec<u8> = (0..next() % 40).map(|_| next() as u8).collect();
					if opcode == 0x0406 && params.len() >= 2 {
						let target = [0x0100u16, 0x0101, 0x0180, 0x0181, 0x0064][(next() % 5) as usize];
						params[..2].copy_from_slice(&target.to_le_bytes());
					}
					le.command(opcode, &params);
				}
				2..=4 => {
					let opcode = [0x02, 0x04, 0x06, 0x08, 0x0a, 0x0c, 0x10, 0x12, 0x52, 0x16, 0x18, 0x0e, 0x20, 0x01, 0x05, 0x09, 0x0b, 0x11, 0x13, 0x1b, 0x1d][(next() % 21) as usize];
					let from = (next() % 50) as u16;
					let mut pdu = vec![opcode];
					pdu.extend_from_slice(&from.to_le_bytes());
					if next() % 2 == 0 {
						pdu.extend_from_slice(&(from + (next() % 50) as u16).to_le_bytes());
					}
					pdu.extend((0..next() % 30).map(|_| next() as u8));
					le.acl(handle, &l2cap(0x0004, &pdu));
				}
				5 => {
					let target = [0x0100u16, 0x0101, 0x0180, 0x0181, 0x0064][(next() % 5) as usize];
					let sdu: Vec<u8> = (0..next() % 200).map(|_| next() as u8).collect();
					let mut packet = iso_packet(target, next() as u16, &sdu);
					if next() % 4 == 0 {
						packet[1] ^= (next() as u8) & 0x70;
					}
					le.iso(&packet);
				}
				6 => {
					le.broadcast((next() % 3) as u8);
				}
				7 => {
					le.earbud_volume([EARBUD_L, EARBUD_R][(next() % 2) as usize], next() as u8);
					le.earbud_call([EARBUD_L, EARBUD_R][(next() % 2) as usize], (next() % 3) as u8);
				}
				8 => {
					le.extended_scan_enable(&[(next() % 2) as u8, 0, (next() % 4) as u8, 0, 0, 0], now);
					le.extended_reports();
					le.advertise();
				}
				9 if next() % 10 == 0 => {
					let device = [EARBUD_L, EARBUD_R][(next() % 2) as usize];
					le.act_disconnect(device);
					le.connect(usize::from(device - 8), host, false);
					le.encrypt(0x0060 + u16::from(device - 8), [0; 8], 0, [(next() % 2) as u8; 16]);
				}
				10 if next() % 50 == 0 => {
					le.choose_kind(next() % 3 != 0);
					le.hci_reset();
					bond_left(&mut le);
				}
				11 => {
					let params = match next() % 5 {
						0 => set_cig(&[(0, (next() % 200) as u16, (next() % 60) as u16), (1, 120, 0)]),
						1 => vec![1, (0x0100 + next() % 2) as u8, 0x01, handle as u8, 0x00],
						2 => path([0x0100u16, 0x0101, 0x0180, 0x0181][(next() % 4) as usize], (next() % 2) as u8),
						3 => {
							let mut create = vec![0x00, broadcast::SID, 0x01];
							create.extend_from_slice(&[0x0e, 0x00, 0x20, 0xdc, 0x1b, 0xd0, 0, 0, 0xe8, 0x03, 0]);
							create
						}
						_ => {
							let mut big = vec![0x05, (next() % 3) as u8, 0x00, (next() % 2) as u8];
							big.extend_from_slice(&BROADCAST_CODE);
							big.extend_from_slice(&[0, 0xe8, 0x03, 2, 1, 2]);
							big
						}
					};
					let opcode = match params.len() {
						27 => 0x2062,
						5 => 0x2064,
						13 => 0x206e,
						14 => 0x2044,
						_ => 0x206b,
					};
					le.command(opcode, &params);
				}
				_ => {}
			}
			le.tick(now);
			le.take_log();
		}
	}
}
