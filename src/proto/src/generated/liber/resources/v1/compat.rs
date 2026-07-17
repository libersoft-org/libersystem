use super::*;
use alloc::string::String;

#[test]
fn resource_type_wire_is_stable() {
	let sample = ResourceType::Memory;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(ResourceType::decode(&bytes).unwrap(), sample);
}
#[test]
fn resource_usage_wire_is_stable() {
	let sample = ResourceUsage { r#type: ResourceType::Memory, used: 7, limit: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ResourceUsage::decode(&bytes).unwrap(), sample);
}
#[test]
fn budget_wire_is_stable() {
	let sample = Budget { name: String::from("x"), usage: alloc::vec![ResourceUsage { r#type: ResourceType::Memory, used: 7, limit: 7 }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Budget::decode(&bytes).unwrap(), sample);
}
