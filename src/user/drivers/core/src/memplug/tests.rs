use super::*;

#[test]
// THE OFFSETS ARE THE SPECIFICATION'S: a request four bytes out names another block, and a device answers ERROR at best.
fn a_request_is_its_type_then_the_address_and_count_after_the_padding() {
	let plug = request(REQ_PLUG, 0x1_4020_0000, 16);
	assert_eq!(u16::from_le_bytes([plug[0], plug[1]]), REQ_PLUG);
	assert!(plug[2..8].iter().all(|byte| *byte == 0), "the padding");
	assert_eq!(u64::from_le_bytes(plug[8..16].try_into().unwrap()), 0x1_4020_0000);
	assert_eq!(u16::from_le_bytes([plug[16], plug[17]]), 16);
	assert!(plug[18..].iter().all(|byte| *byte == 0));
	let all = request(REQ_UNPLUG_ALL, 0x1_4020_0000, 16);
	assert!(all[2..].iter().all(|byte| *byte == 0), "UNPLUG_ALL names nothing");
	let mut answer = [0u8; RESPONSE_LEN];
	answer[8..10].copy_from_slice(&STATE_MIXED.to_le_bytes());
	assert_eq!(response(&answer), Some((RESP_ACK, STATE_MIXED)));
	answer[0] = 7;
	assert_eq!(response(&answer), None, "an answer type the specification does not name");
	assert_eq!(response(&answer[..9]), None, "a short answer");
}

#[test]
fn the_configuration_is_read_in_the_specifications_order() {
	let mut bytes = [0u8; CONFIG_LEN];
	bytes[0..8].copy_from_slice(&(2u64 << 20).to_le_bytes());
	bytes[16..24].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
	bytes[24..32].copy_from_slice(&(1u64 << 30).to_le_bytes());
	bytes[32..40].copy_from_slice(&(512u64 << 20).to_le_bytes());
	bytes[48..56].copy_from_slice(&(256u64 << 20).to_le_bytes());
	let config = Config::parse(&bytes).expect("whole");
	assert!(config.usable());
	assert_eq!((config.blocks(), config.usable_blocks()), (512, 256), "a gigabyte of region, half of it usable now");
	assert_eq!(config.requested_size, 256 << 20);
	assert!(!Config { block_size: 3 << 20, ..config }.usable(), "a block that is no power of two");
	assert!(!Config { addr: config.addr + 4096, ..config }.usable(), "a region that does not start on a block");
	assert_eq!(Config::parse(&bytes[..55]), None);
}

#[test]
// UP BY RUNS FROM THE BOTTOM, DOWN BY BLOCKS FROM THE TOP, and a block in use skipped until the next round.
fn a_step_plugs_the_lowest_run_and_offers_back_the_highest_block_not_in_use() {
	let none = [false; 8];
	assert_eq!(step(&[false; 8], 0, 8, 8, &none), Step::Hold, "nothing asked");
	assert_eq!(step(&[false; 8], 5, 8, 8, &none), Step::Plug(0, 5));
	assert_eq!(step(&[false; 8], 5, 3, 8, &none), Step::Plug(0, 3), "no more than the policy allows");
	assert_eq!(step(&[false; 8], 5, 8, 2, &none), Step::Plug(0, 2), "nor more than the region can use now");
	assert_eq!(step(&[true, true, false, true, false, false, false, false], 6, 8, 8, &none), Step::Plug(2, 1), "the lowest run first");
	assert_eq!(step(&[false; 40], 40, 40, 40, &[false; 40]), Step::Plug(0, PLUG_BATCH), "a batch at a time");
	let online = [true, true, true, true, false, false, false, false];
	assert_eq!(step(&online, 4, 8, 8, &none), Step::Hold, "as asked");
	assert_eq!(step(&online, 1, 8, 8, &none), Step::Unplug(3), "the highest block first");
	let mut held = none;
	held[3] = true;
	assert_eq!(step(&online, 1, 8, 8, &held), Step::Unplug(2), "a block in use this round is skipped");
	held[2] = true;
	held[1] = true;
	held[0] = true;
	assert_eq!(step(&online, 1, 8, 8, &held), Step::Hold, "and when every one is in use, nothing moves");
	assert_eq!(step(&online, 8, 2, 8, &none), Step::Unplug(3), "a lowered allowance gives back too");
}
