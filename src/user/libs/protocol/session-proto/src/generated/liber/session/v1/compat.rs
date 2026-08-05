use super::*;
use alloc::string::String;

#[test]
fn job_info_wire_is_stable() {
	let sample = JobInfo { id: 7, name: String::from("x"), stopped: true, group: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 0, 120, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(JobInfo::decode(&bytes).unwrap(), sample);
}
