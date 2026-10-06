use super::*;
use crate::le_audio::location;
use alloc::vec;

#[test]
// THE STATES, each parsed from the parameters ASCS lays out for it.
fn ase_states_are_read_with_their_parameters() {
	assert_eq!(parse_ase(&[1, state::IDLE]), Some(Ase { id: 1, state: State::Idle }));
	let config = Config { allocation: location::FRONT_LEFT, ..Config::new(48_000, 10_000, 120) };
	let field = config.encode();
	let mut configured = vec![1, state::CODEC_CONFIGURED, 0, 2, 2, 20, 0, 0x10, 0x27, 0, 0x40, 0x9c, 0, 0x10, 0x27, 0, 0x40, 0x9c, 0];
	configured.extend_from_slice(&le_audio::LC3_ID);
	configured.push(field.len() as u8);
	configured.extend_from_slice(&field);
	let parsed = parse_ase(&configured).unwrap();
	assert_eq!(parsed.state, State::CodecConfigured { preferences: Preferences { retransmissions: 2, max_latency_ms: 20, delay_min_us: 10_000, delay_max_us: 40_000 }, config: Some(config) });
	assert_eq!(parse_ase(&configured[..configured.len() - 1]), None);
	let qos = [2, state::QOS_CONFIGURED, 1, 0, 0x10, 0x27, 0, 0, 2, 120, 0, 2, 20, 0, 0x40, 0x9c, 0];
	assert_eq!(parse_ase(&qos).map(|ase| ase.state), Some(State::QosConfigured { cig: 1, cis: 0 }));
	assert_eq!(parse_ase(&[2, state::STREAMING, 1, 0, 0]).map(|ase| ase.state), Some(State::Streaming { cig: 1, cis: 0 }));
	assert_eq!(parse_ase(&[2, 9]), None, "a state ASCS does not have");
}

#[test]
// THE OPERATIONS, byte for byte, and the control point's answer.
fn operations_are_written_and_answers_read() {
	let config = Config::new(16_000, 10_000, 40);
	let codec = config_codec(&[(1, LOW_LATENCY, config)]);
	assert_eq!(&codec[..10], &[opcode::CONFIG_CODEC, 1, 1, LOW_LATENCY, PHY_2M, 6, 0, 0, 0, 0]);
	assert_eq!(&codec[11..], &config.encode()[..]);
	let qos = config_qos(&[Qos { ase: 1, cig: 1, cis: 0, sdu_interval_us: 10_000, max_sdu: 40, retransmissions: 2, max_latency_ms: 20, presentation_delay_us: 40_000 }]);
	assert_eq!(qos, [opcode::CONFIG_QOS, 1, 1, 1, 0, 0x10, 0x27, 0, 0, 2, 40, 0, 2, 20, 0, 0x40, 0x9c, 0]);
	assert_eq!(enable(&[(1, le_audio::context::MEDIA)]), [opcode::ENABLE, 1, 1, 4, 3, 2, 4, 0]);
	assert_eq!(ases_only(opcode::RELEASE, &[1, 2]), [opcode::RELEASE, 2, 1, 2]);
	assert_eq!(parse_response(&[opcode::CONFIG_CODEC, 2, 1, 0, 0, 2, 3, 1]), Some(Response { opcode: 1, ases: vec![(1, 0, 0), (2, 3, 1)] }));
	assert_eq!(parse_response(&[9, 0xff, 0, 1, 0]), Some(Response { opcode: 9, ases: vec![(0, 1, 0)] }));
	assert_eq!(parse_response(&[1, 2, 1, 0]), None);
}
