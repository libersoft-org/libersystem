use super::*;
use alloc::string::String;

#[test]
fn capability_wire_is_stable() {
	let sample = Capability::Log;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(Capability::decode(&bytes).unwrap(), sample);
}
#[test]
fn manifest_wire_is_stable() {
	let sample = Manifest { component: String::from("x"), requested: alloc::vec![Capability::Log], grants: alloc::vec![Capability::Log] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 0, 0, 1, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Manifest::decode(&bytes).unwrap(), sample);
}
#[test]
fn audit_entry_wire_is_stable() {
	let sample = AuditEntry { component: String::from("x"), capability: Capability::Log, granted: true, dynamic: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 0, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(AuditEntry::decode(&bytes).unwrap(), sample);
}
