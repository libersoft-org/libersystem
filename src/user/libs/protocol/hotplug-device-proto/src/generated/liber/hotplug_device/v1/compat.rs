use super::*;
use alloc::string::String;

#[test]
fn hotplug_status_wire_is_stable() {
	let sample = HotplugStatus { block_size: 7, region_size: 7, requested: 7, plugged: 7, allowed: 7, in_use: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(HotplugStatus::decode(&bytes).unwrap(), sample);
}
