use super::*;
use alloc::vec;

fn hex(text: &str) -> [u8; 16] {
	let digits: Vec<u8> = text.bytes().filter(|byte| byte.is_ascii_hexdigit()).collect();
	let mut out = [0u8; 16];
	for (index, pair) in digits.chunks(2).enumerate() {
		out[index] = u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap();
	}
	out
}

#[test]
// CSIS'S OWN SAMPLE DATA: sih (A.1) and sef (A.2), and sdf undoing sef.
fn the_set_functions_meet_the_specifications_sample_data() {
	let sirk = hex("457d7d09 21a1fd22 cecd8c86 dd72cccd");
	assert_eq!(sih(&sirk, 0x0069_f563), 0x0019_48da);
	let ltk = hex("676e1b9b d448696f 061ec622 3ce5ced9");
	assert_eq!(s1(b"SIRKenc"), hex("6901983f 18149e82 3c7d133a 7d774572"));
	assert_eq!(k1(&ltk, &s1(b"SIRKenc"), b"csis"), hex("5277453c c094d982 b0e8ee53 2f2d1f8b"));
	let encrypted = sef(&ltk, &sirk);
	assert_eq!(encrypted, hex("170a3835 e13524a0 7e2562d5 f25fd346"));
	assert_eq!(sdf(&ltk, &encrypted), sirk);
	// THE CHARACTERISTIC'S BYTES: type 0 and the encrypted key least significant first, the LTK as the bond keeps it.
	let mut value = vec![0x00];
	value.extend(encrypted.iter().rev());
	let mut ltk_wire = ltk;
	ltk_wire.reverse();
	assert_eq!(sirk_of(&value, &ltk_wire), Some(sirk));
	let mut plain = vec![0x01];
	plain.extend(sirk.iter().rev());
	assert_eq!(sirk_of(&plain, &[0; 16]), Some(sirk));
	assert_eq!(sirk_of(&[0x02; 17], &[0; 16]), None);
}

#[test]
// AN RSI made for a set resolves with its SIRK and not with another's.
fn an_rsi_resolves_only_with_its_sets_key() {
	let sirk = hex("457d7d09 21a1fd22 cecd8c86 dd72cccd");
	let rsi = make_rsi(&sirk, 0x1234_5678);
	assert!(rsi_resolves(&sirk, &rsi));
	assert!(!rsi_resolves(&[0x55; 16], &rsi));
	let mut wrong_bits = rsi;
	wrong_bits[5] |= 0xc0;
	assert!(!rsi_resolves(&sirk, &wrong_bits), "a prand whose top bits are not 01");
	let advertised = [7, ad::RSI, rsi[0], rsi[1], rsi[2], rsi[3], rsi[4], rsi[5]];
	assert_eq!(super::rsi(&advertised), Some(rsi));
}

#[test]
// CAPABILITIES AND THE CHOICE: the best BAP configuration the sink supports, for music and for voice.
fn the_best_supported_configuration_is_chosen() {
	// 16, 24, 32 and 48 kHz; 10 ms; 26 to 155 octets.
	let capabilities = Capabilities { frequencies: 0b1011_0100, durations: 0b10, channel_counts: 0b1, min_octets: 26, max_octets: 155, frames_per_sdu: 1 };
	assert_eq!(Capabilities::parse(&capabilities.encode()), Some(capabilities));
	assert_eq!(choose(&capabilities, Purpose::Media), Some(Config::new(48_000, 10_000, 120)));
	assert_eq!(choose(&capabilities, Purpose::Voice), Some(Config::new(16_000, 10_000, 40)));
	let narrow = Capabilities { frequencies: 0b100, max_octets: 60, ..capabilities };
	assert_eq!(choose(&narrow, Purpose::Media), Some(Config::new(16_000, 10_000, 40)));
	let short_frames = Capabilities { durations: 0b01, ..capabilities };
	assert_eq!(choose(&short_frames, Purpose::Media), None, "7.5 ms alone is not what this host asks for");
	// A PAC characteristic's records, LC3's found among them.
	let value = encode_pac(&LC3_ID, &capabilities.encode(), &[3, 1, 4, 0]);
	let pacs = parse_pacs(&value).unwrap();
	assert_eq!(pacs[0].metadata, [3, 1, 4, 0]);
	assert_eq!(lc3_capabilities(&pacs), Some(capabilities));
	assert_eq!(parse_pacs(&value[..value.len() - 1]), None);
	assert_eq!(ltvs(&[3, 1, 2]), None, "an LTV past its field");
}

#[test]
// A CONFIGURATION round-trips, and a BIS's fields override its subgroup's.
fn configurations_round_trip_and_override() {
	let config = Config { allocation: location::FRONT_LEFT | location::FRONT_RIGHT, ..Config::new(48_000, 10_000, 100) };
	assert_eq!(Config::parse(&config.encode()), Some(config));
	assert_eq!(config.channels(), 2);
	assert_eq!((config.sdu_interval_us(), config.max_sdu()), (10_000, 200));
	assert_eq!(Config::parse(&[2, 1, 8]), None, "no duration or octets");
	let base = encode_base(40_000, &Config::new(48_000, 10_000, 100), &[location::FRONT_LEFT, location::FRONT_RIGHT]);
	let parsed = parse_base(&base).unwrap();
	assert_eq!(parsed.presentation_delay_us, 40_000);
	assert_eq!(
		parsed.subgroups[0].bises,
		vec![
			Bis { index: 1, config: Config { allocation: 1, ..Config::new(48_000, 10_000, 100) } },
			Bis { index: 2, config: Config { allocation: 2, ..Config::new(48_000, 10_000, 100) } }
		]
	);
	assert_eq!(parse_base(&base[..base.len() - 2]), None);
}

#[test]
// THE ANNOUNCEMENTS: a broadcast's id and name in its extended advertising, its BASE in the periodic train's.
fn broadcasts_are_found_by_their_announcements() {
	let mut data = vec![6, ad::SERVICE_DATA_16, 0x52, 0x18, 0x01, 0x02, 0x03];
	data.extend_from_slice(&[6, ad::BROADCAST_NAME, b'r', b'a', b'd', b'i', 0x07]);
	assert_eq!(broadcast_announcement(&data), Some((0x030201, String::from("radi"))));
	let base = encode_base(40_000, &Config::new(24_000, 10_000, 60), &[location::FRONT_LEFT]);
	let mut periodic = vec![3 + base.len() as u8, ad::SERVICE_DATA_16, 0x51, 0x18];
	periodic.extend_from_slice(&base);
	assert_eq!(service_data(&periodic, uuid::BASIC_AUDIO_ANNOUNCEMENT), Some(&base[..]));
	assert_eq!(broadcast_announcement(&periodic), None);
}

#[test]
// THE VOLUME: a state read, a set written with its counter, and AudioService's level both ways.
fn volume_state_and_levels() {
	assert_eq!(VolumeState::parse(&[200, 1, 7]), Some(VolumeState { setting: 200, muted: true, counter: 7 }));
	assert_eq!(VolumeState::parse(&[1, 2]), None);
	assert_eq!(set_absolute_volume(7, 128), [4, 7, 128]);
	assert_eq!((setting_of(0), setting_of(100), setting_of(50)), (0, 255, 128));
	for level in 0..=100u8 {
		assert_eq!(level_of(setting_of(level)), level);
	}
}
