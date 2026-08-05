use super::*;
use alloc::string::String;

#[test]
fn env_var_wire_is_stable() {
	let sample = EnvVar { name: String::from("x"), value: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(EnvVar::decode(&bytes).unwrap(), sample);
}
#[test]
fn launch_context_wire_is_stable() {
	let sample = LaunchContext { arguments: String::from("x"), cwd: String::from("x"), environment: alloc::vec![EnvVar { name: String::from("x"), value: String::from("x") }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 0, 120, 1, 0, 1, 0, 120, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(LaunchContext::decode(&bytes).unwrap(), sample);
}
#[test]
fn error_wire_is_stable() {
	let sample = Error::Denied;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Error::decode(&bytes).unwrap(), sample);
}
