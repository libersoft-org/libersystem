use super::*;
use alloc::string::String;

#[test]
fn severity_wire_is_stable() {
	let sample = Severity::Trace;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Severity::decode(&bytes).unwrap(), sample);
}
#[test]
fn field_wire_is_stable() {
	let sample = Field { key: String::from("x"), value: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(Field::decode(&bytes).unwrap(), sample);
}
#[test]
fn entry_wire_is_stable() {
	let sample = Entry { timestamp: 7, severity: Severity::Trace, source: String::from("x"), fields: alloc::vec![Field { key: String::from("x"), value: String::from("x") }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120, 1, 0, 1, 0, 120, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(Entry::decode(&bytes).unwrap(), sample);
}
#[test]
fn query_wire_is_stable() {
	let sample = Query { since: Some(7), min_severity: Some(Severity::Trace), source: Some(String::from("x")), boot: Some(7), limit: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 120, 1, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Query::decode(&bytes).unwrap(), sample);
}
