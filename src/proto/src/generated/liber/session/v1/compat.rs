use super::*;
use alloc::string::String;

#[test]
fn job_info_wire_is_stable() {
	let sample = JobInfo { id: 7, name: String::from("x"), stopped: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 0, 120, 1];
	assert_eq!(bytes, golden);
	assert_eq!(JobInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn env_var_wire_is_stable() {
	let sample = EnvVar { name: String::from("x"), value: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(EnvVar::decode(&bytes).unwrap(), sample);
}
