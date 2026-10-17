use super::*;
use alloc::string::String;

#[test]
fn share_kind_wire_is_stable() {
	let sample = ShareKind::File;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(ShareKind::decode(&bytes).unwrap(), sample);
}
#[test]
fn share_node_wire_is_stable() {
	let sample = ShareNode { handle: 7, kind: ShareKind::File, size: 7, permissions: 7, modified: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ShareNode::decode(&bytes).unwrap(), sample);
}
#[test]
fn share_info_wire_is_stable() {
	let sample = ShareInfo { tag: String::from("x"), max_read: 7, max_nodes: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ShareInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn share_entry_wire_is_stable() {
	let sample = ShareEntry { name: String::from("x"), kind: ShareKind::File };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ShareEntry::decode(&bytes).unwrap(), sample);
}
#[test]
fn share_listing_wire_is_stable() {
	let sample = ShareListing { entries: alloc::vec![ShareEntry { name: String::from("x"), kind: ShareKind::File }], next: 7, done: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 1, 0, 120, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1];
	assert_eq!(bytes, golden);
	assert_eq!(ShareListing::decode(&bytes).unwrap(), sample);
}
#[test]
fn share_data_wire_is_stable() {
	let sample = ShareData { bytes: alloc::vec![7] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 7];
	assert_eq!(bytes, golden);
	assert_eq!(ShareData::decode(&bytes).unwrap(), sample);
}
