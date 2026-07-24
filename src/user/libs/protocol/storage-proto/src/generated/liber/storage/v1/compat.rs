use super::*;
use alloc::string::String;

#[test]
fn open_opts_wire_is_stable() {
	let sample = OpenOpts { path: String::from("x"), write: true, create: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(OpenOpts::decode(&bytes).unwrap(), sample);
}
#[test]
fn file_type_wire_is_stable() {
	let sample = FileType::File;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(FileType::decode(&bytes).unwrap(), sample);
}
#[test]
fn file_info_wire_is_stable() {
	let sample = FileInfo { name: String::from("x"), size: 7, r#type: FileType::File, mtime: 7, ctime: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(FileInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn snapshot_info_wire_is_stable() {
	let sample = SnapshotInfo { name: String::from("x"), generation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(SnapshotInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn volume_status_wire_is_stable() {
	let sample = VolumeStatus { label: String::from("x"), total_bytes: 7, free_bytes: 7, compression: true, read_only: true, filesystem: String::from("x") };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 0, 120, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(VolumeStatus::decode(&bytes).unwrap(), sample);
}
#[test]
fn fsck_report_wire_is_stable() {
	let sample = FsckReport { checksum_failures: 7, damaged: alloc::vec![String::from("x")] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 1, 0, 1, 0, 120];
	assert_eq!(bytes, golden);
	assert_eq!(FsckReport::decode(&bytes).unwrap(), sample);
}
